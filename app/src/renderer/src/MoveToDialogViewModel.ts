import { Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { TypedSignal } from './JsonRpcConnection'
import type { ChoiceItem } from './ChoiceItem'

/**
 * Body of the Move to dialog: the template groups (the root first, labelled "(root)"), the template's current group
 * preselected. Move is enabled for another group and raises OnChosen with its path; the owner closes the dialog.
 */
export class MoveToDialogViewModel extends Observable
{
    private static readonly PropSelectedGroup = 'SelectedGroup'

    public readonly Groups = new ObservableCollection<ChoiceItem>()
    public readonly OnChosen = new TypedSignal<string>()

    private readonly currentPath: string
    private readonly moveCommand: RelayCommand
    private _selected: ChoiceItem | null

    constructor(groups: readonly ChoiceItem[], currentPath: string)
    {
        super()
        this.currentPath = currentPath
        for (const group of groups)
        {
            this.Groups.Add(group)
        }
        this._selected = groups.find(g => g.Value === currentPath) ?? null
        this.moveCommand = new RelayCommand(() => this.Choose(), () => this.CanMove)
    }

    public get MoveCommand(): ICommand
    {
        return this.moveCommand
    }

    public get CanMove(): boolean
    {
        return this._selected !== null && this._selected.Value !== this.currentPath
    }

    public get SelectedGroup(): ChoiceItem | null
    {
        return this._selected
    }

    /** A cleared selection (null) is ignored, so the list keeps a group selected. */
    public set SelectedGroup(group: ChoiceItem | null)
    {
        if (group === null || group === undefined || group === this._selected)
        {
            return
        }
        const old = this._selected
        this._selected = group
        this.RaisePropertyChanged(MoveToDialogViewModel.PropSelectedGroup, old, group)
        this.moveCommand.RaiseCanExecuteChanged()
    }

    private Choose(): void
    {
        const group = this._selected
        if (group !== null && this.CanMove)
        {
            this.OnChosen.Raise(group.Value)
        }
    }
}
