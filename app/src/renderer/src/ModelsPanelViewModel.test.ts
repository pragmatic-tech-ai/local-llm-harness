import { describe, expect, it } from 'vitest'
import { ModelsPanelViewModel } from './ModelsPanelViewModel'
import { RuntimeClient } from './RuntimeClient'
import { FakeSocket } from './FakeSocket'
import { RepositoryKind, type RepositorySummary } from './agents'

class PanelRig
{
    public static readonly Caps = { measure_prompt: 'exact' as const, tokenize: true, raw_prompt: true, logprobs: true, load_unload: true }
    public static readonly Hive: RepositorySummary = { name: 'hive', kind: RepositoryKind.Pragmatic, editable: false, capabilities: PanelRig.Caps, model_count: 12 }
    public static readonly Router: RepositorySummary = {
        name: 'openrouter', kind: RepositoryKind.OpenRouter, editable: true, capabilities: PanelRig.Caps, model_count: 48,
        has_key: true, key_hint: '7f2a', id_prefixes: ['anthropic/'], fetched_at: 1700000000,
    }

    public readonly socket = new FakeSocket()
    public readonly client = new RuntimeClient(() => this.socket)
    public readonly panel = new ModelsPanelViewModel()

    constructor()
    {
        this.client.Connect('ws://runtime')
        this.socket.Open()
    }

    public static async Flush(): Promise<void>
    {
        await new Promise(r => setTimeout(r, 0))
    }

    public Methods(): string[]
    {
        return this.socket.sent.map(f => JSON.parse(f).method)
    }

    public async Respond(method: string, result: unknown): Promise<void>
    {
        const frames = this.socket.sent.map(f => JSON.parse(f)).filter(f => f.method === method)
        this.client.HandleFrame(JSON.stringify({ id: frames.pop()?.id ?? -1, result }))
        await PanelRig.Flush()
    }
}

describe('ModelsPanelViewModel', () =>
{
    it('panel_lists_repositories_with_counts', async () =>
    {
        const rig = new PanelRig()
        rig.panel.Bind(rig.client)
        const refreshed = rig.panel.Refresh()
        await rig.Respond('list_repositories', [PanelRig.Hive, PanelRig.Router])
        await refreshed
        expect(rig.panel.Rows.Count).toBe(2)
        expect(rig.panel.Rows.Get(0)!.Text).toBe('hive  Pragmatic  12 models')
        expect(rig.panel.Rows.Get(1)!.Text).toBe('openrouter  OpenRouter  48 models')
        expect(rig.panel.Find('openrouter')).toBe(rig.panel.Rows.Get(1))
        expect(rig.panel.AddOpenRouterCommand.CanExecute(undefined)).toBe(true)
    })

    it('repositories_changed_reloads_panel', async () =>
    {
        const rig = new PanelRig()
        rig.panel.Bind(rig.client)
        void rig.panel.Refresh()
        await rig.Respond('list_repositories', [PanelRig.Hive, PanelRig.Router])
        const routerRow = rig.panel.Rows.Get(1)!
        rig.panel.Select(routerRow)
        let refreshes = 0
        rig.panel.OnRefreshed.Subscribe(() => refreshes++)

        rig.client.HandleFrame(JSON.stringify({ method: 'repositories_changed', params: {} }))
        await PanelRig.Flush()
        expect(rig.Methods().filter(m => m === 'list_repositories').length).toBe(2)
        await rig.Respond('list_repositories', [PanelRig.Hive, { ...PanelRig.Router, model_count: 50 }])
        // Reconciled in place: the same row object, still selected, with its new count.
        expect(rig.panel.Rows.Get(1)).toBe(routerRow)
        expect(rig.panel.Selected).toBe(routerRow)
        expect(routerRow.Text).toBe('openrouter  OpenRouter  50 models')
        expect(refreshes).toBe(1)

        // A repository that disappears drops its row and the selection.
        rig.client.HandleFrame(JSON.stringify({ method: 'repositories_changed', params: {} }))
        await PanelRig.Flush()
        await rig.Respond('list_repositories', [PanelRig.Hive])
        expect(rig.panel.Rows.Count).toBe(1)
        expect(rig.panel.Selected).toBe(null)
    })

    it('a_user_pick_is_a_request_and_add_signals', async () =>
    {
        const rig = new PanelRig()
        rig.panel.Bind(rig.client)
        void rig.panel.Refresh()
        await rig.Respond('list_repositories', [PanelRig.Hive])
        const asked: string[] = []
        rig.panel.OnSelectRequested.Subscribe(row => asked.push(row.Name))
        let adds = 0
        rig.panel.OnAddRequested.Subscribe(() => adds++)
        rig.panel.Selected = rig.panel.Rows.Get(0)!
        expect(asked).toEqual(['hive'])
        expect(rig.panel.Selected).toBe(null)
        rig.panel.AddOpenRouterCommand.Execute(undefined)
        expect(adds).toBe(1)
    })

    it('a_failed_listing_shows_why', async () =>
    {
        const rig = new PanelRig()
        rig.panel.Bind(rig.client)
        void rig.panel.Refresh()
        const id = JSON.parse(rig.socket.sent[0]).id
        rig.client.HandleFrame(JSON.stringify({ id, error: { code: -32603, message: 'boom' } }))
        await PanelRig.Flush()
        expect(rig.panel.StatusText).toBe('repositories unavailable: boom')
    })
})
