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

/** Typed signal: handlers subscribe, the owner raises. Used for every app-level event, not only turn notes. */
export class TypedSignal<T>
{
    private static readonly HandlerFailedMessage = 'Signal handler threw'
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
                console.error(TypedSignal.HandlerFailedMessage, e)
            }
        }
    }
}

/**
 * Shared JSON-RPC-over-WebSocket plumbing. Frame parsing (HandleFrame) is independent
 * of the socket so it is unit-testable. Requests issued before the socket opens
 * are queued and flushed on open. Subclasses route notifications in Dispatch.
 */
export abstract class JsonRpcConnection
{
    private static readonly NotConnectedMessage = 'Runtime not connected'
    private static readonly SocketErrorMessage = 'Runtime socket error'
    private static readonly RpcErrorFallback = 'RPC error'
    private static readonly SocketConnecting = 0
    private static readonly SocketOpen = 1

    private socket: IHarnessSocket | null = null
    private nextId = 1
    private readonly pending = new Map<number, IPending>()
    private readonly queue: string[] = []

    /** Raised when the socket opens (after the queued requests are flushed). */
    public readonly OnOpened = new TypedSignal<void>()
    /** Raised when the live socket closes on its own (never for a deliberate Disconnect or a replaced socket). */
    public readonly OnClosed = new TypedSignal<void>()

    constructor(private readonly socketFactory: HarnessSocketFactory = (url) => new WebSocket(url) as unknown as IHarnessSocket)
    {
    }

    protected abstract Dispatch(method: string, params: unknown): void

    public get IsConnected(): boolean
    {
        return this.socket !== null && this.socket.readyState === JsonRpcConnection.SocketOpen
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
            this.OnOpened.Raise(undefined)
        }
        socket.onmessage = (ev) => this.HandleFrame(String(ev.data))
        socket.onerror = () => console.error(JsonRpcConnection.SocketErrorMessage)
        socket.onclose = () =>
        {
            this.RejectAllPending()
            if (this.socket === socket)
            {
                this.socket = null
                this.OnClosed.Raise(undefined)
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
                entry.reject(new Error(frame.error.message ?? JsonRpcConnection.RpcErrorFallback))
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

    protected Request<T>(method: string, params: unknown): Promise<T>
    {
        const socket = this.socket
        // Queue only while the socket is actually connecting; otherwise it will
        // never flush, so fail fast instead of hanging forever.
        if (socket === null || (socket.readyState !== JsonRpcConnection.SocketConnecting && socket.readyState !== JsonRpcConnection.SocketOpen))
        {
            return Promise.reject(new Error(JsonRpcConnection.NotConnectedMessage))
        }
        const id = this.nextId++
        const frame = JSON.stringify({ id, method, params })
        return new Promise<T>((resolve, reject) =>
        {
            this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject })
            if (socket.readyState === JsonRpcConnection.SocketOpen)
            {
                socket.send(frame)
            }
            else
            {
                this.queue.push(frame)
            }
        })
    }

    protected RejectAllPending(): void
    {
        for (const entry of this.pending.values())
        {
            entry.reject(new Error(JsonRpcConnection.NotConnectedMessage))
        }
        this.pending.clear()
        this.queue.length = 0
    }
}
