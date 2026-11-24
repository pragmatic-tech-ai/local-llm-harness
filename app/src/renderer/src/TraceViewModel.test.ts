import { describe, expect, it } from 'vitest'
import { RuntimeClient } from './RuntimeClient'
import { FakeSocket } from './FakeSocket'
import { TraceViewModel, type TraceTurnViewModel } from './TraceViewModel'
import type { StepRecord, TracePage, TraceTurn, TurnRecord } from './agents'

class Rig
{
    public readonly socket = new FakeSocket()
    public readonly client = new RuntimeClient(() => this.socket)
    public readonly trace = new TraceViewModel()

    constructor()
    {
        this.client.Connect('ws://runtime')
        this.socket.Open()
    }

    public static Step(turn: number, step: number, over: Partial<StepRecord> = {}): StepRecord
    {
        return {
            v: 1, kind: 'step', turn, step, at: 1, prompt_tokens: 3102, n_ctx: 8192, out_tokens: 38, think_tokens: 0,
            load_ms: null, ttft_ms: 1900, gen_ms: 2600, end: 'tool_calls', tools: [], error: null, ...over,
        }
    }

    public static Summary(turn: number, over: Partial<TurnRecord> = {}): TurnRecord
    {
        return {
            v: 1, kind: 'turn', turn, at: 2, steps: 2, reason: 'answered', total_ms: 21400, out_tokens: 59, think_tokens: 0,
            peak_fill: 0.41, committed: true, user: 'hi', ...over,
        }
    }

    public static Turn(turn: number, over: Partial<TraceTurn> = {}): TraceTurn
    {
        return { turn, steps: [Rig.Step(turn, 1)], summary: Rig.Summary(turn, { steps: 1 }), reset_before: false, ...over }
    }

    public Frames(method: string): { id: number, method: string, params: Record<string, unknown> }[]
    {
        return this.socket.sent.map(f => JSON.parse(f)).filter(f => f.method === method)
    }

    public async Respond(method: string, result: unknown): Promise<void>
    {
        const id = this.Frames(method).pop()?.id ?? -1
        this.client.HandleFrame(JSON.stringify({ id, result }))
        await new Promise(r => setTimeout(r, 0))
    }

    public Notify(method: string, params: unknown): void
    {
        this.client.HandleFrame(JSON.stringify({ method, params }))
    }

    public async Show(id: string, page: TracePage): Promise<void>
    {
        const done = this.trace.ShowSession(this.client, id)
        await this.Respond('get_trace', page)
        await done
    }

    public Turn(index: number): TraceTurnViewModel
    {
        return this.trace.Turns.Get(index)!
    }

    public Headers(): string[]
    {
        return this.trace.Turns.ToArray().map(t => t.HeaderText)
    }
}

describe('TraceViewModel', () =>
{
    it('first_page_lists_turns_newest_first_with_headers', async () =>
    {
        const rig = new Rig()
        const turn3 = Rig.Turn(3, { steps: [Rig.Step(3, 1), Rig.Step(3, 2, { end: 'answered' })], summary: Rig.Summary(3) })
        const turn2 = Rig.Turn(2, { summary: Rig.Summary(2, { reason: 'budget', steps: 8, total_ms: 94000, peak_fill: 0.876 }) })
        await rig.Show('s1', { turns: [turn3, turn2], more: false, reset_pending: false })
        expect(rig.Frames('get_trace')[0].params).toEqual({ id: 's1' })
        expect(rig.Headers()).toEqual([
            'Turn 3 · answered · 2 steps · 21.4 s · peak 41%',
            'Turn 2 · budget · 8 steps · 94.0 s · peak 88%',
        ])
        expect(rig.Turn(0).Expanded).toBe(true)
        expect(rig.Turn(1).Expanded).toBe(false)
        expect(rig.Turn(0).Steps.Count).toBe(2)
        rig.Turn(1).Expanded = true
        expect(rig.Turn(1).Expanded).toBe(true)
    })

    it('step_text_formats_fill_tokens_and_timing', async () =>
    {
        const rig = new Rig()
        const steps = [
            Rig.Step(1, 1),
            Rig.Step(1, 2, { think_tokens: 5, load_ms: 12345, ttft_ms: null, gen_ms: 0 }),
            Rig.Step(1, 3, { prompt_tokens: null as unknown as number, n_ctx: null as unknown as number, ttft_ms: null, gen_ms: null, out_tokens: 0, end: 'error', error: 'link failed' }),
        ]
        await rig.Show('s1', { turns: [Rig.Turn(1, { steps })], more: false, reset_pending: false })
        const [first, second, third] = rig.Turn(0).Steps.ToArray()
        expect(first.Text).toBe('1  ctx 3 102/8 192 (38%)')
        expect(first.StatsText).toBe('out 38  ttft 1.9 s  gen 2.6 s  14.6 t/s')
        expect(first.FillFraction).toBeCloseTo(3102 / 8192)
        expect(first.HasError).toBe(false)
        expect(second.StatsText).toBe('out 38 (think 5)  load 12.3 s  gen 0.0 s')
        expect(third.Text).toBe('3  —')
        expect(third.FillFraction).toBe(0)
        expect(third.StatsText).toBe('out 0')
        expect(third.ErrorText).toBe('link failed')
        expect(third.HasError).toBe(true)
    })

    it('tool_rows_format', async () =>
    {
        const rig = new Rig()
        const tools = [
            { name: 'read_file', id: 'c1', ok: true, ms: 4, bytes: 26 },
            { name: 'fetch', id: 'c2', ok: false, ms: 1200, bytes: 4096 },
        ]
        const aborted = Rig.Step(1, 2, { end: 'aborted' })
        await rig.Show('s1', { turns: [Rig.Turn(1, { steps: [Rig.Step(1, 1, { tools }), aborted] })], more: false, reset_pending: false })
        const [step, last] = rig.Turn(0).Steps.ToArray()
        expect(step.Tools).toEqual(['└ read_file  ok  4 ms  26 B', '└ fetch  failed  1 200 ms  4 096 B'])
        expect(step.HasTools).toBe(true)
        expect(last.ErrorText).toBe('aborted')
        expect(last.HasTools).toBe(false)
    })

    it('live_step_then_turn_updates_in_place', async () =>
    {
        const rig = new Rig()
        await rig.Show('s1', { turns: [Rig.Turn(1)], more: false, reset_pending: false })
        rig.Notify('trace_step', { id: 's1', record: Rig.Step(2, 1) })
        expect(rig.Headers()).toEqual(['Turn 2 · running…', 'Turn 1 · answered · 1 steps · 21.4 s · peak 41%'])
        expect(rig.Turn(0).Expanded).toBe(true)
        expect(rig.Turn(1).Expanded).toBe(false)
        const row = rig.Turn(0)
        rig.Notify('trace_step', { id: 's1', record: Rig.Step(2, 2, { end: 'answered' }) })
        expect(row.Steps.Count).toBe(2)
        rig.Notify('trace_turn', { id: 's1', record: Rig.Summary(2) })
        expect(rig.Turn(0)).toBe(row)
        expect(row.HeaderText).toBe('Turn 2 · answered · 2 steps · 21.4 s · peak 41%')
        expect(rig.trace.Turns.Count).toBe(2)
    })

    it('live_record_replaces_paged_duplicate', async () =>
    {
        const rig = new Rig()
        // Live first, page later: the page's copy of step 1 does not overwrite the live one.
        const done = rig.trace.ShowSession(rig.client, 's1')
        rig.Notify('trace_step', { id: 's1', record: Rig.Step(4, 1, { out_tokens: 99 }) })
        await rig.Respond('get_trace', { turns: [Rig.Turn(4, { steps: [Rig.Step(4, 1, { out_tokens: 10 })], summary: null })], more: false, reset_pending: false })
        await done
        expect(rig.trace.Turns.Count).toBe(1)
        expect(rig.Turn(0).Steps.Count).toBe(1)
        expect(rig.Turn(0).Steps.Get(0)!.StatsText.startsWith('out 99')).toBe(true)
        expect(rig.Turn(0).HeaderText).toBe('Turn 4 · running…')
        // Page first, live later: the live record replaces the paged one in place.
        rig.Notify('trace_step', { id: 's1', record: Rig.Step(4, 1, { out_tokens: 7 }) })
        expect(rig.Turn(0).Steps.Count).toBe(1)
        expect(rig.Turn(0).Steps.Get(0)!.StatsText.startsWith('out 7')).toBe(true)
        rig.Notify('trace_turn', { id: 's1', record: Rig.Summary(4, { steps: 1, reason: 'answered' }) })
        rig.Notify('trace_turn', { id: 's1', record: Rig.Summary(4, { steps: 1, reason: 'aborted' }) })
        expect(rig.trace.Turns.Count).toBe(1)
        expect(rig.Turn(0).HeaderText).toBe('Turn 4 · aborted · 1 steps · 21.4 s · peak 41%')
    })

    it('turn_without_summary_and_not_live_is_incomplete', async () =>
    {
        const rig = new Rig()
        await rig.Show('s1', { turns: [Rig.Turn(5, { summary: null }), Rig.Turn(4)], more: false, reset_pending: false })
        expect(rig.Turn(0).HeaderText).toBe('Turn 5 · incomplete')
        // A live step for that paged turn makes it the live turn.
        rig.Notify('trace_step', { id: 's1', record: Rig.Step(5, 2) })
        expect(rig.Turn(0).HeaderText).toBe('Turn 5 · running…')
        // Re-shown (reconnect): the same turn, from the page only, is incomplete again.
        await rig.Show('s1', { turns: [Rig.Turn(5, { summary: null })], more: false, reset_pending: false })
        expect(rig.Headers()).toEqual(['Turn 5 · incomplete'])
    })

    it('load_older_appends_and_disables_at_end', async () =>
    {
        const rig = new Rig()
        await rig.Show('s1', { turns: [Rig.Turn(9), Rig.Turn(8)], more: true, reset_pending: false })
        expect(rig.trace.LoadOlderCommand.CanExecute(undefined)).toBe(true)
        expect(rig.trace.HasMore).toBe(true)
        rig.trace.LoadOlderCommand.Execute(undefined)
        expect(rig.Frames('get_trace').pop()!.params).toEqual({ id: 's1', before: 8 })
        // While the page is in flight a second click is off.
        expect(rig.trace.LoadOlderCommand.CanExecute(undefined)).toBe(false)
        await rig.Respond('get_trace', { turns: [Rig.Turn(7), Rig.Turn(6)], more: false, reset_pending: true })
        expect(rig.trace.Turns.ToArray().map(t => t.Turn)).toEqual([9, 8, 7, 6])
        expect(rig.Turn(2).Expanded).toBe(false)
        expect(rig.trace.LoadOlderCommand.CanExecute(undefined)).toBe(false)
        expect(rig.trace.HasMore).toBe(false)
        // reset_pending only counts on the first page.
        expect(rig.trace.ResetPending).toBe(false)
    })

    it('reset_dividers_and_pending_reset', async () =>
    {
        const rig = new Rig()
        await rig.Show('s1', { turns: [Rig.Turn(2, { reset_before: true }), Rig.Turn(1)], more: false, reset_pending: true })
        expect(rig.trace.ResetPending).toBe(true)
        expect(rig.Turn(0).ResetBefore).toBe(true)
        expect(rig.Turn(1).ResetBefore).toBe(false)
        // The next live turn follows the pending reset: the divider moves below it.
        rig.Notify('trace_step', { id: 's1', record: Rig.Step(3, 1) })
        expect(rig.trace.ResetPending).toBe(false)
        expect(rig.Turn(0).Turn).toBe(3)
        expect(rig.Turn(0).ResetBefore).toBe(true)
        // An empty trace with a reset still shows the divider at the top.
        await rig.Show('s2', { turns: [], more: false, reset_pending: true })
        expect(rig.trace.ResetPending).toBe(true)
        expect(rig.trace.Turns.Count).toBe(0)
    })

    it('warning_shows_and_clears_on_switch', async () =>
    {
        const rig = new Rig()
        await rig.Show('s1', { turns: [], more: false, reset_pending: false })
        expect(rig.trace.HasWarning).toBe(false)
        rig.Notify('trace_warning', { id: 's1', message: 'trace write failed: disk full' })
        expect(rig.trace.HasWarning).toBe(true)
        expect(rig.trace.WarningText).toBe('trace write failed: disk full')
        await rig.Show('s2', { turns: [], more: false, reset_pending: false })
        expect(rig.trace.HasWarning).toBe(false)
        expect(rig.trace.WarningText).toBe('')
        // A failed page load is shown the same way.
        const done = rig.trace.ShowSession(rig.client, 's3')
        rig.client.HandleFrame(JSON.stringify({ id: rig.Frames('get_trace').pop()!.id, error: { code: -32602, message: 'unknown session: s3' } }))
        await done
        expect(rig.trace.WarningText).toBe('trace unavailable: unknown session: s3')
    })

    it('signals_for_other_sessions_are_ignored', async () =>
    {
        const rig = new Rig()
        await rig.Show('s1', { turns: [], more: false, reset_pending: false })
        rig.Notify('trace_step', { id: 'other', record: Rig.Step(1, 1) })
        rig.Notify('trace_turn', { id: 'other', record: Rig.Summary(1) })
        rig.Notify('trace_warning', { id: 'other', message: 'x' })
        expect(rig.trace.Turns.Count).toBe(0)
        expect(rig.trace.HasWarning).toBe(false)
        // Nothing shown: no session, nothing applied, and a late page for the old session is dropped.
        const late = rig.trace.ShowSession(rig.client, 's1')
        await rig.trace.ShowSession(null, null)
        await rig.Respond('get_trace', { turns: [Rig.Turn(1)], more: true, reset_pending: false })
        await late
        rig.Notify('trace_step', { id: 's1', record: Rig.Step(2, 1) })
        expect(rig.trace.Turns.Count).toBe(0)
        expect(rig.trace.HasMore).toBe(false)
    })
})
