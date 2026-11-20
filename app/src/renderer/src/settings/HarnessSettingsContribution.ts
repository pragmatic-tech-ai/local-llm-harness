import { Application, type IServiceProvider } from '@pragmatic-tech-ai/mural/runtime'
import { ApplicationSettings, type IDocument, type ISettingsContribution } from '@pragmatic-tech-ai/mural/framework'
import type { Geometry } from '@pragmatic-tech-ai/mural/visual-engine'
import { SettingsPageViewModel } from './SettingsPageViewModel'

/** The app half of mural's settings seam: the rail gear glyph and the settings page view. */
export class HarnessSettingsContribution implements ISettingsContribution
{
    private static readonly IconResourceKey = 'SettingsIcon'
    private static readonly TooltipText = 'Settings'

    public readonly Icon: Geometry | undefined
    public readonly Tooltip = HarnessSettingsContribution.TooltipText

    private readonly provider: IServiceProvider

    constructor(provider: IServiceProvider)
    {
        this.provider = provider
        this.Icon = Application.current?.Resources.Resolve(HarnessSettingsContribution.IconResourceKey) as Geometry | undefined
    }

    public CreateView(): IDocument
    {
        return new SettingsPageViewModel(this.provider.getRequired(ApplicationSettings.Key))
    }
}
