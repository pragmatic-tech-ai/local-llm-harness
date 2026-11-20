import { describe, expect, it } from 'vitest'
import type { IServiceProvider } from '@pragmatic-tech-ai/mural/runtime'
import { ContentHostService } from '@pragmatic-tech-ai/mural/framework/shell/services/content-host-service.js'
import { RuntimeManagementService } from './RuntimeManagementService'
import { ChatViewModel } from './ChatViewModel'
import { HarnessClient } from './HarnessClient'
import { HarnessPickerViewModel } from './HarnessPickerViewModel'
import { SupervisorClient } from './SupervisorClient'
import { FakeSocket } from './FakeSocket'
import { ApplicationSettings } from '@pragmatic-tech-ai/mural/framework'
import { Signal } from '@pragmatic-tech-ai/mural/runtime'

class SpySupervisor extends SupervisorClient
{
    public readonly Socket = new FakeSocket()

    constructor(public readonly Id: number, private readonly log: string[], private readonly fails: () => boolean = () => false)
    {
        super(() => this.Socket)
    }

    public override Connect(url: string): void
    {
        this.log.push(`sconnect${this.Id}:${url}`)
        if (this.fails())
        {
            throw new Error('bad url')
        }
        super.Connect(url)
        this.Socket.Open()
    }

    public override Disconnect(): void
    {
        this.log.push(`sdisconnect${this.Id}`)
        super.Disconnect()
    }
}

class FakeSettings
{
    private readonly signals = new Map<string, Signal<unknown>>()

    constructor(public Value: unknown)
    {
    }

    public Get(): unknown
    {
        return this.Value
    }

    public Changed(key: string): Signal<unknown>
    {
        let signal = this.signals.get(key)
        if (signal === undefined)
        {
            signal = new Signal<unknown>()
            this.signals.set(key, signal)
        }
        return signal
    }

    public Change(value: unknown, key: string = 'supervisor.url'): void
    {
        this.Value = value
        this.Changed(key).emit(undefined)
    }
}

class SpyClient extends HarnessClient
{
    constructor(public readonly Id: number, private readonly log: string[])
    {
        super(() => new FakeSocket())
    }

    public override Connect(url: string): void
    {
        this.log.push(`connect${this.Id}:${url}`)
        super.Connect(url)
    }

    public override Disconnect(): void
    {
        this.log.push(`disconnect${this.Id}`)
        super.Disconnect()
    }
}

class FakeContentHost
{
    public readonly viewed: unknown[] = []

    public View(content: unknown): void
    {
        this.viewed.push(content)
    }
}

class StubProvider
{
    constructor(private readonly host: FakeContentHost, private readonly settings: FakeSettings)
    {
    }

    public getRequired<T>(token: unknown): T
    {
        if (token === ContentHostService.Key)
        {
            return this.host as unknown as T
        }
        if (token === ApplicationSettings.Key)
        {
            return this.settings as unknown as T
        }
        throw new Error('unexpected token')
    }
}

class Rig
{
    public readonly log: string[] = []
    public readonly clients: SpyClient[] = []
    public readonly socket = new FakeSocket()
    public readonly host = new FakeContentHost()
    public readonly chat = new ChatViewModel()
    public readonly picker = new HarnessPickerViewModel()
    public readonly supervisor: SupervisorClient
    public readonly supervisors: SpySupervisor[] = []
    public readonly settings: FakeSettings
    public ThrowOnConnect = false
    public readonly service: RuntimeManagementService

    constructor(url: string = '')
    {
        this.settings = new FakeSettings(url)
        this.supervisor = new SupervisorClient(() => this.socket)
        this.supervisor.Connect('ws://supervisor')
        this.socket.Open()
        const bind = this.chat.Bind.bind(this.chat)
        this.chat.Bind = (client) =>
        {
            this.log.push(client === null ? 'bind:null' : `bind${(client as SpyClient).Id}`)
            bind(client)
        }
        this.service = new RuntimeManagementService(
            new StubProvider(this.host, this.settings) as unknown as IServiceProvider,
            () => this.chat,
            () => this.picker,
            () =>
            {
                const c = new SpyClient(this.clients.length + 1, this.log)
                this.clients.push(c)
                return c
            },
            () =>
            {
                const c = new SpySupervisor(this.supervisors.length + 1, this.log, () => this.ThrowOnConnect)
                this.supervisors.push(c)
                return c
            })
        if (url === '')
        {
            // The empty-url ctor path unbinds the (already unbound) chat; drop that noise.
            this.log.length = 0
        }
    }

    /** Answer the attached supervisor's three discovery requests so a ready harness (and a runtime) is adopted. */
    public async DriveReady(supervisor: SpySupervisor): Promise<void>
    {
        const ready = { id: 'h1', runtime: 'r', model: 'm', state: 'ready', url: 'ws://h' }
        supervisor.HandleFrame(JSON.stringify({ id: 1, result: [{ name: 'r', kind: 'http', available: true }] }))
        supervisor.HandleFrame(JSON.stringify({ id: 2, result: [{ name: 'm', runtimes: ['r'], size_hint: '' }] }))
        supervisor.HandleFrame(JSON.stringify({ id: 3, result: [ready] }))
        await new Promise(r => setTimeout(r, 0))
    }

    public Attach(): void
    {
        this.service.Attach(this.supervisor)
    }
}

describe('RuntimeManagementService', () =>
{
    it('ctor_views_chat_and_exposes_stable_picker', () =>
    {
        const rig = new Rig()
        expect(rig.host.viewed).toEqual([rig.chat])
        expect(rig.service.Picker).toBe(rig.picker)
    })

    it('inert_until_attached', () =>
    {
        const rig = new Rig()
        rig.service.Picker.Refresh()
        // Without the client guard, Refresh would dereference a null client and the
        // caught failure would surface as an error status; the guard keeps it inert.
        expect(rig.socket.sent).toHaveLength(0)
        expect(rig.service.Picker.State).toBe('stopped')
        expect(rig.service.Picker.StatusText).toBe('Stopped')
    })

    it('attach_binds_connection_and_refreshes', () =>
    {
        const rig = new Rig()
        rig.Attach()
        const sent = JSON.stringify(rig.socket.sent)
        expect(sent).toContain('list_runtimes')
        expect(sent).toContain('list_models')
        expect(sent).toContain('list_harnesses')
    })

    it('ready_binds_fresh_client_to_chat', () =>
    {
        const rig = new Rig()
        rig.Attach()
        expect(rig.chat.CanChat).toBe(false)
        rig.picker.OnReady.Raise('ws://a')
        expect(rig.clients).toHaveLength(1)
        expect(rig.log[0]).toBe('connect1:ws://a')
        expect(rig.log).toContain('bind1')
        expect(rig.chat.CanChat).toBe(true)
    })

    it('second_ready_disconnects_previous_before_binding_new', () =>
    {
        const rig = new Rig()
        rig.Attach()
        rig.picker.OnReady.Raise('ws://a')
        rig.picker.OnReady.Raise('ws://b')
        expect(rig.clients).toHaveLength(2)
        expect(rig.log.indexOf('disconnect1')).toBeGreaterThanOrEqual(0)
        expect(rig.log.indexOf('disconnect1')).toBeLessThan(rig.log.indexOf('connect2:ws://b'))
        expect(rig.log.indexOf('disconnect1')).toBeLessThan(rig.log.indexOf('bind2'))
    })

    it('cleared_unbinds_chat_and_disconnects', () =>
    {
        const rig = new Rig()
        rig.Attach()
        rig.picker.OnReady.Raise('ws://a')
        rig.log.length = 0
        rig.picker.OnCleared.Raise(undefined)
        expect(rig.chat.CanChat).toBe(false)
        expect(rig.log).toEqual(['bind:null', 'disconnect1'])
    })

    it('bind_backend_binds_chat_without_supervisor', () =>
    {
        const rig = new Rig()
        const client = new SpyClient(9, rig.log)
        client.Connect('ws://direct')
        rig.service.BindBackend(client)
        expect(rig.chat.CanChat).toBe(true)
        expect(rig.log).toContain('bind9')
    })

    it('empty_url_creates_no_supervisor', () =>
    {
        const rig = new Rig('')
        expect(rig.supervisors).toHaveLength(0)
        expect(rig.service.Picker.State).toBe('stopped')
    })

    it('nonempty_url_connects_and_attaches', () =>
    {
        const rig = new Rig('ws://s1')
        expect(rig.supervisors).toHaveLength(1)
        expect(rig.log).toContain('sconnect1:ws://s1')
        expect(JSON.stringify(rig.supervisors[0].Socket.sent)).toContain('list_runtimes')
    })

    it('change_disconnects_old_before_connecting_new_and_reattaches', () =>
    {
        const rig = new Rig('ws://s1')
        rig.settings.Change('ws://s2')
        expect(rig.supervisors).toHaveLength(2)
        expect(rig.log.indexOf('sdisconnect1')).toBeGreaterThanOrEqual(0)
        expect(rig.log.indexOf('sdisconnect1')).toBeLessThan(rig.log.indexOf('sconnect2:ws://s2'))
        expect(JSON.stringify(rig.supervisors[1].Socket.sent)).toContain('list_harnesses')
    })

    it('change_to_empty_disconnects_and_unbinds_chat', () =>
    {
        const rig = new Rig('ws://s1')
        rig.picker.OnReady.Raise('ws://a')
        rig.log.length = 0
        rig.settings.Change('  ')
        expect(rig.log).toContain('sdisconnect1')
        expect(rig.log).toContain('bind:null')
        expect(rig.chat.CanChat).toBe(false)
        expect(rig.supervisors).toHaveLength(1)
    })

    it('rapid_second_change_disconnects_first_before_connecting_second', () =>
    {
        const rig = new Rig('')
        rig.settings.Change('ws://a')
        rig.settings.Change('ws://b')
        expect(rig.supervisors).toHaveLength(2)
        expect(rig.log.indexOf('sdisconnect1')).toBeLessThan(rig.log.indexOf('sconnect2:ws://b'))
    })

    it('valid_to_valid_change_unbinds_old_harness_chat', async () =>
    {
        const rig = new Rig('ws://s1')
        await rig.DriveReady(rig.supervisors[0])
        expect(rig.picker.State).toBe('ready')
        expect(rig.picker.CanStop).toBe(true)
        expect(rig.picker.Runtimes.Count).toBe(1)
        expect(rig.chat.CanChat).toBe(true)
        rig.settings.Change('ws://s2')
        expect(rig.chat.CanChat).toBe(false)
        expect(rig.log).toContain('bind:null')
        expect(rig.picker.State).toBe('stopped')
        expect(rig.picker.CanStop).toBe(false)
        expect(rig.picker.CanStart).toBe(false)
        expect(rig.picker.Runtimes.Count).toBe(0)
        expect(rig.log.indexOf('disconnect1')).toBeGreaterThanOrEqual(0)
    })

    it('change_to_empty_leaves_picker_inert', async () =>
    {
        const rig = new Rig('ws://s1')
        await rig.DriveReady(rig.supervisors[0])
        expect(rig.picker.State).toBe('ready')
        expect(rig.chat.CanChat).toBe(true)
        rig.settings.Change('')
        expect(rig.picker.Runtimes.Count).toBe(0)
        expect(rig.picker.State).toBe('stopped')
        expect(rig.picker.CanStop).toBe(false)
        expect(rig.chat.CanChat).toBe(false)
    })

    it('other_key_change_does_not_reconnect', () =>
    {
        const rig = new Rig('ws://s1')
        const before = [...rig.log]
        rig.settings.Change('ws://other', 'some.other.key')
        expect(rig.supervisors).toHaveLength(1)
        expect(rig.log).toEqual(before)
    })

    it('throwing_connect_goes_inert', () =>
    {
        const rig = new Rig('')
        rig.ThrowOnConnect = true
        expect(() => rig.settings.Change('bad')).not.toThrow()
        expect(rig.picker.State).toBe('stopped')
    })
})
