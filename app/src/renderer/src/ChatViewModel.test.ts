import { describe, expect, it } from 'vitest'
import { ChatViewModel } from './ChatViewModel'
import { HarnessClient } from './HarnessClient'
import { FakeSocket } from './FakeSocket'
import type { AssistantMessage } from './messages'

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

    it('BlocksSecondSendWhileTurnActiveThenAllowsAfterDone', () =>
    {
        const rig = new Rig(true)
        rig.Send('one')
        rig.Send('two')
        expect(rig.Count()).toBe(2)
        expect(rig.vm.Draft).toBe('two')
        rig.Notify('turn_chunk', { text: 'x', tokens: [], stop: false })
        rig.Notify('turn_done', {})
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
        rig.Send('two')
        // The old turn's send_turn response (id 2; set_sampling takes id 1) arrives late, as an error.
        rig.client.HandleFrame(JSON.stringify({ id: 2, error: { code: 1, message: 'late' } }))
        await Promise.resolve()
        rig.Notify('turn_chunk', { text: 'z', tokens: [], stop: false })
        expect(rig.Reply(1).Text).toBe('')
        expect(rig.Reply(3).Text).toBe('z')
    })

    it('NoDuplicateSendFailedNoteAfterTurnError', async () =>
    {
        const rig = new Rig(true)
        rig.Send('one')
        rig.Notify('turn_error', { message: 'boom' })
        rig.client.HandleFrame(JSON.stringify({ id: 2, error: { code: 1, message: 'boom' } }))
        await Promise.resolve()
        expect(rig.Reply(1).Text).toBe('\n[error: boom]')
    })

    it('RejectedTurnAppendsWindowNoteAndAllowsNextSend', () =>
    {
        const rig = new Rig(true)
        rig.Send('one')
        rig.Notify('turn_rejected', { needed: 9, total: 4 })
        expect(rig.Reply(1).Text).toBe('\n[context window full: needed 9 of 4 tokens]')
        rig.Send('two')
        expect(rig.Count()).toBe(4)
    })

    it('SendWithNoConnectionSettlesWithNote', async () =>
    {
        const rig = new Rig(false)
        rig.Send('hi')
        await new Promise(r => setTimeout(r, 0))
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
})
