import { ServiceBase, ServiceKey } from '@pragmatic-tech-ai/mural/runtime'
import type { IServiceProvider } from '@pragmatic-tech-ai/mural/runtime'
import { ContentHostService } from '@pragmatic-tech-ai/mural/framework/shell/services/content-host-service.js'
import { DialogService, NavigationDestination, NavigationService, type IActivatable } from '@pragmatic-tech-ai/mural/framework'
import type { IDisposable } from './JsonRpcConnection'
import type { RuntimeClient } from './RuntimeClient'
import { SessionsService } from './SessionsService'
import { ConnectionState } from './ConnectionState'
import { ModelsPanelViewModel } from './ModelsPanelViewModel'
import { RepositoryEditorViewModel } from './RepositoryEditorViewModel'
import type { RepositoryRowViewModel } from './RepositoryRowViewModel'
import { TextFormat } from './TextFormat'

/**
 * The Models capability: the repository list in the side pane (`Panel`) and the repository editor in the content host
 * (`Editor`, shown when the capability is activated or a repository is opened). It talks over the Sessions
 * capability's runtime connection and follows its connection state. Unsaved editor changes are guarded: picking
 * another repository, adding one, or leaving the rail asks Discard / Cancel first. Delete asks for confirmation.
 */
export class ModelsService extends ServiceBase implements IActivatable
{
    public static readonly Key = new ServiceKey<ModelsService>('ModelsService')

    private static readonly PropActiveService = 'ActiveService'
    private static readonly CancelLabel = 'Cancel'
    private static readonly DiscardTitle = 'Unsaved changes'
    private static readonly DiscardFormat = 'Discard the unsaved changes to "{0}"?'
    private static readonly DiscardConfirm = 'Discard'
    private static readonly DeleteTitle = 'Delete repository'
    private static readonly DeleteFormat = 'Delete the repository "{0}" and its stored key?'
    private static readonly DeleteConfirm = 'Delete'

    public readonly Panel = new ModelsPanelViewModel()
    public readonly Editor: RepositoryEditorViewModel

    private readonly client: RuntimeClient
    private readonly host: ContentHostService
    private readonly subscriptions: IDisposable[] = []
    /** The rail item that showed this capability while it is the active one (undefined otherwise). */
    private modelsItem: unknown = undefined
    /** A leave-the-rail confirm is open. */
    private leaving = false

    constructor(provider: IServiceProvider)
    {
        super(provider)
        const sessions = provider.getRequired(SessionsService.Key)
        this.client = sessions.Client
        this.host = provider.getRequired(ContentHostService.Key)
        this.Editor = new RepositoryEditorViewModel(this.client)
        this.subscriptions.push(
            sessions.OnConnectionChanged.Subscribe(state => this.ApplyConnection(state)),
            sessions.OnRuntimeErrorChanged.Subscribe(text => this.Panel.SetRuntimeError(text)),
            this.Panel.OnSelectRequested.Subscribe(row => void this.RequestSelect(row)),
            this.Panel.OnRefreshed.Subscribe(() => this.OnListRefreshed()),
            this.Panel.OnAddRequested.Subscribe(() => void this.Add()),
            this.Editor.OnDeleteRequested.Subscribe(name => void this.Delete(name)))
        const navigation = provider.get(NavigationService.Key)
        if (navigation !== undefined)
        {
            const active = navigation.PropertyChanged(ModelsService.PropActiveService)
                .subscribe(() => this.OnActiveServiceChanged(navigation))
            this.subscriptions.push({ Dispose: () => active.dispose() })
        }
        this.Panel.SetRuntimeError(sessions.RuntimeError)
        this.ApplyConnection(sessions.Connection)
    }

    /** The rail switched to Models: present the editor in the content host. */
    public OnActivated(): void
    {
        this.host.View(this.Editor)
    }

    private ApplyConnection(state: ConnectionState): void
    {
        if (state !== ConnectionState.Connected)
        {
            this.Panel.Bind(null)
            return
        }
        this.Panel.Bind(this.client)
        void this.Panel.Refresh()
    }

    private async RequestSelect(row: RepositoryRowViewModel): Promise<void>
    {
        if (row.Name === this.Editor.RepositoryName)
        {
            this.Panel.Select(row)
            return
        }
        if (!(await this.MayDiscardEdits()))
        {
            this.Panel.KeepSelection()
            return
        }
        this.Panel.Select(row)
        this.host.View(this.Editor)
        this.Editor.Open(row.Summary)
    }

    /**
     * After a refresh the open repository follows: a clean one takes its fresher state; one that is gone closes when
     * clean. A just-saved repository (a draft or a rename) is selected once it is listed.
     */
    private OnListRefreshed(): void
    {
        const name = this.Editor.RepositoryName
        if (name.length === 0)
        {
            return
        }
        const row = this.Panel.Find(name)
        if (row === undefined)
        {
            if (!this.Editor.IsDirty)
            {
                this.Editor.Clear()
            }
            return
        }
        this.Panel.Select(row)
        this.Editor.Reload(row.Summary)
    }

    private async Add(): Promise<void>
    {
        if (!(await this.MayDiscardEdits()))
        {
            return
        }
        this.Panel.Select(null)
        this.Editor.OpenNew(this.Panel.Names())
        this.host.View(this.Editor)
    }

    private async Delete(name: string): Promise<void>
    {
        const dialogs = this.Provider.get(DialogService.Key)
        if (dialogs !== undefined)
        {
            const confirmed = await dialogs.Confirm({
                Title: ModelsService.DeleteTitle,
                Message: TextFormat.Format(ModelsService.DeleteFormat, name),
                ConfirmLabel: ModelsService.DeleteConfirm,
                CancelLabel: ModelsService.CancelLabel,
            })
            if (!confirmed)
            {
                return
            }
        }
        // A refusal shows in the editor; repositories_changed then drops the row.
        if (await this.Editor.Delete())
        {
            this.Panel.Select(null)
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
            Title: ModelsService.DiscardTitle,
            Message: TextFormat.Format(ModelsService.DiscardFormat, this.Editor.Name),
            ConfirmLabel: ModelsService.DiscardConfirm,
            CancelLabel: ModelsService.CancelLabel,
        })
        if (discard)
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
        return discard
    }

    /**
     * Follows the rail. Arriving records the rail item; leaving with unsaved edits asks Discard / Cancel, and Cancel
     * switches the rail back to Models and re-activates its rail item (as the Agents capability does).
     */
    private OnActiveServiceChanged(navigation: NavigationService): void
    {
        if (navigation.ActiveService === this)
        {
            this.modelsItem = navigation.SelectedItem
            return
        }
        const back = this.modelsItem
        this.modelsItem = undefined
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
