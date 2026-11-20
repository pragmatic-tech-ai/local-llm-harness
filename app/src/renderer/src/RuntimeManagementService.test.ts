import { describe, expect, it } from 'vitest'
import type { IServiceProvider } from '@pragmatic-tech-ai/mural/runtime'
import { ContentHostService } from '@pragmatic-tech-ai/mural/framework/shell/services/content-host-service.js'
import { RuntimeManagementService } from './RuntimeManagementService'
import { ChatViewModel } from './ChatViewModel'
import { HarnessClient } from './HarnessClient'
import { HarnessPickerViewModel } from './HarnessPickerViewModel'
import { SupervisorClient } from './SupervisorClient'
import { FakeSocket } from './FakeSocket'

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
    constructor(private readonly host: FakeContentHost)
    {
    }

    public getRequired<T>(token: unknown): T
    {
        if (token === ContentHostService.Key)
        {
            return this.host as unknown as T
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
    public readonly service: RuntimeManagementService

    constructor()
    {
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
            new StubProvider(this.host) as unknown as IServiceProvider,
            () => this.chat,
            () => this.picker,
            () =>
            {
                const c = new SpyClient(this.clients.length + 1, this.log)
                this.clients.push(c)
                return c
            })
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
        expect(rig.service.Picker.StatusText).toBe('')
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
})
