// Instrument panel: sampling knobs, thinking toggle, context fill, raw prompt,
// per-token logprobs, tokenization. Merged app-global by app.mu.
import InstrumentPanelViewModel from "../InstrumentPanelViewModel.js"
import LogprobVM from "../LogprobVM.js"

resources InstrumentPanelResources {
    ItemsPanelTemplate x:key="VerticalStackPanel" {
        StackPanel [ Orientation = Vertical ]
    }

    DataTemplate [ DataType = InstrumentPanelViewModel ] {
        ScrollViewer [ HorizontalScrollEnabled = false ] {
            StackPanel [ Orientation = Vertical, Margin = (8,8,8,8) ] {
                TextBlock [ Text = "Model", Style = @BodySm, Foreground = @Fg2 ]
                TextBlock [ Text = $ModelName, TextWrapping = Wrap, Margin = (0,0,0,8) ]

                TextBlock [ Text = "Sampling (applies to the next turn)", Style = @BodySm, Foreground = @Fg2 ]
                DockPanel [ LastChildFill = true ] {
                    TextBlock [ DockPanel.Dock = Left, Text = "Temperature", Margin = (0,0,8,0), VerticalAlignment = Center ]
                    SliderSpinEdit [ Value = $Temperature, Minimum = 0, Maximum = 2, SmallChange = 0.05, DecimalPlaces = 2 ]
                }
                DockPanel [ LastChildFill = true ] {
                    TextBlock [ DockPanel.Dock = Left, Text = "Top P", Margin = (0,0,8,0), VerticalAlignment = Center ]
                    SliderSpinEdit [ Value = $TopP, Minimum = 0, Maximum = 1, SmallChange = 0.01, DecimalPlaces = 2 ]
                }
                DockPanel [ LastChildFill = true ] {
                    TextBlock [ DockPanel.Dock = Left, Text = "Top K", Margin = (0,0,8,0), VerticalAlignment = Center ]
                    SliderSpinEdit [ Value = $TopK, Minimum = 0, Maximum = 100, SmallChange = 1, DecimalPlaces = 0 ]
                }
                DockPanel [ LastChildFill = true ] {
                    TextBlock [ DockPanel.Dock = Left, Text = "Repeat penalty", Margin = (0,0,8,0), VerticalAlignment = Center ]
                    SliderSpinEdit [ Value = $RepeatPenalty, Minimum = 0.8, Maximum = 1.5, SmallChange = 0.01, DecimalPlaces = 2 ]
                }
                DockPanel [ LastChildFill = true ] {
                    TextBlock [ DockPanel.Dock = Left, Text = "Seed (-1 = random)", Margin = (0,0,8,0), VerticalAlignment = Center ]
                    SpinEdit [ Value = $Seed, Minimum = -1, Maximum = 2147483647, SmallChange = 1, DecimalPlaces = 0 ]
                }
                DockPanel [ LastChildFill = true ] {
                    TextBlock [ DockPanel.Dock = Left, Text = "Max tokens (-1 = unlimited)", Margin = (0,0,8,0), VerticalAlignment = Center ]
                    SpinEdit [ Value = $MaxTokens, Minimum = -1, Maximum = 1000000, SmallChange = 16, DecimalPlaces = 0 ]
                }
                DockPanel [ LastChildFill = true, Margin = (0,6,0,8) ] {
                    TextBlock [ DockPanel.Dock = Left, Text = "Thinking", Margin = (0,0,8,0), VerticalAlignment = Center ]
                    Checkbox [ IsChecked = $EnableThinking ]
                }

                TextBlock [ Text = "Context", Style = @BodySm, Foreground = @Fg2 ]
                TextBlock [ Text = $ContextLabel ]
                // Bar: a track Border with a fill Border whose Width follows $ContextBarWidth
                // (mural ships no ProgressBar).
                Border [ Fill = @Bg2, Stroke = Pen [ Brush = @Border ], CornerRadius = 4, Height = 8,
                         HorizontalAlignment = Left, Width = 300, Margin = (0,2,0,8), ClipToBounds = true ] {
                    Border [ Fill = @Fg2, CornerRadius = 4, HorizontalAlignment = Left, Width = $ContextBarWidth ]
                }
                TextBlock [ Text = $Warning, Foreground = @StateDanger, TextWrapping = Wrap,
                            Visibility = $HasWarning << ToVisibility, Margin = (0,0,0,8) ]

                TextBlock [ Text = "Raw prompt", Style = @BodySm, Foreground = @Fg2 ]
                Border [ Stroke = Pen [ Brush = @Border ], CornerRadius = 4, Padding = (6,4,6,4), Margin = (0,0,0,8), ClipToBounds = true ] {
                    ScrollViewer [ MaxHeight = 200, HorizontalScrollEnabled = false ] {
                        TextBlock [ Text = $Prompt, TextWrapping = Wrap, FontFamily = "Consolas" ]
                    }
                }

                TextBlock [ Text = "Logprobs (last reply)", Style = @BodySm, Foreground = @Fg2 ]
                Border [ Stroke = Pen [ Brush = @Border ], CornerRadius = 4, Padding = (6,4,6,4), Margin = (0,0,0,8), ClipToBounds = true ] {
                    ScrollViewer [ MaxHeight = 200, HorizontalScrollEnabled = false ] {
                        ItemsControl [ ItemsSource = $Logprobs, ItemsPanel = @VerticalStackPanel ]
                    }
                }

                TextBlock [ Text = "Tokenize", Style = @BodySm, Foreground = @Fg2 ]
                DockPanel [ LastChildFill = true ] {
                    Button [ DockPanel.Dock = Right, Command = $TokenizeCommand, Margin = (6,0,0,0) ] {
                        TextBlock [ Text = "Tokenize" ]
                    }
                    TextBox [ Text = $TokenizeInput, Placeholder = "Text to tokenize…" ]
                }
                TextBlock [ Text = $TokenizeResult, TextWrapping = Wrap, FontFamily = "Consolas", Margin = (0,4,0,0) ]
            }
        }
    }

    DataTemplate [ DataType = LogprobVM ] {
        DockPanel [ LastChildFill = false ] {
            TextBlock [ DockPanel.Dock = Right, Text = $LogprobDisplay, Foreground = @Fg2, FontFamily = "Consolas" ]
            TextBlock [ Text = $Token, FontFamily = "Consolas" ]
        }
    }
}
