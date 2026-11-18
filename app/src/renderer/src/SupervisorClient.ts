import { JsonRpcConnection, TurnSignal } from './JsonRpcConnection'

export interface RuntimeInfo
{
    name: string
    kind: string
    available: boolean
}

export interface ModelInfo
{
    name: string
    runtimes: string[]
    size_hint: string
}

export interface HarnessInfo
{
    id: string
    runtime: string
    model: string
    state: string
    url?: string
}

/** JSON-RPC client for the supervisor control server: list/start/stop harnesses. */
export class SupervisorClient extends JsonRpcConnection
{
    private static readonly MethodListRuntimes = 'list_runtimes'
    private static readonly MethodListModels = 'list_models'
    private static readonly MethodListHarnesses = 'list_harnesses'
    private static readonly MethodStart = 'start'
    private static readonly MethodStop = 'stop'
    private static readonly NoteHarnessChanged = 'harness_changed'

    public readonly OnHarnessChanged = new TurnSignal<HarnessInfo>()

    public ListRuntimes(): Promise<RuntimeInfo[]>
    {
        return this.Request<RuntimeInfo[]>(SupervisorClient.MethodListRuntimes, {})
    }

    public ListModels(): Promise<ModelInfo[]>
    {
        return this.Request<ModelInfo[]>(SupervisorClient.MethodListModels, {})
    }

    public ListHarnesses(): Promise<HarnessInfo[]>
    {
        return this.Request<HarnessInfo[]>(SupervisorClient.MethodListHarnesses, {})
    }

    public Start(runtime: string, model: string): Promise<HarnessInfo>
    {
        return this.Request<HarnessInfo>(SupervisorClient.MethodStart, { runtime, model })
    }

    public async Stop(id: string): Promise<void>
    {
        await this.Request<unknown>(SupervisorClient.MethodStop, { id })
    }

    protected Dispatch(method: string, params: unknown): void
    {
        if (method === SupervisorClient.NoteHarnessChanged)
        {
            this.OnHarnessChanged.Raise(params as HarnessInfo)
        }
    }
}
