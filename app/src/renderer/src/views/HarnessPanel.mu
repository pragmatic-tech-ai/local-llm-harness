// Harness picker: runtime + model selectors, Start/Stop, status. Merged app-global by app.mu.
import HarnessPickerViewModel from "../HarnessPickerViewModel.js"

resources HarnessPanelResources {
    DataTemplate [ DataType = HarnessPickerViewModel ] {
        StackPanel [ Orientation = Vertical, Margin = (12,8,12,0) ] {
            DockPanel [ LastChildFill = true, Margin = (0,0,0,4) ] {
                TextBlock [ DockPanel.Dock = Left, Text = "Runtime", Margin = (0,0,8,0), VerticalAlignment = Center ]
                ComboBox [ ItemsSource = $Runtimes, DisplayMemberPath = "name", SelectedItem = $SelectedRuntime ]
            }
            DockPanel [ LastChildFill = true, Margin = (0,0,0,4) ] {
                TextBlock [ DockPanel.Dock = Left, Text = "Model", Margin = (0,0,8,0), VerticalAlignment = Center ]
                ComboBox [ ItemsSource = $Models, DisplayMemberPath = "name", SelectedItem = $SelectedModel ]
            }
            DockPanel [ LastChildFill = true, Margin = (0,0,0,4) ] {
                TextBlock [ DockPanel.Dock = Left, Text = "Context", Margin = (0,0,8,0), VerticalAlignment = Center ]
                TextBox [ Text = $ContextSizeText, VerticalAlignment = Center ]
            }
            DockPanel [ LastChildFill = true, Margin = (0,0,0,4) ] {
                Button [ DockPanel.Dock = Left, Command = $StartCommand, Margin = (0,0,6,0) ] {
                    TextBlock [ Text = "Start" ]
                }
                Button [ DockPanel.Dock = Left, Command = $StopCommand, Margin = (0,0,12,0) ] {
                    TextBlock [ Text = "Stop" ]
                }
                TextBlock [ DockPanel.Dock = Left, Text = $State, Style = @BodySm, Foreground = @Fg2, FontFamily = "Consolas", VerticalAlignment = Center, Margin = (0,0,12,0) ]
                TextBlock [ Text = $StatusText, Foreground = @Fg2, VerticalAlignment = Center, TextWrapping = Wrap ]
            }
        }
    }
}
