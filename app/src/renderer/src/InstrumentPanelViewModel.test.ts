import { describe, expect, it } from 'vitest'
import { ChatViewModel } from './ChatViewModel'
import { HarnessClient } from './HarnessClient'
import { FakeSocket } from './FakeSocket'
import { LogprobVM } from './LogprobVM'

class Rig
{
    public readonly socket = new FakeSocket()
    public readonly client = new HarnessClient(() => this.socket)
    public readonly vm: ChatViewModel

    constructor()
    {
        this.client.Connect('ws://x')
        this.socket.Open()
        this.vm = new ChatViewModel(this.client)
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

    /** Sampling configs sent so far, in order. */
    public SamplingSent(): { temperature: number }[]
    {
        return this.socket.sent
            .map(f => JSON.parse(f) as { method: string, params: { config?: { temperature: number } } })
            .filter(f => f.method === 'set_sampling')
            .map(f => f.params.config as { temperature: number })
    }

    public Methods(): string[]
    {
        return this.socket.sent.map(f => (JSON.parse(f) as { method: string }).method)
    }
}

describe('InstrumentPanelViewModel', () =>
{
    it('KnobEditsApplyToNextTurnNotCurrent', () =>
    {
        const rig = new Rig()
        rig.Send('one')
        rig.vm.Panel.Temperature = 1.5
        rig.Notify('turn_done', {})
        rig.Send('two')

        const sent = rig.SamplingSent()
        expect(sent.map(c => c.temperature)).toEqual([0.8, 1.5])
        // set_sampling lands before each send_turn
        expect(rig.Methods().filter(m => m !== 'model_props')).toEqual(['set_sampling', 'send_turn', 'set_sampling', 'send_turn'])
    })

    it('ForwardsThinkingToggleOnSend', () =>
    {
        const rig = new Rig()
        rig.vm.Panel.EnableThinking = true
        rig.Send('one')
        const turn = rig.socket.sent.map(f => JSON.parse(f)).find(f => f.method === 'send_turn')
        expect(turn.params.enable_thinking).toBe(true)
    })

    it('WindowFullSurfacesWarning', () =>
    {
        const rig = new Rig()
        rig.Send('one')
        rig.Notify('turn_started', { prompt: 'p', prompt_tokens: 5000, n_ctx: 4096 })
        rig.Notify('turn_rejected', { needed: 5000, total: 4096 })
        expect(rig.vm.Panel.Warning).toContain('5000')
        expect(rig.vm.Panel.Warning).toContain('4096')
        expect(rig.vm.Panel.HasWarning).toBe(true)
        expect(rig.vm.Transcript.ToArray().length).toBe(2)
        rig.Notify('turn_started', { prompt: 'p2', prompt_tokens: 10, n_ctx: 4096 })
        expect(rig.vm.Panel.Warning).toBe('')
    })

    it('TracksPromptContextAndLogprobsPerTurn', () =>
    {
        const rig = new Rig()
        rig.Send('one')
        rig.Notify('turn_started', { prompt: 'RAW', prompt_tokens: 1024, n_ctx: 4096 })
        rig.Notify('turn_chunk', { text: 'a', tokens: [{ token: 'a', logprob: -0.5, top: [] }], stop: false })
        expect(rig.vm.Panel.Prompt).toBe('RAW')
        expect(rig.vm.Panel.ContextFraction).toBe(0.25)
        expect(rig.vm.Panel.ContextLabel).toBe('1024 / 4096 (25%)')
        expect(rig.vm.Panel.Logprobs.ToArray().length).toBe(1)
        rig.Notify('turn_done', {})
        rig.Send('two')
        rig.Notify('turn_started', { prompt: 'RAW2', prompt_tokens: 1, n_ctx: 4096 })
        expect(rig.vm.Panel.Logprobs.ToArray().length).toBe(0)
    })

    it('ShowsModelNameAsBasename', async () =>
    {
        const rig = new Rig()
        rig.client.HandleFrame(JSON.stringify({ id: 1, result: { n_ctx: 4096, model_name: 'C:\\models\\gemma\\g-12b.gguf' } }))
        await Promise.resolve()
        expect(rig.vm.Panel.ModelName).toBe('g-12b.gguf')
        expect(rig.vm.Panel.ContextTotal).toBe(4096)
    })

    it('DefaultsMaxTokensTo256', () =>
    {
        const rig = new Rig()
        expect(rig.vm.Panel.Config.max_tokens).toBe(256)
    })
})

describe('LogprobVM', () =>
{
    it('RendersSentinelAsNotAvailable', () =>
    {
        expect(new LogprobVM({ token: 't', logprob: -3.4028235e38, top: [] }).LogprobDisplay).toBe('n/a')
        expect(new LogprobVM({ token: 't', logprob: Number.NEGATIVE_INFINITY, top: [] }).LogprobDisplay).toBe('n/a')
    })

    it('RendersNormalValueAsNumber', () =>
    {
        const vm = new LogprobVM({ token: 'hi', logprob: -0.12345, top: [] })
        expect(vm.LogprobDisplay).toBe('-0.123')
        expect(vm.Token).toBe('hi')
    })
})
