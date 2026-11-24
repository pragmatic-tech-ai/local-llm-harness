import { Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { TypedSignal, type IDisposable } from './JsonRpcConnection'
import type { RuntimeClient } from './RuntimeClient'
import type { RepositorySummary } from './agents'
import { RepositoryRowViewModel } from './RepositoryRowViewModel'
import { TextFormat } from './TextFormat'

/**
 * The Models side panel: the repositories (Pragmatic first, then OpenRouter) with their kind and model count, over an
 * Add OpenRouter repository button. It refreshes on `repositories_changed`, reconciling rows in place so the selection
 * survives. A user pick is only a request (OnSelectRequested): the owning service grants it with Select, or keeps the
 * current row (KeepSelection) when the editor has unsaved edits the user chose to keep.
 */
export class ModelsPanelViewModel extends Observable
{
    private static readonly PropSelected = 'Selected'
    private static readonly PropStatusText = 'StatusText'
    private static readonly RefreshFailedFormat = 'repositories unavailable: {0}'
    private static readonly RuntimeOffFormat = 'The runtime is off on the supervisor: {0}'

    public readonly Rows = new ObservableCollection<RepositoryRowViewModel>()
    /** The user picked a row; the owner decides (Select or KeepSelection). */
    public readonly OnSelectRequested = new TypedSignal<RepositoryRowViewModel>()
    /** A refresh was applied (a selected row that disappeared is already deselected). */
    public readonly OnRefreshed = new TypedSignal<void>()
    /** Add OpenRouter repository was asked for. */
    public readonly OnAddRequested = new TypedSignal<void>()

    private readonly addOpenRouterCommand: RelayCommand
    private readonly refreshCommand: RelayCommand
    private _selected: RepositoryRowViewModel | null = null
    private _statusText = ''
    private runtimeError = ''
    private client: RuntimeClient | null = null
    private subscriptions: IDisposable[] = []
    /** Orders refreshes so a slow early reply cannot overwrite a newer one. */
    private refreshToken = 0

    constructor()
    {
        super()
        this.addOpenRouterCommand = new RelayCommand(() => this.OnAddRequested.Raise(), () => this.IsBound)
        this.refreshCommand = new RelayCommand(() => void this.Refresh(), () => this.IsBound)
    }

    /** Follows the connection's `repositories_changed`, or goes inert with null (the rows stay as last seen). */
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
            this.subscriptions.push(client.OnRepositoriesChanged.Subscribe(() => void this.Refresh()))
        }
        this.addOpenRouterCommand.RaiseCanExecuteChanged()
        this.refreshCommand.RaiseCanExecuteChanged()
    }

    public get AddOpenRouterCommand(): ICommand
    {
        return this.addOpenRouterCommand
    }

    public get RefreshCommand(): ICommand
    {
        return this.refreshCommand
    }

    public get IsBound(): boolean
    {
        return this.client !== null
    }

    public get Selected(): RepositoryRowViewModel | null
    {
        return this._selected
    }

    /** A user selection is a request to the owner; clearing it keeps the current row (re-announced to the list). */
    public set Selected(row: RepositoryRowViewModel | null)
    {
        if (row === this._selected)
        {
            return
        }
        if (row === null || row === undefined)
        {
            this.KeepSelection()
            return
        }
        this.OnSelectRequested.Raise(row)
    }

    /** Makes the row the selection (a granted request, or the owner's own choice). */
    public Select(row: RepositoryRowViewModel | null): void
    {
        const old = this._selected
        if (old === row)
        {
            return
        }
        this._selected = row
        this.RaisePropertyChanged(ModelsPanelViewModel.PropSelected, old, row)
    }

    /** Re-announces the current selection after the binding write, so a refused pick snaps back in the list. */
    public KeepSelection(): void
    {
        void Promise.resolve().then(() =>
        {
            this.RaisePropertyChanged(ModelsPanelViewModel.PropSelected, undefined, this._selected)
        })
    }

    /** A refresh failure, or why the runtime is off; "" when all is well. */
    public get StatusText(): string
    {
        return this._statusText.length > 0 ? this._statusText : this.runtimeError
    }

    public SetRuntimeError(text: string): void
    {
        const old = this.StatusText
        this.runtimeError = text.length > 0 ? TextFormat.Format(ModelsPanelViewModel.RuntimeOffFormat, text) : ''
        this.RaisePropertyChanged(ModelsPanelViewModel.PropStatusText, old, this.StatusText)
    }

    public Find(name: string): RepositoryRowViewModel | undefined
    {
        for (let i = 0; i < this.Rows.Count; i++)
        {
            const row = this.Rows.Get(i)!
            if (row.Name === name)
            {
                return row
            }
        }
        return undefined
    }

    /** Every repository name listed (a new repository's default name avoids them). */
    public Names(): string[]
    {
        const names: string[] = []
        for (let i = 0; i < this.Rows.Count; i++)
        {
            names.push(this.Rows.Get(i)!.Name)
        }
        return names
    }

    /** Re-lists the repositories; a failure shows in StatusText and keeps the rows. */
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
            const listed = await client.ListRepositories()
            if (token !== this.refreshToken)
            {
                return
            }
            this.SetStatus('')
            this.Reconcile(listed)
            this.OnRefreshed.Raise()
        }
        catch (e)
        {
            if (token === this.refreshToken)
            {
                this.SetStatus(TextFormat.Format(ModelsPanelViewModel.RefreshFailedFormat, TextFormat.Message(e)))
            }
        }
    }

    /** Updates rows in place by name, drops the gone ones and adds the new ones, in the server's order. */
    private Reconcile(listed: RepositorySummary[]): void
    {
        const byName = new Map<string, RepositoryRowViewModel>()
        for (let i = 0; i < this.Rows.Count; i++)
        {
            const row = this.Rows.Get(i)!
            byName.set(row.Name, row)
        }
        const next: RepositoryRowViewModel[] = []
        for (const summary of listed)
        {
            const row = byName.get(summary.name)
            if (row !== undefined)
            {
                row.Update(summary)
                next.push(row)
            }
            else
            {
                next.push(new RepositoryRowViewModel(summary))
            }
        }
        const same = next.length === this.Rows.Count && next.every((row, i) => this.Rows.Get(i) === row)
        if (!same)
        {
            this.Rows.Batch(() =>
            {
                this.Rows.Clear()
                for (const row of next)
                {
                    this.Rows.Add(row)
                }
            })
            if (this._selected !== null && next.includes(this._selected))
            {
                // The list was rebuilt: show the kept row as selected again.
                this.KeepSelection()
            }
        }
        if (this._selected !== null && !next.includes(this._selected))
        {
            this.Select(null)
        }
    }

    private SetStatus(text: string): void
    {
        const old = this.StatusText
        if (this._statusText === text)
        {
            return
        }
        this._statusText = text
        this.RaisePropertyChanged(ModelsPanelViewModel.PropStatusText, old, this.StatusText)
    }
}
