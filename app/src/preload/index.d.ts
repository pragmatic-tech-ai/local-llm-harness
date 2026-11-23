import type { ISettingsBridge } from '../shared/settings-api'

declare global
{
    interface Window
    {
        harness: { SupervisorUrl(): Promise<string | null>, settings: ISettingsBridge }
    }
}
