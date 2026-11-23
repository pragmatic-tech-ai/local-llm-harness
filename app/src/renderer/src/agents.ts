import type { SamplingConfig } from './RuntimeClient'

/** Wire shapes of the runtime's template and session protocol. Fields keep the wire's snake_case. */

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
    sampling?: SamplingConfig
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
}

export interface Placement
{
    state: 'ready' | 'needs_load' | 'unavailable'
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

export interface ChatMessage
{
    role: 'system' | 'user' | 'assistant' | 'tool'
    content: string
    tool_calls?: unknown[]
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

export interface CatalogInfo
{
    runtimes: Record<string, unknown>[]
    models: Record<string, unknown>[]
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
