import { Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { TurnSignal, type IDisposable } from './JsonRpcConnection'
import type { HarnessInfo, ModelInfo, RuntimeInfo, SupervisorClient } from './SupervisorClient'

/** Runtime/model picker over the supervisor: lists choices, starts/stops a harness, tracks its lifecycle state. */
export class HarnessPickerViewModel extends Observable
{
    private static readonly StateStopped = 'stopped'
    private static readonly StateLoading = 'loading'
    private static readonly StateReady = 'ready'
    private static readonly StateFailed = 'failed'
    private static readonly LoadingFormat = 'Loading {0}/{1}…'
    private static readonly ReadyFormat = 'Ready: {0}/{1}'
    private static readonly FailedFormat = 'Failed: {0}/{1}'
    private static readonly StoppedText = 'Stopped'
    private static readonly StopFailedFormat = 'stop failed: {0}'
    private static readonly RefreshFailedFormat = 'refresh failed: {0}'
    private static readonly PropSelectedRuntime = 'SelectedRuntime'
    private static readonly PropSelectedModel = 'SelectedModel'
    private static readonly PropState = 'State'
    private static readonly PropStatusText = 'StatusText'
    private static readonly PropCanStart = 'CanStart'
    private static readonly PropCanStop = 'CanStop'
    private static readonly PropContextSizeText = 'ContextSizeText'
    private static readonly DefaultContextSize = 8192
    private static readonly DefaultContextText = '8192'
    private static readonly FormatToken = /\{(\d+)\}/g

    private _selectedRuntime: RuntimeInfo | null = null
    private _selectedModel: ModelInfo | null = null
    private _state = HarnessPickerViewModel.StateStopped
    private _statusText = ''
    private _canStart = false
    private _canStop = false
    /** Context-window size for the next launch, as typed; parsed to a positive int on Start. */
    private _contextSizeText = HarnessPickerViewModel.DefaultContextText
    private currentId: string | null = null
    /** True while a launch is in flight or a harness is live; events are ignored once the user stops. */
    private active = false
    /** Ids of harnesses the user stopped (or whose start was superseded); their late events are dropped. */
    private readonly ignoredIds = new Set<string>()
    /** Bumped on Stop so a late Start response for a cancelled launch is ignored. */
    private generation = 0
    private readonly _startCommand: RelayCommand
    private readonly _stopCommand: RelayCommand

    public readonly Runtimes = new ObservableCollection<RuntimeInfo>()
    public readonly Models = new ObservableCollection<ModelInfo>()
    public readonly OnReady = new TurnSignal<string>()
    public readonly OnCleared = new TurnSignal<void>()

    private client: SupervisorClient | null = null
    private harnessSubscription: IDisposable | null = null

    constructor(client?: SupervisorClient)
    {
        super()
        this._startCommand = new RelayCommand(() => void this.Start(), () => this._canStart)
        this._stopCommand = new RelayCommand(() => void this.Stop(), () => this._canStop)
        if (client !== undefined)
        {
            this.Bind(client)
        }
    }

    /** Attach the supervisor connection. Until bound the picker is inert (no listing, no start/stop). */
    public Bind(client: SupervisorClient): void
    {
        this.harnessSubscription?.Dispose()
        this.client = client
        this.harnessSubscription = client.OnHarnessChanged.Subscribe(info => this.ApplyInfo(info))
    }

    /** Detach from the supervisor and return to the pristine inert state (no lists, no selection, nothing startable). */
    public Unbind(): void
    {
        this.harnessSubscription?.Dispose()
        this.harnessSubscription = null
        this.client = null
        this.generation++
        this.Runtimes.Clear()
        this.Models.Clear()
        this.SelectedRuntime = null
        this.SelectedModel = null
        this.active = false
        this.currentId = null
        this.SetState(HarnessPickerViewModel.StateStopped, HarnessPickerViewModel.StoppedText)
    }

    public get SelectedRuntime(): RuntimeInfo | null
    {
        return this._selectedRuntime
    }

    public set SelectedRuntime(v: RuntimeInfo | null)
    {
        this.Change(HarnessPickerViewModel.PropSelectedRuntime, this._selectedRuntime, v, x => this._selectedRuntime = x)
        this.Reevaluate()
    }

    public get SelectedModel(): ModelInfo | null
    {
        return this._selectedModel
    }

    public set SelectedModel(v: ModelInfo | null)
    {
        this.Change(HarnessPickerViewModel.PropSelectedModel, this._selectedModel, v, x => this._selectedModel = x)
        this.Reevaluate()
    }

    public get ContextSizeText(): string
    {
        return this._contextSizeText
    }

    public set ContextSizeText(v: string)
    {
        this.Change(HarnessPickerViewModel.PropContextSizeText, this._contextSizeText, v, x => this._contextSizeText = x)
    }

    /** The context size sent on the next launch: the typed value parsed to a positive integer, or the safe default. */
    private ParsedContextSize(): number
    {
        const n = Number.parseInt(this._contextSizeText, 10)
        return Number.isFinite(n) && n > 0 ? n : HarnessPickerViewModel.DefaultContextSize
    }

    public get State(): string
    {
        return this._state
    }

    public get StatusText(): string
    {
        return this._statusText
    }

    public get CanStart(): boolean
    {
        return this._canStart
    }

    public get CanStop(): boolean
    {
        return this._canStop
    }

    public get StartCommand(): ICommand
    {
        return this._startCommand
    }

    public get StopCommand(): ICommand
    {
        return this._stopCommand
    }

    /** Repopulates runtimes/models and adopts the state of any already-live harness. */
    public Refresh(): void
    {
        void this.RefreshAsync()
    }

    private async RefreshAsync(): Promise<void>
    {
        const client = this.client
        if (client === null)
        {
            return
        }
        try
        {
            const [runtimes, models, harnesses] = await Promise.all([
                client.ListRuntimes(),
                client.ListModels(),
                client.ListHarnesses()])
            if (this.client !== client)
            {
                // Unbound or rebound while the listing was in flight: drop the stale reply.
                return
            }
            this.Runtimes.Clear()
            for (const r of runtimes)
            {
                this.Runtimes.Add(r)
            }
            this.Models.Clear()
            for (const m of models)
            {
                this.Models.Add(m)
            }
            if (harnesses.length > 0)
            {
                // Single-harness model: the supervisor runs at most one, so adopt the first.
                // Only an adopted (non-ignored) harness activates the picker.
                if (!this.ignoredIds.has(harnesses[0].id))
                {
                    this.active = true
                    this.ApplyInfo(harnesses[0])
                }
            }
        }
        catch (e)
        {
            this.SetStatus(HarnessPickerViewModel.Format(HarnessPickerViewModel.RefreshFailedFormat, HarnessPickerViewModel.Message(e)))
        }
    }

    private async Start(): Promise<void>
    {
        const client = this.client
        const runtime = this._selectedRuntime
        const model = this._selectedModel
        if (client === null || runtime === null || model === null)
        {
            return
        }
        // A deliberate launch commits to a fresh harness, so drop ids ignored from a prior
        // stop/supersede. Supervisor ids are deterministic (runtime:model), so restarting the
        // same model reuses the id Stop added to the ignore set; without this, the new harness's
        // Ready would be dropped and the picker would hang on "loading". Events from the stopped
        // period are already suppressed by the `active` flag (set false on Stop, true just below).
        this.ignoredIds.clear()
        const gen = ++this.generation
        if (this.currentId !== null)
        {
            // A late event from the harness being replaced must not disturb the new load.
            this.ignoredIds.add(this.currentId)
        }
        this.currentId = null
        this.active = true
        this.SetState(HarnessPickerViewModel.StateLoading,
            HarnessPickerViewModel.Format(HarnessPickerViewModel.LoadingFormat, runtime.name, model.name))
        try
        {
            const info = await client.Start(runtime.name, model.name, this.ParsedContextSize())
            if (gen === this.generation)
            {
                this.ApplyInfo(info)
            }
            else
            {
                // Superseded by Stop/Start while loading: shut the orphan down and ignore its events.
                this.ignoredIds.add(info.id)
                client.Stop(info.id).catch(() => undefined)
            }
        }
        catch (e)
        {
            if (gen === this.generation)
            {
                const previous = this._state
                this.currentId = null
                this.SetState(HarnessPickerViewModel.StateFailed, HarnessPickerViewModel.Message(e))
                if (previous !== HarnessPickerViewModel.StateFailed)
                {
                    this.OnCleared.Raise(undefined)
                }
            }
        }
    }

    private async Stop(): Promise<void>
    {
        const id = this.currentId
        this.generation++
        this.active = false
        this.currentId = null
        if (id !== null)
        {
            this.ignoredIds.add(id)
        }
        this.SetState(HarnessPickerViewModel.StateStopped, HarnessPickerViewModel.StoppedText)
        this.OnCleared.Raise(undefined)
        if (id === null || this.client === null)
        {
            return
        }
        try
        {
            await this.client.Stop(id)
        }
        catch (e)
        {
            this.SetStatus(HarnessPickerViewModel.Format(HarnessPickerViewModel.StopFailedFormat, HarnessPickerViewModel.Message(e)))
        }
    }

    /** Accepted limitation: events carry only a harness id, so a sub-second Stop then Start before the first
     *  launch's reply can momentarily adopt the first launch's harness_changed as the second launch's;
     *  the stale reply self-heals this when it lands (it is ignored and its harness stopped). */
    /** State is keyed off the harness info, not message order; transitions fire the signals once. */
    private ApplyInfo(info: HarnessInfo): void
    {
        if (!this.active || this.ignoredIds.has(info.id) || (this.currentId !== null && this.currentId !== info.id))
        {
            return
        }
        const previous = this._state
        this.currentId = info.state === HarnessPickerViewModel.StateStopped ? null : info.id
        this.SetState(info.state, HarnessPickerViewModel.TextFor(info))
        if (info.state === previous)
        {
            return
        }
        if (info.state === HarnessPickerViewModel.StateReady && info.url)
        {
            this.OnReady.Raise(info.url)
        }
        else if (info.state === HarnessPickerViewModel.StateFailed || info.state === HarnessPickerViewModel.StateStopped)
        {
            this.OnCleared.Raise(undefined)
        }
    }

    private static TextFor(info: HarnessInfo): string
    {
        switch (info.state)
        {
            case HarnessPickerViewModel.StateLoading:
                return HarnessPickerViewModel.Format(HarnessPickerViewModel.LoadingFormat, info.runtime, info.model)
            case HarnessPickerViewModel.StateReady:
                return HarnessPickerViewModel.Format(HarnessPickerViewModel.ReadyFormat, info.runtime, info.model)
            case HarnessPickerViewModel.StateFailed:
                return HarnessPickerViewModel.Format(HarnessPickerViewModel.FailedFormat, info.runtime, info.model)
            default:
                return HarnessPickerViewModel.StoppedText
        }
    }

    private SetState(state: string, text: string): void
    {
        this.Change(HarnessPickerViewModel.PropState, this._state, state, x => this._state = x)
        this.SetStatus(text)
        this.Reevaluate()
    }

    private SetStatus(text: string): void
    {
        this.Change(HarnessPickerViewModel.PropStatusText, this._statusText, text, x => this._statusText = x)
    }

    private Reevaluate(): void
    {
        const loading = this._state === HarnessPickerViewModel.StateLoading
        const stoppable = loading || this._state === HarnessPickerViewModel.StateReady || this._state === HarnessPickerViewModel.StateFailed
        this.Change(HarnessPickerViewModel.PropCanStart, this._canStart, this._selectedRuntime !== null && this._selectedModel !== null && !loading, x => this._canStart = x)
        this.Change(HarnessPickerViewModel.PropCanStop, this._canStop, stoppable, x => this._canStop = x)
        this._startCommand.RaiseCanExecuteChanged()
        this._stopCommand.RaiseCanExecuteChanged()
    }

    /** Assigns before raising INPC so a TwoWay binding re-reading the source sees the new value. */
    private Change<T>(name: string, old: T, value: T, assign: (v: T) => void): void
    {
        if (old === value)
        {
            return
        }
        assign(value)
        this.RaisePropertyChanged(name, old, value)
    }

    private static Message(e: unknown): string
    {
        return e instanceof Error ? e.message : String(e)
    }

    private static Format(template: string, ...values: unknown[]): string
    {
        return template.replace(HarnessPickerViewModel.FormatToken, (_m, i: string) => String(values[Number(i)]))
    }
}
