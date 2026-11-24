import { describe, expect, it } from 'vitest'
import { RuntimeClient } from './RuntimeClient'
import { FakeSocket } from './FakeSocket'
import { NewSessionDialogViewModel } from './NewSessionDialogViewModel'
import type { SessionSummary, TemplateNode } from './agents'

class Rig
{
    public readonly socket = new FakeSocket()
    public readonly client = new RuntimeClient(() => this.socket)
    public readonly vm = new NewSessionDialogViewModel(this.client)
    public readonly created: SessionSummary[] = []

    constructor()
    {
        this.client.Connect('ws://runtime')
        this.socket.Open()
        this.vm.OnCreated.Subscribe(s => this.created.push(s))
    }

    public static readonly Tree: TemplateNode[] = [
        { kind: 'group', name: 'Examples', path: 'Examples', children: [
            { kind: 'template', id: 't1', path: 'Examples', name: 'Researcher', runtime: 'llamacpp', model: 'gemma-4', error: null },
            { kind: 'template', id: 't2', path: 'Examples', name: 'Broken', runtime: '', model: '', error: 'unknown tool: nope' },
        ] },
    ]

    public async Respond(method: string, result: unknown): Promise<void>
    {
        const frames = this.socket.sent.map(f => JSON.parse(f) as { id: number, method: string })
        const id = frames.filter(f => f.method === method).pop()?.id ?? -1
        this.client.HandleFrame(JSON.stringify({ id, result }))
        await new Promise(r => setTimeout(r, 0))
    }

    public async Load(): Promise<void>
    {
        const done = this.vm.Load()
        await this.Respond('list_templates', Rig.Tree)
        await done
    }
}

describe('NewSessionDialogViewModel', () =>
{
    it('summary_shows_model_tools_and_budget', async () =>
    {
        const rig = new Rig()
        await rig.Load()
        const group = rig.vm.Roots.Get(0)!
        expect(group.IsGroup).toBe(true)
        expect(group.Children!.Count).toBe(2)
        rig.vm.SelectedNode = group
        expect(rig.vm.Summary).toBe('Choose a template.')
        expect(rig.vm.CreateCommand.CanExecute(undefined)).toBe(false)
        rig.vm.SelectedNode = group.Children!.Get(0)!
        await rig.Respond('get_template', { id: 't1', path: 'Examples', template: {
            name: 'Researcher', system_prompt: 'read', tools: ['read_file'], step_budget: 4, runtime: 'llamacpp', model: 'gemma-4', n_ctx: 8192, enable_thinking: false,
        } })
        expect(rig.vm.Summary).toBe('Model: gemma-4 (llamacpp, context 8192)\nRepository: hive\nTools: read_file\nStep budget: 4')
        expect(rig.vm.CreateCommand.CanExecute(undefined)).toBe(true)
    })

    it('broken_template_cannot_be_created', async () =>
    {
        const rig = new Rig()
        await rig.Load()
        rig.vm.SelectedNode = rig.vm.Roots.Get(0)!.Children!.Get(1)!
        expect(rig.vm.Summary).toBe('This template cannot be used: unknown tool: nope')
        expect(rig.vm.CreateCommand.CanExecute(undefined)).toBe(false)
    })

    it('create_failure_is_shown_and_raises_nothing', async () =>
    {
        const rig = new Rig()
        await rig.Load()
        rig.vm.SelectedNode = rig.vm.Roots.Get(0)!.Children!.Get(0)!
        rig.vm.CreateCommand.Execute(undefined)
        const frames = rig.socket.sent.map(f => JSON.parse(f) as { id: number, method: string })
        const id = frames.filter(f => f.method === 'create_session').pop()!.id
        rig.client.HandleFrame(JSON.stringify({ id, error: { code: -32603, message: 'unknown template: t1' } }))
        await new Promise(r => setTimeout(r, 0))
        expect(rig.vm.StatusText).toBe('could not create the session: unknown template: t1')
        expect(rig.created).toEqual([])
        expect(rig.vm.CreateCommand.CanExecute(undefined)).toBe(true)
    })
    it('summary_names_the_repository', () =>
    {
        const template = { name: 'R', system_prompt: '', tools: [], step_budget: 4, runtime: 'openrouter', model: 'google/gemma-small', n_ctx: 8192, enable_thinking: false }
        expect(NewSessionDialogViewModel.Describe({ ...template, repository: 'or' }))
            .toBe('Model: google/gemma-small (openrouter, context 8192)\nRepository: or\nTools: none\nStep budget: 4')
        expect(NewSessionDialogViewModel.Describe({ ...template, host: 'hive' })).toContain('\nRepository: hive\n')
    })
})
