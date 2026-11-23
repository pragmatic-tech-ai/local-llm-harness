import { ServiceBase, ServiceKey } from '@pragmatic-tech-ai/mural/runtime'
import type { IServiceProvider } from '@pragmatic-tech-ai/mural/runtime'
import type { IDisposable } from './JsonRpcConnection'
import { ContentHostService } from '@pragmatic-tech-ai/mural/framework/shell/services/content-host-service.js'
import { ChatViewModel } from './ChatViewModel'
import { RuntimeClient } from './RuntimeClient'
import { HarnessPickerViewModel } from './HarnessPickerViewModel'
import { SupervisorClient } from './SupervisorClient'
import { SupervisorUrlSettingKey } from './SettingsKeys'
import { ApplicationSettings } from '@pragmatic-tech-ai/mural/framework'

/**
 * Runtime-management capability. Owns the chat (shown as the content host's content) and the harness picker rendered in
 * the capability's side pane, coordinating the two: a harness reaching ready gets a fresh client bound to the chat;
 * clearing unbinds it. The side pane is a view-reusing ContentControl that builds its view for this service once, so the
 * Picker must be a stable object from construction — Attach feeds it the supervisor connection later (its runtime/model
 * lists then populate over the ObservableCollection change-notifications), never swapping the Picker instance.
 */
export class RuntimeManagementService extends ServiceBase
{
    public static readonly Key = new ServiceKey<RuntimeManagementService>('RuntimeManagementService')

    private readonly chat: ChatViewModel
    private readonly picker: HarnessPickerViewModel
    private readonly subscriptions: IDisposable[]
    private chatClient: RuntimeClient | null = null
    private supervisor: SupervisorClient | null = null

    constructor(
        provider: IServiceProvider,
        chatFactory: () => ChatViewModel = () => new ChatViewModel(),
        pickerFactory: () => HarnessPickerViewModel = () => new HarnessPickerViewModel(),
        private readonly harnessFactory: () => RuntimeClient = () => new RuntimeClient(),
        private readonly supervisorFactory: () => SupervisorClient = () => new SupervisorClient())
    {
        super(provider)
        this.chat = chatFactory()
        this.picker = pickerFactory()
        this.subscriptions = [
            this.picker.OnReady.Subscribe(url => this.AttachClient(url)),
            this.picker.OnCleared.Subscribe(() => this.DetachClient()),
        ]
        provider.getRequired(ContentHostService.Key).View(this.chat)
        const settings = provider.getRequired(ApplicationSettings.Key)
        this.ApplyConnection(this.ReadUrl(settings))
        const changed = settings.Changed(SupervisorUrlSettingKey)
            .subscribe(() => this.ApplyConnection(this.ReadUrl(settings)))
        this.subscriptions.push({ Dispose: () => changed.dispose() })
    }

    public get Picker(): HarnessPickerViewModel
    {
        return this.picker
    }

    /** Give the picker its supervisor connection and begin discovery. */
    public Attach(client: SupervisorClient): void
    {
        this.picker.Bind(client)
        this.picker.Refresh()
    }

    /** Bind the chat to a directly-connected backend (no supervisor); used by the direct-backend fallback. */
    public BindBackend(client: RuntimeClient): void
    {
        this.chatClient?.Disconnect()
        this.chatClient = client
        this.chat.Bind(client)
    }

    private ReadUrl(settings: ApplicationSettings): string
    {
        const value = settings.Get(SupervisorUrlSettingKey)
        return typeof value === 'string' ? value.trim() : ''
    }

    /** Reconnect to the supervisor named by the setting: drop the old connection, then connect anew (or go inert when empty). */
    private ApplyConnection(url: string): void
    {
        // The live harness and the picker's state belong to the connection being dropped; Unbind's
        // generation bump also invalidates any in-flight Start against the old client.
        this.DetachClient()
        this.picker.Unbind()
        this.supervisor?.Disconnect()
        this.supervisor = null
        if (url.length === 0)
        {
            return
        }
        const client = this.supervisorFactory()
        this.supervisor = client
        try
        {
            client.Connect(url)
            this.Attach(client)
        }
        catch
        {
            // A bad url must not escape the change handler or leave the service half-wired.
            this.supervisor?.Disconnect()
            this.supervisor = null
        }
    }

    private AttachClient(url: string): void
    {
        this.chatClient?.Disconnect()
        const client = this.harnessFactory()
        client.Connect(url)
        this.chatClient = client
        this.chat.Bind(client)
    }

    private DetachClient(): void
    {
        this.chat.Bind(null)
        this.chatClient?.Disconnect()
        this.chatClient = null
    }

    public override dispose(): void
    {
        for (const subscription of this.subscriptions)
        {
            subscription.Dispose()
        }
        this.chatClient?.Disconnect()
        this.chatClient = null
        this.supervisor?.Disconnect()
        this.supervisor = null
    }
}
