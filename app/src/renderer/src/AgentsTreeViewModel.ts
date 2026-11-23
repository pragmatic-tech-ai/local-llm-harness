import { Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { TypedSignal, type IDisposable } from './JsonRpcConnection'
import type { RuntimeClient } from './RuntimeClient'
import { TemplateTreeNodeViewModel } from './TemplateTreeNodeViewModel'
import { ChoiceItem } from './ChoiceItem'
import { TextFormat } from './TextFormat'

/**
 * The Agents side panel: the template tree (groups and templates) under a toolbar (new template, new group, rename,
 * delete, move to, refresh). It refreshes on `templates_changed`, reconciling nodes in place so selection and
 * expansion survive. A user selection is only a request (OnSelectRequested): the owning service grants it with
 * Select, or keeps the current one (KeepSelection) when the editor has unsaved edits the user chose to keep. The
 * dialogs behind the commands belong to the service, which the panel only signals.
 */
export class AgentsTreeViewModel extends Observable
{
    private static readonly PropSelected = 'Selected'
    private static readonly PropStatusText = 'StatusText'
    private static readonly PropIsEmpty = 'IsEmpty'
    private static readonly RootLabel = '(root)'
    private static readonly RefreshFailedFormat = 'templates unavailable: {0}'
    private static readonly RuntimeOffFormat = 'The runtime is off on the supervisor: {0}'

    public readonly Roots = new ObservableCollection<TemplateTreeNodeViewModel>()
    /** The user picked a node in the tree; the owner decides (Select or KeepSelection). */
    public readonly OnSelectRequested = new TypedSignal<TemplateTreeNodeViewModel>()
    /** A refresh was applied to the tree (a selected node that disappeared is already deselected). */
    public readonly OnRefreshed = new TypedSignal<void>()
    /** New template, in the group path carried. */
    public readonly OnNewTemplateRequested = new TypedSignal<string>()
    /** New group, under the group path carried. */
    public readonly OnNewGroupRequested = new TypedSignal<string>()
    public readonly OnRenameRequested = new TypedSignal<TemplateTreeNodeViewModel>()
    public readonly OnDeleteRequested = new TypedSignal<TemplateTreeNodeViewModel>()
    public readonly OnMoveRequested = new TypedSignal<TemplateTreeNodeViewModel>()

    private readonly newTemplateCommand: RelayCommand
    private readonly newGroupCommand: RelayCommand
    private readonly renameCommand: RelayCommand
    private readonly deleteCommand: RelayCommand
    private readonly moveToCommand: RelayCommand
    private readonly refreshCommand: RelayCommand
    private _selected: TemplateTreeNodeViewModel | null = null
    private _statusText = ''
    private runtimeError = ''
    private client: RuntimeClient | null = null
    private subscriptions: IDisposable[] = []
    /** Orders refreshes so a slow early reply cannot overwrite a newer one. */
    private refreshToken = 0
    /** A template to select once a refresh lists it (a draft that was just saved). */
    private pendingSelectId: string | null = null

    constructor()
    {
        super()
        this.newTemplateCommand = new RelayCommand(() => this.OnNewTemplateRequested.Raise(this.SelectedGroupPath), () => this.IsBound)
        this.newGroupCommand = new RelayCommand(() => this.OnNewGroupRequested.Raise(this.SelectedGroupPath), () => this.IsBound)
        this.renameCommand = new RelayCommand(() => this.ForSelected(node => this.OnRenameRequested.Raise(node)), () => this.IsBound && this._selected !== null)
        this.deleteCommand = new RelayCommand(() => this.ForSelected(node => this.OnDeleteRequested.Raise(node)), () => this.IsBound && this._selected !== null)
        this.moveToCommand = new RelayCommand(() => this.ForSelected(node => this.OnMoveRequested.Raise(node)), () => this.IsBound && this._selected?.IsTemplate === true)
        this.refreshCommand = new RelayCommand(() => void this.Refresh(), () => this.IsBound)
    }

    /** Follows the runtime connection's `templates_changed`, or goes inert with null (the tree stays as last seen). */
    public Bind(client: RuntimeClient | null): void
    {
        for (const s of this.subscriptions)
        {
            s.Dispose()
        }
        this.subscriptions = []
        this.client = client
        this.refreshToken++
        if (client !== null)
        {
            this.subscriptions.push(client.OnTemplatesChanged.Subscribe(() => void this.Refresh()))
        }
        this.RaiseCommands()
    }

    public get NewTemplateCommand(): ICommand
    {
        return this.newTemplateCommand
    }

    public get NewGroupCommand(): ICommand
    {
        return this.newGroupCommand
    }

    public get RenameCommand(): ICommand
    {
        return this.renameCommand
    }

    public get DeleteCommand(): ICommand
    {
        return this.deleteCommand
    }

    public get MoveToCommand(): ICommand
    {
        return this.moveToCommand
    }

    public get RefreshCommand(): ICommand
    {
        return this.refreshCommand
    }

    public get Selected(): TemplateTreeNodeViewModel | null
    {
        return this._selected
    }

    /** A user selection is a request to the owner; clearing it keeps the current node (re-announced to the tree). */
    public set Selected(node: TemplateTreeNodeViewModel | null)
    {
        if (node === this._selected)
        {
            return
        }
        if (node === null || node === undefined)
        {
            this.KeepSelection()
            return
        }
        this.OnSelectRequested.Raise(node)
    }

    /** Makes the node the selection (granted request, or the owner's own choice); raises nothing else. */
    public Select(node: TemplateTreeNodeViewModel | null): void
    {
        const old = this._selected
        if (old === node)
        {
            return
        }
        this._selected = node
        this.RaisePropertyChanged(AgentsTreeViewModel.PropSelected, old, node)
        this.RaiseCommands()
    }

    /** Re-announces the current selection after the binding write, so a refused pick snaps back in the tree. */
    public KeepSelection(): void
    {
        void Promise.resolve().then(() =>
        {
            this.RaisePropertyChanged(AgentsTreeViewModel.PropSelected, undefined, this._selected)
        })
    }

    /** Selects the template once a refresh lists it. */
    public SelectWhenListed(id: string): void
    {
        const node = TemplateTreeNodeViewModel.FindTemplate(this.Roots, id)
        if (node !== undefined)
        {
            this.pendingSelectId = null
            this.Select(node)
            return
        }
        this.pendingSelectId = id
    }

    /** The group a new template or group goes into: the selected group, the selected template's group, else the root. */
    public get SelectedGroupPath(): string
    {
        return this._selected?.Path ?? ''
    }

    public get StatusText(): string
    {
        return this._statusText
    }

    public get IsEmpty(): boolean
    {
        return this.Roots.Count === 0
    }

    /** Shows a failed action (rename, delete, ...) on the panel's status line. */
    public ReportError(text: string): void
    {
        this.SetStatus(text)
    }

    /** Why the supervisor says the runtime is off, shown on the status line ("" clears it once the runtime is back). */
    public SetRuntimeError(text: string): void
    {
        const shown = this.RuntimeErrorText(this.runtimeError)
        this.runtimeError = text
        if (text.length > 0)
        {
            this.SetStatus(this.RuntimeErrorText(text))
        }
        else if (shown.length > 0 && this._statusText === shown)
        {
            this.SetStatus('')
        }
    }

    public Find(id: string): TemplateTreeNodeViewModel | undefined
    {
        return TemplateTreeNodeViewModel.FindTemplate(this.Roots, id)
    }

    /** Every group as a choice (value = path), the root first and labelled "(root)", then depth-first. */
    public Groups(): ChoiceItem[]
    {
        const groups = [new ChoiceItem('', AgentsTreeViewModel.RootLabel)]
        AgentsTreeViewModel.CollectGroups(this.Roots, groups)
        return groups
    }

    /** Re-reads the tree and reconciles it in place. */
    public async Refresh(): Promise<void>
    {
        const client = this.client
        if (client === null)
        {
            return
        }
        const token = ++this.refreshToken
        try
        {
            const nodes = await client.ListTemplates()
            if (token !== this.refreshToken)
            {
                return
            }
            this.SetStatus('')
            const wasEmpty = this.IsEmpty
            TemplateTreeNodeViewModel.Reconcile(this.Roots, nodes, TemplateTreeNodeViewModel.Pool(this.Roots))
            if (wasEmpty !== this.IsEmpty)
            {
                this.RaisePropertyChanged(AgentsTreeViewModel.PropIsEmpty, wasEmpty, this.IsEmpty)
            }
            this.AfterReconcile()
        }
        catch (e)
        {
            if (token === this.refreshToken)
            {
                this.SetStatus(TextFormat.Format(AgentsTreeViewModel.RefreshFailedFormat, TextFormat.Message(e)))
            }
        }
    }

    private AfterReconcile(): void
    {
        const selected = this._selected
        if (selected !== null && !this.Contains(selected))
        {
            this.Select(null)
        }
        const pending = this.pendingSelectId
        if (pending !== null)
        {
            this.SelectWhenListed(pending)
        }
        this.OnRefreshed.Raise(undefined)
    }

    private Contains(node: TemplateTreeNodeViewModel): boolean
    {
        return TemplateTreeNodeViewModel.Pool(this.Roots).get(node.Key) === node
    }

    private static CollectGroups(nodes: Iterable<TemplateTreeNodeViewModel>, into: ChoiceItem[]): void
    {
        for (const node of nodes)
        {
            if (node.IsGroup)
            {
                into.push(new ChoiceItem(node.Path))
                AgentsTreeViewModel.CollectGroups(node.Children ?? [], into)
            }
        }
    }

    private get IsBound(): boolean
    {
        return this.client !== null
    }

    private ForSelected(action: (node: TemplateTreeNodeViewModel) => void): void
    {
        const node = this._selected
        if (node !== null)
        {
            action(node)
        }
    }

    private RaiseCommands(): void
    {
        for (const command of [this.newTemplateCommand, this.newGroupCommand, this.renameCommand, this.deleteCommand, this.moveToCommand, this.refreshCommand])
        {
            command.RaiseCanExecuteChanged()
        }
    }

    private RuntimeErrorText(error: string): string
    {
        return error.length > 0 ? TextFormat.Format(AgentsTreeViewModel.RuntimeOffFormat, error) : ''
    }

    private SetStatus(text: string): void
    {
        const old = this._statusText
        if (old !== text)
        {
            this._statusText = text
            this.RaisePropertyChanged(AgentsTreeViewModel.PropStatusText, old, text)
        }
    }
}
