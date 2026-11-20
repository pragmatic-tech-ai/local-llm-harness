import type { ISettingsBridge } from '../shared/settings-api'

declare global
{
    interface Window
    {
        harness: { BackendUrl(): Promise<string | null>, SupervisorUrl(): Promise<string | null>, settings: ISettingsBridge }
    }
}
