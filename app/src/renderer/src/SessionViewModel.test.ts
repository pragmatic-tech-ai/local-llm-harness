import { describe, expect, it } from 'vitest'
import { RuntimeClient } from './RuntimeClient'
import { FakeSocket } from './FakeSocket'
import { SessionViewModel } from './SessionViewModel'
import type { AttachedSession, Placement } from './agents'

class Rig
{
    public readonly socket = new FakeSocket()
    public readonly client = new RuntimeClient(() => this.socket)
    public readonly view = new SessionViewModel(this.client)

    constructor()
    {
        this.client.Connect('ws://runtime')
        this.socket.Open()
    }

    public static Attached(placement: Placement): AttachedSession
    {
        const summary = { id: 's1', title: 'Default #1', template_id: 'tpl', template_name: 'Default', runtime: 'llamacpp', model: 'gemma-3-4b', updated: 1, placement, busy: false }
        return {
            summary,
            snapshot: { name: 'Researcher', system_prompt: '', tools: ['read_file', 'get_time'], step_budget: 8, runtime: 'llamacpp', model: 'gemma-3-4b', n_ctx: 8192, enable_thinking: false },
            transcript: [],
            sampling: { temperature: 0.8, top_p: 0.95, top_k: 40, repeat_penalty: 1.1, seed: -1, max_tokens: 256 },
            enable_thinking: false,
            placement,
        }
    }

    public IdOf(method: string): number
    {
        const frames = this.socket.sent.map(f => JSON.parse(f) as { id: number, method: string })
        return frames.filter(f => f.method === method).pop()?.id ?? -1
    }

    public Frame(method: string): { id: number, method: string, params: unknown } | undefined
    {
        return this.socket.sent.map(f => JSON.parse(f)).filter(f => f.method === method).pop()
    }

    public async Respond(method: string, result: unknown): Promise<void>
    {
        this.client.HandleFrame(JSON.stringify({ id: this.IdOf(method), result }))
        await new Promise(r => setTimeout(r, 0))
    }

    public Notify(method: string, params: unknown): void
    {
        this.client.HandleFrame(JSON.stringify({ method, params }))
    }

    public async Show(placement: Placement): Promise<void>
    {
        const done = this.view.Attach('s1')
        await this.Respond('attach_session', Rig.Attached(placement))
        await done
    }
}

describe('SessionViewModel', () =>
{
    it('load_button_label_names_eviction', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: 'needs_load', host: 'local', evicts: 'gemma-4-12B-it-QAT-Q4_0' })
        expect(rig.view.CanLoad).toBe(true)
        expect(rig.view.LoadLabel).toBe('Load model (evicts gemma-4-12B-it-QAT-Q4_0)')
        expect(rig.view.PlacementText).toBe('model not loaded')

        const free = new Rig()
        await free.Show({ state: 'needs_load', host: 'local', evicts: null })
        expect(free.view.LoadLabel).toBe('Load model')
    })

    it('header_shows_template_model_host_and_tools', async () =>
    {
        const rig = new Rig()
        expect(rig.view.HasSession).toBe(false)
        expect(rig.view.IsEmpty).toBe(true)
        await rig.Show({ state: 'ready', host: 'local' })
        expect(rig.view.HasSession).toBe(true)
        expect(rig.view.HeaderText).toBe('Researcher · gemma-3-4b @ local')
        expect(rig.view.PlacementText).toBe('live on local')
        expect(rig.view.ToolsText).toBe('tools: read_file, get_time')
        expect(rig.view.CanLoad).toBe(false)
        expect(rig.view.CanUnload).toBe(true)
        expect(rig.view.Chat.CanChat).toBe(true)
    })

    it('unavailable_shows_the_reason', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: 'unavailable', reason: 'no host serves gemma-9' })
        expect(rig.view.PlacementText).toBe('unavailable: no host serves gemma-9')
        expect(rig.view.HeaderText).toBe('Researcher · gemma-3-4b')
        expect(rig.view.CanLoad).toBe(false)
    })

    it('load_command_loads_and_shows_ready', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: 'needs_load', host: 'local', evicts: 'gemma-4' })
        rig.view.LoadCommand.Execute(undefined)
        expect(rig.Frame('load_session_model')?.params).toEqual({ id: 's1' })
        expect(rig.view.PlacementText).toBe('loading model…')
        expect(rig.view.CanLoad).toBe(false)
        const propsBefore = rig.socket.sent.filter(f => JSON.parse(f).method === 'model_props').length
        await rig.Respond('load_session_model', { state: 'ready', host: 'local' })
        expect(rig.view.PlacementText).toBe('live on local')
        expect(rig.socket.sent.filter(f => JSON.parse(f).method === 'model_props').length).toBe(propsBefore + 1)
    })

    it('load_failure_is_shown', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: 'needs_load', host: 'local', evicts: null })
        rig.view.LoadCommand.Execute(undefined)
        rig.client.HandleFrame(JSON.stringify({ id: rig.IdOf('load_session_model'), error: { code: -32603, message: 'out of memory' } }))
        await new Promise(r => setTimeout(r, 0))
        expect(rig.view.PlacementText).toBe('load failed: out of memory')
        expect(rig.view.CanLoad).toBe(true)
    })

    it('unload_command_and_placement_changed', async () =>
    {
        const rig = new Rig()
        await rig.Show({ state: 'ready', host: 'local' })
        rig.view.UnloadCommand.Execute(undefined)
        expect(rig.Frame('unload_session_model')?.params).toEqual({ id: 's1' })
        rig.Notify('placement_changed', { id: 's1', placement: { state: 'needs_load', host: 'local', evicts: null } })
        expect(rig.view.CanLoad).toBe(true)
        rig.Notify('placement_changed', { id: 'other', placement: { state: 'ready', host: 'local' } })
        expect(rig.view.CanLoad).toBe(true)
    })

    it('seeds_panel_from_attached_session', async () =>
    {
        const rig = new Rig()
        const done = rig.view.Attach('s1')
        const attached = Rig.Attached({ state: 'ready', host: 'local' })
        await rig.Respond('attach_session', { ...attached, sampling: { ...attached.sampling, temperature: 0.3 }, enable_thinking: true })
        await done
        expect(rig.view.Panel.Temperature).toBe(0.3)
        expect(rig.view.Panel.EnableThinking).toBe(true)
    })

    it('stale_attach_reply_is_dropped', async () =>
    {
        const rig = new Rig()
        const first = rig.view.Attach('s1')
        const firstId = rig.IdOf('attach_session')
        const second = rig.view.Attach('s2')
        const attached = Rig.Attached({ state: 'ready', host: 'local' })
        rig.client.HandleFrame(JSON.stringify({ id: firstId, result: attached }))
        await first
        expect(rig.view.HasSession).toBe(false)
        await rig.Respond('attach_session', { ...attached, snapshot: { ...attached.snapshot, name: 'Second' } })
        await second
        expect(rig.view.ShownId).toBe('s2')
        expect(rig.view.HeaderText).toBe('Second · gemma-3-4b @ local')
    })
})
