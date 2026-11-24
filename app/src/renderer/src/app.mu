import ChatResources from "./views/Chat.mu.js"
import InstrumentPanelResources from "./views/InstrumentPanel.mu.js"
import TracePanelResources from "./views/TracePanel.mu.js"
import SessionsPanelResources from "./views/SessionsPanel.mu.js"
import SessionViewResources from "./views/SessionView.mu.js"
import NewSessionDialogResources from "./views/NewSessionDialog.mu.js"
import TextPromptResources from "./views/TextPrompt.mu.js"
import AgentsPanelResources from "./views/AgentsPanel.mu.js"
import TemplateEditorResources from "./views/TemplateEditor.mu.js"
import RuntimeIcons from "./views/RuntimeIcons.mu.js"
import SessionsModule from "./views/SessionsModule.mu.js"
import SessionsService from "./SessionsService.js"
import AgentsModule from "./views/AgentsModule.mu.js"
import AgentsService from "./AgentsService.js"
import HarnessSettingsStore from "./HarnessSettingsStore.js"
import SettingsResources from "./views/SettingsResources.mu.js"
import HarnessSettingsContribution from "./settings/HarnessSettingsContribution.js"
import ApplicationSettings from "@pragmatic-tech-ai/mural/framework"
import SettingsStoreKey from "@pragmatic-tech-ai/mural/framework"
import SettingsContributionKey from "@pragmatic-tech-ai/mural/framework"
import EditorShell from "@pragmatic-tech-ai/mural/framework/shell/editor-shell.js"
import Pragmatic from "@pragmatic-tech-ai/mural/resources/pragmatic"
import PragmaticDark from "@pragmatic-tech-ai/mural/resources/pragmatic"

Application [ Theme = Pragmatic, Scheme = PragmaticDark ] {
    .services: {
        ContentHostService
        HarnessSettingsStore -> SettingsStoreKey
        ApplicationSettings
        HarnessSettingsContribution -> SettingsContributionKey
    }
    .modules: {
        SessionsModule
        AgentsModule
    }
    resources: {
        merge RuntimeIcons
        merge SettingsResources
        merge ChatResources
        merge InstrumentPanelResources
        merge TracePanelResources
        merge SessionsPanelResources
        merge SessionViewResources
        merge NewSessionDialogResources
        merge TextPromptResources
        merge AgentsPanelResources
        merge TemplateEditorResources
        DataTemplate [ DataType = SessionsService ] {
            ContentPresenter [ Content = $Panel ]
        }
        DataTemplate [ DataType = AgentsService ] {
            ContentPresenter [ Content = $Panel ]
        }
        EditorShell x:root { }
    }
}
