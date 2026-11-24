// Template editor (host area): Save / Revert over the template's fields: name, system prompt, step budget, runtime
// and model (from the catalog, models filtered by the runtime), context size, context strategy and compact-at
// percent, thinking, sampling defaults (blank = none), preferred host and the tool checkboxes. A field that does not parse shows its error under it and disables
// Save; load and save errors show above the form, and a catalog failure in a banner that stays until a retry works.
// The form is read-only while the template loads. Merged app-global by app.mu.
import TemplateEditorViewModel from "../TemplateEditorViewModel.js"
import ToolChoice from "../ToolChoice.js"

resources TemplateEditorResources {
    ItemsPanelTemplate x:key="AgentToolsPanel" {
        StackPanel [ Orientation = Vertical ]
    }

    DataTemplate [ DataType = ToolChoice ] {
        DockPanel [ LastChildFill = true, Margin = (0,0,0,4) ] {
            Checkbox [ DockPanel.Dock = Left, IsChecked = $Enabled, VerticalAlignment = Center, Margin = (0,0,8,0) ]
            StackPanel [ Orientation = Vertical ] {
                StackPanel [ Orientation = Horizontal ] {
                    TextBlock [ Text = $Name, Foreground = @Fg1 ]
                    TextBlock [ Text = "writes", Style = @BodySm, Foreground = @StateDanger, Margin = (8,0,0,0),
                                VerticalAlignment = Center, Visibility = $Writes << ToVisibility ]
                }
                TextBlock [ Text = $Description, Style = @BodySm, Foreground = @Fg2, TextWrapping = Wrap ]
            }
        }
    }

    DataTemplate [ DataType = TemplateEditorViewModel ] {
        DockPanel [ LastChildFill = true ] {
            TextBlock [ DockPanel.Dock = Top, Text = $CatalogError, Foreground = @StateDanger, TextWrapping = Wrap,
                        Margin = (12,8,12,0), Visibility = $HasCatalogError << ToVisibility ]
            Grid {
                TextBlock [ Text = $EmptyText, Foreground = @Fg2, HorizontalAlignment = Center, VerticalAlignment = Center,
                            Visibility = $IsEmpty << ToVisibility ]
                DockPanel [ LastChildFill = true, Visibility = $HasTemplate << ToVisibility ] {
                    DockPanel [ DockPanel.Dock = Top, LastChildFill = true, Margin = (12,12,12,0) ] {
                        Button [ DockPanel.Dock = Right, Variant = Filled, Command = $SaveCommand, Margin = (6,0,0,0) ] {
                            TextBlock [ Text = "Save" ]
                        }
                        Button [ DockPanel.Dock = Right, Command = $RevertCommand, Margin = (6,0,0,0) ] {
                            TextBlock [ Text = "Revert" ]
                        }
                        TextBlock [ Text = $HeaderText, Foreground = @Fg1, VerticalAlignment = Center, TextWrapping = Wrap ]
                    }
                    TextBlock [ DockPanel.Dock = Top, Text = $ErrorText, Foreground = @StateDanger, TextWrapping = Wrap,
                                Margin = (12,8,12,0), Visibility = $HasErrorText << ToVisibility ]
                    ScrollViewer [ HorizontalScrollEnabled = false ] {
                        StackPanel [ Orientation = Vertical, Margin = (12,12,12,12), IsEnabled = $IsEditable ] {
                            TextBlock [ Text = "Name", Style = @BodySm, Foreground = @Fg2 ]
                            TextBox [ Text = $Name, HorizontalAlignment = Stretch ]
                            TextBlock [ Text = $NameError, Style = @BodySm, Foreground = @StateDanger, Visibility = $HasNameError << ToVisibility ]

                            TextBlock [ Text = "System prompt", Style = @BodySm, Foreground = @Fg2, Margin = (0,8,0,0) ]
                            TextBox [ Text = $SystemPrompt, AcceptsReturn = true, TextWrapping = Wrap, MinHeight = 120,
                                      HorizontalAlignment = Stretch, Placeholder = "No system prompt" ]

                            TextBlock [ Text = "Step budget", Style = @BodySm, Foreground = @Fg2, Margin = (0,8,0,0) ]
                            TextBox [ Text = $StepBudgetText, HorizontalAlignment = Stretch ]
                            TextBlock [ Text = $StepBudgetError, Style = @BodySm, Foreground = @StateDanger, Visibility = $HasStepBudgetError << ToVisibility ]

                            TextBlock [ Text = "Runtime", Style = @BodySm, Foreground = @Fg2, Margin = (0,8,0,0) ]
                            ComboBox [ ItemsSource = $RuntimeChoices, SelectedItem = $RuntimeChoice, HorizontalAlignment = Stretch ]

                            TextBlock [ Text = "Model", Style = @BodySm, Foreground = @Fg2, Margin = (0,8,0,0) ]
                            ComboBox [ ItemsSource = $ModelChoices, SelectedItem = $ModelChoice, HorizontalAlignment = Stretch ]

                            TextBlock [ Text = "Context size (tokens)", Style = @BodySm, Foreground = @Fg2, Margin = (0,8,0,0) ]
                            TextBox [ Text = $ContextSizeText, HorizontalAlignment = Stretch ]
                            TextBlock [ Text = $ContextSizeError, Style = @BodySm, Foreground = @StateDanger, Visibility = $HasContextSizeError << ToVisibility ]

                            TextBlock [ Text = "Context", Style = @BodySm, Foreground = @Fg2, Margin = (0,8,0,0) ]
                            ComboBox [ ItemsSource = $ContextChoices, SelectedItem = $ContextChoice, HorizontalAlignment = Stretch ]

                            TextBlock [ Text = "Compact at %", Style = @BodySm, Foreground = @Fg2, Margin = (0,8,0,0) ]
                            TextBox [ Text = $CompactAtText, HorizontalAlignment = Stretch ]
                            TextBlock [ Text = $CompactAtError, Style = @BodySm, Foreground = @StateDanger, Visibility = $HasCompactAtError << ToVisibility ]

                            TextBlock [ Text = "Workspace", Style = @BodySm, Foreground = @Fg2, Margin = (0,8,0,0) ]
                            ComboBox [ ItemsSource = $WorkspaceChoices, SelectedItem = $WorkspaceChoice, HorizontalAlignment = Stretch ]
                            TextBox [ Text = $WorkspaceName, IsEnabled = $IsWorkspaceNameEnabled, HorizontalAlignment = Stretch,
                                      Placeholder = "Workspace name", Margin = (0,4,0,0) ]
                            TextBlock [ Text = $WorkspaceNameError, Style = @BodySm, Foreground = @StateDanger, Visibility = $HasWorkspaceNameError << ToVisibility ]

                            DockPanel [ LastChildFill = true, Margin = (0,8,0,0) ] {
                                TextBlock [ DockPanel.Dock = Left, Text = "Thinking", Margin = (0,0,8,0), VerticalAlignment = Center ]
                                Checkbox [ IsChecked = $EnableThinking ]
                            }

                            TextBlock [ Text = "Temperature (blank = worker default)", Style = @BodySm, Foreground = @Fg2, Margin = (0,8,0,0) ]
                            TextBox [ Text = $TemperatureText, HorizontalAlignment = Stretch, Placeholder = "default" ]
                            TextBlock [ Text = $TemperatureError, Style = @BodySm, Foreground = @StateDanger, Visibility = $HasTemperatureError << ToVisibility ]

                            TextBlock [ Text = "Top P (blank = worker default)", Style = @BodySm, Foreground = @Fg2, Margin = (0,8,0,0) ]
                            TextBox [ Text = $TopPText, HorizontalAlignment = Stretch, Placeholder = "default" ]
                            TextBlock [ Text = $TopPError, Style = @BodySm, Foreground = @StateDanger, Visibility = $HasTopPError << ToVisibility ]

                            TextBlock [ Text = "Preferred host", Style = @BodySm, Foreground = @Fg2, Margin = (0,8,0,0) ]
                            ComboBox [ ItemsSource = $HostChoices, SelectedItem = $HostChoice, HorizontalAlignment = Stretch ]

                            TextBlock [ Text = "Tools", Style = @BodySm, Foreground = @Fg2, Margin = (0,12,0,4) ]
                            ItemsControl [ ItemsSource = $Tools, ItemsPanel = @AgentToolsPanel ]
                        }
                    }
                }
            }
        }
    }
}
