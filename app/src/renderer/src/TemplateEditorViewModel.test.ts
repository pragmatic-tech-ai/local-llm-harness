import { describe, expect, it } from 'vitest'
import { RuntimeClient } from './RuntimeClient'
import { FakeSocket } from './FakeSocket'
import { TemplateEditorViewModel } from './TemplateEditorViewModel'
import { TemplateTreeNodeViewModel } from './TemplateTreeNodeViewModel'
import type { AgentTemplate, CatalogInfo, TemplateLeafNode, ToolSpec } from './agents'

class Rig
{
    public readonly socket = new FakeSocket()
    public readonly client = new RuntimeClient(() => this.socket)
    public readonly editor = new TemplateEditorViewModel(this.client)
    public readonly saved: string[] = []

    constructor()
    {
        this.client.Connect('ws://runtime')
        this.socket.Open()
        this.editor.OnSaved.Subscribe(id => this.saved.push(id))
    }

    public static readonly Catalog: CatalogInfo = {
        runtimes: [{ name: 'llamacpp', kind: 'llamacpp' }, { name: 'candle', kind: 'candle' }],
        models: [
            { name: 'gemma-4', runtimes: ['llamacpp'], size_hint: null },
            { name: 'tiny', runtimes: ['candle', 'llamacpp'], size_hint: null },
            { name: 'phi', runtimes: ['candle'], size_hint: null },
        ],
        hosts: ['hive'],
    }

    public static readonly Tools: ToolSpec[] = [
        { name: 'read_file', description: 'Read a file', parameters: {}, side_effects: false },
        { name: 'get_time', description: 'The time', parameters: {}, side_effects: false },
    ]

    public static readonly Researcher: AgentTemplate = {
        name: 'Researcher', system_prompt: 'read', tools: ['read_file'], step_budget: 8, runtime: 'llamacpp', model: 'gemma-4',
        n_ctx: 8192, enable_thinking: false,
    }

    public static readonly Leaf: TemplateLeafNode = { kind: 'template', id: 't1', path: 'Examples', name: 'Researcher', runtime: 'llamacpp', model: 'gemma-4', error: null }

    public Frames(): { id: number, method: string, params: Record<string, unknown> }[]
    {
        return this.socket.sent.map(f => JSON.parse(f))
    }

    public async Respond(method: string, result: unknown): Promise<void>
    {
        const id = this.Frames().filter(f => f.method === method).pop()?.id ?? -1
        this.client.HandleFrame(JSON.stringify({ id, result }))
        await new Promise(r => setTimeout(r, 0))
    }

    public async Fail(method: string, message: string): Promise<void>
    {
        const id = this.Frames().filter(f => f.method === method).pop()?.id ?? -1
        this.client.HandleFrame(JSON.stringify({ id, error: { code: -32603, message } }))
        await new Promise(r => setTimeout(r, 0))
    }

    public async LoadCatalog(): Promise<void>
    {
        void this.editor.LoadCatalog()
        await this.Respond('list_catalog', Rig.Catalog)
        await this.Respond('list_all_tools', Rig.Tools)
    }

    public async Open(template: AgentTemplate = Rig.Researcher, leaf: TemplateLeafNode = Rig.Leaf): Promise<void>
    {
        await this.LoadCatalog()
        void this.editor.Open(new TemplateTreeNodeViewModel(leaf))
        await this.Respond('get_template', { id: leaf.id, path: leaf.path, template })
    }
}

describe('TemplateEditorViewModel', () =>
{
    it('opens_a_template_into_its_fields', async () =>
    {
        const rig = new Rig()
        await rig.Open({ ...Rig.Researcher, sampling: { temperature: 0.7, top_p: 0.9, top_k: 40, repeat_penalty: 1.1, seed: -1, max_tokens: -1 }, host: 'hive' })
        const e = rig.editor
        expect(e.HasTemplate).toBe(true)
        expect(e.Name).toBe('Researcher')
        expect(e.SystemPrompt).toBe('read')
        expect(e.StepBudgetText).toBe('8')
        expect(e.Runtime).toBe('llamacpp')
        expect(e.Model).toBe('gemma-4')
        expect(e.ContextSizeText).toBe('8192')
        expect(e.TemperatureText).toBe('0.7')
        expect(e.TopPText).toBe('0.9')
        expect(e.Host).toBe('hive')
        expect(e.HostChoice?.Value).toBe('hive')
        expect(e.Tools.ToArray().map(t => [t.Name, t.Description, t.Enabled])).toEqual([['read_file', 'Read a file', true], ['get_time', 'The time', false]])
        expect(e.IsDirty).toBe(false)
        expect(e.SaveCommand.CanExecute(undefined)).toBe(false)
        expect(e.ErrorText).toBe('')
    })

    it('editor_save_sends_fields_and_clears_dirty', async () =>
    {
        const rig = new Rig()
        await rig.Open()
        const e = rig.editor
        e.Name = 'Researcher 2'
        e.StepBudgetText = '4'
        e.TemperatureText = '0.5'
        e.EnableThinking = true
        e.Tools.Get(1)!.Enabled = true
        e.HostChoice = e.HostChoices.ToArray().find(c => c.Value === 'hive')!
        expect(e.IsDirty).toBe(true)
        expect(e.SaveCommand.CanExecute(undefined)).toBe(true)
        e.SaveCommand.Execute(undefined)
        const save = rig.Frames().pop()!
        expect(save.method).toBe('save_template')
        expect(save.params).toEqual({
            id: 't1',
            path: 'Examples',
            template: {
                name: 'Researcher 2', system_prompt: 'read', tools: ['read_file', 'get_time'], step_budget: 4, runtime: 'llamacpp',
                model: 'gemma-4', n_ctx: 8192, enable_thinking: true, sampling: { temperature: 0.5 }, host: 'hive',
            },
        })
        await rig.Respond('save_template', { id: 't1' })
        expect(e.IsDirty).toBe(false)
        expect(e.ErrorText).toBe('')
        expect(rig.saved).toEqual(['t1'])
    })

    it('editor_shows_validation_error', async () =>
    {
        const rig = new Rig()
        await rig.Open()
        rig.editor.Name = 'Other'
        rig.editor.SaveCommand.Execute(undefined)
        await rig.Fail('save_template', 'unknown tool: x')
        expect(rig.editor.ErrorText).toBe('unknown tool: x')
        expect(rig.editor.IsDirty).toBe(true)
        expect(rig.saved).toEqual([])
    })

    it('model_choices_filter_by_runtime', async () =>
    {
        const rig = new Rig()
        await rig.Open()
        const e = rig.editor
        expect(e.RuntimeChoices.ToArray().map(c => c.Value)).toEqual(['llamacpp', 'candle'])
        expect(e.ModelChoices.ToArray().map(c => c.Label)).toEqual(['gemma-4', 'tiny'])
        e.RuntimeChoice = e.RuntimeChoices.Get(1)!
        expect(e.Runtime).toBe('candle')
        // The template's model is kept, marked, rather than silently changed.
        expect(e.Model).toBe('gemma-4')
        expect(e.ModelChoices.ToArray().map(c => c.Label)).toEqual(['tiny', 'phi', 'gemma-4 (not in catalog)'])
        expect(e.ModelChoice?.Value).toBe('gemma-4')
        e.ModelChoice = e.ModelChoices.Get(1)!
        expect(e.Model).toBe('phi')
        expect(e.ModelChoices.ToArray().map(c => c.Label)).toEqual(['tiny', 'phi'])
    })

    it('model_outside_the_catalog_stays_a_marked_choice', async () =>
    {
        const rig = new Rig()
        await rig.Open({ ...Rig.Researcher, model: 'gone-model' })
        expect(rig.editor.ModelChoices.ToArray().map(c => c.Label)).toEqual(['gemma-4', 'tiny', 'gone-model (not in catalog)'])
        expect(rig.editor.ModelChoice?.Value).toBe('gone-model')
        expect(rig.editor.IsDirty).toBe(false)
    })

    it('broken_template_opens_with_error', async () =>
    {
        const rig = new Rig()
        await rig.LoadCatalog()
        const broken: TemplateLeafNode = { kind: 'template', id: 't2', path: 'Examples', name: 'Broken', runtime: '', model: '', error: 't2: missing field `n_ctx`' }
        void rig.editor.Open(new TemplateTreeNodeViewModel(broken))
        await new Promise(r => setTimeout(r, 0))
        const e = rig.editor
        // get_template cannot read a broken file: the editor seeds the defaults instead of asking.
        expect(rig.Frames().map(f => f.method)).not.toContain('get_template')
        expect(e.HasTemplate).toBe(true)
        expect(e.ErrorText).toBe('t2: missing field `n_ctx`')
        expect(e.Name).toBe('Broken')
        expect(e.StepBudgetText).toBe('8')
        expect(e.ContextSizeText).toBe('8192')
        expect(e.Runtime).toBe('llamacpp')
        expect(e.Model).toBe('gemma-4')
        // Save repairs the file in place.
        expect(e.SaveCommand.CanExecute(undefined)).toBe(true)
        e.SaveCommand.Execute(undefined)
        const save = rig.Frames().pop()!
        expect(save.params.id).toBe('t2')
        expect(save.params.path).toBe('Examples')
        await rig.Respond('save_template', { id: 't2' })
        expect(e.ErrorText).toBe('')
        expect(e.SaveCommand.CanExecute(undefined)).toBe(false)
    })

    it('unparsable_numbers_show_field_errors_and_disable_save', async () =>
    {
        const rig = new Rig()
        await rig.Open()
        const e = rig.editor
        e.StepBudgetText = 'many'
        expect(e.StepBudgetError).not.toBe('')
        expect(e.SaveCommand.CanExecute(undefined)).toBe(false)
        e.StepBudgetText = '0'
        expect(e.HasStepBudgetError).toBe(true)
        e.StepBudgetText = '3'
        e.ContextSizeText = '8k'
        expect(e.HasContextSizeError).toBe(true)
        e.ContextSizeText = '4096'
        e.TemperatureText = 'warm'
        expect(e.HasTemperatureError).toBe(true)
        e.TemperatureText = ''
        e.TopPText = '1.5'
        expect(e.HasTopPError).toBe(true)
        expect(e.SaveCommand.CanExecute(undefined)).toBe(false)
        e.Save()
        expect(rig.Frames().map(f => f.method)).not.toContain('save_template')
        e.TopPText = '0.9'
        expect(e.SaveCommand.CanExecute(undefined)).toBe(true)
        e.SaveCommand.Execute(undefined)
        const template = rig.Frames().pop()!.params.template as AgentTemplate
        expect(template.step_budget).toBe(3)
        expect(template.n_ctx).toBe(4096)
        expect(template.sampling).toEqual({ top_p: 0.9 })
    })

    it('blank_sampling_sends_no_sampling_and_keeps_other_knobs', async () =>
    {
        const rig = new Rig()
        await rig.Open({ ...Rig.Researcher, sampling: { temperature: 0.7, top_p: 0.9, top_k: 20, repeat_penalty: 1.1, seed: 3, max_tokens: -1 } })
        const e = rig.editor
        e.TemperatureText = '0.2'
        e.SaveCommand.Execute(undefined)
        expect((rig.Frames().pop()!.params.template as AgentTemplate).sampling).toEqual({ temperature: 0.2, top_p: 0.9, top_k: 20, repeat_penalty: 1.1, seed: 3, max_tokens: -1 })
        await rig.Respond('save_template', { id: 't1' })
        e.TemperatureText = ''
        e.TopPText = ''
        e.SaveCommand.Execute(undefined)
        expect('sampling' in (rig.Frames().pop()!.params.template as AgentTemplate)).toBe(false)
    })

    it('revert_restores_the_loaded_fields', async () =>
    {
        const rig = new Rig()
        await rig.Open()
        const e = rig.editor
        e.Name = 'Changed'
        e.Tools.Get(0)!.Enabled = false
        expect(e.RevertCommand.CanExecute(undefined)).toBe(true)
        e.RevertCommand.Execute(undefined)
        expect(e.Name).toBe('Researcher')
        expect(e.Tools.Get(0)!.Enabled).toBe(true)
        expect(e.IsDirty).toBe(false)
    })

    it('new_template_is_an_unsaved_untitled_draft', async () =>
    {
        const rig = new Rig()
        await rig.LoadCatalog()
        const e = rig.editor
        e.OpenNew('Examples')
        expect(e.Name).toBe('Untitled')
        expect(e.IsNew).toBe(true)
        expect(e.IsDirty).toBe(true)
        expect(e.Runtime).toBe('llamacpp')
        expect(e.Model).toBe('gemma-4')
        e.SaveCommand.Execute(undefined)
        const save = rig.Frames().pop()!
        expect(save.params.id).toBeUndefined()
        expect(save.params.path).toBe('Examples')
        await rig.Respond('save_template', { id: 'untitled' })
        expect(e.TemplateId).toBe('untitled')
        expect(e.IsNew).toBe(false)
        expect(e.IsDirty).toBe(false)
        expect(rig.saved).toEqual(['untitled'])
    })

    it('blank_name_disables_save', async () =>
    {
        const rig = new Rig()
        await rig.Open()
        rig.editor.Name = '  '
        expect(rig.editor.HasNameError).toBe(true)
        expect(rig.editor.SaveCommand.CanExecute(undefined)).toBe(false)
    })

    it('reload_is_dropped_when_edits_started_meanwhile', async () =>
    {
        const rig = new Rig()
        await rig.Open()
        void rig.editor.Reload(new TemplateTreeNodeViewModel(Rig.Leaf))
        rig.editor.Name = 'Typing'
        await rig.Respond('get_template', { id: 't1', path: 'Examples', template: { ...Rig.Researcher, name: 'Server' } })
        expect(rig.editor.Name).toBe('Typing')
    })
})
