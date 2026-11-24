// Models side panel: the repositories (Pragmatic first, then OpenRouter) with their kind and model count, over an
// Add OpenRouter repository button. A repository whose model list failed to fetch is marked. Merged app-global by
// app.mu.
import ModelsPanelViewModel from "../ModelsPanelViewModel.js"
import RepositoryRowViewModel from "../RepositoryRowViewModel.js"

resources ModelsPanelResources {
    DataTemplate x:key="RepositoryRow" [ DataType = RepositoryRowViewModel ] {
        StackPanel [ Orientation = Vertical ] {
            TextBlock [ Text = $Name, Foreground = @Fg1, TextWrapping = NoWrap, TextTrimming = CharacterEllipsis ]
            StackPanel [ Orientation = Horizontal ] {
                TextBlock [ Text = $DetailText, Style = @BodySm, Foreground = @Fg2 ]
                TextBlock [ Text = "fetch failed", Style = @BodySm, Foreground = @StateDanger, Margin = (6,0,0,0),
                            Visibility = $HasError << ToVisibility ]
            }
        }
    }

    DataTemplate [ DataType = ModelsPanelViewModel ] {
        DockPanel [ LastChildFill = true, Margin = (8,8,8,8) ] {
            WrapPanel [ DockPanel.Dock = Top, Orientation = Horizontal, Margin = (0,0,0,8) ] {
                Button [ Variant = Tonal, Command = $AddOpenRouterCommand, Margin = (0,0,6,6) ] {
                    TextBlock [ Text = "Add OpenRouter repository" ]
                }
                Button [ Variant = Text, Command = $RefreshCommand, Margin = (0,0,6,6) ] {
                    TextBlock [ Text = "Refresh" ]
                }
            }
            TextBlock [ DockPanel.Dock = Top, Text = $StatusText, Style = @BodySm, Foreground = @StateDanger, TextWrapping = Wrap ]
            ListBox [ ItemsSource = $Rows, ItemTemplate = @RepositoryRow, SelectedItem = $Selected ]
        }
    }
}
