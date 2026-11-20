import { describe, expect, it } from 'vitest'
import type { IServiceProvider } from '@pragmatic-tech-ai/mural/runtime'
import { RuntimeManagementService } from './RuntimeManagementService'
import { ChatViewModel } from './ChatViewModel'
import { HarnessClient } from './HarnessClient'
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

class Rig
{
    public readonly log: string[] = []
    public readonly clients: SpyClient[] = []
    public readonly socket = new FakeSocket()
    public readonly service: RuntimeManagementService
    public readonly chat = new ChatViewModel()

    constructor()
    {
        this.service = new RuntimeManagementService({} as IServiceProvider, () =>
        {
            const c = new SpyClient(this.clients.length + 1, this.log)
            this.clients.push(c)
            return c
        })
        const bind = this.chat.Bind.bind(this.chat)
        this.chat.Bind = (client) =>
        {
            this.log.push(client === null ? 'bind:null' : `bind${(client as SpyClient).Id}`)
            bind(client)
        }
    }

    public Attach(): void
    {
        const supervisor = new SupervisorClient(() => this.socket)
        supervisor.Connect('ws://supervisor')
        this.socket.Open()
        this.service.Attach(supervisor, this.chat)
    }
}

describe('RuntimeManagementService', () =>
{
    it('picker_is_null_before_attach', () =>
    {
        expect(new Rig().service.Picker).toBeNull()
    })

    it('attach_creates_picker_refreshes_and_raises_change', () =>
    {
        const rig = new Rig()
        const changed: string[] = []
        rig.service.PropertyChanged('Picker').subscribe(e => changed.push(e.property))
        rig.Attach()
        expect(rig.service.Picker).not.toBeNull()
        const sent = JSON.stringify(rig.socket.sent)
        expect(sent).toContain('list_runtimes')
        expect(sent).toContain('list_models')
        expect(sent).toContain('list_harnesses')
        expect(changed).toContain('Picker')
    })

    it('ready_binds_fresh_client_to_chat', () =>
    {
        const rig = new Rig()
        rig.Attach()
        expect(rig.chat.CanChat).toBe(false)
        rig.service.Picker!.OnReady.Raise('ws://a')
        expect(rig.clients).toHaveLength(1)
        expect(rig.log[0]).toBe('connect1:ws://a')
        expect(rig.log).toContain('bind1')
        expect(rig.chat.CanChat).toBe(true)
    })

    it('second_ready_disconnects_previous_before_binding_new', () =>
    {
        const rig = new Rig()
        rig.Attach()
        rig.service.Picker!.OnReady.Raise('ws://a')
        rig.service.Picker!.OnReady.Raise('ws://b')
        expect(rig.clients).toHaveLength(2)
        expect(rig.log.indexOf('disconnect1')).toBeGreaterThanOrEqual(0)
        expect(rig.log.indexOf('disconnect1')).toBeLessThan(rig.log.indexOf('connect2:ws://b'))
        expect(rig.log.indexOf('disconnect1')).toBeLessThan(rig.log.indexOf('bind2'))
    })

    it('second_attach_detaches_stale_picker', () =>
    {
        const rig = new Rig()
        rig.Attach()
        const first = rig.service.Picker!
        first.OnReady.Raise('ws://a')
        rig.Attach()
        expect(rig.service.Picker).not.toBe(first)
        rig.log.length = 0
        first.OnReady.Raise('ws://stale')
        expect(rig.clients).toHaveLength(1)
        expect(rig.log).toEqual([])
    })

    it('cleared_unbinds_chat_and_disconnects', () =>
    {
        const rig = new Rig()
        rig.Attach()
        rig.service.Picker!.OnReady.Raise('ws://a')
        rig.log.length = 0
        rig.service.Picker!.OnCleared.Raise(undefined)
        expect(rig.chat.CanChat).toBe(false)
        expect(rig.log).toEqual(['bind:null', 'disconnect1'])
    })
})
