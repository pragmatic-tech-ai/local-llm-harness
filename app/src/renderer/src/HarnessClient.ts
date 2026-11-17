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

interface IPending
{
    resolve: (value: unknown) => void
    reject: (reason: Error) => void
}

/** Typed signal: handlers subscribe, the client raises (mirrors SidecarExitSignal). */
export class TurnSignal<T>
{
    private readonly handlers: ((payload: T) => void)[] = []

    public Subscribe(handler: (payload: T) => void): void
    {
        this.handlers.push(handler)
    }

    public Raise(payload: T): void
    {
        for (const handler of this.handlers)
        {
            handler(payload)
        }
    }
}

/**
 * JSON-RPC client over one WebSocket. Frame parsing (HandleFrame) is independent
 * of the socket so it is unit-testable. Requests issued before the socket opens
 * are queued and flushed on open.
 */
export class HarnessClient
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
    private static readonly NotConnectedMessage = 'Harness connection closed'
    private static readonly RpcErrorFallback = 'RPC error'

    public readonly OnTurnStarted = new TurnSignal<TurnStarted>()
    public readonly OnTurnChunk = new TurnSignal<TurnChunk>()
    public readonly OnTurnDone = new TurnSignal<Record<string, never>>()
    public readonly OnTurnError = new TurnSignal<TurnError>()
    public readonly OnTurnRejected = new TurnSignal<TurnRejected>()

    private socket: WebSocket | null = null
    private connected = false
    private nextId = 1
    private readonly pending = new Map<number, IPending>()
    private readonly queue: string[] = []

    public get IsConnected(): boolean
    {
        return this.connected
    }

    public Connect(url: string): void
    {
        const socket = new WebSocket(url)
        this.socket = socket
        socket.onopen = () =>
        {
            this.connected = true
            for (const frame of this.queue.splice(0))
            {
                socket.send(frame)
            }
        }
        socket.onmessage = (ev: MessageEvent) => this.HandleFrame(String(ev.data))
        socket.onclose = () =>
        {
            this.connected = false
            this.RejectAllPending()
        }
        socket.onerror = () =>
        {
            this.connected = false
        }
    }

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

    /** Parses one text frame: responses (have id) settle pending requests; notifications raise events. */
    public HandleFrame(raw: string): void
    {
        const frame = JSON.parse(raw) as {
            id?: number
            result?: unknown
            error?: { code?: number, message?: string }
            method?: string
            params?: unknown
        }
        if (typeof frame.id === 'number')
        {
            const entry = this.pending.get(frame.id)
            if (entry === undefined)
            {
                return
            }
            this.pending.delete(frame.id)
            if (frame.error !== undefined)
            {
                entry.reject(new Error(frame.error.message ?? HarnessClient.RpcErrorFallback))
            }
            else
            {
                entry.resolve(frame.result)
            }
            return
        }
        if (typeof frame.method === 'string')
        {
            this.Dispatch(frame.method, frame.params)
        }
    }

    private Dispatch(method: string, params: unknown): void
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

    private Request<T>(method: string, params: unknown): Promise<T>
    {
        const id = this.nextId++
        const frame = JSON.stringify({ id, method, params })
        return new Promise<T>((resolve, reject) =>
        {
            this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject })
            if (this.connected && this.socket !== null)
            {
                this.socket.send(frame)
            }
            else
            {
                this.queue.push(frame)
            }
        })
    }

    private RejectAllPending(): void
    {
        for (const entry of this.pending.values())
        {
            entry.reject(new Error(HarnessClient.NotConnectedMessage))
        }
        this.pending.clear()
        this.queue.length = 0
    }
}
