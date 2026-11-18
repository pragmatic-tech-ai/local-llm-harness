import { Key, Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { HarnessClient, type IDisposable } from './HarnessClient'
import { AssistantMessage, UserMessage } from './messages'
import { InstrumentPanelViewModel } from './InstrumentPanelViewModel'

/** Owns the transcript, the composer draft, and the send flow. */
export class ChatViewModel extends Observable
{
    private static readonly DraftProperty = 'Draft'
    private static readonly CanChatProperty = 'CanChat'
    private static readonly ErrorFormat = '\n[error: {0}]'
    private static readonly RejectedFormat = '\n[context window full: needed {0} of {1} tokens]'
    private static readonly SendFailedFormat = '\n[send failed: {0}]'

    public readonly Transcript = new ObservableCollection<UserMessage | AssistantMessage>()
    public readonly Panel: InstrumentPanelViewModel

    private _draft = ''
    private current: AssistantMessage | null = null
    private readonly _sendCommand: RelayCommand
    private readonly _submitCommand: RelayCommand
    private client: HarnessClient | null = null
    private subscriptions: IDisposable[] = []

    constructor(client?: HarnessClient)
    {
        super()
        this.Panel = new InstrumentPanelViewModel()
        this._sendCommand = new RelayCommand(() => this.Send(), () => this.CanChat)
        // Enter-to-send: the TextBox (SubmitsOnEnter) leaves Return unhandled so
        // KeyDown bubbles to a DockPanel trigger; this command sends only on Return.
        this._submitCommand = new RelayCommand((arg) =>
        {
            if ((arg as { Key?: unknown } | undefined)?.Key === Key.Return)
            {
                this.Send()
            }
        })
        if (client)
        {
            this.Bind(client)
        }
    }

    public get CanChat(): boolean
    {
        return this.client !== null
    }

    /** Rebinds to a (new) harness client, or unbinds with null. Stale subscriptions are disposed first. */
    public Bind(client: HarnessClient | null): void
    {
        for (const s of this.subscriptions)
        {
            s.Dispose()
        }
        this.subscriptions = []
        // A rebind means a new/removed harness: drop any turn waiting on the old one.
        this.current = null
        const old = this.CanChat
        this.client = client
        if (client)
        {
            this.subscriptions.push(
                client.OnTurnChunk.Subscribe(p => this.current?.Append(p.text)),
                client.OnTurnDone.Subscribe(() => this.EndTurn(null)),
                client.OnTurnError.Subscribe(p => this.EndTurn(ChatViewModel.Format(ChatViewModel.ErrorFormat, p.message))),
                client.OnTurnRejected.Subscribe(p => this.EndTurn(ChatViewModel.Format(ChatViewModel.RejectedFormat, p.needed, p.total))))
        }
        this.Panel.Bind(client)
        this._sendCommand.RaiseCanExecuteChanged()
        this._submitCommand.RaiseCanExecuteChanged()
        this.RaisePropertyChanged(ChatViewModel.CanChatProperty, old, this.CanChat)
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

    /** A turn is in flight while `current` is set; sends are blocked until a terminal notification clears it. */
    private Send(): void
    {
        const client = this.client
        const draft = this._draft.trim()
        if (client === null || draft.length === 0 || this.current !== null)
        {
            return
        }
        this.Transcript.Add(new UserMessage(draft))
        this.Draft = ''
        const reply = new AssistantMessage('')
        this.Transcript.Add(reply)
        this.current = reply
        this.Panel.ClearWarning()
        // Apply the knobs to THIS turn (set_sampling lands before send_turn on the single ws stream).
        client.SetSampling(this.Panel.Config).catch((e: Error) => this.Panel.ReportSamplingError(e.message))
        client.SendTurn(draft, this.Panel.EnableThinking).then(
            () => this.ClearIfCurrent(reply),
            (e: Error) =>
            {
                // Only a still-active turn reports: after a terminal notification
                // current !== reply, so no duplicate note follows [error: ...].
                if (this.current === reply)
                {
                    reply.Append(ChatViewModel.Format(ChatViewModel.SendFailedFormat, e.message))
                    this.current = null
                }
            })
    }

    private ClearIfCurrent(reply: AssistantMessage): void
    {
        if (this.current === reply)
        {
            this.current = null
        }
    }

    private EndTurn(note: string | null): void
    {
        if (note !== null)
        {
            this.current?.Append(note)
        }
        this.current = null
    }

    private static Format(template: string, ...values: unknown[]): string
    {
        return template.replace(/\{(\d+)\}/g, (_m, i: string) => String(values[Number(i)]))
    }
}
