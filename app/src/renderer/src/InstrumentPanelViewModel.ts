import { Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { RuntimeClient, TypedSignal, type IDisposable, type ModelProps } from './RuntimeClient'
import type { SamplingConfig } from './agents'
import { LogprobVM } from './LogprobVM'

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
    /** Raised once per Bind with the harness's model_props, so siblings need no second fetch. */
    public readonly OnModelProps = new TypedSignal<ModelProps>()

    private client: RuntimeClient | null = null
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
        // Fresh harness = fresh panel: drop the previous harness's display state.
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
                this.SetWarning(InstrumentPanelViewModel.Format(InstrumentPanelViewModel.WarningFormat, p.needed, p.total))
            }))
            // Shows the model + window size before the first turn. Rejects (ignored) if there is no connection.
            client.ModelProps().then(
                props =>
                {
                    if (this.client !== client)
                    {
                        return
                    }
                    this.SetModelName(props.model_name)
                    this.SetContext(this._contextUsed, props.n_ctx)
                    this.OnModelProps.Raise(props)
                },
                () => undefined)
        }
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

    /** The current knobs as the wire DTO; applied per turn by ChatViewModel.Send. */
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

    /** Called when a new turn is attempted; a failed set_sampling then re-raises its own warning. */
    public ClearWarning(): void
    {
        this.SetWarning('')
    }

    /** Surfaces a failed set_sampling: the turn would otherwise run with stale knobs unnoticed. */
    public ReportSamplingError(message: string): void
    {
        this.SetWarning(InstrumentPanelViewModel.Format(InstrumentPanelViewModel.SamplingFailedFormat, message))
    }

    /** Fill width in pixels for the bar (clamped to the track). */
    public get ContextBarWidth(): number
    {
        return Math.min(1, this.ContextFraction) * InstrumentPanelViewModel.BarPixels
    }

    public get ContextLabel(): string
    {
        const pct = Math.round(this.ContextFraction * InstrumentPanelViewModel.PercentScale)
        return InstrumentPanelViewModel.Format(InstrumentPanelViewModel.ContextFormat, this._contextUsed, this._contextTotal, pct)
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
                this.SetTokenizeResult(InstrumentPanelViewModel.Format(InstrumentPanelViewModel.TokenizeFormat, r.count, ids))
            },
            (e: Error) =>
            {
                this.SetTokenizeResult(InstrumentPanelViewModel.Format(InstrumentPanelViewModel.TokenizeFailedFormat, e.message))
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

    private static Format(template: string, ...values: unknown[]): string
    {
        return template.replace(/\{(\d+)\}/g, (_m, i: string) => String(values[Number(i)]))
    }
}
