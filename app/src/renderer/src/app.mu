import ChatResources from "./views/Chat.mu.js"
import InstrumentPanelResources from "./views/InstrumentPanel.mu.js"
import SessionsPanelResources from "./views/SessionsPanel.mu.js"
import SessionViewResources from "./views/SessionView.mu.js"
import NewSessionDialogResources from "./views/NewSessionDialog.mu.js"
import TextPromptResources from "./views/TextPrompt.mu.js"
import RuntimeIcons from "./views/RuntimeIcons.mu.js"
import SessionsModule from "./views/SessionsModule.mu.js"
import SessionsService from "./SessionsService.js"
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
    }
    resources: {
        merge RuntimeIcons
        merge SettingsResources
        merge ChatResources
        merge InstrumentPanelResources
        merge SessionsPanelResources
        merge SessionViewResources
        merge NewSessionDialogResources
        merge TextPromptResources
        DataTemplate [ DataType = SessionsService ] {
            ContentPresenter [ Content = $Panel ]
        }
        EditorShell x:root { }
    }
}
