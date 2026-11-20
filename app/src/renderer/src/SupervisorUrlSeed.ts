import type { ApplicationSettings } from '@pragmatic-tech-ai/mural/framework'
import { SupervisorUrlSettingKey } from './SettingsKeys'

/** First-run seeding of the persisted supervisor url from the environment default. */
export class SupervisorUrlSeed
{
    /** True when the setting value is unset or blank. */
    public static IsEmpty(value: unknown): boolean
    {
        return typeof value !== 'string' || value.trim().length === 0
    }

    /** Set the saved url from the env default only when nothing is saved yet; returns whether it seeded. */
    public static Seed(settings: ApplicationSettings, envUrl: string | null): boolean
    {
        if (SupervisorUrlSeed.IsEmpty(envUrl) || !SupervisorUrlSeed.IsEmpty(settings.Get(SupervisorUrlSettingKey)))
        {
            return false
        }
        settings.Set(SupervisorUrlSettingKey, (envUrl as string).trim())
        return true
    }
}
