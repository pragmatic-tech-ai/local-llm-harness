import { describe, expect, it } from 'vitest'
import { RepositoryModelRowViewModel } from './RepositoryModelRowViewModel'

describe('RepositoryModelRowViewModel', () =>
{
    it('openrouter_row_shows_context_and_price', () =>
    {
        const row = RepositoryModelRowViewModel.FromOpenRouter({
            id: 'anthropic/claude-sonnet', name: 'Claude', context_length: 200000, prompt_price: 3, completion_price: 15, reasoning: true,
        })
        expect(row.Id).toBe('anthropic/claude-sonnet')
        expect(row.ContextText).toBe('200K')
        expect(row.PriceText).toBe('3.00/15.00')
        expect(row.IsPriced).toBe(true)
        expect(row.RuntimesText).toBe('')
    })

    it('context_rounds_to_k_and_m', () =>
    {
        const row = (n: number) => RepositoryModelRowViewModel.FromOpenRouter({ id: 'x', name: 'x', context_length: n, prompt_price: 0.1, completion_price: 0.3, reasoning: false })
        expect(row(131072).ContextText).toBe('131K')
        expect(row(1048576).ContextText).toBe('1M')
        expect(row(2000000).ContextText).toBe('2M')
        expect(row(512).ContextText).toBe('512')
        expect(row(1).PriceText).toBe('0.10/0.30')
    })

    it('pragmatic_row_lists_runtimes', () =>
    {
        const row = RepositoryModelRowViewModel.FromCatalog({ name: 'gemma-4', runtimes: ['llamacpp', 'candle'], size_hint: null })
        expect(row.Id).toBe('gemma-4')
        expect(row.RuntimesText).toBe('llamacpp, candle')
        expect(row.IsPriced).toBe(false)
        expect(row.ContextText).toBe('')
    })
})
