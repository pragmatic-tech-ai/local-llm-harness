import { describe, expect, it } from 'vitest'
import { RepositoryEditorViewModel } from './RepositoryEditorViewModel'
import { RuntimeClient } from './RuntimeClient'
import { FakeSocket } from './FakeSocket'
import { RepositoryKind, type RepositorySummary } from './agents'

class EditorRig
{
    public static readonly Caps = { measure_prompt: 'exact' as const, tokenize: true, raw_prompt: true, logprobs: true, load_unload: true }
    public static readonly Hive: RepositorySummary = { name: 'hive', kind: RepositoryKind.Pragmatic, editable: false, capabilities: EditorRig.Caps, model_count: 1 }
    public static readonly FetchedAt = 1700000000
    public static readonly Router: RepositorySummary = {
        name: 'openrouter', kind: RepositoryKind.OpenRouter, editable: true,
        capabilities: { ...EditorRig.Caps, measure_prompt: 'estimated', tokenize: false, raw_prompt: false, logprobs: false, load_unload: false },
        model_count: 1, has_key: true, key_hint: '7f2a', id_prefixes: ['anthropic/', 'google/'], fetched_at: EditorRig.FetchedAt,
    }
    public static readonly Claude = { id: 'anthropic/claude', name: 'Claude', context_length: 200000, prompt_price: 3, completion_price: 15, reasoning: true }

    public readonly socket = new FakeSocket()
    public readonly client = new RuntimeClient(() => this.socket)
    public readonly editor = new RepositoryEditorViewModel(this.client)

    constructor()
    {
        this.client.Connect('ws://runtime')
        this.socket.Open()
    }

    public static async Flush(): Promise<void>
    {
        await new Promise(r => setTimeout(r, 0))
    }

    public static Clock(seconds: number): string
    {
        const at = new Date(seconds * 1000)
        return String(at.getHours()).padStart(2, '0') + ':' + String(at.getMinutes()).padStart(2, '0')
    }

    public Frames(method: string): { id: number, params: Record<string, unknown> }[]
    {
        return this.socket.sent.map(f => JSON.parse(f)).filter(f => f.method === method)
    }

    public async Respond(method: string, result: unknown): Promise<void>
    {
        this.client.HandleFrame(JSON.stringify({ id: this.Frames(method).pop()?.id ?? -1, result }))
        await EditorRig.Flush()
    }

    public async Fail(method: string, message: string): Promise<void>
    {
        this.client.HandleFrame(JSON.stringify({ id: this.Frames(method).pop()?.id ?? -1, error: { code: -32603, message } }))
        await EditorRig.Flush()
    }

    public async OpenRouter(summary: RepositorySummary = EditorRig.Router): Promise<void>
    {
        this.editor.Open(summary)
        await this.Respond('list_repository_models', { models: [EditorRig.Claude], fetched_at: EditorRig.FetchedAt })
    }
}

describe('RepositoryEditorViewModel', () =>
{
    it('starts_empty', () =>
    {
        const rig = new EditorRig()
        expect(rig.editor.IsEmpty).toBe(true)
        expect(rig.editor.HasRepository).toBe(false)
        expect(rig.editor.IsDirty).toBe(false)
    })

    it('pragmatic_editor_is_read_only', async () =>
    {
        const rig = new EditorRig()
        rig.editor.Open(EditorRig.Hive)
        expect(rig.Frames('list_repository_models')[0].params).toEqual({ name: 'hive', refresh: false })
        await rig.Respond('list_repository_models', { models: [{ name: 'gemma-4', runtimes: ['llamacpp', 'candle'], size_hint: null }], fetched_at: null })
        expect(rig.editor.IsPragmatic).toBe(true)
        expect(rig.editor.IsOpenRouter).toBe(false)
        expect(rig.editor.IsEditable).toBe(false)
        expect(rig.editor.SaveCommand.CanExecute(undefined)).toBe(false)
        expect(rig.editor.DeleteCommand.CanExecute(undefined)).toBe(false)
        expect(rig.editor.RefreshCommand.CanExecute(undefined)).toBe(false)
        expect(rig.editor.HasNoKeyWarning).toBe(false)
        expect(rig.editor.Models.Count).toBe(1)
        expect(rig.editor.Models.Get(0)!.Id).toBe('gemma-4')
        expect(rig.editor.Models.Get(0)!.RuntimesText).toBe('llamacpp, candle')
        expect(rig.editor.HeaderText).toBe('hive  Pragmatic')
        expect(rig.editor.IsDirty).toBe(false)
    })

    it('openrouter_editor_shows_key_hint_filter_and_models', async () =>
    {
        const rig = new EditorRig()
        await rig.OpenRouter()
        expect(rig.editor.IsEditable).toBe(true)
        expect(rig.editor.Name).toBe('openrouter')
        expect(rig.editor.KeyHint).toBe('••••7f2a')
        expect(rig.editor.NewKey).toBe('')
        expect(rig.editor.FilterText).toBe('anthropic/, google/')
        expect(rig.editor.Models.Get(0)!.ContextText).toBe('200K')
        expect(rig.editor.Models.Get(0)!.PriceText).toBe('3.00/15.00')
        expect(rig.editor.IsDirty).toBe(false)
        expect(rig.editor.SaveCommand.CanExecute(undefined)).toBe(false)
        expect(rig.editor.DeleteCommand.CanExecute(undefined)).toBe(true)
        expect(rig.editor.RefreshCommand.CanExecute(undefined)).toBe(true)
    })

    it('openrouter_save_sends_key_only_when_changed', async () =>
    {
        const rig = new EditorRig()
        await rig.OpenRouter()
        rig.editor.FilterText = 'anthropic/'
        expect(rig.editor.IsDirty).toBe(true)
        const first = rig.editor.Save()
        expect(rig.Frames('save_repository')[0].params).toEqual({ name: 'openrouter', previous_name: 'openrouter', id_prefixes: ['anthropic/'] })
        await rig.Respond('save_repository', { name: 'openrouter' })
        await expect(first).resolves.toBe(true)
        expect(rig.editor.IsDirty).toBe(false)

        rig.editor.NewKey = 'sk-or-new-key-9999'
        rig.editor.Name = 'router2'
        const second = rig.editor.Save()
        expect(rig.Frames('save_repository')[1].params).toEqual({
            name: 'router2', previous_name: 'openrouter', id_prefixes: ['anthropic/'], api_key: 'sk-or-new-key-9999',
        })
        await rig.Respond('save_repository', { name: 'router2' })
        await second
        // The typed key is dropped once saved; the next save (a rename back) carries none.
        expect(rig.editor.NewKey).toBe('')
        rig.editor.Name = 'openrouter'
        void rig.editor.Save()
        expect(rig.Frames('save_repository')[2].params).toEqual({ name: 'openrouter', previous_name: 'router2', id_prefixes: ['anthropic/'] })
        // The hint is the new key's last 4 characters, never the key.
        expect(rig.editor.KeyHint).toBe('••••9999')
    })

    it('a_new_draft_saves_without_previous_name', async () =>
    {
        const rig = new EditorRig()
        rig.editor.OpenNew(['openrouter'])
        rig.editor.NewKey = 'sk-or-draft-1234'
        rig.editor.NewKey = ''
        expect(rig.editor.IsNew).toBe(true)
        expect(rig.editor.IsDirty).toBe(true)
        expect(rig.editor.Name).toBe('openrouter-2')
        expect(rig.editor.HasNoKeyWarning).toBe(true)
        expect(rig.editor.DeleteCommand.CanExecute(undefined)).toBe(false)
        void rig.editor.Save()
        expect(rig.Frames('save_repository')[0].params).toEqual({ name: 'openrouter-2', id_prefixes: [] })
        await rig.Respond('save_repository', { name: 'openrouter-2', fetch_error: 'openrouter: 503 down' })
        expect(rig.editor.IsNew).toBe(false)
        expect(rig.editor.ErrorText).toBe('openrouter: 503 down')
        expect(rig.editor.FetchedText).toBe('not fetched yet')
    })

    it('a_draft_saved_with_a_key_shows_its_hint', async () =>
    {
        const rig = new EditorRig()
        rig.editor.OpenNew([])
        rig.editor.NewKey = 'sk-or-v1-abcd'
        expect(rig.editor.HasNoKeyWarning).toBe(false)
        void rig.editor.Save()
        expect(rig.Frames('save_repository')[0].params).toEqual({ name: 'openrouter', id_prefixes: [], api_key: 'sk-or-v1-abcd' })
        await rig.Respond('save_repository', { name: 'openrouter' })
        expect(rig.editor.NewKey).toBe('')
        expect(rig.editor.KeyHint).toBe('••••abcd')
        expect(rig.editor.HasNoKeyWarning).toBe(false)
        expect(rig.editor.IsDirty).toBe(false)
    })

    it('filter_text_splits_to_prefixes', async () =>
    {
        const rig = new EditorRig()
        await rig.OpenRouter()
        rig.editor.FilterText = ' anthropic/ ,google/,, , meta-llama/ '
        expect(rig.editor.Prefixes).toEqual(['anthropic/', 'google/', 'meta-llama/'])
        rig.editor.FilterText = '   '
        expect(rig.editor.Prefixes).toEqual([])
    })

    it('refresh_shows_fetched_time_and_errors', async () =>
    {
        const rig = new EditorRig()
        await rig.OpenRouter()
        expect(rig.editor.FetchedText).toBe('fetched ' + EditorRig.Clock(EditorRig.FetchedAt))
        rig.editor.RefreshCommand.Execute(undefined)
        expect(rig.Frames('list_repository_models')[1].params).toEqual({ name: 'openrouter', refresh: true })
        const later = EditorRig.FetchedAt + 3600
        const two = [EditorRig.Claude, { ...EditorRig.Claude, id: 'google/gemini' }]
        await rig.Respond('list_repository_models', { models: two, fetched_at: later })
        expect(rig.editor.Models.Count).toBe(2)
        expect(rig.editor.FetchedText).toBe('fetched ' + EditorRig.Clock(later))
        expect(rig.editor.HasErrorText).toBe(false)

        // A failed fetch keeps the previous list and says why.
        rig.editor.RefreshCommand.Execute(undefined)
        await rig.Respond('list_repository_models', { models: two, fetched_at: later, error: 'openrouter: 401 bad key' })
        expect(rig.editor.ErrorText).toBe('openrouter: 401 bad key')
        expect(rig.editor.Models.Count).toBe(2)

        rig.editor.RefreshCommand.Execute(undefined)
        await rig.Fail('list_repository_models', 'model list unavailable for repository openrouter')
        expect(rig.editor.ErrorText).toBe('model list unavailable for repository openrouter')
    })

    it('no_key_shows_warning', async () =>
    {
        const rig = new EditorRig()
        await rig.OpenRouter({ ...EditorRig.Router, has_key: false, key_hint: undefined })
        expect(rig.editor.HasNoKeyWarning).toBe(true)
        expect(rig.editor.NoKeyWarning).toBe('no API key: models can be listed but not run')
        expect(rig.editor.KeyHint).toBe('not set')
        rig.editor.NewKey = 'sk-typed'
        expect(rig.editor.HasNoKeyWarning).toBe(false)
        rig.editor.NewKey = ''
        expect(rig.editor.HasNoKeyWarning).toBe(true)
        await rig.OpenRouter()
        expect(rig.editor.HasNoKeyWarning).toBe(false)
        // Switching to a repository without a key raises the hint's change.
        const hints: unknown[] = []
        rig.editor.PropertyChanged('KeyHint').subscribe(e => hints.push((e as { newValue?: unknown }).newValue ?? rig.editor.KeyHint))
        await rig.OpenRouter({ ...EditorRig.Router, name: 'bare', has_key: false, key_hint: undefined })
        expect(rig.editor.KeyHint).toBe('not set')
        expect(hints).toContain('not set')
    })

    it('save_error_shows_and_keeps_edits', async () =>
    {
        const rig = new EditorRig()
        await rig.OpenRouter()
        rig.editor.Name = 'bad name'
        const saved = rig.editor.Save()
        await rig.Fail('save_repository', 'repository name must be 1-64 letters, digits, _ or -')
        await expect(saved).resolves.toBe(false)
        expect(rig.editor.ErrorText).toBe('repository name must be 1-64 letters, digits, _ or -')
        expect(rig.editor.IsDirty).toBe(true)
        rig.editor.Revert()
        expect(rig.editor.Name).toBe('openrouter')
        expect(rig.editor.IsDirty).toBe(false)
    })

    it('hidden_texts_read_empty_and_shown_texts_never_do', async () =>
    {
        // mural shows a stale render for a text changed while collapsed and set to the same value once shown.
        const rig = new EditorRig()
        const formTexts = () => [rig.editor.KeyHint, rig.editor.FetchedText]
        expect(formTexts()).toEqual(['', ''])
        expect(rig.editor.CapabilitiesText).toBe('')
        await rig.OpenRouter()
        expect(formTexts().every(t => t.length > 0)).toBe(true)
        rig.editor.Open(EditorRig.Hive)
        await rig.Respond('list_repository_models', { models: [], fetched_at: null })
        expect(formTexts()).toEqual(['', ''])
        rig.editor.OpenNew([])
        expect(formTexts()).toEqual(['not set', 'not fetched yet'])
        expect(rig.editor.HasCapabilities).toBe(false)
        rig.editor.Clear()
        expect(formTexts()).toEqual(['', ''])
    })

    it('a_late_model_list_for_an_earlier_repository_is_dropped', async () =>
    {
        const rig = new EditorRig()
        rig.editor.Open(EditorRig.Router)
        const early = rig.Frames('list_repository_models')[0].id
        rig.editor.Open(EditorRig.Hive)
        rig.client.HandleFrame(JSON.stringify({ id: early, result: { models: [EditorRig.Claude], fetched_at: 1 } }))
        await EditorRig.Flush()
        expect(rig.editor.Models.Count).toBe(0)
        expect(rig.editor.Name).toBe('hive')
    })
})
