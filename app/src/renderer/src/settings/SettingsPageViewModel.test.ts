import { describe, expect, it } from 'vitest'
import type { ApplicationSettings, Setting } from '@pragmatic-tech-ai/mural/framework'
import { SettingsPageViewModel, StringSettingRow } from './SettingsPageViewModel'

class FakeSetting
{
    public Value: unknown
    public readonly Kind = 'string'
    public readonly Definition: { Category: string }

    constructor(public readonly Label: string, category: string, value: unknown)
    {
        this.Value = value
        this.Definition = { Category: category }
    }
}

class FakeApplicationSettings
{
    constructor(public readonly Settings: FakeSetting[])
    {
    }
}

describe('SettingsPageViewModel', () =>
{
    it('groups_settings_by_category_with_blank_as_general', () =>
    {
        const settings = new FakeApplicationSettings([
            new FakeSetting('Url', 'Runtime', 'a'),
            new FakeSetting('Other', '', 'b'),
        ])
        const page = new SettingsPageViewModel(settings as unknown as ApplicationSettings)
        const categories = Array.from(page.Categories)
        expect(categories.map(c => c.Name)).toEqual(['Runtime', 'General'])
        expect(categories[0].Rows.length).toBe(1)
        expect(page.Id).toBe('settings')
        expect(page.IsDirty).toBe(false)
    })

    it('string_row_reads_and_writes_through_to_setting', () =>
    {
        const setting = new FakeSetting('Url', 'Runtime', 'http://x')
        const row = new StringSettingRow(setting as unknown as Setting)
        expect(row.Label).toBe('Url')
        expect(row.Value).toBe('http://x')
        row.Value = 'http://y'
        expect(setting.Value).toBe('http://y')
    })
})
