import { describe, expect, it } from 'vitest'
import type { IServiceProvider } from '@pragmatic-tech-ai/mural/runtime'
import { ContentHostService } from '@pragmatic-tech-ai/mural/framework/shell/services/content-host-service.js'
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
    constructor(private readonly client: SupervisorClient | undefined, private readonly host: FakeContentHost)
    {
    }

    public get<T>(token: unknown): T | undefined
    {
        return token === SupervisorClient.Key ? (this.client as unknown as T | undefined) : undefined
    }

    public getRequired<T>(token: unknown): T
    {
        if (token === ContentHostService.Key)
        {
            return this.host as unknown as T
        }
        throw new Error('unexpected token')
    }

    public has(token: unknown): boolean
    {
        return token === SupervisorClient.Key && this.client !== undefined
    }
}

class Rig
{
    public readonly log: string[] = []
    public readonly clients: SpyClient[] = []
    public readonly socket = new FakeSocket()
    public readonly host = new FakeContentHost()
    public readonly chat = new ChatViewModel()
    public readonly service: RuntimeManagementService

    constructor(withClient = true)
    {
        let supervisor: SupervisorClient | undefined
        if (withClient)
        {
            supervisor = new SupervisorClient(() => this.socket)
            supervisor.Connect('ws://supervisor')
            this.socket.Open()
        }
        const bind = this.chat.Bind.bind(this.chat)
        this.chat.Bind = (client) =>
        {
            this.log.push(client === null ? 'bind:null' : `bind${(client as SpyClient).Id}`)
            bind(client)
        }
        this.service = new RuntimeManagementService(
            new StubProvider(supervisor, this.host) as unknown as IServiceProvider,
            () => this.chat,
            () =>
            {
                const c = new SpyClient(this.clients.length + 1, this.log)
                this.clients.push(c)
                return c
            })
    }
}

describe('RuntimeManagementService', () =>
{
    it('with_client_views_chat_and_creates_picker', () =>
    {
        const rig = new Rig()
        expect(rig.host.viewed).toEqual([rig.chat])
        expect(rig.service.Picker).not.toBeNull()
        const sent = JSON.stringify(rig.socket.sent)
        expect(sent).toContain('list_runtimes')
        expect(sent).toContain('list_models')
        expect(sent).toContain('list_harnesses')
    })

    it('without_client_does_nothing', () =>
    {
        const rig = new Rig(false)
        expect(rig.host.viewed).toEqual([])
        expect(rig.service.Picker).toBeNull()
    })

    it('ready_binds_fresh_client_to_chat', () =>
    {
        const rig = new Rig()
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
                rig.service.Picker!.OnReady.Raise('ws://a')
        rig.service.Picker!.OnReady.Raise('ws://b')
        expect(rig.clients).toHaveLength(2)
        expect(rig.log.indexOf('disconnect1')).toBeGreaterThanOrEqual(0)
        expect(rig.log.indexOf('disconnect1')).toBeLessThan(rig.log.indexOf('connect2:ws://b'))
        expect(rig.log.indexOf('disconnect1')).toBeLessThan(rig.log.indexOf('bind2'))
    })

    it('cleared_unbinds_chat_and_disconnects', () =>
    {
        const rig = new Rig()
                rig.service.Picker!.OnReady.Raise('ws://a')
        rig.log.length = 0
        rig.service.Picker!.OnCleared.Raise(undefined)
        expect(rig.chat.CanChat).toBe(false)
        expect(rig.log).toEqual(['bind:null', 'disconnect1'])
    })
})
