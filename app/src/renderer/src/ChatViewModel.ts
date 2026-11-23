import { Key, Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { RuntimeClient, type IDisposable, type ModelProps, type ToolCallNote } from './RuntimeClient'
import { AssistantMessage, ToolCallMessage, ToolResultMessage, UserMessage } from './messages'
import { InstrumentPanelViewModel } from './InstrumentPanelViewModel'

/** Owns the transcript, the composer draft, and the send flow. */
export class ChatViewModel extends Observable
{
    private static readonly DraftProperty = 'Draft'
    private static readonly CanChatProperty = 'CanChat'
    private static readonly ComposerPlaceholderProperty = 'ComposerPlaceholder'
    private static readonly NoHarnessHint = 'Start a harness to chat'
    private static readonly ComposerHint = 'Message the model…'
    private static readonly ErrorFormat = '\n[error: {0}]'
    private static readonly RejectedFormat = '\n[context window full: needed {0} of {1} tokens]'
    private static readonly DisconnectedNote = '\n[disconnected]'
    private static readonly SendFailedFormat = '\n[send failed: {0}]'
    private static readonly ToolsProperty = 'ToolsText'
    private static readonly ToolsOff = 'tools: off'
    private static readonly ToolsPrefix = 'tools: '
    private static readonly ToolsSeparator = ', '
    private static readonly NotKeptFormat = '[not kept: {0}]'
    private static readonly ParseErrorFormat = '{0}  [parse error: {1}]'
    private static readonly AnsweredReason = 'answered'

    public readonly Transcript = new ObservableCollection<UserMessage | AssistantMessage | ToolCallMessage | ToolResultMessage>()
    public readonly Panel: InstrumentPanelViewModel

    private _draft = ''
    /** The live assistant message; null between model passes of one tool loop. */
    private segment: AssistantMessage | null = null
    /** A turn (the whole tool loop) is in flight from Send until its send_turn promise settles. */
    private busy = false
    /** Identifies the turn a settling promise belongs to, so a stale settle cannot clear a newer turn. */
    private turnToken = 0
    /** Set once turn_error/turn_rejected has reported, so a following send_turn failure adds no duplicate note. */
    private reported = false
    private _toolsText = ChatViewModel.ToolsOff
    private readonly _sendCommand: RelayCommand
    private readonly _submitCommand: RelayCommand
    private client: RuntimeClient | null = null
    private subscriptions: IDisposable[] = []

    constructor(client?: RuntimeClient)
    {
        super()
        this.Panel = new InstrumentPanelViewModel()
        this.Panel.OnModelProps.Subscribe(p => this.RefreshTools(p))
        this._sendCommand = new RelayCommand(() => this.Send(), () => this.CanChat)
        // Enter-to-send: the TextBox (SubmitsOnEnter) leaves Return unhandled so
        // KeyDown bubbles to a DockPanel trigger; this command sends only on Return.
        this._submitCommand = new RelayCommand((arg) =>
        {
            if ((arg as { Key?: unknown } | undefined)?.Key === Key.Return)
            {
                this.Send()
            }
        }, () => this.CanChat)
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
    public Bind(client: RuntimeClient | null): void
    {
        for (const s of this.subscriptions)
        {
            s.Dispose()
        }
        this.subscriptions = []
        // A rebind means a new/removed harness: drop any turn waiting on the old one.
        this.AbandonTurn(ChatViewModel.DisconnectedNote)
        const old = this.CanChat
        this.client = client
        if (client)
        {
            this.subscriptions.push(
                client.OnTurnStarted.Subscribe(() => this.OnPassStarted()),
                client.OnTurnChunk.Subscribe(p => this.OnChunk(p.text)),
                client.OnTurnDone.Subscribe(() => { this.segment = null }),
                client.OnTurnError.Subscribe(p => this.ReportFailure(ChatViewModel.Format(ChatViewModel.ErrorFormat, p.message))),
                client.OnTurnRejected.Subscribe(p => this.ReportFailure(ChatViewModel.Format(ChatViewModel.RejectedFormat, p.needed, p.total))),
                client.OnToolCall.Subscribe(p => this.OnToolCall(p)),
                client.OnToolResult.Subscribe(p => this.Transcript.Add(new ToolResultMessage(p.name, p.ok, ChatViewModel.Stringify(p.content)))),
                client.OnLoopDone.Subscribe(p => this.OnLoopDone(p.reason)))
        }
        this.SetToolsText(ChatViewModel.ToolsOff)
        this.Panel.Bind(client)
        this._sendCommand.RaiseCanExecuteChanged()
        this._submitCommand.RaiseCanExecuteChanged()
        this.RaisePropertyChanged(ChatViewModel.CanChatProperty, old, this.CanChat)
        this.RaisePropertyChanged(ChatViewModel.ComposerPlaceholderProperty, old ? ChatViewModel.ComposerHint : ChatViewModel.NoHarnessHint, this.ComposerPlaceholder)
    }

    /** Composer hint: explains why the box is disabled when no harness is bound. */
    public get ComposerPlaceholder(): string
    {
        return this.CanChat ? ChatViewModel.ComposerHint : ChatViewModel.NoHarnessHint
    }

    /** Header indicator: the tools the runtime exposes, or 'tools: off'. */
    public get ToolsText(): string
    {
        return this._toolsText
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

    /** A turn is in flight from Send until its send_turn promise settles (after loop_done); sends are blocked meanwhile. */
    private Send(): void
    {
        const client = this.client
        const draft = this._draft.trim()
        if (client === null || draft.length === 0 || this.busy)
        {
            return
        }
        this.Transcript.Add(new UserMessage(draft))
        this.Draft = ''
        const reply = new AssistantMessage('')
        this.Transcript.Add(reply)
        this.segment = reply
        this.busy = true
        this.reported = false
        const token = ++this.turnToken
        this.Panel.ClearWarning()
        // Apply the knobs to THIS turn (set_sampling lands before send_turn on the single ws stream).
        client.SetSampling(this.Panel.Config).catch((e: Error) => this.Panel.ReportSamplingError(e.message))
        client.SetThinking(this.Panel.EnableThinking).catch((e: Error) => this.Panel.ReportSamplingError(e.message))
        client.SendTurn(draft).then(
            () => this.Settle(token),
            (e: Error) =>
            {
                // A turn_error/turn_rejected already reported; a stale turn (rebound) reports nothing.
                if (token === this.turnToken && !this.reported)
                {
                    this.AppendNote(ChatViewModel.Format(ChatViewModel.SendFailedFormat, e.message))
                }
                this.Settle(token)
            })
    }

    private Settle(token: number): void
    {
        if (token === this.turnToken)
        {
            this.busy = false
            this.segment = null
        }
    }

    /** A new model pass of the tool loop opens a fresh assistant message. */
    private OnPassStarted(): void
    {
        if (this.busy && this.segment === null)
        {
            this.segment = new AssistantMessage('')
            this.Transcript.Add(this.segment)
        }
    }

    /** Chunks go to the live segment; after a tool call closed it, the next chunk of the pass opens a new one below the call. */
    private OnChunk(text: string): void
    {
        if (this.segment === null)
        {
            if (!this.busy || text.length === 0)
            {
                return
            }
            this.segment = new AssistantMessage('')
            this.Transcript.Add(this.segment)
        }
        this.segment.Append(text)
    }

    /** A call block closes the live segment so later text of the same pass renders below it. */
    private OnToolCall(p: ToolCallNote): void
    {
        this.Transcript.Add(ChatViewModel.CallMessage(p))
        this.segment = null
    }

    private ReportFailure(note: string): void
    {
        this.reported = true
        this.AppendNote(note)
        this.segment = null
    }

    /** Appends to the live segment, or opens one when the failure lands between passes. */
    private AppendNote(note: string): void
    {
        if (this.segment === null)
        {
            this.segment = new AssistantMessage('')
            this.Transcript.Add(this.segment)
        }
        this.segment.Append(note)
    }

    private OnLoopDone(reason: string): void
    {
        this.segment = null
        if (reason !== ChatViewModel.AnsweredReason)
        {
            this.Transcript.Add(new AssistantMessage(ChatViewModel.Format(ChatViewModel.NotKeptFormat, reason)))
        }
    }

    /** Drops any turn waiting on a previous client, noting the abandoned reply. */
    private AbandonTurn(note: string): void
    {
        if (this.busy)
        {
            this.AppendNote(note)
        }
        this.segment = null
        this.busy = false
        this.turnToken++
    }

    /** Panel-fetched props for the bound client: list the runtime's tools when the model supports them. */
    private RefreshTools(props: ModelProps): void
    {
        const client = this.client
        if (client === null || props.supports_tools !== true)
        {
            return
        }
        client.ListTools().then(
            tools =>
            {
                if (this.client === client && tools.length > 0)
                {
                    this.SetToolsText(ChatViewModel.ToolsPrefix + tools.map(t => t.name).join(ChatViewModel.ToolsSeparator))
                }
            },
            () => undefined)
    }

    private SetToolsText(value: string): void
    {
        const old = this._toolsText
        this._toolsText = value
        if (old !== value)
        {
            this.RaisePropertyChanged(ChatViewModel.ToolsProperty, old, value)
        }
    }

    /** A call whose arguments did not parse shows the raw text plus the parse error. */
    private static CallMessage(p: ToolCallNote): ToolCallMessage
    {
        const text = p.parse_error ? ChatViewModel.Format(ChatViewModel.ParseErrorFormat, p.raw, p.parse_error) : ChatViewModel.Stringify(p.arguments)
        return new ToolCallMessage(p.name, text)
    }

    /** Strings pass through; undefined/null render empty; everything else as JSON. */
    private static Stringify(value: unknown): string
    {
        if (value === undefined || value === null)
        {
            return ''
        }
        return typeof value === 'string' ? value : (JSON.stringify(value) ?? String(value))
    }

    private static Format(template: string, ...values: unknown[]): string
    {
        return template.replace(/\{(\d+)\}/g, (_m, i: string) => String(values[Number(i)]))
    }
}
