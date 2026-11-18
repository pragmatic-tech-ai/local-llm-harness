/** Pure, Electron-free resolution of an optional remote supervisor url (kept apart from index.ts, which boots Electron on import). */
export class SupervisorUrl
{
    private static readonly SupervisorUrlEnv = 'HARNESS_SUPERVISOR_URL'

    /** The remote supervisor url from the environment, or null when unset/empty/whitespace. */
    public static Resolve(env: NodeJS.ProcessEnv): string | null
    {
        const url = env[SupervisorUrl.SupervisorUrlEnv]?.trim()
        return url ? url : null
    }
}
