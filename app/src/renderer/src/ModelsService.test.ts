import { describe, expect, it } from 'vitest'
import { Observable, type IServiceProvider } from '@pragmatic-tech-ai/mural/runtime'
import { ContentHostService } from '@pragmatic-tech-ai/mural/framework/shell/services/content-host-service.js'
import { DialogService, NavigationService, type ConfirmOptions } from '@pragmatic-tech-ai/mural/framework'
import { ModelsService } from './ModelsService'
import { SessionsService } from './SessionsService'
import { RuntimeClient, TypedSignal } from './RuntimeClient'
import { FakeSocket } from './FakeSocket'
import { ConnectionState } from './ConnectionState'
import { RepositoryKind, type RepositorySummary } from './agents'

class ModelsHost
{
    public readonly viewed: unknown[] = []

    public View(content: unknown): void
    {
        this.viewed.push(content)
    }

    public get Last(): unknown
    {
        return this.viewed[this.viewed.length - 1]
    }
}

class ModelsDialogs
{
    public readonly confirms: ConfirmOptions[] = []
    public ConfirmResult = true

    public Confirm(options: ConfirmOptions): Promise<boolean>
    {
        this.confirms.push(options)
        return Promise.resolve(this.ConfirmResult)
    }
}

class ModelsSessions
{
    public readonly OnConnectionChanged = new TypedSignal<ConnectionState>()
    public readonly OnRuntimeErrorChanged = new TypedSignal<string>()
    public Connection = ConnectionState.Connected
    public RuntimeError = ''

    constructor(public readonly Client: RuntimeClient)
    {
    }

    public Change(state: ConnectionState): void
    {
        this.Connection = state
        this.OnConnectionChanged.Raise(state)
    }
}

class ModelsNavigation extends Observable
{
    public SelectedItem: unknown = undefined
    public ActiveService: unknown = undefined

    public Switch(item: unknown, service: unknown): void
    {
        this.SelectedItem = item
        const old = this.ActiveService
        this.ActiveService = service
        this.RaisePropertyChanged('ActiveService', old, service)
    }
}

class ModelsProvider
{
    constructor(private readonly entries: Map<unknown, unknown>)
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
        return this.entries.get(token) as T | undefined
    }
}

class ModelsRig
{
    public static readonly Caps = { measure_prompt: 'exact' as const, tokenize: true, raw_prompt: true, logprobs: true, load_unload: true }
    public static readonly Hive: RepositorySummary = { name: 'hive', kind: RepositoryKind.Pragmatic, editable: false, capabilities: ModelsRig.Caps, model_count: 1 }
    public static readonly Router: RepositorySummary = {
        name: 'openrouter', kind: RepositoryKind.OpenRouter, editable: true, capabilities: ModelsRig.Caps, model_count: 0,
        has_key: true, key_hint: '7f2a', id_prefixes: [], fetched_at: 1700000000,
    }
    public static readonly ModelsItem = { name: 'models-item' }
    public static readonly SessionsItem = { name: 'sessions-item' }
    public static readonly OtherService = { name: 'sessions-service' }

    public readonly socket = new FakeSocket()
    public readonly client = new RuntimeClient(() => this.socket)
    public readonly host = new ModelsHost()
    public readonly dialogs = new ModelsDialogs()
    public readonly sessions = new ModelsSessions(this.client)
    public readonly navigation = new ModelsNavigation()
    public readonly service: ModelsService

    constructor()
    {
        this.client.Connect('ws://runtime')
        this.socket.Open()
        this.service = new ModelsService(new ModelsProvider(new Map<unknown, unknown>([
            [ContentHostService.Key, this.host],
            [DialogService.Key, this.dialogs],
            [SessionsService.Key, this.sessions],
            [NavigationService.Key, this.navigation],
        ])) as unknown as IServiceProvider)
    }

    public static async Flush(): Promise<void>
    {
        await new Promise(r => setTimeout(r, 0))
    }

    public Frames(method: string): { id: number, params: Record<string, unknown> }[]
    {
        return this.socket.sent.map(f => JSON.parse(f)).filter(f => f.method === method)
    }

    public async Respond(method: string, result: unknown): Promise<void>
    {
        this.client.HandleFrame(JSON.stringify({ id: this.Frames(method).pop()?.id ?? -1, result }))
        await ModelsRig.Flush()
    }

    public async Fail(method: string, message: string): Promise<void>
    {
        this.client.HandleFrame(JSON.stringify({ id: this.Frames(method).pop()?.id ?? -1, error: { code: -32603, message } }))
        await ModelsRig.Flush()
    }

    public async Ready(): Promise<void>
    {
        await this.Respond('list_repositories', [ModelsRig.Hive, ModelsRig.Router])
    }

    public async OpenRouter(): Promise<void>
    {
        this.service.Panel.Selected = this.service.Panel.Rows.Get(1)!
        await ModelsRig.Flush()
        await this.Respond('list_repository_models', { models: [], fetched_at: 1700000000 })
    }
}

describe('ModelsService', () =>
{
    it('lists_over_the_shared_connection_and_activation_shows_the_editor', async () =>
    {
        const rig = new ModelsRig()
        expect(rig.Frames('list_repositories').length).toBe(1)
        await rig.Ready()
        expect(rig.service.Panel.Rows.Count).toBe(2)
        rig.service.OnActivated()
        expect(rig.host.Last).toBe(rig.service.Editor)
    })

    it('selecting_a_row_opens_it', async () =>
    {
        const rig = new ModelsRig()
        await rig.Ready()
        await rig.OpenRouter()
        expect(rig.service.Panel.Selected).toBe(rig.service.Panel.Rows.Get(1))
        expect(rig.service.Editor.Name).toBe('openrouter')
        expect(rig.host.Last).toBe(rig.service.Editor)
    })

    it('add_opens_a_draft_with_an_unused_name', async () =>
    {
        const rig = new ModelsRig()
        await rig.Ready()
        rig.service.Panel.AddOpenRouterCommand.Execute(undefined)
        await ModelsRig.Flush()
        expect(rig.service.Editor.IsNew).toBe(true)
        expect(rig.service.Editor.Name).toBe('openrouter-2')
        expect(rig.service.Panel.Selected).toBe(null)
    })

    it('delete_refused_shows_server_error', async () =>
    {
        const rig = new ModelsRig()
        await rig.Ready()
        await rig.OpenRouter()
        rig.service.Editor.DeleteCommand.Execute(undefined)
        await ModelsRig.Flush()
        expect(rig.dialogs.confirms.length).toBe(1)
        expect(rig.Frames('delete_repository')[0].params).toEqual({ name: 'openrouter' })
        await rig.Fail('delete_repository', 'repository is used by templates: Router')
        expect(rig.service.Editor.ErrorText).toBe('repository is used by templates: Router')
        expect(rig.service.Editor.HasRepository).toBe(true)

        // Cancelled at the confirm: nothing is sent.
        rig.dialogs.ConfirmResult = false
        rig.service.Editor.DeleteCommand.Execute(undefined)
        await ModelsRig.Flush()
        expect(rig.Frames('delete_repository').length).toBe(1)
    })

    it('delete_accepted_clears_the_editor_once_listed_gone', async () =>
    {
        const rig = new ModelsRig()
        await rig.Ready()
        await rig.OpenRouter()
        rig.service.Editor.DeleteCommand.Execute(undefined)
        await ModelsRig.Flush()
        await rig.Respond('delete_repository', {})
        rig.client.HandleFrame(JSON.stringify({ method: 'repositories_changed', params: {} }))
        await ModelsRig.Flush()
        await rig.Respond('list_repositories', [ModelsRig.Hive])
        expect(rig.service.Editor.IsEmpty).toBe(true)
        expect(rig.service.Panel.Rows.Count).toBe(1)
    })

    it('unsaved_changes_guard_on_leave', async () =>
    {
        const rig = new ModelsRig()
        await rig.Ready()
        rig.navigation.Switch(ModelsRig.ModelsItem, rig.service)
        await rig.OpenRouter()
        rig.service.Editor.FilterText = 'anthropic/'

        // Leaving and choosing Cancel: back on Models, the edits kept.
        rig.dialogs.ConfirmResult = false
        rig.navigation.Switch(ModelsRig.SessionsItem, ModelsRig.OtherService)
        await ModelsRig.Flush()
        expect(rig.dialogs.confirms.length).toBe(1)
        expect(rig.navigation.SelectedItem).toBe(ModelsRig.ModelsItem)
        expect(rig.service.Editor.FilterText).toBe('anthropic/')

        // Picking another row asks too; Cancel keeps the open repository.
        rig.service.Panel.Selected = rig.service.Panel.Rows.Get(0)!
        await ModelsRig.Flush()
        expect(rig.dialogs.confirms.length).toBe(2)
        expect(rig.service.Editor.Name).toBe('openrouter')

        // Leaving and choosing Discard drops the edits.
        rig.dialogs.ConfirmResult = true
        rig.navigation.Switch(ModelsRig.ModelsItem, rig.service)
        rig.navigation.Switch(ModelsRig.SessionsItem, ModelsRig.OtherService)
        await ModelsRig.Flush()
        expect(rig.dialogs.confirms.length).toBe(3)
        expect(rig.service.Editor.FilterText).toBe('')
        expect(rig.service.Editor.IsDirty).toBe(false)

        // A clean editor leaves without asking.
        rig.navigation.Switch(ModelsRig.ModelsItem, rig.service)
        rig.navigation.Switch(ModelsRig.SessionsItem, ModelsRig.OtherService)
        await ModelsRig.Flush()
        expect(rig.dialogs.confirms.length).toBe(3)
    })

    it('repositories_changed_refreshes_a_clean_editor', async () =>
    {
        const rig = new ModelsRig()
        await rig.Ready()
        await rig.OpenRouter()
        rig.client.HandleFrame(JSON.stringify({ method: 'repositories_changed', params: {} }))
        await ModelsRig.Flush()
        await rig.Respond('list_repositories', [ModelsRig.Hive, { ...ModelsRig.Router, key_hint: 'abcd', model_count: 1 }])
        expect(rig.service.Editor.KeyHint).toBe('••••abcd')
        expect(rig.Frames('list_repository_models').length).toBe(2)
    })

    it('lost_connection_unbinds_and_reconnect_relists', async () =>
    {
        const rig = new ModelsRig()
        await rig.Ready()
        rig.sessions.Change(ConnectionState.Disconnected)
        expect(rig.service.Panel.AddOpenRouterCommand.CanExecute(undefined)).toBe(false)
        rig.sessions.Change(ConnectionState.Connected)
        expect(rig.Frames('list_repositories').length).toBe(2)
        expect(rig.service.Panel.AddOpenRouterCommand.CanExecute(undefined)).toBe(true)
    })
})
