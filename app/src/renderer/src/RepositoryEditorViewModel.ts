import { Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'
import { TypedSignal } from './JsonRpcConnection'
import type { RuntimeClient } from './RuntimeClient'
import { RepositoryKind, type RepositoryModels, type RepositorySummary, type SaveRepositoryParams } from './agents'
import { RepositoryRowViewModel } from './RepositoryRowViewModel'
import { RepositoryModelRowViewModel } from './RepositoryModelRowViewModel'
import { TextFormat } from './TextFormat'

/**
 * The host-area repository editor. One stable instance: Open shows a listed repository, OpenNew an unsaved OpenRouter
 * draft, Clear the empty state. A Pragmatic repository is read-only: it lists its models with the runtimes able to
 * load them. An OpenRouter repository edits its name, its key (a password field; the stored key is never shown, only
 * its last 4 characters, and Save sends `api_key` only when a new one was typed) and its filter (comma separated id
 * prefixes), and offers Save, Delete and Refresh models over its model table (id, context, price in/out per million
 * tokens). Server errors (a refused save or delete, a failed fetch) show in ErrorText. Delete is only a request
 * (OnDeleteRequested): the service confirms, then calls Delete.
 *
 * HiddenTextRule: mural keeps showing a TextBlock's old render when its text changed while an ancestor was collapsed
 * and the same text is set again once shown. So every text inside a part that can collapse (the whole form when empty,
 * the OpenRouter part for a Pragmatic repository) reads "" while hidden and is never "" while shown (or the TextBlock
 * collapses itself when it is ""): a hidden-then-shown text always differs, so the binding pushes it.
 */
export class RepositoryEditorViewModel extends Observable
{
    private static readonly DefaultName = 'openrouter'
    private static readonly UniqueFormat = '{0}-{1}'
    private static readonly EmptyText = 'Select a repository, or add an OpenRouter one.'
    private static readonly NewHeader = 'New OpenRouter repository'
    private static readonly HeaderFormat = '{0}  {1}'
    private static readonly KeyMask = '••••'
    private static readonly KeyNotSet = 'not set'
    private static readonly NoKeyWarningText = 'no API key: models can be listed but not run'
    private static readonly FetchedFormat = 'fetched {0}:{1}'
    private static readonly NeverFetched = 'not fetched yet'
    private static readonly ModelsHeaderFormat = '{0} models'
    private static readonly NameRequired = 'Name is required.'
    private static readonly MeasureFormat = 'prompt tokens {0}'
    private static readonly TokenizeLabel = 'tokenize'
    private static readonly RawPromptLabel = 'raw prompt'
    private static readonly LogprobsLabel = 'logprobs'
    private static readonly LoadUnloadLabel = 'load/unload'
    /** How many trailing key characters the server's `key_hint` shows. */
    private static readonly KeyHintLength = 4
    private static readonly PrefixSeparator = ','
    private static readonly PrefixJoin = ', '
    private static readonly FingerprintSeparator = '\u0000'
    private static readonly ClockPad = '0'
    private static readonly ClockDigits = 2
    private static readonly MillisPerSecond = 1000
    private static readonly PropName = 'Name'
    private static readonly PropNewKey = 'NewKey'
    private static readonly PropFilterText = 'FilterText'
    private static readonly FieldProps = [RepositoryEditorViewModel.PropName, RepositoryEditorViewModel.PropNewKey, RepositoryEditorViewModel.PropFilterText]
    private static readonly DerivedProps = ['HasRepository', 'IsEmpty', 'IsNew', 'IsPragmatic', 'IsOpenRouter', 'IsEditable',
        'IsDirty', 'CanSave', 'HeaderText', 'KeyHint', 'HasNoKeyWarning', 'FetchedText', 'ErrorText', 'HasErrorText',
        'NameError', 'HasNameError', 'ModelsHeader', 'CapabilitiesText', 'HasCapabilities']

    public readonly Models = new ObservableCollection<RepositoryModelRowViewModel>()
    /** A save succeeded; carries the saved name. */
    public readonly OnSaved = new TypedSignal<string>()
    /** Delete was asked for; the owner confirms, then calls Delete. */
    public readonly OnDeleteRequested = new TypedSignal<string>()

    private readonly client: RuntimeClient
    private readonly saveCommand: RelayCommand
    private readonly revertCommand: RelayCommand
    private readonly deleteCommand: RelayCommand
    private readonly refreshCommand: RelayCommand
    private open = false
    private isNew = false
    private kind: RepositoryKind = RepositoryKind.OpenRouter
    /** The repository's listed state (null for a draft or when empty). */
    private summary: RepositorySummary | null = null
    /** The stored key's last characters ("" when no key is stored). */
    private keyHint = ''
    /** The name it is saved under (a rename sends it as `previous_name`). */
    private savedName = ''
    private savedPrefixes: string[] = []
    private name = ''
    private newKey = ''
    private filterText = ''
    private fetchedAt: number | null = null
    private errorText = ''
    private busy = false
    /** The form's fingerprint when filled or saved: anything else is an unsaved edit. */
    private baseline = ''
    /** Orders opens and model listings: a late reply for an earlier repository is dropped. */
    private loadToken = 0

    constructor(client: RuntimeClient)
    {
        super()
        this.client = client
        this.saveCommand = new RelayCommand(() => void this.Save(), () => this.CanSave)
        this.revertCommand = new RelayCommand(() => this.Revert(), () => this.IsEditable && this.IsDirty && !this.isNew)
        this.deleteCommand = new RelayCommand(() => this.OnDeleteRequested.Raise(this.savedName), () => this.IsEditable && !this.isNew)
        this.refreshCommand = new RelayCommand(() => void this.Refresh(), () => this.IsEditable && !this.isNew)
        this.baseline = this.Fingerprint()
    }

    public get SaveCommand(): ICommand
    {
        return this.saveCommand
    }

    public get RevertCommand(): ICommand
    {
        return this.revertCommand
    }

    public get DeleteCommand(): ICommand
    {
        return this.deleteCommand
    }

    public get RefreshCommand(): ICommand
    {
        return this.refreshCommand
    }

    public get HasRepository(): boolean
    {
        return this.open
    }

    public get IsEmpty(): boolean
    {
        return !this.open
    }

    public get EmptyText(): string
    {
        return RepositoryEditorViewModel.EmptyText
    }

    /** An unsaved draft from Add OpenRouter repository. */
    public get IsNew(): boolean
    {
        return this.open && this.isNew
    }

    public get IsPragmatic(): boolean
    {
        return this.open && this.kind === RepositoryKind.Pragmatic
    }

    public get IsOpenRouter(): boolean
    {
        return this.open && this.kind === RepositoryKind.OpenRouter
    }

    /** The form accepts edits: an OpenRouter repository, not mid save or delete. */
    public get IsEditable(): boolean
    {
        return this.IsOpenRouter && !this.busy
    }

    /** The name the repository is saved under ("" for a draft or when empty). */
    public get RepositoryName(): string
    {
        return this.isNew ? '' : this.savedName
    }

    public get HeaderText(): string
    {
        if (!this.open)
        {
            return ''
        }
        return this.isNew
            ? RepositoryEditorViewModel.NewHeader
            : TextFormat.Format(RepositoryEditorViewModel.HeaderFormat, this.savedName, RepositoryRowViewModel.KindLabel(this.kind))
    }

    public get HasCapabilities(): boolean
    {
        return this.CapabilitiesText.length > 0
    }

    /** What the provider can do, as a line under the header. */
    public get CapabilitiesText(): string
    {
        const caps = this.summary?.capabilities
        if (caps === undefined)
        {
            return ''
        }
        const flags: [boolean, string][] = [
            [caps.tokenize, RepositoryEditorViewModel.TokenizeLabel],
            [caps.raw_prompt, RepositoryEditorViewModel.RawPromptLabel],
            [caps.logprobs, RepositoryEditorViewModel.LogprobsLabel],
            [caps.load_unload, RepositoryEditorViewModel.LoadUnloadLabel],
        ]
        const shown = [TextFormat.Format(RepositoryEditorViewModel.MeasureFormat, caps.measure_prompt)]
        for (const [on, label] of flags)
        {
            if (on)
            {
                shown.push(label)
            }
        }
        return shown.join(RepositoryEditorViewModel.PrefixJoin)
    }

    /** Unsaved edits (a draft always counts). */
    public get IsDirty(): boolean
    {
        return this.IsOpenRouter && (this.isNew || this.Fingerprint() !== this.baseline)
    }

    public get CanSave(): boolean
    {
        return this.IsEditable && this.IsDirty && !this.HasNameError
    }

    public get Name(): string
    {
        return this.name
    }

    public set Name(value: string)
    {
        this.SetField(RepositoryEditorViewModel.PropName, this.name, value, () => { this.name = value })
    }

    public get NameError(): string
    {
        return this.IsOpenRouter && this.name.trim().length === 0 ? RepositoryEditorViewModel.NameRequired : ''
    }

    public get HasNameError(): boolean
    {
        return this.NameError.length > 0
    }

    /** The stored key's last 4 characters behind a mask, or `not set`. */
    public get KeyHint(): string
    {
        if (!this.IsOpenRouter)
        {
            // Hidden: "" (see HiddenTextRule), never a value the shown form can have.
            return ''
        }
        return this.keyHint.length > 0 ? RepositoryEditorViewModel.KeyMask + this.keyHint : RepositoryEditorViewModel.KeyNotSet
    }

    /** A new key (the password field); empty keeps the stored one. */
    public get NewKey(): string
    {
        return this.newKey
    }

    public set NewKey(value: string)
    {
        this.SetField(RepositoryEditorViewModel.PropNewKey, this.newKey, value, () => { this.newKey = value })
    }

    /** No key is stored and none is typed: the models can be listed but not run. */
    public get HasNoKeyWarning(): boolean
    {
        return this.IsOpenRouter && this.keyHint.length === 0 && this.newKey.trim().length === 0
    }

    public get NoKeyWarning(): string
    {
        return RepositoryEditorViewModel.NoKeyWarningText
    }

    /** The id prefixes, comma separated. */
    public get FilterText(): string
    {
        return this.filterText
    }

    public set FilterText(value: string)
    {
        this.SetField(RepositoryEditorViewModel.PropFilterText, this.filterText, value, () => { this.filterText = value })
    }

    /** FilterText split on commas, trimmed, blanks dropped. */
    public get Prefixes(): string[]
    {
        return this.filterText.split(RepositoryEditorViewModel.PrefixSeparator).map(p => p.trim()).filter(p => p.length > 0)
    }

    /** `fetched HH:MM` (local time) for an OpenRouter list; "" otherwise. */
    public get FetchedText(): string
    {
        if (!this.IsOpenRouter)
        {
            return ''
        }
        if (this.isNew || this.fetchedAt === null)
        {
            return RepositoryEditorViewModel.NeverFetched
        }
        const at = new Date(this.fetchedAt * RepositoryEditorViewModel.MillisPerSecond)
        return TextFormat.Format(RepositoryEditorViewModel.FetchedFormat, RepositoryEditorViewModel.Pad(at.getHours()), RepositoryEditorViewModel.Pad(at.getMinutes()))
    }

    public get ModelsHeader(): string
    {
        return this.open ? TextFormat.Format(RepositoryEditorViewModel.ModelsHeaderFormat, this.Models.Count) : ''
    }

    /** The last refused save or delete, or the model list's fetch error, verbatim; "" when none. */
    public get ErrorText(): string
    {
        return this.errorText
    }

    public get HasErrorText(): boolean
    {
        return this.errorText.length > 0
    }

    /** Shows a listed repository and lists its models (from the cache; Refresh refetches). */
    public Open(summary: RepositorySummary): void
    {
        this.open = true
        this.isNew = false
        this.errorText = summary.error ?? ''
        this.Models.Clear()
        this.Fill(summary)
        void this.LoadModels(false)
    }

    /** A clean open repository was listed again: takes its fresher state and re-lists its models. */
    public Reload(summary: RepositorySummary): void
    {
        if (!this.open || this.isNew || this.IsDirty || summary.name !== this.savedName)
        {
            return
        }
        if (summary.error !== undefined)
        {
            this.errorText = summary.error
        }
        this.Fill(summary)
        void this.LoadModels(false)
    }

    /** An unsaved OpenRouter draft, named `openrouter` (or `openrouter-2`, ... when taken). */
    public OpenNew(taken: string[]): void
    {
        this.loadToken++
        this.open = true
        this.isNew = true
        this.kind = RepositoryKind.OpenRouter
        this.summary = null
        this.keyHint = ''
        this.savedName = ''
        this.savedPrefixes = []
        this.name = RepositoryEditorViewModel.UniqueName(taken)
        this.newKey = ''
        this.filterText = ''
        this.fetchedAt = null
        this.errorText = ''
        this.Models.Clear()
        this.baseline = this.Fingerprint()
        this.RaiseAll()
    }

    /** Back to the empty state. */
    public Clear(): void
    {
        this.loadToken++
        this.open = false
        this.isNew = false
        this.summary = null
        this.keyHint = ''
        this.savedName = ''
        this.savedPrefixes = []
        this.name = ''
        this.newKey = ''
        this.filterText = ''
        this.fetchedAt = null
        this.errorText = ''
        this.Models.Clear()
        this.baseline = this.Fingerprint()
        this.RaiseAll()
    }

    /** Drops the unsaved edits (back to what was listed or last saved). */
    public Revert(): void
    {
        if (this.isNew)
        {
            return
        }
        this.name = this.savedName
        this.newKey = ''
        this.filterText = this.savedPrefixes.join(RepositoryEditorViewModel.PrefixJoin)
        this.baseline = this.Fingerprint()
        this.RaiseAll()
    }

    /** Sends the form to `save_repository`; resolves true when saved (a failed fetch after it still saved). */
    public async Save(): Promise<boolean>
    {
        if (!this.CanSave)
        {
            return false
        }
        const params: SaveRepositoryParams = { name: this.name.trim(), id_prefixes: this.Prefixes }
        if (!this.isNew)
        {
            params.previous_name = this.savedName
        }
        const key = this.newKey.trim()
        if (key.length > 0)
        {
            params.api_key = key
        }
        this.busy = true
        this.Changed()
        try
        {
            const saved = await this.client.SaveRepository(params)
            this.isNew = false
            this.savedName = saved.name
            this.name = saved.name
            this.savedPrefixes = params.id_prefixes
            if (key.length > 0)
            {
                // The stored key is now the typed one (repositories_changed then brings the server's hint).
                this.keyHint = key.slice(-RepositoryEditorViewModel.KeyHintLength)
            }
            if (this.newKey.trim() === key)
            {
                this.newKey = ''
            }
            this.baseline = this.Fingerprint()
            this.errorText = saved.fetch_error ?? ''
            this.OnSaved.Raise(saved.name)
            return true
        }
        catch (e)
        {
            this.errorText = TextFormat.Message(e)
            return false
        }
        finally
        {
            this.busy = false
            this.RaiseAll()
        }
    }

    /** Deletes the repository (after the owner confirmed). A refusal shows in ErrorText; success clears the editor. */
    public async Delete(): Promise<boolean>
    {
        if (!this.IsEditable || this.isNew)
        {
            return false
        }
        this.busy = true
        this.Changed()
        try
        {
            await this.client.DeleteRepository(this.savedName)
            this.Clear()
            return true
        }
        catch (e)
        {
            this.errorText = TextFormat.Message(e)
            return false
        }
        finally
        {
            this.busy = false
            this.Changed()
        }
    }

    /** Refetches the OpenRouter model list. */
    public async Refresh(): Promise<void>
    {
        if (!this.IsOpenRouter || this.isNew)
        {
            return
        }
        await this.LoadModels(true)
    }

    private static UniqueName(taken: string[]): string
    {
        const used = new Set(taken)
        let name = RepositoryEditorViewModel.DefaultName
        for (let n = 2; used.has(name); n++)
        {
            name = TextFormat.Format(RepositoryEditorViewModel.UniqueFormat, RepositoryEditorViewModel.DefaultName, n)
        }
        return name
    }

    private static Pad(value: number): string
    {
        return String(value).padStart(RepositoryEditorViewModel.ClockDigits, RepositoryEditorViewModel.ClockPad)
    }

    private Fill(summary: RepositorySummary): void
    {
        this.loadToken++
        this.kind = summary.kind
        this.summary = summary
        this.keyHint = summary.has_key === true ? summary.key_hint ?? '' : ''
        this.savedName = summary.name
        this.savedPrefixes = summary.id_prefixes ?? []
        this.name = summary.name
        this.newKey = ''
        this.filterText = this.savedPrefixes.join(RepositoryEditorViewModel.PrefixJoin)
        this.fetchedAt = summary.fetched_at ?? null
        this.baseline = this.Fingerprint()
        this.RaiseAll()
    }

    private async LoadModels(refresh: boolean): Promise<void>
    {
        const token = ++this.loadToken
        const name = this.savedName
        try
        {
            const listed = await this.client.ListRepositoryModels(name, refresh)
            if (token !== this.loadToken)
            {
                return
            }
            this.ApplyModels(listed, refresh)
        }
        catch (e)
        {
            if (token === this.loadToken)
            {
                this.errorText = TextFormat.Message(e)
                this.Changed()
            }
        }
    }

    private ApplyModels(listed: RepositoryModels, refresh: boolean): void
    {
        this.Models.Batch(() =>
        {
            this.Models.Clear()
            for (const model of listed.models)
            {
                this.Models.Add(RepositoryModelRowViewModel.From(model))
            }
        })
        this.fetchedAt = listed.fetched_at ?? null
        if (refresh || listed.error !== undefined)
        {
            this.errorText = listed.error ?? ''
        }
        this.Changed()
    }

    private Fingerprint(): string
    {
        return [this.name, this.newKey, this.Prefixes.join(RepositoryEditorViewModel.PrefixSeparator)].join(RepositoryEditorViewModel.FingerprintSeparator)
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

    /** Raises everything derived from the form and refreshes the commands. */
    private Changed(): void
    {
        this.RaiseNamed(RepositoryEditorViewModel.DerivedProps)
        this.saveCommand.RaiseCanExecuteChanged()
        this.revertCommand.RaiseCanExecuteChanged()
        this.deleteCommand.RaiseCanExecuteChanged()
        this.refreshCommand.RaiseCanExecuteChanged()
    }

    private RaiseAll(): void
    {
        this.RaiseNamed(RepositoryEditorViewModel.FieldProps)
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
