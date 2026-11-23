/** The app's one home for message text helpers: positional `{n}` formatting and error-to-text. */
export class TextFormat
{
    private static readonly Token = /\{(\d+)\}/g

    /** Replaces each `{n}` in the template with the n-th value. */
    public static Format(template: string, ...values: unknown[]): string
    {
        return template.replace(TextFormat.Token, (_m, i: string) => String(values[Number(i)]))
    }

    /** The message of a rejection: an Error's message, else the value as text. */
    public static Message(e: unknown): string
    {
        return e instanceof Error ? e.message : String(e)
    }
}
