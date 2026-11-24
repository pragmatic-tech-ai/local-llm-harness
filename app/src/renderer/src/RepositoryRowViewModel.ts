import { Observable } from '@pragmatic-tech-ai/mural/runtime'
import { RepositoryKind, type RepositorySummary } from './agents'
import { TextFormat } from './TextFormat'

/** One row of the Models panel: `{name}  {Kind}  {n} models`. Updated in place so selection survives a refresh. */
export class RepositoryRowViewModel extends Observable
{
    private static readonly RowFormat = '{0}  {1}  {2} models'
    private static readonly DetailFormat = '{0}  {1} models'
    private static readonly PragmaticLabel = 'Pragmatic'
    private static readonly OpenRouterLabel = 'OpenRouter'
    private static readonly PropText = 'Text'
    private static readonly PropDetailText = 'DetailText'
    private static readonly PropHasError = 'HasError'

    public readonly Name: string

    private summary: RepositorySummary

    constructor(summary: RepositorySummary)
    {
        super()
        this.Name = summary.name
        this.summary = summary
    }

    /** The kind's display name. */
    public static KindLabel(kind: RepositoryKind): string
    {
        return kind === RepositoryKind.OpenRouter ? RepositoryRowViewModel.OpenRouterLabel : RepositoryRowViewModel.PragmaticLabel
    }

    public get Summary(): RepositorySummary
    {
        return this.summary
    }

    public get Kind(): RepositoryKind
    {
        return this.summary.kind
    }

    public get IsOpenRouter(): boolean
    {
        return this.summary.kind === RepositoryKind.OpenRouter
    }

    public get Text(): string
    {
        return TextFormat.Format(RepositoryRowViewModel.RowFormat, this.Name, RepositoryRowViewModel.KindLabel(this.summary.kind), this.summary.model_count)
    }

    /** The row's second line: `{Kind}  {n} models` (the panel is narrow; the name is the first line). */
    public get DetailText(): string
    {
        return TextFormat.Format(RepositoryRowViewModel.DetailFormat, RepositoryRowViewModel.KindLabel(this.summary.kind), this.summary.model_count)
    }

    /** The last model-list fetch failed. */
    public get HasError(): boolean
    {
        return (this.summary.error ?? '').length > 0
    }

    /** Takes a fresher listing of the same repository, raising only what changed. */
    public Update(summary: RepositorySummary): void
    {
        const oldText = this.Text
        const oldError = this.HasError
        this.summary = summary
        if (this.Text !== oldText)
        {
            this.RaisePropertyChanged(RepositoryRowViewModel.PropText, oldText, this.Text)
            this.RaisePropertyChanged(RepositoryRowViewModel.PropDetailText, undefined, this.DetailText)
        }
        if (this.HasError !== oldError)
        {
            this.RaisePropertyChanged(RepositoryRowViewModel.PropHasError, oldError, this.HasError)
        }
    }
}
