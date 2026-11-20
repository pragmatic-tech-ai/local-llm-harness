import RuntimeManagementService from "../RuntimeManagementService.js"

shell module RuntimeModule [ Name = "Runtime" ] {
    .services: {
        RuntimeManagementService
    }

    .settings: {
        SettingDefinition
            [ Key = "supervisor.url", Label = "Supervisor URL",
              Description = "WebSocket URL of the harness supervisor (e.g. ws://host:8778).",
              Kind = String, Default = "", Category = "Runtime" ]
    }

    Capability [ Name = "Runtime", Icon = @RuntimeIcon, ServiceKey = RuntimeManagementService ]
}
