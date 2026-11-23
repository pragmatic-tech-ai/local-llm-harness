import AgentsService from "../AgentsService.js"

shell module AgentsModule [ Name = "Agents" ] {
    .services: {
        AgentsService
    }

    Capability [ Name = "Agents", Icon = @AgentsIcon, ServiceKey = AgentsService ]
}
