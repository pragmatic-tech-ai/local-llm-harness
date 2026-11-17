import { Key, Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { HarnessClient } from './HarnessClient'
import { AssistantMessage, UserMessage } from './messages'

/** Owns the transcript, the composer draft, and the send flow. */
export class ChatViewModel extends Observable
{
    private static readonly DraftProperty = 'Draft'
    private static readonly ErrorPrefix = '\n[error: '
    private static readonly Suffix = ']'
    private static readonly RejectedPrefix = '\n[context window full: needed '
    private static readonly RejectedOf = ' of '
    private static readonly RejectedSuffix = ' tokens]'
    private static readonly SendFailedPrefix = '\n[send failed: '

    public readonly Transcript = new ObservableCollection<UserMessage | AssistantMessage>()
    public EnableThinking = false

    private _draft = ''
    private current: AssistantMessage | null = null
    private readonly _sendCommand: RelayCommand
    private readonly _submitCommand: RelayCommand

    constructor(private readonly client: HarnessClient)
    {
        super()
        this._sendCommand = new RelayCommand(() => this.Send())
        // Enter-to-send: the TextBox (SubmitsOnEnter) leaves Return unhandled so
        // KeyDown bubbles to a DockPanel trigger; this command sends only on Return.
        this._submitCommand = new RelayCommand((arg) =>
        {
            if ((arg as { Key?: unknown } | undefined)?.Key === Key.Return)
            {
                this.Send()
            }
        })
        client.OnTurnChunk.Subscribe(p => this.current?.Append(p.text))
        client.OnTurnDone.Subscribe(() =>
        {
            this.current = null
        })
        client.OnTurnError.Subscribe(p =>
        {
            this.current?.Append(ChatViewModel.ErrorPrefix + p.message + ChatViewModel.Suffix)
            this.current = null
        })
        client.OnTurnRejected.Subscribe(p =>
        {
            this.current?.Append(ChatViewModel.RejectedPrefix + p.needed + ChatViewModel.RejectedOf + p.total + ChatViewModel.RejectedSuffix)
            this.current = null
        })
    }

    public get Draft(): string
    {
        return this._draft
    }

    public set Draft(value: string)
    {
        const old = this._draft
        this._draft = value
        this.RaisePropertyChanged(ChatViewModel.DraftProperty, old, value)
    }

    public get SendCommand(): ICommand
    {
        return this._sendCommand
    }

    public get SubmitCommand(): ICommand
    {
        return this._submitCommand
    }

    private Send(): void
    {
        const draft = this._draft
        if (draft.trim().length === 0)
        {
            return
        }
        this.Transcript.Add(new UserMessage(draft))
        this.Draft = ''
        const reply = new AssistantMessage('')
        this.Transcript.Add(reply)
        this.current = reply
        this.client.SendTurn(draft, this.EnableThinking).catch((e: Error) =>
        {
            reply.Append(ChatViewModel.SendFailedPrefix + e.message + ChatViewModel.Suffix)
            this.current = null
        })
    }
}
