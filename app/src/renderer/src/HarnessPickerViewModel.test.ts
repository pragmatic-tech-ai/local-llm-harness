import { describe, expect, it } from 'vitest'
import { HarnessPickerViewModel } from './HarnessPickerViewModel'
import { SupervisorClient, type HarnessInfo, type ModelInfo, type RuntimeInfo } from './SupervisorClient'
import { FakeSocket } from './FakeSocket'

class Rig
{
    public readonly socket = new FakeSocket()
    public readonly client = new SupervisorClient(() => this.socket)
    public readonly picker: HarnessPickerViewModel
    public readonly ready: string[] = []
    public cleared = 0

    constructor()
    {
        this.client.Connect('ws://x')
        this.socket.Open()
        this.picker = new HarnessPickerViewModel(this.client)
        this.picker.OnReady.Subscribe(u => this.ready.push(u))
        this.picker.OnCleared.Subscribe(() => this.cleared++)
    }

    public Reply(id: number, result: unknown): void
    {
        this.client.HandleFrame(JSON.stringify({ id, result }))
    }

    public Fail(id: number, message: string): void
    {
        this.client.HandleFrame(JSON.stringify({ id, error: { message } }))
    }

    public Method(i: number): string
    {
        return JSON.parse(this.socket.sent[i]).method
    }

    public async Flush(): Promise<void>
    {
        await new Promise(r => setTimeout(r, 0))
    }

    public async Populate(): Promise<void>
    {
        const runtimes: RuntimeInfo[] = [{ name: 'llamacpp', kind: 'http', available: true }]
        const models: ModelInfo[] = [{ name: 'gemma', runtimes: ['llamacpp'], size_hint: '4GB' }]
        this.picker.Refresh()
        this.Reply(1, runtimes)
        this.Reply(2, models)
        this.Reply(3, [])
        await this.Flush()
        this.picker.SelectedRuntime = runtimes[0]
        this.picker.SelectedModel = models[0]
    }
}

describe('HarnessPickerViewModel', () =>
{
    it('refresh_populates_runtimes_and_models', async () =>
    {
        const rig = new Rig()
        await rig.Populate()
        expect(rig.picker.Runtimes.Count).toBe(1)
        expect(rig.picker.Models.Count).toBe(1)
        expect(rig.picker.State).toBe('stopped')
    })

    it('unbind_resets_to_inert_state', async () =>
    {
        const rig = new Rig()
        await rig.Populate()
        rig.picker.Unbind()
        expect(rig.picker.Runtimes.Count).toBe(0)
        expect(rig.picker.Models.Count).toBe(0)
        expect(rig.picker.SelectedRuntime).toBeNull()
        expect(rig.picker.State).toBe('stopped')
        expect(rig.picker.CanStart).toBe(false)
        const sent = rig.socket.sent.length
        rig.picker.StartCommand.Execute()
        await rig.Flush()
        expect(rig.socket.sent).toHaveLength(sent)
    })

    it('start_disabled_without_selection', () =>
    {
        const rig = new Rig()
        expect(rig.picker.CanStart).toBe(false)
        expect(rig.picker.StartCommand.CanExecute()).toBe(false)
    })

    it('start_sets_loading_then_ready_and_raises_url', async () =>
    {
        const rig = new Rig()
        await rig.Populate()
        expect(rig.picker.CanStart).toBe(true)
        rig.picker.StartCommand.Execute()
        expect(rig.picker.State).toBe('loading')
        expect(rig.picker.CanStart).toBe(false)
        expect(rig.picker.CanStop).toBe(true)
        const info: HarnessInfo = { id: 'h1', runtime: 'llamacpp', model: 'gemma', state: 'ready', url: 'ws://h1' }
        // harness_changed first, then the response: OnReady must fire exactly once.
        rig.client.HandleFrame(JSON.stringify({ method: 'harness_changed', params: info }))
        rig.Reply(4, info)
        await rig.Flush()
        expect(rig.picker.State).toBe('ready')
        expect(rig.ready).toEqual(['ws://h1'])
    })

    it('stop_clears_and_raises_cleared', async () =>
    {
        const rig = new Rig()
        await rig.Populate()
        rig.picker.StartCommand.Execute()
        rig.Reply(4, { id: 'h1', runtime: 'llamacpp', model: 'gemma', state: 'ready', url: 'ws://h1' })
        await rig.Flush()
        rig.picker.StopCommand.Execute()
        expect(rig.Method(4)).toBe('stop')
        expect(rig.picker.State).toBe('stopped')
        expect(rig.cleared).toBe(1)
    })

    it('start_reply_before_harness_changed_raises_ready_once', async () =>
    {
        const rig = new Rig()
        await rig.Populate()
        rig.picker.StartCommand.Execute()
        const info: HarnessInfo = { id: 'h1', runtime: 'llamacpp', model: 'gemma', state: 'ready', url: 'ws://h1' }
        rig.Reply(4, info)
        await rig.Flush()
        rig.client.HandleFrame(JSON.stringify({ method: 'harness_changed', params: info }))
        expect(rig.picker.State).toBe('ready')
        expect(rig.ready).toEqual(['ws://h1'])
    })

    it('restart_after_failed_reaches_ready', async () =>
    {
        const rig = new Rig()
        await rig.Populate()
        rig.picker.StartCommand.Execute()
        rig.client.HandleFrame(JSON.stringify({ method: 'harness_changed', params: { id: 'h1', runtime: 'llamacpp', model: 'gemma', state: 'failed' } }))
        rig.Fail(4, 'boom')
        await rig.Flush()
        rig.picker.StartCommand.Execute()
        const info: HarnessInfo = { id: 'h2', runtime: 'llamacpp', model: 'gemma', state: 'ready', url: 'ws://h2' }
        rig.client.HandleFrame(JSON.stringify({ method: 'harness_changed', params: info }))
        rig.Reply(5, info)
        await rig.Flush()
        expect(rig.picker.State).toBe('ready')
        expect(rig.ready).toEqual(['ws://h2'])
    })

    it('restart_after_ready_reaches_ready', async () =>
    {
        const rig = new Rig()
        await rig.Populate()
        rig.picker.StartCommand.Execute()
        rig.Reply(4, { id: 'h1', runtime: 'llamacpp', model: 'gemma', state: 'ready', url: 'ws://h1' })
        await rig.Flush()
        rig.picker.StartCommand.Execute()
        expect(rig.picker.State).toBe('loading')
        const info: HarnessInfo = { id: 'h2', runtime: 'llamacpp', model: 'gemma', state: 'ready', url: 'ws://h2' }
        rig.Reply(5, info)
        await rig.Flush()
        expect(rig.picker.State).toBe('ready')
        expect(rig.ready).toEqual(['ws://h1', 'ws://h2'])
    })

    it('late_event_from_replaced_harness_is_ignored', async () =>
    {
        const rig = new Rig()
        await rig.Populate()
        rig.picker.StartCommand.Execute()
        rig.Reply(4, { id: 'h1', runtime: 'llamacpp', model: 'gemma', state: 'ready', url: 'ws://h1' })
        await rig.Flush()
        rig.picker.StartCommand.Execute()
        rig.client.HandleFrame(JSON.stringify({ method: 'harness_changed', params: { id: 'h1', runtime: 'llamacpp', model: 'gemma', state: 'stopped' } }))
        expect(rig.picker.State).toBe('loading')
        rig.Reply(5, { id: 'h2', runtime: 'llamacpp', model: 'gemma', state: 'ready', url: 'ws://h2' })
        await rig.Flush()
        expect(rig.picker.State).toBe('ready')
        expect(rig.ready).toEqual(['ws://h1', 'ws://h2'])
    })

    it('stop_during_loading_is_final', async () =>
    {
        const rig = new Rig()
        await rig.Populate()
        rig.picker.StartCommand.Execute()
        rig.picker.StopCommand.Execute()
        expect(rig.picker.State).toBe('stopped')
        const info: HarnessInfo = { id: 'h1', runtime: 'llamacpp', model: 'gemma', state: 'ready', url: 'ws://h1' }
        rig.client.HandleFrame(JSON.stringify({ method: 'harness_changed', params: info }))
        rig.Reply(4, info)
        await rig.Flush()
        expect(rig.picker.State).toBe('stopped')
        expect(rig.ready).toEqual([])
        expect(rig.Method(rig.socket.sent.length - 1)).toBe('stop')
    })

    it('harness_changed_to_failed_updates_state', async () =>
    {
        const rig = new Rig()
        await rig.Populate()
        rig.picker.StartCommand.Execute()
        rig.client.HandleFrame(JSON.stringify({ method: 'harness_changed', params: { id: 'h1', runtime: 'llamacpp', model: 'gemma', state: 'failed' } }))
        expect(rig.picker.State).toBe('failed')
        expect(rig.picker.StatusText).not.toBe('')
        expect(rig.cleared).toBe(1)
    })

    it('incompatible_start_surfaces_error_text', async () =>
    {
        const rig = new Rig()
        await rig.Populate()
        rig.picker.StartCommand.Execute()
        rig.Fail(4, 'model gemma incompatible with runtime llamacpp')
        await rig.Flush()
        expect(rig.picker.State).not.toBe('ready')
        expect(rig.picker.StatusText).toContain('incompatible')
        expect(rig.ready).toEqual([])
    })
})
