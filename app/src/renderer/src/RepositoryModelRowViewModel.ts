import { Observable } from '@pragmatic-tech-ai/mural/runtime'
import type { CatalogModel, OpenRouterModelInfo } from './agents'

/**
 * One row of the repository editor's model table. An OpenRouter model shows its id, its context (`200K`) and its
 * price in and out per million tokens (`3.00/15.00`); a Pragmatic model its name and the runtimes able to load it.
 */
export class RepositoryModelRowViewModel extends Observable
{
    private static readonly Thousand = 1000
    private static readonly Million = 1000000
    private static readonly KiloSuffix = 'K'
    private static readonly MegaSuffix = 'M'
    private static readonly PriceSeparator = '/'
    private static readonly RuntimeSeparator = ', '
    private static readonly PriceDigits = 2

    public readonly Id: string
    public readonly ContextText: string
    public readonly PriceText: string
    public readonly RuntimesText: string

    private constructor(id: string, contextText: string, priceText: string, runtimesText: string)
    {
        super()
        this.Id = id
        this.ContextText = contextText
        this.PriceText = priceText
        this.RuntimesText = runtimesText
    }

    public static FromOpenRouter(model: OpenRouterModelInfo): RepositoryModelRowViewModel
    {
        const price = model.prompt_price.toFixed(RepositoryModelRowViewModel.PriceDigits)
            + RepositoryModelRowViewModel.PriceSeparator
            + model.completion_price.toFixed(RepositoryModelRowViewModel.PriceDigits)
        return new RepositoryModelRowViewModel(model.id, RepositoryModelRowViewModel.ContextLabel(model.context_length), price, '')
    }

    public static FromCatalog(model: CatalogModel): RepositoryModelRowViewModel
    {
        return new RepositoryModelRowViewModel(model.name, '', '', (model.runtimes ?? []).join(RepositoryModelRowViewModel.RuntimeSeparator))
    }

    /** A wire model of either kind: one with an `id` is an OpenRouter model. */
    public static From(model: CatalogModel | OpenRouterModelInfo): RepositoryModelRowViewModel
    {
        return 'id' in model ? RepositoryModelRowViewModel.FromOpenRouter(model) : RepositoryModelRowViewModel.FromCatalog(model)
    }

    /** A token count as `512`, `131K` or `1M` (one decimal kept below 10M when not whole, e.g. `1.5M`). */
    public static ContextLabel(tokens: number): string
    {
        if (tokens >= RepositoryModelRowViewModel.Million)
        {
            const millions = Math.round(tokens / RepositoryModelRowViewModel.Million * 10) / 10
            return String(millions) + RepositoryModelRowViewModel.MegaSuffix
        }
        if (tokens >= RepositoryModelRowViewModel.Thousand)
        {
            return String(Math.round(tokens / RepositoryModelRowViewModel.Thousand)) + RepositoryModelRowViewModel.KiloSuffix
        }
        return String(tokens)
    }

    /** The row carries a price (an OpenRouter model). */
    public get IsPriced(): boolean
    {
        return this.PriceText.length > 0
    }
}
