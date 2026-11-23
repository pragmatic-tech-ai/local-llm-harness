import { Observable, ObservableCollection } from '@pragmatic-tech-ai/mural/runtime'
import type { TemplateNode } from './agents'

/**
 * One row of a template tree (a group or a template) for a TreeView bound through a HierarchicalDataTemplate with
 * `itemsselector = Children`. Templates have no Children (undefined), so the tree draws them as leaves.
 */
export class TemplateTreeNodeViewModel extends Observable
{
    private static readonly KindGroup = 'group'
    private static readonly PropIsExpanded = 'IsExpanded'

    public readonly Node: TemplateNode
    private readonly children: ObservableCollection<TemplateTreeNodeViewModel> | undefined
    private _isExpanded = true

    constructor(node: TemplateNode)
    {
        super()
        this.Node = node
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
}
