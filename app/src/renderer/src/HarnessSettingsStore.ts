import type { ISettingsStore } from '@pragmatic-tech-ai/mural/framework'
import type { ISettingsBridge } from '../../shared/settings-api'

/** Renderer-side ISettingsStore backed by the preload settings bridge (window.harness.settings). */
export class HarnessSettingsStore implements ISettingsStore
{
    private static readonly MissingBridgeMessage = 'Settings bridge (window.harness.settings) is not available'

    private readonly bridge: ISettingsBridge

    public constructor(accessor: () => ISettingsBridge | undefined = HarnessSettingsStore.DefaultAccessor)
    {
        const bridge = accessor()
        if (!bridge)
        {
            throw new Error(HarnessSettingsStore.MissingBridgeMessage)
        }
        this.bridge = bridge
    }

    public Load(): Record<string, unknown>
    {
        return this.bridge.snapshot
    }

    public Save(values: Record<string, unknown>): void
    {
        this.bridge.Save(values)
    }

    private static DefaultAccessor(): ISettingsBridge | undefined
    {
        return window.harness?.settings
    }
}
