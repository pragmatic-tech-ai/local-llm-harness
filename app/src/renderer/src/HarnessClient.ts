import { JsonRpcConnection, TurnSignal } from './JsonRpcConnection'

export { TurnSignal }
export type { IHarnessSocket, HarnessSocketFactory, IDisposable } from './JsonRpcConnection'

export interface SamplingConfig
{
    temperature: number
    top_p: number
    top_k: number
    repeat_penalty: number
    seed: number
    max_tokens: number
}

export interface ModelProps
{
    n_ctx: number
    model_name: string
}

export interface TokenizeResult
{
    tokens: number[]
    count: number
}

export interface TokenInfo
{
    token: string
    logprob: number
    top: [string, number][]
}

export interface TurnStarted
{
    prompt: string
    prompt_tokens: number
    n_ctx: number
}

export interface TurnChunk
{
    text: string
    tokens: TokenInfo[]
    stop: boolean
}

export interface TurnError
{
    message: string
}

export interface TurnRejected
{
    needed: number
    total: number
}

/** JSON-RPC chat client for the harness: turn streaming plus model/sampling calls. */
export class HarnessClient extends JsonRpcConnection
{
    private static readonly MethodModelProps = 'model_props'
    private static readonly MethodTokenize = 'tokenize'
    private static readonly MethodSetSampling = 'set_sampling'
    private static readonly MethodSendTurn = 'send_turn'
    private static readonly NoteTurnStarted = 'turn_started'
    private static readonly NoteTurnChunk = 'turn_chunk'
    private static readonly NoteTurnDone = 'turn_done'
    private static readonly NoteTurnError = 'turn_error'
    private static readonly NoteTurnRejected = 'turn_rejected'

    public readonly OnTurnStarted = new TurnSignal<TurnStarted>()
    public readonly OnTurnChunk = new TurnSignal<TurnChunk>()
    public readonly OnTurnDone = new TurnSignal<Record<string, never>>()
    public readonly OnTurnError = new TurnSignal<TurnError>()
    public readonly OnTurnRejected = new TurnSignal<TurnRejected>()

    public ModelProps(): Promise<ModelProps>
    {
        return this.Request<ModelProps>(HarnessClient.MethodModelProps, {})
    }

    public Tokenize(text: string): Promise<TokenizeResult>
    {
        return this.Request<TokenizeResult>(HarnessClient.MethodTokenize, { text })
    }

    public async SetSampling(config: SamplingConfig): Promise<void>
    {
        await this.Request<null>(HarnessClient.MethodSetSampling, { config })
    }

    /** Resolves when the send_turn response arrives, i.e. after the stream's terminal notification. */
    public async SendTurn(content: string, enableThinking: boolean): Promise<void>
    {
        await this.Request<null>(HarnessClient.MethodSendTurn, { content, enable_thinking: enableThinking })
    }

    protected Dispatch(method: string, params: unknown): void
    {
        switch (method)
        {
            case HarnessClient.NoteTurnStarted:
                this.OnTurnStarted.Raise(params as TurnStarted)
                break
            case HarnessClient.NoteTurnChunk:
                this.OnTurnChunk.Raise(params as TurnChunk)
                break
            case HarnessClient.NoteTurnDone:
                this.OnTurnDone.Raise({})
                break
            case HarnessClient.NoteTurnError:
                this.OnTurnError.Raise(params as TurnError)
                break
            case HarnessClient.NoteTurnRejected:
                this.OnTurnRejected.Raise(params as TurnRejected)
                break
        }
    }
}
