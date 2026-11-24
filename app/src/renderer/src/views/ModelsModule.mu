import ModelsService from "../ModelsService.js"

shell module ModelsModule [ Name = "Models" ] {
    .services: {
        ModelsService
    }

    Capability [ Name = "Models", Icon = @ModelsIcon, ServiceKey = ModelsService ]
}
