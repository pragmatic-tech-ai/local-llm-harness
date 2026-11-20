import { Observable } from '@pragmatic-tech-ai/mural/runtime'
import type { ApplicationSettings, IDocument, Setting } from '@pragmatic-tech-ai/mural/framework'

/** One editable String-kind row wrapping a live Setting; Value proxies Setting.Value. */
export class StringSettingRow extends Observable
{
    private static readonly PropValue = 'Value'

    constructor(public readonly Setting: Setting)
    {
        super()
    }

    public get Label(): string
    {
        return this.Setting.Label
    }

    public get Value(): string
    {
        return this.Setting.Value as string
    }

    public set Value(v: string)
    {
        const old = this.Setting.Value as string
        if (old === v)
        {
            return
        }
        this.Setting.Value = v
        this.RaisePropertyChanged(StringSettingRow.PropValue, old, v)
    }
}

/** A category section: a name over its rows. */
export class SettingsCategory extends Observable
{
    public readonly Rows: StringSettingRow[] = []

    constructor(public readonly Name: string)
    {
        super()
    }
}

/** The settings page document: ApplicationSettings grouped by Definition.Category (blank = General). */
export class SettingsPageViewModel extends Observable implements IDocument
{
    private static readonly PageId = 'settings'
    private static readonly PageTitle = 'Settings'
    private static readonly GeneralCategory = 'General'

    public readonly Id = SettingsPageViewModel.PageId
    public readonly Title = SettingsPageViewModel.PageTitle
    public readonly IsDirty = false
    public readonly Categories: SettingsCategory[] = []

    constructor(settings: ApplicationSettings)
    {
        super()
        const byName = new Map<string, SettingsCategory>()
        for (const setting of settings.Settings)
        {
            const name = setting.Definition.Category || SettingsPageViewModel.GeneralCategory
            let category = byName.get(name)
            if (category === undefined)
            {
                category = new SettingsCategory(name)
                byName.set(name, category)
                this.Categories.push(category)
            }
            // Only String is shipped; every kind falls back to the string row.
            category.Rows.push(new StringSettingRow(setting))
        }
    }

    public Save(): void
    {
        // Settings persist live through ApplicationSettings store.
    }
}
