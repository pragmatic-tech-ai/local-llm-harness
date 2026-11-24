import { JsonRpcConnection, TypedSignal } from './JsonRpcConnection'
import type {
    AgentTemplate,
    AttachedSession,
    CatalogInfo,
    NotesChanged,
    NotesResult,
    Placement,
    PlacementChanged,
    RepositoryModels,
    RepositorySummary,
    SamplingConfig,
    SaveRepositoryParams,
    SaveRepositoryResult,
    SessionBusy,
    SessionLoading,
    SessionSummary,
    TemplateNode,
    TemplateRecord,
    ToolSpec,
    TracePage,
    TraceStepNote,
    TraceTurnNote,
    TraceWarning
} from './agents'

export type { SamplingConfig }

export { TypedSignal }
export type { IHarnessSocket, HarnessSocketFactory, IDisposable } from './JsonRpcConnection'

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
    /** Null from a provider that has no raw prompt to show. */
    prompt: string | null
    prompt_tokens: number
    n_ctx: number
    /** The prompt token count is an estimate (absent = exact). */
    estimated?: boolean
}

export interface TurnChunk
{
    text: string
    tokens: TokenInfo[]
    stop: boolean
    /** Reasoning text, shown apart from the answer and never committed (absent = answer text). */
    reasoning?: boolean
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

/** JSON-RPC client for the agent runtime: templates, sessions, catalog and the attached session chat stream. */
export class RuntimeClient extends JsonRpcConnection
{
    private static readonly MethodModelProps = 'model_props'
    private static readonly MethodTokenize = 'tokenize'
    private static readonly MethodSetSampling = 'set_sampling'
    private static readonly MethodSendTurn = 'send_turn'
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
    private static readonly MethodGetTrace = 'get_trace'
    private static readonly MethodGetNotes = 'get_notes'
    private static readonly MethodListRepositories = 'list_repositories'
    private static readonly MethodListRepositoryModels = 'list_repository_models'
    private static readonly MethodSaveRepository = 'save_repository'
    private static readonly MethodDeleteRepository = 'delete_repository'
    private static readonly NoteRepositoriesChanged = 'repositories_changed'
    private static readonly NoteNotesChanged = 'notes_changed'
    private static readonly NoteTraceStep = 'trace_step'
    private static readonly NoteTraceTurn = 'trace_turn'
    private static readonly NoteTraceWarning = 'trace_warning'
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
    public readonly OnTraceStep = new TypedSignal<TraceStepNote>()
    public readonly OnTraceTurn = new TypedSignal<TraceTurnNote>()
    public readonly OnTraceWarning = new TypedSignal<TraceWarning>()
    public readonly OnNotesChanged = new TypedSignal<NotesChanged>()
    /** A repository was saved or deleted, or its model list refreshed. */
    public readonly OnRepositoriesChanged = new TypedSignal<Record<string, never>>()

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

    /** Resolves when the send_turn response arrives, i.e. after the stream's terminal notification. */
    public async SendTurn(content: string): Promise<void>
    {
        await this.Request<null>(RuntimeClient.MethodSendTurn, { content })
    }

    public ListTemplates(): Promise<TemplateNode[]>
    {
        return this.Request<TemplateNode[]>(RuntimeClient.MethodListTemplates, {})
    }

    /** The template; `error` says why it fails validation (null when it passes, or from a server without the field). */
    public async GetTemplate(id: string): Promise<TemplateRecord>
    {
        const record = await this.Request<TemplateRecord>(RuntimeClient.MethodGetTemplate, { id })
        return { ...record, error: record.error ?? null }
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

    /** Every tool in the runtime registry (the template editor's checkboxes). */
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

    /** A page of the session's trace, newest turns first; `before` is a turn number to page back from. Needs no attach. */
    public GetTrace(id: string, before?: number, limit?: number): Promise<TracePage>
    {
        const params: { id: string, before?: number, limit?: number } = { id }
        if (before !== undefined)
        {
            params.before = before
        }
        if (limit !== undefined)
        {
            params.limit = limit
        }
        return this.Request<TracePage>(RuntimeClient.MethodGetTrace, params)
    }

    /** The session's working notes (needs no attach). */
    public GetNotes(id: string): Promise<NotesResult>
    {
        return this.Request<NotesResult>(RuntimeClient.MethodGetNotes, { id })
    }

    /** Every repository (Pragmatic first, then OpenRouter in file order); never a key, only `key_hint`. */
    public ListRepositories(): Promise<RepositorySummary[]>
    {
        return this.Request<RepositorySummary[]>(RuntimeClient.MethodListRepositories, {})
    }

    /** A repository's models; `refresh` refetches an OpenRouter list. */
    public ListRepositoryModels(name: string, refresh: boolean): Promise<RepositoryModels>
    {
        return this.Request<RepositoryModels>(RuntimeClient.MethodListRepositoryModels, { name, refresh })
    }

    /** Creates, updates or renames an OpenRouter repository. Only the fields set are sent (no `api_key` keeps the stored key). */
    public SaveRepository(params: SaveRepositoryParams): Promise<SaveRepositoryResult>
    {
        const sent: SaveRepositoryParams = { name: params.name, id_prefixes: params.id_prefixes }
        if (params.previous_name !== undefined)
        {
            sent.previous_name = params.previous_name
        }
        if (params.api_key !== undefined)
        {
            sent.api_key = params.api_key
        }
        return this.Request<SaveRepositoryResult>(RuntimeClient.MethodSaveRepository, sent)
    }

    public async DeleteRepository(name: string): Promise<void>
    {
        await this.Request<null>(RuntimeClient.MethodDeleteRepository, { name })
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
            case RuntimeClient.NoteRepositoriesChanged:
                this.OnRepositoriesChanged.Raise({})
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
            case RuntimeClient.NoteNotesChanged:
                this.OnNotesChanged.Raise(params as NotesChanged)
                break
            case RuntimeClient.NoteTraceStep:
                this.OnTraceStep.Raise(params as TraceStepNote)
                break
            case RuntimeClient.NoteTraceTurn:
                this.OnTraceTurn.Raise(params as TraceTurnNote)
                break
            case RuntimeClient.NoteTraceWarning:
                this.OnTraceWarning.Raise(params as TraceWarning)
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
