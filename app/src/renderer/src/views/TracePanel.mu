// Agent trace (instrument panel, below the context bar): turns newest first, each a toggle header over its steps,
// a step's fill line + small fill bar, token/timing line, tool rows and error; reset dividers; "Load older…".
// Merged app-global by app.mu.
import TraceViewModel from "../TraceViewModel.js"
import TraceTurnViewModel from "../TraceViewModel.js"
import TraceStepViewModel from "../TraceViewModel.js"
import TraceToolViewModel from "../TraceViewModel.js"

resources TracePanelResources {
    ItemsPanelTemplate x:key="TraceStackPanel" {
        StackPanel [ Orientation = Vertical ]
    }

    DataTemplate [ DataType = TraceViewModel ] {
        StackPanel [ Orientation = Vertical, Margin = (0,0,0,8) ] {
            TextBlock [ Text = "Trace", Style = @BodySm, Foreground = @Fg2 ]
            TextBlock [ Text = $WarningText, Foreground = @StateDanger, TextWrapping = Wrap,
                        Visibility = $HasWarning << ToVisibility ]
            Border [ Stroke = Pen [ Brush = @Border ], CornerRadius = 4, Padding = (6,4,6,4), ClipToBounds = true ] {
                // The trace scrolls on its own, inside the panel's scroll.
                ScrollViewer [ MaxHeight = 320, HorizontalScrollEnabled = false ] {
                    StackPanel [ Orientation = Vertical ] {
                        TextBlock [ Text = "── reset ──", Foreground = @Fg2, FontFamily = "Consolas",
                                    Visibility = $ResetPending << ToVisibility ]
                        ItemsControl [ ItemsSource = $Turns, ItemsPanel = @TraceStackPanel ]
                        Button [ Variant = Text, Command = $LoadOlderCommand, HorizontalAlignment = Left,
                                 Visibility = $HasMore << ToVisibility ] {
                            TextBlock [ Text = "Load older…" ]
                        }
                    }
                }
            }
        }
    }

    // A turn: the header toggles its steps; a reset before it is drawn below (the list is newest first).
    DataTemplate [ DataType = TraceTurnViewModel ] {
        StackPanel [ Orientation = Vertical ] {
            ToggleButton [ Variant = Text, IsChecked = $Expanded, HorizontalAlignment = Stretch ] {
                StackPanel [ Orientation = Horizontal ] {
                    TextBlock [ Text = $Glyph, FontFamily = "Consolas", Margin = (0,0,6,0) ]
                    TextBlock [ Text = $HeaderText, FontFamily = "Consolas", TextWrapping = Wrap ]
                }
            }
            ItemsControl [ ItemsSource = $Steps, ItemsPanel = @TraceStackPanel, Margin = (16,0,0,4),
                           Visibility = $Expanded << ToVisibility ]
            TextBlock [ Text = "── reset ──", Foreground = @Fg2, FontFamily = "Consolas",
                        Visibility = $ResetBefore << ToVisibility ]
        }
    }

    // A tool sub-row of a step.
    DataTemplate [ DataType = TraceToolViewModel ] {
        StackPanel [ Orientation = Horizontal ] {
            TextBlock [ Text = $Text, Style = @BodySm, FontFamily = "Consolas" ]
            TextBlock [ Text = $RepeatText, Style = @BodySm, Foreground = @StateDanger, FontFamily = "Consolas",
                        Margin = (8,0,0,0), Visibility = $HasRepeat << ToVisibility ]
        }
    }

    // A step: fill text + bar (the context-bar style, smaller), then out/think/load/ttft/gen/rate, the compaction line, tool rows, error.
    DataTemplate [ DataType = TraceStepViewModel ] {
        StackPanel [ Orientation = Vertical, Margin = (0,2,0,2) ] {
            StackPanel [ Orientation = Horizontal ] {
                TextBlock [ Text = $Text, FontFamily = "Consolas", Margin = (0,0,8,0), VerticalAlignment = Center ]
                Border [ Fill = @Bg2, Stroke = Pen [ Brush = @Border ], CornerRadius = 3, Height = 6,
                         HorizontalAlignment = Left, VerticalAlignment = Center, Width = $BarTrackWidth, ClipToBounds = true ] {
                    Border [ Fill = @Fg2, CornerRadius = 3, HorizontalAlignment = Left, Width = $BarWidth ]
                }
            }
            // No wrapping: the Wrap path re-joins words with single spaces, losing the double-space separators.
            TextBlock [ Text = $StatsText, Style = @BodySm, Foreground = @Fg2, FontFamily = "Consolas" ]
            TextBlock [ Text = $CompactionText, Style = @BodySm, Foreground = @Fg2, FontFamily = "Consolas",
                        Visibility = $HasCompaction << ToVisibility ]
            ItemsControl [ ItemsSource = $Tools, ItemsPanel = @TraceStackPanel, Margin = (12,0,0,0),
                           Visibility = $HasTools << ToVisibility ]
            TextBlock [ Text = $ErrorText, Style = @BodySm, Foreground = @StateDanger, TextWrapping = Wrap,
                        Visibility = $HasError << ToVisibility ]
        }
    }
}
