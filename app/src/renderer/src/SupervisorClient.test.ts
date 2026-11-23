import { describe, expect, it } from 'vitest'
import { SupervisorClient, type HarnessInfo } from './SupervisorClient'
import { FakeSocket } from './FakeSocket'

describe('SupervisorClient', () =>
{
    it('list_harnesses_parses', async () =>
    {
        const socket = new FakeSocket()
        const client = new SupervisorClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const pending = client.ListHarnesses()
        expect(JSON.parse(socket.sent[0]).method).toBe('list_harnesses')
        const expected: HarnessInfo[] = [
            { id: 'a', runtime: 'llamacpp', model: 'gemma', state: 'ready', url: 'ws://a', chat_url: 'ws://a:8779' },
            { id: 'b', runtime: 'candle', model: 'm', state: 'stopped' }
        ]
        client.HandleFrame(JSON.stringify({ id: 1, result: expected }))
        await expect(pending).resolves.toEqual(expected)
    })

    it('rpc_error_rejects', async () =>
    {
        const socket = new FakeSocket()
        const client = new SupervisorClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const pending = client.ListHarnesses()
        client.HandleFrame(JSON.stringify({ id: 1, error: { code: -1, message: 'boom' } }))
        await expect(pending).rejects.toThrow('boom')
    })

    it('runtime_url_takes_chat_url_port_on_supervisor_host', () =>
    {
        const harnesses: HarnessInfo[] = [
            { id: 'a', runtime: 'r', model: 'm', state: 'loading' },
            { id: 'b', runtime: 'r', model: 'm', state: 'ready', chat_url: 'ws://192.168.1.101:9001' }
        ]
        expect(SupervisorClient.RuntimeUrl('ws://localhost:8778', harnesses)).toBe('ws://localhost:9001')
    })

    it('runtime_url_falls_back_to_8779', () =>
    {
        expect(SupervisorClient.RuntimeUrl('ws://hive.local:8778', [])).toBe('ws://hive.local:8779')
        expect(SupervisorClient.RuntimeUrl('wss://hive.local:8778/', [{ id: 'a', runtime: 'r', model: 'm', state: 'stopped' }])).toBe('wss://hive.local:8779')
    })
})
