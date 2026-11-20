import RuntimeManagementService from "../RuntimeManagementService.js"

shell module RuntimeModule [ Name = "Runtime" ] {
    .services: {
        RuntimeManagementService
    }

    Capability [ Name = "Runtime", Icon = @RuntimeIcon, ServiceKey = RuntimeManagementService ]
}
