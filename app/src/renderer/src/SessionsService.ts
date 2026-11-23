import { RelayCommand, ServiceBase, ServiceKey } from '@pragmatic-tech-ai/mural/runtime'
import type { IServiceProvider } from '@pragmatic-tech-ai/mural/runtime'
import { ContentHostService } from '@pragmatic-tech-ai/mural/framework/shell/services/content-host-service.js'
import { ApplicationSettings, ButtonVariant, DialogAction, DialogService } from '@pragmatic-tech-ai/mural/framework'
import type { IDisposable } from './JsonRpcConnection'
import { RuntimeClient } from './RuntimeClient'
import { SupervisorClient, type HarnessInfo } from './SupervisorClient'
import { SupervisorUrlSettingKey } from './SettingsKeys'
import type { SessionSummary } from './agents'
import { SessionsPanelViewModel } from './SessionsPanelViewModel'
import type { SessionRowViewModel } from './SessionRowViewModel'
import { SessionViewModel } from './SessionViewModel'
import { NewSessionDialogViewModel } from './NewSessionDialogViewModel'
import { TextPromptViewModel } from './TextPromptViewModel'

/**
 * The Sessions capability. Owns the app's single runtime connection (`Client`, shared with the Agents capability),
 * the side-pane list (`Panel`) and the host-area session view (`View`, shown in the content host from the start).
 * The runtime url comes from the `supervisor.url` setting through the supervisor's `chat_url`; a dropped connection
 * reconnects with backoff (1 s, 2 s, 4 s, ... capped at 10 s) and re-attaches the session that was shown.
 */
export class SessionsService extends ServiceBase
{
    public static readonly Key = new ServiceKey<SessionsService>('SessionsService')

    private static readonly BackoffStartMs = 1000
    private static readonly BackoffCapMs = 10000
    private static readonly NewSessionTitle = 'New session'
    private static readonly NewSessionWidth = 520
    private static readonly CancelLabel = 'Cancel'
    private static readonly CreateLabel = 'Create'
    private static readonly RenameTitle = 'Rename session'
    private static readonly RenameLabel = 'Title'
    private static readonly RenameConfirm = 'Rename'
    private static readonly DeleteTitle = 'Delete session'
    private static readonly DeleteFormat = 'Delete "{0}" and its transcript?'
    private static readonly DeleteConfirm = 'Delete'
    private static readonly FormatToken = '{0}'
    private static readonly ActionFailed = 'Session action failed'

    public readonly Client: RuntimeClient
    public readonly Panel = new SessionsPanelViewModel()
    public readonly View: SessionViewModel

    private readonly subscriptions: IDisposable[] = []
    private runtimeUrl: string | null = null
    /** Bumped on every settings change so a stale endpoint discovery is dropped. */
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
            this.Panel.OnOpen.Subscribe(id => void this.View.Attach(id)),
            this.Panel.OnSelectionLost.Subscribe(id => this.OnSessionGone(id)),
            this.Panel.OnNewSessionRequested.Subscribe(() => void this.OpenNewSessionDialog()),
            this.Panel.OnRenameRequested.Subscribe(row => void this.Rename(row)),
            this.Panel.OnDeleteRequested.Subscribe(row => void this.Delete(row)))
        provider.getRequired(ContentHostService.Key).View(this.View)
        const settings = provider.getRequired(ApplicationSettings.Key)
        void this.ApplyConnection(SessionsService.ReadUrl(settings))
        const changed = settings.Changed(SupervisorUrlSettingKey)
            .subscribe(() => void this.ApplyConnection(SessionsService.ReadUrl(settings)))
        this.subscriptions.push({ Dispose: () => changed.dispose() })
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
        finally
        {
            created.Dispose()
        }
    }

    /** Shows a session that was just created: its row selected, its view attached. */
    public OpenCreated(summary: SessionSummary): void
    {
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
        const title = await TextPromptViewModel.Ask(dialogs, {
            Title: SessionsService.RenameTitle,
            Label: SessionsService.RenameLabel,
            Text: row.Title,
            ConfirmLabel: SessionsService.RenameConfirm,
        })
        if (title !== undefined && title !== row.Title)
        {
            // The list follows through sessions_changed.
            await this.Client.RenameSession(row.Id, title).catch(e => console.error(SessionsService.ActionFailed, e))
        }
    }

    private async Delete(row: SessionRowViewModel): Promise<void>
    {
        const dialogs = this.Provider.get(DialogService.Key)
        if (dialogs === undefined)
        {
            return
        }
        const confirmed = await dialogs.Confirm({
            Title: SessionsService.DeleteTitle,
            Message: SessionsService.DeleteFormat.replace(SessionsService.FormatToken, row.Title),
            ConfirmLabel: SessionsService.DeleteConfirm,
        })
        if (confirmed)
        {
            // sessions_changed then drops the row, and the view if it was shown.
            await this.Client.DeleteSession(row.Id).catch(e => console.error(SessionsService.ActionFailed, e))
        }
    }

    private static ReadUrl(settings: ApplicationSettings): string
    {
        const value = settings.Get(SupervisorUrlSettingKey)
        return typeof value === 'string' ? value.trim() : ''
    }

    /** Points the runtime connection at the supervisor named by the setting (or goes inert when empty). */
    private async ApplyConnection(supervisorUrl: string): Promise<void>
    {
        const generation = ++this.generation
        this.CancelReconnect()
        this.attempt = 0
        this.runtimeUrl = null
        this.Client.Disconnect()
        this.Panel.Bind(null)
        this.View.Clear()
        if (supervisorUrl.length === 0)
        {
            return
        }
        const runtimeUrl = await this.DiscoverRuntimeUrl(supervisorUrl)
        if (generation !== this.generation || runtimeUrl === null)
        {
            return
        }
        this.runtimeUrl = runtimeUrl
        this.Connect()
    }

    /** Asks the supervisor for its harnesses' `chat_url`; an unreachable supervisor still yields the default runtime port. */
    private async DiscoverRuntimeUrl(supervisorUrl: string): Promise<string | null>
    {
        let harnesses: HarnessInfo[] = []
        const supervisor = this.supervisorFactory()
        try
        {
            supervisor.Connect(supervisorUrl)
            harnesses = await supervisor.ListHarnesses()
        }
        catch
        {
            // Fall back to the supervisor host on the default runtime port.
        }
        finally
        {
            supervisor.Disconnect()
        }
        try
        {
            return SupervisorClient.RuntimeUrl(supervisorUrl, harnesses)
        }
        catch
        {
            // Not a url: stay inert until the setting is fixed.
            return null
        }
    }

    private Connect(): void
    {
        const url = this.runtimeUrl
        if (url === null)
        {
            return
        }
        try
        {
            this.Client.Connect(url)
        }
        catch
        {
            this.ScheduleReconnect()
        }
    }

    /** (Re)connected: follow the list, and re-attach the session that was shown. */
    private OnConnected(): void
    {
        this.attempt = 0
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
        if (this.runtimeUrl === null || this.reconnectTimer !== null)
        {
            return
        }
        const delay = Math.min(SessionsService.BackoffStartMs * 2 ** this.attempt, SessionsService.BackoffCapMs)
        this.attempt++
        this.reconnectTimer = setTimeout(() =>
        {
            this.reconnectTimer = null
            this.Connect()
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
        this.runtimeUrl = null
        this.Client.Disconnect()
        this.View.dispose()
        super.dispose()
    }
}
