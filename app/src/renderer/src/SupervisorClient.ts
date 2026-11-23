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

/**
 * JSON-RPC client for the supervisor control server, used only to discover the agent runtime endpoint:
 * the supervisor advertises the runtime as `chat_url` on its harnesses.
 */
export class SupervisorClient extends JsonRpcConnection
{
    private static readonly MethodListHarnesses = 'list_harnesses'
    /** The runtime's port when no harness advertises a `chat_url`. */
    private static readonly DefaultRuntimePort = '8779'

    public ListHarnesses(): Promise<HarnessInfo[]>
    {
        return this.Request<HarnessInfo[]>(SupervisorClient.MethodListHarnesses, {})
    }

    /**
     * The runtime url: the supervisor url (scheme and host kept, so a tunnel or LAN name still works) with its port
     * replaced by the port of the first advertised `chat_url`, else by the default runtime port.
     */
    public static RuntimeUrl(supervisorUrl: string, harnesses: readonly HarnessInfo[]): string
    {
        const supervisor = new URL(supervisorUrl)
        let port = SupervisorClient.DefaultRuntimePort
        for (const harness of harnesses)
        {
            if (harness.chat_url)
            {
                const advertised = new URL(harness.chat_url).port
                if (advertised.length > 0)
                {
                    port = advertised
                    break
                }
            }
        }
        return `${supervisor.protocol}//${supervisor.hostname}:${port}`
    }

    protected Dispatch(): void
    {
        // The supervisor's harness notifications served the removed picker; nothing listens now.
    }
}
