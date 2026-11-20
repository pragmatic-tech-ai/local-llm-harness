import { describe, expect, it } from 'vitest'
import type { ISettingsBridge } from '../../shared/settings-api'
import { HarnessSettingsStore } from './HarnessSettingsStore'

class FakeBridge implements ISettingsBridge
{
    public snapshot: Record<string, unknown> = { 'a': 1 }
    public saved: Record<string, unknown>[] = []

    public Save(values: Record<string, unknown>): void
    {
        this.saved.push(values)
    }
}

describe('HarnessSettingsStore', () =>
{
    it('load_returns_bridge_snapshot', () =>
    {
        const bridge = new FakeBridge()
        const store = new HarnessSettingsStore(() => bridge)
        expect(store.Load()).toEqual({ 'a': 1 })
    })

    it('save_forwards_values_to_bridge', () =>
    {
        const bridge = new FakeBridge()
        const store = new HarnessSettingsStore(() => bridge)
        const values = { 'b': 'x' }
        store.Save(values)
        expect(bridge.saved).toEqual([values])
    })

    it('throws_when_bridge_absent', () =>
    {
        expect(() => new HarnessSettingsStore(() => undefined)).toThrow(/settings bridge/i)
    })
})
