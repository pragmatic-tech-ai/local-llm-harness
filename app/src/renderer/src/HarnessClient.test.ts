import { describe, expect, it, vi } from 'vitest'
import { HarnessClient, TurnSignal, type ModelProps, type TurnChunk } from './HarnessClient'
import { FakeSocket } from './FakeSocket'

describe('HarnessClient', () =>
{
    it('MapsTurnChunkNotificationsToText', () =>
    {
        const client = new HarnessClient()
        const chunks: TurnChunk[] = []
        const order: string[] = []
        client.OnTurnChunk.Subscribe(p =>
        {
            chunks.push(p)
            order.push('chunk')
        })
        client.OnTurnDone.Subscribe(() => order.push('done'))

        client.HandleFrame(JSON.stringify({ method: 'turn_started', params: { prompt: 'ping', prompt_tokens: 1, n_ctx: 4096 } }))
        client.HandleFrame(JSON.stringify({ method: 'turn_chunk', params: { text: 'po', tokens: [], stop: false } }))
        client.HandleFrame(JSON.stringify({ method: 'turn_chunk', params: { text: 'ng', tokens: [], stop: true } }))
        client.HandleFrame(JSON.stringify({ method: 'turn_done', params: {} }))

        expect(chunks.map(c => c.text)).toEqual(['po', 'ng'])
        expect(order).toEqual(['chunk', 'chunk', 'done'])
    })

    it('ResolvesModelPropsById', async () =>
    {
        const socket = new FakeSocket()
        const client = new HarnessClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const pending = client.ModelProps()
        const expected: ModelProps = { n_ctx: 4096, model_name: 'm' }
        client.HandleFrame(JSON.stringify({ id: 1, result: expected }))
        await expect(pending).resolves.toEqual(expected)
    })

    it('RejectsOnErrorResponse', async () =>
    {
        const socket = new FakeSocket()
        const client = new HarnessClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const pending = client.Tokenize('x')
        client.HandleFrame(JSON.stringify({ id: 1, error: { code: -1, message: 'boom' } }))
        await expect(pending).rejects.toThrow('boom')
    })

    it('RaisesTurnErrorAndRejected', () =>
    {
        const client = new HarnessClient()
        const errors: string[] = []
        const rejected: number[] = []
        client.OnTurnError.Subscribe(p => errors.push(p.message))
        client.OnTurnRejected.Subscribe(p => rejected.push(p.needed, p.total))
        client.HandleFrame(JSON.stringify({ method: 'turn_error', params: { message: 'bad' } }))
        client.HandleFrame(JSON.stringify({ method: 'turn_rejected', params: { needed: 5, total: 4 } }))
        expect(errors).toEqual(['bad'])
        expect(rejected).toEqual([5, 4])
    })

    it('IgnoresUnknownAndMalformedFrames', () =>
    {
        const client = new HarnessClient()
        const raws = ['not json', 'null', '42', '"str"', '[]', '{}', '{"method":"mystery","params":{}}', '{"id":99,"result":1}']
        for (const raw of raws)
        {
            expect(() => client.HandleFrame(raw)).not.toThrow()
        }
    })

    it('TreatsNullErrorAsResultWithoutThrowing', async () =>
    {
        const socket = new FakeSocket()
        const client = new HarnessClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const pending = client.ModelProps()
        expect(() => client.HandleFrame('{"id":1,"error":null}')).not.toThrow()
        await expect(pending).resolves.toBeUndefined()
    })

    it('RejectsImmediatelyWhenNeverConnected', async () =>
    {
        const client = new HarnessClient()
        await expect(client.SendTurn('hi', false)).rejects.toThrow('not connected')
    })

    it('QueuesWhileConnectingThenFlushesOnOpen', () =>
    {
        const socket = new FakeSocket()
        const client = new HarnessClient(() => socket)
        client.Connect('ws://x')
        void client.ModelProps()
        expect(socket.sent).toEqual([])
        socket.Open()
        expect(socket.sent.length).toBe(1)
        expect(JSON.parse(socket.sent[0]).method).toBe('model_props')
        expect(client.IsConnected).toBe(true)
    })

    it('RejectsPendingAndLaterRequestsAfterClose', async () =>
    {
        const socket = new FakeSocket()
        const client = new HarnessClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const inFlight = client.SendTurn('hi', false)
        socket.Close()
        await expect(inFlight).rejects.toThrow('not connected')
        await expect(client.ModelProps()).rejects.toThrow('not connected')
        expect(client.IsConnected).toBe(false)
    })

    it('ThrowingSubscriberDoesNotBlockOthers', () =>
    {
        const signal = new TurnSignal<number>()
        const seen: number[] = []
        const spy = vi.spyOn(console, 'error').mockImplementation(() => undefined)
        signal.Subscribe(() =>
        {
            throw new Error('x')
        })
        signal.Subscribe(n => seen.push(n))
        expect(() => signal.Raise(7)).not.toThrow()
        expect(seen).toEqual([7])
        spy.mockRestore()
    })
})
