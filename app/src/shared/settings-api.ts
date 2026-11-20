/** IPC channel names for the persisted settings store (shared by main and preload; no Electron imports). */
export enum SettingsChannel
{
    GetSnapshot = 'settings:get-snapshot',
    Save = 'settings:save'
}

/** The settings bridge the preload exposes to the renderer. */
export interface ISettingsBridge
{
    snapshot: Record<string, unknown>
    Save(values: Record<string, unknown>): void
}
