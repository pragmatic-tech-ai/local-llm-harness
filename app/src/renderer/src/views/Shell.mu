// Shell view: harness picker above the chat. Merged app-global by app.mu.
import ShellViewModel from "../ShellViewModel.js"

resources ShellResources {
    DataTemplate [ DataType = ShellViewModel ] {
        DockPanel [ LastChildFill = true ] {
            ContentControl [ DockPanel.Dock = Top, Content = $Picker ]
            ContentControl [ Content = $Chat ]
        }
    }
}
