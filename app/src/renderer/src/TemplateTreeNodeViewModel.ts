import { Observable, ObservableCollection } from '@pragmatic-tech-ai/mural/runtime'
import type { TemplateNode } from './agents'

/**
 * One row of a template tree (a group or a template) for a TreeView bound through a HierarchicalDataTemplate with
 * `itemsselector = Children`. Templates have no Children (undefined), so the tree draws them as leaves. `Reconcile`
 * makes a tree match a fresh `list_templates` reply in place, reusing nodes by `Key` so selection and expansion
 * survive a refresh (a moved template keeps its node object).
 */
export class TemplateTreeNodeViewModel extends Observable
{
    private static readonly KindGroup = 'group'
    private static readonly PropIsExpanded = 'IsExpanded'
    private static readonly GroupKeyPrefix = 'group:'
    private static readonly TemplateKeyPrefix = 'template:'
    private static readonly NodeProps = ['Node', 'Name', 'Path', 'Error', 'HasError']

    private node: TemplateNode
    private readonly children: ObservableCollection<TemplateTreeNodeViewModel> | undefined
    private _isExpanded = true

    constructor(node: TemplateNode)
    {
        super()
        this.node = node
        if (node.kind === TemplateTreeNodeViewModel.KindGroup)
        {
            this.children = new ObservableCollection<TemplateTreeNodeViewModel>()
            for (const child of TemplateTreeNodeViewModel.FromNodes(node.children))
            {
                this.children.Add(child)
            }
        }
    }

    /** View models for a `list_templates` tree, in server order. */
    public static FromNodes(nodes: readonly TemplateNode[]): TemplateTreeNodeViewModel[]
    {
        return nodes.map(node => new TemplateTreeNodeViewModel(node))
    }

    /** The wire node this row shows (replaced by Update). */
    public get Node(): TemplateNode
    {
        return this.node
    }

    /** Identity across refreshes: a group by its path, a template by its id. */
    public get Key(): string
    {
        return TemplateTreeNodeViewModel.KeyOf(this.node)
    }

    public static KeyOf(node: TemplateNode): string
    {
        return node.kind === TemplateTreeNodeViewModel.KindGroup
            ? TemplateTreeNodeViewModel.GroupKeyPrefix + node.path
            : TemplateTreeNodeViewModel.TemplateKeyPrefix + node.id
    }

    public get Name(): string
    {
        return this.Node.name
    }

    /** The group path (for a group, its own path; for a template, the group it lives in). */
    public get Path(): string
    {
        return this.Node.path
    }

    public get IsGroup(): boolean
    {
        return this.Node.kind === TemplateTreeNodeViewModel.KindGroup
    }

    public get IsTemplate(): boolean
    {
        return !this.IsGroup
    }

    /** The template id, or null for a group. */
    public get TemplateId(): string | null
    {
        return this.Node.kind === TemplateTreeNodeViewModel.KindGroup ? null : this.Node.id
    }

    /** A template file that failed to parse or validate carries its error; groups and good templates have null. */
    public get Error(): string | null
    {
        return this.Node.kind === TemplateTreeNodeViewModel.KindGroup ? null : this.Node.error
    }

    public get HasError(): boolean
    {
        return this.Error !== null
    }

    public get Children(): ObservableCollection<TemplateTreeNodeViewModel> | undefined
    {
        return this.children
    }

    public get IsExpanded(): boolean
    {
        return this._isExpanded
    }

    public set IsExpanded(value: boolean)
    {
        const old = this._isExpanded
        if (old !== value)
        {
            this._isExpanded = value
            this.RaisePropertyChanged(TemplateTreeNodeViewModel.PropIsExpanded, old, value)
        }
    }

    /** Depth-first search of a tree for the node of a template id. */
    public static FindTemplate(roots: Iterable<TemplateTreeNodeViewModel>, id: string): TemplateTreeNodeViewModel | undefined
    {
        for (const node of roots)
        {
            if (node.TemplateId === id)
            {
                return node
            }
            const found = node.children ? TemplateTreeNodeViewModel.FindTemplate(node.children, id) : undefined
            if (found !== undefined)
            {
                return found
            }
        }
        return undefined
    }

    /** Every node of a tree by Key (the pool Reconcile reuses nodes from). */
    public static Pool(roots: Iterable<TemplateTreeNodeViewModel>, into: Map<string, TemplateTreeNodeViewModel> = new Map()): Map<string, TemplateTreeNodeViewModel>
    {
        for (const node of roots)
        {
            into.set(node.Key, node)
            if (node.children)
            {
                TemplateTreeNodeViewModel.Pool(node.children, into)
            }
        }
        return into
    }

    /**
     * Makes `target` match `nodes` (their order) in place: a node whose Key is in the pool is reused (updated, and
     * moved here if it lived elsewhere), others are created, and rows no longer listed are removed. No reset is raised.
     */
    public static Reconcile(target: ObservableCollection<TemplateTreeNodeViewModel>, nodes: readonly TemplateNode[], pool: Map<string, TemplateTreeNodeViewModel>): void
    {
        const rows = nodes.map(node =>
        {
            const key = TemplateTreeNodeViewModel.KeyOf(node)
            const reused = pool.get(key)
            pool.delete(key)
            const row = reused ?? TemplateTreeNodeViewModel.Empty(node)
            row.Update(node, pool)
            return row
        })
        rows.forEach((row, index) =>
        {
            const at = target.IndexOf(row)
            if (at < 0)
            {
                target.Insert(index, row)
            }
            else if (at !== index)
            {
                target.Move(at, index)
            }
        })
        // Every kept row now sits in front, so what remains at the tail is gone (or moved elsewhere).
        while (target.Count > rows.length)
        {
            target.RemoveAt(target.Count - 1)
        }
    }

    /** Points the row at a newer wire node with the same Key (and reconciles a group's children). */
    public Update(node: TemplateNode, pool: Map<string, TemplateTreeNodeViewModel>): void
    {
        const old = this.node
        this.node = node
        if (old !== node)
        {
            for (const name of TemplateTreeNodeViewModel.NodeProps)
            {
                this.RaisePropertyChanged(name, undefined, (this as unknown as Record<string, unknown>)[name])
            }
        }
        if (this.children !== undefined && node.kind === TemplateTreeNodeViewModel.KindGroup)
        {
            TemplateTreeNodeViewModel.Reconcile(this.children, node.children, pool)
        }
    }

    /** A row for the node with no children yet (Update fills them from the pool). */
    private static Empty(node: TemplateNode): TemplateTreeNodeViewModel
    {
        return new TemplateTreeNodeViewModel(node.kind === TemplateTreeNodeViewModel.KindGroup ? { ...node, children: [] } : node)
    }
}
