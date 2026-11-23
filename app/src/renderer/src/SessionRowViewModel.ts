import { Observable, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { PlacementState, type Placement, type SessionSummary } from './agents'
import { PlacementText } from './PlacementText'

/** A session's live state as the list shows it: the placement, or busy while a turn runs. */
export enum SessionStateKind
{
    Live = 'live',
    Idle = 'idle',
    Unavailable = 'unavailable',
    Busy = 'busy',
}

/** What a row asks of the list that owns it (its right-click actions). */
export interface ISessionRowOwner
{
    RequestRename(row: SessionRowViewModel): void
    RequestDelete(row: SessionRowViewModel): void
}

/** One row of the Sessions panel: title, template name and the state dot. Updated in place so selection survives a refresh. */
export class SessionRowViewModel extends Observable
{
    private static readonly LiveText = 'live'
    private static readonly IdleText = 'idle'
    private static readonly BusyText = 'busy'
    private static readonly PropTitle = 'Title'
    private static readonly PropTemplateName = 'TemplateName'
    private static readonly PropStateText = 'StateText'
    private static readonly PropStateKind = 'StateKind'
    private static readonly PropBusy = 'Busy'
    private static readonly PropIsLive = 'IsLive'
    private static readonly PropIsIdle = 'IsIdle'
    private static readonly PropIsUnavailable = 'IsUnavailable'
    private static readonly PropIsBusy = 'IsBusy'

    public readonly Id: string
    public readonly RenameCommand: ICommand
    public readonly DeleteCommand: ICommand

    private summary: SessionSummary
    private _busy: boolean

    constructor(owner: ISessionRowOwner, summary: SessionSummary)
    {
        super()
        this.Id = summary.id
        this.summary = summary
        this._busy = summary.busy
        this.RenameCommand = new RelayCommand(() => owner.RequestRename(this))
        this.DeleteCommand = new RelayCommand(() => owner.RequestDelete(this))
    }

    public get Summary(): SessionSummary
    {
        return this.summary
    }

    public get Title(): string
    {
        return this.summary.title
    }

    public get TemplateName(): string
    {
        return this.summary.template_name
    }

    public get Placement(): Placement
    {
        return this.summary.placement
    }

    public get Busy(): boolean
    {
        return this._busy
    }

    public get StateKind(): SessionStateKind
    {
        if (this._busy)
        {
            return SessionStateKind.Busy
        }
        switch (this.summary.placement.state)
        {
            case PlacementState.Ready:
                return SessionStateKind.Live
            case PlacementState.NeedsLoad:
                return SessionStateKind.Idle
            default:
                return SessionStateKind.Unavailable
        }
    }

    public get StateText(): string
    {
        switch (this.StateKind)
        {
            case SessionStateKind.Busy:
                return SessionRowViewModel.BusyText
            case SessionStateKind.Live:
                return SessionRowViewModel.LiveText
            case SessionStateKind.Idle:
                return SessionRowViewModel.IdleText
            default:
                return PlacementText.Unavailable(this.summary.placement)
        }
    }

    public get IsLive(): boolean
    {
        return this.StateKind === SessionStateKind.Live
    }

    public get IsIdle(): boolean
    {
        return this.StateKind === SessionStateKind.Idle
    }

    public get IsUnavailable(): boolean
    {
        return this.StateKind === SessionStateKind.Unavailable
    }

    public get IsBusy(): boolean
    {
        return this.StateKind === SessionStateKind.Busy
    }

    /** A fresh list row for this session (title, template, placement and busy all refresh). */
    public Update(summary: SessionSummary): void
    {
        const oldTitle = this.summary.title
        const oldTemplate = this.summary.template_name
        this.ChangeState(() =>
        {
            this.summary = summary
            this._busy = summary.busy
        })
        this.Notify(SessionRowViewModel.PropTitle, oldTitle, summary.title)
        this.Notify(SessionRowViewModel.PropTemplateName, oldTemplate, summary.template_name)
    }

    public SetPlacement(placement: Placement): void
    {
        this.ChangeState(() => { this.summary = { ...this.summary, placement } })
    }

    public SetBusy(busy: boolean): void
    {
        this.ChangeState(() => { this._busy = busy })
    }

    /** Applies a state mutation, then raises every derived state property that changed. */
    private ChangeState(mutate: () => void): void
    {
        const oldKind = this.StateKind
        const oldText = this.StateText
        const oldBusy = this._busy
        mutate()
        this.Notify(SessionRowViewModel.PropBusy, oldBusy, this._busy)
        this.Notify(SessionRowViewModel.PropStateText, oldText, this.StateText)
        if (oldKind === this.StateKind)
        {
            return
        }
        this.RaisePropertyChanged(SessionRowViewModel.PropStateKind, oldKind, this.StateKind)
        this.Notify(SessionRowViewModel.PropIsLive, oldKind === SessionStateKind.Live, this.IsLive)
        this.Notify(SessionRowViewModel.PropIsIdle, oldKind === SessionStateKind.Idle, this.IsIdle)
        this.Notify(SessionRowViewModel.PropIsUnavailable, oldKind === SessionStateKind.Unavailable, this.IsUnavailable)
        this.Notify(SessionRowViewModel.PropIsBusy, oldKind === SessionStateKind.Busy, this.IsBusy)
    }

    private Notify<T>(name: string, old: T, value: T): void
    {
        if (old !== value)
        {
            this.RaisePropertyChanged(name, old, value)
        }
    }
}
