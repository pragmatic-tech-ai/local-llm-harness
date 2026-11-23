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

/** An assistant turn whose Text grows as chunks stream in. */
export class AssistantMessage extends Observable
{
    private static readonly TextProperty = 'Text'
    private _text: string

    constructor(text: string)
    {
        super()
        this._text = text
    }

    public get Text(): string
    {
        return this._text
    }

    public Append(delta: string): void
    {
        const old = this._text
        this._text = old + delta
        this.RaisePropertyChanged(AssistantMessage.TextProperty, old, this._text)
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
