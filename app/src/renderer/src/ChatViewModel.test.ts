import { describe, expect, it } from 'vitest'
import { ChatViewModel } from './ChatViewModel'
import { HarnessClient } from './HarnessClient'
import { FakeSocket } from './FakeSocket'
import type { AssistantMessage, ToolCallMessage, ToolResultMessage } from './messages'

class Rig
{
    public readonly socket = new FakeSocket()
    public readonly client = new HarnessClient(() => this.socket)
    public readonly vm = new ChatViewModel(this.client)

    constructor(open: boolean)
    {
        if (open)
        {
            this.client.Connect('ws://x')
            this.socket.Open()
        }
    }

    public Send(text: string): void
    {
        this.vm.Draft = text
        this.vm.SendCommand.Execute()
    }

    public Notify(method: string, params: unknown): void
    {
        this.client.HandleFrame(JSON.stringify({ method, params }))
    }

    /** Id of the most recent frame sent with this method. */
    public IdOf(method: string): number
    {
        const frames = this.socket.sent.map(f => JSON.parse(f) as { id: number, method: string })
        return frames.filter(f => f.method === method).pop()?.id ?? -1
    }

    /** Answers the most recent request of this method. */
    public Respond(method: string, result: unknown = null): void
    {
        this.client.HandleFrame(JSON.stringify({ id: this.IdOf(method), result }))
    }

    public async Flush(): Promise<void>
    {
        await new Promise(r => setTimeout(r, 0))
    }

    public Count(): number
    {
        return this.vm.Transcript.ToArray().length
    }

    public Reply(index: number): AssistantMessage
    {
        return this.vm.Transcript.ToArray()[index] as AssistantMessage
    }
}

describe('ChatViewModel', () =>
{
    it('IgnoresEmptyDraft', () =>
    {
        const rig = new Rig(true)
        rig.Send('   ')
        expect(rig.Count()).toBe(0)
        expect(rig.socket.sent.length).toBe(0)
    })

    it('StreamsChunksIntoTheCurrentReply', () =>
    {
        const rig = new Rig(true)
        rig.Send('hi')
        rig.Notify('turn_chunk', { text: 'a', tokens: [], stop: false })
        rig.Notify('turn_chunk', { text: 'b', tokens: [], stop: true })
        rig.Notify('turn_done', {})
        expect(rig.vm.Draft).toBe('')
        expect(rig.Reply(1).Text).toBe('ab')
    })

    it('BlocksSecondSendWhileTurnActiveThenAllowsAfterDone', async () =>
    {
        const rig = new Rig(true)
        rig.Send('one')
        rig.Send('two')
        expect(rig.Count()).toBe(2)
        expect(rig.vm.Draft).toBe('two')
        rig.Notify('turn_chunk', { text: 'x', tokens: [], stop: false })
        rig.Notify('turn_done', {})
        rig.Respond('send_turn')
        await rig.Flush()
        rig.Send('two')
        expect(rig.Count()).toBe(4)
        rig.Notify('turn_chunk', { text: 'y', tokens: [], stop: false })
        expect(rig.Reply(1).Text).toBe('x')
        expect(rig.Reply(3).Text).toBe('y')
    })

    it('LateRejectionOfOldTurnDoesNotClobberNewTarget', async () =>
    {
        const rig = new Rig(true)
        rig.Send('one')
        rig.Notify('turn_done', {})
        // Rebinding drops the in-flight turn; the old turn's send_turn then fails late.
        const oldSend = rig.IdOf('send_turn')
        rig.vm.Bind(rig.client)
        rig.Send('two')
        rig.client.HandleFrame(JSON.stringify({ id: oldSend, error: { code: 1, message: 'late' } }))
        await rig.Flush()
        rig.Notify('turn_chunk', { text: 'z', tokens: [], stop: false })
        expect(rig.Reply(1).Text).toBe('')
        expect(rig.Reply(4).Text).toBe('z')
    })

    it('NoDuplicateSendFailedNoteAfterTurnError', async () =>
    {
        const rig = new Rig(true)
        rig.Send('one')
        rig.Notify('turn_error', { message: 'boom' })
        rig.client.HandleFrame(JSON.stringify({ id: rig.IdOf('send_turn'), error: { code: 1, message: 'boom' } }))
        await rig.Flush()
        expect(rig.Reply(1).Text).toBe('\n[error: boom]')
    })

    it('RejectedTurnAppendsWindowNoteAndAllowsNextSend', async () =>
    {
        const rig = new Rig(true)
        rig.Send('one')
        rig.Notify('turn_rejected', { needed: 9, total: 4 })
        expect(rig.Reply(1).Text).toBe('\n[context window full: needed 9 of 4 tokens]')
        rig.Notify('loop_done', { steps: 0, reason: 'error' })
        rig.Respond('send_turn')
        await rig.Flush()
        rig.Send('two')
        expect(rig.Count()).toBe(5)
    })

    it('SendWithNoConnectionSettlesWithNote', async () =>
    {
        const rig = new Rig(false)
        rig.Send('hi')
        await rig.Flush()
        expect(rig.Reply(1).Text).toContain('[send failed: Harness not connected]')
        rig.Send('again')
        expect(rig.Count()).toBe(4)
    })

    it('starts_unbound_cannot_chat', () =>
    {
        const vm = new ChatViewModel()
        expect(vm.CanChat).toBe(false)
        vm.Draft = 'hi'
        vm.SendCommand.Execute()
        expect(vm.Transcript.ToArray().length).toBe(0)
    })

    it('composer_placeholder_tracks_binding', () =>
    {
        const socket = new FakeSocket()
        const client = new HarnessClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const vm = new ChatViewModel()
        let raised = 0
        vm.PropertyChanged('ComposerPlaceholder').subscribe(() => { raised++ })
        expect(vm.ComposerPlaceholder).toBe('Start a harness to chat')
        vm.Bind(client)
        expect(vm.ComposerPlaceholder).toBe('Message the model…')
        expect(raised).toBe(1)
        vm.Bind(null)
        expect(vm.ComposerPlaceholder).toBe('Start a harness to chat')
    })

    it('bind_enables_chat_and_routes_chunks', () =>
    {
        const socket = new FakeSocket()
        const client = new HarnessClient(() => socket)
        client.Connect('ws://x')
        socket.Open()
        const vm = new ChatViewModel()
        vm.Bind(client)
        expect(vm.CanChat).toBe(true)
        vm.Draft = 'hi'
        vm.SendCommand.Execute()
        client.HandleFrame(JSON.stringify({ method: 'turn_chunk', params: { text: 'a', tokens: [], stop: false } }))
        expect((vm.Transcript.ToArray()[1] as AssistantMessage).Text).toBe('a')
    })

    it('rebind_ignores_old_client_signals', () =>
    {
        const a = new Rig(true)
        a.Send('hi')
        const clientB = new HarnessClient(() => new FakeSocket())
        a.vm.Bind(clientB)
        a.Notify('turn_chunk', { text: 'zzz', tokens: [], stop: false })
        expect(a.Reply(1).Text).not.toContain('zzz')
        // The panel must ignore the old client too.
        a.Notify('turn_started', { prompt: 'old', prompt_tokens: 1, n_ctx: 2 })
        a.Notify('turn_chunk', { text: 'q', tokens: [{ token: 'q', logprob: -1, top: [] }], stop: false })
        expect(a.vm.Panel.Logprobs.ToArray().length).toBe(0)
        expect(a.vm.Panel.Prompt).toBe('')
    })

    it('rebind_notes_abandoned_reply_and_clears_in_flight_turn', () =>
    {
        const rig = new Rig(true)
        rig.Send('one')
        const socketB = new FakeSocket()
        const clientB = new HarnessClient(() => socketB)
        clientB.Connect('ws://y')
        socketB.Open()
        rig.vm.Bind(clientB)
        expect(rig.Reply(1).Text).toContain('[disconnected]')
        rig.vm.Draft = 'two'
        rig.vm.SendCommand.Execute()
        expect(rig.Count()).toBe(4)
        clientB.HandleFrame(JSON.stringify({ method: 'turn_chunk', params: { text: 'b', tokens: [], stop: false } }))
        expect(rig.Reply(3).Text).toBe('b')
    })

    it('submit_command_disabled_when_unbound', () =>
    {
        const vm = new ChatViewModel()
        expect(vm.SubmitCommand.CanExecute()).toBe(false)
    })

    it('unbind_disables_send', () =>
    {
        const rig = new Rig(true)
        rig.vm.Bind(null)
        expect(rig.vm.CanChat).toBe(false)
        rig.Send('hi')
        expect(rig.Count()).toBe(0)
    })

    it('multi_step_turn_renders_call_result_and_answer', async () =>
    {
        const rig = new Rig(true)
        rig.Send('time?')
        rig.Notify('turn_started', { prompt: 'p', prompt_tokens: 1, n_ctx: 8 })
        rig.Notify('turn_chunk', { text: '', tokens: [], stop: true })
        rig.Notify('tool_call', { id: 'c1', name: 'get_time', arguments: { tz: 'utc' }, raw: '{}' })
        rig.Notify('turn_done', {})
        rig.Notify('tool_result', { id: 'c1', name: 'get_time', ok: true, content: '14:05' })
        rig.Notify('turn_started', { prompt: 'p2', prompt_tokens: 2, n_ctx: 8 })
        rig.Notify('turn_chunk', { text: "It's 14:05.", tokens: [], stop: true })
        rig.Notify('turn_done', {})
        rig.Notify('loop_done', { steps: 2, reason: 'answered' })
        rig.Respond('send_turn')
        await rig.Flush()
        const items = rig.vm.Transcript.ToArray()
        expect(items.map(i => i.constructor.name)).toEqual(['UserMessage', 'AssistantMessage', 'ToolCallMessage', 'ToolResultMessage', 'AssistantMessage'])
        const call = items[2] as ToolCallMessage
        expect(call.Name).toBe('get_time')
        expect(call.Arguments).toBe('{"tz":"utc"}')
        const result = items[3] as ToolResultMessage
        expect(result.StatusText).toBe('ok')
        expect(result.Content).toBe('14:05')
        expect((items[4] as AssistantMessage).Text).toBe("It's 14:05.")
    })

    it('send_resolves_after_loop_done_not_turn_done', async () =>
    {
        const rig = new Rig(true)
        rig.Send('one')
        rig.Notify('turn_chunk', { text: 'a', tokens: [], stop: true })
        rig.Notify('turn_done', {})
        rig.Send('two')
        expect(rig.Count()).toBe(2)
        rig.Notify('loop_done', { steps: 1, reason: 'answered' })
        rig.Respond('send_turn')
        await rig.Flush()
        rig.Send('two')
        expect(rig.Count()).toBe(4)
    })

    it('not_kept_loop_adds_note', () =>
    {
        const rig = new Rig(true)
        rig.Send('one')
        rig.Notify('turn_done', {})
        rig.Notify('loop_done', { steps: 8, reason: 'step_limit' })
        const items = rig.vm.Transcript.ToArray()
        expect((items[items.length - 1] as AssistantMessage).Text).toBe('[not kept: step_limit]')
    })

    it('tools_text_reflects_runtime', async () =>
    {
        const rig = new Rig(false)
        expect(rig.vm.ToolsText).toBe('tools: off')
        rig.client.Connect('ws://x')
        rig.socket.Open()
        rig.vm.Bind(rig.client)
        rig.Respond('model_props', { n_ctx: 8, model_name: 'm', supports_tools: true })
        await rig.Flush()
        rig.Respond('list_tools', [{ name: 'get_time', description: '', side_effects: false }])
        await rig.Flush()
        expect(rig.vm.ToolsText).toBe('tools: get_time')

        const off = new Rig(false)
        off.client.Connect('ws://x')
        off.socket.Open()
        off.vm.Bind(off.client)
        off.Respond('model_props', { n_ctx: 8, model_name: 'm', supports_tools: false })
        await off.Flush()
        expect(off.vm.ToolsText).toBe('tools: off')
        expect(off.IdOf('list_tools')).toBe(-1)
    })

    it('model_props_is_fetched_once_per_bind', () =>
    {
        const rig = new Rig(false)
        rig.client.Connect('ws://x')
        rig.socket.Open()
        rig.vm.Bind(rig.client)
        expect(rig.socket.sent.filter(f => JSON.parse(f).method === 'model_props').length).toBe(1)
    })

    it('disconnect_between_loop_steps_still_notes', () =>
    {
        const rig = new Rig(true)
        rig.Send('one')
        rig.Notify('turn_chunk', { text: 'a', tokens: [], stop: true })
        rig.Notify('turn_done', {})
        rig.vm.Bind(null)
        const items = rig.vm.Transcript.ToArray()
        expect((items[items.length - 1] as AssistantMessage).Text).toBe('\n[disconnected]')
        expect(rig.Reply(1).Text).toBe('a')
    })

    it('rebind_with_live_segment_notes_old_and_leaves_new_untouched', () =>
    {
        const rig = new Rig(true)
        rig.Send('one')
        rig.Notify('turn_chunk', { text: 'a', tokens: [], stop: false })
        const socketB = new FakeSocket()
        const clientB = new HarnessClient(() => socketB)
        clientB.Connect('ws://y')
        socketB.Open()
        rig.vm.Bind(clientB)
        expect(rig.Reply(1).Text).toBe('a\n[disconnected]')
        expect(rig.Count()).toBe(2)
        // Stale chunk on the OLD client must reach neither the old segment nor the new binding.
        rig.Notify('turn_chunk', { text: 'late', tokens: [], stop: false })
        expect(rig.Reply(1).Text).toBe('a\n[disconnected]')
        expect(rig.Count()).toBe(2)
    })

    it('tool_call_with_parse_error_shows_raw_and_error', () =>
    {
        const rig = new Rig(true)
        rig.Send('one')
        rig.Notify('tool_call', { id: 'c1', name: 'read_file', arguments: 'oops', raw: '{"path": ', parse_error: 'EOF' })
        const call = rig.vm.Transcript.ToArray()[2] as ToolCallMessage
        expect(call.Arguments).toBe('{"path":   [parse error: EOF]')
    })

    it('tool_call_without_arguments_renders_empty', () =>
    {
        const rig = new Rig(true)
        rig.Send('one')
        rig.Notify('tool_call', { id: 'c1', name: 'get_time', raw: '' })
        rig.Notify('tool_call', { id: 'c2', name: 'get_time', arguments: null, raw: '' })
        rig.Notify('tool_result', { id: 'c2', name: 'get_time', ok: true })
        const items = rig.vm.Transcript.ToArray()
        expect((items[2] as ToolCallMessage).Arguments).toBe('')
        expect((items[3] as ToolCallMessage).Arguments).toBe('')
        expect((items[4] as ToolResultMessage).Content).toBe('')
    })
})
