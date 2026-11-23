import { Observable, RelayCommand } from '@pragmatic-tech-ai/mural/runtime'
import { ButtonVariant, DialogAction, type DialogService } from '@pragmatic-tech-ai/mural/framework'

/** What a one-line text prompt asks. */
export interface ITextPrompt
{
    Title: string
    Label: string
    Text: string
    ConfirmLabel: string
}

/** Body of a one-line text prompt dialog (rename, new name): a label over a TextBox. */
export class TextPromptViewModel extends Observable
{
    private static readonly PropText = 'Text'
    private static readonly CancelLabel = 'Cancel'
    private static readonly Width = 380

    public readonly Label: string
    private _text: string

    constructor(label: string, text: string)
    {
        super()
        this.Label = label
        this._text = text
    }

    public get Text(): string
    {
        return this._text
    }

    public set Text(value: string)
    {
        const old = this._text
        if (old !== value)
        {
            this._text = value
            this.RaisePropertyChanged(TextPromptViewModel.PropText, old, value)
        }
    }

    /** Shows the prompt; resolves to the trimmed text on confirm, or undefined when cancelled or left blank. */
    public static async Ask(dialogs: DialogService, prompt: ITextPrompt): Promise<string | undefined>
    {
        const body = new TextPromptViewModel(prompt.Label, prompt.Text)
        const result = await dialogs.Show<string>({
            Title: prompt.Title,
            Content: body,
            Width: TextPromptViewModel.Width,
            Actions: [
                new DialogAction(TextPromptViewModel.CancelLabel, new RelayCommand(() => dialogs.Close(undefined))),
                new DialogAction(prompt.ConfirmLabel, new RelayCommand(() => dialogs.Close(body.Text)), ButtonVariant.Filled),
            ],
        })
        const text = typeof result === 'string' ? result.trim() : ''
        return text.length > 0 ? text : undefined
    }
}
