import { Observable } from '@pragmatic-tech-ai/mural/runtime'

/** What a ToolChoice reports its toggles to (the template editor, which tracks dirty state). */
export interface IToolChoiceOwner
{
    ToolToggled(choice: ToolChoice): void
}

/** One tool checkbox in the template editor: a registry tool and whether the template enables it. */
export class ToolChoice extends Observable
{
    private static readonly PropEnabled = 'Enabled'

    public readonly Name: string
    public readonly Description: string
    private readonly owner: IToolChoiceOwner
    private _enabled: boolean

    constructor(owner: IToolChoiceOwner, name: string, description: string, enabled: boolean)
    {
        super()
        this.owner = owner
        this.Name = name
        this.Description = description
        this._enabled = enabled
    }

    public get Enabled(): boolean
    {
        return this._enabled
    }

    public set Enabled(value: boolean)
    {
        const old = this._enabled
        if (old !== value)
        {
            this._enabled = value
            this.RaisePropertyChanged(ToolChoice.PropEnabled, old, value)
            this.owner.ToolToggled(this)
        }
    }
}
