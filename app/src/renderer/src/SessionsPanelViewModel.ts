import { Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { TypedSignal, type IDisposable } from './JsonRpcConnection'
import type { RuntimeClient } from './RuntimeClient'
import type { SessionSummary } from './agents'
import { SessionRowViewModel, type ISessionRowOwner } from './SessionRowViewModel'
import { ConnectionState } from './ConnectionState'
import { TextFormat } from './TextFormat'

/**
 * The Sessions side panel: a New session button over the session list. Selecting a row raises OnOpen; the list
 * refreshes on `sessions_changed` (which also follows every committed turn), updating rows in place so the selection
 * is kept and nothing is re-opened. Row states follow `placement_changed` and `session_busy`. The dialogs behind
 * New / Rename / Delete belong to the owning service, which the panel only signals. While the shown session runs a
 * turn the owner locks switching: a click on another row snaps back with a notice (switching would abort the turn).
 */
export class SessionsPanelViewModel extends Observable implements ISessionRowOwner
{
    private static readonly PropSelectedSession = 'SelectedSession'
    private static readonly PropStatusText = 'StatusText'
    private static readonly PropIsEmpty = 'IsEmpty'
    private static readonly PropEmptyText = 'EmptyText'
    private static readonly PropShowsEmptyText = 'ShowsEmptyText'
    private static readonly RefreshFailedFormat = 'sessions unavailable: {0}'
    private static readonly SwitchLockedText = 'Finish the running turn before switching sessions.'
    private static readonly NoSessionsText = 'No sessions yet.'
    private static readonly ReconnectingText = 'Connecting to the runtime…'
    private static readonly RuntimeOffFormat = 'The runtime is off on the supervisor: {0}'
    private static readonly DisconnectedText = 'Not connected: set the supervisor URL in Settings.'

    public readonly Sessions = new ObservableCollection<SessionRowViewModel>()
    /** A row was chosen by the user: open that session. */
    public readonly OnOpen = new TypedSignal<string>()
    /** The selected session disappeared from the list (deleted). */
    public readonly OnSelectionLost = new TypedSignal<string>()
    public readonly OnNewSessionRequested = new TypedSignal<void>()
    public readonly OnRenameRequested = new TypedSignal<SessionRowViewModel>()
    public readonly OnDeleteRequested = new TypedSignal<SessionRowViewModel>()

    /** Renames the row passed as the parameter, else the selected one. */
    public readonly RenameCommand: ICommand
    /** Deletes the row passed as the parameter, else the selected one. */
    public readonly DeleteCommand: ICommand

    private readonly newSessionCommand: RelayCommand
    private _selected: SessionRowViewModel | null = null
    private _statusText = ''
    private connection = ConnectionState.Disconnected
    /** Why the supervisor says the runtime is off ("" when it is not). */
    private runtimeError = ''
    /**
     * Counts every placement / busy notification; a row's last one is recorded per field, so a list reply computed
     * before a newer notification does not overwrite it.
     */
    private noteCounter = 0
    private readonly placementNoted = new Map<string, number>()
    private readonly busyNoted = new Map<string, number>()
    private switchLocked = false
    private client: RuntimeClient | null = null
    private subscriptions: IDisposable[] = []
    /** Orders refreshes so a slow early reply cannot overwrite a newer one. */
    private refreshToken = 0

    constructor()
    {
        super()
        this.newSessionCommand = new RelayCommand(() => this.OnNewSessionRequested.Raise(undefined), () => this.client !== null && !this.switchLocked)
        this.RenameCommand = new RelayCommand(arg => this.ForRow(arg, row => this.RequestRename(row)))
        this.DeleteCommand = new RelayCommand(arg => this.ForRow(arg, row => this.RequestDelete(row)))
    }

    /** Follows the runtime connection's list-level notifications, or goes inert (and empty) with null. */
    public Bind(client: RuntimeClient | null): void
    {
        for (const s of this.subscriptions)
        {
            s.Dispose()
        }
        this.subscriptions = []
        this.client = client
        this.refreshToken++
        this.placementNoted.clear()
        this.busyNoted.clear()
        if (client !== null)
        {
            this.subscriptions.push(
                client.OnSessionsChanged.Subscribe(() => void this.Refresh()),
                client.OnPlacementChanged.Subscribe(p =>
                {
                    this.placementNoted.set(p.id, ++this.noteCounter)
                    this.Find(p.id)?.SetPlacement(p.placement)
                }),
                client.OnSessionBusy.Subscribe(p =>
                {
                    this.busyNoted.set(p.id, ++this.noteCounter)
                    this.Find(p.id)?.SetBusy(p.busy)
                }))
        }
        else
        {
            this.Reconcile([])
        }
        this.newSessionCommand.RaiseCanExecuteChanged()
    }

    public get NewSessionCommand(): ICommand
    {
        return this.newSessionCommand
    }

    public get SelectedSession(): SessionRowViewModel | null
    {
        return this._selected
    }

    /**
     * A user selection opens the session. Clearing it, or a switch refused while a turn runs, keeps the current row
     * and re-announces it so the list's two-way binding shows it again.
     */
    public set SelectedSession(row: SessionRowViewModel | null)
    {
        if (row === this._selected)
        {
            return
        }
        if (row === null || row === undefined)
        {
            this.ReannounceSelection()
            return
        }
        if (this.switchLocked && this._selected !== null)
        {
            this.ReportSwitchRefused()
            this.ReannounceSelection()
            return
        }
        this.SetSelected(row)
        this.OnOpen.Raise(row.Id)
    }

    /** Locks (a turn runs in the shown session) or unlocks switching to another row. */
    public SetSwitchLocked(locked: boolean): void
    {
        this.switchLocked = locked
        // New session opens the new session, so it is a switch too.
        this.newSessionCommand.RaiseCanExecuteChanged()
        if (!locked && this._statusText === SessionsPanelViewModel.SwitchLockedText)
        {
            this.SetStatus('')
        }
    }

    /** True while a turn in the shown session forbids switching (and so opening another session). */
    public get IsSwitchLocked(): boolean
    {
        return this.switchLocked
    }

    /** Explains a refused switch on the status line. */
    public ReportSwitchRefused(): void
    {
        this.SetStatus(SessionsPanelViewModel.SwitchLockedText)
    }

    /** Shows a failed action (rename, delete, ...) on the panel's status line. */
    public ReportError(text: string): void
    {
        this.SetStatus(text)
    }

    /** The runtime connection as the owner reports it; anything but Connected replaces the empty-list hint. */
    public SetConnection(state: ConnectionState): void
    {
        if (this.connection !== state)
        {
            this.ChangeHint(() => { this.connection = state })
        }
    }

    /** Why the supervisor says the runtime is off ("" when it is not); shown instead of "Connecting…". */
    public SetRuntimeError(text: string): void
    {
        if (this.runtimeError !== text)
        {
            this.ChangeHint(() => { this.runtimeError = text })
        }
    }

    /** The hint shown above the list: the connection state (or why the runtime is off), or that there are no sessions. */
    public get EmptyText(): string
    {
        if (this.connection !== ConnectionState.Connected && this.runtimeError.length > 0)
        {
            return TextFormat.Format(SessionsPanelViewModel.RuntimeOffFormat, this.runtimeError)
        }
        switch (this.connection)
        {
            case ConnectionState.Connected:
                return SessionsPanelViewModel.NoSessionsText
            case ConnectionState.Reconnecting:
                return SessionsPanelViewModel.ReconnectingText
            default:
                return SessionsPanelViewModel.DisconnectedText
        }
    }

    public get ShowsEmptyText(): boolean
    {
        return this.connection !== ConnectionState.Connected || this.IsEmpty
    }

    public get StatusText(): string
    {
        return this._statusText
    }

    public get IsEmpty(): boolean
    {
        return this.Sessions.Count === 0
    }

    /** Re-reads the list and reconciles the rows by id. */
    public async Refresh(): Promise<void>
    {
        const client = this.client
        if (client === null)
        {
            return
        }
        const token = ++this.refreshToken
        const asked = this.noteCounter
        try
        {
            const sessions = await client.ListSessions()
            if (token === this.refreshToken)
            {
                this.SetStatus('')
                this.Reconcile(sessions.map(s => this.KeepNewerNotes(s, asked)))
            }
        }
        catch (e)
        {
            if (token === this.refreshToken)
            {
                this.SetStatus(TextFormat.Format(SessionsPanelViewModel.RefreshFailedFormat, TextFormat.Message(e)))
            }
        }
    }

    /** Adds (or updates) a row for a session the caller just created, and selects it without raising OnOpen. */
    public Show(summary: SessionSummary): SessionRowViewModel
    {
        const row = this.Add(summary)
        this.SetSelected(row)
        return row
    }

    /** Adds (or updates) a row for a session the caller just created, leaving the selection alone. */
    public Add(summary: SessionSummary): SessionRowViewModel
    {
        let row = this.Find(summary.id)
        if (row === undefined)
        {
            row = new SessionRowViewModel(this, summary)
            const wasEmpty = this.IsEmpty
            this.Sessions.Insert(0, row)
            this.RaiseIsEmpty(wasEmpty)
        }
        else
        {
            row.Update(summary)
        }
        return row
    }

    /** Drops the selection without raising anything (the shown session was closed). */
    public ClearSelection(): void
    {
        this.SetSelected(null)
    }

    public Find(id: string): SessionRowViewModel | undefined
    {
        return this.Sessions.ToArray().find(r => r.Id === id)
    }

    public RequestRename(row: SessionRowViewModel): void
    {
        this.OnRenameRequested.Raise(row)
    }

    public RequestDelete(row: SessionRowViewModel): void
    {
        this.OnDeleteRequested.Raise(row)
    }

    /**
     * Makes the rows match the server list (its order) in place: existing rows are updated and moved, new ones
     * inserted, gone ones removed. No reset is raised, so the list keeps its containers and its selection.
     */
    private Reconcile(sessions: readonly SessionSummary[]): void
    {
        const wasEmpty = this.IsEmpty
        const existing = new Map(this.Sessions.ToArray().map(r => [r.Id, r]))
        const rows = sessions.map(summary =>
        {
            const row = existing.get(summary.id)
            if (row === undefined)
            {
                return new SessionRowViewModel(this, summary)
            }
            row.Update(summary)
            return row
        })
        rows.forEach((row, index) =>
        {
            const at = this.Sessions.IndexOf(row)
            if (at < 0)
            {
                this.Sessions.Insert(index, row)
            }
            else if (at !== index)
            {
                this.Sessions.Move(at, index)
            }
        })
        // Every kept row now sits in front, so what remains at the tail is gone from the server.
        while (this.Sessions.Count > rows.length)
        {
            this.Sessions.RemoveAt(this.Sessions.Count - 1)
        }
        this.RaiseIsEmpty(wasEmpty)
        const selected = this._selected
        if (selected !== null && !rows.includes(selected))
        {
            this.SetSelected(null)
            this.OnSelectionLost.Raise(selected.Id)
        }
    }

    /**
     * The listed row, except that a placement or busy flag notified after the list was asked for (at `asked`) is
     * newer than the reply, so the row keeps the notified value.
     */
    private KeepNewerNotes(summary: SessionSummary, asked: number): SessionSummary
    {
        const row = this.Find(summary.id)
        if (row === undefined)
        {
            return summary
        }
        const placementNewer = (this.placementNoted.get(summary.id) ?? 0) > asked
        const busyNewer = (this.busyNoted.get(summary.id) ?? 0) > asked
        return {
            ...summary,
            placement: placementNewer ? row.Placement : summary.placement,
            busy: busyNewer ? row.Busy : summary.busy,
        }
    }

    /** Applies a change to what the hint depends on, then raises the hint properties. */
    private ChangeHint(mutate: () => void): void
    {
        const oldText = this.EmptyText
        const oldShows = this.ShowsEmptyText
        mutate()
        this.RaisePropertyChanged(SessionsPanelViewModel.PropEmptyText, oldText, this.EmptyText)
        this.RaisePropertyChanged(SessionsPanelViewModel.PropShowsEmptyText, oldShows, this.ShowsEmptyText)
    }

    private ForRow(arg: unknown, action: (row: SessionRowViewModel) => void): void
    {
        const row = arg instanceof SessionRowViewModel ? arg : this._selected
        if (row !== null)
        {
            action(row)
        }
    }

    private SetSelected(row: SessionRowViewModel | null): void
    {
        const old = this._selected
        if (old === row)
        {
            return
        }
        this._selected = row
        this.RaisePropertyChanged(SessionsPanelViewModel.PropSelectedSession, old, row)
    }

    /** Re-raises the selection after the current binding write, so a two-way list shows the kept row again. */
    private ReannounceSelection(): void
    {
        void Promise.resolve().then(() =>
        {
            this.RaisePropertyChanged(SessionsPanelViewModel.PropSelectedSession, undefined, this._selected)
        })
    }

    private SetStatus(text: string): void
    {
        const old = this._statusText
        if (old !== text)
        {
            this._statusText = text
            this.RaisePropertyChanged(SessionsPanelViewModel.PropStatusText, old, text)
        }
    }

    private RaiseIsEmpty(wasEmpty: boolean): void
    {
        if (wasEmpty !== this.IsEmpty)
        {
            this.RaisePropertyChanged(SessionsPanelViewModel.PropIsEmpty, wasEmpty, this.IsEmpty)
            this.RaisePropertyChanged(SessionsPanelViewModel.PropShowsEmptyText, undefined, this.ShowsEmptyText)
        }
    }
}
