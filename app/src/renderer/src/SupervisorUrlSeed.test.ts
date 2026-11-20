import { describe, expect, it } from 'vitest'
import type { ApplicationSettings } from '@pragmatic-tech-ai/mural/framework'
import { SupervisorUrlSeed } from './SupervisorUrlSeed'

class FakeSettings
{
    public readonly sets: Array<[string, unknown]> = []

    constructor(private readonly value: unknown)
    {
    }

    public Get(): unknown
    {
        return this.value
    }

    public Set(key: string, value: unknown): void
    {
        this.sets.push([key, value])
    }
}

describe('SupervisorUrlSeed', () =>
{
    it('seeds_when_saved_empty_and_env_present', () =>
    {
        const s = new FakeSettings('')
        SupervisorUrlSeed.Seed(s as unknown as ApplicationSettings, 'ws://x')
        expect(s.sets).toEqual([['supervisor.url', 'ws://x']])
    })

    it('does_not_seed_when_saved_nonempty', () =>
    {
        const s = new FakeSettings('ws://saved')
        SupervisorUrlSeed.Seed(s as unknown as ApplicationSettings, 'ws://x')
        expect(s.sets).toHaveLength(0)
    })

    it('does_not_seed_when_env_blank', () =>
    {
        const s = new FakeSettings('')
        SupervisorUrlSeed.Seed(s as unknown as ApplicationSettings, '   ')
        expect(s.sets).toHaveLength(0)
    })

    it('does_not_seed_when_env_null', () =>
    {
        const s = new FakeSettings(undefined)
        SupervisorUrlSeed.Seed(s as unknown as ApplicationSettings, null)
        expect(s.sets).toHaveLength(0)
    })
})
