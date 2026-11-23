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
    /** Optional sampling defaults; a missing field takes the worker default. */
    sampling?: Partial<SamplingConfig>
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
