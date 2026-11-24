import { Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import type { IDisposable } from './JsonRpcConnection'
import type { RuntimeClient } from './RuntimeClient'
import type { CompactionRecord, StepEnd, StepRecord, TracePage, TraceTurn, TurnRecord } from './agents'
import { TextFormat } from './TextFormat'

/** Number formatting shared by the trace rows: thin-space thousands, one-decimal seconds, integer percentages. */
export class TraceNumbers
{
    /** U+2009 THIN SPACE, as in the sketch's `3 102/8 192`. */
    private static readonly ThousandsSeparator = ' '
    private static readonly ThousandsGroups = /\B(?=(\d{3})+(?!\d))/g
    private static readonly MsPerSecond = 1000
    private static readonly PercentScale = 100
    private static readonly SecondsDecimals = 1

    public static Thousands(n: number): string
    {
        return String(n).replace(TraceNumbers.ThousandsGroups, TraceNumbers.ThousandsSeparator)
    }

    public static Seconds(ms: number): string
    {
        return TraceNumbers.OneDecimal(ms / TraceNumbers.MsPerSecond)
    }

    public static OneDecimal(value: number): string
    {
        return value.toFixed(TraceNumbers.SecondsDecimals)
    }

    public static Percent(fraction: number): number
    {
        return Math.round(fraction * TraceNumbers.PercentScale)
    }

    public static PerSecond(count: number, ms: number): number
    {
        return count / (ms / TraceNumbers.MsPerSecond)
    }
}

/** One tool run of a step, as one sub-row: `└ {name}  ok|failed  {ms} ms  {bytes} B`. */
export class TraceToolViewModel extends Observable
{
    private readonly text: string

    constructor(text: string)
    {
        super()
        this.text = text
    }

    public get Text(): string
    {
        return this.text
    }
}

/** One model call of a traced turn: the fill line with its bar, the token/timing line, tool rows and any error. */
export class TraceStepViewModel extends Observable
{
    private static readonly FillFormat = '{0}  ctx {1}/{2} ({3}%)'
    private static readonly NoFillFormat = '{0}  —'
    private static readonly OutFormat = 'out {0}'
    private static readonly ThinkFormat = ' (think {0})'
    private static readonly LoadFormat = '  load {0} s'
    private static readonly TtftFormat = '  ttft {0} s'
    private static readonly GenFormat = '  gen {0} s'
    private static readonly RateFormat = '  {0} t/s'
    private static readonly ToolFormat = '└ {0}  {1}  {2} ms  {3} B'
    private static readonly TrimFormat = '⟲ trim: {0} → {1} tokens · {2} results elided · {3} turns dropped'
    private static readonly SummarizeFormat = '⟲ summarize: {0} → {1} tokens · covers {2} messages · {3} s'
    private static readonly ReusedFormat = '⟲ summarize (reused): {0} → {1} tokens · covers {2} messages'
    private static readonly FallbackFormat = '⟲ summarize failed → trim: {0} → {1} tokens · {2} results elided · {3} turns dropped'
    private static readonly FallbackErrorFormat = '  ({0})'
    private static readonly TrimOnTopFormat = ' · {0} results elided · {1} turns dropped'
    private static readonly StrategySummarize: CompactionRecord['strategy'] = 'summarize'
    private static readonly ToolOk = 'ok'
    private static readonly ToolFailed = 'failed'
    private static readonly EndRejected: StepEnd = 'rejected'
    private static readonly EndError: StepEnd = 'error'
    private static readonly EndAborted: StepEnd = 'aborted'
    /** Pixel width of a step's fill track (the panel's context bar is wider; these sit beside the fill text). */
    private static readonly BarPixels = 80

    private readonly record: StepRecord
    private readonly tools: TraceToolViewModel[]

    constructor(record: StepRecord)
    {
        super()
        this.record = record
        this.tools = record.tools.map(t => new TraceToolViewModel(TextFormat.Format(TraceStepViewModel.ToolFormat, t.name,
            t.ok ? TraceStepViewModel.ToolOk : TraceStepViewModel.ToolFailed, TraceNumbers.Thousands(t.ms), TraceNumbers.Thousands(t.bytes))))
    }

    public get Step(): number
    {
        return this.record.step
    }

    public get Text(): string
    {
        const r = this.record
        if (!TraceStepViewModel.HasFill(r))
        {
            return TextFormat.Format(TraceStepViewModel.NoFillFormat, r.step)
        }
        return TextFormat.Format(TraceStepViewModel.FillFormat, r.step, TraceNumbers.Thousands(r.prompt_tokens!),
            TraceNumbers.Thousands(r.n_ctx!), TraceNumbers.Percent(r.prompt_tokens! / r.n_ctx!))
    }

    /** Prompt tokens over the window, clamped to [0, 1]; 0 when the step never reached the worker. */
    public get FillFraction(): number
    {
        const r = this.record
        return TraceStepViewModel.HasFill(r) ? Math.min(1, Math.max(0, r.prompt_tokens! / r.n_ctx!)) : 0
    }

    public get BarTrackWidth(): number
    {
        return TraceStepViewModel.BarPixels
    }

    public get BarWidth(): number
    {
        return this.FillFraction * TraceStepViewModel.BarPixels
    }

    public get StatsText(): string
    {
        const r = this.record
        let text = TextFormat.Format(TraceStepViewModel.OutFormat, TraceNumbers.Thousands(r.out_tokens))
        if (r.think_tokens > 0)
        {
            text += TextFormat.Format(TraceStepViewModel.ThinkFormat, TraceNumbers.Thousands(r.think_tokens))
        }
        if (r.load_ms !== null)
        {
            text += TextFormat.Format(TraceStepViewModel.LoadFormat, TraceNumbers.Seconds(r.load_ms))
        }
        if (r.ttft_ms !== null)
        {
            text += TextFormat.Format(TraceStepViewModel.TtftFormat, TraceNumbers.Seconds(r.ttft_ms))
        }
        if (r.gen_ms !== null)
        {
            text += TextFormat.Format(TraceStepViewModel.GenFormat, TraceNumbers.Seconds(r.gen_ms))
            if (r.gen_ms > 0)
            {
                text += TextFormat.Format(TraceStepViewModel.RateFormat, TraceNumbers.OneDecimal(TraceNumbers.PerSecond(r.out_tokens, r.gen_ms)))
            }
        }
        return text
    }

    /** The step's compaction line (`⟲ ...`), or '' when compaction did not run. */
    public get CompactionText(): string
    {
        const c = this.record.compaction
        if (c === undefined || c === null)
        {
            return ''
        }
        const before = TraceNumbers.Thousands(c.before_tokens)
        const after = TraceNumbers.Thousands(c.after_tokens)
        if (c.strategy === TraceStepViewModel.StrategySummarize && c.fallback)
        {
            const line = TextFormat.Format(TraceStepViewModel.FallbackFormat, before, after, c.elided_results, c.dropped_turns)
            return c.summary_error !== null && c.summary_error.length > 0
                ? line + TextFormat.Format(TraceStepViewModel.FallbackErrorFormat, c.summary_error)
                : line
        }
        if (c.strategy === TraceStepViewModel.StrategySummarize)
        {
            const line = c.reused_summary
                ? TextFormat.Format(TraceStepViewModel.ReusedFormat, before, after, c.summarized_messages)
                : TextFormat.Format(TraceStepViewModel.SummarizeFormat, before, after, c.summarized_messages,
                    TraceNumbers.Seconds(c.summary_ms ?? 0))
            return line + TraceStepViewModel.TrimOnTopText(c)
        }
        return TextFormat.Format(TraceStepViewModel.TrimFormat, before, after, c.elided_results, c.dropped_turns)
    }

    /** The trim counts of a summary that trim ran on top of, or '' when it removed nothing. */
    private static TrimOnTopText(c: CompactionRecord): string
    {
        return c.elided_results > 0 || c.dropped_turns > 0
            ? TextFormat.Format(TraceStepViewModel.TrimOnTopFormat, c.elided_results, c.dropped_turns)
            : ''
    }

    public get HasCompaction(): boolean
    {
        return this.record.compaction !== undefined && this.record.compaction !== null
    }

    /** One row per tool the step ran, in run order. */
    public get Tools(): TraceToolViewModel[]
    {
        return this.tools
    }

    public get HasTools(): boolean
    {
        return this.record.tools.length > 0
    }

    /** The turn_error / turn_rejected text when the step ended rejected or error; `aborted` for an aborted step. */
    public get ErrorText(): string
    {
        switch (this.record.end)
        {
            case TraceStepViewModel.EndRejected:
            case TraceStepViewModel.EndError:
                return this.record.error ?? this.record.end
            case TraceStepViewModel.EndAborted:
                return TraceStepViewModel.EndAborted
            default:
                return ''
        }
    }

    public get HasError(): boolean
    {
        return this.ErrorText.length > 0
    }

    private static HasFill(r: StepRecord): boolean
    {
        return r.prompt_tokens !== null && r.n_ctx !== null && r.n_ctx > 0
    }
}

/** One turn of the trace: a header (summary, running… or incomplete), its steps, and a reset divider below it. */
export class TraceTurnViewModel extends Observable
{
    private static readonly SummaryFormat = 'Turn {0} · {1} · {2} steps · {3} s · peak {4}%'
    private static readonly RunningFormat = 'Turn {0} · running…'
    private static readonly IncompleteFormat = 'Turn {0} · incomplete'
    private static readonly GlyphExpanded = '▾'
    private static readonly GlyphCollapsed = '▸'
    private static readonly PropExpanded = 'Expanded'
    private static readonly PropGlyph = 'Glyph'
    private static readonly PropHeaderText = 'HeaderText'
    private static readonly PropResetBefore = 'ResetBefore'

    public readonly Steps = new ObservableCollection<TraceStepViewModel>()

    private readonly turn: number
    private summary: TurnRecord | null = null
    private live = false
    private expanded: boolean
    private userToggled = false
    private resetBefore = false

    constructor(turn: number, expanded: boolean)
    {
        super()
        this.turn = turn
        this.expanded = expanded
    }

    public get Turn(): number
    {
        return this.turn
    }

    public get Expanded(): boolean
    {
        return this.expanded
    }

    /** The user's toggle (the header's binding): from now on the row keeps its expansion when a newer turn arrives. */
    public set Expanded(value: boolean)
    {
        if (value !== this.expanded)
        {
            this.userToggled = true
            this.SetExpanded(value)
        }
    }

    /** The user has toggled this row; its expansion is no longer the default. */
    public get UserToggled(): boolean
    {
        return this.userToggled
    }

    /** A newer turn became the newest: this row collapses unless the user toggled it. */
    public CollapseByDefault(): void
    {
        if (!this.userToggled)
        {
            this.SetExpanded(false)
        }
    }

    /** The expand/collapse marker in front of the header. */
    public get Glyph(): string
    {
        return this.expanded ? TraceTurnViewModel.GlyphExpanded : TraceTurnViewModel.GlyphCollapsed
    }

    public get HeaderText(): string
    {
        const s = this.summary
        if (s !== null)
        {
            return TextFormat.Format(TraceTurnViewModel.SummaryFormat, this.turn, s.reason, s.steps,
                TraceNumbers.Seconds(s.total_ms), TraceNumbers.Percent(s.peak_fill))
        }
        return TextFormat.Format(this.live ? TraceTurnViewModel.RunningFormat : TraceTurnViewModel.IncompleteFormat, this.turn)
    }

    /** A reset came between this turn and the one before it: the divider is drawn below this row. */
    public get ResetBefore(): boolean
    {
        return this.resetBefore
    }

    public SetResetBefore(value: boolean): void
    {
        const old = this.resetBefore
        if (old !== value)
        {
            this.resetBefore = value
            this.RaisePropertyChanged(TraceTurnViewModel.PropResetBefore, old, value)
        }
    }

    /** The turn's summary is known (paged or live). */
    public get HasSummary(): boolean
    {
        return this.summary !== null
    }

    /** A live step arrived for this turn, or its session is busy with it: it runs until its summary arrives. */
    public MarkLive(): void
    {
        if (!this.live)
        {
            this.UpdateHeader(() => { this.live = true })
        }
    }

    /** The session stopped being busy: a live turn whose summary never came is incomplete. */
    public EndLive(): void
    {
        if (this.live && this.summary === null)
        {
            this.UpdateHeader(() => { this.live = false })
        }
    }

    /** Sets the summary; a paged one (`replace` false) does not overwrite one already known. */
    public ApplySummary(summary: TurnRecord, replace: boolean): void
    {
        if (replace || this.summary === null)
        {
            this.UpdateHeader(() =>
            {
                this.summary = summary
                this.live = false
            })
        }
    }

    /** Inserts the step in step order; a live record (`replace` true) replaces a known one, a paged one never does. */
    public ApplyStep(record: StepRecord, replace: boolean): void
    {
        let index = 0
        for (const s of this.Steps)
        {
            if (s.Step === record.step)
            {
                if (replace)
                {
                    this.Steps.SetAt(index, new TraceStepViewModel(record))
                }
                return
            }
            if (s.Step > record.step)
            {
                break
            }
            index++
        }
        this.Steps.Insert(index, new TraceStepViewModel(record))
    }

    private SetExpanded(value: boolean): void
    {
        const old = this.expanded
        if (old !== value)
        {
            this.expanded = value
            this.RaisePropertyChanged(TraceTurnViewModel.PropExpanded, old, value)
            this.RaisePropertyChanged(TraceTurnViewModel.PropGlyph, undefined, this.Glyph)
        }
    }

    private UpdateHeader(mutate: () => void): void
    {
        const old = this.HeaderText
        mutate()
        this.RaisePropertyChanged(TraceTurnViewModel.PropHeaderText, old, this.HeaderText)
    }
}

/**
 * The shown session's agent trace, newest turn first: the first page on show, older pages on demand, and live
 * `trace_step` / `trace_turn` records folded in by `(turn, step)` (a live record replaces a paged one).
 */
export class TraceViewModel extends Observable
{
    private static readonly PageFailedFormat = 'trace unavailable: {0}'
    private static readonly WarningSeparator = ' · '
    private static readonly PropWarningText = 'WarningText'
    private static readonly PropHasWarning = 'HasWarning'
    private static readonly PropHasMore = 'HasMore'
    private static readonly PropResetPending = 'ResetPending'

    public readonly Turns = new ObservableCollection<TraceTurnViewModel>()

    private readonly loadOlderCommand: RelayCommand
    private client: RuntimeClient | null = null
    private id: string | null = null
    private subscriptions: IDisposable[] = []
    /** Orders page loads: a reply for an earlier ShowSession is dropped. */
    private token = 0
    private more = false
    private loadingOlder = false
    private resetPending = false
    /** The oldest turn number any page returned: the `before` of the next older page. */
    private oldestPaged: number | null = null
    /** The last `trace_warning` for this session: cleared by a switch to another session and by a successful page. */
    private noteWarning = ''
    /** The last failed page load: cleared by the next successful page and by every ShowSession. */
    private pageError = ''
    /** The shown session is running a turn (from the attach reply, then `session_busy`). */
    private busy = false

    constructor()
    {
        super()
        this.loadOlderCommand = new RelayCommand(() => void this.LoadOlder(), () => this.more && !this.loadingOlder)
    }

    public get LoadOlderCommand(): ICommand
    {
        return this.loadOlderCommand
    }

    /** Older pages remain: the "Load older…" row shows. */
    public get HasMore(): boolean
    {
        return this.more
    }

    /** A reset came after the newest turn: the divider is drawn above the list. */
    public get ResetPending(): boolean
    {
        return this.resetPending
    }

    public get WarningText(): string
    {
        if (this.noteWarning.length > 0 && this.pageError.length > 0)
        {
            return this.noteWarning + TraceViewModel.WarningSeparator + this.pageError
        }
        return this.noteWarning + this.pageError
    }

    public get HasWarning(): boolean
    {
        return this.WarningText.length > 0
    }

    /**
     * Shows a session's trace (null to show none): clears the list, follows that session's live records and loads the
     * first page. Called on attach, on switch and after reconnect. A `trace_warning` is kept across a same-session
     * reload until its page loads; a page failure is always dropped (the reload retries it). A successful page clears
     * both. When the session is `busy` (and still is once the page loads), its newest summary-less turn is the one
     * running; when busy ends without that turn's summary, the turn is incomplete.
     */
    public async ShowSession(client: RuntimeClient | null, id: string | null, busy = false): Promise<void>
    {
        const token = ++this.token
        this.UpdateWarning(() =>
        {
            if (id !== this.id)
            {
                this.noteWarning = ''
            }
            this.pageError = ''
        })
        this.Unsubscribe()
        this.client = client
        this.id = id
        this.busy = busy
        this.Turns.Clear()
        this.oldestPaged = null
        this.loadingOlder = false
        this.SetMore(false)
        this.SetResetPending(false)
        if (client === null || id === null)
        {
            return
        }
        this.subscriptions = [
            client.OnTraceStep.Subscribe(n =>
            {
                if (n.id === this.id)
                {
                    this.ApplyLiveStep(n.record)
                }
            }),
            client.OnTraceTurn.Subscribe(n =>
            {
                if (n.id === this.id)
                {
                    this.TurnFor(n.record.turn).ApplySummary(n.record, true)
                }
            }),
            client.OnTraceWarning.Subscribe(n =>
            {
                if (n.id === this.id)
                {
                    this.UpdateWarning(() => { this.noteWarning = n.message })
                }
            }),
            client.OnSessionBusy.Subscribe(n =>
            {
                if (n.id === this.id)
                {
                    this.SetBusy(n.busy)
                }
            }),
        ]
        let page: TracePage
        try
        {
            page = await client.GetTrace(id)
        }
        catch (e)
        {
            if (token === this.token)
            {
                this.SetPageError(TextFormat.Format(TraceViewModel.PageFailedFormat, TextFormat.Message(e)))
            }
            return
        }
        if (token !== this.token)
        {
            return
        }
        this.ApplyPage(page)
        if (this.busy)
        {
            this.MarkNewestRunning()
        }
        // Only the first page reports a reset after the newest turn. A live turn shown already came after it.
        if (page.reset_pending)
        {
            const live = this.LiveTurnAfter(page)
            if (live !== null)
            {
                live.SetResetBefore(true)
            }
            else
            {
                this.SetResetPending(true)
            }
        }
    }

    public dispose(): void
    {
        this.token++
        this.Unsubscribe()
    }

    private async LoadOlder(): Promise<void>
    {
        const client = this.client
        const id = this.id
        const before = this.oldestPaged
        if (client === null || id === null || before === null || !this.more || this.loadingOlder)
        {
            return
        }
        const token = this.token
        this.SetLoadingOlder(true)
        let page: TracePage
        try
        {
            page = await client.GetTrace(id, before)
        }
        catch (e)
        {
            if (token === this.token)
            {
                this.SetLoadingOlder(false)
                this.SetPageError(TextFormat.Format(TraceViewModel.PageFailedFormat, TextFormat.Message(e)))
            }
            return
        }
        if (token !== this.token)
        {
            return
        }
        this.SetLoadingOlder(false)
        this.ApplyPage(page)
    }

    private ApplyPage(page: TracePage): void
    {
        // A page loaded: the log reads fine again, so both a page failure and an earlier trace_warning are cleared.
        this.UpdateWarning(() =>
        {
            this.pageError = ''
            this.noteWarning = ''
        })
        for (const t of page.turns)
        {
            this.ApplyPagedTurn(t)
            this.oldestPaged = this.oldestPaged === null ? t.turn : Math.min(this.oldestPaged, t.turn)
        }
        this.SetMore(page.more)
    }

    private ApplyPagedTurn(t: TraceTurn): void
    {
        const row = this.TurnFor(t.turn)
        for (const s of t.steps)
        {
            row.ApplyStep(s, false)
        }
        if (t.summary !== null)
        {
            row.ApplySummary(t.summary, false)
        }
        row.SetResetBefore(t.reset_before)
    }

    private ApplyLiveStep(record: StepRecord): void
    {
        const isNew = this.Find(record.turn) === null
        const row = this.TurnFor(record.turn)
        if (isNew && this.resetPending && this.Turns.IndexOf(row) === 0)
        {
            // The first turn after a pending reset: the divider now sits between it and the older turns.
            row.SetResetBefore(true)
            this.SetResetPending(false)
        }
        row.MarkLive()
        row.ApplyStep(record, true)
    }

    /** The session is busy: its newest turn, when it has no summary yet, is the one running. */
    private MarkNewestRunning(): void
    {
        const newest = this.Turns.Count > 0 ? this.Turns.Get(0)! : null
        if (newest !== null && !newest.HasSummary)
        {
            newest.MarkLive()
        }
    }

    /** Busy ended: every turn still running without a summary is incomplete. */
    private SetBusy(busy: boolean): void
    {
        this.busy = busy
        if (!busy)
        {
            for (const t of this.Turns)
            {
                t.EndLive()
            }
        }
    }

    /** The oldest shown turn newer than every paged turn (a live one, so it came after the page's pending reset). */
    private LiveTurnAfter(page: TracePage): TraceTurnViewModel | null
    {
        const newestPaged = page.turns.length > 0 ? page.turns[0].turn : 0
        let found: TraceTurnViewModel | null = null
        for (const t of this.Turns)
        {
            if (t.Turn > newestPaged)
            {
                found = t
            }
        }
        return found
    }

    /** The row for a turn number, created in newest-first order when missing. A new newest row takes the expansion. */
    private TurnFor(turn: number): TraceTurnViewModel
    {
        const found = this.Find(turn)
        if (found !== null)
        {
            return found
        }
        let index = 0
        while (index < this.Turns.Count && this.Turns.Get(index)!.Turn > turn)
        {
            index++
        }
        const row = new TraceTurnViewModel(turn, index === 0)
        if (index === 0 && this.Turns.Count > 0)
        {
            this.Turns.Get(0)!.CollapseByDefault()
        }
        this.Turns.Insert(index, row)
        return row
    }

    private Find(turn: number): TraceTurnViewModel | null
    {
        for (const t of this.Turns)
        {
            if (t.Turn === turn)
            {
                return t
            }
        }
        return null
    }

    private Unsubscribe(): void
    {
        for (const s of this.subscriptions)
        {
            s.Dispose()
        }
        this.subscriptions = []
    }

    private SetPageError(text: string): void
    {
        this.UpdateWarning(() => { this.pageError = text })
    }

    /** Applies a change to either warning source, then notifies the combined line when it changed. */
    private UpdateWarning(mutate: () => void): void
    {
        const old = this.WarningText
        const oldHas = this.HasWarning
        mutate()
        const text = this.WarningText
        if (old !== text)
        {
            this.RaisePropertyChanged(TraceViewModel.PropWarningText, old, text)
        }
        if (oldHas !== this.HasWarning)
        {
            this.RaisePropertyChanged(TraceViewModel.PropHasWarning, oldHas, this.HasWarning)
        }
    }

    private SetMore(more: boolean): void
    {
        const old = this.more
        this.more = more
        if (old !== more)
        {
            this.RaisePropertyChanged(TraceViewModel.PropHasMore, old, more)
        }
        this.loadOlderCommand.RaiseCanExecuteChanged()
    }

    private SetLoadingOlder(loading: boolean): void
    {
        this.loadingOlder = loading
        this.loadOlderCommand.RaiseCanExecuteChanged()
    }

    private SetResetPending(pending: boolean): void
    {
        const old = this.resetPending
        if (old !== pending)
        {
            this.resetPending = pending
            this.RaisePropertyChanged(TraceViewModel.PropResetPending, old, pending)
        }
    }
}
