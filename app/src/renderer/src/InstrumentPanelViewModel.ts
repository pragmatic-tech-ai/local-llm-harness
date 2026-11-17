import { Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { HarnessClient, type SamplingConfig } from './HarnessClient'
import { LogprobVM } from './LogprobVM'

/** Low-level state of the model: sampling knobs, raw prompt, context fill, logprobs, tokenization. */
export class InstrumentPanelViewModel extends Observable
{
    private static readonly WarningFormat = 'context window full: needs {0} tokens, only {1} available — shorten the conversation'
    private static readonly ContextFormat = '{0} / {1} ({2}%)'
    private static readonly TokenizeFormat = '{0} tokens: [{1}]'
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

    constructor(private readonly client: HarnessClient)
    {
        super()
        this._tokenizeCommand = new RelayCommand(() => this.Tokenize())
        client.OnTurnStarted.Subscribe(p =>
        {
            this.Logprobs.Clear()
            this.SetWarning('')
            this._prompt = this.Change('Prompt', this._prompt, p.prompt)
            this.SetContext(p.prompt_tokens, p.n_ctx)
        })
        client.OnTurnChunk.Subscribe(p =>
        {
            for (const info of p.tokens)
            {
                this.Logprobs.Add(new LogprobVM(info))
            }
        })
        client.OnTurnRejected.Subscribe(p =>
        {
            this.SetWarning(InstrumentPanelViewModel.Format(InstrumentPanelViewModel.WarningFormat, p.needed, p.total))
        })
        // Shows the model + window size before the first turn. Rejects (ignored) if there is no connection.
        client.ModelProps().then(
            props =>
            {
                this.SetModelName(props.model_name)
                this.SetContext(this._contextUsed, props.n_ctx)
            },
            () => undefined)
    }

    public get Temperature(): number
    {
        return this._temperature
    }

    public set Temperature(v: number)
    {
        this._temperature = this.Change('Temperature', this._temperature, v)
    }

    public get TopP(): number
    {
        return this._topP
    }

    public set TopP(v: number)
    {
        this._topP = this.Change('TopP', this._topP, v)
    }

    public get TopK(): number
    {
        return this._topK
    }

    public set TopK(v: number)
    {
        this._topK = this.Change('TopK', this._topK, v)
    }

    public get RepeatPenalty(): number
    {
        return this._repeatPenalty
    }

    public set RepeatPenalty(v: number)
    {
        this._repeatPenalty = this.Change('RepeatPenalty', this._repeatPenalty, v)
    }

    public get Seed(): number
    {
        return this._seed
    }

    public set Seed(v: number)
    {
        this._seed = this.Change('Seed', this._seed, v)
    }

    public get MaxTokens(): number
    {
        return this._maxTokens
    }

    public set MaxTokens(v: number)
    {
        this._maxTokens = this.Change('MaxTokens', this._maxTokens, v)
    }

    public get EnableThinking(): boolean
    {
        return this._enableThinking
    }

    public set EnableThinking(v: boolean)
    {
        this._enableThinking = this.Change('EnableThinking', this._enableThinking, v)
    }

    /** The current knobs as the wire DTO; applied per turn by ChatViewModel.Send. */
    public get Config(): SamplingConfig
    {
        return {
            temperature: this._temperature,
            top_p: this._topP,
            top_k: this._topK,
            repeat_penalty: this._repeatPenalty,
            seed: this._seed,
            max_tokens: this._maxTokens,
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
        this._tokenizeInput = this.Change('TokenizeInput', this._tokenizeInput, v)
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
        this._tokenizeResult = this.Change('TokenizeResult', this._tokenizeResult, v)
    }

    private SetModelName(full: string): void
    {
        const parts = full.split(InstrumentPanelViewModel.PathSeparators)
        this._modelName = this.Change('ModelName', this._modelName, parts[parts.length - 1])
    }

    private SetWarning(v: string): void
    {
        const oldHas = this.HasWarning
        this._warning = this.Change('Warning', this._warning, v)
        if (oldHas !== this.HasWarning)
        {
            this.RaisePropertyChanged('HasWarning', oldHas, this.HasWarning)
        }
    }

    private SetContext(used: number, total: number): void
    {
        this._contextUsed = this.Change('ContextUsed', this._contextUsed, used)
        this._contextTotal = this.Change('ContextTotal', this._contextTotal, total)
        this.RaisePropertyChanged('ContextFraction', undefined, this.ContextFraction)
        this.RaisePropertyChanged('ContextBarWidth', undefined, this.ContextBarWidth)
        this.RaisePropertyChanged('ContextLabel', undefined, this.ContextLabel)
    }

    /** Raises INPC for a changed value and returns it, so setters stay one line. */
    private Change<T>(name: string, old: T, value: T): T
    {
        if (old !== value)
        {
            this.RaisePropertyChanged(name, old, value)
        }
        return value
    }

    private static Format(template: string, ...values: unknown[]): string
    {
        return template.replace(/\{(\d+)\}/g, (_m, i: string) => String(values[Number(i)]))
    }
}
