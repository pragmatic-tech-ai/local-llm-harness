import { RelayCommand, ServiceBase, ServiceKey } from '@pragmatic-tech-ai/mural/runtime'
import type { IServiceProvider } from '@pragmatic-tech-ai/mural/runtime'
import { ContentHostService } from '@pragmatic-tech-ai/mural/framework/shell/services/content-host-service.js'
import { ButtonVariant, DialogAction, DialogService, NavigationDestination, NavigationService, type IActivatable } from '@pragmatic-tech-ai/mural/framework'
import type { IDisposable } from './JsonRpcConnection'
import type { RuntimeClient } from './RuntimeClient'
import { SessionsService } from './SessionsService'
import { ConnectionState } from './ConnectionState'
import { AgentsTreeViewModel } from './AgentsTreeViewModel'
import { TemplateEditorViewModel } from './TemplateEditorViewModel'
import type { TemplateTreeNodeViewModel } from './TemplateTreeNodeViewModel'
import { MoveToDialogViewModel } from './MoveToDialogViewModel'
import { TextPromptViewModel } from './TextPromptViewModel'
import { TextFormat } from './TextFormat'

/**
 * The Agents capability: the template tree in the side pane (`Panel`) and the template editor in the content host
 * (`Editor`, shown when the capability is activated or a template is opened). It talks over the Sessions capability's
 * runtime connection (no second socket) and follows its connection state; when the supervisor says the runtime is
 * off, the tree's status line says why. The dialogs behind the tree's commands
 * live here. Unsaved editor changes are guarded: picking another template, starting a new one, or leaving the rail
 * asks Discard / Cancel first.
 */
export class AgentsService extends ServiceBase implements IActivatable
{
    public static readonly Key = new ServiceKey<AgentsService>('AgentsService')

    private static readonly PropActiveService = 'ActiveService'
    private static readonly PathSeparator = '/'
    private static readonly CancelLabel = 'Cancel'
    private static readonly DiscardTitle = 'Unsaved changes'
    private static readonly DiscardFormat = 'Discard the unsaved changes to "{0}"?'
    private static readonly DiscardConfirm = 'Discard'
    private static readonly NewGroupTitle = 'New group'
    private static readonly NewGroupName = 'New group'
    private static readonly NameLabel = 'Name'
    private static readonly CreateLabel = 'Create'
    private static readonly RenameGroupTitle = 'Rename group'
    private static readonly RenameTemplateTitle = 'Rename template'
    private static readonly RenameConfirm = 'Rename'
    private static readonly RenameDirtyText = 'Save or revert the open template first.'
    private static readonly DeleteGroupTitle = 'Delete group'
    private static readonly DeleteGroupFormat = 'Delete the group "{0}"? Only an empty group can be deleted.'
    private static readonly DeleteTemplateTitle = 'Delete template'
    private static readonly DeleteTemplateFormat = 'Delete "{0}"? Sessions started from it keep running.'
    private static readonly DeleteConfirm = 'Delete'
    private static readonly MoveTitle = 'Move template'
    private static readonly MoveLabel = 'Move'
    private static readonly MoveWidth = 360
    private static readonly CreateGroupFailedFormat = 'could not create the group: {0}'
    private static readonly RenameFailedFormat = 'rename failed: {0}'
    private static readonly DeleteFailedFormat = 'delete failed: {0}'
    private static readonly MoveFailedFormat = 'move failed: {0}'

    public readonly Panel = new AgentsTreeViewModel()
    public readonly Editor: TemplateEditorViewModel

    private readonly client: RuntimeClient
    private readonly host: ContentHostService
    private readonly subscriptions: IDisposable[] = []
    /** The rail item that showed this capability while it is the active one (undefined otherwise). */
    private agentsItem: unknown = undefined
    /** A leave-the-rail confirm is open. */
    private leaving = false

    constructor(provider: IServiceProvider)
    {
        super(provider)
        const sessions = provider.getRequired(SessionsService.Key)
        this.client = sessions.Client
        this.host = provider.getRequired(ContentHostService.Key)
        this.Editor = new TemplateEditorViewModel(this.client)
        this.subscriptions.push(
            sessions.OnConnectionChanged.Subscribe(state => this.ApplyConnection(state)),
            sessions.OnRuntimeErrorChanged.Subscribe(text => this.Panel.SetRuntimeError(text)),
            this.Panel.OnSelectRequested.Subscribe(node => void this.RequestSelect(node)),
            this.Panel.OnRefreshed.Subscribe(() => this.OnTreeRefreshed()),
            this.Panel.OnNewTemplateRequested.Subscribe(path => void this.NewTemplate(path)),
            this.Panel.OnNewGroupRequested.Subscribe(path => void this.NewGroup(path)),
            this.Panel.OnRenameRequested.Subscribe(node => void this.Rename(node)),
            this.Panel.OnDeleteRequested.Subscribe(node => void this.Delete(node)),
            this.Panel.OnMoveRequested.Subscribe(node => void this.Move(node)),
            this.Editor.OnSaved.Subscribe(id => this.Panel.SelectWhenListed(id)))
        const navigation = provider.get(NavigationService.Key)
        if (navigation !== undefined)
        {
            const active = navigation.PropertyChanged(AgentsService.PropActiveService)
                .subscribe(() => this.OnActiveServiceChanged(navigation))
            this.subscriptions.push({ Dispose: () => active.dispose() })
        }
        this.Panel.SetRuntimeError(sessions.RuntimeError)
        this.ApplyConnection(sessions.Connection)
    }

    /** The rail switched to Agents: present the editor in the content host. */
    public OnActivated(): void
    {
        this.host.View(this.Editor)
    }

    private ApplyConnection(state: ConnectionState): void
    {
        if (state !== ConnectionState.Connected)
        {
            // The tree stays as last seen; its commands go inert until the connection is back.
            this.Panel.Bind(null)
            return
        }
        this.Panel.Bind(this.client)
        void this.Panel.Refresh()
        void this.Editor.LoadCatalog()
    }

    private async RequestSelect(node: TemplateTreeNodeViewModel): Promise<void>
    {
        if (node.IsGroup || (node.TemplateId === this.Editor.TemplateId && !this.Editor.IsNew))
        {
            this.Panel.Select(node)
            return
        }
        if (!(await this.MayDiscardEdits()))
        {
            this.Panel.KeepSelection()
            return
        }
        this.Panel.Select(node)
        this.host.View(this.Editor)
        void this.Editor.Open(node)
    }

    /**
     * After a refresh: retry an unknown catalog; the open template follows a move and reloads when clean. When it is
     * gone, a clean editor closes and a dirty one keeps its edits as an unsaved draft (Save recreates it).
     */
    private OnTreeRefreshed(): void
    {
        this.Editor.EnsureCatalog()
        const id = this.Editor.TemplateId
        if (id === null || this.Editor.IsNew)
        {
            return
        }
        const node = this.Panel.Find(id)
        if (node === undefined)
        {
            if (this.Editor.IsDirty)
            {
                this.Editor.Orphan()
            }
            else
            {
                this.Editor.Clear()
            }
            return
        }
        this.Editor.SetPath(node.Path)
        if (!this.Editor.IsDirty)
        {
            void this.Editor.Reload(node)
        }
    }

    private async NewTemplate(path: string): Promise<void>
    {
        if (!(await this.MayDiscardEdits()))
        {
            return
        }
        if (this.Panel.Selected?.IsTemplate === true)
        {
            this.Panel.Select(null)
        }
        this.Editor.OpenNew(path)
        this.host.View(this.Editor)
    }

    private async NewGroup(parent: string): Promise<void>
    {
        const dialogs = this.Provider.get(DialogService.Key)
        if (dialogs === undefined)
        {
            return
        }
        try
        {
            const name = await TextPromptViewModel.Ask(dialogs, {
                Title: AgentsService.NewGroupTitle,
                Label: AgentsService.NameLabel,
                Text: AgentsService.NewGroupName,
                ConfirmLabel: AgentsService.CreateLabel,
            })
            if (name !== undefined)
            {
                // The tree follows through templates_changed.
                await this.client.CreateGroup(parent.length > 0 ? parent + AgentsService.PathSeparator + name : name)
            }
        }
        catch (e)
        {
            this.Panel.ReportError(TextFormat.Format(AgentsService.CreateGroupFailedFormat, TextFormat.Message(e)))
        }
    }

    private async Rename(node: TemplateTreeNodeViewModel): Promise<void>
    {
        const dialogs = this.Provider.get(DialogService.Key)
        const id = node.TemplateId
        if (dialogs === undefined)
        {
            return
        }
        if (id !== null && id === this.Editor.TemplateId && this.Editor.IsDirty)
        {
            this.Panel.ReportError(AgentsService.RenameDirtyText)
            return
        }
        try
        {
            const name = await TextPromptViewModel.Ask(dialogs, {
                Title: id === null ? AgentsService.RenameGroupTitle : AgentsService.RenameTemplateTitle,
                Label: AgentsService.NameLabel,
                Text: node.Name,
                ConfirmLabel: AgentsService.RenameConfirm,
            })
            if (name === undefined || name === node.Name)
            {
                return
            }
            if (id === null)
            {
                await this.client.RenameGroup(node.Path, name)
                return
            }
            // A template's name is a field of its file (the id, its file stem, stays).
            const record = await this.client.GetTemplate(id)
            await this.client.SaveTemplate(id, record.path, { ...record.template, name })
        }
        catch (e)
        {
            this.Panel.ReportError(TextFormat.Format(AgentsService.RenameFailedFormat, TextFormat.Message(e)))
        }
    }

    private async Delete(node: TemplateTreeNodeViewModel): Promise<void>
    {
        const dialogs = this.Provider.get(DialogService.Key)
        const id = node.TemplateId
        if (dialogs === undefined)
        {
            return
        }
        // Deleting the open template with unsaved edits: those are discarded first (or the delete is cancelled).
        if (id !== null && id === this.Editor.TemplateId && !(await this.MayDiscardEdits()))
        {
            return
        }
        try
        {
            const confirmed = await dialogs.Confirm({
                Title: id === null ? AgentsService.DeleteGroupTitle : AgentsService.DeleteTemplateTitle,
                Message: TextFormat.Format(id === null ? AgentsService.DeleteGroupFormat : AgentsService.DeleteTemplateFormat, node.Name),
                ConfirmLabel: AgentsService.DeleteConfirm,
            })
            if (!confirmed)
            {
                return
            }
            // templates_changed then drops the node, and closes the editor if it showed the template.
            if (id === null)
            {
                await this.client.DeleteGroup(node.Path)
            }
            else
            {
                await this.client.DeleteTemplate(id)
            }
        }
        catch (e)
        {
            this.Panel.ReportError(TextFormat.Format(AgentsService.DeleteFailedFormat, TextFormat.Message(e)))
        }
    }

    private async Move(node: TemplateTreeNodeViewModel): Promise<void>
    {
        const dialogs = this.Provider.get(DialogService.Key)
        const id = node.TemplateId
        if (dialogs === undefined || id === null)
        {
            return
        }
        const body = new MoveToDialogViewModel(this.Panel.Groups(), node.Path)
        const chosen = body.OnChosen.Subscribe(path => dialogs.Close(path))
        try
        {
            const path = await dialogs.Show<string>({
                Title: AgentsService.MoveTitle,
                Content: body,
                Width: AgentsService.MoveWidth,
                Actions: [
                    new DialogAction(AgentsService.CancelLabel, new RelayCommand(() => dialogs.Close(undefined))),
                    new DialogAction(AgentsService.MoveLabel, body.MoveCommand, ButtonVariant.Filled),
                ],
            })
            if (typeof path === 'string')
            {
                // The tree (and the open editor's group) follow through templates_changed.
                await this.client.MoveTemplate(id, path)
            }
        }
        catch (e)
        {
            this.Panel.ReportError(TextFormat.Format(AgentsService.MoveFailedFormat, TextFormat.Message(e)))
        }
        finally
        {
            chosen.Dispose()
        }
    }

    /** True when there are no unsaved edits, or the user chose to discard them (which drops them). */
    private async MayDiscardEdits(): Promise<boolean>
    {
        if (!this.Editor.IsDirty)
        {
            return true
        }
        const dialogs = this.Provider.get(DialogService.Key)
        const discard = dialogs === undefined || await dialogs.Confirm({
            Title: AgentsService.DiscardTitle,
            Message: TextFormat.Format(AgentsService.DiscardFormat, this.Editor.Name),
            ConfirmLabel: AgentsService.DiscardConfirm,
            CancelLabel: AgentsService.CancelLabel,
        })
        if (discard)
        {
            this.DiscardEdits()
        }
        return discard
    }

    private DiscardEdits(): void
    {
        if (this.Editor.IsNew)
        {
            this.Editor.Clear()
        }
        else
        {
            this.Editor.Revert()
        }
    }

    /**
     * Follows the rail. Arriving records the rail item; leaving with unsaved edits asks Discard / Cancel, and Cancel
     * switches the rail back to Agents and re-activates its rail item, so NavigationService's click tracking points at
     * Agents again (otherwise the next click on the other item would count as a re-click and hide the side pane).
     */
    private OnActiveServiceChanged(navigation: NavigationService): void
    {
        if (navigation.ActiveService === this)
        {
            this.agentsItem = navigation.SelectedItem
            return
        }
        const back = this.agentsItem
        this.agentsItem = undefined
        if (back === undefined || this.leaving || !this.Editor.IsDirty)
        {
            return
        }
        void this.ConfirmLeave(navigation, back)
    }

    private async ConfirmLeave(navigation: NavigationService, back: unknown): Promise<void>
    {
        this.leaving = true
        try
        {
            if (!(await this.MayDiscardEdits()))
            {
                navigation.SelectedItem = back
                if (back instanceof NavigationDestination)
                {
                    back.ActivateCommand?.Execute(undefined)
                }
            }
        }
        finally
        {
            this.leaving = false
        }
    }

    public override dispose(): void
    {
        for (const subscription of this.subscriptions)
        {
            subscription.Dispose()
        }
        this.Panel.Bind(null)
        super.dispose()
    }
}
