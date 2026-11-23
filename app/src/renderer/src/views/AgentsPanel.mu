// Agents side panel: a toolbar (new template, new group, rename, delete, move to, refresh) over the template tree.
// A broken template file is marked in the tree; the editor shows its error. Also the Move to dialog body (the group
// list). Merged app-global by app.mu.
import AgentsTreeViewModel from "../AgentsTreeViewModel.js"
import TemplateTreeNodeViewModel from "../TemplateTreeNodeViewModel.js"
import MoveToDialogViewModel from "../MoveToDialogViewModel.js"
import ChoiceItem from "../ChoiceItem.js"

resources AgentsPanelResources {
    HierarchicalDataTemplate x:key="AgentTreeNode" [ DataType = TemplateTreeNodeViewModel, itemsselector = Children ] {
        StackPanel [ Orientation = Horizontal, VerticalAlignment = Center ] {
            TextBlock [ Text = $Name, VerticalAlignment = Center ]
            TextBlock [ Text = "broken", Style = @BodySm, Foreground = @StateDanger, Margin = (6,0,0,0),
                        VerticalAlignment = Center, Visibility = $HasError << ToVisibility ]
        }
    }

    Style x:key="AgentTreeItemStyle" [ TargetType = TreeViewItem ] {
        IsExpanded = $IsExpanded;
    }

    DataTemplate x:key="AgentGroupChoice" [ DataType = ChoiceItem ] {
        TextBlock [ Text = $Label ]
    }

    DataTemplate [ DataType = AgentsTreeViewModel ] {
        DockPanel [ LastChildFill = true, Margin = (8,8,8,8) ] {
            WrapPanel [ DockPanel.Dock = Top, Orientation = Horizontal, Margin = (0,0,0,8) ] {
                Button [ Variant = Tonal, Command = $NewTemplateCommand, Margin = (0,0,6,6) ] {
                    TextBlock [ Text = "New template" ]
                }
                Button [ Variant = Text, Command = $NewGroupCommand, Margin = (0,0,6,6) ] {
                    TextBlock [ Text = "New group" ]
                }
                Button [ Variant = Text, Command = $RenameCommand, Margin = (0,0,6,6) ] {
                    TextBlock [ Text = "Rename" ]
                }
                Button [ Variant = Text, Command = $DeleteCommand, Margin = (0,0,6,6) ] {
                    TextBlock [ Text = "Delete" ]
                }
                Button [ Variant = Text, Command = $MoveToCommand, Margin = (0,0,6,6) ] {
                    TextBlock [ Text = "Move to…" ]
                }
                Button [ Variant = Text, Command = $RefreshCommand, Margin = (0,0,6,6) ] {
                    TextBlock [ Text = "Refresh" ]
                }
            }
            TextBlock [ DockPanel.Dock = Top, Text = $StatusText, Style = @BodySm, Foreground = @StateDanger, TextWrapping = Wrap ]
            TreeView [ Indent = 14,
                       ItemsSource        = $Roots,
                       ItemTemplate       = @AgentTreeNode,
                       ItemContainerStyle = @AgentTreeItemStyle,
                       SelectedDataItem   = $Selected,
                       SelectionMode      = Single ]
        }
    }

    DataTemplate [ DataType = MoveToDialogViewModel ] {
        StackPanel [ Orientation = Vertical, HorizontalAlignment = Stretch ] {
            TextBlock [ Text = "Move to group", Style = @BodySm, Foreground = @Fg2, Margin = (0,0,0,4) ]
            ListBox [ ItemsSource = $Groups, ItemTemplate = @AgentGroupChoice, SelectedItem = $SelectedGroup ]
        }
    }
}
