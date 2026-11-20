import './fonts.css'
import { app } from './app.mu.js'
import { HarnessClient } from './HarnessClient'
import { ChatViewModel } from './ChatViewModel'
import { SupervisorClient } from './SupervisorClient'
import { HtmlTarget } from '@pragmatic-tech-ai/mural/visual-engine'
import { ThemeManager, Density, ServiceLifetime } from '@pragmatic-tech-ai/mural/runtime'
import { ContentHostService } from '@pragmatic-tech-ai/mural/framework'

/** Renderer bootstrap: mounts the mural shell and hands the ContentHost its first view. */
class RendererBootstrap
{
    private static readonly IconFont = '24px "Material Symbols Outlined"'
    private static readonly AppElementId = 'app'
    private static readonly SplashElementId = 'splash'
    private static readonly NoBackendWarning = 'No backend url; chat cannot send.'

    public static async Run(): Promise<void>
    {
        ThemeManager.Density = Density.Compact
        try
        {
            // Preload the icon font so first layout uses real glyph metrics.
            // A layout optimization only — a load failure (e.g. the font not
            // served in dev) must never abort the bootstrap.
            await document.fonts.load(RendererBootstrap.IconFont)
        }
        catch
        {
            // non-fatal
        }
        const host = document.getElementById(RendererBootstrap.AppElementId) as HTMLElement
        const supervisorUrl = await window.harness.SupervisorUrl()
        if (supervisorUrl !== null)
        {
            const supervisor = new SupervisorClient()
            supervisor.Connect(supervisorUrl)
            app.Services.register(SupervisorClient.Key, () => supervisor, ServiceLifetime.Singleton)
        }
        app.initialize(new HtmlTarget(host))
        const contentHost = app.Services.getRequired(ContentHostService.Key)
        if (supervisorUrl === null)
        {
            const url = await window.harness.BackendUrl()
            const client = new HarnessClient()
            if (url !== null)
            {
                client.Connect(url)
            }
            else
            {
                console.warn(RendererBootstrap.NoBackendWarning)
            }
            contentHost.View(new ChatViewModel(client))
        }
        RendererBootstrap.RemoveSplash()
    }

    private static RemoveSplash(): void
    {
        requestAnimationFrame(() => requestAnimationFrame(() =>
        {
            document.getElementById(RendererBootstrap.SplashElementId)?.remove()
        }))
    }
}

void RendererBootstrap.Run()
