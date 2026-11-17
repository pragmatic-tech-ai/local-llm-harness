import './fonts.css'
import { app } from './app.mu.js'
import { PlaceholderViewModel } from './PlaceholderViewModel'
import { HtmlTarget } from '@pragmatic-tech-ai/mural/visual-engine'
import { ThemeManager, Density } from '@pragmatic-tech-ai/mural/runtime'
import { ContentHostService } from '@pragmatic-tech-ai/mural/framework'

/** Renderer bootstrap: mounts the mural shell and hands the ContentHost its first view. */
class RendererBootstrap
{
    private static readonly IconFont = '24px "Material Symbols Outlined"'
    private static readonly AppElementId = 'app'
    private static readonly SplashElementId = 'splash'

    public static async Run(): Promise<void>
    {
        ThemeManager.Density = Density.Compact
        await document.fonts.load(RendererBootstrap.IconFont)
        const host = document.getElementById(RendererBootstrap.AppElementId) as HTMLElement
        app.initialize(new HtmlTarget(host))
        app.Services.getRequired(ContentHostService.Key).View(new PlaceholderViewModel())
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
