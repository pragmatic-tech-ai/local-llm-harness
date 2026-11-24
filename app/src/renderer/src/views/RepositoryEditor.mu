// Repository editor (host area). A Pragmatic repository is read-only: its capabilities and its models with the
// runtimes able to load them. An OpenRouter repository edits its name, its key (a password field; the stored key
// shows only as its last 4 characters, and a blank field keeps it) and its filter (comma separated id prefixes),
// with Save, Revert, Delete and Refresh models over the model table (id, context, price in/out per million tokens).
// Server errors show above the form; a repository without a key shows the no-key warning. Merged app-global by app.mu.
import RepositoryEditorViewModel from "../RepositoryEditorViewModel.js"
import RepositoryModelRowViewModel from "../RepositoryModelRowViewModel.js"

resources RepositoryEditorResources {
    DataTemplate [ DataType = RepositoryModelRowViewModel ] {
        DockPanel [ LastChildFill = true, Margin = (0,1,0,1) ] {
            TextBlock [ DockPanel.Dock = Right, Text = $PriceText, Style = @BodySm, Foreground = @Fg2, Margin = (12,0,0,0),
                        Visibility = $IsPriced << ToVisibility ]
            TextBlock [ DockPanel.Dock = Right, Text = $ContextText, Style = @BodySm, Foreground = @Fg2, Margin = (12,0,0,0),
                        Visibility = $IsPriced << ToVisibility ]
            TextBlock [ DockPanel.Dock = Right, Text = $RuntimesText, Style = @BodySm, Foreground = @Fg2, Margin = (12,0,0,0) ]
            TextBlock [ Text = $Id, Foreground = @Fg1, TextWrapping = NoWrap, TextTrimming = CharacterEllipsis ]
        }
    }

    DataTemplate [ DataType = RepositoryEditorViewModel ] {
        Grid {
            TextBlock [ Text = $EmptyText, Foreground = @Fg2, HorizontalAlignment = Center, VerticalAlignment = Center,
                        Visibility = $IsEmpty << ToVisibility ]
            DockPanel [ LastChildFill = true, Visibility = $HasRepository << ToVisibility ] {
                DockPanel [ DockPanel.Dock = Top, LastChildFill = true, Margin = (12,12,12,0) ] {
                    StackPanel [ DockPanel.Dock = Right, Orientation = Horizontal, Visibility = $IsOpenRouter << ToVisibility ] {
                        Button [ Command = $RevertCommand, Margin = (6,0,0,0) ] {
                            TextBlock [ Text = "Revert" ]
                        }
                        Button [ Variant = Text, Command = $DeleteCommand, Margin = (6,0,0,0) ] {
                            TextBlock [ Text = "Delete" ]
                        }
                        Button [ Variant = Filled, Command = $SaveCommand, Margin = (6,0,0,0) ] {
                            TextBlock [ Text = "Save" ]
                        }
                    }
                    StackPanel [ Orientation = Vertical ] {
                        TextBlock [ Text = $HeaderText, Foreground = @Fg1, TextWrapping = Wrap ]
                        TextBlock [ Text = $CapabilitiesText, Style = @BodySm, Foreground = @Fg3, TextWrapping = Wrap,
                                    Visibility = $HasCapabilities << ToVisibility ]
                    }
                }
                TextBlock [ DockPanel.Dock = Top, Text = $ErrorText, Foreground = @StateDanger, TextWrapping = Wrap,
                            Margin = (12,8,12,0), Visibility = $HasErrorText << ToVisibility ]
                TextBlock [ DockPanel.Dock = Top, Text = $NoKeyWarning, Foreground = @StateWarning, TextWrapping = Wrap,
                            Margin = (12,8,12,0), Visibility = $HasNoKeyWarning << ToVisibility ]
                StackPanel [ DockPanel.Dock = Top, Orientation = Vertical, Margin = (12,12,12,0), IsEnabled = $IsEditable,
                             Visibility = $IsOpenRouter << ToVisibility ] {
                    TextBlock [ Text = "Name", Style = @BodySm, Foreground = @Fg2 ]
                    TextBox [ Text = $Name, HorizontalAlignment = Stretch ]
                    TextBlock [ Text = $NameError, Style = @BodySm, Foreground = @StateDanger, Visibility = $HasNameError << ToVisibility ]

                    DockPanel [ LastChildFill = true, Margin = (0,8,0,0) ] {
                        TextBlock [ DockPanel.Dock = Left, Text = "API key", Style = @BodySm, Foreground = @Fg2, Margin = (0,0,8,0) ]
                        TextBlock [ Text = $KeyHint, Style = @BodySm, Foreground = @Fg1 ]
                    }
                    TextBox [ Text = $NewKey, IsPassword = true, HorizontalAlignment = Stretch,
                              Placeholder = "Change: paste a new key (blank keeps the stored one)" ]

                    TextBlock [ Text = "Filter (id prefixes, comma separated)", Style = @BodySm, Foreground = @Fg2, Margin = (0,8,0,0) ]
                    TextBox [ Text = $FilterText, HorizontalAlignment = Stretch, Placeholder = "e.g. anthropic/, google/ (blank lists every tool model)" ]

                    DockPanel [ LastChildFill = true, Margin = (0,8,0,0) ] {
                        Button [ DockPanel.Dock = Left, Variant = Tonal, Command = $RefreshCommand, Margin = (0,0,8,0) ] {
                            TextBlock [ Text = "Refresh models" ]
                        }
                        TextBlock [ Text = $FetchedText, Style = @BodySm, Foreground = @Fg2, VerticalAlignment = Center ]
                    }
                }
                TextBlock [ DockPanel.Dock = Top, Text = $ModelsHeader, Style = @BodySm, Foreground = @Fg2, Margin = (12,12,12,4) ]
                ScrollViewer [ HorizontalScrollEnabled = false ] {
                    ItemsControl [ ItemsSource = $Models, ItemsPanel = @VerticalStackPanel, Margin = (12,0,12,12) ]
                }
            }
        }
    }
}
