import { ServiceBase, ServiceKey } from '@pragmatic-tech-ai/mural/runtime'
import type { IServiceProvider } from '@pragmatic-tech-ai/mural/runtime'
import { ContentHostService } from '@pragmatic-tech-ai/mural/framework/shell/services/content-host-service.js'
import { ChatViewModel } from './ChatViewModel'
import { HarnessClient } from './HarnessClient'
import { HarnessPickerViewModel } from './HarnessPickerViewModel'
import { SupervisorClient } from './SupervisorClient'

/**
 * Runtime-management capability. Self-initializes from DI: when a SupervisorClient is registered it shows the chat in the
 * content host and owns the harness picker, coordinating it with the chat (a ready harness gets a fresh client bound to
 * the chat; clearing unbinds it). Without a supervisor it does nothing and Picker stays null.
 */
export class RuntimeManagementService extends ServiceBase
{
    public static readonly Key = new ServiceKey<RuntimeManagementService>('RuntimeManagementService')
    private static readonly PickerProperty = 'Picker'

    private picker: HarnessPickerViewModel | null = null
    private chat: ChatViewModel | null = null
    private chatClient: HarnessClient | null = null
    private pickerSubscriptions: { Dispose(): void }[] = []

    constructor(
        provider: IServiceProvider,
        chatFactory: () => ChatViewModel = () => new ChatViewModel(),
        private readonly harnessFactory: () => HarnessClient = () => new HarnessClient())
    {
        super(provider)
        const client = provider.get(SupervisorClient.Key)
        if (client !== undefined)
        {
            const chat = chatFactory()
            provider.getRequired(ContentHostService.Key).View(chat)
            this.Initialize(client, chat)
        }
    }

    public get Picker(): HarnessPickerViewModel | null
    {
        return this.picker
    }

    private Initialize(client: SupervisorClient, chat: ChatViewModel): void
    {
        for (const subscription of this.pickerSubscriptions)
        {
            subscription.Dispose()
        }
        const picker = new HarnessPickerViewModel(client)
        this.pickerSubscriptions = [
            picker.OnReady.Subscribe(url => this.AttachClient(url)),
            picker.OnCleared.Subscribe(() => this.DetachClient()),
        ]
        this.chat = chat
        const old = this.picker
        this.picker = picker
        this.RaisePropertyChanged(RuntimeManagementService.PickerProperty, old, picker)
        picker.Refresh()
    }

    private AttachClient(url: string): void
    {
        this.chatClient?.Disconnect()
        const client = this.harnessFactory()
        client.Connect(url)
        this.chatClient = client
        this.chat?.Bind(client)
    }

    private DetachClient(): void
    {
        this.chat?.Bind(null)
        this.chatClient?.Disconnect()
        this.chatClient = null
    }
}
