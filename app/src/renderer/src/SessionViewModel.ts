import { Observable, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import type { IDisposable } from './JsonRpcConnection'
import type { RuntimeClient } from './RuntimeClient'
import type { AgentTemplate, AttachedSession, Placement } from './agents'
import { ChatViewModel } from './ChatViewModel'
import type { InstrumentPanelViewModel } from './InstrumentPanelViewModel'

/**
 * The host-area view of the shown session: a header (template · model @ host, placement, Load / Unload, tools) over
 * the existing chat and instrument panel. One stable instance; Attach re-points it at a session over the shared
 * runtime connection, Clear empties it.
 */
export class SessionViewModel extends Observable
{
    private static readonly PlacementReady = 'ready'
    private static readonly PlacementNeedsLoad = 'needs_load'
    private static readonly HeaderFormat = '{0} · {1} @ {2}'
    private static readonly HeaderNoHostFormat = '{0} · {1}'
    private static readonly LiveFormat = 'live on {0}'
    private static readonly NeedsLoadText = 'model not loaded'
    private static readonly UnavailableFormat = 'unavailable: {0}'
    private static readonly UnavailableText = 'unavailable'
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
    private static readonly FormatToken = /\{(\d+)\}/g
    private static readonly Props = ['HasSession', 'IsEmpty', 'HeaderText', 'PlacementText', 'CanLoad', 'CanUnload', 'LoadLabel', 'ToolsText']

    public readonly Chat = new ChatViewModel()

    private readonly client: RuntimeClient
    private readonly loadCommand: RelayCommand
    private readonly unloadCommand: RelayCommand
    private readonly subscriptions: IDisposable[]
    private shownId: string | null = null
    private snapshot: AgentTemplate | null = null
    private placement: Placement | null = null
    /** A transient line (attaching, loading, an error) shown in place of the placement text. */
    private status = ''
    private loading = false
    /** Orders attaches: a late reply for an earlier selection is dropped. */
    private attachToken = 0

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
                    this.ApplyPlacement(p.placement)
                }
            }),
        ]
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
            ? SessionViewModel.Format(SessionViewModel.HeaderFormat, snapshot.name, snapshot.model, host)
            : SessionViewModel.Format(SessionViewModel.HeaderNoHostFormat, snapshot.name, snapshot.model)
    }

    public get PlacementText(): string
    {
        if (this.status.length > 0)
        {
            return this.status
        }
        const placement = this.placement
        if (placement === null)
        {
            return ''
        }
        switch (placement.state)
        {
            case SessionViewModel.PlacementReady:
                return SessionViewModel.Format(SessionViewModel.LiveFormat, placement.host ?? '')
            case SessionViewModel.PlacementNeedsLoad:
                return SessionViewModel.NeedsLoadText
            default:
                return placement.reason ? SessionViewModel.Format(SessionViewModel.UnavailableFormat, placement.reason) : SessionViewModel.UnavailableText
        }
    }

    public get CanLoad(): boolean
    {
        return !this.loading && this.placement?.state === SessionViewModel.PlacementNeedsLoad
    }

    public get CanUnload(): boolean
    {
        return !this.loading && this.placement?.state === SessionViewModel.PlacementReady
    }

    public get LoadLabel(): string
    {
        const evicts = this.placement?.evicts
        return evicts ? SessionViewModel.Format(SessionViewModel.LoadEvictsFormat, evicts) : SessionViewModel.LoadLabelText
    }

    /** The tools indicator: the session's own tool subset (from its snapshot), or 'tools: off'. */
    public get ToolsText(): string
    {
        const tools = this.snapshot?.tools ?? []
        return tools.length > 0 ? SessionViewModel.ToolsPrefix + tools.join(SessionViewModel.ToolsSeparator) : SessionViewModel.ToolsOff
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
        let attached: AttachedSession
        try
        {
            attached = await this.client.AttachSession(id)
        }
        catch (e)
        {
            if (token === this.attachToken)
            {
                this.Update(() => { this.status = SessionViewModel.Format(SessionViewModel.AttachFailedFormat, SessionViewModel.Message(e)) })
            }
            return
        }
        if (token !== this.attachToken)
        {
            return
        }
        this.Update(() =>
        {
            this.snapshot = attached.snapshot
            this.placement = attached.placement
            this.status = ''
        })
        this.Chat.Bind(this.client)
        this.Chat.LoadHistory(attached.transcript)
        this.Panel.ApplySession(attached.sampling, attached.enable_thinking)
    }

    /** The connection dropped: the chat goes inert until the session is re-attached. */
    public Detach(): void
    {
        this.attachToken++
        this.Chat.Bind(null)
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
    }

    private Reset(): void
    {
        this.Chat.Clear()
        this.Update(() =>
        {
            this.snapshot = null
            this.placement = null
            this.status = ''
            this.loading = false
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
                this.ApplyPlacement(placement)
            }
        }
        catch (e)
        {
            if (id === this.shownId)
            {
                this.Update(() =>
                {
                    this.loading = false
                    this.status = SessionViewModel.Format(SessionViewModel.LoadFailedFormat, SessionViewModel.Message(e))
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
                this.Update(() => { this.status = SessionViewModel.Format(SessionViewModel.UnloadFailedFormat, SessionViewModel.Message(e)) })
            }
        }
    }

    private ApplyPlacement(placement: Placement): void
    {
        const wasReady = this.placement?.state === SessionViewModel.PlacementReady
        this.Update(() => { this.placement = placement })
        if (!wasReady && placement.state === SessionViewModel.PlacementReady && this.snapshot !== null)
        {
            // The model is up now: show its name and window size.
            this.Panel.RefreshModelProps()
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

    private static Message(e: unknown): string
    {
        return e instanceof Error ? e.message : String(e)
    }

    private static Format(template: string, ...values: unknown[]): string
    {
        return template.replace(SessionViewModel.FormatToken, (_m, i: string) => String(values[Number(i)]))
    }
}
