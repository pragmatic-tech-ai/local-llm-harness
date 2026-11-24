import { describe, expect, it } from 'vitest'
import { RuntimeClient } from './RuntimeClient'
import { FakeSocket } from './FakeSocket'
import { SessionViewModel } from './SessionViewModel'
import { PlacementState, type AttachedSession, type Placement, type ProviderCapabilities } from './agents'

class Rig
{
    public readonly socket = new FakeSocket()
    public readonly client = new RuntimeClient(() => this.socket)
    public readonly view = new SessionViewModel(this.client)

    constructor()
    {
        this.client.Connect('ws://runtime')
        this.socket.Open()
    }

    public static Attached(placement: Placement): AttachedSession
    {
        const summary = { id: 's1', title: 'Default #1', template_id: 'tpl', template_name: 'Default', runtime: 'llamacpp', model: 'gemma-3-4b', updated: 1, placement, busy: false }
        return {
            summary,
            snapshot: { name: 'Researcher', system_prompt: '', tools: ['read_file', 'get_time'], step_budget: 8, runtime: 'llamacpp', model: 'gemma-3-4b', n_ctx: 8192, enable_thinking: false },
            transcript: [],
            sampling: { temperature: 0.8, top_p: 0.95, top_k: 40, repeat_penalty: 1.1, seed: -1, max_tokens: 256 },
            enable_thinking: false,
            placement,
        }
    }

    public IdOf(method: string): number
    {
        const frames = this.socket.sent.map(f => JSON.parse(f) as { id: number, method: string })
        return frames.filter(f => f.method === method).pop()?.id ?? -1
    }

    public Frame(method: string): { id: number, method: string, params: unknown } | undefined
    {
        return this.socket.sent.map(f => JSON.parse(f)).filter(f => f.method === method).pop()
    }

    public async Respond(method: string, result: unknown): Promise<void>
    {
        this.client.HandleFrame(JSON.stringify({ id: this.IdOf(method), result }))
        await new Promise(r => setTimeout(r, 0))
    }

    public Notify(method: string, params: unknown): void
    {
        this.client.HandleFrame(JSON.stringify({ method, params }))
    }

    public async Show(placement: Placement): Promise<void>
    {
        const done = this.view.Attach('s1')
        await this.Respond('attach_session', Rig.Attached(placement))
        await done
    }
}

describe('SessionViewModel', () =>
{
    it('load_button_label_names_eviction', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: PlacementState.NeedsLoad, host: 'local', evicts: 'gemma-4-12B-it-QAT-Q4_0' })
        expect(rig.view.CanLoad).toBe(true)
        expect(rig.view.LoadLabel).toBe('Load model (evicts gemma-4-12B-it-QAT-Q4_0)')
        expect(rig.view.PlacementText).toBe('model not loaded')

        const free = new Rig()
        await free.Show({ state: PlacementState.NeedsLoad, host: 'local', evicts: null })
        expect(free.view.LoadLabel).toBe('Load model')
    })

    it('header_shows_template_model_host_and_tools', async () =>
    {
        const rig = new Rig()
        expect(rig.view.HasSession).toBe(false)
        expect(rig.view.IsEmpty).toBe(true)
        await rig.Show({ state: PlacementState.Ready, host: 'local' })
        expect(rig.view.HasSession).toBe(true)
        expect(rig.view.HeaderText).toBe('Researcher · gemma-3-4b @ local')
        expect(rig.view.PlacementText).toBe('live on local')
        expect(rig.view.ToolsText).toBe('tools: read_file, get_time')
        expect(rig.view.CanLoad).toBe(false)
        expect(rig.view.CanUnload).toBe(true)
        expect(rig.view.Chat.CanChat).toBe(true)
    })

    it('unavailable_shows_the_reason', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: PlacementState.Unavailable, reason: 'no host serves gemma-9' })
        expect(rig.view.PlacementText).toBe('unavailable: no host serves gemma-9')
        expect(rig.view.HeaderText).toBe('Researcher · gemma-3-4b')
        expect(rig.view.CanLoad).toBe(false)
    })

    it('load_command_loads_and_shows_ready', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: PlacementState.NeedsLoad, host: 'local', evicts: 'gemma-4' })
        rig.view.LoadCommand.Execute(undefined)
        expect(rig.Frame('load_session_model')?.params).toEqual({ id: 's1' })
        expect(rig.view.PlacementText).toBe('loading model…')
        expect(rig.view.CanLoad).toBe(false)
        const propsBefore = rig.socket.sent.filter(f => JSON.parse(f).method === 'model_props').length
        await rig.Respond('load_session_model', { state: PlacementState.Ready, host: 'local' })
        expect(rig.view.PlacementText).toBe('live on local')
        expect(rig.socket.sent.filter(f => JSON.parse(f).method === 'model_props').length).toBe(propsBefore + 1)
    })

    it('load_failure_is_shown', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: PlacementState.NeedsLoad, host: 'local', evicts: null })
        rig.view.LoadCommand.Execute(undefined)
        rig.client.HandleFrame(JSON.stringify({ id: rig.IdOf('load_session_model'), error: { code: -32603, message: 'out of memory' } }))
        await new Promise(r => setTimeout(r, 0))
        expect(rig.view.PlacementText).toBe('load failed: out of memory')
        expect(rig.view.CanLoad).toBe(true)
    })

    it('unload_command_and_placement_changed', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: PlacementState.Ready, host: 'local' })
        rig.view.UnloadCommand.Execute(undefined)
        expect(rig.Frame('unload_session_model')?.params).toEqual({ id: 's1' })
        rig.Notify('placement_changed', { id: 's1', placement: { state: PlacementState.NeedsLoad, host: 'local', evicts: null } })
        expect(rig.view.CanLoad).toBe(true)
        rig.Notify('placement_changed', { id: 'other', placement: { state: PlacementState.Ready, host: 'local' } })
        expect(rig.view.CanLoad).toBe(true)
    })

    it('seeds_panel_from_attached_session', async () =>
    {
        const rig = new Rig()
        const done = rig.view.Attach('s1')
        const attached = Rig.Attached({ state: PlacementState.Ready, host: 'local' })
        await rig.Respond('attach_session', { ...attached, sampling: { ...attached.sampling, temperature: 0.3 }, enable_thinking: true })
        await done
        expect(rig.view.Panel.Temperature).toBe(0.3)
        expect(rig.view.Panel.EnableThinking).toBe(true)
    })

    it('stale_attach_reply_is_dropped', async () =>
    {
        const rig = new Rig()
        const first = rig.view.Attach('s1')
        const firstId = rig.IdOf('attach_session')
        const second = rig.view.Attach('s2')
        const attached = Rig.Attached({ state: PlacementState.Ready, host: 'local' })
        rig.client.HandleFrame(JSON.stringify({ id: firstId, result: attached }))
        await first
        expect(rig.view.HasSession).toBe(false)
        await rig.Respond('attach_session', { ...attached, snapshot: { ...attached.snapshot, name: 'Second' } })
        await second
        expect(rig.view.ShownId).toBe('s2')
        expect(rig.view.HeaderText).toBe('Second · gemma-3-4b @ local')
    })

    it('first_message_load_disables_load_until_the_turn_starts', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: PlacementState.NeedsLoad, host: 'local', evicts: null })
        expect(rig.view.CanLoad).toBe(true)
        rig.Notify('session_loading', { id: 's1' })
        expect(rig.view.CanLoad).toBe(false)
        expect(rig.view.PlacementText).toBe('loading model…')
        rig.Notify('turn_started', { prompt: 'p', prompt_tokens: 1, n_ctx: 8 })
        expect(rig.view.PlacementText).toBe('model not loaded')
        rig.Notify('session_loading', { id: 's1' })
        rig.Notify('loop_done', { steps: 0, reason: 'no_harness' })
        expect(rig.view.CanLoad).toBe(true)
        rig.Notify('session_loading', { id: 'other' })
        expect(rig.view.CanLoad).toBe(true)
    })

    it('a_reconnect_during_a_first_message_load_ends_loading_model', async () =>
    {
        const rig = new Rig()
        const needsLoad: Placement = { state: PlacementState.NeedsLoad, host: 'local', evicts: null }
        await rig.Show(needsLoad)
        rig.Notify('session_loading', { id: 's1' })
        expect(rig.view.PlacementText).toBe('loading model…')
        // The socket drops before turn_started: the turn is lost with it.
        rig.view.Detach()
        expect(rig.view.PlacementText).toBe('model not loaded')
        // Re-attached (same session): Load follows the placement again.
        await rig.Show(needsLoad)
        expect(rig.view.PlacementText).toBe('model not loaded')
        expect(rig.view.CanLoad).toBe(true)
    })

    it('an_attach_reply_older_than_a_placement_change_keeps_the_change', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: PlacementState.NeedsLoad, host: 'local', evicts: null })
        const ready: Placement = { state: PlacementState.Ready, host: 'local' }
        // Re-attach; the model finishes loading before the (older) reply lands.
        const done = rig.view.Attach('s1')
        rig.Notify('placement_changed', { id: 's1', placement: ready })
        await rig.Respond('attach_session', Rig.Attached({ state: PlacementState.NeedsLoad, host: 'local', evicts: null }))
        await done
        expect(rig.view.PlacementText).toBe('live on local')
        expect(rig.view.CanUnload).toBe(true)
    })

    it('stale_marks_placement_and_disables_load', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: PlacementState.NeedsLoad, host: 'local', evicts: null })
        rig.view.SetStale(true)
        expect(rig.view.PlacementText).toBe('model not loaded (stale: not connected)')
        expect(rig.view.CanLoad).toBe(false)
        rig.view.SetStale(false)
        expect(rig.view.CanLoad).toBe(true)
    })
    it('attach_loads_trace_and_reconnect_reloads', async () =>
    {
        const rig = new Rig()
        const turn = (n: number) => ({ turn: n, steps: [], summary: null, reset_before: false })
        expect(rig.view.Panel.Trace).toBe(rig.view.Trace)
        await rig.Show({ state: PlacementState.Ready, host: 'local' })
        expect(rig.Frame('get_trace')?.params).toEqual({ id: 's1' })
        await rig.Respond('get_trace', { turns: [turn(1)], more: false, reset_pending: false })
        expect(rig.view.Trace.Turns.Count).toBe(1)
        // The connection drops and comes back: the re-attach reloads the first page.
        rig.view.Detach()
        await rig.Show({ state: PlacementState.Ready, host: 'local' })
        expect(rig.socket.sent.filter(f => JSON.parse(f).method === 'get_trace').length).toBe(2)
        await rig.Respond('get_trace', { turns: [turn(2), turn(1)], more: false, reset_pending: false })
        expect(rig.view.Trace.Turns.ToArray().map(t => t.Turn)).toEqual([2, 1])
        // Switching sessions clears the trace at once; clearing the view leaves it empty.
        void rig.view.Attach('s2')
        expect(rig.view.Trace.Turns.Count).toBe(0)
        rig.view.Clear()
        expect(rig.view.Trace.Turns.Count).toBe(0)
    })

    it('attach_to_a_busy_session_shows_its_unfinished_turn_running', async () =>
    {
        const rig = new Rig()
        const attached = Rig.Attached({ state: PlacementState.Ready, host: 'local' })
        const done = rig.view.Attach('s1')
        await rig.Respond('attach_session', { ...attached, summary: { ...attached.summary, busy: true } })
        await done
        await rig.Respond('get_trace', { turns: [{ turn: 3, steps: [], summary: null, reset_before: false }], more: false, reset_pending: false })
        expect(rig.view.Trace.Turns.Get(0)!.HeaderText).toBe('Turn 3 · running…')
        rig.Notify('session_busy', { id: 's1', busy: false })
        expect(rig.view.Trace.Turns.Get(0)!.HeaderText).toBe('Turn 3 · incomplete')
    })

    it('session_loads_notes_on_open', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: PlacementState.Ready, host: 'local' })
        expect(rig.view.HasNotes).toBe(false)
        expect(rig.Frame('get_notes')?.params).toEqual({ id: 's1' })
        await rig.Respond('get_notes', { notes: ['plan A', 'plan B'] })
        expect(rig.view.HasNotes).toBe(true)
        expect(rig.view.IsNotesExpanded).toBe(true)
        expect(rig.view.Notes.ToArray().map(n => n.Text)).toEqual(['1. plan A', '2. plan B'])
    })

    it('notes_changed_for_other_session_is_ignored', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: PlacementState.Ready, host: 'local' })
        await rig.Respond('get_notes', { notes: ['plan A'] })
        rig.Notify('notes_changed', { id: 'other', notes: ['x', 'y'] })
        expect(rig.view.Notes.ToArray().map(n => n.Text)).toEqual(['1. plan A'])
    })

    it('notes_changed_replaces_list_and_empty_hides_panel', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: PlacementState.Ready, host: 'local' })
        await rig.Respond('get_notes', { notes: ['plan A'] })
        rig.Notify('notes_changed', { id: 's1', notes: ['a', 'b', 'c'] })
        expect(rig.view.Notes.ToArray().map(n => n.Text)).toEqual(['1. a', '2. b', '3. c'])
        expect(rig.view.HasNotes).toBe(true)
        rig.Notify('notes_changed', { id: 's1', notes: [] })
        expect(rig.view.Notes.Count).toBe(0)
        expect(rig.view.HasNotes).toBe(false)
    })

    it('notes_change_raises_has_notes_only', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: PlacementState.Ready, host: 'local' })
        const raised: string[] = []
        for (const name of ['HasNotes', 'HeaderText', 'PlacementText', 'CanLoad'])
        {
            rig.view.PropertyChanged(name).subscribe(() => { raised.push(name) })
        }
        await rig.Respond('get_notes', { notes: ['plan A'] })
        expect(raised).toEqual(['HasNotes'])
    })

    it('notes_expander_toggles_and_notifies', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: PlacementState.Ready, host: 'local' })
        let raised = 0
        rig.view.PropertyChanged('IsNotesExpanded').subscribe(() => { raised++ })
        expect(rig.view.IsNotesExpanded).toBe(true)
        rig.view.IsNotesExpanded = false
        expect(rig.view.IsNotesExpanded).toBe(false)
        expect(raised).toBe(1)
        // Setting the same value again is not a change.
        rig.view.IsNotesExpanded = false
        expect(raised).toBe(1)
        rig.view.IsNotesExpanded = true
        expect(rig.view.IsNotesExpanded).toBe(true)
        expect(raised).toBe(2)
    })

    it('notes_header_counts_and_marks_folding', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: PlacementState.Ready, host: 'local' })
        await rig.Respond('get_notes', { notes: ['plan A', 'plan B'] })
        expect(rig.view.NotesHeader).toBe('▾ Notes (2)')
        let raised = 0
        rig.view.PropertyChanged('NotesHeader').subscribe(() => { raised++ })
        rig.view.IsNotesExpanded = false
        expect(rig.view.NotesHeader).toBe('▸ Notes (2)')
        expect(raised).toBe(1)
    })

    it('switching_sessions_clears_notes_at_once', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: PlacementState.Ready, host: 'local' })
        await rig.Respond('get_notes', { notes: ['plan A'] })
        expect(rig.view.HasNotes).toBe(true)
        let raised = 0
        rig.view.PropertyChanged('HasNotes').subscribe(() => { raised++ })
        const done = rig.view.Attach('s2')
        // Reset on the switch: the old session's notes are gone before the new attach lands.
        expect(rig.view.Notes.Count).toBe(0)
        expect(rig.view.HasNotes).toBe(false)
        expect(raised).toBeGreaterThan(0)
        const attached = Rig.Attached({ state: PlacementState.Ready, host: 'local' })
        await rig.Respond('attach_session', { ...attached, summary: { ...attached.summary, id: 's2' } })
        await done
        expect(rig.Frame('get_notes')?.params).toEqual({ id: 's2' })
        await rig.Respond('get_notes', { notes: ['plan B'] })
        expect(rig.view.Notes.ToArray().map(n => n.Text)).toEqual(['1. plan B'])
    })
    it('load_buttons_hidden_without_load_unload', async () =>
    {
        const remote: ProviderCapabilities = { measure_prompt: 'estimated', tokenize: false, raw_prompt: false, logprobs: false, load_unload: false }
        const rig = new Rig()
        expect(rig.view.IsLoadable).toBe(true)
        const raised: string[] = []
        for (const name of ['IsLoadable', 'LoadLabel'])
        {
            rig.view.PropertyChanged(name).subscribe(() => raised.push(name))
        }
        const done = rig.view.Attach('s1')
        await rig.Respond('attach_session', { ...Rig.Attached({ state: PlacementState.Ready, host: 'or' }), capabilities: remote })
        await done
        expect(rig.view.Capabilities).toEqual(remote)
        expect(rig.view.IsLoadable).toBe(false)
        // The buttons disappear after their label changed (HiddenTextRule).
        expect(raised.lastIndexOf('IsLoadable')).toBeGreaterThan(raised.lastIndexOf('LoadLabel'))
        expect(rig.view.Panel.IsTokenizeAvailable).toBe(false)

        const local = new Rig()
        await local.Show({ state: PlacementState.Ready, host: 'local' })
        // Absent (an older runtime) or full capabilities keep Load and Unload.
        expect(local.view.Capabilities).toBe(null)
        expect(local.view.IsLoadable).toBe(true)
        const gone = new Rig()
        const goneDone = gone.view.Attach('s1')
        await gone.Respond('attach_session', { ...Rig.Attached({ state: PlacementState.Unavailable, reason: 'repository not found: or' }), capabilities: null })
        await goneDone
        expect(gone.view.IsLoadable).toBe(true)
        expect(gone.view.Panel.IsTokenizeAvailable).toBe(true)
    })

    it('clear_resets_the_panel_capabilities', async () =>
    {
        const remote: ProviderCapabilities = { measure_prompt: 'estimated', tokenize: false, raw_prompt: false, logprobs: false, load_unload: false }
        const rig = new Rig()
        const done = rig.view.Attach('s1')
        await rig.Respond('attach_session', { ...Rig.Attached({ state: PlacementState.Ready, host: 'or' }), capabilities: remote })
        await done
        expect(rig.view.Panel.IsTokenizeAvailable).toBe(false)
        rig.view.Clear()
        expect(rig.view.Capabilities).toBe(null)
        expect(rig.view.Panel.IsTokenizeAvailable).toBe(true)
        expect(rig.view.Panel.IsRawPromptAvailable).toBe(true)
    })
})
