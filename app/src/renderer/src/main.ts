import './fonts.css'
import { app } from './app.mu.js'
import { HarnessClient } from './HarnessClient'
import { SupervisorClient } from './SupervisorClient'
import { RuntimeManagementService } from './RuntimeManagementService'
import { HtmlTarget } from '@pragmatic-tech-ai/mural/visual-engine'
import { ThemeManager, Density } from '@pragmatic-tech-ai/mural/runtime'

/** Renderer bootstrap: mounts the mural shell and wires the runtime-management capability to its data source. */
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
        // Resolve and wire the capability before mounting so discovery is already
        // under way when its side pane first renders. The pane is a view-reusing
        // ContentControl, so the service exposes a stable Picker (set in its ctor);
        // Attach only feeds it the connection, never swaps the instance.
        const runtime = app.Services.getRequired(RuntimeManagementService.Key)
        if (supervisorUrl !== null)
        {
            const supervisor = new SupervisorClient()
            supervisor.Connect(supervisorUrl)
            runtime.Attach(supervisor)
        }
        else
        {
            const url = await window.harness.BackendUrl()
            if (url !== null)
            {
                const client = new HarnessClient()
                client.Connect(url)
                runtime.BindBackend(client)
            }
            else
            {
                console.warn(RendererBootstrap.NoBackendWarning)
            }
        }
        app.initialize(new HtmlTarget(host))
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
