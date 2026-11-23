import { afterEach, describe, expect, it, vi } from 'vitest'
import type { IServiceProvider } from '@pragmatic-tech-ai/mural/runtime'
import { Signal } from '@pragmatic-tech-ai/mural/runtime'
import { ContentHostService } from '@pragmatic-tech-ai/mural/framework/shell/services/content-host-service.js'
import { ApplicationSettings, DialogService, type DialogOptions } from '@pragmatic-tech-ai/mural/framework'
import { SessionsService } from './SessionsService'
import { RuntimeClient } from './RuntimeClient'
import { SupervisorClient } from './SupervisorClient'
import { FakeSocket } from './FakeSocket'
import { NewSessionDialogViewModel } from './NewSessionDialogViewModel'
import { PlacementState, type AttachedSession, type Placement, type SessionSummary } from './agents'
import { ConnectionState } from './ConnectionState'
import type { AssistantMessage, ToolCallMessage, UserMessage } from './messages'

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

    public Change(value: unknown): void
    {
        this.Value = value
        this.Changed('supervisor.url').emit(undefined)
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

class FakeDialogs
{
    public readonly shown: DialogOptions[] = []
    public readonly closed: unknown[] = []
    public ConfirmResult = true
    private resolve: ((value: unknown) => void) | null = null

    public Show<T>(options: DialogOptions): Promise<T | undefined>
    {
        this.shown.push(options)
        return new Promise<T | undefined>(r => { this.resolve = r as (value: unknown) => void })
    }

    public Close(result?: unknown): void
    {
        this.closed.push(result)
        this.resolve?.(result)
        this.resolve = null
    }

    public Confirm(): Promise<boolean>
    {
        return Promise.resolve(this.ConfirmResult)
    }
}

class StubProvider
{
    constructor(private readonly host: FakeContentHost, private readonly settings: FakeSettings, private readonly dialogs: FakeDialogs)
    {
    }

    public getRequired<T>(token: unknown): T
    {
        const found = this.get<T>(token)
        if (found === undefined)
        {
            throw new Error('unexpected token')
        }
        return found
    }

    public get<T>(token: unknown): T | undefined
    {
        if (token === ContentHostService.Key)
        {
            return this.host as unknown as T
        }
        if (token === ApplicationSettings.Key)
        {
            return this.settings as unknown as T
        }
        if (token === DialogService.Key)
        {
            return this.dialogs as unknown as T
        }
        return undefined
    }
}

/** A supervisor socket that is open at once and answers every request (list_harnesses) with no harnesses, unless silent. */
class SupervisorSocket extends FakeSocket
{
    constructor(private readonly silent: boolean)
    {
        super()
        this.readyState = 1
    }

    public override send(data: string): void
    {
        super.send(data)
        if (!this.silent)
        {
            const request = JSON.parse(data) as { id: number }
            this.onmessage?.({ data: JSON.stringify({ id: request.id, result: [] }) })
        }
    }
}

class Rig
{
    public readonly host = new FakeContentHost()
    public readonly dialogs = new FakeDialogs()
    public readonly settings: FakeSettings
    public readonly sockets: FakeSocket[] = []
    public readonly urls: string[] = []
    public readonly supervisorSockets: FakeSocket[] = []
    public readonly service: SessionsService
    /** When set, the supervisor never answers (discovery must time out). */
    public SupervisorSilent = false

    constructor(url: string = 'ws://hive:8778')
    {
        this.settings = new FakeSettings(url)
        this.service = new SessionsService(
            new StubProvider(this.host, this.settings, this.dialogs) as unknown as IServiceProvider,
            () => new RuntimeClient(u =>
            {
                this.urls.push(u)
                const socket = new FakeSocket()
                this.sockets.push(socket)
                return socket
            }),
            () => new SupervisorClient(() =>
            {
                const socket = new SupervisorSocket(this.SupervisorSilent)
                this.supervisorSockets.push(socket)
                return socket
            }))
    }

    public get Socket(): FakeSocket
    {
        return this.sockets[this.sockets.length - 1]
    }

    public static readonly Ready: Placement = { state: PlacementState.Ready, host: 'local' }
    public static readonly NeedsLoad: Placement = { state: PlacementState.NeedsLoad, host: 'local', evicts: null }

    public static Summary(id: string, placement: Placement = Rig.Ready): SessionSummary
    {
        return { id, title: `Default #${id}`, template_id: 'tpl', template_name: 'Default', runtime: 'llamacpp', model: 'gemma', updated: 1, placement, busy: false }
    }

    public static Attached(id: string, transcript: AttachedSession['transcript'] = []): AttachedSession
    {
        const summary = Rig.Summary(id)
        return {
            summary,
            snapshot: { name: 'Default', system_prompt: 'be brief', tools: [], step_budget: 8, runtime: 'llamacpp', model: 'gemma', n_ctx: 8192, enable_thinking: false },
            transcript,
            sampling: { temperature: 0.8, top_p: 0.95, top_k: 40, repeat_penalty: 1.1, seed: -1, max_tokens: 256 },
            enable_thinking: false,
            placement: summary.placement,
        }
    }

    public static async Flush(): Promise<void>
    {
        for (let i = 0; i < 5; i++)
        {
            await Promise.resolve()
        }
    }

    /** Lets discovery (the answering supervisor) finish, then opens the runtime socket. */
    public async Connect(): Promise<void>
    {
        await Rig.Flush()
        this.Socket.Open()
    }

    public Frames(socket: FakeSocket = this.Socket): { id: number, method: string, params: unknown }[]
    {
        return socket.sent.map(f => JSON.parse(f))
    }

    public Methods(socket: FakeSocket = this.Socket): string[]
    {
        return this.Frames(socket).map(f => f.method)
    }

    public async Respond(method: string, result: unknown): Promise<void>
    {
        const id = this.Frames().filter(f => f.method === method).pop()?.id ?? -1
        this.service.Client.HandleFrame(JSON.stringify({ id, result }))
        await Rig.Flush()
    }

    public Notify(method: string, params: unknown): void
    {
        this.service.Client.HandleFrame(JSON.stringify({ method, params }))
    }

    /** Connects, lists the given sessions and selects one, answering its attach. */
    public async ShowSession(id: string, rows: SessionSummary[], transcript: AttachedSession['transcript'] = []): Promise<void>
    {
        await this.Connect()
        await this.Respond('list_sessions', rows)
        this.service.Panel.SelectedSession = this.service.Panel.Find(id)!
        await this.Respond('attach_session', Rig.Attached(id, transcript))
    }
}

describe('SessionsService', () =>
{
    afterEach(() =>
    {
        vi.useRealTimers()
    })

    it('views_the_session_view_and_finds_the_runtime_through_the_supervisor', async () =>
    {
        const rig = new Rig()
        expect(rig.host.viewed).toEqual([rig.service.View])
        await rig.Connect()
        expect(JSON.parse(rig.supervisorSockets[0].sent[0]).method).toBe('list_harnesses')
        expect(rig.urls).toEqual(['ws://hive:8779'])
        expect(rig.supervisorSockets[0].closeCalls).toBe(1)
        expect(rig.Methods()).toEqual(['list_sessions'])
    })

    it('activation_re_presents_the_session_view', () =>
    {
        const rig = new Rig()
        rig.host.viewed.push('agents editor')
        rig.service.OnActivated()
        expect(rig.host.viewed).toEqual([rig.service.View, 'agents editor', rig.service.View])
    })

    it('empty_url_stays_inert', () =>
    {
        const rig = new Rig('')
        expect(rig.supervisorSockets).toHaveLength(0)
        expect(rig.sockets).toHaveLength(0)
    })

    it('selecting_row_attaches_and_shows_history', async () =>
    {
        const rig = new Rig()
        await rig.ShowSession('s1', [Rig.Summary('s1'), Rig.Summary('s2')], [
            { role: 'system', content: 'be brief' },
            { role: 'user', content: 'what time is it?' },
            { role: 'assistant', content: '', tool_calls: [{ id: 'c1', type: 'function', function: { name: 'get_time', arguments: {} } }] },
            { role: 'tool', content: '', tool_call_id: 'c1', tool_result: { time: '12:00' } },
            { role: 'assistant', content: 'It is noon.' },
        ])
        const attach = rig.Frames().find(f => f.method === 'attach_session')
        expect(attach?.params).toEqual({ id: 's1' })
        const items = rig.service.View.Chat.Transcript.ToArray()
        expect(items.map(i => i.constructor.name)).toEqual(['UserMessage', 'ToolCallMessage', 'ToolResultMessage', 'AssistantMessage'])
        expect((items[0] as UserMessage).Text).toBe('what time is it?')
        expect((items[1] as ToolCallMessage).Name).toBe('get_time')
        expect((items[3] as AssistantMessage).Text).toBe('It is noon.')
        expect(rig.service.View.HasSession).toBe(true)
        expect(rig.service.View.Chat.CanChat).toBe(true)
    })

    it('sessions_changed_after_a_turn_does_not_reattach', async () =>
    {
        const rig = new Rig()
        await rig.ShowSession('s1', [Rig.Summary('s1')])
        rig.Notify('sessions_changed', {})
        await rig.Respond('list_sessions', [Rig.Summary('s1')])
        expect(rig.Methods().filter(m => m === 'attach_session')).toHaveLength(1)
        expect(rig.service.Panel.SelectedSession?.Id).toBe('s1')
    })

    it('new_session_dialog_creates_and_opens', async () =>
    {
        const rig = new Rig()
        await rig.Connect()
        await rig.Respond('list_sessions', [])
        rig.service.Panel.NewSessionCommand.Execute(undefined)
        await Rig.Flush()
        expect(rig.dialogs.shown).toHaveLength(1)
        const body = rig.dialogs.shown[0].Content as NewSessionDialogViewModel
        expect(body).toBeInstanceOf(NewSessionDialogViewModel)
        await rig.Respond('list_templates', [
            { kind: 'group', name: 'Examples', path: 'Examples', children: [
                { kind: 'template', id: 't1', path: 'Examples', name: 'Default', runtime: 'llamacpp', model: 'gemma', error: null },
            ] },
        ])
        body.SelectedNode = body.Roots.Get(0)!.Children!.Get(0)!
        expect(body.CreateCommand.CanExecute(undefined)).toBe(true)
        body.CreateCommand.Execute(undefined)
        expect(rig.Frames().find(f => f.method === 'create_session')?.params).toEqual({ template_id: 't1' })
        await rig.Respond('create_session', Rig.Summary('new', Rig.NeedsLoad))
        expect(rig.dialogs.closed).toHaveLength(1)
        expect(rig.Frames().filter(f => f.method === 'attach_session').pop()?.params).toEqual({ id: 'new' })
        expect(rig.service.Panel.SelectedSession?.Id).toBe('new')
        expect(rig.service.View.ShownId).toBe('new')
    })

    it('reconnect_reattaches_shown_session', async () =>
    {
        vi.useFakeTimers()
        const rig = new Rig()
        await rig.ShowSession('s1', [Rig.Summary('s1')])
        const first = rig.Socket
        first.Close()
        expect(rig.service.View.Chat.CanChat).toBe(false)
        expect(rig.sockets).toHaveLength(1)
        await vi.advanceTimersByTimeAsync(999)
        expect(rig.sockets).toHaveLength(1)
        await vi.advanceTimersByTimeAsync(1)
        expect(rig.sockets).toHaveLength(2)
        // A failed retry backs off to 2 s.
        rig.Socket.Close()
        await vi.advanceTimersByTimeAsync(1999)
        expect(rig.sockets).toHaveLength(2)
        await vi.advanceTimersByTimeAsync(1)
        expect(rig.sockets).toHaveLength(3)
        rig.Socket.Open()
        expect(rig.Methods()).toEqual(['list_sessions', 'attach_session'])
        expect(rig.Frames()[1].params).toEqual({ id: 's1' })
        await rig.Respond('attach_session', Rig.Attached('s1', [{ role: 'user', content: 'still here' }]))
        expect(rig.service.View.Chat.CanChat).toBe(true)
        expect(rig.service.View.Chat.Transcript.Count).toBe(1)
    })

    it('backoff_caps_at_ten_seconds', async () =>
    {
        vi.useFakeTimers()
        const rig = new Rig()
        await rig.ShowSession('s1', [Rig.Summary('s1')])
        const delays: number[] = []
        for (let i = 0; i < 6; i++)
        {
            const before = rig.sockets.length
            rig.Socket.Close()
            let waited = 0
            while (rig.sockets.length === before)
            {
                await vi.advanceTimersByTimeAsync(500)
                waited += 500
            }
            delays.push(waited)
        }
        expect(delays).toEqual([1000, 2000, 4000, 8000, 10000, 10000])
    })

    it('deleted_shown_session_clears_view', async () =>
    {
        const rig = new Rig()
        await rig.ShowSession('s1', [Rig.Summary('s1'), Rig.Summary('s2')])
        rig.service.Panel.Sessions.Get(0)!.DeleteCommand.Execute(undefined)
        await Rig.Flush()
        expect(rig.Frames().find(f => f.method === 'delete_session')?.params).toEqual({ id: 's1' })
        await rig.Respond('delete_session', null)
        rig.Notify('sessions_changed', {})
        await rig.Respond('list_sessions', [Rig.Summary('s2')])
        expect(rig.service.View.HasSession).toBe(false)
        expect(rig.service.View.ShownId).toBeNull()
        expect(rig.service.View.Chat.Transcript.Count).toBe(0)
        expect(rig.service.View.Chat.CanChat).toBe(false)
        expect(rig.service.Panel.SelectedSession).toBeNull()
    })

    it('rename_prompts_and_renames', async () =>
    {
        const rig = new Rig()
        await rig.ShowSession('s1', [Rig.Summary('s1')])
        rig.service.Panel.RenameCommand.Execute(undefined)
        await Rig.Flush()
        expect(rig.dialogs.shown).toHaveLength(1)
        rig.dialogs.Close('  Notes reader ')
        await Rig.Flush()
        expect(rig.Frames().find(f => f.method === 'rename_session')?.params).toEqual({ id: 's1', title: 'Notes reader' })
    })

    it('settings_change_reconnects_and_clears', async () =>
    {
        const rig = new Rig()
        await rig.ShowSession('s1', [Rig.Summary('s1')])
        const first = rig.Socket
        rig.settings.Change('ws://other:8778')
        expect(first.closeCalls).toBe(1)
        expect(rig.service.View.HasSession).toBe(false)
        expect(rig.service.Panel.Sessions.Count).toBe(0)
        await rig.Connect()
        expect(rig.urls).toEqual(['ws://hive:8779', 'ws://other:8779'])
    })

    it('reorder_after_sessions_changed_keeps_selection_without_reattach', async () =>
    {
        const rig = new Rig()
        await rig.ShowSession('s2', [Rig.Summary('s1'), Rig.Summary('s2'), Rig.Summary('s3')])
        const selected = rig.service.Panel.SelectedSession
        const kinds: string[] = []
        rig.service.Panel.Sessions.Subscribe(change => kinds.push(change.kind))
        // A committed turn in s2 moves it to the top (newest first).
        rig.Notify('sessions_changed', {})
        await rig.Respond('list_sessions', [Rig.Summary('s2'), Rig.Summary('s1'), Rig.Summary('s3')])
        expect(rig.service.Panel.Sessions.ToArray().map(r => r.Id)).toEqual(['s2', 's1', 's3'])
        expect(kinds).toEqual(['moved'])
        expect(rig.service.Panel.SelectedSession).toBe(selected)
        expect(rig.Methods().filter(m => m === 'attach_session')).toHaveLength(1)
    })

    it('switch_is_refused_while_the_shown_session_runs_a_turn', async () =>
    {
        const rig = new Rig()
        await rig.ShowSession('s1', [Rig.Summary('s1'), Rig.Summary('s2')])
        const panel = rig.service.Panel
        const shown = panel.SelectedSession
        rig.service.View.Chat.Draft = 'long job'
        rig.service.View.Chat.SendCommand.Execute(undefined)
        expect(rig.service.View.Chat.IsBusy).toBe(true)
        panel.SelectedSession = panel.Find('s2')!
        await Rig.Flush()
        expect(panel.SelectedSession).toBe(shown)
        expect(panel.StatusText).toBe('Finish the running turn before switching sessions.')
        expect(rig.Methods().filter(m => m === 'attach_session')).toHaveLength(1)
        // The turn settles: the notice goes and switching works again.
        rig.Notify('loop_done', { steps: 1, reason: 'answered' })
        await rig.Respond('send_turn', null)
        expect(panel.StatusText).toBe('')
        panel.SelectedSession = panel.Find('s2')!
        expect(rig.Frames().filter(f => f.method === 'attach_session').pop()?.params).toEqual({ id: 's2' })
    })

    it('rename_and_delete_failures_show_on_the_panel', async () =>
    {
        const rig = new Rig()
        await rig.ShowSession('s1', [Rig.Summary('s1')])
        rig.service.Panel.RenameCommand.Execute(undefined)
        await Rig.Flush()
        rig.dialogs.Close('New title')
        await Rig.Flush()
        const rename = rig.Frames().find(f => f.method === 'rename_session')!
        rig.service.Client.HandleFrame(JSON.stringify({ id: rename.id, error: { code: -32603, message: 'unknown session: s1' } }))
        await Rig.Flush()
        expect(rig.service.Panel.StatusText).toBe('rename failed: unknown session: s1')
        rig.service.Panel.DeleteCommand.Execute(undefined)
        await Rig.Flush()
        rig.Socket.Close()
        await Rig.Flush()
        expect(rig.service.Panel.StatusText).toBe('delete failed: Harness not connected')
    })

    it('connection_state_shows_on_panel_and_marks_placement_stale', async () =>
    {
        vi.useFakeTimers()
        const rig = new Rig()
        expect(rig.service.Connection).toBe(ConnectionState.Reconnecting)
        expect(rig.service.Panel.EmptyText).toBe('Connecting to the runtime…')
        await rig.ShowSession('s1', [Rig.Summary('s1')])
        expect(rig.service.Connection).toBe(ConnectionState.Connected)
        expect(rig.service.Panel.ShowsEmptyText).toBe(false)
        expect(rig.service.View.PlacementText).toBe('live on local')
        expect(rig.service.View.CanUnload).toBe(true)
        rig.Socket.Close()
        expect(rig.service.Connection).toBe(ConnectionState.Reconnecting)
        expect(rig.service.Panel.ShowsEmptyText).toBe(true)
        expect(rig.service.View.PlacementText).toBe('live on local (stale: not connected)')
        expect(rig.service.View.CanUnload).toBe(false)
        rig.settings.Change('')
        expect(rig.service.Connection).toBe(ConnectionState.Disconnected)
        expect(rig.service.Panel.EmptyText).toBe('Not connected: set the supervisor URL in Settings.')
    })

    it('silent_supervisor_times_out_to_the_default_port_and_retries_discover_again', async () =>
    {
        vi.useFakeTimers()
        const rig = new Rig('')
        rig.SupervisorSilent = true
        rig.settings.Change('ws://hive:8778')
        await vi.advanceTimersByTimeAsync(2999)
        expect(rig.urls).toEqual([])
        await vi.advanceTimersByTimeAsync(1)
        expect(rig.urls).toEqual(['ws://hive:8779'])
        expect(rig.supervisorSockets[0].closeCalls).toBe(1)
        // The runtime is down too: the retry after 1 s asks the supervisor again.
        rig.Socket.Close()
        await vi.advanceTimersByTimeAsync(1000)
        expect(rig.supervisorSockets).toHaveLength(2)
        await vi.advanceTimersByTimeAsync(3000)
        expect(rig.urls).toEqual(['ws://hive:8779', 'ws://hive:8779'])
    })

    it('new_session_is_disabled_while_the_shown_session_runs_a_turn', async () =>
    {
        const rig = new Rig()
        await rig.ShowSession('s1', [Rig.Summary('s1')])
        const command = rig.service.Panel.NewSessionCommand
        expect(command.CanExecute(undefined)).toBe(true)
        rig.service.View.Chat.Draft = 'long job'
        rig.service.View.Chat.SendCommand.Execute(undefined)
        expect(command.CanExecute(undefined)).toBe(false)
        rig.Notify('loop_done', { steps: 1, reason: 'answered' })
        await rig.Respond('send_turn', null)
        expect(command.CanExecute(undefined)).toBe(true)
    })

    it('session_created_during_a_turn_is_listed_but_not_opened', async () =>
    {
        const rig = new Rig()
        await rig.ShowSession('s1', [Rig.Summary('s1')])
        rig.service.Panel.NewSessionCommand.Execute(undefined)
        await Rig.Flush()
        const body = rig.dialogs.shown[0].Content as NewSessionDialogViewModel
        await rig.Respond('list_templates', [{ kind: 'template', id: 't1', path: '', name: 'Default', runtime: 'llamacpp', model: 'gemma', error: null }])
        body.SelectedNode = body.Roots.Get(0)!
        // The turn starts while the dialog is still open.
        rig.service.View.Chat.Draft = 'long job'
        rig.service.View.Chat.SendCommand.Execute(undefined)
        body.CreateCommand.Execute(undefined)
        await rig.Respond('create_session', Rig.Summary('new'))
        expect(rig.dialogs.closed).toHaveLength(1)
        expect(rig.service.Panel.Find('new')).toBeDefined()
        expect(rig.service.Panel.SelectedSession?.Id).toBe('s1')
        expect(rig.service.View.ShownId).toBe('s1')
        expect(rig.Methods().filter(m => m === 'attach_session')).toHaveLength(1)
        expect(rig.service.Panel.StatusText).toBe('Finish the running turn before switching sessions.')
        expect(rig.service.View.Chat.IsBusy).toBe(true)
    })
})

