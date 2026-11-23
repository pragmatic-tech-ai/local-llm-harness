import { JsonRpcConnection, TypedSignal } from './JsonRpcConnection'
import type {
    AgentTemplate,
    AttachedSession,
    CatalogInfo,
    Placement,
    PlacementChanged,
    SessionBusy,
    SessionLoading,
    SessionSummary,
    TemplateNode,
    TemplateRecord,
    ToolSpec
} from './agents'

export { TypedSignal }
export type { IHarnessSocket, HarnessSocketFactory, IDisposable } from './JsonRpcConnection'

export interface SamplingConfig
{
    temperature: number
    top_p: number
    top_k: number
    repeat_penalty: number
    seed: number
    max_tokens: number
}

export interface ModelProps
{
    n_ctx: number
    model_name: string
    supports_tools?: boolean
}

export interface TokenizeResult
{
    tokens: number[]
    count: number
}

export interface TokenInfo
{
    token: string
    logprob: number
    top: [string, number][]
}

export interface TurnStarted
{
    prompt: string
    prompt_tokens: number
    n_ctx: number
}

export interface TurnChunk
{
    text: string
    tokens: TokenInfo[]
    stop: boolean
}

export interface TurnError
{
    message: string
}

export interface TurnRejected
{
    needed: number
    total: number
}

export interface ToolCallNote
{
    id: string
    name: string
    arguments: unknown
    raw: string
    parse_error?: string
}

export interface ToolResultNote
{
    id: string
    name: string
    ok: boolean
    content: unknown
}

export interface LoopDone
{
    steps: number
    reason: string
}

export interface ToolInfo
{
    name: string
    description: string
    side_effects: boolean
}

/** JSON-RPC client for the agent runtime: templates, sessions, catalog and the attached session chat stream. */
export class RuntimeClient extends JsonRpcConnection
{
    private static readonly MethodModelProps = 'model_props'
    private static readonly MethodTokenize = 'tokenize'
    private static readonly MethodSetSampling = 'set_sampling'
    private static readonly MethodSendTurn = 'send_turn'
    private static readonly MethodListTools = 'list_tools'
    private static readonly MethodListTemplates = 'list_templates'
    private static readonly MethodGetTemplate = 'get_template'
    private static readonly MethodSaveTemplate = 'save_template'
    private static readonly MethodDeleteTemplate = 'delete_template'
    private static readonly MethodCreateGroup = 'create_group'
    private static readonly MethodRenameGroup = 'rename_group'
    private static readonly MethodDeleteGroup = 'delete_group'
    private static readonly MethodMoveTemplate = 'move_template'
    private static readonly MethodListCatalog = 'list_catalog'
    private static readonly MethodListAllTools = 'list_all_tools'
    private static readonly MethodListSessions = 'list_sessions'
    private static readonly MethodCreateSession = 'create_session'
    private static readonly MethodAttachSession = 'attach_session'
    private static readonly MethodRenameSession = 'rename_session'
    private static readonly MethodDeleteSession = 'delete_session'
    private static readonly MethodLoadSessionModel = 'load_session_model'
    private static readonly MethodUnloadSessionModel = 'unload_session_model'
    private static readonly MethodSetThinking = 'set_thinking'
    private static readonly MethodReset = 'reset'
    private static readonly NoteSessionsChanged = 'sessions_changed'
    private static readonly NoteTemplatesChanged = 'templates_changed'
    private static readonly NotePlacementChanged = 'placement_changed'
    private static readonly NoteSessionLoading = 'session_loading'
    private static readonly NoteSessionBusy = 'session_busy'
    private static readonly NoteTurnStarted = 'turn_started'
    private static readonly NoteTurnChunk = 'turn_chunk'
    private static readonly NoteTurnDone = 'turn_done'
    private static readonly NoteTurnError = 'turn_error'
    private static readonly NoteTurnRejected = 'turn_rejected'
    private static readonly NoteToolCall = 'tool_call'
    private static readonly NoteToolResult = 'tool_result'
    private static readonly NoteLoopDone = 'loop_done'

    public readonly OnTurnStarted = new TypedSignal<TurnStarted>()
    public readonly OnTurnChunk = new TypedSignal<TurnChunk>()
    public readonly OnTurnDone = new TypedSignal<Record<string, never>>()
    public readonly OnTurnError = new TypedSignal<TurnError>()
    public readonly OnTurnRejected = new TypedSignal<TurnRejected>()
    public readonly OnToolCall = new TypedSignal<ToolCallNote>()
    public readonly OnToolResult = new TypedSignal<ToolResultNote>()
    public readonly OnLoopDone = new TypedSignal<LoopDone>()
    public readonly OnSessionsChanged = new TypedSignal<Record<string, never>>()
    public readonly OnTemplatesChanged = new TypedSignal<Record<string, never>>()
    public readonly OnPlacementChanged = new TypedSignal<PlacementChanged>()
    public readonly OnSessionLoading = new TypedSignal<SessionLoading>()
    public readonly OnSessionBusy = new TypedSignal<SessionBusy>()

    public ModelProps(): Promise<ModelProps>
    {
        return this.Request<ModelProps>(RuntimeClient.MethodModelProps, {})
    }

    public Tokenize(text: string): Promise<TokenizeResult>
    {
        return this.Request<TokenizeResult>(RuntimeClient.MethodTokenize, { text })
    }

    public async SetSampling(config: SamplingConfig): Promise<void>
    {
        await this.Request<null>(RuntimeClient.MethodSetSampling, { config })
    }

    /** The agent runtime's tool catalogue (a plain worker rejects this). */
    public ListTools(): Promise<ToolInfo[]>
    {
        return this.Request<ToolInfo[]>(RuntimeClient.MethodListTools, {})
    }

    /** Resolves when the send_turn response arrives, i.e. after the stream's terminal notification. */
    public async SendTurn(content: string, enableThinking: boolean): Promise<void>
    {
        await this.Request<null>(RuntimeClient.MethodSendTurn, { content, enable_thinking: enableThinking })
    }

    public ListTemplates(): Promise<TemplateNode[]>
    {
        return this.Request<TemplateNode[]>(RuntimeClient.MethodListTemplates, {})
    }

    public GetTemplate(id: string): Promise<TemplateRecord>
    {
        return this.Request<TemplateRecord>(RuntimeClient.MethodGetTemplate, { id })
    }

    /** Creates a template when id is omitted, else replaces it; resolves to the id. The server defaults a missing path to the root. */
    public async SaveTemplate(id: string | undefined, path: string | undefined, template: AgentTemplate): Promise<string>
    {
        const params: { id?: string, path?: string, template: AgentTemplate } = { template }
        if (id !== undefined)
        {
            params.id = id
        }
        if (path !== undefined)
        {
            params.path = path
        }
        const saved = await this.Request<{ id: string }>(RuntimeClient.MethodSaveTemplate, params)
        return saved.id
    }

    public async DeleteTemplate(id: string): Promise<void>
    {
        await this.Request<null>(RuntimeClient.MethodDeleteTemplate, { id })
    }

    public async CreateGroup(path: string): Promise<void>
    {
        await this.Request<null>(RuntimeClient.MethodCreateGroup, { path })
    }

    public async RenameGroup(path: string, newName: string): Promise<void>
    {
        await this.Request<null>(RuntimeClient.MethodRenameGroup, { path, new_name: newName })
    }

    public async DeleteGroup(path: string): Promise<void>
    {
        await this.Request<null>(RuntimeClient.MethodDeleteGroup, { path })
    }

    public async MoveTemplate(id: string, path: string): Promise<void>
    {
        await this.Request<null>(RuntimeClient.MethodMoveTemplate, { id, path })
    }

    public ListCatalog(): Promise<CatalogInfo>
    {
        return this.Request<CatalogInfo>(RuntimeClient.MethodListCatalog, {})
    }

    /** Every tool in the runtime registry (ListTools is the attached session own subset). */
    public ListAllTools(): Promise<ToolSpec[]>
    {
        return this.Request<ToolSpec[]>(RuntimeClient.MethodListAllTools, {})
    }

    public ListSessions(): Promise<SessionSummary[]>
    {
        return this.Request<SessionSummary[]>(RuntimeClient.MethodListSessions, {})
    }

    public CreateSession(templateId: string): Promise<SessionSummary>
    {
        return this.Request<SessionSummary>(RuntimeClient.MethodCreateSession, { template_id: templateId })
    }

    /** Attaches this connection to the session (replacing any earlier attachment); chat methods then act on it. */
    public AttachSession(id: string): Promise<AttachedSession>
    {
        return this.Request<AttachedSession>(RuntimeClient.MethodAttachSession, { id })
    }

    public async RenameSession(id: string, title: string): Promise<void>
    {
        await this.Request<null>(RuntimeClient.MethodRenameSession, { id, title })
    }

    public async DeleteSession(id: string): Promise<void>
    {
        await this.Request<null>(RuntimeClient.MethodDeleteSession, { id })
    }

    public LoadSessionModel(id: string): Promise<Placement>
    {
        return this.Request<Placement>(RuntimeClient.MethodLoadSessionModel, { id })
    }

    public async UnloadSessionModel(id: string): Promise<void>
    {
        await this.Request<null>(RuntimeClient.MethodUnloadSessionModel, { id })
    }

    public async SetThinking(enable: boolean): Promise<void>
    {
        await this.Request<null>(RuntimeClient.MethodSetThinking, { enable })
    }

    /** Resets the attached session transcript to its initial state. */
    public async Reset(): Promise<void>
    {
        await this.Request<null>(RuntimeClient.MethodReset, {})
    }

    protected Dispatch(method: string, params: unknown): void
    {
        switch (method)
        {
            case RuntimeClient.NoteSessionsChanged:
                this.OnSessionsChanged.Raise({})
                break
            case RuntimeClient.NoteTemplatesChanged:
                this.OnTemplatesChanged.Raise({})
                break
            case RuntimeClient.NotePlacementChanged:
                this.OnPlacementChanged.Raise(params as PlacementChanged)
                break
            case RuntimeClient.NoteSessionLoading:
                this.OnSessionLoading.Raise(params as SessionLoading)
                break
            case RuntimeClient.NoteSessionBusy:
                this.OnSessionBusy.Raise(params as SessionBusy)
                break
            case RuntimeClient.NoteTurnStarted:
                this.OnTurnStarted.Raise(params as TurnStarted)
                break
            case RuntimeClient.NoteTurnChunk:
                this.OnTurnChunk.Raise(params as TurnChunk)
                break
            case RuntimeClient.NoteTurnDone:
                this.OnTurnDone.Raise({})
                break
            case RuntimeClient.NoteTurnError:
                this.OnTurnError.Raise(params as TurnError)
                break
            case RuntimeClient.NoteTurnRejected:
                this.OnTurnRejected.Raise(params as TurnRejected)
                break
            case RuntimeClient.NoteToolCall:
                this.OnToolCall.Raise(params as ToolCallNote)
                break
            case RuntimeClient.NoteToolResult:
                this.OnToolResult.Raise(params as ToolResultNote)
                break
            case RuntimeClient.NoteLoopDone:
                this.OnLoopDone.Raise(params as LoopDone)
                break
        }
    }
}
