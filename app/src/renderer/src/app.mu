import PlaceholderViewModel from "./PlaceholderViewModel.js"
import EditorShell from "@pragmatic-tech-ai/mural/framework/shell/editor-shell.js"
import Pragmatic from "@pragmatic-tech-ai/mural/resources/pragmatic"
import PragmaticDark from "@pragmatic-tech-ai/mural/resources/pragmatic"

Application [ Theme = Pragmatic, Scheme = PragmaticDark ] {
    .services: {
        ContentHostService
    }
    resources: {
        DataTemplate [ DataType = PlaceholderViewModel ] { TextBlock [ Text = $Message ] }
        EditorShell x:root { }
    }
}
