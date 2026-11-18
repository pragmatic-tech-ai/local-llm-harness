/** Pure, Electron-free resolution of an optional remote backend url (kept apart from index.ts, which boots Electron on import). */
export class BackendUrl
{
    private static readonly BackendUrlEnv = 'HARNESS_BACKEND_URL'

    /** The remote backend url from the environment, or null when unset/empty (spawn the local sidecar). */
    public static Resolve(env: NodeJS.ProcessEnv): string | null
    {
        const url = env[BackendUrl.BackendUrlEnv]
        return url ? url : null
    }
}
