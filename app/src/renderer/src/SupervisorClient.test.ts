import { describe, expect, it } from 'vitest'
import { SupervisorClient, type HarnessInfo, type SupervisorHello } from './SupervisorClient'
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
        expect(SupervisorClient.RuntimeUrl('ws://localhost:8778', null, harnesses)).toBe('ws://localhost:9001')
    })

    it('hello_parses_and_its_runtime_url_wins_over_chat_url', async () =>
    {
        const socket = new FakeSocket()
        const client = new SupervisorClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const pending = client.Hello()
        expect(JSON.parse(socket.sent[0]).method).toBe('hello')
        const hello: SupervisorHello = { runtime_url: 'ws://192.168.1.101:9100', runtime_error: null }
        client.HandleFrame(JSON.stringify({ id: 1, result: hello }))
        await expect(pending).resolves.toEqual(hello)
        const harnesses: HarnessInfo[] = [{ id: 'b', runtime: 'r', model: 'm', state: 'ready', chat_url: 'ws://192.168.1.101:9001' }]
        expect(SupervisorClient.RuntimeUrl('ws://hive.local:8778', hello, harnesses)).toBe('ws://hive.local:9100')
        // No runtime url in hello (the runtime is off): the harnesses, then the default port.
        const off: SupervisorHello = { runtime_url: null, runtime_error: 'agents data unavailable: x' }
        expect(SupervisorClient.RuntimeUrl('ws://hive.local:8778', off, harnesses)).toBe('ws://hive.local:9001')
        expect(SupervisorClient.RuntimeUrl('ws://hive.local:8778', off, [])).toBe('ws://hive.local:8779')
    })

    it('runtime_url_falls_back_to_8779', () =>
    {
        expect(SupervisorClient.RuntimeUrl('ws://hive.local:8778', null, [])).toBe('ws://hive.local:8779')
        expect(SupervisorClient.RuntimeUrl('wss://hive.local:8778/', null, [{ id: 'a', runtime: 'r', model: 'm', state: 'stopped' }])).toBe('wss://hive.local:8779')
    })
})
