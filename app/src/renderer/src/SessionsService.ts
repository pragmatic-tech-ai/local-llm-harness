import { RelayCommand, ServiceBase, ServiceKey } from '@pragmatic-tech-ai/mural/runtime'
import type { IServiceProvider } from '@pragmatic-tech-ai/mural/runtime'
import { ContentHostService } from '@pragmatic-tech-ai/mural/framework/shell/services/content-host-service.js'
import { ApplicationSettings, ButtonVariant, DialogAction, DialogService, type IActivatable } from '@pragmatic-tech-ai/mural/framework'
import { TypedSignal, type IDisposable } from './JsonRpcConnection'
import { RuntimeClient } from './RuntimeClient'
import { SupervisorClient, type HarnessInfo } from './SupervisorClient'
import { SupervisorUrlSettingKey } from './SettingsKeys'
import type { SessionSummary } from './agents'
import { ConnectionState } from './ConnectionState'
import { SessionsPanelViewModel } from './SessionsPanelViewModel'
import type { SessionRowViewModel } from './SessionRowViewModel'
import { SessionViewModel } from './SessionViewModel'
import { NewSessionDialogViewModel } from './NewSessionDialogViewModel'
import { TextPromptViewModel } from './TextPromptViewModel'
import { TextFormat } from './TextFormat'

/**
 * The Sessions capability. Owns the app's single runtime connection (`Client`, shared with the Agents capability),
 * the side-pane list (`Panel`) and the host-area session view (`View`, shown in the content host from the start).
 * Each connection attempt asks the supervisor (named by the `supervisor.url` setting, bounded by a timeout) for the
 * runtime's `chat_url`, falling back to the supervisor host on :8779. A failed or dropped connection retries with
 * backoff (1 s, 2 s, 4 s, ... capped at 10 s) and re-attaches the session that was shown. `Connection` reports the
 * state; the panel and the session header show it. The content host is shared with the Agents capability, so
 * activating the Sessions rail re-presents the session view.
 */
export class SessionsService extends ServiceBase implements IActivatable
{
    public static readonly Key = new ServiceKey<SessionsService>('SessionsService')

    private static readonly BackoffStartMs = 1000
    private static readonly BackoffCapMs = 10000
    private static readonly DiscoveryTimeoutMs = 3000
    private static readonly DiscoveryTimedOut = 'supervisor did not answer'
    private static readonly NewSessionTitle = 'New session'
    private static readonly NewSessionWidth = 520
    private static readonly CancelLabel = 'Cancel'
    private static readonly CreateLabel = 'Create'
    private static readonly RenameTitle = 'Rename session'
    private static readonly RenameLabel = 'Title'
    private static readonly RenameConfirm = 'Rename'
    private static readonly RenameFailedFormat = 'rename failed: {0}'
    private static readonly DeleteTitle = 'Delete session'
    private static readonly DeleteFormat = 'Delete "{0}" and its transcript?'
    private static readonly DeleteConfirm = 'Delete'
    private static readonly DeleteFailedFormat = 'delete failed: {0}'
    private static readonly DialogFailedFormat = 'could not open the dialog: {0}'

    public readonly Client: RuntimeClient
    public readonly Panel = new SessionsPanelViewModel()
    public readonly View: SessionViewModel
    /** Raised whenever `Connection` changes. */
    public readonly OnConnectionChanged = new TypedSignal<ConnectionState>()

    private readonly host: ContentHostService
    private readonly subscriptions: IDisposable[] = []
    private connection = ConnectionState.Disconnected
    private supervisorUrl = ''
    /** Bumped on every settings change so a stale discovery or retry is dropped. */
    private generation = 0
    private attempt = 0
    private reconnectTimer: ReturnType<typeof setTimeout> | null = null

    constructor(
        provider: IServiceProvider,
        clientFactory: () => RuntimeClient = () => new RuntimeClient(),
        private readonly supervisorFactory: () => SupervisorClient = () => new SupervisorClient())
    {
        super(provider)
        this.Client = clientFactory()
        this.View = new SessionViewModel(this.Client)
        this.subscriptions.push(
            this.Client.OnOpened.Subscribe(() => this.OnConnected()),
            this.Client.OnClosed.Subscribe(() => this.OnDisconnected()),
            this.View.Chat.OnBusyChanged.Subscribe(busy => this.Panel.SetSwitchLocked(busy)),
            this.Panel.OnOpen.Subscribe(id => void this.View.Attach(id)),
            this.Panel.OnSelectionLost.Subscribe(id => this.OnSessionGone(id)),
            this.Panel.OnNewSessionRequested.Subscribe(() => void this.OpenNewSessionDialog()),
            this.Panel.OnRenameRequested.Subscribe(row => void this.Rename(row)),
            this.Panel.OnDeleteRequested.Subscribe(row => void this.Delete(row)))
        this.SetConnection(ConnectionState.Disconnected)
        this.host = provider.getRequired(ContentHostService.Key)
        this.host.View(this.View)
        const settings = provider.getRequired(ApplicationSettings.Key)
        this.ApplyConnection(SessionsService.ReadUrl(settings))
        const changed = settings.Changed(SupervisorUrlSettingKey)
            .subscribe(() => this.ApplyConnection(SessionsService.ReadUrl(settings)))
        this.subscriptions.push({ Dispose: () => changed.dispose() })
    }

    public get Connection(): ConnectionState
    {
        return this.connection
    }

    /** The rail switched (back) to Sessions: present the session view in the shared content host. */
    public OnActivated(): void
    {
        this.host.View(this.View)
    }

    /** Opens the New session dialog; Create makes the session, closes the dialog and opens the session. */
    public async OpenNewSessionDialog(): Promise<void>
    {
        const dialogs = this.Provider.get(DialogService.Key)
        if (dialogs === undefined)
        {
            return
        }
        const body = new NewSessionDialogViewModel(this.Client)
        const created = body.OnCreated.Subscribe(summary =>
        {
            dialogs.Close(summary)
            this.OpenCreated(summary)
        })
        void body.Load()
        try
        {
            await dialogs.Show<SessionSummary>({
                Title: SessionsService.NewSessionTitle,
                Content: body,
                Width: SessionsService.NewSessionWidth,
                Actions: [
                    new DialogAction(SessionsService.CancelLabel, new RelayCommand(() => dialogs.Close(undefined))),
                    new DialogAction(SessionsService.CreateLabel, body.CreateCommand, ButtonVariant.Filled),
                ],
            })
        }
        catch (e)
        {
            this.Panel.ReportError(TextFormat.Format(SessionsService.DialogFailedFormat, TextFormat.Message(e)))
        }
        finally
        {
            created.Dispose()
        }
    }

    /**
     * Shows a session that was just created: its row selected, its view attached. While the shown session runs a
     * turn (the dialog was open when it started) the session is only listed: opening it would abort that turn.
     */
    public OpenCreated(summary: SessionSummary): void
    {
        if (this.View.Chat.IsBusy)
        {
            this.Panel.Add(summary)
            this.Panel.ReportSwitchRefused()
            return
        }
        this.Panel.Show(summary)
        void this.View.Attach(summary.id)
    }

    private async Rename(row: SessionRowViewModel): Promise<void>
    {
        const dialogs = this.Provider.get(DialogService.Key)
        if (dialogs === undefined)
        {
            return
        }
        try
        {
            const title = await TextPromptViewModel.Ask(dialogs, {
                Title: SessionsService.RenameTitle,
                Label: SessionsService.RenameLabel,
                Text: row.Title,
                ConfirmLabel: SessionsService.RenameConfirm,
            })
            if (title !== undefined && title !== row.Title)
            {
                // The list follows through sessions_changed.
                await this.Client.RenameSession(row.Id, title)
            }
        }
        catch (e)
        {
            this.Panel.ReportError(TextFormat.Format(SessionsService.RenameFailedFormat, TextFormat.Message(e)))
        }
    }

    private async Delete(row: SessionRowViewModel): Promise<void>
    {
        const dialogs = this.Provider.get(DialogService.Key)
        if (dialogs === undefined)
        {
            return
        }
        try
        {
            const confirmed = await dialogs.Confirm({
                Title: SessionsService.DeleteTitle,
                Message: TextFormat.Format(SessionsService.DeleteFormat, row.Title),
                ConfirmLabel: SessionsService.DeleteConfirm,
            })
            if (confirmed)
            {
                // sessions_changed then drops the row, and the view if it was shown.
                await this.Client.DeleteSession(row.Id)
            }
        }
        catch (e)
        {
            this.Panel.ReportError(TextFormat.Format(SessionsService.DeleteFailedFormat, TextFormat.Message(e)))
        }
    }

    private static ReadUrl(settings: ApplicationSettings): string
    {
        const value = settings.Get(SupervisorUrlSettingKey)
        return typeof value === 'string' ? value.trim() : ''
    }

    /** Points the runtime connection at the supervisor named by the setting (or goes inert when empty). */
    private ApplyConnection(supervisorUrl: string): void
    {
        this.generation++
        this.CancelReconnect()
        this.attempt = 0
        this.supervisorUrl = supervisorUrl
        this.Client.Disconnect()
        this.Panel.Bind(null)
        this.View.Clear()
        if (supervisorUrl.length === 0)
        {
            this.SetConnection(ConnectionState.Disconnected)
            return
        }
        void this.Connect()
    }

    /** One connection attempt: discover the runtime url, then open the runtime socket (or schedule the next try). */
    private async Connect(): Promise<void>
    {
        const generation = this.generation
        this.SetConnection(ConnectionState.Reconnecting)
        const runtimeUrl = await this.DiscoverRuntimeUrl(this.supervisorUrl)
        if (generation !== this.generation)
        {
            return
        }
        if (runtimeUrl === null)
        {
            this.ScheduleReconnect()
            return
        }
        try
        {
            this.Client.Connect(runtimeUrl)
        }
        catch
        {
            this.ScheduleReconnect()
        }
    }

    /**
     * Asks the supervisor for its harnesses' `chat_url`. An unreachable or silent supervisor (no answer within the
     * timeout) still yields the default runtime port; a malformed url yields null.
     */
    private async DiscoverRuntimeUrl(supervisorUrl: string): Promise<string | null>
    {
        let harnesses: HarnessInfo[] = []
        const supervisor = this.supervisorFactory()
        let timer: ReturnType<typeof setTimeout> | null = null
        try
        {
            supervisor.Connect(supervisorUrl)
            const timeout = new Promise<never>((_resolve, reject) =>
            {
                timer = setTimeout(() => reject(new Error(SessionsService.DiscoveryTimedOut)), SessionsService.DiscoveryTimeoutMs)
            })
            harnesses = await Promise.race([supervisor.ListHarnesses(), timeout])
        }
        catch
        {
            // Fall back to the supervisor host on the default runtime port.
        }
        finally
        {
            if (timer !== null)
            {
                clearTimeout(timer)
            }
            supervisor.Disconnect()
        }
        try
        {
            return SupervisorClient.RuntimeUrl(supervisorUrl, harnesses)
        }
        catch
        {
            return null
        }
    }

    /** (Re)connected: follow the list, and re-attach the session that was shown. */
    private OnConnected(): void
    {
        this.attempt = 0
        this.SetConnection(ConnectionState.Connected)
        this.Panel.Bind(this.Client)
        void this.Panel.Refresh()
        const shown = this.View.ShownId
        if (shown !== null)
        {
            void this.View.Attach(shown)
        }
    }

    /** The list stays as it was while the connection is retried; the chat goes inert. */
    private OnDisconnected(): void
    {
        this.View.Detach()
        this.ScheduleReconnect()
    }

    private ScheduleReconnect(): void
    {
        if (this.supervisorUrl.length === 0 || this.reconnectTimer !== null)
        {
            return
        }
        this.SetConnection(ConnectionState.Reconnecting)
        const delay = Math.min(SessionsService.BackoffStartMs * 2 ** this.attempt, SessionsService.BackoffCapMs)
        this.attempt++
        this.reconnectTimer = setTimeout(() =>
        {
            this.reconnectTimer = null
            void this.Connect()
        }, delay)
    }

    private CancelReconnect(): void
    {
        if (this.reconnectTimer !== null)
        {
            clearTimeout(this.reconnectTimer)
            this.reconnectTimer = null
        }
    }

    private SetConnection(state: ConnectionState): void
    {
        this.Panel.SetConnection(state)
        this.View.SetStale(state !== ConnectionState.Connected)
        if (this.connection !== state)
        {
            this.connection = state
            this.OnConnectionChanged.Raise(state)
        }
    }

    private OnSessionGone(id: string): void
    {
        if (id === this.View.ShownId)
        {
            this.View.Clear()
        }
    }

    public override dispose(): void
    {
        for (const subscription of this.subscriptions)
        {
            subscription.Dispose()
        }
        this.generation++
        this.CancelReconnect()
        this.supervisorUrl = ''
        this.Client.Disconnect()
        this.View.dispose()
        super.dispose()
    }
}
