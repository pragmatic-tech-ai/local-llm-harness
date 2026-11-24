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
    it('tool_choice_marks_writing_tools', async () =>
    {
        const rig = new Rig()
        rig.client.ListAllTools = () => Promise.resolve([
            { name: 'read_file', description: 'Read', parameters: {}, side_effects: false },
            { name: 'write_file', description: 'Write', parameters: {}, side_effects: true },
        ])
        await rig.Open({ ...Rig.Researcher, tools: ['read_file', 'write_file', 'gone'] })
        const writes = rig.editor.Tools.ToArray().map(t => t.Writes)
        expect(writes).toEqual([false, true, false])
    })

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
                model: 'gemma-4', n_ctx: 8192, enable_thinking: true, context_strategy: 'off', compact_at_pct: 75,
                sampling: { temperature: 0.5, top_p: 0.95 }, host: 'hive', workspace: { mode: 'session' },
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

    it('editor_loads_and_saves_context_fields', async () =>
    {
        const rig = new Rig()
        await rig.Open({ ...Rig.Researcher, context_strategy: 'summarize', compact_at_pct: 60, sampling: { top_k: 40, seed: 7 } })
        const e = rig.editor
        expect(e.Context).toBe('summarize')
        expect(e.ContextChoice?.Label).toBe('Summarize')
        expect(e.ContextChoices.ToArray().map(c => c.Label)).toEqual(['Off', 'Trim', 'Summarize'])
        expect(e.CompactAtText).toBe('60')
        expect(e.IsDirty).toBe(false)
        e.ContextChoice = e.ContextChoices.ToArray().find(c => c.Value === 'trim')!
        e.CompactAtText = '80'
        expect(e.IsDirty).toBe(true)
        e.Revert()
        expect(e.Context).toBe('summarize')
        expect(e.CompactAtText).toBe('60')
        expect(e.IsDirty).toBe(false)
        e.Context = 'trim'
        e.CompactAtText = '80'
        e.SaveCommand.Execute(undefined)
        const template = rig.Frames().pop()!.params.template as AgentTemplate
        expect(template.context_strategy).toBe('trim')
        expect(template.compact_at_pct).toBe(80)
        expect(template.sampling).toEqual({ top_k: 40, seed: 7, temperature: 0.8, top_p: 0.95 })
    })

    it('editor_loads_and_saves_workspace', async () =>
    {
        const rig = new Rig()
        await rig.Open({ ...Rig.Researcher, workspace: { mode: 'named', name: 'notes' } })
        const e = rig.editor
        expect(e.Workspace).toBe('named')
        expect(e.WorkspaceChoice?.Label).toBe('Named')
        expect(e.WorkspaceChoices.ToArray().map(c => c.Label)).toEqual(['Session', 'Shared', 'Named'])
        expect(e.WorkspaceName).toBe('notes')
        expect(e.IsDirty).toBe(false)
        e.WorkspaceChoice = e.WorkspaceChoices.ToArray().find(c => c.Value === 'shared')!
        e.WorkspaceName = 'other'
        expect(e.IsDirty).toBe(true)
        e.Revert()
        expect(e.Workspace).toBe('named')
        expect(e.WorkspaceName).toBe('notes')
        expect(e.IsDirty).toBe(false)
        e.WorkspaceName = '  other  '
        e.SaveCommand.Execute(undefined)
        expect((rig.Frames().pop()!.params.template as AgentTemplate).workspace).toEqual({ mode: 'named', name: 'other' })
    })

    it('named_name_enabled_only_for_named', async () =>
    {
        const rig = new Rig()
        await rig.Open()
        const e = rig.editor
        expect(e.IsWorkspaceNameEnabled).toBe(false)
        e.Workspace = 'shared'
        expect(e.IsWorkspaceNameEnabled).toBe(false)
        e.Workspace = 'named'
        expect(e.IsWorkspaceNameEnabled).toBe(true)
        e.Workspace = 'session'
        expect(e.IsWorkspaceNameEnabled).toBe(false)
    })

    it('old_template_without_workspace_shows_session', async () =>
    {
        const rig = new Rig()
        await rig.Open()
        expect(rig.editor.Workspace).toBe('session')
        expect(rig.editor.WorkspaceChoice?.Label).toBe('Session')
        expect(rig.editor.WorkspaceName).toBe('')
        expect(rig.editor.IsDirty).toBe(false)
    })

    it('switching_from_named_drops_name_on_save', async () =>
    {
        const rig = new Rig()
        await rig.Open({ ...Rig.Researcher, workspace: { mode: 'named', name: 'notes' } })
        rig.editor.Workspace = 'shared'
        rig.editor.SaveCommand.Execute(undefined)
        expect((rig.Frames().pop()!.params.template as AgentTemplate).workspace).toEqual({ mode: 'shared' })
    })

    it('named_workspace_needs_a_valid_name', async () =>
    {
        const rig = new Rig()
        await rig.Open()
        const e = rig.editor
        expect(e.HasWorkspaceNameError).toBe(false)
        e.Workspace = 'named'
        for (const blank of ['', '   '])
        {
            e.WorkspaceName = blank
            expect(e.WorkspaceNameError).toBe('workspace name is required for named mode')
            expect(e.CanSave).toBe(false)
        }
        for (const bad of ['two words', 'a/b', 'x'.repeat(65), 'café'])
        {
            e.WorkspaceName = bad
            expect(e.HasWorkspaceNameError).toBe(true)
            expect(e.WorkspaceNameError).toBe('workspace name must be 1-64 letters, digits, _ or -')
            expect(e.CanSave).toBe(false)
        }
        for (const good of ['notes', 'A_b-9', 'x'.repeat(64), '  padded  '])
        {
            e.WorkspaceName = good
            expect(e.HasWorkspaceNameError).toBe(false)
            expect(e.CanSave).toBe(true)
        }
        // Any other mode ignores the name.
        e.WorkspaceName = 'two words'
        e.Workspace = 'shared'
        expect(e.HasWorkspaceNameError).toBe(false)
        expect(e.CanSave).toBe(true)
    })

    it('workspace_name_counts_toward_dirty_only_when_named', async () =>
    {
        const rig = new Rig()
        await rig.Open({ ...Rig.Researcher, workspace: { mode: 'shared' } })
        const e = rig.editor
        e.WorkspaceName = 'ignored'
        expect(e.IsDirty).toBe(false)
        e.Workspace = 'named'
        expect(e.IsDirty).toBe(true)
        e.Workspace = 'shared'
        expect(e.IsDirty).toBe(false)
    })

    it('compact_at_out_of_range_blocks_save', async () =>
    {
        const rig = new Rig()
        await rig.Open()
        const e = rig.editor
        for (const bad of ['9', '96', 'abc', '', '7.5'])
        {
            e.CompactAtText = bad
            expect(e.HasCompactAtError).toBe(true)
            expect(e.CompactAtError).toBe('Compact at must be a whole percentage from 10 to 95.')
            expect(e.CanSave).toBe(false)
        }
        for (const good of ['10', '95'])
        {
            e.CompactAtText = good
            expect(e.HasCompactAtError).toBe(false)
            expect(e.CanSave).toBe(true)
        }
    })

    it('old_template_without_fields_shows_off_and_75', async () =>
    {
        const rig = new Rig()
        await rig.Open()
        expect(rig.editor.Context).toBe('off')
        expect(rig.editor.ContextChoice?.Label).toBe('Off')
        expect(rig.editor.CompactAtText).toBe('75')
        expect(rig.editor.IsDirty).toBe(false)
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
        // The template's model is kept, marked (it is in the catalog, just not for candle), rather than silently changed.
        expect(e.Model).toBe('gemma-4')
        expect(e.ModelChoices.ToArray().map(c => c.Label)).toEqual(['tiny', 'phi', 'gemma-4 (not served by candle)'])
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
        // get_template cannot read a file that does not parse: the editor falls back to the defaults.
        await rig.Fail('get_template', 't2: missing field `n_ctx`')
        const e = rig.editor
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

    it('template_failing_validation_opens_with_its_real_fields_and_the_error', async () =>
    {
        const rig = new Rig()
        await rig.LoadCatalog()
        const flagged: TemplateLeafNode = { ...Rig.Leaf, error: 'unknown tool: nope' }
        void rig.editor.Open(new TemplateTreeNodeViewModel(flagged))
        const template: AgentTemplate = { ...Rig.Researcher, system_prompt: 'keep me', tools: ['read_file', 'nope'], step_budget: 3 }
        await rig.Respond('get_template', { id: 't1', path: 'Examples', template, error: 'unknown tool: nope' })
        const e = rig.editor
        expect(rig.Frames().map(f => f.method)).toContain('get_template')
        expect(e.ErrorText).toBe('unknown tool: nope')
        expect(e.SystemPrompt).toBe('keep me')
        expect(e.StepBudgetText).toBe('3')
        expect(e.Model).toBe('gemma-4')
        expect(e.IsDirty).toBe(false)
        // Broken, so Save (which repairs it) is offered while clean; Revert keeps the error shown.
        expect(e.SaveCommand.CanExecute(undefined)).toBe(true)
        e.SystemPrompt = 'edited'
        e.Revert()
        expect(e.SystemPrompt).toBe('keep me')
        expect(e.ErrorText).toBe('unknown tool: nope')
        // A catalog arriving later does not reseed the real fields.
        await rig.LoadCatalog()
        expect(e.SystemPrompt).toBe('keep me')
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
        // A blank temperature sends the worker default (the backend sampling fields are not optional).
        expect(template.sampling).toEqual({ temperature: 0.8, top_p: 0.9 })
    })

    it('blank_sampling_fields_keep_the_other_knobs', async () =>
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
        // Both fields blank: the hand-set knobs stay, the two shown ones go back to the worker defaults.
        expect((rig.Frames().pop()!.params.template as AgentTemplate).sampling).toEqual({ temperature: 0.8, top_p: 0.95, top_k: 20, repeat_penalty: 1.1, seed: 3, max_tokens: -1 })
    })

    it('no_sampling_is_sent_when_none_was_loaded_and_both_fields_are_blank', async () =>
    {
        const rig = new Rig()
        await rig.Open()
        rig.editor.Name = 'Other'
        rig.editor.SaveCommand.Execute(undefined)
        expect('sampling' in (rig.Frames().pop()!.params.template as AgentTemplate)).toBe(false)
    })

    it('a_later_open_wins_over_a_late_reply_for_an_earlier_one', async () =>
    {
        const rig = new Rig()
        await rig.LoadCatalog()
        void rig.editor.Open(new TemplateTreeNodeViewModel(Rig.Leaf))
        const first = rig.Frames().pop()!
        void rig.editor.Open(new TemplateTreeNodeViewModel({ ...Rig.Leaf, id: 't3', path: '', name: 'Loose' }))
        // The request is recorded at once, and the form is read-only until its reply.
        expect(rig.editor.TemplateId).toBe('t3')
        expect(rig.editor.Path).toBe('')
        expect(rig.editor.IsEditable).toBe(false)
        expect(rig.editor.SaveCommand.CanExecute(undefined)).toBe(false)
        rig.client.HandleFrame(JSON.stringify({ id: first.id, result: { id: 't1', path: 'Examples', template: Rig.Researcher } }))
        await new Promise(r => setTimeout(r, 0))
        expect(rig.editor.TemplateId).toBe('t3')
        expect(rig.editor.IsLoading).toBe(true)
        // A reload for the earlier template is ignored too.
        void rig.editor.Reload(new TemplateTreeNodeViewModel(Rig.Leaf))
        expect(rig.Frames().filter(f => f.method === 'get_template').length).toBe(2)
        await rig.Respond('get_template', { id: 't3', path: '', template: { ...Rig.Researcher, name: 'Loose' } })
        expect(rig.editor.Name).toBe('Loose')
        expect(rig.editor.IsEditable).toBe(true)
    })

    it('catalog_failure_stays_visible_and_is_retried_on_the_next_open', async () =>
    {
        const rig = new Rig()
        void rig.editor.LoadCatalog()
        await rig.Fail('list_catalog', 'down')
        expect(rig.editor.CatalogError).toBe('catalog unavailable: down')
        expect(rig.editor.HasCatalogError).toBe(true)
        void rig.editor.Open(new TemplateTreeNodeViewModel(Rig.Leaf))
        const retried = rig.Frames().filter(f => f.method === 'list_catalog').length
        expect(retried).toBe(2)
        await rig.Respond('get_template', { id: 't1', path: 'Examples', template: { ...Rig.Researcher, model: 'gone-model' } })
        // The catalog is still unknown: the error stays, and nothing is marked as missing from it.
        expect(rig.editor.CatalogError).toBe('catalog unavailable: down')
        expect(rig.editor.ModelChoices.ToArray().map(c => c.Label)).toEqual(['gone-model'])
        expect(rig.editor.RuntimeChoices.ToArray().map(c => c.Label)).toEqual(['llamacpp'])
        await rig.Respond('list_catalog', Rig.Catalog)
        await rig.Respond('list_all_tools', Rig.Tools)
        expect(rig.editor.CatalogError).toBe('')
        expect(rig.editor.ModelChoices.ToArray().map(c => c.Label)).toEqual(['gemma-4', 'tiny', 'gone-model (not in catalog)'])
        // Once known, no further retry.
        rig.editor.EnsureCatalog()
        expect(rig.Frames().filter(f => f.method === 'list_catalog').length).toBe(2)
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
        // Every registry tool starts enabled, as in the seeded Default.
        expect(e.Tools.ToArray().map(t => t.Enabled)).toEqual([true, true])
        expect(e.IsNew).toBe(true)
        expect(e.IsDirty).toBe(true)
        expect(e.Runtime).toBe('llamacpp')
        expect(e.Model).toBe('gemma-4')
        e.SaveCommand.Execute(undefined)
        const save = rig.Frames().pop()!
        expect(save.params.id).toBeUndefined()
        expect(save.params.path).toBe('Examples')
        expect((save.params.template as AgentTemplate).tools).toEqual(['read_file', 'get_time'])
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
