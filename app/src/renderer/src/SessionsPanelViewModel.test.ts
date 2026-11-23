import { describe, expect, it } from 'vitest'
import { RuntimeClient } from './RuntimeClient'
import { FakeSocket } from './FakeSocket'
import { SessionsPanelViewModel } from './SessionsPanelViewModel'
import { SessionStateKind } from './SessionRowViewModel'
import type { Placement, SessionSummary } from './agents'

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

const Ready: Placement = { state: 'ready', host: 'local' }
const NeedsLoad: Placement = { state: 'needs_load', host: 'local', evicts: 'gemma-3' }
const Unavailable: Placement = { state: 'unavailable', reason: 'no host serves gemma-9' }

describe('SessionsPanelViewModel', () =>
{
    it('sessions_panel_lists_sessions_with_state', async () =>
    {
        const rig = new Rig()
        await rig.Load([Rig.Row('a', Ready), Rig.Row('b', NeedsLoad), Rig.Row('c', Unavailable), Rig.Row('d', Ready, true)])
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
        await rig.Load([Rig.Row('a', Ready), Rig.Row('b', NeedsLoad)])
        const row = rig.panel.Sessions.Get(0)!
        const changed: string[] = []
        row.PropertyChanged('StateKind').subscribe(() => changed.push('StateKind'))
        rig.Notify('placement_changed', { id: 'a', placement: NeedsLoad })
        expect(row.StateKind).toBe(SessionStateKind.Idle)
        expect(changed).toEqual(['StateKind'])
        expect(rig.panel.Sessions.Get(1)!.StateKind).toBe(SessionStateKind.Idle)
    })

    it('session_busy_drives_row_busy', async () =>
    {
        const rig = new Rig()
        await rig.Load([Rig.Row('a', Ready)])
        const row = rig.panel.Sessions.Get(0)!
        rig.Notify('session_busy', { id: 'a', busy: true })
        expect([row.Busy, row.StateKind]).toEqual([true, SessionStateKind.Busy])
        rig.Notify('session_busy', { id: 'a', busy: false })
        expect([row.Busy, row.StateKind]).toEqual([false, SessionStateKind.Live])
    })

    it('selecting_a_row_raises_open_once', async () =>
    {
        const rig = new Rig()
        await rig.Load([Rig.Row('a', Ready), Rig.Row('b', NeedsLoad)])
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
        await rig.Load([Rig.Row('a', Ready), Rig.Row('b', NeedsLoad)])
        const selected = rig.panel.Sessions.Get(1)!
        rig.panel.SelectedSession = selected
        rig.Notify('sessions_changed', {})
        await rig.List([Rig.Row('c', Ready), { ...Rig.Row('b', NeedsLoad), title: 'Renamed' }, Rig.Row('a', Ready)])
        expect(rig.panel.Sessions.ToArray().map(r => r.Id)).toEqual(['c', 'b', 'a'])
        expect(rig.panel.SelectedSession).toBe(selected)
        expect(selected.Title).toBe('Renamed')
        expect(rig.opened).toEqual(['b'])
        expect(rig.lost).toEqual([])
    })

    it('refresh_dropping_selected_row_raises_selection_lost', async () =>
    {
        const rig = new Rig()
        await rig.Load([Rig.Row('a', Ready), Rig.Row('b', NeedsLoad)])
        rig.panel.SelectedSession = rig.panel.Sessions.Get(0)!
        rig.Notify('sessions_changed', {})
        await rig.List([Rig.Row('b', NeedsLoad)])
        expect(rig.lost).toEqual(['a'])
        expect(rig.panel.SelectedSession).toBeNull()
    })

    it('row_commands_signal_the_owner', async () =>
    {
        const rig = new Rig()
        await rig.Load([Rig.Row('a', Ready)])
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
        await rig.Load([Rig.Row('a', Ready)])
        rig.panel.Bind(null)
        expect(rig.panel.Sessions.Count).toBe(0)
        expect(rig.panel.IsEmpty).toBe(true)
        expect(rig.panel.NewSessionCommand.CanExecute(undefined)).toBe(false)
    })
})
