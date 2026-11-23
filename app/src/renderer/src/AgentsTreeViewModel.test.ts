import { describe, expect, it } from 'vitest'
import { RuntimeClient } from './RuntimeClient'
import { FakeSocket } from './FakeSocket'
import { AgentsTreeViewModel } from './AgentsTreeViewModel'
import type { TemplateTreeNodeViewModel } from './TemplateTreeNodeViewModel'
import type { TemplateNode } from './agents'

class Rig
{
    public readonly socket = new FakeSocket()
    public readonly client = new RuntimeClient(() => this.socket)
    public readonly tree = new AgentsTreeViewModel()
    public readonly requested: TemplateTreeNodeViewModel[] = []

    constructor()
    {
        this.client.Connect('ws://runtime')
        this.socket.Open()
        this.tree.Bind(this.client)
        this.tree.OnSelectRequested.Subscribe(node => this.requested.push(node))
    }

    public static readonly Researcher: TemplateNode = { kind: 'template', id: 't1', path: 'Examples', name: 'Researcher', runtime: 'llamacpp', model: 'gemma-4', error: null }
    public static readonly Broken: TemplateNode = { kind: 'template', id: 't2', path: 'Examples', name: 'Broken', runtime: '', model: '', error: 'unknown tool: nope' }
    public static readonly Loose: TemplateNode = { kind: 'template', id: 't3', path: '', name: 'Loose', runtime: 'llamacpp', model: 'gemma-4', error: null }

    public static readonly Tree: TemplateNode[] = [
        { kind: 'group', name: 'Examples', path: 'Examples', children: [Rig.Researcher, Rig.Broken] },
        Rig.Loose,
    ]

    public Methods(): string[]
    {
        return this.socket.sent.map(f => (JSON.parse(f) as { method: string }).method)
    }

    public async Respond(method: string, result: unknown): Promise<void>
    {
        const frames = this.socket.sent.map(f => JSON.parse(f) as { id: number, method: string })
        const id = frames.filter(f => f.method === method).pop()?.id ?? -1
        this.client.HandleFrame(JSON.stringify({ id, result }))
        await new Promise(r => setTimeout(r, 0))
    }

    public async Load(nodes: TemplateNode[] = Rig.Tree): Promise<void>
    {
        void this.tree.Refresh()
        await this.Respond('list_templates', nodes)
    }
}

describe('AgentsTreeViewModel', () =>
{
    it('tree_renders_groups_and_templates', async () =>
    {
        const rig = new Rig()
        await rig.Load()
        expect(rig.tree.Roots.Count).toBe(2)
        const group = rig.tree.Roots.Get(0)!
        expect(group.IsGroup).toBe(true)
        expect(group.Name).toBe('Examples')
        expect(group.Children!.ToArray().map(n => n.Name)).toEqual(['Researcher', 'Broken'])
        expect(group.Children!.Get(1)!.Error).toBe('unknown tool: nope')
        const loose = rig.tree.Roots.Get(1)!
        expect(loose.IsTemplate).toBe(true)
        expect(loose.Children).toBeUndefined()
        expect(rig.tree.IsEmpty).toBe(false)
    })

    it('templates_changed_refreshes_tree', async () =>
    {
        const rig = new Rig()
        await rig.Load()
        const group = rig.tree.Roots.Get(0)!
        const researcher = group.Children!.Get(0)!
        rig.tree.Select(researcher)
        group.IsExpanded = false
        rig.client.HandleFrame(JSON.stringify({ method: 'templates_changed', params: {} }))
        expect(rig.Methods()).toEqual(['list_templates', 'list_templates'])
        // Researcher renamed and moved to the root, Broken deleted, a new group added.
        await rig.Respond('list_templates', [
            { kind: 'group', name: 'Examples', path: 'Examples', children: [] },
            { kind: 'group', name: 'Work', path: 'Work', children: [] },
            { ...Rig.Researcher, path: '', name: 'Researcher 2' },
            Rig.Loose,
        ])
        expect(rig.tree.Roots.ToArray().map(n => n.Name)).toEqual(['Examples', 'Work', 'Researcher 2', 'Loose'])
        // The same objects: selection and expansion are kept, nothing is re-opened.
        expect(rig.tree.Roots.Get(0)).toBe(group)
        expect(group.IsExpanded).toBe(false)
        expect(group.Children!.Count).toBe(0)
        expect(rig.tree.Roots.Get(2)).toBe(researcher)
        expect(researcher.Path).toBe('')
        expect(rig.tree.Selected).toBe(researcher)
        expect(rig.requested).toEqual([])
    })

    it('selection_is_cleared_when_the_selected_template_disappears', async () =>
    {
        const rig = new Rig()
        await rig.Load()
        const loose = rig.tree.Roots.Get(1)!
        rig.tree.Select(loose)
        void rig.tree.Refresh()
        await rig.Respond('list_templates', [Rig.Tree[0]])
        expect(rig.tree.Selected).toBeNull()
    })

    it('user_selection_is_a_request_the_owner_grants', async () =>
    {
        const rig = new Rig()
        await rig.Load()
        const loose = rig.tree.Roots.Get(1)!
        rig.tree.Selected = loose
        expect(rig.requested).toEqual([loose])
        expect(rig.tree.Selected).toBeNull()
        rig.tree.Select(loose)
        expect(rig.tree.Selected).toBe(loose)
        rig.tree.Selected = loose
        expect(rig.requested).toEqual([loose])
    })

    it('select_when_listed_selects_a_template_once_it_appears', async () =>
    {
        const rig = new Rig()
        await rig.Load()
        rig.tree.SelectWhenListed('t9')
        void rig.tree.Refresh()
        await rig.Respond('list_templates', [...Rig.Tree, { ...Rig.Loose, id: 't9', name: 'Untitled' }])
        expect(rig.tree.Selected?.TemplateId).toBe('t9')
        expect(rig.requested).toEqual([])
    })

    it('commands_signal_with_the_selected_group', async () =>
    {
        const rig = new Rig()
        await rig.Load()
        const newTemplate: string[] = []
        const newGroup: string[] = []
        const moved: TemplateTreeNodeViewModel[] = []
        rig.tree.OnNewTemplateRequested.Subscribe(p => newTemplate.push(p))
        rig.tree.OnNewGroupRequested.Subscribe(p => newGroup.push(p))
        rig.tree.OnMoveRequested.Subscribe(n => moved.push(n))
        expect(rig.tree.RenameCommand.CanExecute(undefined)).toBe(false)
        expect(rig.tree.MoveToCommand.CanExecute(undefined)).toBe(false)
        rig.tree.NewTemplateCommand.Execute(undefined)
        const group = rig.tree.Roots.Get(0)!
        rig.tree.Select(group)
        expect(rig.tree.MoveToCommand.CanExecute(undefined)).toBe(false)
        rig.tree.NewGroupCommand.Execute(undefined)
        const researcher = group.Children!.Get(0)!
        rig.tree.Select(researcher)
        rig.tree.NewTemplateCommand.Execute(undefined)
        rig.tree.MoveToCommand.Execute(undefined)
        expect(newTemplate).toEqual(['', 'Examples'])
        expect(newGroup).toEqual(['Examples'])
        expect(moved).toEqual([researcher])
    })

    it('groups_list_root_first_with_nested_paths', async () =>
    {
        const rig = new Rig()
        await rig.Load([
            { kind: 'group', name: 'A', path: 'A', children: [{ kind: 'group', name: 'B', path: 'A/B', children: [] }] },
        ])
        expect(rig.tree.Groups().map(g => [g.Value, g.Label])).toEqual([['', '(root)'], ['A', 'A'], ['A/B', 'A/B']])
    })

    it('refresh_failure_shows_on_status_line', async () =>
    {
        const rig = new Rig()
        void rig.tree.Refresh()
        const id = (JSON.parse(rig.socket.sent[0]) as { id: number }).id
        rig.client.HandleFrame(JSON.stringify({ id, error: { code: -32603, message: 'boom' } }))
        await new Promise(r => setTimeout(r, 0))
        expect(rig.tree.StatusText).toBe('templates unavailable: boom')
    })
})
