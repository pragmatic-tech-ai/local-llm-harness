// Sessions side panel: a New session button over the session list. Each row shows the title, the template name and
// a state dot (live = model loaded, idle = needs load, unavailable = no host, busy = a turn is running). Right-click
// gives Rename and Delete. Merged app-global by app.mu.
import SessionsPanelViewModel from "../SessionsPanelViewModel.js"
import SessionRowViewModel from "../SessionRowViewModel.js"

resources SessionsPanelResources {
    ContextMenu x:key="SessionContextMenu" {
        MenuItem [ Header = "Rename", Command = $RenameCommand ]
        MenuItem [ Header = "Delete", Command = $DeleteCommand ]
    }

    DataTemplate x:key="SessionRow" [ DataType = SessionRowViewModel ] {
        DockPanel [ LastChildFill = true, ContextMenuService.ContextMenu = @SessionContextMenu ] {
            Grid [ DockPanel.Dock = Left, VerticalAlignment = Center, Margin = (0,0,8,0) ] {
                Border [ Width = 8, Height = 8, CornerRadius = 4, Fill = @StateSuccess, Visibility = $IsLive << ToVisibility ]
                Border [ Width = 8, Height = 8, CornerRadius = 4, Fill = @Fg3, Visibility = $IsIdle << ToVisibility ]
                Border [ Width = 8, Height = 8, CornerRadius = 4, Fill = @StateDanger, Visibility = $IsUnavailable << ToVisibility ]
                Border [ Width = 8, Height = 8, CornerRadius = 4, Fill = @StateWarning, Visibility = $IsBusy << ToVisibility ]
            }
            StackPanel [ Orientation = Vertical ] {
                TextBlock [ Text = $Title, Foreground = @Fg1, TextWrapping = NoWrap, TextTrimming = CharacterEllipsis ]
                StackPanel [ Orientation = Horizontal ] {
                    TextBlock [ Text = $TemplateName, Style = @BodySm, Foreground = @Fg2, Margin = (0,0,6,0) ]
                    TextBlock [ Text = $StateText, Style = @BodySm, Foreground = @Fg3 ]
                }
            }
        }
    }

    DataTemplate [ DataType = SessionsPanelViewModel ] {
        DockPanel [ LastChildFill = true, Margin = (8,8,8,8) ] {
            Button [ DockPanel.Dock = Top, Variant = Tonal, Command = $NewSessionCommand,
                     HorizontalAlignment = Stretch, Margin = (0,0,0,8) ] {
                TextBlock [ Text = "New session", HorizontalAlignment = Center ]
            }
            TextBlock [ DockPanel.Dock = Top, Text = $StatusText, Style = @BodySm, Foreground = @Fg2, TextWrapping = Wrap ]
            TextBlock [ DockPanel.Dock = Top, Text = "No sessions yet.", Style = @BodySm, Foreground = @Fg2,
                        Visibility = $IsEmpty << ToVisibility ]
            ListBox [ ItemsSource = $Sessions, ItemTemplate = @SessionRow, SelectedItem = $SelectedSession ]
        }
    }
}
