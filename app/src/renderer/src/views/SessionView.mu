// Session view (host area): a header line (template · model @ host, placement, Load / Unload model, tools) over the
// existing chat + instrument panel; an empty state when no session is shown. Merged app-global by app.mu.
import SessionViewModel from "../SessionViewModel.js"

resources SessionViewResources {
    DataTemplate [ DataType = SessionViewModel ] {
        Grid {
            TextBlock [ Text = $EmptyText, Foreground = @Fg2, HorizontalAlignment = Center, VerticalAlignment = Center,
                        Visibility = $IsEmpty << ToVisibility ]
            DockPanel [ LastChildFill = true, Visibility = $HasSession << ToVisibility ] {
                DockPanel [ DockPanel.Dock = Top, LastChildFill = true, Margin = (12,12,12,0) ] {
                    Button [ DockPanel.Dock = Right, Command = $UnloadCommand, Margin = (6,0,0,0) ] {
                        TextBlock [ Text = "Unload model" ]
                    }
                    Button [ DockPanel.Dock = Right, Command = $LoadCommand, Margin = (6,0,0,0) ] {
                        TextBlock [ Text = $LoadLabel ]
                    }
                    StackPanel [ Orientation = Vertical, VerticalAlignment = Center ] {
                        TextBlock [ Text = $HeaderText, Foreground = @Fg1, TextWrapping = Wrap ]
                        StackPanel [ Orientation = Horizontal ] {
                            TextBlock [ Text = $PlacementText, Style = @BodySm, Foreground = @Fg2, Margin = (0,0,12,0) ]
                            TextBlock [ Text = $ToolsText, Style = @BodySm, Foreground = @Fg2 ]
                        }
                    }
                }
                StackPanel [ DockPanel.Dock = Top, Orientation = Vertical, Margin = (12,8,12,0),
                             Visibility = $HasNotes << ToVisibility ] {
                    ToggleButton [ Variant = Text, IsChecked = $IsNotesExpanded, HorizontalAlignment = Left ] {
                        TextBlock [ Text = "Notes", Style = @BodySm, Foreground = @Fg2 ]
                    }
                    ItemsControl [ ItemsSource = $Notes, Visibility = $IsNotesExpanded << ToVisibility ]
                }
                ContentControl [ Content = $Chat ]
            }
        }
    }
}
