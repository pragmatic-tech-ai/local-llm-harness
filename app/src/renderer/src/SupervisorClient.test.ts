import { describe, expect, it } from 'vitest'
import { SupervisorClient, type HarnessInfo, type ModelInfo } from './SupervisorClient'
import { FakeSocket } from './FakeSocket'

describe('SupervisorClient', () =>
{
    it('start_resolves_with_harness_info', async () =>
    {
        const socket = new FakeSocket()
        const client = new SupervisorClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const pending = client.Start('llamacpp', 'gemma')
        expect(JSON.parse(socket.sent[0])).toEqual({ id: 1, method: 'start', params: { runtime: 'llamacpp', model: 'gemma' } })
        const expected: HarnessInfo = { id: 'h1', runtime: 'llamacpp', model: 'gemma', state: 'running', url: 'ws://h1' }
        client.HandleFrame(JSON.stringify({ id: 1, result: expected }))
        await expect(pending).resolves.toEqual(expected)
    })

    it('list_models_parses', async () =>
    {
        const socket = new FakeSocket()
        const client = new SupervisorClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const pending = client.ListModels()
        const expected: ModelInfo[] = [{ name: 'gemma', runtimes: ['llamacpp', 'candle'], size_hint: '4GB' }]
        client.HandleFrame(JSON.stringify({ id: 1, result: expected }))
        await expect(pending).resolves.toEqual(expected)
    })

    it('harness_changed_notification_raises_signal', () =>
    {
        const client = new SupervisorClient()
        const seen: HarnessInfo[] = []
        client.OnHarnessChanged.Subscribe(p => seen.push(p))
        const info: HarnessInfo = { id: 'h1', runtime: 'candle', model: 'm', state: 'stopped' }
        client.HandleFrame(JSON.stringify({ method: 'harness_changed', params: info }))
        expect(seen).toEqual([info])
    })

    it('request_before_open_is_queued_then_flushed', async () =>
    {
        const socket = new FakeSocket()
        const client = new SupervisorClient(() => socket)
        client.Connect('ws://x')
        const pending = client.ListRuntimes()
        expect(socket.sent).toEqual([])
        socket.Open()
        expect(socket.sent).toHaveLength(1)
        client.HandleFrame(JSON.stringify({ id: 1, result: [{ name: 'llamacpp', kind: 'native', available: true }] }))
        await expect(pending).resolves.toEqual([{ name: 'llamacpp', kind: 'native', available: true }])
    })

    it('rpc_error_rejects', async () =>
    {
        const socket = new FakeSocket()
        const client = new SupervisorClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const pending = client.Stop('nope')
        client.HandleFrame(JSON.stringify({ id: 1, error: { code: -1, message: 'unknown harness' } }))
        await expect(pending).rejects.toThrow('unknown harness')
    })

    it('stop_sends_id_on_the_wire', () =>
    {
        const socket = new FakeSocket()
        const client = new SupervisorClient(() => socket)
        client.Connect('ws://x')
        void client.Stop('candle:gemma-3-4b').catch(() => undefined)
        socket.Open()
        expect(JSON.parse(socket.sent[0])).toEqual({ id: 1, method: 'stop', params: { id: 'candle:gemma-3-4b' } })
    })

    it('list_harnesses_parses', async () =>
    {
        const socket = new FakeSocket()
        const client = new SupervisorClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const pending = client.ListHarnesses()
        expect(JSON.parse(socket.sent[0]).method).toBe('list_harnesses')
        const expected: HarnessInfo[] = [
            { id: 'a', runtime: 'llamacpp', model: 'gemma', state: 'running', url: 'ws://a' },
            { id: 'b', runtime: 'candle', model: 'm', state: 'stopped' }
        ]
        client.HandleFrame(JSON.stringify({ id: 1, result: expected }))
        await expect(pending).resolves.toEqual(expected)
    })
})
