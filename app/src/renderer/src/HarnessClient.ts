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

/** The subset of WebSocket the client uses; lets tests substitute a fake. */
export interface IHarnessSocket
{
    readyState: number
    send(data: string): void
    close(): void
    onopen: ((ev: unknown) => void) | null
    onmessage: ((ev: { data: unknown }) => void) | null
    onclose: ((ev: unknown) => void) | null
    onerror: ((ev: unknown) => void) | null
}

export type HarnessSocketFactory = (url: string) => IHarnessSocket

interface IPending
{
    resolve: (value: unknown) => void
    reject: (reason: Error) => void
}

export interface IDisposable
{
    Dispose(): void
}

/** Typed signal: handlers subscribe, the client raises (mirrors SidecarExitSignal). */
export class TurnSignal<T>
{
    private static readonly HandlerFailedMessage = 'Turn signal handler threw'
    private readonly handlers: { handler: (payload: T) => void }[] = []

    public Subscribe(handler: (payload: T) => void): IDisposable
    {
        const entry = { handler }
        this.handlers.push(entry)
        return {
            Dispose: () =>
            {
                const index = this.handlers.indexOf(entry)
                if (index >= 0)
                {
                    this.handlers.splice(index, 1)
                }
            }
        }
    }

    public Raise(payload: T): void
    {
        for (const entry of [...this.handlers])
        {
            try
            {
                entry.handler(payload)
            }
            catch (e)
            {
                console.error(TurnSignal.HandlerFailedMessage, e)
            }
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
    private static readonly NotConnectedMessage = 'Harness not connected'
    private static readonly SocketErrorMessage = 'Harness socket error'
    private static readonly RpcErrorFallback = 'RPC error'

    public readonly OnTurnStarted = new TurnSignal<TurnStarted>()
    public readonly OnTurnChunk = new TurnSignal<TurnChunk>()
    public readonly OnTurnDone = new TurnSignal<Record<string, never>>()
    public readonly OnTurnError = new TurnSignal<TurnError>()
    public readonly OnTurnRejected = new TurnSignal<TurnRejected>()

    private static readonly SocketConnecting = 0
    private static readonly SocketOpen = 1

    private socket: IHarnessSocket | null = null
    private nextId = 1
    private readonly pending = new Map<number, IPending>()
    private readonly queue: string[] = []

    constructor(private readonly socketFactory: HarnessSocketFactory = (url) => new WebSocket(url) as unknown as IHarnessSocket)
    {
    }

    public get IsConnected(): boolean
    {
        return this.socket !== null && this.socket.readyState === HarnessClient.SocketOpen
    }

    public Connect(url: string): void
    {
        this.Disconnect()
        const socket = this.socketFactory(url)
        this.socket = socket
        socket.onopen = () =>
        {
            for (const frame of this.queue.splice(0))
            {
                socket.send(frame)
            }
        }
        socket.onmessage = (ev) => this.HandleFrame(String(ev.data))
        socket.onerror = () => console.error(HarnessClient.SocketErrorMessage)
        socket.onclose = () =>
        {
            this.RejectAllPending()
            if (this.socket === socket)
            {
                this.socket = null
            }
        }
    }

    /** Closes the current socket, detaches its handlers so no late frames dispatch, and rejects pending requests. */
    public Disconnect(): void
    {
        const socket = this.socket
        this.socket = null
        if (socket !== null)
        {
            socket.onopen = null
            socket.onmessage = null
            socket.onclose = null
            socket.onerror = null
            socket.close()
        }
        this.RejectAllPending()
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

    /** Parses one text frame: responses (have id) settle pending requests; notifications raise events. Malformed frames are ignored. */
    public HandleFrame(raw: string): void
    {
        let parsed: unknown
        try
        {
            parsed = JSON.parse(raw)
        }
        catch
        {
            return
        }
        if (typeof parsed !== 'object' || parsed === null)
        {
            return
        }
        const frame = parsed as {
            id?: unknown
            result?: unknown
            error?: { code?: number, message?: string } | null
            method?: unknown
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
            if (frame.error !== undefined && frame.error !== null)
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
        const socket = this.socket
        // Queue only while the socket is actually connecting; otherwise it will
        // never flush, so fail fast instead of hanging forever.
        if (socket === null || (socket.readyState !== HarnessClient.SocketConnecting && socket.readyState !== HarnessClient.SocketOpen))
        {
            return Promise.reject(new Error(HarnessClient.NotConnectedMessage))
        }
        const id = this.nextId++
        const frame = JSON.stringify({ id, method, params })
        return new Promise<T>((resolve, reject) =>
        {
            this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject })
            if (socket.readyState === HarnessClient.SocketOpen)
            {
                socket.send(frame)
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
