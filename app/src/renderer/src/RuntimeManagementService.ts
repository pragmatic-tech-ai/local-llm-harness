import { ServiceBase, ServiceKey } from '@pragmatic-tech-ai/mural/runtime'
import type { IServiceProvider } from '@pragmatic-tech-ai/mural/runtime'
import { ContentHostService } from '@pragmatic-tech-ai/mural/framework/shell/services/content-host-service.js'
import { ChatViewModel } from './ChatViewModel'
import { HarnessClient } from './HarnessClient'
import { HarnessPickerViewModel } from './HarnessPickerViewModel'
import { SupervisorClient } from './SupervisorClient'

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
    private chatClient: HarnessClient | null = null

    constructor(
        provider: IServiceProvider,
        chatFactory: () => ChatViewModel = () => new ChatViewModel(),
        pickerFactory: () => HarnessPickerViewModel = () => new HarnessPickerViewModel(),
        private readonly harnessFactory: () => HarnessClient = () => new HarnessClient())
    {
        super(provider)
        this.chat = chatFactory()
        this.picker = pickerFactory()
        this.picker.OnReady.Subscribe(url => this.AttachClient(url))
        this.picker.OnCleared.Subscribe(() => this.DetachClient())
        provider.getRequired(ContentHostService.Key).View(this.chat)
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
    public BindBackend(client: HarnessClient): void
    {
        this.chat.Bind(client)
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
}
