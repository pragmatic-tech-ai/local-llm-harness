// Settings page + String row templates. Merged app-global by app.mu.
import SettingsPageViewModel from "../settings/SettingsPageViewModel.js"
import SettingsCategory from "../settings/SettingsPageViewModel.js"
import StringSettingRow from "../settings/SettingsPageViewModel.js"

resources SettingsResources {
    DataTemplate [ DataType = SettingsPageViewModel ] {
        ScrollViewer [ HorizontalScrollEnabled = false ] {
            ItemsControl [ ItemsSource = $Categories, ItemsPanel = @VerticalStackPanel ]
        }
    }

    DataTemplate [ DataType = SettingsCategory ] {
        StackPanel [ Orientation = Vertical, Margin = (0,0,0,28) ] {
            TextBlock [ Text = $Name, Foreground = @Fg1, Margin = (0,0,0,10) ]
            ItemsControl [ ItemsSource = $Rows, ItemsPanel = @VerticalStackPanel ]
        }
    }

    DataTemplate [ DataType = StringSettingRow ] {
        StackPanel [ Orientation = Vertical, Margin = (0,7,0,7) ] {
            TextBlock [ Text = $Label, Foreground = @Fg1 ]
            TextBox [ Text = $Value, Margin = (0,4,0,0) ]
        }
    }
}
