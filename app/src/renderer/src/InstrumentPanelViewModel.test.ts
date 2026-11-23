import { describe, expect, it } from 'vitest'
import { ChatViewModel } from './ChatViewModel'
import { RuntimeClient } from './RuntimeClient'
import { FakeSocket } from './FakeSocket'
import { LogprobVM } from './LogprobVM'

class Rig
{
    public readonly socket = new FakeSocket()
    public readonly client = new RuntimeClient(() => this.socket)
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

    public async Flush(): Promise<void>
    {
        await new Promise(r => setTimeout(r, 0))
    }

    /** Ends the turn the way the runtime does: answers the latest send_turn (after its terminal notifications). */
    public async FinishTurn(): Promise<void>
    {
        this.client.HandleFrame(JSON.stringify({ id: this.IdOf('send_turn'), result: null }))
        await this.Flush()
    }

    /** Id of the most recent frame sent with this method. */
    public IdOf(method: string): number
    {
        const frames = this.socket.sent.map(f => JSON.parse(f) as { id: number, method: string })
        return frames.filter(f => f.method === method).pop()?.id ?? -1
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
    it('KnobEditsApplyToNextTurnNotCurrent', async () =>
    {
        const rig = new Rig()
        rig.Send('one')
        rig.vm.Panel.Temperature = 1.5
        rig.Notify('turn_done', {})
        await rig.FinishTurn()
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

    it('KnobSetterAssignsBeforeNotifying', () =>
    {
        // Regression: a two-way spin control re-reads the source SYNCHRONOUSLY while
        // PropertyChanged fires. The field must already hold the new value by then, or the
        // binding writes the stale value back to the control and the knob "snaps back" — looking
        // like a dead button. Reading the property inside its own change notification must see new.
        const rig = new Rig()
        let observedDuringNotify = -1
        rig.vm.Panel.PropertyChanged('Temperature').subscribe(() => { observedDuringNotify = rig.vm.Panel.Temperature })
        rig.vm.Panel.Temperature = 1.5
        expect(observedDuringNotify).toBe(1.5)
        expect(rig.vm.Panel.Temperature).toBe(1.5)
    })

    it('ToleratesUndefinedModelName', async () =>
    {
        // Defensive: if model_props ever delivers no model_name, the basename split must not throw
        // (it did: `undefined.split`). The ModelProps().then runs as a microtask, so await it.
        const rig = new Rig()
        rig.client.HandleFrame(JSON.stringify({ id: 1, result: { n_ctx: 10 } }))
        await Promise.resolve()
        await Promise.resolve()
        expect(rig.vm.Panel.ModelName).toBe('')
        // Proves SetContext ran AFTER the guarded SetModelName (it wouldn't, if the split threw).
        expect(rig.vm.Panel.ContextTotal).toBe(10)
    })

    it('WindowFullSurfacesWarning', async () =>
    {
        const rig = new Rig()
        rig.Send('one')
        rig.Notify('turn_started', { prompt: 'p', prompt_tokens: 5000, n_ctx: 4096 })
        rig.Notify('turn_rejected', { needed: 5000, total: 4096 })
        expect(rig.vm.Panel.Warning).toContain('5000')
        expect(rig.vm.Panel.Warning).toContain('4096')
        expect(rig.vm.Panel.HasWarning).toBe(true)
        expect(rig.vm.Transcript.ToArray().length).toBe(2)
        rig.Notify('turn_done', {})
        await rig.FinishTurn()
        rig.Send('two')
        expect(rig.vm.Panel.Warning).toBe('')
    })

    it('TracksPromptContextAndLogprobsPerTurn', async () =>
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
        await rig.FinishTurn()
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

    it('SurfacesSamplingFailureOnPanel', async () =>
    {
        const rig = new Rig()
        rig.Send('one')
        expect(rig.Methods().filter(m => m === 'model_props').length).toBe(1)
        rig.client.HandleFrame(JSON.stringify({ id: rig.IdOf('set_sampling'), error: { code: 1, message: 'bad config' } }))
        await rig.Flush()
        expect(rig.vm.Panel.Warning).toContain('bad config')
    })

    it('RoundsIntegerKnobsInConfig', () =>
    {
        const rig = new Rig()
        rig.vm.Panel.TopK = 40.0000001
        rig.vm.Panel.Seed = 7.2
        rig.vm.Panel.MaxTokens = 255.6
        rig.vm.Panel.Temperature = 0.85
        const c = rig.vm.Panel.Config
        expect([c.top_k, c.seed, c.max_tokens, c.temperature]).toEqual([40, 7, 256, 0.85])
    })

    it('ToleratesChunkWithoutTokens', () =>
    {
        const rig = new Rig()
        rig.Send('one')
        expect(() => rig.Notify('turn_chunk', { text: 'a', stop: false })).not.toThrow()
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

    it('LateModelPropsFromOldClientIsIgnoredAfterRebind', async () =>
    {
        const rig = new Rig()
        const socketB = new FakeSocket()
        const clientB = new RuntimeClient(() => socketB)
        clientB.Connect('ws://y')
        socketB.Open()
        rig.vm.Panel.Bind(clientB)
        rig.client.HandleFrame(JSON.stringify({ id: 1, result: { n_ctx: 111, model_name: 'A.gguf' } }))
        await Promise.resolve()
        await Promise.resolve()
        expect(rig.vm.Panel.ModelName).toBe('')
        expect(rig.vm.Panel.ContextTotal).toBe(0)
        clientB.HandleFrame(JSON.stringify({ id: 1, result: { n_ctx: 222, model_name: 'B.gguf' } }))
        await Promise.resolve()
        await Promise.resolve()
        expect(rig.vm.Panel.ModelName).toBe('B.gguf')
        expect(rig.vm.Panel.ContextTotal).toBe(222)
    })
})
