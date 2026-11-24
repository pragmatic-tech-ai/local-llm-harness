import { describe, expect, it } from 'vitest'
import { RepositoryRowViewModel } from './RepositoryRowViewModel'
import { RepositoryKind, type RepositorySummary } from './agents'

class Rows
{
    public static readonly Caps = { measure_prompt: 'exact' as const, tokenize: true, raw_prompt: true, logprobs: true, load_unload: true }

    public static Summary(name: string, kind: RepositoryKind, count: number): RepositorySummary
    {
        return { name, kind, editable: kind === RepositoryKind.OpenRouter, capabilities: Rows.Caps, model_count: count }
    }
}

describe('RepositoryRowViewModel', () =>
{
    it('text_shows_name_kind_and_count', () =>
    {
        const hive = new RepositoryRowViewModel(Rows.Summary('hive', RepositoryKind.Pragmatic, 12))
        const router = new RepositoryRowViewModel(Rows.Summary('openrouter', RepositoryKind.OpenRouter, 48))
        expect(hive.Text).toBe('hive  Pragmatic  12 models')
        expect(router.Text).toBe('openrouter  OpenRouter  48 models')
        expect(router.DetailText).toBe('OpenRouter  48 models')
        expect(router.IsOpenRouter).toBe(true)
        expect(hive.IsOpenRouter).toBe(false)
    })

    it('update_changes_the_count_in_place', () =>
    {
        const row = new RepositoryRowViewModel(Rows.Summary('or', RepositoryKind.OpenRouter, 0))
        const raised: string[] = []
        row.PropertyChanged('Text').subscribe(() => raised.push('Text'))
        row.Update(Rows.Summary('or', RepositoryKind.OpenRouter, 3))
        expect(row.Text).toBe('or  OpenRouter  3 models')
        expect(raised).toEqual(['Text'])
    })
})
