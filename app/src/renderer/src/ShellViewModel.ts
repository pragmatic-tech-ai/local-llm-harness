import { Observable } from '@pragmatic-tech-ai/mural/runtime'
import { HarnessClient } from './HarnessClient'
import { HarnessPickerViewModel } from './HarnessPickerViewModel'
import { ChatViewModel } from './ChatViewModel'
import type { SupervisorClient } from './SupervisorClient'

/** Coordinates the harness picker and the chat: a ready harness gets a fresh client bound to chat; clearing unbinds it. */
export class ShellViewModel extends Observable
{
    public readonly Picker: HarnessPickerViewModel
    public readonly Chat: ChatViewModel
    private chatClient: HarnessClient | null = null

    constructor(supervisor: SupervisorClient, private readonly harnessFactory: () => HarnessClient = () => new HarnessClient())
    {
        super()
        this.Picker = new HarnessPickerViewModel(supervisor)
        this.Chat = new ChatViewModel()
        this.Picker.OnReady.Subscribe(url => this.AttachClient(url))
        this.Picker.OnCleared.Subscribe(() => this.DetachClient())
    }

    /** Kicks off runtime/model discovery (and adoption of an already-live harness). */
    public Start(): void
    {
        this.Picker.Refresh()
    }

    private AttachClient(url: string): void
    {
        this.chatClient?.Disconnect()
        const client = this.harnessFactory()
        client.Connect(url)
        this.chatClient = client
        this.Chat.Bind(client)
    }

    private DetachClient(): void
    {
        this.Chat.Bind(null)
        this.chatClient?.Disconnect()
        this.chatClient = null
    }
}
