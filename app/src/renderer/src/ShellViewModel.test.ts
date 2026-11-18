import { describe, expect, it } from 'vitest'
import { ShellViewModel } from './ShellViewModel'
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
    public readonly shell: ShellViewModel

    constructor()
    {
        const socket = new FakeSocket()
        const supervisor = new SupervisorClient(() => socket)
        this.shell = new ShellViewModel(supervisor, () =>
        {
            const c = new SpyClient(this.clients.length + 1, this.log)
            this.clients.push(c)
            return c
        })
    }
}

describe('ShellViewModel', () =>
{
    it('ready_binds_fresh_client_to_chat', () =>
    {
        const rig = new Rig()
        expect(rig.shell.Chat.CanChat).toBe(false)
        rig.shell.Picker.OnReady.Raise('ws://a')
        expect(rig.clients).toHaveLength(1)
        expect(rig.log[0]).toBe('connect1:ws://a')
        expect(rig.shell.Chat.CanChat).toBe(true)
    })

    it('switch_disconnects_previous_client_then_binds_new', () =>
    {
        const rig = new Rig()
        rig.shell.Picker.OnReady.Raise('ws://a')
        rig.shell.Picker.OnReady.Raise('ws://b')
        expect(rig.clients).toHaveLength(2)
        expect(rig.log.indexOf('disconnect1')).toBeGreaterThanOrEqual(0)
        expect(rig.log.indexOf('disconnect1')).toBeLessThan(rig.log.indexOf('connect2:ws://b'))
        expect(rig.shell.Chat.CanChat).toBe(true)
    })

    it('cleared_unbinds_chat', () =>
    {
        const rig = new Rig()
        rig.shell.Picker.OnReady.Raise('ws://a')
        rig.log.length = 0
        rig.shell.Picker.OnCleared.Raise(undefined)
        expect(rig.shell.Chat.CanChat).toBe(false)
        expect(rig.log).toEqual(['disconnect1'])
    })
})
