import { Observable } from '@pragmatic-tech-ai/mural/runtime'
import { TextFormat } from './TextFormat'

/** One row of a session's notes panel: the note's number and text. */
export class NoteRowViewModel extends Observable
{
    private static readonly RowFormat = '{0}. {1}'

    constructor(private readonly number: number, private readonly text: string)
    {
        super()
    }

    /** `{n}. {text}`, numbered from 1. */
    public get Text(): string
    {
        return TextFormat.Format(NoteRowViewModel.RowFormat, this.number, this.text)
    }
}
