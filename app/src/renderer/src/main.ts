import './fonts.css'
import { app } from './app.mu.js'
import { RuntimeClient } from './RuntimeClient'
import { SupervisorUrlSeed } from './SupervisorUrlSeed'
import { SupervisorUrlSettingKey } from './SettingsKeys'
import { RuntimeManagementService } from './RuntimeManagementService'
import { HtmlTarget } from '@pragmatic-tech-ai/mural/visual-engine'
import { ApplicationSettings } from '@pragmatic-tech-ai/mural/framework'
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
        // The runtime service owns the supervisor connection, driven by the persisted
        // supervisor.url setting; constructing it applies the saved value and subscribes.
        const settings = app.Services.getRequired(ApplicationSettings.Key)
        const runtime = app.Services.getRequired(RuntimeManagementService.Key)
        // First run: seed the setting from the env default (the Set connects live).
        SupervisorUrlSeed.Seed(settings, await window.harness.SupervisorUrl())
        if (SupervisorUrlSeed.IsEmpty(settings.Get(SupervisorUrlSettingKey)))
        {
            const url = await window.harness.BackendUrl()
            if (url !== null)
            {
                const client = new RuntimeClient()
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
