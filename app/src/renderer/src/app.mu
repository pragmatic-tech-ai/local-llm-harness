import ChatResources from "./views/Chat.mu.js"
import InstrumentPanelResources from "./views/InstrumentPanel.mu.js"
import HarnessPanelResources from "./views/HarnessPanel.mu.js"
import RuntimeIcons from "./views/RuntimeIcons.mu.js"
import RuntimeModule from "./views/RuntimeModule.mu.js"
import RuntimeManagementService from "./RuntimeManagementService.js"
import EditorShell from "@pragmatic-tech-ai/mural/framework/shell/editor-shell.js"
import Pragmatic from "@pragmatic-tech-ai/mural/resources/pragmatic"
import PragmaticDark from "@pragmatic-tech-ai/mural/resources/pragmatic"

Application [ Theme = Pragmatic, Scheme = PragmaticDark ] {
    .services: {
        ContentHostService
    }
    .modules: {
        RuntimeModule
    }
    resources: {
        merge RuntimeIcons
        merge ChatResources
        merge InstrumentPanelResources
        merge HarnessPanelResources
        DataTemplate [ DataType = RuntimeManagementService ] {
            ContentControl [ Content = $Picker ]
        }
        EditorShell x:root { }
    }
}
