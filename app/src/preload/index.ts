import { contextBridge, ipcRenderer } from 'electron'

/** Exposes the harness bridge (backend ws url) to the renderer. */
class HarnessBridge
{
    private static readonly GlobalName = 'harness'
    private static readonly BackendUrlChannel = 'backend-url'
    private static readonly SupervisorUrlChannel = 'supervisor-url'

    public static Expose(): void
    {
        contextBridge.exposeInMainWorld(HarnessBridge.GlobalName, {
            BackendUrl: (): Promise<string | null> => ipcRenderer.invoke(HarnessBridge.BackendUrlChannel),
            SupervisorUrl: (): Promise<string | null> => ipcRenderer.invoke(HarnessBridge.SupervisorUrlChannel),
        })
    }
}

HarnessBridge.Expose()
