// Chat view: streaming transcript above a composer. Merged app-global by app.mu.
import ChatViewModel from "../ChatViewModel.js"
import UserMessage from "../messages.js"
import AssistantMessage from "../messages.js"

resources ChatResources {
    DataTemplate [ DataType = ChatViewModel ] {
        DockPanel [ LastChildFill = true, Margin = (12,12,12,12) ] {
            resources: {
                // Enter-to-send. SubmitsOnEnter leaves a plain Return unhandled, so
                // KeyDown bubbles to this DockPanel; SubmitCommand sends only on Return.
                Style [ TargetType = DockPanel ] {
                    on KeyDown { InvokeCommand [ Command = $SubmitCommand ] }
                }
            }
            DockPanel [ DockPanel.Dock = Bottom, LastChildFill = true, Margin = (0,8,0,0) ] {
                Button [ DockPanel.Dock = Right, Command = $SendCommand, Margin = (6,0,0,0), VerticalAlignment = Bottom ] {
                    TextBlock [ Text = "Send" ]
                }
                TextBox [ Text = $Draft, AcceptsReturn = true, SubmitsOnEnter = true, TextWrapping = Wrap,
                          MaxHeight = 200, Placeholder = "Message the model…" ]
            }
            ScrollViewer [ HorizontalScrollEnabled = false, AutoScrollToEnd = true ] {
                ItemsControl [ ItemsSource = $Transcript, ItemsPanel = @VerticalStackPanel ]
            }
        }
    }

    ItemsPanelTemplate x:key="VerticalStackPanel" {
        StackPanel [ Orientation = Vertical ]
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
}
