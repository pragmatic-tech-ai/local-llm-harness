// New session dialog body (DialogService supplies the chrome, title and the Cancel / Create actions): the template
// tree beside a short summary of the selected template. Merged app-global by app.mu.
import NewSessionDialogViewModel from "../NewSessionDialogViewModel.js"
import TemplateTreeNodeViewModel from "../TemplateTreeNodeViewModel.js"

resources NewSessionDialogResources {
    HierarchicalDataTemplate x:key="TemplatePickNode" [ DataType = TemplateTreeNodeViewModel, itemsselector = Children ] {
        TextBlock [ Text = $Name, VerticalAlignment = Center ]
    }

    Style x:key="TemplatePickItemStyle" [ TargetType = TreeViewItem ] {
        IsExpanded = $IsExpanded;
    }

    DataTemplate [ DataType = NewSessionDialogViewModel ] {
        DockPanel [ LastChildFill = true, MinHeight = 240 ] {
            TextBlock [ DockPanel.Dock = Bottom, Text = $StatusText, Style = @BodySm, Foreground = @StateDanger, TextWrapping = Wrap ]
            Border [ DockPanel.Dock = Right, Width = 220, Fill = @Bg2, CornerRadius = 6, Padding = (8,8,8,8), Margin = (12,0,0,0) ] {
                TextBlock [ Text = $Summary, Style = @BodySm, Foreground = @Fg1, TextWrapping = Wrap ]
            }
            TreeView [ Indent = 14,
                       ItemsSource        = $Roots,
                       ItemTemplate       = @TemplatePickNode,
                       ItemContainerStyle = @TemplatePickItemStyle,
                       SelectedDataItem   = $SelectedNode,
                       SelectionMode      = Single ]
        }
    }
}
