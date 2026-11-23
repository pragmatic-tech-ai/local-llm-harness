import { Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { TypedSignal, type IDisposable } from './JsonRpcConnection'
import type { RuntimeClient } from './RuntimeClient'
import type { SessionSummary } from './agents'
import { SessionRowViewModel, type ISessionRowOwner } from './SessionRowViewModel'

/**
 * The Sessions side panel: a New session button over the session list. Selecting a row raises OnOpen; the list
 * refreshes on `sessions_changed` (which also follows every committed turn), updating rows in place so the selection
 * is kept and nothing is re-opened. Row states follow `placement_changed` and `session_busy`. The dialogs behind
 * New / Rename / Delete belong to the owning service, which the panel only signals.
 */
export class SessionsPanelViewModel extends Observable implements ISessionRowOwner
{
    private static readonly PropSelectedSession = 'SelectedSession'
    private static readonly PropStatusText = 'StatusText'
    private static readonly PropIsEmpty = 'IsEmpty'
    private static readonly RefreshFailedFormat = 'sessions unavailable: {0}'
    private static readonly FormatToken = '{0}'

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
    private client: RuntimeClient | null = null
    private subscriptions: IDisposable[] = []
    /** Orders refreshes so a slow early reply cannot overwrite a newer one. */
    private refreshToken = 0

    constructor()
    {
        super()
        this.newSessionCommand = new RelayCommand(() => this.OnNewSessionRequested.Raise(undefined), () => this.client !== null)
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
        if (client !== null)
        {
            this.subscriptions.push(
                client.OnSessionsChanged.Subscribe(() => void this.Refresh()),
                client.OnPlacementChanged.Subscribe(p => this.Find(p.id)?.SetPlacement(p.placement)),
                client.OnSessionBusy.Subscribe(p => this.Find(p.id)?.SetBusy(p.busy)))
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

    /** A user selection opens the session; clearing it (or re-selecting the same row) does nothing. */
    public set SelectedSession(row: SessionRowViewModel | null)
    {
        if (row === this._selected || row === null || row === undefined)
        {
            return
        }
        this.SetSelected(row)
        this.OnOpen.Raise(row.Id)
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
        try
        {
            const sessions = await client.ListSessions()
            if (token === this.refreshToken)
            {
                this.SetStatus('')
                this.Reconcile(sessions)
            }
        }
        catch (e)
        {
            if (token === this.refreshToken)
            {
                this.SetStatus(SessionsPanelViewModel.RefreshFailedFormat.replace(SessionsPanelViewModel.FormatToken, (e as Error).message))
            }
        }
    }

    /** Adds (or updates) a row for a session the caller just created, and selects it without raising OnOpen. */
    public Show(summary: SessionSummary): SessionRowViewModel
    {
        let row = this.Find(summary.id)
        if (row === undefined)
        {
            row = new SessionRowViewModel(this, summary)
            this.Sessions.Insert(0, row)
            this.RaiseIsEmpty(true)
        }
        else
        {
            row.Update(summary)
        }
        this.SetSelected(row)
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

    /** Makes the rows match the server list (its order), reusing row objects by id. */
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
        const same = rows.length === this.Sessions.Count && rows.every((r, i) => this.Sessions.Get(i) === r)
        if (!same)
        {
            this.Sessions.Batch(() =>
            {
                this.Sessions.Clear()
                for (const row of rows)
                {
                    this.Sessions.Add(row)
                }
            })
        }
        this.RaiseIsEmpty(wasEmpty)
        const selected = this._selected
        if (selected !== null && !rows.includes(selected))
        {
            this.SetSelected(null)
            this.OnSelectionLost.Raise(selected.Id)
        }
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
        }
    }
}
