// Chat view: streaming transcript above a composer. Merged app-global by app.mu.
import ChatViewModel from "../ChatViewModel.js"
import UserMessage from "../messages.js"
import AssistantMessage from "../messages.js"
import ToolCallMessage from "../messages.js"
import ToolResultMessage from "../messages.js"

resources ChatResources {
    DataTemplate [ DataType = ChatViewModel ] {
        DockPanel [ LastChildFill = true, Margin = (12,12,12,12) ] {
            // Instrument panel to the right of the transcript (Content region split, not the shell dock).
            ContentControl [ DockPanel.Dock = Right, Width = 360, Content = $Panel, Margin = (12,0,0,0) ]
            DockPanel [ DockPanel.Dock = Bottom, Style = @ComposerSubmitStyle, LastChildFill = true, Margin = (0,8,0,0) ] {
                Button [ DockPanel.Dock = Right, Command = $SendCommand, Margin = (6,0,0,0), VerticalAlignment = Bottom ] {
                    TextBlock [ Text = "Send" ]
                }
                TextBox [ Text = $Draft, AcceptsReturn = true, SubmitsOnEnter = true, TextWrapping = Wrap,
                          MaxHeight = 200, IsEnabled = $CanChat, Placeholder = $ComposerPlaceholder ]
            }
            ScrollViewer [ HorizontalScrollEnabled = false, AutoScrollToEnd = true ] {
                ItemsControl [ ItemsSource = $Transcript, ItemsPanel = @VerticalStackPanel ]
            }
        }
    }

    // Enter-to-send. SubmitsOnEnter leaves a plain Return unhandled, so KeyDown
    // bubbles from the TextBox to the composer DockPanel, which references this
    // style explicitly (not implicit) so no other DockPanel fires it.
    Style x:key="ComposerSubmitStyle" [ TargetType = DockPanel ] {
        on KeyDown { InvokeCommand [ Command = $SubmitCommand ] }
    }

    DataTemplate [ DataType = UserMessage ] {
        Border [ Stroke = Pen [ Brush = @Border ], CornerRadius = 8,
                 Padding = (10,6,10,6), Margin = (40,3,0,3) ] {
            TextBlock [ Text = $Text, TextWrapping = Wrap ]
        }
    }

    DataTemplate [ DataType = AssistantMessage ] {
        Border [ Padding = (10,6,10,6), Margin = (0,3,40,3) ] {
            TextBlock [ Text = $Text, TextWrapping = Wrap ]
        }
    }

    // Tool call: "→ name arguments", mono, in the assistant bubble style.
    DataTemplate [ DataType = ToolCallMessage ] {
        Border [ Padding = (10,6,10,6), Margin = (0,3,40,3) ] {
            DockPanel [ LastChildFill = true ] {
                TextBlock [ DockPanel.Dock = Left, Text = "→", FontFamily = "Consolas", Margin = (0,0,6,0) ]
                TextBlock [ DockPanel.Dock = Left, Text = $Name, FontFamily = "Consolas", Margin = (0,0,6,0) ]
                TextBlock [ Text = $Arguments, FontFamily = "Consolas", TextWrapping = Wrap ]
            }
        }
    }

    // Tool result: "← name status: content", dimmed.
    DataTemplate [ DataType = ToolResultMessage ] {
        Border [ Padding = (10,6,10,6), Margin = (0,3,40,3) ] {
            DockPanel [ LastChildFill = true ] {
                TextBlock [ DockPanel.Dock = Left, Text = "←", Foreground = @Fg2, FontFamily = "Consolas", Margin = (0,0,6,0) ]
                TextBlock [ DockPanel.Dock = Left, Text = $Name, Foreground = @Fg2, FontFamily = "Consolas", Margin = (0,0,6,0) ]
                TextBlock [ DockPanel.Dock = Left, Text = $StatusText, Foreground = @Fg2, FontFamily = "Consolas", Margin = (0,0,6,0) ]
                TextBlock [ Text = $Content, Foreground = @Fg2, FontFamily = "Consolas", TextWrapping = Wrap ]
            }
        }
    }
}
