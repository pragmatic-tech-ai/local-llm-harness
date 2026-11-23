import { describe, expect, it } from 'vitest'
import { Observable, type IServiceProvider } from '@pragmatic-tech-ai/mural/runtime'
import { ContentHostService } from '@pragmatic-tech-ai/mural/framework/shell/services/content-host-service.js'
import { DialogService, NavigationService, type ConfirmOptions, type DialogOptions } from '@pragmatic-tech-ai/mural/framework'
import { AgentsService } from './AgentsService'
import { SessionsService } from './SessionsService'
import { RuntimeClient, TypedSignal } from './RuntimeClient'
import { FakeSocket } from './FakeSocket'
import { ConnectionState } from './ConnectionState'
import { MoveToDialogViewModel } from './MoveToDialogViewModel'
import { TextPromptViewModel } from './TextPromptViewModel'
import type { AgentTemplate, CatalogInfo, TemplateNode, ToolSpec } from './agents'

class FakeContentHost
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

class FakeDialogs
{
    public readonly shown: DialogOptions[] = []
    public readonly confirms: ConfirmOptions[] = []
    public ConfirmResult = true
    private resolve: ((value: unknown) => void) | null = null

    public Show<T>(options: DialogOptions): Promise<T | undefined>
    {
        this.shown.push(options)
        return new Promise<T | undefined>(r => { this.resolve = r as (value: unknown) => void })
    }

    public Close(result?: unknown): void
    {
        this.resolve?.(result)
        this.resolve = null
    }

    public Confirm(options: ConfirmOptions): Promise<boolean>
    {
        this.confirms.push(options)
        return Promise.resolve(this.ConfirmResult)
    }
}

/** The part of SessionsService the Agents capability uses: the shared client and the connection state. */
class FakeSessions
{
    public readonly OnConnectionChanged = new TypedSignal<ConnectionState>()
    public Connection = ConnectionState.Connected

    constructor(public readonly Client: RuntimeClient)
    {
    }

    public Change(state: ConnectionState): void
    {
        this.Connection = state
        this.OnConnectionChanged.Raise(state)
    }
}

/** NavigationService's observable surface: the selected rail item and the active capability service. */
class FakeNavigation extends Observable
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

class StubProvider
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

class Rig
{
    public readonly socket = new FakeSocket()
    public readonly client = new RuntimeClient(() => this.socket)
    public readonly host = new FakeContentHost()
    public readonly dialogs = new FakeDialogs()
    public readonly sessions = new FakeSessions(this.client)
    public readonly navigation = new FakeNavigation()
    public readonly service: AgentsService

    public static readonly AgentsItem = { name: 'agents-item' }
    public static readonly SessionsItem = { name: 'sessions-item' }
    public static readonly OtherService = { name: 'sessions-service' }

    public static readonly Researcher: TemplateNode = { kind: 'template', id: 't1', path: 'Examples', name: 'Researcher', runtime: 'llamacpp', model: 'gemma-4', error: null }
    public static readonly Loose: TemplateNode = { kind: 'template', id: 't3', path: '', name: 'Loose', runtime: 'llamacpp', model: 'gemma-4', error: null }
    public static readonly Tree: TemplateNode[] = [
        { kind: 'group', name: 'Examples', path: 'Examples', children: [Rig.Researcher] },
        Rig.Loose,
    ]

    public static readonly Catalog: CatalogInfo = {
        runtimes: [{ name: 'llamacpp', kind: 'llamacpp' }],
        models: [{ name: 'gemma-4', runtimes: ['llamacpp'], size_hint: null }],
        hosts: ['hive'],
    }

    public static readonly Tools: ToolSpec[] = [{ name: 'read_file', description: 'Read a file', parameters: {}, side_effects: false }]

    public static readonly Template: AgentTemplate = {
        name: 'Researcher', system_prompt: '', tools: ['read_file'], step_budget: 8, runtime: 'llamacpp', model: 'gemma-4', n_ctx: 8192, enable_thinking: false,
    }

    constructor(withNavigation: boolean = true)
    {
        this.client.Connect('ws://runtime')
        this.socket.Open()
        const entries = new Map<unknown, unknown>([
            [ContentHostService.Key, this.host],
            [DialogService.Key, this.dialogs],
            [SessionsService.Key, this.sessions],
        ])
        if (withNavigation)
        {
            entries.set(NavigationService.Key, this.navigation)
        }
        this.service = new AgentsService(new StubProvider(entries) as unknown as IServiceProvider)
    }

    public static async Flush(): Promise<void>
    {
        await new Promise(r => setTimeout(r, 0))
    }

    public Frames(): { id: number, method: string, params: Record<string, unknown> }[]
    {
        return this.socket.sent.map(f => JSON.parse(f))
    }

    public Methods(): string[]
    {
        return this.Frames().map(f => f.method)
    }

    public async Respond(method: string, result: unknown): Promise<void>
    {
        const id = this.Frames().filter(f => f.method === method).pop()?.id ?? -1
        this.client.HandleFrame(JSON.stringify({ id, result }))
        await Rig.Flush()
    }

    public async Fail(method: string, message: string): Promise<void>
    {
        const id = this.Frames().filter(f => f.method === method).pop()?.id ?? -1
        this.client.HandleFrame(JSON.stringify({ id, error: { code: -32603, message } }))
        await Rig.Flush()
    }

    public async Ready(): Promise<void>
    {
        await this.Respond('list_templates', Rig.Tree)
        await this.Respond('list_catalog', Rig.Catalog)
        await this.Respond('list_all_tools', Rig.Tools)
    }

    public get Panel()
    {
        return this.service.Panel
    }

    public Group()
    {
        return this.Panel.Roots.Get(0)!
    }

    public ResearcherNode()
    {
        return this.Group().Children!.Get(0)!
    }

    public LooseNode()
    {
        return this.Panel.Roots.Get(1)!
    }

    /** Selects Researcher as the user would and answers its read. */
    public async OpenResearcher(): Promise<void>
    {
        this.Panel.Selected = this.ResearcherNode()
        await Rig.Flush()
        await this.Respond('get_template', { id: 't1', path: 'Examples', template: Rig.Template })
    }
}

describe('AgentsService', () =>
{
    it('lists_templates_catalog_and_tools_over_the_shared_connection', async () =>
    {
        const rig = new Rig()
        expect(rig.Methods()).toEqual(['list_templates', 'list_catalog', 'list_all_tools'])
        await rig.Ready()
        expect(rig.Panel.Roots.Count).toBe(2)
        expect(rig.service.Editor.Tools.Count).toBe(1)
        expect(rig.host.viewed).toEqual([])
    })

    it('selecting_template_opens_editor', async () =>
    {
        const rig = new Rig()
        await rig.Ready()
        await rig.OpenResearcher()
        expect(rig.Panel.Selected).toBe(rig.ResearcherNode())
        expect(rig.host.Last).toBe(rig.service.Editor)
        expect(rig.service.Editor.TemplateId).toBe('t1')
        expect(rig.service.Editor.Name).toBe('Researcher')
        // A group selection keeps the editor where it is.
        rig.Panel.Selected = rig.Group()
        await Rig.Flush()
        expect(rig.Panel.Selected).toBe(rig.Group())
        expect(rig.service.Editor.TemplateId).toBe('t1')
    })

    it('activation_shows_the_editor', () =>
    {
        const rig = new Rig()
        rig.service.OnActivated()
        expect(rig.host.Last).toBe(rig.service.Editor)
    })

    it('delete_group_error_surfaces', async () =>
    {
        const rig = new Rig()
        await rig.Ready()
        rig.Panel.Selected = rig.Group()
        await Rig.Flush()
        rig.Panel.DeleteCommand.Execute(undefined)
        await Rig.Flush()
        expect(rig.dialogs.confirms.length).toBe(1)
        expect(rig.Frames().pop()).toMatchObject({ method: 'delete_group', params: { path: 'Examples' } })
        await rig.Fail('delete_group', 'group is not empty')
        expect(rig.Panel.StatusText).toBe('delete failed: group is not empty')
    })

    it('delete_template_clears_the_editor_once_it_is_gone', async () =>
    {
        const rig = new Rig()
        await rig.Ready()
        await rig.OpenResearcher()
        rig.Panel.DeleteCommand.Execute(undefined)
        await Rig.Flush()
        expect(rig.Frames().pop()).toMatchObject({ method: 'delete_template', params: { id: 't1' } })
        await rig.Respond('delete_template', null)
        rig.client.HandleFrame(JSON.stringify({ method: 'templates_changed', params: {} }))
        await rig.Respond('list_templates', [{ kind: 'group', name: 'Examples', path: 'Examples', children: [] }, Rig.Loose])
        expect(rig.service.Editor.HasTemplate).toBe(false)
        expect(rig.Panel.Selected).toBeNull()
    })

    it('dirty_editor_asks_before_switching_template', async () =>
    {
        const rig = new Rig()
        await rig.Ready()
        await rig.OpenResearcher()
        rig.service.Editor.Name = 'Edited'
        rig.dialogs.ConfirmResult = false
        rig.Panel.Selected = rig.LooseNode()
        await Rig.Flush()
        expect(rig.dialogs.confirms.map(c => c.ConfirmLabel)).toEqual(['Discard'])
        expect(rig.Panel.Selected).toBe(rig.ResearcherNode())
        expect(rig.service.Editor.Name).toBe('Edited')
        expect(rig.Methods().filter(m => m === 'get_template').length).toBe(1)
        rig.dialogs.ConfirmResult = true
        rig.Panel.Selected = rig.LooseNode()
        await Rig.Flush()
        expect(rig.Panel.Selected).toBe(rig.LooseNode())
        expect(rig.Frames().pop()).toMatchObject({ method: 'get_template', params: { id: 't3' } })
        // A clean editor switches without asking.
        await rig.Respond('get_template', { id: 't3', path: '', template: { ...Rig.Template, name: 'Loose' } })
        rig.Panel.Selected = rig.ResearcherNode()
        await Rig.Flush()
        expect(rig.dialogs.confirms.length).toBe(2)
    })

    it('dirty_editor_asks_before_leaving_the_rail', async () =>
    {
        const rig = new Rig()
        await rig.Ready()
        rig.navigation.Switch(Rig.AgentsItem, rig.service)
        await rig.OpenResearcher()
        rig.service.Editor.Name = 'Edited'
        rig.dialogs.ConfirmResult = false
        rig.navigation.Switch(Rig.SessionsItem, Rig.OtherService)
        await Rig.Flush()
        expect(rig.dialogs.confirms.length).toBe(1)
        // Cancel: back to the Agents rail item, the edit kept.
        expect(rig.navigation.SelectedItem).toBe(Rig.AgentsItem)
        expect(rig.service.Editor.Name).toBe('Edited')
        rig.navigation.Switch(Rig.AgentsItem, rig.service)
        rig.dialogs.ConfirmResult = true
        rig.navigation.Switch(Rig.SessionsItem, Rig.OtherService)
        await Rig.Flush()
        // Discard: the switch stands and the edit is reverted.
        expect(rig.navigation.SelectedItem).toBe(Rig.SessionsItem)
        expect(rig.service.Editor.Name).toBe('Researcher')
        expect(rig.service.Editor.IsDirty).toBe(false)
        // Leaving a clean editor does not ask.
        rig.navigation.Switch(Rig.AgentsItem, rig.service)
        rig.navigation.Switch(Rig.SessionsItem, Rig.OtherService)
        await Rig.Flush()
        expect(rig.dialogs.confirms.length).toBe(2)
    })

    it('new_template_opens_a_draft_that_saves_into_the_selected_group', async () =>
    {
        const rig = new Rig()
        await rig.Ready()
        rig.Panel.Selected = rig.Group()
        await Rig.Flush()
        rig.Panel.NewTemplateCommand.Execute(undefined)
        await Rig.Flush()
        const editor = rig.service.Editor
        expect(rig.host.Last).toBe(editor)
        expect(editor.IsNew).toBe(true)
        expect(editor.Name).toBe('Untitled')
        editor.SaveCommand.Execute(undefined)
        expect(rig.Frames().pop()).toMatchObject({ method: 'save_template', params: { path: 'Examples', template: { name: 'Untitled' } } })
        rig.client.HandleFrame(JSON.stringify({ method: 'templates_changed', params: {} }))
        await rig.Respond('save_template', { id: 'untitled' })
        const untitled: TemplateNode = { kind: 'template', id: 'untitled', path: 'Examples', name: 'Untitled', runtime: 'llamacpp', model: 'gemma-4', error: null }
        await rig.Respond('list_templates', [{ kind: 'group', name: 'Examples', path: 'Examples', children: [Rig.Researcher, untitled] }, Rig.Loose])
        expect(rig.Panel.Selected?.TemplateId).toBe('untitled')
        expect(editor.TemplateId).toBe('untitled')
    })

    it('new_group_prompts_with_the_default_name_and_creates_it_in_the_selected_group', async () =>
    {
        const rig = new Rig()
        await rig.Ready()
        rig.Panel.Selected = rig.Group()
        await Rig.Flush()
        rig.Panel.NewGroupCommand.Execute(undefined)
        await Rig.Flush()
        const prompt = rig.dialogs.shown[0].Content as TextPromptViewModel
        expect(prompt).toBeInstanceOf(TextPromptViewModel)
        expect(prompt.Text).toBe('New group')
        rig.dialogs.Close('New group')
        await Rig.Flush()
        expect(rig.Frames().pop()).toMatchObject({ method: 'create_group', params: { path: 'Examples/New group' } })
    })

    it('rename_group_and_template', async () =>
    {
        const rig = new Rig()
        await rig.Ready()
        rig.Panel.Selected = rig.Group()
        await Rig.Flush()
        rig.Panel.RenameCommand.Execute(undefined)
        await Rig.Flush()
        rig.dialogs.Close('Samples')
        await Rig.Flush()
        expect(rig.Frames().pop()).toMatchObject({ method: 'rename_group', params: { path: 'Examples', new_name: 'Samples' } })
        await rig.Respond('rename_group', null)
        rig.Panel.Select(rig.LooseNode())
        rig.Panel.RenameCommand.Execute(undefined)
        await Rig.Flush()
        rig.dialogs.Close('Tight')
        await Rig.Flush()
        await rig.Respond('get_template', { id: 't3', path: '', template: { ...Rig.Template, name: 'Loose' } })
        expect(rig.Frames().pop()).toMatchObject({ method: 'save_template', params: { id: 't3', path: '', template: { name: 'Tight' } } })
    })

    it('rename_of_the_dirty_open_template_is_refused', async () =>
    {
        const rig = new Rig()
        await rig.Ready()
        await rig.OpenResearcher()
        rig.service.Editor.Name = 'Edited'
        rig.Panel.RenameCommand.Execute(undefined)
        await Rig.Flush()
        expect(rig.dialogs.shown).toEqual([])
        expect(rig.Panel.StatusText).toBe('Save or revert the open template first.')
    })

    it('move_to_lists_the_groups_and_moves', async () =>
    {
        const rig = new Rig()
        await rig.Ready()
        await rig.OpenResearcher()
        rig.Panel.MoveToCommand.Execute(undefined)
        await Rig.Flush()
        const body = rig.dialogs.shown[0].Content as MoveToDialogViewModel
        expect(body).toBeInstanceOf(MoveToDialogViewModel)
        expect(body.Groups.ToArray().map(g => g.Label)).toEqual(['(root)', 'Examples'])
        expect(body.SelectedGroup?.Value).toBe('Examples')
        expect(body.MoveCommand.CanExecute(undefined)).toBe(false)
        body.SelectedGroup = body.Groups.Get(0)!
        expect(body.MoveCommand.CanExecute(undefined)).toBe(true)
        body.MoveCommand.Execute(undefined)
        await Rig.Flush()
        expect(rig.Frames().pop()).toMatchObject({ method: 'move_template', params: { id: 't1', path: '' } })
        await rig.Respond('move_template', null)
        rig.client.HandleFrame(JSON.stringify({ method: 'templates_changed', params: {} }))
        await rig.Respond('list_templates', [{ kind: 'group', name: 'Examples', path: 'Examples', children: [] }, { ...Rig.Researcher, path: '' }, Rig.Loose])
        // The open editor follows the move, so its next save does not move the template back.
        expect(rig.service.Editor.Path).toBe('')
        expect(rig.Panel.Selected?.TemplateId).toBe('t1')
    })

    it('templates_changed_reloads_a_clean_editor', async () =>
    {
        const rig = new Rig()
        await rig.Ready()
        await rig.OpenResearcher()
        rig.client.HandleFrame(JSON.stringify({ method: 'templates_changed', params: {} }))
        await rig.Respond('list_templates', Rig.Tree)
        await rig.Respond('get_template', { id: 't1', path: 'Examples', template: { ...Rig.Template, system_prompt: 'hand edited' } })
        expect(rig.service.Editor.SystemPrompt).toBe('hand edited')
    })

    it('reconnect_relists_and_lost_connection_disables_commands', async () =>
    {
        const rig = new Rig()
        await rig.Ready()
        rig.sessions.Change(ConnectionState.Reconnecting)
        expect(rig.Panel.NewTemplateCommand.CanExecute(undefined)).toBe(false)
        expect(rig.Panel.Roots.Count).toBe(2)
        rig.sessions.Change(ConnectionState.Connected)
        expect(rig.Methods().slice(-3)).toEqual(['list_templates', 'list_catalog', 'list_all_tools'])
        expect(rig.Panel.NewTemplateCommand.CanExecute(undefined)).toBe(true)
    })

    it('works_without_a_navigation_service', async () =>
    {
        const rig = new Rig(false)
        await rig.Ready()
        await rig.OpenResearcher()
        expect(rig.service.Editor.Name).toBe('Researcher')
    })
})
