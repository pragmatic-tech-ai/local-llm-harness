import { Key, Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { RuntimeClient, type IDisposable, type ToolCallNote } from './RuntimeClient'
import type { ChatMessage } from './agents'
import { AssistantMessage, ToolCallMessage, ToolResultMessage, UserMessage } from './messages'
import { InstrumentPanelViewModel } from './InstrumentPanelViewModel'
import { TypedSignal } from './JsonRpcConnection'
import { TextFormat } from './TextFormat'

export type TranscriptItem = UserMessage | AssistantMessage | ToolCallMessage | ToolResultMessage

/** Owns the transcript, the composer draft, and the send flow of the attached session. */
export class ChatViewModel extends Observable
{
    private static readonly DraftProperty = 'Draft'
    private static readonly CanChatProperty = 'CanChat'
    private static readonly ComposerPlaceholderProperty = 'ComposerPlaceholder'
    private static readonly NoSessionHint = 'Select a session to chat'
    private static readonly ComposerHint = 'Message the model…'
    private static readonly ErrorFormat = '\n[error: {0}]'
    private static readonly RejectedFormat = '\n[context window full: needed {0} of {1} tokens]'
    private static readonly DisconnectedNote = '\n[disconnected]'
    private static readonly SendFailedFormat = '\n[send failed: {0}]'
    private static readonly LoadingNote = '[loading model…]'
    private static readonly ErrorKey = 'error'
    private static readonly NotKeptFormat = '[not kept: {0}]'
    private static readonly ParseErrorFormat = '{0}  [parse error: {1}]'
    private static readonly AnsweredReason = 'answered'
    private static readonly RoleUser = 'user'
    private static readonly RoleAssistant = 'assistant'
    private static readonly RoleTool = 'tool'

    public readonly Transcript = new ObservableCollection<TranscriptItem>()
    public readonly Panel: InstrumentPanelViewModel
    /** Raised when a turn starts (true) or settles / is abandoned (false). */
    public readonly OnBusyChanged = new TypedSignal<boolean>()

    private _draft = ''
    /** The live assistant message; null between model passes of one tool loop. */
    private segment: AssistantMessage | null = null
    /** A turn (the whole tool loop) is in flight from Send until its send_turn promise settles. */
    private busy = false
    /** Identifies the turn a settling promise belongs to, so a stale settle cannot clear a newer turn. */
    private turnToken = 0
    /** Set once turn_error/turn_rejected has reported, so a following send_turn failure adds no duplicate note. */
    private reported = false
    private readonly _sendCommand: RelayCommand
    private readonly _submitCommand: RelayCommand
    private client: RuntimeClient | null = null
    private subscriptions: IDisposable[] = []

    constructor(client?: RuntimeClient)
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

    /** Binds to the client once a session is attached on it, or unbinds with null. Stale subscriptions are disposed first. */
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
                client.OnTurnError.Subscribe(p => this.ReportFailure(TextFormat.Format(ChatViewModel.ErrorFormat, p.message))),
                client.OnTurnRejected.Subscribe(p => this.ReportFailure(TextFormat.Format(ChatViewModel.RejectedFormat, p.needed, p.total))),
                client.OnToolCall.Subscribe(p => this.OnToolCall(p)),
                client.OnToolResult.Subscribe(p => this.Transcript.Add(new ToolResultMessage(p.name, p.ok, ChatViewModel.Stringify(p.content)))),
                client.OnLoopDone.Subscribe(p => this.OnLoopDone(p.reason)),
                client.OnSessionLoading.Subscribe(() => this.OnSessionLoading()))
        }
        this.Panel.Bind(client)
        this._sendCommand.RaiseCanExecuteChanged()
        this._submitCommand.RaiseCanExecuteChanged()
        this.RaisePropertyChanged(ChatViewModel.CanChatProperty, old, this.CanChat)
        this.RaisePropertyChanged(ChatViewModel.ComposerPlaceholderProperty, old ? ChatViewModel.ComposerHint : ChatViewModel.NoSessionHint, this.ComposerPlaceholder)
    }

    /** Composer hint: explains why the box is disabled when no session is attached. */
    public get ComposerPlaceholder(): string
    {
        return this.CanChat ? ChatViewModel.ComposerHint : ChatViewModel.NoSessionHint
    }

    /** Replaces the transcript with a session's committed history (system messages hidden); drops any turn in flight. */
    public LoadHistory(messages: readonly ChatMessage[]): void
    {
        this.AbandonTurn('')
        const items = ChatViewModel.HistoryItems(messages)
        this.Transcript.Batch(() =>
        {
            this.Transcript.Clear()
            for (const item of items)
            {
                this.Transcript.Add(item)
            }
        })
    }

    /** Empties the transcript (no session shown). */
    public Clear(): void
    {
        this.AbandonTurn('')
        this.Transcript.Clear()
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
        this.SetBusy(true)
        this.reported = false
        const token = ++this.turnToken
        this.Panel.ClearWarning()
        // Knobs changed since the session's values were applied go out before THIS turn (set_sampling /
        // set_thinking land before send_turn on the single ws stream; the runtime persists them per session).
        const sampling = this.Panel.TakeSamplingChange()
        if (sampling !== null)
        {
            client.SetSampling(sampling).catch((e: unknown) => this.Panel.ReportSamplingError(TextFormat.Message(e)))
        }
        const thinking = this.Panel.TakeThinkingChange()
        if (thinking !== null)
        {
            client.SetThinking(thinking).catch((e: unknown) => this.Panel.ReportSamplingError(TextFormat.Message(e)))
        }
        client.SendTurn(draft).then(
            () => this.Settle(token),
            (e: unknown) =>
            {
                // A turn_error/turn_rejected already reported; a stale turn (rebound) reports nothing.
                if (token === this.turnToken && !this.reported)
                {
                    this.AppendNote(TextFormat.Format(ChatViewModel.SendFailedFormat, TextFormat.Message(e)))
                }
                this.Settle(token)
            })
    }

    private Settle(token: number): void
    {
        if (token === this.turnToken)
        {
            this.SetBusy(false)
            this.segment = null
        }
    }

    /** True from Send until the turn's send_turn settles (or the turn is abandoned). */
    public get IsBusy(): boolean
    {
        return this.busy
    }

    private SetBusy(value: boolean): void
    {
        if (this.busy !== value)
        {
            this.busy = value
            this.OnBusyChanged.Raise(value)
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

    /** The runtime loads the session's model before this turn: say so where the reply will appear. */
    private OnSessionLoading(): void
    {
        if (!this.busy)
        {
            return
        }
        if (this.segment === null || this.segment.Text.length > 0)
        {
            this.Transcript.Add(new AssistantMessage(ChatViewModel.LoadingNote))
        }
        else
        {
            this.segment.Append(ChatViewModel.LoadingNote)
        }
        // The first pass then opens its own reply below the note.
        this.segment = null
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
            this.Transcript.Add(new AssistantMessage(TextFormat.Format(ChatViewModel.NotKeptFormat, reason)))
        }
    }

    /** Drops any turn waiting on a previous client, noting the abandoned reply (an empty note adds nothing). */
    private AbandonTurn(note: string): void
    {
        if (this.busy && note.length > 0)
        {
            this.AppendNote(note)
        }
        this.segment = null
        this.SetBusy(false)
        this.turnToken++
    }

    /** Committed messages as transcript items: tool calls become call blocks, tool messages result blocks; system is hidden. */
    private static HistoryItems(messages: readonly ChatMessage[]): TranscriptItem[]
    {
        const items: TranscriptItem[] = []
        const callNames = new Map<string, string>()
        for (const message of messages)
        {
            switch (message.role)
            {
                case ChatViewModel.RoleUser:
                    items.push(new UserMessage(message.content))
                    break
                case ChatViewModel.RoleAssistant:
                    if (message.content.length > 0)
                    {
                        items.push(new AssistantMessage(message.content))
                    }
                    for (const call of message.tool_calls ?? [])
                    {
                        callNames.set(call.id, call.function.name)
                        items.push(new ToolCallMessage(call.function.name, ChatViewModel.Stringify(call.function.arguments)))
                    }
                    break
                case ChatViewModel.RoleTool:
                {
                    const result = message.tool_result ?? message.content
                    const name = callNames.get(message.tool_call_id ?? '') ?? ''
                    items.push(new ToolResultMessage(name, !ChatViewModel.IsErrorResult(result), ChatViewModel.Stringify(result)))
                    break
                }
            }
        }
        return items
    }

    /** A failed tool's committed result is the runtime's `{error: msg}` object. */
    private static IsErrorResult(result: unknown): boolean
    {
        if (typeof result !== 'object' || result === null || Array.isArray(result))
        {
            return false
        }
        const keys = Object.keys(result)
        return keys.length === 1 && keys[0] === ChatViewModel.ErrorKey
    }

    /** A call whose arguments did not parse shows the raw text plus the parse error. */
    private static CallMessage(p: ToolCallNote): ToolCallMessage
    {
        const text = p.parse_error ? TextFormat.Format(ChatViewModel.ParseErrorFormat, p.raw, p.parse_error) : ChatViewModel.Stringify(p.arguments)
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

}
