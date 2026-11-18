import { app, BrowserWindow, ipcMain } from 'electron'
import { join } from 'node:path'
import { electronApp, is, optimizer } from '@electron-toolkit/utils'
import { Sidecar } from './sidecar'
import { BackendUrl } from './backend-url'
import { SupervisorUrl } from './supervisor-url'

/** Owns the BrowserWindow and the Rust backend sidecar for the app's lifetime. */
export class MainApplication
{
    private static readonly AppUserModelId = 'com.local-llm-harness.app'
    private static readonly BackgroundColor = '#0A0A0B'
    private static readonly BackendUrlChannel = 'backend-url'
    private static readonly SupervisorUrlChannel = 'supervisor-url'
    private static readonly BeforeQuitEvent = 'before-quit'
    private static readonly BrowserWindowCreatedEvent = 'browser-window-created'
    private static readonly ActivateEvent = 'activate'
    private static readonly WindowAllClosedEvent = 'window-all-closed'
    private static readonly ReadyToShowEvent = 'ready-to-show'
    private static readonly DarwinPlatform = 'darwin'
    private static readonly PreloadFile = '../preload/index.js'
    private static readonly RendererFile = '../renderer/index.html'
    private static readonly RendererUrlEnv = 'ELECTRON_RENDERER_URL'
    private static readonly ServerUrlEnv = 'HARNESS_SERVER_URL'
    private static readonly ModelEnv = 'HARNESS_MODEL'
    private static readonly BindEnv = 'HARNESS_BIND'
    private static readonly ExitMessage = 'sidecar exited unexpectedly: '
    private static readonly StartFailedMessage = 'sidecar failed to start: '
    private static readonly WindowWidth = 1280
    private static readonly WindowHeight = 800

    // Unset env vars fall back to the Sidecar defaults.
    private readonly sidecar = new Sidecar({
        serverUrl: process.env[MainApplication.ServerUrlEnv],
        model: process.env[MainApplication.ModelEnv],
        bind: process.env[MainApplication.BindEnv],
    })
    private readonly remoteUrl: string | null = MainApplication.ResolveBackendUrl(process.env)
    private readonly supervisorUrl: string | null = SupervisorUrl.Resolve(process.env)
    private quitting = false
    private startPromise: Promise<unknown> | null = null

    /** The remote backend url from HARNESS_BACKEND_URL, or null to spawn the local sidecar. */
    public static ResolveBackendUrl(env: NodeJS.ProcessEnv): string | null
    {
        return BackendUrl.Resolve(env)
    }

    public Run(): void
    {
        this.sidecar.OnExit.Subscribe((code, expected) =>
        {
            if (!expected)
            {
                console.error(MainApplication.ExitMessage + code)
            }
        })
        ipcMain.handle(MainApplication.SupervisorUrlChannel, () => this.supervisorUrl)
        ipcMain.handle(MainApplication.BackendUrlChannel, async () =>
        {
            if (this.remoteUrl)
            {
                return this.remoteUrl
            }
            // Wait for the sidecar to finish starting so the renderer gets the
            // real url, not the null it holds before Start() resolves.
            try
            {
                await this.startPromise
            }
            catch
            {
                // start failed; return the current (likely null) url
            }
            return this.sidecar.Url
        })
        app.on(MainApplication.BeforeQuitEvent, (event) => void this.OnBeforeQuit(event))
        app.on(MainApplication.WindowAllClosedEvent, () =>
        {
            if (process.platform !== MainApplication.DarwinPlatform)
            {
                app.quit()
            }
        })
        void app.whenReady().then(() => this.OnReady())
    }

    private async OnReady(): Promise<void>
    {
        electronApp.setAppUserModelId(MainApplication.AppUserModelId)
        app.on(MainApplication.BrowserWindowCreatedEvent, (_event, window) => optimizer.watchWindowShortcuts(window))
        app.on(MainApplication.ActivateEvent, () =>
        {
            if (BrowserWindow.getAllWindows().length === 0)
            {
                this.CreateWindow()
            }
        })
        if (this.supervisorUrl || this.remoteUrl)
        {
            this.CreateWindow()
            return
        }
        this.startPromise = this.sidecar.Start()
        this.CreateWindow()
        try
        {
            await this.startPromise
        }
        catch (err)
        {
            console.error(MainApplication.StartFailedMessage + (err instanceof Error ? err.message : String(err)))
        }
    }

    private CreateWindow(): void
    {
        const window = new BrowserWindow({
            width: MainApplication.WindowWidth,
            height: MainApplication.WindowHeight,
            show: false,
            backgroundColor: MainApplication.BackgroundColor,
            webPreferences: { preload: join(__dirname, MainApplication.PreloadFile), sandbox: false },
        })
        window.on(MainApplication.ReadyToShowEvent, () => window.show())
        const rendererUrl = process.env[MainApplication.RendererUrlEnv]
        if (is.dev && rendererUrl)
        {
            void window.loadURL(rendererUrl)
        }
        else
        {
            void window.loadFile(join(__dirname, MainApplication.RendererFile))
        }
    }

    private async OnBeforeQuit(event: { preventDefault(): void }): Promise<void>
    {
        if (this.quitting)
        {
            return
        }
        this.quitting = true
        event.preventDefault()
        if (this.startPromise)
        {
            await this.sidecar.Stop()
        }
        app.quit()
    }
}

new MainApplication().Run()
