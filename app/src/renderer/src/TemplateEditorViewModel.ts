import { Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { TypedSignal, type IDisposable } from './JsonRpcConnection'
import type { RuntimeClient } from './RuntimeClient'
import { RepositoryKind, TemplateRepository, type AgentTemplate, type CatalogInfo, type CatalogModel, type CatalogRepository, type ContextStrategy,
    type OpenRouterModelInfo, type SamplingConfig, type TemplateLeafNode, type ToolSpec, type WorkspaceMode } from './agents'
import type { TemplateTreeNodeViewModel } from './TemplateTreeNodeViewModel'
import { ChoiceItem } from './ChoiceItem'
import { ToolChoice, type IToolChoiceOwner } from './ToolChoice'
import { TextFormat } from './TextFormat'

/**
 * The host-area template editor. One stable instance: Open shows a template from the tree, OpenNew an unsaved
 * `Untitled` draft, Clear the empty state. The number fields are text so a typo can be shown rather than sent: one
 * that does not parse gets a field error and disables Save. Repository / runtime / model come from `list_catalog`:
 * the models are the repository's, a Pragmatic one's filtered by the runtime; an OpenRouter repository has no runtime
 * choice (its runtime is `openrouter`, the Runtime row hidden), and picking one of its models sets the context size
 * to min(context_length, 32768). A value the catalog lacks stays as a marked choice. A template saved before
 * repositories (`host`, or neither) reads as its host or `hive`; Save always sends `repository`, never `host`.
 * A template that parses but fails validation (`get_template`'s `error`, the tree marks it broken) opens with its real fields and the error in
 * ErrorText; only a file that does not load at all (it does not parse, or its id is a duplicate) opens with the
 * fields seeded from the defaults plus what the tree node carries. Either way Save repairs it. Validation errors from
 * `save_template` show in ErrorText; a catalog failure shows in CatalogError until a retry (EnsureCatalog, on the
 * next open or refresh) succeeds. A `repositories_changed` re-reads the catalog (unsaved edits stay), so the repository
 * and model choices follow the Models rail. Switching from an OpenRouter repository back to a Pragmatic one restores the
 * runtime and context size last used on a Pragmatic one (else the loaded template's, when it was not OpenRouter).
 *
 * HiddenTextRule (see RepositoryEditorViewModel): a bound text changed while its part is collapsed keeps its old
 * render, so Changed raises the visibility props that are now true first and those now false last.
 */
export class TemplateEditorViewModel extends Observable implements IToolChoiceOwner
{
    private static readonly DefaultName = 'Untitled'
    private static readonly DefaultStepBudget = 8
    private static readonly DefaultContextSize = 8192
    private static readonly DefaultContextStrategy: ContextStrategy = 'off'
    private static readonly DefaultCompactAtPct = 75
    private static readonly MinCompactAtPct = 10
    private static readonly MaxCompactAtPct = 95
    /** The runtime value of every OpenRouter template. */
    private static readonly OpenRouterRuntime = 'openrouter'
    /** The default context size of an OpenRouter model is its context length, capped here. */
    private static readonly OpenRouterContextCap = 32768
    private static readonly RootLabel = '(root)'
    private static readonly NewHeaderFormat = 'New template in {0}'
    private static readonly HeaderFormat = '{0} in {1}'
    private static readonly EmptyText = 'Select a template to edit, or create a new one.'
    private static readonly NotInRegistry = 'not in the tool registry'
    private static readonly NameRequired = 'Name is required.'
    private static readonly StepBudgetInvalid = 'Step budget must be a whole number of at least 1.'
    private static readonly ContextSizeInvalid = 'Context size must be a whole number of tokens.'
    private static readonly CompactAtInvalidFormat = 'Compact at must be a whole percentage from {0} to {1}.'
    private static readonly TemperatureInvalid = 'Temperature must be a number of 0 or more, or blank.'
    private static readonly TopPInvalid = 'Top P must be a number from 0 to 1, or blank.'
    private static readonly CatalogFailedFormat = 'catalog unavailable: {0}'
    private static readonly DeletedText = 'This template was deleted; Save recreates it.'
    /** The worker sampling defaults (backend SamplingConfig::default), sent for a blank field. */
    private static readonly DefaultTemperature = 0.8
    private static readonly DefaultTopP = 0.95
    private static readonly Whole = /^\d+$/
    private static readonly Decimal = /^[+-]?(\d+\.?\d*|\.\d+)$/
    private static readonly PropName = 'Name'
    private static readonly PropSystemPrompt = 'SystemPrompt'
    private static readonly PropStepBudgetText = 'StepBudgetText'
    private static readonly PropRuntime = 'Runtime'
    private static readonly PropModel = 'Model'
    private static readonly PropContextSizeText = 'ContextSizeText'
    private static readonly PropContext = 'Context'
    private static readonly PropCompactAtText = 'CompactAtText'
    private static readonly PropEnableThinking = 'EnableThinking'
    private static readonly PropTemperatureText = 'TemperatureText'
    private static readonly PropTopPText = 'TopPText'
    private static readonly PropRepository = 'Repository'
    private static readonly PropWorkspace = 'Workspace'
    private static readonly PropWorkspaceName = 'WorkspaceName'
    private static readonly WorkspaceNamed: WorkspaceMode = 'named'
    private static readonly DefaultWorkspace: WorkspaceMode = 'session'
    /** What a pre-repository runtime's hosts are taken to offer (the editor only reads their kind and models). */
    private static readonly LegacyCapabilities = { measure_prompt: 'exact', tokenize: true, raw_prompt: true, logprobs: true, load_unload: true } as const
    /** The backend's workspace name rule (WorkspaceConfig::validate), with its messages verbatim. */
    private static readonly WorkspaceNamePattern = /^[A-Za-z0-9_-]{1,64}$/
    private static readonly WorkspaceNameRequired = 'workspace name is required for named mode'
    private static readonly WorkspaceNameInvalid = 'workspace name must be 1-64 letters, digits, _ or -'
    private static readonly WorkspaceNameReserved = 'workspace name is a reserved Windows device name'
    /** Windows device names (the backend's Sandbox::RESERVED); a folder with one of these names cannot be made. */
    private static readonly ReservedDeviceNames: ReadonlySet<string> = new Set(['CON', 'PRN', 'AUX', 'NUL', 'CONIN$', 'CONOUT$',
        'COM1', 'COM2', 'COM3', 'COM4', 'COM5', 'COM6', 'COM7', 'COM8', 'COM9', 'COM¹', 'COM²', 'COM³',
        'LPT1', 'LPT2', 'LPT3', 'LPT4', 'LPT5', 'LPT6', 'LPT7', 'LPT8', 'LPT9', 'LPT¹', 'LPT²', 'LPT³'])
    private static readonly WorkspaceModes: readonly (readonly [WorkspaceMode, string])[] = [['session', 'Session'], ['shared', 'Shared'], ['named', 'Named']]
    private static readonly PropPath = 'Path'
    private static readonly PropTemplateId = 'TemplateId'
    private static readonly StrategyChoices: readonly (readonly [ContextStrategy, string])[] = [['off', 'Off'], ['trim', 'Trim'], ['summarize', 'Summarize']]
    private static readonly FieldProps = [TemplateEditorViewModel.PropName, TemplateEditorViewModel.PropSystemPrompt,
        TemplateEditorViewModel.PropStepBudgetText, TemplateEditorViewModel.PropRuntime, TemplateEditorViewModel.PropModel,
        TemplateEditorViewModel.PropContextSizeText, TemplateEditorViewModel.PropContext, TemplateEditorViewModel.PropCompactAtText,
        TemplateEditorViewModel.PropEnableThinking,
        TemplateEditorViewModel.PropTemperatureText, TemplateEditorViewModel.PropTopPText, TemplateEditorViewModel.PropRepository,
        TemplateEditorViewModel.PropWorkspace, TemplateEditorViewModel.PropWorkspaceName,
        TemplateEditorViewModel.PropPath, TemplateEditorViewModel.PropTemplateId]
    private static readonly DerivedProps = ['HasTemplate', 'IsEmpty', 'IsNew', 'IsDirty', 'CanSave', 'CanRevert', 'IsLoading',
        'IsEditable', 'CatalogError', 'HasCatalogError', 'HeaderText', 'ErrorText',
        'HasErrorText', 'NameError', 'HasNameError', 'StepBudgetError', 'HasStepBudgetError', 'ContextSizeError',
        'HasContextSizeError', 'CompactAtError', 'HasCompactAtError', 'ContextChoice', 'TemperatureError', 'HasTemperatureError', 'TopPError', 'HasTopPError', 'RuntimeChoice',
        'ModelChoice', 'RepositoryChoice', 'WorkspaceChoice', 'IsWorkspaceNameEnabled', 'WorkspaceNameError', 'HasWorkspaceNameError']

    /** Bound to a Visibility (true = shown); raised around the other props per HiddenTextRule. */
    private static readonly VisibilityProps = ['IsRuntimeVisible']

    public readonly RepositoryChoices = new ObservableCollection<ChoiceItem>()
    public readonly RuntimeChoices = new ObservableCollection<ChoiceItem>()
    public readonly ModelChoices = new ObservableCollection<ChoiceItem>()
    /** Off, Trim, Summarize: what the session does when the prompt nears the window. */
    public readonly ContextChoices = new ObservableCollection<ChoiceItem>()
    /** Session, Shared, Named: which folder the file tools write to. */
    public readonly WorkspaceChoices = new ObservableCollection<ChoiceItem>()
    public readonly Tools = new ObservableCollection<ToolChoice>()
    /** A save succeeded; carries the template id (new for a draft). */
    public readonly OnSaved = new TypedSignal<string>()

    private readonly client: RuntimeClient
    private readonly repositoriesChanged: IDisposable
    private readonly saveCommand: RelayCommand
    private readonly revertCommand: RelayCommand
    private catalog: CatalogInfo | null = null
    private registry: ToolSpec[] | null = null
    private open = false
    private templateId: string | null = null
    private path = ''
    private isNew = false
    /** The template is broken (fails validation, or did not load): Save (which repairs it) is allowed while clean. */
    private broken = false
    /** The file did not load, so the fields came from the defaults (a catalog arriving later completes them). */
    private seeded = false
    private saving = false
    /** `get_template` for the shown template is in flight. */
    private loading = false
    private catalogLoading = false
    private catalogError = ''
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
    private context: ContextStrategy = TemplateEditorViewModel.DefaultContextStrategy
    private compactAtText = ''
    private enableThinking = false
    private temperatureText = ''
    private topPText = ''
    private repository: string = TemplateRepository.Default
    private workspace: WorkspaceMode = TemplateEditorViewModel.DefaultWorkspace
    private workspaceName = ''
    private sampling: Partial<SamplingConfig> | undefined = undefined
    private enabled = new Set<string>()
    /** The runtime and context size last set while a Pragmatic repository was chosen (restored when one is again). */
    private lastPragmatic: { runtime: string, contextSizeText: string } | null = null

    constructor(client: RuntimeClient)
    {
        super()
        this.client = client
        this.repositoriesChanged = client.OnRepositoriesChanged.Subscribe(() => void this.LoadCatalog())
        for (const choice of TemplateEditorViewModel.StrategyChoices)
        {
            this.ContextChoices.Add(new ChoiceItem(choice[0], choice[1]))
        }
        for (const choice of TemplateEditorViewModel.WorkspaceModes)
        {
            this.WorkspaceChoices.Add(new ChoiceItem(choice[0], choice[1]))
        }
        this.saveCommand = new RelayCommand(() => void this.Save(), () => this.CanSave)
        this.revertCommand = new RelayCommand(() => this.Revert(), () => this.CanRevert)
        this.Fill(this.Defaults('', '', '', false))
    }

    public get SaveCommand(): ICommand
    {
        return this.saveCommand
    }

    /** Stops following `repositories_changed`. */
    public dispose(): void
    {
        this.repositoriesChanged.Dispose()
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
        return this.open && !this.saving && !this.loading && (this.IsDirty || this.broken) && !this.HasFieldErrors
    }

    public get CanRevert(): boolean
    {
        return this.open && !this.saving && !this.loading && this.Fingerprint() !== this.baseline
    }

    /** The shown template's `get_template` is in flight: the form is read-only until it lands. */
    public get IsLoading(): boolean
    {
        return this.loading
    }

    /** The form accepts edits (a template is shown and not loading or saving). */
    public get IsEditable(): boolean
    {
        return this.open && !this.loading && !this.saving
    }

    /** Why the catalog is unknown (kept until a retry succeeds), or "". */
    public get CatalogError(): string
    {
        return this.catalogError
    }

    public get HasCatalogError(): boolean
    {
        return this.catalogError.length > 0
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

    /** Picking an OpenRouter model also sets the runtime to `openrouter` and the context size to its default. */
    public set Model(value: string)
    {
        this.SetField(TemplateEditorViewModel.PropModel, this.model, value, () =>
        {
            this.model = value
            this.ApplyOpenRouterModel()
            this.RebuildChoices()
        }, true)
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

    public get Workspace(): WorkspaceMode
    {
        return this.workspace
    }

    public set Workspace(value: WorkspaceMode)
    {
        this.SetField(TemplateEditorViewModel.PropWorkspace, this.workspace, value, () => { this.workspace = value })
    }

    public get WorkspaceChoice(): ChoiceItem | null
    {
        return TemplateEditorViewModel.FindChoice(this.WorkspaceChoices, this.workspace)
    }

    public set WorkspaceChoice(choice: ChoiceItem | null)
    {
        if (choice instanceof ChoiceItem)
        {
            this.Workspace = choice.Value as WorkspaceMode
        }
    }

    public get WorkspaceName(): string
    {
        return this.workspaceName
    }

    public set WorkspaceName(value: string)
    {
        this.SetField(TemplateEditorViewModel.PropWorkspaceName, this.workspaceName, value, () => { this.workspaceName = value })
    }

    /** The name box is live only for a Named workspace. */
    public get IsWorkspaceNameEnabled(): boolean
    {
        return this.workspace === TemplateEditorViewModel.WorkspaceNamed
    }

    /** A Named workspace needs a valid name; any other mode ignores the name. */
    public get WorkspaceNameError(): string
    {
        if (!this.open || !this.IsWorkspaceNameEnabled)
        {
            return ''
        }
        const name = this.workspaceName.trim()
        if (name.length === 0)
        {
            return TemplateEditorViewModel.WorkspaceNameRequired
        }
        if (!TemplateEditorViewModel.WorkspaceNamePattern.test(name))
        {
            return TemplateEditorViewModel.WorkspaceNameInvalid
        }
        return TemplateEditorViewModel.ReservedDeviceNames.has(name.toUpperCase()) ? TemplateEditorViewModel.WorkspaceNameReserved : ''
    }

    public get HasWorkspaceNameError(): boolean
    {
        return this.WorkspaceNameError.length > 0
    }

    public get Context(): ContextStrategy
    {
        return this.context
    }

    public set Context(value: ContextStrategy)
    {
        this.SetField(TemplateEditorViewModel.PropContext, this.context, value, () => { this.context = value })
    }

    public get ContextChoice(): ChoiceItem | null
    {
        return TemplateEditorViewModel.FindChoice(this.ContextChoices, this.context)
    }

    public set ContextChoice(choice: ChoiceItem | null)
    {
        if (choice instanceof ChoiceItem)
        {
            this.Context = choice.Value as ContextStrategy
        }
    }

    /** The percent of the window the prompt may fill before compaction runs (10 to 95). */
    public get CompactAtText(): string
    {
        return this.compactAtText
    }

    public set CompactAtText(value: string)
    {
        this.SetField(TemplateEditorViewModel.PropCompactAtText, this.compactAtText, value, () => { this.compactAtText = value })
    }

    public get CompactAtError(): string
    {
        return this.open && TemplateEditorViewModel.ParsePercent(this.compactAtText) === null
            ? TextFormat.Format(TemplateEditorViewModel.CompactAtInvalidFormat, TemplateEditorViewModel.MinCompactAtPct, TemplateEditorViewModel.MaxCompactAtPct)
            : ''
    }

    public get HasCompactAtError(): boolean
    {
        return this.CompactAtError.length > 0
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

    /** The repository the model comes from (`hive` when the template names none). */
    public get Repository(): string
    {
        return this.repository
    }

    /**
     * Changing the repository re-lists the models. An OpenRouter one sets the runtime to `openrouter`; a Pragmatic one
     * takes the catalog's first runtime when the current one is not a catalog runtime; coming back from an OpenRouter
     * one it restores the last Pragmatic runtime and context size. A model the repository lacks is replaced by its first
     * (an OpenRouter pick then sets the context size).
     */
    public set Repository(value: string)
    {
        this.SetField(TemplateEditorViewModel.PropRepository, this.repository, value, () =>
        {
            const fromOpenRouter = this.IsOpenRouter
            if (!fromOpenRouter)
            {
                this.lastPragmatic = { runtime: this.runtime, contextSizeText: this.contextSizeText }
            }
            this.repository = value
            this.FitToRepository(fromOpenRouter)
            this.RebuildChoices()
        }, true)
    }

    public get RepositoryChoice(): ChoiceItem | null
    {
        return TemplateEditorViewModel.FindChoice(this.RepositoryChoices, this.repository)
    }

    public set RepositoryChoice(choice: ChoiceItem | null)
    {
        if (choice instanceof ChoiceItem)
        {
            this.Repository = choice.Value
        }
    }

    /** The Runtime row shows only for a Pragmatic repository (an unknown one counts as Pragmatic). */
    public get IsRuntimeVisible(): boolean
    {
        return !this.IsOpenRouter
    }

    /** Reads the catalog (repository / runtime / model choices) and the whole tool registry (the tool checkboxes). */
    public async LoadCatalog(): Promise<void>
    {
        this.catalogLoading = true
        try
        {
            const [catalog, registry] = await Promise.all([this.client.ListCatalog(), this.client.ListAllTools()])
            const pristine = this.Fingerprint() === this.baseline
            this.catalog = catalog
            this.registry = registry
            this.catalogError = ''
            const loaded = this.loaded
            if (this.open && pristine && loaded !== null && !this.loading)
            {
                if (this.isNew)
                {
                    // A draft started before the catalog arrived: complete its runtime, model and tools.
                    this.Fill(this.Defaults(loaded.name, loaded.runtime, loaded.model, true))
                    return
                }
                if (this.seeded && (loaded.runtime === '' || loaded.model === ''))
                {
                    this.Fill(this.Defaults(loaded.name, loaded.runtime, loaded.model, false))
                    return
                }
            }
            this.RebuildChoices()
            this.RebuildTools()
            this.RaiseAll()
        }
        catch (e)
        {
            this.catalogError = TextFormat.Format(TemplateEditorViewModel.CatalogFailedFormat, TextFormat.Message(e))
            this.RebuildChoices()
            this.Changed()
        }
        finally
        {
            this.catalogLoading = false
        }
    }

    /** Retries the catalog when it is still unknown (after a failure); a no-op once it is known or while loading. */
    public EnsureCatalog(): void
    {
        if (this.catalog === null && !this.catalogLoading)
        {
            void this.LoadCatalog()
        }
    }

    /**
     * Shows a template from the tree. The id is recorded at once (the form goes read-only until `get_template`
     * replies), so a refresh landing meanwhile targets this template; one whose file does not load opens from the
     * defaults.
     */
    public async Open(node: TemplateTreeNodeViewModel): Promise<void>
    {
        this.EnsureCatalog()
        await this.Load(node, false)
    }

    /**
     * Re-reads the shown template (it changed on the server). Ignored for another template than the one last
     * requested, and dropped when edits started before the reply.
     */
    public async Reload(node: TemplateTreeNodeViewModel): Promise<void>
    {
        if (node.TemplateId === null || node.TemplateId !== this.templateId || this.IsDirty)
        {
            return
        }
        await this.Load(node, true)
    }

    /** Starts an unsaved `Untitled` draft in the group, with every registry tool enabled. */
    public OpenNew(path: string): void
    {
        this.openToken++
        this.open = true
        this.templateId = null
        this.path = path
        this.isNew = true
        this.broken = false
        this.seeded = false
        this.loading = false
        this.errorText = ''
        this.Fill(this.Defaults(TemplateEditorViewModel.DefaultName, '', '', true))
        this.EnsureCatalog()
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
        this.seeded = false
        this.loading = false
        this.errorText = ''
        this.Fill(this.Defaults('', '', '', false))
    }

    /**
     * The shown template was deleted elsewhere while it had unsaved edits: keep them as an unsaved draft in the same
     * group (Save recreates it), with a notice.
     */
    public Orphan(): void
    {
        this.openToken++
        this.templateId = null
        this.isNew = true
        this.broken = false
        this.seeded = false
        this.loading = false
        this.errorText = TemplateEditorViewModel.DeletedText
        this.RaiseAll()
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
            this.seeded = false
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
        if (!onlyIfClean)
        {
            // Record the request now: the tree shows this template, so must everything that asks the editor.
            this.open = true
            this.templateId = id
            this.path = node.Path
            this.isNew = false
            this.broken = false
            this.errorText = ''
        }
        this.loading = true
        this.RaiseAll()
        try
        {
            const record = await this.client.GetTemplate(id)
            if (token !== this.openToken || record.id !== this.templateId || (onlyIfClean && this.IsDirty))
            {
                return
            }
            this.loading = false
            this.open = true
            this.path = record.path
            this.isNew = false
            // A validation failure still reads: the real fields, with the reason it is flagged.
            this.broken = record.error !== null
            this.seeded = false
            this.errorText = record.error ?? ''
            this.Fill(record.template)
        }
        catch (e)
        {
            if (token === this.openToken && !(onlyIfClean && this.IsDirty))
            {
                this.Seed(node, id, node.Error ?? TextFormat.Message(e))
            }
        }
        finally
        {
            if (token === this.openToken && this.loading)
            {
                this.loading = false
                this.Changed()
            }
        }
    }

    /** Opens a template whose file does not load: the defaults, overlaid with what the tree node carries. */
    private Seed(node: TemplateTreeNodeViewModel, id: string, error: string): void
    {
        const leaf = node.IsTemplate ? node.Node as TemplateLeafNode : null
        this.open = true
        this.templateId = id
        this.path = node.Path
        this.isNew = false
        this.broken = true
        this.seeded = true
        this.loading = false
        this.errorText = error
        this.Fill(this.Defaults(node.Name, leaf?.runtime ?? '', leaf?.model ?? '', false))
    }

    /**
     * A fresh template: the given name / runtime / model, else the catalog's first runtime and a model it serves;
     * with `allTools` every registry tool is enabled (as in the seeded Default).
     */
    private Defaults(name: string, runtime: string, model: string, allTools: boolean): AgentTemplate
    {
        const catalog = this.catalog
        const chosenRuntime = runtime.length > 0 ? runtime : catalog?.runtimes[0]?.name ?? ''
        const chosenModel = model.length > 0
            ? model
            : catalog?.models.find(m => m.runtimes.includes(chosenRuntime))?.name ?? ''
        return {
            name,
            system_prompt: '',
            tools: allTools ? (this.registry ?? []).map(t => t.name) : [],
            step_budget: TemplateEditorViewModel.DefaultStepBudget,
            runtime: chosenRuntime,
            model: chosenModel,
            n_ctx: TemplateEditorViewModel.DefaultContextSize,
            enable_thinking: false,
            context_strategy: TemplateEditorViewModel.DefaultContextStrategy,
            compact_at_pct: TemplateEditorViewModel.DefaultCompactAtPct,
            repository: this.DefaultRepository(chosenRuntime, chosenModel),
        }
    }

    /**
     * The repository of a fresh or seeded template: for an `openrouter` runtime the OpenRouter repository listing the
     * model (else the first OpenRouter one), otherwise `hive`.
     */
    private DefaultRepository(runtime: string, model: string): string
    {
        if (runtime !== TemplateEditorViewModel.OpenRouterRuntime)
        {
            return TemplateRepository.Default
        }
        const remote = this.Repositories().filter(r => r.kind === RepositoryKind.OpenRouter)
        return (remote.find(r => TemplateEditorViewModel.ModelIds(r).includes(model)) ?? remote[0])?.name ?? TemplateRepository.Default
    }

    /** Fills the form from a template; that becomes the clean state. */
    private Fill(template: AgentTemplate): void
    {
        this.loaded = template
        this.lastPragmatic = null
        this.name = template.name
        this.systemPrompt = template.system_prompt
        this.stepBudgetText = String(template.step_budget)
        this.runtime = template.runtime
        this.model = template.model
        this.contextSizeText = String(template.n_ctx)
        this.context = template.context_strategy ?? TemplateEditorViewModel.DefaultContextStrategy
        this.compactAtText = String(template.compact_at_pct ?? TemplateEditorViewModel.DefaultCompactAtPct)
        this.enableThinking = template.enable_thinking
        this.sampling = template.sampling
        this.temperatureText = template.sampling?.temperature !== undefined ? String(template.sampling.temperature) : ''
        this.topPText = template.sampling?.top_p !== undefined ? String(template.sampling.top_p) : ''
        this.repository = TemplateRepository.Of(template)
        this.workspace = template.workspace?.mode ?? TemplateEditorViewModel.DefaultWorkspace
        this.workspaceName = template.workspace?.name ?? ''
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
        const compactAt = TemplateEditorViewModel.ParsePercent(this.compactAtText)
        const temperature = TemplateEditorViewModel.ParseOptional(this.temperatureText)
        const topP = TemplateEditorViewModel.ParseOptional(this.topPText)
        if (stepBudget === null || contextSize === null || compactAt === null || temperature === null || topP === null || this.HasFieldErrors)
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
            context_strategy: this.context,
            compact_at_pct: compactAt,
        }
        if (this.sampling !== undefined || temperature !== undefined || topP !== undefined)
        {
            // Every knob the editor does not show (top_k, seed, ...) is kept as loaded. The backend sampling fields
            // are not optional, so a blank field sends the worker default rather than removing the knob.
            template.sampling = {
                ...this.sampling,
                temperature: temperature ?? TemplateEditorViewModel.DefaultTemperature,
                top_p: topP ?? TemplateEditorViewModel.DefaultTopP,
            }
        }
        template.workspace = this.workspace === TemplateEditorViewModel.WorkspaceNamed
            ? { mode: this.workspace, name: this.workspaceName.trim() }
            : { mode: this.workspace }
        template.repository = this.repository
        return template
    }

    private get HasFieldErrors(): boolean
    {
        return this.HasNameError || this.HasStepBudgetError || this.HasContextSizeError || this.HasCompactAtError || this.HasTemperatureError || this.HasTopPError
            || this.HasWorkspaceNameError
    }

    /**
     * Rebuilds the repository / runtime / model lists for the current values. The models are the repository's (a
     * Pragmatic one's those the runtime loads). A value missing from them stays as a marked choice: "(not in catalog)",
     * or for a model of the repository the runtime cannot load "(not served by {runtime})". While the catalog is
     * unknown nothing is marked.
     */
    private RebuildChoices(): void
    {
        const catalog = this.catalog
        const repository = this.CurrentRepository()
        const runtimes = (catalog?.runtimes ?? []).map(r => new ChoiceItem(r.name))
        const models = this.ModelsOf(repository).map(m => new ChoiceItem(m))
        const repositories = this.Repositories().map(r => new ChoiceItem(r.name))
        const modelInRepository = repository !== null && TemplateEditorViewModel.ModelIds(repository).includes(this.model)
        TemplateEditorViewModel.Replace(this.RepositoryChoices, this.WithValue(repositories, this.repository, ChoiceItem.Missing(this.repository)))
        TemplateEditorViewModel.Replace(this.RuntimeChoices, this.WithValue(runtimes, this.runtime, ChoiceItem.Missing(this.runtime)))
        TemplateEditorViewModel.Replace(this.ModelChoices, this.WithValue(models, this.model,
            modelInRepository ? ChoiceItem.NotServed(this.model, this.runtime) : ChoiceItem.Missing(this.model)))
    }

    /** The catalog's repositories; a runtime older than repositories offers its hosts as Pragmatic ones over its models. */
    private Repositories(): CatalogRepository[]
    {
        const catalog = this.catalog
        if (catalog === null)
        {
            return []
        }
        return catalog.repositories ?? catalog.hosts.map(name => ({
            name, kind: RepositoryKind.Pragmatic, capabilities: TemplateEditorViewModel.LegacyCapabilities, models: catalog.models,
        }))
    }

    private CurrentRepository(): CatalogRepository | null
    {
        return this.Repositories().find(r => r.name === this.repository) ?? null
    }

    private get IsOpenRouter(): boolean
    {
        return this.CurrentRepository()?.kind === RepositoryKind.OpenRouter
    }

    /** The model choices of a repository: an OpenRouter one's every model, a Pragmatic one's those the runtime loads. */
    private ModelsOf(repository: CatalogRepository | null): string[]
    {
        if (repository === null)
        {
            return []
        }
        if (repository.kind === RepositoryKind.OpenRouter)
        {
            return TemplateEditorViewModel.ModelIds(repository)
        }
        return repository.models
            .filter((m): m is CatalogModel => TemplateEditorViewModel.IsCatalogModel(m) && m.runtimes.includes(this.runtime))
            .map(m => m.name)
    }

    /** Every model id a repository lists (a Pragmatic model's name, an OpenRouter model's id). */
    private static ModelIds(repository: CatalogRepository): string[]
    {
        return repository.models.map(m => TemplateEditorViewModel.IsCatalogModel(m) ? m.name : m.id)
    }

    private static IsCatalogModel(model: CatalogModel | OpenRouterModelInfo): model is CatalogModel
    {
        return !('id' in model)
    }

    /** After a repository change: the runtime and model it can run (see the Repository setter). */
    private FitToRepository(fromOpenRouter: boolean): void
    {
        const repository = this.CurrentRepository()
        if (repository === null)
        {
            return
        }
        if (repository.kind === RepositoryKind.OpenRouter)
        {
            this.runtime = TemplateEditorViewModel.OpenRouterRuntime
        }
        else
        {
            if (fromOpenRouter)
            {
                this.RestorePragmatic()
            }
            if (!(this.catalog?.runtimes ?? []).some(r => r.name === this.runtime))
            {
                this.runtime = this.catalog?.runtimes[0]?.name ?? ''
            }
        }
        const models = this.ModelsOf(repository)
        if (!models.includes(this.model) && models.length > 0)
        {
            this.model = models[0]
        }
        this.ApplyOpenRouterModel()
    }

    /**
     * Back on a Pragmatic repository from an OpenRouter one: the runtime and context size last used on a Pragmatic one,
     * else the loaded template's when it was not an OpenRouter one, else the default context size (the runtime then
     * falls to the catalog's first).
     */
    private RestorePragmatic(): void
    {
        const loaded = this.loaded
        const fallback = loaded !== null && loaded.runtime !== TemplateEditorViewModel.OpenRouterRuntime
            ? { runtime: loaded.runtime, contextSizeText: String(loaded.n_ctx) }
            : { runtime: '', contextSizeText: String(TemplateEditorViewModel.DefaultContextSize) }
        const restored = this.lastPragmatic ?? fallback
        this.runtime = restored.runtime
        this.contextSizeText = restored.contextSizeText
    }

    /** An OpenRouter model was picked: the runtime is `openrouter` and the context size min(context_length, 32768). */
    private ApplyOpenRouterModel(): void
    {
        const repository = this.CurrentRepository()
        if (repository?.kind !== RepositoryKind.OpenRouter)
        {
            return
        }
        this.runtime = TemplateEditorViewModel.OpenRouterRuntime
        const model = repository.models.find((m): m is OpenRouterModelInfo => !TemplateEditorViewModel.IsCatalogModel(m) && m.id === this.model)
        if (model !== undefined && model.context_length > 0)
        {
            this.contextSizeText = String(Math.min(model.context_length, TemplateEditorViewModel.OpenRouterContextCap))
        }
    }

    /** One checkbox per registry tool, then any enabled tool the registry lacks (so it can be seen and unticked). */
    private RebuildTools(): void
    {
        const registry = this.registry
        const choices = (registry ?? []).map(t => new ToolChoice(this, t.name, t.description, this.enabled.has(t.name), t.side_effects))
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

    /** The offered choices plus the current value when it is not among them (marked, unless the catalog is unknown). */
    private WithValue(choices: ChoiceItem[], value: string, marked: ChoiceItem): ChoiceItem[]
    {
        if (value.length === 0 || choices.some(c => c.Value === value))
        {
            return choices
        }
        return [...choices, this.catalog === null ? new ChoiceItem(value) : marked]
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

    /** A whole percentage from 10 to 95, or null. */
    private static ParsePercent(text: string): number | null
    {
        const trimmed = text.trim()
        if (!TemplateEditorViewModel.Whole.test(trimmed))
        {
            return null
        }
        const value = Number(trimmed)
        return value >= TemplateEditorViewModel.MinCompactAtPct && value <= TemplateEditorViewModel.MaxCompactAtPct ? value : null
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

    /** What the form holds, as one comparable string (the tool set order-free; the workspace name only when Named, as only then is it saved). */
    private Fingerprint(): string
    {
        const workspaceName = this.IsWorkspaceNameEnabled ? this.workspaceName : ''
        return JSON.stringify([this.name, this.systemPrompt, this.stepBudgetText, this.runtime, this.model, this.contextSizeText,
            this.context, this.compactAtText, this.enableThinking, this.temperatureText, this.topPText, this.repository, this.workspace, workspaceName, [...this.enabled].sort()])
    }

    /** Assigns and raises; `cascades` for a field whose assignment may change others (all fields are then raised). */
    private SetField<T>(prop: string, old: T, value: T, assign: () => void, cascades: boolean = false): void
    {
        if (old === value)
        {
            return
        }
        assign()
        if (cascades)
        {
            this.RaiseAll()
            return
        }
        this.RaisePropertyChanged(prop, old, value)
        this.Changed()
    }

    /**
     * Raises everything derived from the form (dirty, errors, choices) and refreshes the commands; with `withFields`
     * the fields too. Visibility props now true go first and those now false last (HiddenTextRule).
     */
    private Changed(withFields: boolean = false): void
    {
        this.RaiseVisibility(true)
        if (withFields)
        {
            this.RaiseNamed(TemplateEditorViewModel.FieldProps)
        }
        this.RaiseNamed(TemplateEditorViewModel.DerivedProps)
        this.RaiseVisibility(false)
        this.saveCommand.RaiseCanExecuteChanged()
        this.revertCommand.RaiseCanExecuteChanged()
    }

    private RaiseAll(): void
    {
        this.Changed(true)
    }

    /** Raises the visibility props whose value is `shown` (HiddenTextRule). */
    private RaiseVisibility(shown: boolean): void
    {
        this.RaiseNamed(TemplateEditorViewModel.VisibilityProps.filter(name => Boolean((this as unknown as Record<string, unknown>)[name]) === shown))
    }

    private RaiseNamed(names: readonly string[]): void
    {
        for (const name of names)
        {
            this.RaisePropertyChanged(name, undefined, (this as unknown as Record<string, unknown>)[name])
        }
    }
}
