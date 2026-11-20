import { contextBridge, ipcRenderer } from 'electron'
import { SettingsChannel } from '../shared/settings-api'

/** Exposes the harness bridge (backend ws url, persisted settings) to the renderer. */
class HarnessBridge
{
    private static readonly GlobalName = 'harness'
    private static readonly BackendUrlChannel = 'backend-url'
    private static readonly SupervisorUrlChannel = 'supervisor-url'

    public static Expose(): void
    {
        const snapshot = Object.freeze(ipcRenderer.sendSync(SettingsChannel.GetSnapshot) as Record<string, unknown>)
        contextBridge.exposeInMainWorld(HarnessBridge.GlobalName, {
            BackendUrl: (): Promise<string | null> => ipcRenderer.invoke(HarnessBridge.BackendUrlChannel),
            SupervisorUrl: (): Promise<string | null> => ipcRenderer.invoke(HarnessBridge.SupervisorUrlChannel),
            settings: {
                snapshot,
                Save: (values: Record<string, unknown>): Promise<void> => ipcRenderer.invoke(SettingsChannel.Save, values),
            },
        })
    }
}

HarnessBridge.Expose()
