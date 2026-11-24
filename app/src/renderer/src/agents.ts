/** Wire shapes of the runtime's template and session protocol. Fields keep the wire's snake_case. */

export interface SamplingConfig
{
    temperature: number
    top_p: number
    top_k: number
    repeat_penalty: number
    seed: number
    max_tokens: number
}

/** How a session keeps its prompt inside the window: nothing, trim old results and turns, or summarize them. */
export type ContextStrategy = 'off' | 'trim' | 'summarize'

/** Where a session's file tools write: its own folder, one shared folder, or a named folder. */
export type WorkspaceMode = 'session' | 'shared' | 'named'

export interface WorkspaceConfig
{
    mode: WorkspaceMode
    /** Only with `named`. */
    name?: string
}

export interface AgentTemplate
{
    name: string
    system_prompt: string
    tools: string[]
    step_budget: number
    runtime: string
    model: string
    n_ctx: number
    enable_thinking: boolean
    /** Absent in a template saved before compaction existed: read as `off`. */
    context_strategy?: ContextStrategy
    /** Percent of the window the prompt may fill before compaction runs (10 to 95); absent reads as 75. */
    compact_at_pct?: number
    /** Optional sampling defaults; a missing field takes the worker default. */
    sampling?: Partial<SamplingConfig>
    /** Absent in a template saved before workspaces existed: read as `session`. */
    workspace?: WorkspaceConfig
    /** The repository the model comes from; absent reads as `hive`. */
    repository?: string
    /** The old name of `repository`, still read (a save never sends it). */
    host?: string
}

/** Which repository a template names: `repository`, else the legacy `host`, else `hive`. */
export class TemplateRepository
{
    /** The Pragmatic repository a template without one runs on. */
    public static readonly Default = 'hive'

    public static Of(template: AgentTemplate): string
    {
        return template.repository ?? template.host ?? TemplateRepository.Default
    }
}

export interface TemplateGroupNode
{
    kind: 'group'
    name: string
    path: string
    children: TemplateNode[]
}

export interface TemplateLeafNode
{
    kind: 'template'
    id: string
    path: string
    name: string
    runtime: string
    model: string
    error: string | null
}

export type TemplateNode = TemplateGroupNode | TemplateLeafNode

export interface TemplateRecord
{
    id: string
    path: string
    template: AgentTemplate
    /** Why the template fails validation (it parsed, so `template` is real); null when it passes. */
    error: string | null
}

/** Where a session's model stands on its host (the wire `placement.state`). */
export enum PlacementState
{
    Ready = 'ready',
    NeedsLoad = 'needs_load',
    Unavailable = 'unavailable',
}

export interface Placement
{
    state: PlacementState
    host?: string
    evicts?: string | null
    reason?: string
}

export interface SessionSummary
{
    id: string
    title: string
    template_id: string
    template_name: string
    runtime: string
    model: string
    updated: number
    placement: Placement
    busy: boolean
    /** The provider's capabilities; null when the session's repository is gone (absent from an older runtime). */
    capabilities?: ProviderCapabilities | null
}

/** One OpenAI-style call an assistant message carries in the committed transcript. */
export interface ChatToolCall
{
    id: string
    type: string
    function: { name: string, arguments: unknown }
}

export interface ChatMessage
{
    role: 'system' | 'user' | 'assistant' | 'tool'
    content: string
    tool_calls?: ChatToolCall[]
    tool_call_id?: string
    tool_result?: unknown
}

export interface AttachedSession
{
    summary: SessionSummary
    snapshot: AgentTemplate
    transcript: ChatMessage[]
    sampling: SamplingConfig
    enable_thinking: boolean
    placement: Placement
    /** The provider's capabilities; null when the session's repository is gone (absent from an older runtime). */
    capabilities?: ProviderCapabilities | null
}

export interface CatalogRuntime
{
    name: string
    kind: string
}

export interface CatalogModel
{
    name: string
    /** The runtimes able to load this model. */
    runtimes: string[]
    size_hint: unknown
}

export interface CatalogInfo
{
    runtimes: CatalogRuntime[]
    models: CatalogModel[]
    /** Kept for older apps; the editor reads `repositories`. */
    hosts: string[]
    /** Absent from a runtime older than repositories. */
    repositories?: CatalogRepository[]
}

export interface ToolSpec
{
    name: string
    description: string
    parameters: unknown
    side_effects: boolean
}

export interface NotesResult
{
    notes: string[]
}

export interface NotesChanged
{
    id: string
    notes: string[]
}

export interface PlacementChanged
{
    id: string
    placement: Placement
}

export interface SessionLoading
{
    id: string
}

export interface SessionBusy
{
    id: string
    busy: boolean
}

export type StepEnd = 'tool_calls' | 'answered' | 'budget' | 'rejected' | 'error' | 'aborted'

export interface ToolTiming
{
    name: string
    id: string
    ok: boolean
    ms: number
    bytes: number
    /** Set when the call repeated a call that just failed the same way. */
    repeat_failure?: boolean
}

/** What compaction did to a step's prompt. Present on a step only when compaction ran. */
export interface CompactionRecord
{
    strategy: 'trim' | 'summarize'
    before_tokens: number
    after_tokens: number
    elided_results: number
    dropped_turns: number
    summarized_messages: number
    summary_ms: number | null
    reused_summary: boolean
    fallback: boolean
    summary_prompt_tokens: number | null
    summary_out_tokens: number | null
    summary_error: string | null
    /** The provider-reported cost of the summary calls (USD); absent when none was reported. */
    summary_cost?: number
}

/** One model call of a turn. `v` and `kind` are the log-line envelope; `kind` discriminates it from a TurnRecord. */
export interface StepRecord
{
    v: number
    kind: 'step'
    turn: number
    step: number
    at: number
    /** Null for a step that never reached the worker (a step-0 link or load failure). */
    prompt_tokens: number | null
    n_ctx: number | null
    out_tokens: number
    think_tokens: number
    load_ms: number | null
    ttft_ms: number | null
    gen_ms: number | null
    end: StepEnd
    tools: ToolTiming[]
    error: string | null
    compaction?: CompactionRecord | null
    /** The provider-reported cost of the step (USD); absent when none was reported. */
    cost?: number
    /** The prompt token count is an estimate (absent = exact). */
    estimated?: boolean
}

/** A turn's closing summary. `reason` is a loop_done reason or 'aborted'. */
export interface TurnRecord
{
    v: number
    kind: 'turn'
    turn: number
    at: number
    steps: number
    reason: string
    total_ms: number
    out_tokens: number
    think_tokens: number
    peak_fill: number
    committed: boolean
    user: string
}

/** A turn as paged by get_trace; `summary` is null while running or after a crash. */
export interface TraceTurn
{
    turn: number
    steps: StepRecord[]
    summary: TurnRecord | null
    reset_before: boolean
}

export interface TracePage
{
    turns: TraceTurn[]
    more: boolean
    reset_pending: boolean
}

export interface TraceStepNote
{
    id: string
    record: StepRecord
}

export interface TraceTurnNote
{
    id: string
    record: TurnRecord
}

export interface TraceWarning
{
    id: string
    message: string
}

/** How a repository serves its models (the wire `kind`). */
export enum RepositoryKind
{
    Pragmatic = 'pragmatic',
    OpenRouter = 'openrouter',
}

/** What a repository's provider can do (the wire `capabilities`). */
export interface ProviderCapabilities
{
    measure_prompt: 'exact' | 'estimated'
    tokenize: boolean
    raw_prompt: boolean
    logprobs: boolean
    load_unload: boolean
}

/** One `list_catalog` repository: the editor's source for repository and model choices. */
export interface CatalogRepository
{
    name: string
    kind: RepositoryKind
    capabilities: ProviderCapabilities
    models: (CatalogModel | OpenRouterModelInfo)[]
}

/** One `list_repositories` row. The key itself never travels; `key_hint` is its last 4 characters. */
export interface RepositorySummary
{
    name: string
    kind: RepositoryKind
    editable: boolean
    capabilities: ProviderCapabilities
    model_count: number
    has_key?: boolean
    key_hint?: string
    id_prefixes?: string[]
    /** Unix seconds of the last model-list fetch. */
    fetched_at?: number
    error?: string
}

/** One OpenRouter model. Prices are USD per million tokens. */
export interface OpenRouterModelInfo
{
    id: string
    name: string
    context_length: number
    prompt_price: number
    completion_price: number
    reasoning: boolean
}

/** A `list_repository_models` result: a Pragmatic repository lists catalog models, an OpenRouter one its served list. */
export interface RepositoryModels
{
    models: (CatalogModel | OpenRouterModelInfo)[]
    fetched_at: number | null
    error?: string
}

/** `save_repository` params: an omitted `api_key` keeps the stored one; `previous_name` names the one updated. */
export interface SaveRepositoryParams
{
    name: string
    previous_name?: string
    id_prefixes: string[]
    api_key?: string
}

export interface SaveRepositoryResult
{
    name: string
    fetch_error?: string
}
