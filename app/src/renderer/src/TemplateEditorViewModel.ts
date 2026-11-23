import { Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { TypedSignal } from './JsonRpcConnection'
import type { RuntimeClient } from './RuntimeClient'
import type { AgentTemplate, CatalogInfo, SamplingConfig, TemplateLeafNode, ToolSpec } from './agents'
import type { TemplateTreeNodeViewModel } from './TemplateTreeNodeViewModel'
import { ChoiceItem } from './ChoiceItem'
import { ToolChoice, type IToolChoiceOwner } from './ToolChoice'
import { TextFormat } from './TextFormat'

/**
 * The host-area template editor. One stable instance: Open shows a template from the tree, OpenNew an unsaved
 * `Untitled` draft, Clear the empty state. The number fields are text so a typo can be shown rather than sent: one
 * that does not parse gets a field error and disables Save. Runtime / model / host come from `list_catalog`, the
 * models filtered by the runtime; a value the catalog lacks stays as a marked choice. A template file that fails to
 * load opens with its error in ErrorText and the fields seeded from the defaults (plus what the tree node carries),
 * so Save repairs it. Validation errors from `save_template` show in ErrorText.
 */
export class TemplateEditorViewModel extends Observable implements IToolChoiceOwner
{
    private static readonly DefaultName = 'Untitled'
    private static readonly DefaultStepBudget = 8
    private static readonly DefaultContextSize = 8192
    private static readonly AnyHostLabel = '(any host)'
    private static readonly RootLabel = '(root)'
    private static readonly NewHeaderFormat = 'New template in {0}'
    private static readonly HeaderFormat = '{0} in {1}'
    private static readonly EmptyText = 'Select a template to edit, or create a new one.'
    private static readonly NotInRegistry = 'not in the tool registry'
    private static readonly NameRequired = 'Name is required.'
    private static readonly StepBudgetInvalid = 'Step budget must be a whole number of at least 1.'
    private static readonly ContextSizeInvalid = 'Context size must be a whole number of tokens.'
    private static readonly TemperatureInvalid = 'Temperature must be a number of 0 or more, or blank.'
    private static readonly TopPInvalid = 'Top P must be a number from 0 to 1, or blank.'
    private static readonly CatalogFailedFormat = 'catalog unavailable: {0}'
    private static readonly Whole = /^\d+$/
    private static readonly Decimal = /^[+-]?(\d+\.?\d*|\.\d+)$/
    private static readonly PropName = 'Name'
    private static readonly PropSystemPrompt = 'SystemPrompt'
    private static readonly PropStepBudgetText = 'StepBudgetText'
    private static readonly PropRuntime = 'Runtime'
    private static readonly PropModel = 'Model'
    private static readonly PropContextSizeText = 'ContextSizeText'
    private static readonly PropEnableThinking = 'EnableThinking'
    private static readonly PropTemperatureText = 'TemperatureText'
    private static readonly PropTopPText = 'TopPText'
    private static readonly PropHost = 'Host'
    private static readonly PropPath = 'Path'
    private static readonly FieldProps = ['Name', 'SystemPrompt', 'StepBudgetText', 'Runtime', 'Model', 'ContextSizeText',
        'EnableThinking', 'TemperatureText', 'TopPText', 'Host', 'Path', 'TemplateId']
    private static readonly DerivedProps = ['HasTemplate', 'IsEmpty', 'IsNew', 'IsDirty', 'CanSave', 'HeaderText', 'ErrorText',
        'HasErrorText', 'NameError', 'HasNameError', 'StepBudgetError', 'HasStepBudgetError', 'ContextSizeError',
        'HasContextSizeError', 'TemperatureError', 'HasTemperatureError', 'TopPError', 'HasTopPError', 'RuntimeChoice',
        'ModelChoice', 'HostChoice']

    public readonly RuntimeChoices = new ObservableCollection<ChoiceItem>()
    public readonly ModelChoices = new ObservableCollection<ChoiceItem>()
    public readonly HostChoices = new ObservableCollection<ChoiceItem>()
    public readonly Tools = new ObservableCollection<ToolChoice>()
    /** A save succeeded; carries the template id (new for a draft). */
    public readonly OnSaved = new TypedSignal<string>()

    private readonly client: RuntimeClient
    private readonly saveCommand: RelayCommand
    private readonly revertCommand: RelayCommand
    private catalog: CatalogInfo | null = null
    private registry: ToolSpec[] | null = null
    private open = false
    private templateId: string | null = null
    private path = ''
    private isNew = false
    /** Opened from the defaults because the file did not load: Save (which repairs it) is allowed while clean. */
    private broken = false
    private saving = false
    /** What the form was last filled from (Revert goes back to it). */
    private loaded: AgentTemplate | null = null
    /** The form's fingerprint when it was filled or saved: anything else is an unsaved edit. */
    private baseline = ''
    /** Orders opens and reloads: a late reply for an earlier template is dropped. */
    private openToken = 0
    private errorText = ''
    private name = ''
    private systemPrompt = ''
    private stepBudgetText = ''
    private runtime = ''
    private model = ''
    private contextSizeText = ''
    private enableThinking = false
    private temperatureText = ''
    private topPText = ''
    private host = ''
    private sampling: Partial<SamplingConfig> | undefined = undefined
    private enabled = new Set<string>()

    constructor(client: RuntimeClient)
    {
        super()
        this.client = client
        this.saveCommand = new RelayCommand(() => void this.Save(), () => this.CanSave)
        this.revertCommand = new RelayCommand(() => this.Revert(), () => this.CanRevert)
        this.Fill(this.Defaults('', '', ''))
    }

    public get SaveCommand(): ICommand
    {
        return this.saveCommand
    }

    public get RevertCommand(): ICommand
    {
        return this.revertCommand
    }

    /** The template shown (null for a draft or when empty). */
    public get TemplateId(): string | null
    {
        return this.templateId
    }

    /** The group the template lives in ("" is the root). */
    public get Path(): string
    {
        return this.path
    }

    public get HasTemplate(): boolean
    {
        return this.open
    }

    public get IsEmpty(): boolean
    {
        return !this.open
    }

    public get EmptyText(): string
    {
        return TemplateEditorViewModel.EmptyText
    }

    /** An unsaved draft from New template. */
    public get IsNew(): boolean
    {
        return this.open && this.isNew
    }

    /** Unsaved edits (a draft always counts). */
    public get IsDirty(): boolean
    {
        return this.open && (this.isNew || this.Fingerprint() !== this.baseline)
    }

    public get CanSave(): boolean
    {
        return this.open && !this.saving && (this.IsDirty || this.broken) && !this.HasFieldErrors
    }

    public get CanRevert(): boolean
    {
        return this.open && !this.saving && this.Fingerprint() !== this.baseline
    }

    public get HeaderText(): string
    {
        if (!this.open)
        {
            return ''
        }
        const group = this.path.length > 0 ? this.path : TemplateEditorViewModel.RootLabel
        return this.isNew
            ? TextFormat.Format(TemplateEditorViewModel.NewHeaderFormat, group)
            : TextFormat.Format(TemplateEditorViewModel.HeaderFormat, this.templateId, group)
    }

    /** The load error of a broken template, or the last save / catalog error. */
    public get ErrorText(): string
    {
        return this.errorText
    }

    public get HasErrorText(): boolean
    {
        return this.errorText.length > 0
    }

    public get Name(): string
    {
        return this.name
    }

    public set Name(value: string)
    {
        this.SetField(TemplateEditorViewModel.PropName, this.name, value, () => { this.name = value })
    }

    public get NameError(): string
    {
        return this.open && this.name.trim().length === 0 ? TemplateEditorViewModel.NameRequired : ''
    }

    public get HasNameError(): boolean
    {
        return this.NameError.length > 0
    }

    public get SystemPrompt(): string
    {
        return this.systemPrompt
    }

    public set SystemPrompt(value: string)
    {
        this.SetField(TemplateEditorViewModel.PropSystemPrompt, this.systemPrompt, value, () => { this.systemPrompt = value })
    }

    public get StepBudgetText(): string
    {
        return this.stepBudgetText
    }

    public set StepBudgetText(value: string)
    {
        this.SetField(TemplateEditorViewModel.PropStepBudgetText, this.stepBudgetText, value, () => { this.stepBudgetText = value })
    }

    public get StepBudgetError(): string
    {
        return this.open && TemplateEditorViewModel.ParseCount(this.stepBudgetText) === null ? TemplateEditorViewModel.StepBudgetInvalid : ''
    }

    public get HasStepBudgetError(): boolean
    {
        return this.StepBudgetError.length > 0
    }

    public get Runtime(): string
    {
        return this.runtime
    }

    /** Changing the runtime re-filters the models; the current model stays (marked when this runtime lacks it). */
    public set Runtime(value: string)
    {
        this.SetField(TemplateEditorViewModel.PropRuntime, this.runtime, value, () =>
        {
            this.runtime = value
            this.RebuildChoices()
        })
    }

    public get RuntimeChoice(): ChoiceItem | null
    {
        return TemplateEditorViewModel.FindChoice(this.RuntimeChoices, this.runtime)
    }

    public set RuntimeChoice(choice: ChoiceItem | null)
    {
        if (choice instanceof ChoiceItem)
        {
            this.Runtime = choice.Value
        }
    }

    public get Model(): string
    {
        return this.model
    }

    public set Model(value: string)
    {
        this.SetField(TemplateEditorViewModel.PropModel, this.model, value, () =>
        {
            this.model = value
            this.RebuildChoices()
        })
    }

    public get ModelChoice(): ChoiceItem | null
    {
        return TemplateEditorViewModel.FindChoice(this.ModelChoices, this.model)
    }

    public set ModelChoice(choice: ChoiceItem | null)
    {
        if (choice instanceof ChoiceItem)
        {
            this.Model = choice.Value
        }
    }

    public get ContextSizeText(): string
    {
        return this.contextSizeText
    }

    public set ContextSizeText(value: string)
    {
        this.SetField(TemplateEditorViewModel.PropContextSizeText, this.contextSizeText, value, () => { this.contextSizeText = value })
    }

    public get ContextSizeError(): string
    {
        return this.open && TemplateEditorViewModel.ParseCount(this.contextSizeText) === null ? TemplateEditorViewModel.ContextSizeInvalid : ''
    }

    public get HasContextSizeError(): boolean
    {
        return this.ContextSizeError.length > 0
    }

    public get EnableThinking(): boolean
    {
        return this.enableThinking
    }

    public set EnableThinking(value: boolean)
    {
        this.SetField(TemplateEditorViewModel.PropEnableThinking, this.enableThinking, value, () => { this.enableThinking = value })
    }

    /** Blank means no temperature default (the worker's applies). */
    public get TemperatureText(): string
    {
        return this.temperatureText
    }

    public set TemperatureText(value: string)
    {
        this.SetField(TemplateEditorViewModel.PropTemperatureText, this.temperatureText, value, () => { this.temperatureText = value })
    }

    public get TemperatureError(): string
    {
        const value = TemplateEditorViewModel.ParseOptional(this.temperatureText)
        return this.open && (value === null || (value !== undefined && value < 0)) ? TemplateEditorViewModel.TemperatureInvalid : ''
    }

    public get HasTemperatureError(): boolean
    {
        return this.TemperatureError.length > 0
    }

    /** Blank means no top_p default (the worker's applies). */
    public get TopPText(): string
    {
        return this.topPText
    }

    public set TopPText(value: string)
    {
        this.SetField(TemplateEditorViewModel.PropTopPText, this.topPText, value, () => { this.topPText = value })
    }

    public get TopPError(): string
    {
        const value = TemplateEditorViewModel.ParseOptional(this.topPText)
        return this.open && (value === null || (value !== undefined && (value < 0 || value > 1))) ? TemplateEditorViewModel.TopPInvalid : ''
    }

    public get HasTopPError(): boolean
    {
        return this.TopPError.length > 0
    }

    /** The preferred host ("" = any). */
    public get Host(): string
    {
        return this.host
    }

    public set Host(value: string)
    {
        this.SetField(TemplateEditorViewModel.PropHost, this.host, value, () =>
        {
            this.host = value
            this.RebuildChoices()
        })
    }

    public get HostChoice(): ChoiceItem | null
    {
        return TemplateEditorViewModel.FindChoice(this.HostChoices, this.host)
    }

    public set HostChoice(choice: ChoiceItem | null)
    {
        if (choice instanceof ChoiceItem)
        {
            this.Host = choice.Value
        }
    }

    /** Reads the catalog (runtime / model / host choices) and the whole tool registry (the tool checkboxes). */
    public async LoadCatalog(): Promise<void>
    {
        try
        {
            const [catalog, registry] = await Promise.all([this.client.ListCatalog(), this.client.ListAllTools()])
            const pristine = this.Fingerprint() === this.baseline
            this.catalog = catalog
            this.registry = registry
            const loaded = this.loaded
            if (this.open && pristine && loaded !== null && (this.isNew || this.broken) && (loaded.runtime === '' || loaded.model === ''))
            {
                // A draft or broken template seeded before the catalog arrived: complete its runtime and model.
                this.Fill(this.Defaults(loaded.name, loaded.runtime, loaded.model))
                return
            }
            this.RebuildChoices()
            this.RebuildTools()
            this.RaiseAll()
        }
        catch (e)
        {
            this.SetError(TextFormat.Format(TemplateEditorViewModel.CatalogFailedFormat, TextFormat.Message(e)))
        }
    }

    /** Shows a template from the tree (its fields from `get_template`; a broken one from the defaults). */
    public async Open(node: TemplateTreeNodeViewModel): Promise<void>
    {
        await this.Load(node, false)
    }

    /** Re-reads the shown template (it changed on the server); dropped when edits started meanwhile. */
    public async Reload(node: TemplateTreeNodeViewModel): Promise<void>
    {
        if (this.IsDirty)
        {
            return
        }
        await this.Load(node, true)
    }

    /** Starts an unsaved `Untitled` draft in the group. */
    public OpenNew(path: string): void
    {
        this.openToken++
        this.open = true
        this.templateId = null
        this.path = path
        this.isNew = true
        this.broken = false
        this.errorText = ''
        this.Fill(this.Defaults(TemplateEditorViewModel.DefaultName, '', ''))
    }

    /** Back to the empty state (nothing shown). */
    public Clear(): void
    {
        this.openToken++
        this.open = false
        this.templateId = null
        this.path = ''
        this.isNew = false
        this.broken = false
        this.errorText = ''
        this.Fill(this.Defaults('', '', ''))
    }

    /** The shown template moved to another group (a save goes to where it is now). */
    public SetPath(path: string): void
    {
        const old = this.path
        if (old !== path)
        {
            this.path = path
            this.RaisePropertyChanged(TemplateEditorViewModel.PropPath, old, path)
            this.Changed()
        }
    }

    /** Sends the form to `save_template`; resolves true when saved. */
    public async Save(): Promise<boolean>
    {
        const template = this.CanSave ? this.Build() : null
        if (template === null)
        {
            return false
        }
        const sent = this.Fingerprint()
        this.saving = true
        this.Changed()
        try
        {
            const id = await this.client.SaveTemplate(this.templateId ?? undefined, this.path, template)
            this.templateId = id
            this.isNew = false
            this.broken = false
            this.errorText = ''
            this.loaded = template
            this.baseline = sent
            this.OnSaved.Raise(id)
            return true
        }
        catch (e)
        {
            this.errorText = TextFormat.Message(e)
            return false
        }
        finally
        {
            this.saving = false
            this.RaiseAll()
        }
    }

    /** Drops the unsaved edits (back to what was loaded or last saved). */
    public Revert(): void
    {
        if (this.loaded !== null)
        {
            if (!this.broken)
            {
                this.errorText = ''
            }
            this.Fill(this.loaded)
        }
    }

    public ToolToggled(choice: ToolChoice): void
    {
        if (choice.Enabled)
        {
            this.enabled.add(choice.Name)
        }
        else
        {
            this.enabled.delete(choice.Name)
        }
        this.Changed()
    }

    private async Load(node: TemplateTreeNodeViewModel, onlyIfClean: boolean): Promise<void>
    {
        const token = ++this.openToken
        const id = node.TemplateId
        if (id === null)
        {
            return
        }
        if (node.Error !== null)
        {
            this.Seed(node, id, node.Error)
            return
        }
        try
        {
            const record = await this.client.GetTemplate(id)
            if (token !== this.openToken || (onlyIfClean && this.IsDirty))
            {
                return
            }
            this.open = true
            this.templateId = record.id
            this.path = record.path
            this.isNew = false
            this.broken = false
            this.errorText = ''
            this.Fill(record.template)
        }
        catch (e)
        {
            if (token === this.openToken && !(onlyIfClean && this.IsDirty))
            {
                this.Seed(node, id, TextFormat.Message(e))
            }
        }
    }

    /** Opens a template whose file cannot be read: the defaults, overlaid with what the tree node carries. */
    private Seed(node: TemplateTreeNodeViewModel, id: string, error: string): void
    {
        const leaf = node.IsTemplate ? node.Node as TemplateLeafNode : null
        this.open = true
        this.templateId = id
        this.path = node.Path
        this.isNew = false
        this.broken = true
        this.errorText = error
        this.Fill(this.Defaults(node.Name, leaf?.runtime ?? '', leaf?.model ?? ''))
    }

    /** A fresh template: the given name / runtime / model, else the catalog's first runtime and a model it serves. */
    private Defaults(name: string, runtime: string, model: string): AgentTemplate
    {
        const catalog = this.catalog
        const chosenRuntime = runtime.length > 0 ? runtime : catalog?.runtimes[0]?.name ?? ''
        const chosenModel = model.length > 0
            ? model
            : catalog?.models.find(m => m.runtimes.includes(chosenRuntime))?.name ?? ''
        return {
            name,
            system_prompt: '',
            tools: [],
            step_budget: TemplateEditorViewModel.DefaultStepBudget,
            runtime: chosenRuntime,
            model: chosenModel,
            n_ctx: TemplateEditorViewModel.DefaultContextSize,
            enable_thinking: false,
        }
    }

    /** Fills the form from a template; that becomes the clean state. */
    private Fill(template: AgentTemplate): void
    {
        this.loaded = template
        this.name = template.name
        this.systemPrompt = template.system_prompt
        this.stepBudgetText = String(template.step_budget)
        this.runtime = template.runtime
        this.model = template.model
        this.contextSizeText = String(template.n_ctx)
        this.enableThinking = template.enable_thinking
        this.sampling = template.sampling
        this.temperatureText = template.sampling?.temperature !== undefined ? String(template.sampling.temperature) : ''
        this.topPText = template.sampling?.top_p !== undefined ? String(template.sampling.top_p) : ''
        this.host = template.host ?? ''
        this.enabled = new Set(template.tools)
        this.RebuildChoices()
        this.RebuildTools()
        this.baseline = this.Fingerprint()
        this.RaiseAll()
    }

    /** The template the form describes, or null while a field does not parse. */
    private Build(): AgentTemplate | null
    {
        const stepBudget = TemplateEditorViewModel.ParseCount(this.stepBudgetText)
        const contextSize = TemplateEditorViewModel.ParseCount(this.contextSizeText)
        const temperature = TemplateEditorViewModel.ParseOptional(this.temperatureText)
        const topP = TemplateEditorViewModel.ParseOptional(this.topPText)
        if (stepBudget === null || contextSize === null || temperature === null || topP === null || this.HasFieldErrors)
        {
            return null
        }
        const template: AgentTemplate = {
            name: this.name.trim(),
            system_prompt: this.systemPrompt,
            tools: this.Tools.ToArray().filter(t => t.Enabled).map(t => t.Name),
            step_budget: stepBudget,
            runtime: this.runtime,
            model: this.model,
            n_ctx: contextSize,
            enable_thinking: this.enableThinking,
        }
        if (temperature !== undefined || topP !== undefined)
        {
            // The knobs this editor does not show (top_k, seed, ...) are kept as loaded.
            const sampling: Partial<SamplingConfig> = { ...this.sampling }
            delete sampling.temperature
            delete sampling.top_p
            if (temperature !== undefined)
            {
                sampling.temperature = temperature
            }
            if (topP !== undefined)
            {
                sampling.top_p = topP
            }
            template.sampling = sampling
        }
        if (this.host.length > 0)
        {
            template.host = this.host
        }
        return template
    }

    private get HasFieldErrors(): boolean
    {
        return this.HasNameError || this.HasStepBudgetError || this.HasContextSizeError || this.HasTemperatureError || this.HasTopPError
    }

    /** Rebuilds the runtime / model / host lists for the current values (a value the catalog lacks stays, marked). */
    private RebuildChoices(): void
    {
        const catalog = this.catalog
        const runtimes = (catalog?.runtimes ?? []).map(r => new ChoiceItem(r.name))
        const models = (catalog?.models ?? []).filter(m => m.runtimes.includes(this.runtime)).map(m => new ChoiceItem(m.name))
        const hosts = [new ChoiceItem('', TemplateEditorViewModel.AnyHostLabel), ...(catalog?.hosts ?? []).map(h => new ChoiceItem(h))]
        TemplateEditorViewModel.Replace(this.RuntimeChoices, TemplateEditorViewModel.WithValue(runtimes, this.runtime))
        TemplateEditorViewModel.Replace(this.ModelChoices, TemplateEditorViewModel.WithValue(models, this.model))
        TemplateEditorViewModel.Replace(this.HostChoices, TemplateEditorViewModel.WithValue(hosts, this.host))
    }

    /** One checkbox per registry tool, then any enabled tool the registry lacks (so it can be seen and unticked). */
    private RebuildTools(): void
    {
        const registry = this.registry
        const choices = (registry ?? []).map(t => new ToolChoice(this, t.name, t.description, this.enabled.has(t.name)))
        const missingDescription = registry === null ? '' : TemplateEditorViewModel.NotInRegistry
        for (const name of this.enabled)
        {
            if (!choices.some(c => c.Name === name))
            {
                choices.push(new ToolChoice(this, name, missingDescription, true))
            }
        }
        this.Tools.Batch(() =>
        {
            this.Tools.Clear()
            for (const choice of choices)
            {
                this.Tools.Add(choice)
            }
        })
    }

    /** The offered choices plus the current value when it is not among them. */
    private static WithValue(choices: ChoiceItem[], value: string): ChoiceItem[]
    {
        return value.length === 0 || choices.some(c => c.Value === value) ? choices : [...choices, ChoiceItem.Missing(value)]
    }

    /** Replaces a choice list's items, keeping it unchanged (no notification) when the values and labels match. */
    private static Replace(target: ObservableCollection<ChoiceItem>, choices: ChoiceItem[]): void
    {
        const current = target.ToArray()
        if (current.length === choices.length && current.every((c, i) => c.Value === choices[i].Value && c.Label === choices[i].Label))
        {
            return
        }
        target.Batch(() =>
        {
            target.Clear()
            for (const choice of choices)
            {
                target.Add(choice)
            }
        })
    }

    private static FindChoice(choices: ObservableCollection<ChoiceItem>, value: string): ChoiceItem | null
    {
        return choices.ToArray().find(c => c.Value === value) ?? null
    }

    /** A whole number of at least 1, or null. */
    private static ParseCount(text: string): number | null
    {
        const trimmed = text.trim()
        if (!TemplateEditorViewModel.Whole.test(trimmed))
        {
            return null
        }
        const value = Number(trimmed)
        return value >= 1 ? value : null
    }

    /** Blank gives undefined (no value), a decimal its number, anything else null. */
    private static ParseOptional(text: string): number | undefined | null
    {
        const trimmed = text.trim()
        if (trimmed.length === 0)
        {
            return undefined
        }
        return TemplateEditorViewModel.Decimal.test(trimmed) ? Number(trimmed) : null
    }

    /** What the form holds, as one comparable string (the tool set order-free). */
    private Fingerprint(): string
    {
        return JSON.stringify([this.name, this.systemPrompt, this.stepBudgetText, this.runtime, this.model, this.contextSizeText,
            this.enableThinking, this.temperatureText, this.topPText, this.host, [...this.enabled].sort()])
    }

    private SetField<T>(prop: string, old: T, value: T, assign: () => void): void
    {
        if (old === value)
        {
            return
        }
        assign()
        this.RaisePropertyChanged(prop, old, value)
        this.Changed()
    }

    private SetError(text: string): void
    {
        this.errorText = text
        this.Changed()
    }

    /** Raises everything derived from the form (dirty, errors, choices) and refreshes the commands. */
    private Changed(): void
    {
        this.RaiseNamed(TemplateEditorViewModel.DerivedProps)
        this.saveCommand.RaiseCanExecuteChanged()
        this.revertCommand.RaiseCanExecuteChanged()
    }

    private RaiseAll(): void
    {
        this.RaiseNamed(TemplateEditorViewModel.FieldProps)
        this.Changed()
    }

    private RaiseNamed(names: readonly string[]): void
    {
        for (const name of names)
        {
            this.RaisePropertyChanged(name, undefined, (this as unknown as Record<string, unknown>)[name])
        }
    }
}
