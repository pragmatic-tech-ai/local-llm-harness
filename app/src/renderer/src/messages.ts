import { Observable } from '@pragmatic-tech-ai/mural/runtime'

/** A user turn in the transcript (immutable text). */
export class UserMessage extends Observable
{
    private readonly _text: string

    constructor(text: string)
    {
        super()
        this._text = text
    }

    public get Text(): string
    {
        return this._text
    }
}

/**
 * An assistant turn whose Text grows as chunks stream in. Reasoning chunks grow Reasoning instead (shown dimmed above
 * the answer, never committed, so a history reload has none). HasReasoning turns true before the first reasoning text
 * is written, so its block is visible when the text lands (HiddenTextRule, see RepositoryEditorViewModel).
 */
export class AssistantMessage extends Observable
{
    private static readonly TextProperty = 'Text'
    private static readonly ReasoningProperty = 'Reasoning'
    private static readonly HasReasoningProperty = 'HasReasoning'
    private _text: string
    private _reasoning = ''

    constructor(text: string)
    {
        super()
        this._text = text
    }

    public get Text(): string
    {
        return this._text
    }

    public get Reasoning(): string
    {
        return this._reasoning
    }

    public get HasReasoning(): boolean
    {
        return this._reasoning.length > 0
    }

    public Append(delta: string): void
    {
        const old = this._text
        this._text = old + delta
        this.RaisePropertyChanged(AssistantMessage.TextProperty, old, this._text)
    }

    public AppendReasoning(delta: string): void
    {
        if (delta.length === 0)
        {
            return
        }
        const old = this._reasoning
        const had = this.HasReasoning
        this._reasoning = old + delta
        if (!had)
        {
            this.RaisePropertyChanged(AssistantMessage.HasReasoningProperty, had, true)
        }
        this.RaisePropertyChanged(AssistantMessage.ReasoningProperty, old, this._reasoning)
    }
}

/** A tool call the model made (arguments pre-rendered as JSON text). */
export class ToolCallMessage extends Observable
{
    private readonly _name: string
    private readonly _arguments: string

    constructor(name: string, argumentsText: string)
    {
        super()
        this._name = name
        this._arguments = argumentsText
    }

    public get Name(): string
    {
        return this._name
    }

    public get Arguments(): string
    {
        return this._arguments
    }
}

/** A tool's outcome, shown beneath its call. */
export class ToolResultMessage extends Observable
{
    private static readonly StatusOk = 'ok'
    private static readonly StatusFailed = 'failed'
    private readonly _name: string
    private readonly _ok: boolean
    private readonly _content: string

    constructor(name: string, ok: boolean, contentText: string)
    {
        super()
        this._name = name
        this._ok = ok
        this._content = contentText
    }

    public get Name(): string
    {
        return this._name
    }

    public get Ok(): boolean
    {
        return this._ok
    }

    public get Content(): string
    {
        return this._content
    }

    public get StatusText(): string
    {
        return this._ok ? ToolResultMessage.StatusOk : ToolResultMessage.StatusFailed
    }
}
