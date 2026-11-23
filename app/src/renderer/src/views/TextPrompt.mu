// One-line text prompt dialog body (rename, new name); DialogService supplies the chrome and actions. Merged
// app-global by app.mu.
import TextPromptViewModel from "../TextPromptViewModel.js"

resources TextPromptResources {
    DataTemplate [ DataType = TextPromptViewModel ] {
        StackPanel [ Orientation = Vertical, HorizontalAlignment = Stretch ] {
            TextBlock [ Text = $Label, Style = @BodySm, Foreground = @Fg2 ]
            TextBox [ Text = $Text, HorizontalAlignment = Stretch ]
        }
    }
}
