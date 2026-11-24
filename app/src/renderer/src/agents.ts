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
    host?: string
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
    hosts: string[]
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
