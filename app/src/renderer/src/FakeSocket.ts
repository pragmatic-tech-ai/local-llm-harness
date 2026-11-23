import type { IHarnessSocket } from './RuntimeClient'

/** Test double for the WebSocket: records sent frames and lets tests drive its lifecycle. */
export class FakeSocket implements IHarnessSocket
{
    public readyState = 0
    public readonly sent: string[] = []
    public closeCalls = 0
    public onopen: ((ev: unknown) => void) | null = null
    public onmessage: ((ev: { data: unknown }) => void) | null = null
    public onclose: ((ev: unknown) => void) | null = null
    public onerror: ((ev: unknown) => void) | null = null

    public send(data: string): void
    {
        this.sent.push(data)
    }

    public close(): void
    {
        this.closeCalls++
        this.readyState = 3
    }

    public Open(): void
    {
        this.readyState = 1
        this.onopen?.({})
    }

    public Close(): void
    {
        this.readyState = 3
        this.onclose?.({})
    }
}
