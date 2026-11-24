import { Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import type { IDisposable } from './JsonRpcConnection'
import type { RuntimeClient } from './RuntimeClient'
import { PlacementState, type AgentTemplate, type AttachedSession, type Placement } from './agents'
import { PlacementText } from './PlacementText'
import { ChatViewModel } from './ChatViewModel'
import type { InstrumentPanelViewModel } from './InstrumentPanelViewModel'
import { TextFormat } from './TextFormat'
import { TraceViewModel } from './TraceViewModel'

/**
 * The host-area view of the shown session: a header (template · model @ host, placement, Load / Unload, tools) over
 * the existing chat and instrument panel. One stable instance; Attach re-points it at a session over the shared
 * runtime connection, Clear empties it.
 */
export class SessionViewModel extends Observable
{
    private static readonly HeaderFormat = '{0} · {1} @ {2}'
    private static readonly HeaderNoHostFormat = '{0} · {1}'
    private static readonly LiveFormat = 'live on {0}'
    private static readonly NeedsLoadText = 'model not loaded'
    private static readonly StaleFormat = '{0} (stale: not connected)'
    private static readonly LoadingText = 'loading model…'
    private static readonly AttachingText = 'opening session…'
    private static readonly AttachFailedFormat = 'could not open session: {0}'
    private static readonly LoadFailedFormat = 'load failed: {0}'
    private static readonly UnloadFailedFormat = 'unload failed: {0}'
    private static readonly LoadLabelText = 'Load model'
    private static readonly LoadEvictsFormat = 'Load model (evicts {0})'
    private static readonly ToolsOff = 'tools: off'
    private static readonly ToolsPrefix = 'tools: '
    private static readonly ToolsSeparator = ', '
    private static readonly EmptyText = 'Select a session, or start a new one.'
    private static readonly NoteFormat = '{0}. {1}'
    private static readonly PropIsNotesExpanded = 'IsNotesExpanded'
    private static readonly Props = ['HasNotes', 'HasSession', 'IsEmpty', 'HeaderText', 'PlacementText', 'CanLoad', 'CanUnload', 'LoadLabel', 'ToolsText', 'IsStale']

    public readonly Chat = new ChatViewModel()
    /** The shown session's agent trace, hosted by the instrument panel below its context bar. */
    public readonly Trace = new TraceViewModel()

    /** The session's working notes, each shown as `{n}. {text}`. */
    public readonly Notes = new ObservableCollection<string>()

    private readonly client: RuntimeClient
    private readonly loadCommand: RelayCommand
    private readonly unloadCommand: RelayCommand
    private readonly subscriptions: IDisposable[]
    private shownId: string | null = null
    private snapshot: AgentTemplate | null = null
    private placement: Placement | null = null
    /** A transient line (attaching, loading, an error) shown in place of the placement text. */
    private status = ''
    /** A Load command is in flight. */
    private loading = false
    /** The runtime is loading the model for a first message (session_loading until turn_started / loop_done). */
    private turnLoading = false
    /** The connection is down: the placement shown is the last one known. */
    private stale = false
    /** Orders attaches: a late reply for an earlier selection is dropped. */
    private attachToken = 0
    /** Counts `placement_changed` for the shown session, so a reply computed before one does not overwrite it. */
    private placementNotes = 0
    /** Counts `notes_changed` for the shown session, so a get_notes reply computed before one does not overwrite it. */
    private notesChanges = 0
    private notesExpanded = true

    constructor(client: RuntimeClient)
    {
        super()
        this.client = client
        this.loadCommand = new RelayCommand(() => void this.Load(), () => this.CanLoad)
        this.unloadCommand = new RelayCommand(() => void this.Unload(), () => this.CanUnload)
        this.subscriptions = [
            client.OnPlacementChanged.Subscribe(p =>
            {
                if (p.id === this.shownId)
                {
                    this.placementNotes++
                    this.ApplyPlacement(p.placement)
                }
            }),
            client.OnSessionLoading.Subscribe(p =>
            {
                if (p.id === this.shownId)
                {
                    this.Update(() => { this.turnLoading = true })
                }
            }),
            client.OnNotesChanged.Subscribe(n =>
            {
                if (n.id === this.shownId)
                {
                    this.notesChanges++
                    this.ShowNotes(n.notes)
                }
            }),
            client.OnTurnStarted.Subscribe(() => this.EndTurnLoading()),
            client.OnLoopDone.Subscribe(() => this.EndTurnLoading()),
        ]
        this.Panel.HostTrace(this.Trace)
    }

    public get Panel(): InstrumentPanelViewModel
    {
        return this.Chat.Panel
    }

    /** The id of the session shown (also while its attach is in flight), or null. */
    public get ShownId(): string | null
    {
        return this.shownId
    }

    public get HasSession(): boolean
    {
        return this.snapshot !== null
    }

    /** No session shown: the empty-state hint is visible. */
    public get IsEmpty(): boolean
    {
        return !this.HasSession
    }

    public get EmptyText(): string
    {
        return SessionViewModel.EmptyText
    }

    public get HeaderText(): string
    {
        const snapshot = this.snapshot
        if (snapshot === null)
        {
            return ''
        }
        const host = this.placement?.host ?? snapshot.host
        return host
            ? TextFormat.Format(SessionViewModel.HeaderFormat, snapshot.name, snapshot.model, host)
            : TextFormat.Format(SessionViewModel.HeaderNoHostFormat, snapshot.name, snapshot.model)
    }

    public get PlacementText(): string
    {
        if (this.status.length > 0)
        {
            return this.status
        }
        if (this.turnLoading)
        {
            return SessionViewModel.LoadingText
        }
        const placement = this.placement
        if (placement === null)
        {
            return ''
        }
        const text = SessionViewModel.Describe(placement)
        return this.stale ? TextFormat.Format(SessionViewModel.StaleFormat, text) : text
    }

    public get CanLoad(): boolean
    {
        return !this.loading && !this.turnLoading && !this.stale && PlacementText.NeedsLoad(this.placement)
    }

    public get CanUnload(): boolean
    {
        return !this.loading && !this.turnLoading && !this.stale && PlacementText.IsReady(this.placement)
    }

    /** True while the shown placement is the last one known from a dropped connection. */
    public get IsStale(): boolean
    {
        return this.stale
    }

    /** The service reports the connection: while it is down the placement is marked stale and Load / Unload are off. */
    public SetStale(stale: boolean): void
    {
        if (this.stale !== stale)
        {
            this.Update(() => { this.stale = stale })
        }
    }

    public get LoadLabel(): string
    {
        const evicts = this.placement?.evicts
        return evicts ? TextFormat.Format(SessionViewModel.LoadEvictsFormat, evicts) : SessionViewModel.LoadLabelText
    }

    /** The tools indicator: the session's own tool subset (from its snapshot), or 'tools: off'. */
    public get ToolsText(): string
    {
        const tools = this.snapshot?.tools ?? []
        return tools.length > 0 ? SessionViewModel.ToolsPrefix + tools.join(SessionViewModel.ToolsSeparator) : SessionViewModel.ToolsOff
    }

    /** The notes panel shows only while the session has notes. */
    public get HasNotes(): boolean
    {
        return this.Notes.Count > 0
    }

    /** The notes panel is open (the default) or folded to its header. */
    public get IsNotesExpanded(): boolean
    {
        return this.notesExpanded
    }

    public set IsNotesExpanded(value: boolean)
    {
        if (this.notesExpanded !== value)
        {
            const old = this.notesExpanded
            this.notesExpanded = value
            this.RaisePropertyChanged(SessionViewModel.PropIsNotesExpanded, old, value)
        }
    }

    public get LoadCommand(): ICommand
    {
        return this.loadCommand
    }

    public get UnloadCommand(): ICommand
    {
        return this.unloadCommand
    }

    /** Attaches the connection to the session and shows it: header, committed transcript, the session's sampling/thinking. */
    public async Attach(id: string): Promise<void>
    {
        const token = ++this.attachToken
        if (id !== this.shownId)
        {
            this.Reset()
        }
        this.shownId = id
        this.Chat.Bind(null)
        this.Update(() => { this.status = SessionViewModel.AttachingText })
        const notesBefore = this.placementNotes
        let attached: AttachedSession
        try
        {
            attached = await this.client.AttachSession(id)
        }
        catch (e)
        {
            if (token === this.attachToken)
            {
                this.Update(() => { this.status = TextFormat.Format(SessionViewModel.AttachFailedFormat, TextFormat.Message(e)) })
            }
            return
        }
        if (token !== this.attachToken)
        {
            return
        }
        // A placement_changed that arrived after the attach was sent is newer than the reply's placement.
        const placementNewer = this.placementNotes !== notesBefore && this.placement !== null
        this.Update(() =>
        {
            this.snapshot = attached.snapshot
            this.placement = placementNewer ? this.placement : attached.placement
            this.status = ''
        })
        this.Chat.Bind(this.client)
        this.Chat.LoadHistory(attached.transcript)
        this.Panel.ApplySession(attached.sampling, attached.enable_thinking)
        // Attach, switch and re-attach after a reconnect all reload the trace's first page; a busy session's newest
        // unfinished turn shows as running.
        void this.Trace.ShowSession(this.client, id, attached.summary.busy)
        void this.LoadNotes(id, token)
    }

    /** Fetches the session's notes; a `notes_changed` that arrived meanwhile is newer than the reply. */
    private async LoadNotes(id: string, token: number): Promise<void>
    {
        const changesBefore = this.notesChanges
        try
        {
            const result = await this.client.GetNotes(id)
            if (token === this.attachToken && id === this.shownId && this.notesChanges === changesBefore)
            {
                this.ShowNotes(result.notes)
            }
        }
        catch
        {
            // The panel stays as it was; the next notes_changed refreshes it.
        }
    }

    private ShowNotes(notes: string[]): void
    {
        this.Notes.Batch(() =>
        {
            this.Notes.Clear()
            notes.forEach((text, i) => this.Notes.Add(TextFormat.Format(SessionViewModel.NoteFormat, i + 1, text)))
        })
        this.Update(() => { })
    }

    /**
     * The connection dropped: the chat goes inert until the session is re-attached. A first-message load the runtime
     * was announcing is lost with the connection, so "loading model…" ends here (the re-attach shows the placement).
     */
    public Detach(): void
    {
        this.attachToken++
        this.Chat.Bind(null)
        if (this.turnLoading || this.loading)
        {
            this.Update(() =>
            {
                this.turnLoading = false
                this.loading = false
                this.status = ''
            })
        }
    }

    /** Shows nothing (the shown session was deleted, or the connection went away for good). */
    public Clear(): void
    {
        this.attachToken++
        this.shownId = null
        this.Chat.Bind(null)
        this.Reset()
    }

    public dispose(): void
    {
        for (const s of this.subscriptions)
        {
            s.Dispose()
        }
        this.Trace.dispose()
    }

    private Reset(): void
    {
        this.Chat.Clear()
        void this.Trace.ShowSession(null, null)
        this.notesChanges++
        this.Notes.Clear()
        this.Update(() =>
        {
            this.snapshot = null
            this.placement = null
            this.status = ''
            this.loading = false
            this.turnLoading = false
        })
    }

    private async Load(): Promise<void>
    {
        const id = this.shownId
        if (id === null || !this.CanLoad)
        {
            return
        }
        this.Update(() =>
        {
            this.loading = true
            this.status = SessionViewModel.LoadingText
        })
        const notesBefore = this.placementNotes
        try
        {
            const placement = await this.client.LoadSessionModel(id)
            if (id === this.shownId)
            {
                this.Update(() =>
                {
                    this.loading = false
                    this.status = ''
                })
                // A placement_changed since the request is newer than the reply.
                if (this.placementNotes === notesBefore)
                {
                    this.ApplyPlacement(placement)
                }
            }
        }
        catch (e)
        {
            if (id === this.shownId)
            {
                this.Update(() =>
                {
                    this.loading = false
                    this.status = TextFormat.Format(SessionViewModel.LoadFailedFormat, TextFormat.Message(e))
                })
            }
        }
    }

    private async Unload(): Promise<void>
    {
        const id = this.shownId
        if (id === null || !this.CanUnload)
        {
            return
        }
        try
        {
            // The new placement arrives as placement_changed.
            await this.client.UnloadSessionModel(id)
        }
        catch (e)
        {
            if (id === this.shownId)
            {
                this.Update(() => { this.status = TextFormat.Format(SessionViewModel.UnloadFailedFormat, TextFormat.Message(e)) })
            }
        }
    }

    private ApplyPlacement(placement: Placement): void
    {
        const wasReady = PlacementText.IsReady(this.placement)
        this.Update(() => { this.placement = placement })
        if (!wasReady && PlacementText.IsReady(placement) && this.snapshot !== null)
        {
            // The model is up now: show its name and window size.
            this.Panel.RefreshModelProps()
        }
    }

    private EndTurnLoading(): void
    {
        if (this.turnLoading)
        {
            this.Update(() => { this.turnLoading = false })
        }
    }

    private static Describe(placement: Placement): string
    {
        switch (placement.state)
        {
            case PlacementState.Ready:
                return TextFormat.Format(SessionViewModel.LiveFormat, placement.host ?? '')
            case PlacementState.NeedsLoad:
                return SessionViewModel.NeedsLoadText
            default:
                return PlacementText.Unavailable(placement)
        }
    }

    /** Applies a state change, then notifies every header property (cheap; keeps the derived getters single-sourced). */
    private Update(mutate: () => void): void
    {
        mutate()
        for (const name of SessionViewModel.Props)
        {
            this.RaisePropertyChanged(name, undefined, (this as unknown as Record<string, unknown>)[name])
        }
        this.loadCommand.RaiseCanExecuteChanged()
        this.unloadCommand.RaiseCanExecuteChanged()
    }
}
