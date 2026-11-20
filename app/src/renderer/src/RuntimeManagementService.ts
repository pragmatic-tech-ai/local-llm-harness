import { ServiceBase, ServiceKey } from '@pragmatic-tech-ai/mural/runtime'
import type { IServiceProvider } from '@pragmatic-tech-ai/mural/runtime'
import { HarnessClient } from './HarnessClient'
import { HarnessPickerViewModel } from './HarnessPickerViewModel'
import type { ChatViewModel } from './ChatViewModel'
import type { SupervisorClient } from './SupervisorClient'

/** Runtime-management capability: owns the harness picker and coordinates it with the chat (a ready harness gets a fresh client bound to the chat; clearing unbinds it). */
export class RuntimeManagementService extends ServiceBase
{
    public static readonly Key = new ServiceKey<RuntimeManagementService>('RuntimeManagementService')
    private static readonly PickerProperty = 'Picker'

    private picker: HarnessPickerViewModel | null = null
    private chat: ChatViewModel | null = null
    private chatClient: HarnessClient | null = null

    constructor(provider: IServiceProvider, private readonly harnessFactory: () => HarnessClient = () => new HarnessClient())
    {
        super(provider)
    }

    public get Picker(): HarnessPickerViewModel | null
    {
        return this.picker
    }

    /** Creates the picker over the supervisor, wires it to the given chat, and kicks off discovery. */
    public Attach(client: SupervisorClient, chat: ChatViewModel): void
    {
        const picker = new HarnessPickerViewModel(client)
        picker.OnReady.Subscribe(url => this.AttachClient(url))
        picker.OnCleared.Subscribe(() => this.DetachClient())
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
