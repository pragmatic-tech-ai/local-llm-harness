import { JsonRpcConnection } from './JsonRpcConnection'

export interface HarnessInfo
{
    id: string
    runtime: string
    model: string
    state: string
    url?: string
    chat_url?: string
}

/** The supervisor's `hello`: the agent runtime it hosts (its url while bound, model loaded or not) or why it is off. */
export interface SupervisorHello
{
    runtime_url: string | null
    runtime_error: string | null
}

/**
 * JSON-RPC client for the supervisor control server, used only to discover the agent runtime endpoint: `hello`
 * advertises it as `runtime_url` (an older supervisor only as `chat_url` on a Ready harness).
 */
export class SupervisorClient extends JsonRpcConnection
{
    private static readonly MethodHello = 'hello'
    private static readonly MethodListHarnesses = 'list_harnesses'
    /** The runtime's port when the supervisor advertises none. */
    private static readonly DefaultRuntimePort = '8779'

    public Hello(): Promise<SupervisorHello>
    {
        return this.Request<SupervisorHello>(SupervisorClient.MethodHello, {})
    }

    public ListHarnesses(): Promise<HarnessInfo[]>
    {
        return this.Request<HarnessInfo[]>(SupervisorClient.MethodListHarnesses, {})
    }

    /**
     * The runtime url: the supervisor url (scheme and host kept, so a tunnel or LAN name still works) with its port
     * replaced by the port of `hello`'s `runtime_url`, else of the first advertised `chat_url`, else by the default
     * runtime port.
     */
    public static RuntimeUrl(supervisorUrl: string, hello: SupervisorHello | null, harnesses: readonly HarnessInfo[]): string
    {
        const supervisor = new URL(supervisorUrl)
        const advertised = [hello?.runtime_url, ...harnesses.map(h => h.chat_url)]
        let port = SupervisorClient.DefaultRuntimePort
        for (const url of advertised)
        {
            const candidate = SupervisorClient.PortOf(url)
            if (candidate !== null)
            {
                port = candidate
                break
            }
        }
        return `${supervisor.protocol}//${supervisor.hostname}:${port}`
    }

    protected Dispatch(): void
    {
        // The supervisor's harness notifications served the removed picker; nothing listens now.
    }

    /** The explicit port of an advertised url, or null (none advertised, or no port in it). */
    private static PortOf(url: string | null | undefined): string | null
    {
        if (typeof url !== 'string' || url.length === 0)
        {
            return null
        }
        const port = new URL(url).port
        return port.length > 0 ? port : null
    }
}
