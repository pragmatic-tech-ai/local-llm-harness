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
