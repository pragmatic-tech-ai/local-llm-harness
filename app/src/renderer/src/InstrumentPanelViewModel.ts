import { Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { RuntimeClient, type IDisposable } from './RuntimeClient'
import type { SamplingConfig } from './agents'
import { LogprobVM } from './LogprobVM'
import { TextFormat } from './TextFormat'
import type { TraceViewModel } from './TraceViewModel'

/** Low-level state of the model: sampling knobs, raw prompt, context fill, logprobs, tokenization. */
export class InstrumentPanelViewModel extends Observable
{
    private static readonly WarningFormat = 'context window full: needs {0} tokens, only {1} available — shorten the conversation'
    private static readonly ContextFormat = '{0} / {1} ({2}%)'
    private static readonly TokenizeFormat = '{0} tokens: [{1}]'
    private static readonly SamplingFailedFormat = 'sampling not applied: {0}'
    private static readonly TokenizeFailedFormat = 'tokenize failed: {0}'
    private static readonly TokenSeparator = ', '
    private static readonly PathSeparators = /[\\/]/
    private static readonly PercentScale = 100
    /** Pixel width of the context-fill track; the fill Border is bound to a fraction of it. */
    private static readonly BarPixels = 300
    private static readonly PropTrace = 'Trace'

    private _temperature = 0.8
    private _topP = 0.95
    private _topK = 40
    private _repeatPenalty = 1.1
    private _seed = -1
    // Diverges from the backend default (-1 = unlimited): a bounded default keeps a slow model from streaming forever.
    private _maxTokens = 256
    private _enableThinking = false
    private _prompt = ''
    private _contextUsed = 0
    private _contextTotal = 0
    private _modelName = ''
    private _warning = ''
    private _tokenizeInput = ''
    private _tokenizeResult = ''
    private readonly _tokenizeCommand: RelayCommand

    public readonly Logprobs = new ObservableCollection<LogprobVM>()

    private client: RuntimeClient | null = null
    private trace: TraceViewModel | null = null
    /** The sampling the session runs with (JSON), as last applied; null = unknown, so the next send applies the knobs. */
    private appliedSampling: string | null = null
    /** The thinking flag the session runs with; null = unknown. */
    private appliedThinking: boolean | null = null
    private subscriptions: IDisposable[] = []

    constructor(client?: RuntimeClient)
    {
        super()
        this._tokenizeCommand = new RelayCommand(() => this.Tokenize())
        if (client)
        {
            this.Bind(client)
        }
    }

    /** Rebinds to a (new) harness client, or unbinds with null. Stale subscriptions are disposed first. */
    public Bind(client: RuntimeClient | null): void
    {
        for (const s of this.subscriptions)
        {
            s.Dispose()
        }
        this.subscriptions = []
        this.client = client
        // A new binding runs with values unknown until ApplySession records the session's own.
        this.appliedSampling = null
        this.appliedThinking = null
        // Fresh session = fresh panel: drop the previous session's display state.
        this.Logprobs.Clear()
        this.Change('Prompt', this._prompt, '', x => this._prompt = x)
        this.SetWarning('')
        this.SetModelName('')
        this.SetContext(0, 0)
        if (client)
        {
            this.subscriptions.push(client.OnTurnStarted.Subscribe(p =>
            {
                this.Logprobs.Clear()
                this.Change('Prompt', this._prompt, p.prompt, x => this._prompt = x)
                this.SetContext(p.prompt_tokens, p.n_ctx)
            }))
            this.subscriptions.push(client.OnTurnChunk.Subscribe(p =>
            {
                for (const info of p.tokens ?? [])
                {
                    this.Logprobs.Add(new LogprobVM(info))
                }
            }))
            this.subscriptions.push(client.OnTurnRejected.Subscribe(p =>
            {
                this.SetWarning(TextFormat.Format(InstrumentPanelViewModel.WarningFormat, p.needed, p.total))
            }))
            this.RefreshModelProps()
        }
    }

    /** Shows the model + window size before the first turn. Rejects (ignored) without a running model. */
    public RefreshModelProps(): void
    {
        const client = this.client
        if (client === null)
        {
            return
        }
        client.ModelProps().then(
            props =>
            {
                if (this.client !== client)
                {
                    return
                }
                this.SetModelName(props.model_name)
                this.SetContext(this._contextUsed, props.n_ctx)
            },
            () => undefined)
    }

    /** Seeds the knobs from an attached session's effective values and records them as what the session runs with. */
    public ApplySession(sampling: SamplingConfig, enableThinking: boolean): void
    {
        this.Temperature = sampling.temperature
        this.TopP = sampling.top_p
        this.TopK = sampling.top_k
        this.RepeatPenalty = sampling.repeat_penalty
        this.Seed = sampling.seed
        this.MaxTokens = sampling.max_tokens
        this.EnableThinking = enableThinking
        this.appliedSampling = JSON.stringify(this.Config)
        this.appliedThinking = enableThinking
    }

    /** The knobs when they differ from what the session runs with (then recorded as applied), else null. */
    public TakeSamplingChange(): SamplingConfig | null
    {
        const config = this.Config
        const key = JSON.stringify(config)
        if (key === this.appliedSampling)
        {
            return null
        }
        this.appliedSampling = key
        return config
    }

    /** The thinking toggle when it differs from what the session runs with (then recorded as applied), else null. */
    public TakeThinkingChange(): boolean | null
    {
        if (this._enableThinking === this.appliedThinking)
        {
            return null
        }
        this.appliedThinking = this._enableThinking
        return this._enableThinking
    }

    public get Temperature(): number
    {
        return this._temperature
    }

    public set Temperature(v: number)
    {
        this.Change('Temperature', this._temperature, v, x => this._temperature = x)
    }

    public get TopP(): number
    {
        return this._topP
    }

    public set TopP(v: number)
    {
        this.Change('TopP', this._topP, v, x => this._topP = x)
    }

    public get TopK(): number
    {
        return this._topK
    }

    public set TopK(v: number)
    {
        this.Change('TopK', this._topK, v, x => this._topK = x)
    }

    public get RepeatPenalty(): number
    {
        return this._repeatPenalty
    }

    public set RepeatPenalty(v: number)
    {
        this.Change('RepeatPenalty', this._repeatPenalty, v, x => this._repeatPenalty = x)
    }

    public get Seed(): number
    {
        return this._seed
    }

    public set Seed(v: number)
    {
        this.Change('Seed', this._seed, v, x => this._seed = x)
    }

    public get MaxTokens(): number
    {
        return this._maxTokens
    }

    public set MaxTokens(v: number)
    {
        this.Change('MaxTokens', this._maxTokens, v, x => this._maxTokens = x)
    }

    public get EnableThinking(): boolean
    {
        return this._enableThinking
    }

    public set EnableThinking(v: boolean)
    {
        this.Change('EnableThinking', this._enableThinking, v, x => this._enableThinking = x)
    }

    /** The current knobs as the wire DTO; ChatViewModel.Send applies them when they changed. */
    public get Config(): SamplingConfig
    {
        return {
            temperature: this._temperature,
            top_p: this._topP,
            top_k: Math.round(this._topK),
            repeat_penalty: this._repeatPenalty,
            seed: Math.round(this._seed),
            max_tokens: Math.round(this._maxTokens),
        }
    }

    public get Prompt(): string
    {
        return this._prompt
    }

    public get ContextUsed(): number
    {
        return this._contextUsed
    }

    public get ContextTotal(): number
    {
        return this._contextTotal
    }

    public get ContextFraction(): number
    {
        return this._contextTotal > 0 ? this._contextUsed / this._contextTotal : 0
    }

    /** Width of the context bar track; the single source for the .mu track and the fill math. */
    public get ContextBarTrackWidth(): number
    {
        return InstrumentPanelViewModel.BarPixels
    }

    /** The agent trace shown below the context bar (owned and driven by the session view), or null. */
    public get Trace(): TraceViewModel | null
    {
        return this.trace
    }

    /** Places the session view's trace in the panel. */
    public HostTrace(trace: TraceViewModel | null): void
    {
        this.Change(InstrumentPanelViewModel.PropTrace, this.trace, trace, x => this.trace = x)
    }

    /** Called when a new turn is attempted; a failed set_sampling then re-raises its own warning. */
    public ClearWarning(): void
    {
        this.SetWarning('')
    }

    /** Surfaces a failed set_sampling: the turn would otherwise run with stale knobs unnoticed. */
    public ReportSamplingError(message: string): void
    {
        // What the session runs with is unknown again: the next send re-applies both.
        this.appliedSampling = null
        this.appliedThinking = null
        this.SetWarning(TextFormat.Format(InstrumentPanelViewModel.SamplingFailedFormat, message))
    }

    /** Fill width in pixels for the bar (clamped to the track). */
    public get ContextBarWidth(): number
    {
        return Math.min(1, this.ContextFraction) * InstrumentPanelViewModel.BarPixels
    }

    public get ContextLabel(): string
    {
        const pct = Math.round(this.ContextFraction * InstrumentPanelViewModel.PercentScale)
        return TextFormat.Format(InstrumentPanelViewModel.ContextFormat, this._contextUsed, this._contextTotal, pct)
    }

    /** Basename only: the backend reports a full path. */
    public get ModelName(): string
    {
        return this._modelName
    }

    public get Warning(): string
    {
        return this._warning
    }

    public get HasWarning(): boolean
    {
        return this._warning.length > 0
    }

    public get TokenizeInput(): string
    {
        return this._tokenizeInput
    }

    public set TokenizeInput(v: string)
    {
        this.Change('TokenizeInput', this._tokenizeInput, v, x => this._tokenizeInput = x)
    }

    public get TokenizeResult(): string
    {
        return this._tokenizeResult
    }

    public get TokenizeCommand(): ICommand
    {
        return this._tokenizeCommand
    }

    private Tokenize(): void
    {
        if (this.client === null)
        {
            return
        }
        this.client.Tokenize(this._tokenizeInput).then(
            r =>
            {
                const ids = r.tokens.join(InstrumentPanelViewModel.TokenSeparator)
                this.SetTokenizeResult(TextFormat.Format(InstrumentPanelViewModel.TokenizeFormat, r.count, ids))
            },
            (e: unknown) =>
            {
                this.SetTokenizeResult(TextFormat.Format(InstrumentPanelViewModel.TokenizeFailedFormat, TextFormat.Message(e)))
            })
    }

    private SetTokenizeResult(v: string): void
    {
        this.Change('TokenizeResult', this._tokenizeResult, v, x => this._tokenizeResult = x)
    }

    private SetModelName(full: string | undefined): void
    {
        const parts = (full ?? '').split(InstrumentPanelViewModel.PathSeparators)
        this.Change('ModelName', this._modelName, parts[parts.length - 1], x => this._modelName = x)
    }

    private SetWarning(v: string): void
    {
        const oldHas = this.HasWarning
        this.Change('Warning', this._warning, v, x => this._warning = x)
        if (oldHas !== this.HasWarning)
        {
            this.RaisePropertyChanged('HasWarning', oldHas, this.HasWarning)
        }
    }

    private SetContext(used: number, total: number): void
    {
        this.Change('ContextUsed', this._contextUsed, used, x => this._contextUsed = x)
        this.Change('ContextTotal', this._contextTotal, total, x => this._contextTotal = x)
        this.RaisePropertyChanged('ContextFraction', undefined, this.ContextFraction)
        this.RaisePropertyChanged('ContextBarWidth', undefined, this.ContextBarWidth)
        this.RaisePropertyChanged('ContextLabel', undefined, this.ContextLabel)
    }

    /** Assigns the new value (via `assign`) BEFORE raising INPC. A TwoWay binding re-reads the
     *  source synchronously while PropertyChanged fires; if the field were still the old value
     *  then (as it is when the setter assigns the return of a notify-first helper), the binding
     *  writes that stale value back to the control — the "knob snaps back / won't move" bug. */
    private Change<T>(name: string, old: T, value: T, assign: (v: T) => void): void
    {
        if (old === value)
        {
            return
        }
        assign(value)
        this.RaisePropertyChanged(name, old, value)
    }
}
