import { Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { TypedSignal } from './JsonRpcConnection'
import type { RuntimeClient } from './RuntimeClient'
import { TemplateRepository, type AgentTemplate, type SessionSummary } from './agents'
import { TemplateTreeNodeViewModel } from './TemplateTreeNodeViewModel'
import { TextFormat } from './TextFormat'

/**
 * Body of the New session dialog: the template tree and a short summary of the selected template (model, repository,
 * tools, budget). Create makes the session and raises OnCreated; the owner closes the dialog and opens the session.
 */
export class NewSessionDialogViewModel extends Observable
{
    private static readonly PropSelectedNode = 'SelectedNode'
    private static readonly PropSummary = 'Summary'
    private static readonly PropStatusText = 'StatusText'
    private static readonly SummaryFormat = 'Model: {0} ({1}, context {2})\nRepository: {3}\nTools: {4}\nStep budget: {5}'
    private static readonly NoTools = 'none'
    private static readonly ToolsSeparator = ', '
    private static readonly GroupHint = 'Choose a template.'
    private static readonly LoadingText = 'Loading…'
    private static readonly BrokenFormat = 'This template cannot be used: {0}'
    private static readonly ListFailedFormat = 'templates unavailable: {0}'
    private static readonly ReadFailedFormat = 'template unavailable: {0}'
    private static readonly CreateFailedFormat = 'could not create the session: {0}'

    public readonly Roots = new ObservableCollection<TemplateTreeNodeViewModel>()
    public readonly OnCreated = new TypedSignal<SessionSummary>()

    private readonly client: RuntimeClient
    private readonly createCommand: RelayCommand
    private _selected: TemplateTreeNodeViewModel | null = null
    private _summary = NewSessionDialogViewModel.GroupHint
    private _statusText = ''
    private creating = false
    /** Orders template reads so a slow reply for an earlier selection is dropped. */
    private readToken = 0

    constructor(client: RuntimeClient)
    {
        super()
        this.client = client
        this.createCommand = new RelayCommand(() => void this.Create(), () => this.CanCreate)
    }

    public get CreateCommand(): ICommand
    {
        return this.createCommand
    }

    public get CanCreate(): boolean
    {
        const node = this._selected
        return !this.creating && node !== null && node.IsTemplate && !node.HasError
    }

    public get SelectedNode(): TemplateTreeNodeViewModel | null
    {
        return this._selected
    }

    public set SelectedNode(node: TemplateTreeNodeViewModel | null)
    {
        const value = node ?? null
        const old = this._selected
        if (old === value)
        {
            return
        }
        this._selected = value
        this.RaisePropertyChanged(NewSessionDialogViewModel.PropSelectedNode, old, value)
        this.createCommand.RaiseCanExecuteChanged()
        void this.ShowSummaryFor(value)
    }

    public get Summary(): string
    {
        return this._summary
    }

    public get StatusText(): string
    {
        return this._statusText
    }

    /** Fills the tree from `list_templates`. */
    public async Load(): Promise<void>
    {
        try
        {
            const nodes = await this.client.ListTemplates()
            this.Roots.Batch(() =>
            {
                this.Roots.Clear()
                for (const node of TemplateTreeNodeViewModel.FromNodes(nodes))
                {
                    this.Roots.Add(node)
                }
            })
        }
        catch (e)
        {
            this.SetStatus(TextFormat.Format(NewSessionDialogViewModel.ListFailedFormat, TextFormat.Message(e)))
        }
    }

    /** One summary for a template: its model, repository, tools and step budget. */
    public static Describe(template: AgentTemplate): string
    {
        const tools = template.tools.length > 0 ? template.tools.join(NewSessionDialogViewModel.ToolsSeparator) : NewSessionDialogViewModel.NoTools
        return TextFormat.Format(NewSessionDialogViewModel.SummaryFormat,
            template.model, template.runtime, template.n_ctx, TemplateRepository.Of(template), tools, template.step_budget)
    }

    private async ShowSummaryFor(node: TemplateTreeNodeViewModel | null): Promise<void>
    {
        const token = ++this.readToken
        const id = node?.TemplateId ?? null
        if (node === null || id === null)
        {
            this.SetSummary(NewSessionDialogViewModel.GroupHint)
            return
        }
        if (node.Error !== null)
        {
            this.SetSummary(TextFormat.Format(NewSessionDialogViewModel.BrokenFormat, node.Error))
            return
        }
        this.SetSummary(NewSessionDialogViewModel.LoadingText)
        try
        {
            const record = await this.client.GetTemplate(id)
            if (token === this.readToken)
            {
                this.SetSummary(NewSessionDialogViewModel.Describe(record.template))
            }
        }
        catch (e)
        {
            if (token === this.readToken)
            {
                this.SetSummary(TextFormat.Format(NewSessionDialogViewModel.ReadFailedFormat, TextFormat.Message(e)))
            }
        }
    }

    private async Create(): Promise<void>
    {
        const id = this._selected?.TemplateId ?? null
        if (id === null || !this.CanCreate)
        {
            return
        }
        this.SetCreating(true)
        this.SetStatus('')
        try
        {
            const summary = await this.client.CreateSession(id)
            this.OnCreated.Raise(summary)
        }
        catch (e)
        {
            this.SetStatus(TextFormat.Format(NewSessionDialogViewModel.CreateFailedFormat, TextFormat.Message(e)))
        }
        finally
        {
            this.SetCreating(false)
        }
    }

    private SetCreating(value: boolean): void
    {
        this.creating = value
        this.createCommand.RaiseCanExecuteChanged()
    }

    private SetSummary(text: string): void
    {
        const old = this._summary
        if (old !== text)
        {
            this._summary = text
            this.RaisePropertyChanged(NewSessionDialogViewModel.PropSummary, old, text)
        }
    }

    private SetStatus(text: string): void
    {
        const old = this._statusText
        if (old !== text)
        {
            this._statusText = text
            this.RaisePropertyChanged(NewSessionDialogViewModel.PropStatusText, old, text)
        }
    }
}
