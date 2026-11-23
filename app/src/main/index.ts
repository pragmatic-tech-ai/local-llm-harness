import { app, BrowserWindow, ipcMain } from 'electron'
import { join } from 'node:path'
import { electronApp, is, optimizer } from '@electron-toolkit/utils'
import { SupervisorUrl } from './supervisor-url'
import { SettingsFile } from './settings'

/** Owns the BrowserWindow for the app's lifetime; the renderer reaches the agent runtime through the supervisor. */
export class MainApplication
{
    private static readonly AppUserModelId = 'com.local-llm-harness.app'
    private static readonly BackgroundColor = '#0A0A0B'
    private static readonly SupervisorUrlChannel = 'supervisor-url'
    private static readonly BrowserWindowCreatedEvent = 'browser-window-created'
    private static readonly ActivateEvent = 'activate'
    private static readonly WindowAllClosedEvent = 'window-all-closed'
    private static readonly ReadyToShowEvent = 'ready-to-show'
    private static readonly DarwinPlatform = 'darwin'
    private static readonly PreloadFile = '../preload/index.js'
    private static readonly RendererFile = '../renderer/index.html'
    private static readonly RendererUrlEnv = 'ELECTRON_RENDERER_URL'
    private static readonly WindowWidth = 1280
    private static readonly WindowHeight = 800

    private readonly supervisorUrl: string | null = SupervisorUrl.Resolve(process.env)

    public Run(): void
    {
        SettingsFile.Register(ipcMain, app.getPath('userData'))
        ipcMain.handle(MainApplication.SupervisorUrlChannel, () => this.supervisorUrl)
        app.on(MainApplication.WindowAllClosedEvent, () =>
        {
            if (process.platform !== MainApplication.DarwinPlatform)
            {
                app.quit()
            }
        })
        void app.whenReady().then(() => this.OnReady())
    }

    private OnReady(): void
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
        this.CreateWindow()
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
}

new MainApplication().Run()
