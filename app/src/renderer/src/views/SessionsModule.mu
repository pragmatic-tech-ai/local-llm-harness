import SessionsService from "../SessionsService.js"

shell module SessionsModule [ Name = "Sessions" ] {
    .services: {
        SessionsService
    }

    .settings: {
        SettingDefinition
            [ Key = "supervisor.url", Label = "Supervisor URL",
              Description = "WebSocket URL of the harness supervisor (e.g. ws://host:8778); the agent runtime is found through it.",
              Kind = String, Default = "", Category = "Runtime" ]
    }

    Capability [ Name = "Sessions", Icon = @RuntimeIcon, ServiceKey = SessionsService ]
}
