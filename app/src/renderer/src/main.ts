import './fonts.css'
import { app } from './app.mu.js'
import { SupervisorUrlSeed } from './SupervisorUrlSeed'
import { SessionsService } from './SessionsService'
import { HtmlTarget } from '@pragmatic-tech-ai/mural/visual-engine'
import { ApplicationSettings } from '@pragmatic-tech-ai/mural/framework'
import { ThemeManager, Density } from '@pragmatic-tech-ai/mural/runtime'

/** Renderer bootstrap: seeds the supervisor url setting (the Sessions capability connects from it) and mounts the mural shell. */
class RendererBootstrap
{
    private static readonly IconFont = '24px "Material Symbols Outlined"'
    private static readonly AppElementId = 'app'
    private static readonly SplashElementId = 'splash'

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
        // Construct the Sessions service up front: it shows the session view in the host area and
        // follows the persisted supervisor.url setting. First run: seed that setting from the env
        // default (the Set connects live).
        const settings = app.Services.getRequired(ApplicationSettings.Key)
        app.Services.getRequired(SessionsService.Key)
        SupervisorUrlSeed.Seed(settings, await window.harness.SupervisorUrl())
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
