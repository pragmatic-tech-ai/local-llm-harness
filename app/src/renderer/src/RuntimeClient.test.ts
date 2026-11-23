import { describe, expect, it, vi } from 'vitest'
import { RuntimeClient, TypedSignal, type ModelProps, type TurnChunk } from './RuntimeClient'
import type { AgentTemplate } from './agents'
import { FakeSocket } from './FakeSocket'

describe('RuntimeClient', () =>
{
    it('MapsTurnChunkNotificationsToText', () =>
    {
        const client = new RuntimeClient()
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
        const client = new RuntimeClient(() => socket)
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
        const client = new RuntimeClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const pending = client.Tokenize('x')
        client.HandleFrame(JSON.stringify({ id: 1, error: { code: -1, message: 'boom' } }))
        await expect(pending).rejects.toThrow('boom')
    })

    it('RaisesTurnErrorAndRejected', () =>
    {
        const client = new RuntimeClient()
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
        const client = new RuntimeClient()
        const raws = ['not json', 'null', '42', '"str"', '[]', '{}', '{"method":"mystery","params":{}}', '{"id":99,"result":1}']
        for (const raw of raws)
        {
            expect(() => client.HandleFrame(raw)).not.toThrow()
        }
    })

    it('TreatsNullErrorAsResultWithoutThrowing', async () =>
    {
        const socket = new FakeSocket()
        const client = new RuntimeClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const pending = client.ModelProps()
        expect(() => client.HandleFrame('{"id":1,"error":null}')).not.toThrow()
        await expect(pending).resolves.toBeUndefined()
    })

    it('RejectsImmediatelyWhenNeverConnected', async () =>
    {
        const client = new RuntimeClient()
        await expect(client.SendTurn('hi')).rejects.toThrow('not connected')
    })

    it('QueuesWhileConnectingThenFlushesOnOpen', () =>
    {
        const socket = new FakeSocket()
        const client = new RuntimeClient(() => socket)
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
        const client = new RuntimeClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const inFlight = client.SendTurn('hi')
        socket.Close()
        await expect(inFlight).rejects.toThrow('not connected')
        await expect(client.ModelProps()).rejects.toThrow('not connected')
        expect(client.IsConnected).toBe(false)
    })

    it('ThrowingSubscriberDoesNotBlockOthers', () =>
    {
        const signal = new TypedSignal<number>()
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

    it('subscribe_returns_disposable_that_unsubscribes', () =>
    {
        const signal = new TypedSignal<number>()
        const seen: number[] = []
        const subscription = signal.Subscribe(n => seen.push(n))
        signal.Raise(1)
        subscription.Dispose()
        signal.Raise(2)
        expect(seen).toEqual([1])
    })

    it('disconnect_rejects_pending_and_closes', async () =>
    {
        const socket = new FakeSocket()
        const client = new RuntimeClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const inFlight = client.SendTurn('hi')
        client.Disconnect()
        await expect(inFlight).rejects.toThrow('not connected')
        expect(socket.closeCalls).toBe(1)
        expect(client.IsConnected).toBe(false)
        expect(socket.onmessage).toBeNull()
    })

    it('reconnect_closes_previous_socket', () =>
    {
        const sockets = [new FakeSocket(), new FakeSocket()]
        let next = 0
        const client = new RuntimeClient(() => sockets[next++])
        client.Connect('ws://a')
        sockets[0].Open()
        client.Connect('ws://b')
        expect(sockets[0].closeCalls).toBe(1)
        expect(sockets[1].closeCalls).toBe(0)
        expect(sockets[0].onmessage).toBeNull()
    })

    it('dispose_removes_exact_subscription_when_handler_subscribed_twice', () =>
    {
        const signal = new TypedSignal<number>()
        const seen: number[] = []
        const handler = (n: number) => seen.push(n)
        signal.Subscribe(handler)
        const second = signal.Subscribe(handler)
        second.Dispose()
        second.Dispose()
        signal.Raise(1)
        expect(seen).toEqual([1])
    })

    it('reconnect_drops_frames_queued_before_open', () =>
    {
        const sockets = [new FakeSocket(), new FakeSocket()]
        let next = 0
        const client = new RuntimeClient(() => sockets[next++])
        client.Connect('ws://a')
        void client.ModelProps().catch(() => undefined)
        client.Connect('ws://b')
        sockets[1].Open()
        expect(sockets[1].sent).toEqual([])
    })

    it('tool_notifications_raise_signals', () =>
    {
        const client = new RuntimeClient()
        const seen: unknown[] = []
        client.OnToolCall.Subscribe(p => seen.push(p))
        client.OnToolResult.Subscribe(p => seen.push(p))
        client.OnLoopDone.Subscribe(p => seen.push(p))
        const call = { id: 'c1', name: 'get_time', arguments: {}, raw: '{}' }
        const result = { id: 'c1', name: 'get_time', ok: true, content: '14:05' }
        const done = { steps: 2, reason: 'answered' }
        client.HandleFrame(JSON.stringify({ method: 'tool_call', params: call }))
        client.HandleFrame(JSON.stringify({ method: 'tool_result', params: result }))
        client.HandleFrame(JSON.stringify({ method: 'loop_done', params: done }))
        expect(seen).toEqual([call, result, done])
    })

    it('each_method_sends_its_wire_name_and_params', async () =>
    {
        const template = { name: 'T' } as AgentTemplate
        const sampling = { temperature: 1, top_p: 1, top_k: 1, repeat_penalty: 1, seed: 1, max_tokens: 1 }
        const placement = { state: 'needs_load', host: 'h', evicts: null }
        const summary = { id: 's', title: 't', template_id: 'a', template_name: 'T', runtime: 'r', model: 'm', updated: 7, placement, busy: false }
        const attached = { summary, snapshot: template, transcript: [{ role: 'user', content: 'hi' }], sampling, enable_thinking: true, placement }
        const nodes = [{ kind: 'template', id: 'a', path: '', name: 'T', runtime: 'r', model: 'm', error: null }]
        const record = { id: 'a', path: 'g', template, error: 'unknown tool: nope' }
        // A server that predates the error field: the record reads as valid.
        const bare = { id: 'a', path: 'g', template }
        const catalog = { runtimes: [{ name: 'r' }], models: [{ name: 'm' }], hosts: ['h'] }
        const specs = [{ name: 'echo', description: 'd', parameters: {}, side_effects: false }]
        const props = { n_ctx: 4096, model_name: 'm' }
        const tokens = { tokens: [1, 2], count: 2 }
        // reply is the wire result; result is what the method must resolve to (defaults to the reply).
        const table: { call: (c: RuntimeClient) => Promise<unknown>, method: string, params: unknown, reply: unknown, result?: unknown }[] = [
            { call: c => c.ListTemplates(), method: 'list_templates', params: {}, reply: nodes },
            { call: c => c.GetTemplate('a'), method: 'get_template', params: { id: 'a' }, reply: record },
            { call: c => c.GetTemplate('a'), method: 'get_template', params: { id: 'a' }, reply: bare, result: { ...bare, error: null } },
            { call: c => c.SaveTemplate('a', 'g', template), method: 'save_template', params: { id: 'a', path: 'g', template }, reply: { id: 'a' }, result: 'a' },
            { call: c => c.SaveTemplate(undefined, undefined, template), method: 'save_template', params: { template }, reply: { id: 'new' }, result: 'new' },
            { call: c => c.DeleteTemplate('a'), method: 'delete_template', params: { id: 'a' }, reply: null, result: undefined },
            { call: c => c.CreateGroup('g'), method: 'create_group', params: { path: 'g' }, reply: null, result: undefined },
            { call: c => c.RenameGroup('g', 'h'), method: 'rename_group', params: { path: 'g', new_name: 'h' }, reply: null, result: undefined },
            { call: c => c.DeleteGroup('g'), method: 'delete_group', params: { path: 'g' }, reply: null, result: undefined },
            { call: c => c.MoveTemplate('a', 'g'), method: 'move_template', params: { id: 'a', path: 'g' }, reply: null, result: undefined },
            { call: c => c.ListCatalog(), method: 'list_catalog', params: {}, reply: catalog },
            { call: c => c.ListAllTools(), method: 'list_all_tools', params: {}, reply: specs },
            { call: c => c.ListSessions(), method: 'list_sessions', params: {}, reply: [summary] },
            { call: c => c.CreateSession('t'), method: 'create_session', params: { template_id: 't' }, reply: summary },
            { call: c => c.AttachSession('s'), method: 'attach_session', params: { id: 's' }, reply: attached },
            { call: c => c.RenameSession('s', 'x'), method: 'rename_session', params: { id: 's', title: 'x' }, reply: null, result: undefined },
            { call: c => c.DeleteSession('s'), method: 'delete_session', params: { id: 's' }, reply: null, result: undefined },
            { call: c => c.LoadSessionModel('s'), method: 'load_session_model', params: { id: 's' }, reply: { state: 'ready', host: 'h' } },
            { call: c => c.UnloadSessionModel('s'), method: 'unload_session_model', params: { id: 's' }, reply: null, result: undefined },
            { call: c => c.SetThinking(true), method: 'set_thinking', params: { enable: true }, reply: null, result: undefined },
            { call: c => c.SetSampling(sampling), method: 'set_sampling', params: { config: sampling }, reply: null, result: undefined },
            { call: c => c.SendTurn('hi'), method: 'send_turn', params: { content: 'hi' }, reply: null, result: undefined },
            { call: c => c.ModelProps(), method: 'model_props', params: {}, reply: props },
            { call: c => c.Tokenize('abc'), method: 'tokenize', params: { text: 'abc' }, reply: tokens }
        ]
        for (const row of table)
        {
            const socket = new FakeSocket()
            const client = new RuntimeClient(() => socket)
            client.Connect('ws://x')
            socket.Open()
            const pending = row.call(client)
            const frame = JSON.parse(socket.sent[0])
            expect(frame.method).toBe(row.method)
            expect(frame.params).toEqual(row.params)
            client.HandleFrame(JSON.stringify({ id: frame.id, result: row.reply }))
            const expected = 'result' in row ? row.result : row.reply
            await expect(pending, row.method).resolves.toEqual(expected)
        }
    })

    it('list_level_notifications_raise_signals', () =>
    {
        const client = new RuntimeClient()
        const seen: unknown[] = []
        client.OnSessionsChanged.Subscribe(() => seen.push('sessions'))
        client.OnTemplatesChanged.Subscribe(() => seen.push('templates'))
        client.OnPlacementChanged.Subscribe(p => seen.push(p))
        client.OnSessionLoading.Subscribe(p => seen.push(p))
        client.OnSessionBusy.Subscribe(p => seen.push(p))
        const placement = { id: 's', placement: { state: 'ready', host: 'h' } }
        client.HandleFrame(JSON.stringify({ method: 'sessions_changed', params: {} }))
        client.HandleFrame(JSON.stringify({ method: 'templates_changed', params: {} }))
        client.HandleFrame(JSON.stringify({ method: 'placement_changed', params: placement }))
        client.HandleFrame(JSON.stringify({ method: 'session_loading', params: { id: 's' } }))
        client.HandleFrame(JSON.stringify({ method: 'session_busy', params: { id: 's', busy: true } }))
        expect(seen).toEqual(['sessions', 'templates', placement, { id: 's' }, { id: 's', busy: true }])
    })
})
