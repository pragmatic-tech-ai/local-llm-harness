import { describe, expect, it } from 'vitest'
import { RuntimeClient } from './RuntimeClient'
import { FakeSocket } from './FakeSocket'
import { SessionsPanelViewModel } from './SessionsPanelViewModel'
import { SessionStateKind } from './SessionRowViewModel'
import { PlacementState, type Placement, type SessionSummary } from './agents'
import { ConnectionState } from './ConnectionState'

class Rig
{
    public readonly socket = new FakeSocket()
    public readonly client = new RuntimeClient(() => this.socket)
    public readonly panel = new SessionsPanelViewModel()
    public readonly opened: string[] = []
    public readonly lost: string[] = []

    constructor()
    {
        this.client.Connect('ws://runtime')
        this.socket.Open()
        this.panel.Bind(this.client)
        this.panel.OnOpen.Subscribe(id => this.opened.push(id))
        this.panel.OnSelectionLost.Subscribe(id => this.lost.push(id))
    }

    public static readonly Ready: Placement = { state: PlacementState.Ready, host: 'local' }
    public static readonly NeedsLoad: Placement = { state: PlacementState.NeedsLoad, host: 'local', evicts: 'gemma-3' }
    public static readonly Unavailable: Placement = { state: PlacementState.Unavailable, reason: 'no host serves gemma-9' }

    public static Row(id: string, placement: Placement, busy: boolean = false): SessionSummary
    {
        return { id, title: `T ${id}`, template_id: 'tpl', template_name: 'Default', runtime: 'llamacpp', model: 'gemma', updated: 1, placement, busy }
    }

    public Notify(method: string, params: unknown): void
    {
        this.client.HandleFrame(JSON.stringify({ method, params }))
    }

    public Methods(): string[]
    {
        return this.socket.sent.map(f => (JSON.parse(f) as { method: string }).method)
    }

    /** Answers the latest list_sessions request. */
    public async List(rows: SessionSummary[]): Promise<void>
    {
        const frames = this.socket.sent.map(f => JSON.parse(f) as { id: number, method: string })
        const id = frames.filter(f => f.method === 'list_sessions').pop()?.id ?? -1
        this.client.HandleFrame(JSON.stringify({ id, result: rows }))
        await new Promise(r => setTimeout(r, 0))
    }

    public async Load(rows: SessionSummary[]): Promise<void>
    {
        const done = this.panel.Refresh()
        await this.List(rows)
        await done
    }
}

describe('SessionsPanelViewModel', () =>
{
    it('sessions_panel_lists_sessions_with_state', async () =>
    {
        const rig = new Rig()
        await rig.Load([Rig.Row('a', Rig.Ready), Rig.Row('b', Rig.NeedsLoad), Rig.Row('c', Rig.Unavailable), Rig.Row('d', Rig.Ready, true)])
        const rows = rig.panel.Sessions.ToArray()
        expect(rows.map(r => r.StateKind)).toEqual([SessionStateKind.Live, SessionStateKind.Idle, SessionStateKind.Unavailable, SessionStateKind.Busy])
        expect(rows.map(r => r.StateText)).toEqual(['live', 'idle', 'unavailable: no host serves gemma-9', 'busy'])
        expect(rows[0].Title).toBe('T a')
        expect(rows[0].TemplateName).toBe('Default')
        expect([rows[0].IsLive, rows[1].IsIdle, rows[2].IsUnavailable, rows[3].IsBusy]).toEqual([true, true, true, true])
        expect(rig.panel.IsEmpty).toBe(false)
    })

    it('placement_changed_updates_row', async () =>
    {
        const rig = new Rig()
        await rig.Load([Rig.Row('a', Rig.Ready), Rig.Row('b', Rig.NeedsLoad)])
        const row = rig.panel.Sessions.Get(0)!
        const changed: string[] = []
        row.PropertyChanged('StateKind').subscribe(() => changed.push('StateKind'))
        rig.Notify('placement_changed', { id: 'a', placement: Rig.NeedsLoad })
        expect(row.StateKind).toBe(SessionStateKind.Idle)
        expect(changed).toEqual(['StateKind'])
        expect(rig.panel.Sessions.Get(1)!.StateKind).toBe(SessionStateKind.Idle)
    })

    it('session_busy_drives_row_busy', async () =>
    {
        const rig = new Rig()
        await rig.Load([Rig.Row('a', Rig.Ready)])
        const row = rig.panel.Sessions.Get(0)!
        rig.Notify('session_busy', { id: 'a', busy: true })
        expect([row.Busy, row.StateKind]).toEqual([true, SessionStateKind.Busy])
        rig.Notify('session_busy', { id: 'a', busy: false })
        expect([row.Busy, row.StateKind]).toEqual([false, SessionStateKind.Live])
    })

    it('selecting_a_row_raises_open_once', async () =>
    {
        const rig = new Rig()
        await rig.Load([Rig.Row('a', Rig.Ready), Rig.Row('b', Rig.NeedsLoad)])
        rig.panel.SelectedSession = rig.panel.Sessions.Get(1)!
        rig.panel.SelectedSession = rig.panel.Sessions.Get(1)!
        rig.panel.SelectedSession = null
        expect(rig.opened).toEqual(['b'])
        // Read through another reference: the null assignment above narrows rig.panel.SelectedSession.
        const panel: SessionsPanelViewModel = rig.panel
        expect(panel.SelectedSession?.Id).toBe('b')
    })

    it('sessions_changed_refresh_keeps_selection_without_reopening', async () =>
    {
        const rig = new Rig()
        await rig.Load([Rig.Row('a', Rig.Ready), Rig.Row('b', Rig.NeedsLoad)])
        const selected = rig.panel.Sessions.Get(1)!
        rig.panel.SelectedSession = selected
        rig.Notify('sessions_changed', {})
        await rig.List([Rig.Row('c', Rig.Ready), { ...Rig.Row('b', Rig.NeedsLoad), title: 'Renamed' }, Rig.Row('a', Rig.Ready)])
        expect(rig.panel.Sessions.ToArray().map(r => r.Id)).toEqual(['c', 'b', 'a'])
        expect(rig.panel.SelectedSession).toBe(selected)
        expect(selected.Title).toBe('Renamed')
        expect(rig.opened).toEqual(['b'])
        expect(rig.lost).toEqual([])
    })

    it('refresh_dropping_selected_row_raises_selection_lost', async () =>
    {
        const rig = new Rig()
        await rig.Load([Rig.Row('a', Rig.Ready), Rig.Row('b', Rig.NeedsLoad)])
        rig.panel.SelectedSession = rig.panel.Sessions.Get(0)!
        rig.Notify('sessions_changed', {})
        await rig.List([Rig.Row('b', Rig.NeedsLoad)])
        expect(rig.lost).toEqual(['a'])
        expect(rig.panel.SelectedSession).toBeNull()
    })

    it('row_commands_signal_the_owner', async () =>
    {
        const rig = new Rig()
        await rig.Load([Rig.Row('a', Rig.Ready)])
        const row = rig.panel.Sessions.Get(0)!
        const asked: string[] = []
        rig.panel.OnRenameRequested.Subscribe(r => asked.push(`rename:${r.Id}`))
        rig.panel.OnDeleteRequested.Subscribe(r => asked.push(`delete:${r.Id}`))
        row.RenameCommand.Execute(undefined)
        rig.panel.DeleteCommand.Execute(row)
        expect(asked).toEqual(['rename:a', 'delete:a'])
    })

    it('unbound_panel_is_empty_and_cannot_create', async () =>
    {
        const rig = new Rig()
        await rig.Load([Rig.Row('a', Rig.Ready)])
        rig.panel.Bind(null)
        expect(rig.panel.Sessions.Count).toBe(0)
        expect(rig.panel.IsEmpty).toBe(true)
        expect(rig.panel.NewSessionCommand.CanExecute(undefined)).toBe(false)
    })

    it('reorder_moves_rows_in_place_without_reset', async () =>
    {
        const rig = new Rig()
        await rig.Load([Rig.Row('a', Rig.Ready), Rig.Row('b', Rig.Ready), Rig.Row('c', Rig.Ready)])
        const rows = rig.panel.Sessions.ToArray()
        rig.panel.SelectedSession = rows[2]
        const kinds: string[] = []
        rig.panel.Sessions.Subscribe(change => kinds.push(change.kind))
        rig.Notify('sessions_changed', {})
        await rig.List([Rig.Row('c', Rig.Ready), Rig.Row('d', Rig.Ready), Rig.Row('a', Rig.Ready)])
        expect(rig.panel.Sessions.ToArray().map(r => r.Id)).toEqual(['c', 'd', 'a'])
        expect(kinds).not.toContain('reset')
        expect(kinds).not.toContain('cleared')
        expect(rig.panel.Sessions.Get(0)).toBe(rows[2])
        expect(rig.panel.Sessions.Get(2)).toBe(rows[0])
        expect(rig.panel.SelectedSession).toBe(rows[2])
        expect(rig.opened).toEqual(['c'])
    })

    it('cleared_or_refused_selection_is_reannounced', async () =>
    {
        const rig = new Rig()
        await rig.Load([Rig.Row('a', Rig.Ready), Rig.Row('b', Rig.Ready)])
        rig.panel.SelectedSession = rig.panel.Sessions.Get(0)!
        const announced: unknown[] = []
        rig.panel.PropertyChanged('SelectedSession').subscribe(() => announced.push(rig.panel.SelectedSession))
        rig.panel.SelectedSession = null
        await Promise.resolve()
        rig.panel.SetSwitchLocked(true)
        rig.panel.SelectedSession = rig.panel.Sessions.Get(1)!
        await Promise.resolve()
        expect(announced).toEqual([rig.panel.Sessions.Get(0), rig.panel.Sessions.Get(0)])
        expect(rig.opened).toEqual(['a'])
        expect(rig.panel.StatusText).toBe('Finish the running turn before switching sessions.')
        rig.panel.SetSwitchLocked(false)
        expect(rig.panel.StatusText).toBe('')
    })

    it('empty_text_follows_connection_state', () =>
    {
        const rig = new Rig()
        expect(rig.panel.EmptyText).toBe('Not connected: set the supervisor URL in Settings.')
        rig.panel.SetConnection(ConnectionState.Reconnecting)
        expect(rig.panel.EmptyText).toBe('Connecting to the runtime…')
        rig.panel.SetConnection(ConnectionState.Connected)
        expect(rig.panel.EmptyText).toBe('No sessions yet.')
        expect(rig.panel.ShowsEmptyText).toBe(true)
    })

    it('report_error_shows_on_status_line', () =>
    {
        const rig = new Rig()
        rig.panel.ReportError('delete failed: boom')
        expect(rig.panel.StatusText).toBe('delete failed: boom')
    })

    it('refresh_failure_shows_on_status_line', async () =>
    {
        const rig = new Rig()
        const done = rig.panel.Refresh()
        rig.socket.Close()
        await done
        expect(rig.panel.StatusText).toBe('sessions unavailable: Harness not connected')
    })
})

