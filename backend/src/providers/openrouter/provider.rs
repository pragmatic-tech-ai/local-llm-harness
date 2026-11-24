//! `OpenRouterProvider`: the always-ready provider behind one OpenRouter repository.
//!
//! Nothing is loaded or evicted: every model in the repository's list is ready at once, and
//! each link is built on demand for the requested model and context size. Until the list is
//! known (never fetched and no cache), any model is served and OpenRouter answers for it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::agents::host::ModelRequirement;
use crate::providers::openrouter::api::{OpenRouterApi, OpenRouterModel};
use crate::providers::openrouter::link::{Calibration, OpenRouterLink};
use crate::providers::{
    HarnessIdentity, HostError, HostState, Measure, Provider, ProviderCapabilities,
};
use crate::runtime::worker::WorkerLink;

/// The provider of one OpenRouter repository.
pub struct OpenRouterProvider {
    repository: String,
    api: Arc<OpenRouterApi>,
    /// The repository's model list; `None` while unknown. Shared with whoever refreshes it.
    models: Arc<RwLock<Option<Vec<OpenRouterModel>>>>,
    /// The API has a key (fixed for the provider's life: a key change builds a new one).
    has_key: bool,
    /// The prompt-size calibration of each model id, shared by all its links.
    calibrations: Mutex<HashMap<String, Arc<Calibration>>>,
    events: broadcast::Sender<()>,
}

impl OpenRouterProvider {
    /// The runtime value of every OpenRouter model.
    pub const RUNTIME: &'static str = "openrouter";
    /// What an always-ready API provider offers: estimated prompt sizes and nothing more.
    pub const CAPABILITIES: ProviderCapabilities = ProviderCapabilities {
        measure_prompt: Measure::Estimated,
        tokenize: false,
        raw_prompt: false,
        logprobs: false,
        load_unload: false,
    };
    const EVENT_CAPACITY: usize = 16;
    const INSTANCE: u64 = 0;
    const ERR_REPOSITORY: &'static str = "repository ";
    const ERR_NO_KEY: &'static str = " has no API key";
    const ERR_MODEL_NOT_IN_REPOSITORY: &'static str = "model not in repository ";

    pub fn new(
        repository: String,
        api: Arc<OpenRouterApi>,
        models: Arc<RwLock<Option<Vec<OpenRouterModel>>>>,
    ) -> Self {
        OpenRouterProvider {
            repository,
            has_key: api.has_key(),
            api,
            models,
            calibrations: Mutex::new(HashMap::new()),
            events: broadcast::channel(Self::EVENT_CAPACITY).0,
        }
    }

    fn no_key_message(&self) -> String {
        format!(
            "{}{}{}",
            Self::ERR_REPOSITORY,
            self.repository,
            Self::ERR_NO_KEY
        )
    }

    fn not_served_message(&self, req: &ModelRequirement) -> String {
        format!(
            "{}{}: {}",
            Self::ERR_MODEL_NOT_IN_REPOSITORY,
            self.repository,
            req.model
        )
    }

    /// The model definition for `req`: from the list when it is known (`None` when the list
    /// lacks it), or a bare definition named by the requirement while the list is unknown.
    fn model_for(&self, req: &ModelRequirement) -> Option<OpenRouterModel> {
        if req.runtime != Self::RUNTIME {
            return None;
        }
        let models = self.models.read().unwrap_or_else(|e| e.into_inner());
        match models.as_ref() {
            Some(list) => list.iter().find(|m| m.id == req.model).cloned(),
            None => Some(OpenRouterModel {
                id: req.model.clone(),
                name: req.model.clone(),
                context_length: req.n_ctx,
                prompt_price: 0.0,
                completion_price: 0.0,
                reasoning: false,
            }),
        }
    }

    /// The calibration of `model`, created on first use.
    fn calibration(&self, model: &str) -> Arc<Calibration> {
        let mut calibrations = self.calibrations.lock().unwrap_or_else(|e| e.into_inner());
        Arc::clone(
            calibrations
                .entry(model.to_string())
                .or_insert_with(|| Arc::new(Calibration::new())),
        )
    }

    /// A link to `req`'s model, when there is a key and the model is served.
    fn link(&self, req: &ModelRequirement) -> Option<Arc<dyn WorkerLink>> {
        if !self.has_key {
            return None;
        }
        let model = self.model_for(req)?;
        let calibration = self.calibration(&model.id);
        Some(Arc::new(OpenRouterLink::new(
            Arc::clone(&self.api),
            model,
            req.n_ctx,
            calibration,
        )))
    }
}

#[async_trait]
impl Provider for OpenRouterProvider {
    fn name(&self) -> &str {
        &self.repository
    }

    fn capabilities(&self) -> ProviderCapabilities {
        Self::CAPABILITIES
    }

    fn unavailable(&self) -> Option<String> {
        (!self.has_key).then(|| self.no_key_message())
    }

    fn serves(&self, req: &ModelRequirement) -> bool {
        self.model_for(req).is_some()
    }

    /// Nothing loads, so nothing is ever loading or loaded.
    fn state(&self) -> HostState {
        HostState::default()
    }

    /// Stable for the provider's lifetime, so a turn never sees a "reload".
    fn identity(&self) -> Option<HarnessIdentity> {
        Some(HarnessIdentity {
            runtime: Self::RUNTIME.to_string(),
            model: self.repository.clone(),
            n_ctx: None,
            instance: Self::INSTANCE,
        })
    }

    fn current_link(&self, req: &ModelRequirement) -> Option<Arc<dyn WorkerLink>> {
        self.link(req)
    }

    async fn ensure_ready(&self, req: &ModelRequirement) -> Result<Arc<dyn WorkerLink>, HostError> {
        if !self.has_key {
            return Err(HostError(self.no_key_message()));
        }
        self.link(req)
            .ok_or_else(|| HostError(self.not_served_message(req)))
    }

    async fn unload_if(&self, _req: &ModelRequirement) -> Result<bool, HostError> {
        Ok(false)
    }

    fn subscribe(&self) -> broadcast::Receiver<()> {
        self.events.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::dispatcher::{Dispatcher, Placement};
    use crate::providers::FixedProviders;

    struct Fixtures;

    impl Fixtures {
        const REPOSITORY: &'static str = "or1";

        fn model(id: &str) -> OpenRouterModel {
            OpenRouterModel {
                id: id.into(),
                name: id.into(),
                context_length: 65536,
                prompt_price: 1.0,
                completion_price: 2.0,
                reasoning: false,
            }
        }

        fn req(runtime: &str, model: &str) -> ModelRequirement {
            ModelRequirement {
                runtime: runtime.into(),
                model: model.into(),
                n_ctx: 8192,
                repository: Self::REPOSITORY.into(),
            }
        }

        fn provider(
            has_key: bool,
            models: Option<Vec<OpenRouterModel>>,
        ) -> (
            Arc<OpenRouterProvider>,
            Arc<RwLock<Option<Vec<OpenRouterModel>>>>,
        ) {
            let key = has_key.then(|| "sk-or-k".to_string());
            let api = Arc::new(OpenRouterApi::new("http://127.0.0.1:1".into(), key));
            let models = Arc::new(RwLock::new(models));
            let provider = Arc::new(OpenRouterProvider::new(
                Self::REPOSITORY.into(),
                api,
                Arc::clone(&models),
            ));
            (provider, models)
        }

        fn dispatcher(provider: Arc<OpenRouterProvider>) -> Dispatcher {
            let provider: Arc<dyn Provider> = provider;
            Dispatcher::new(Arc::new(FixedProviders(vec![(
                Self::REPOSITORY.to_string(),
                provider,
            )])))
        }
    }

    #[tokio::test]
    async fn provider_without_key_is_unavailable() {
        let (provider, _models) = Fixtures::provider(false, None);
        let req = Fixtures::req("openrouter", "vendor/a");
        assert_eq!(
            provider.ensure_ready(&req).await.err(),
            Some(HostError("repository or1 has no API key".into()))
        );
        assert!(provider.current_link(&req).is_none());
        let dispatcher = Fixtures::dispatcher(provider);
        assert_eq!(
            dispatcher.status(&req),
            Placement::Unavailable {
                reason: "repository or1 has no API key".into()
            }
        );
    }

    #[tokio::test]
    async fn provider_with_key_is_always_ready() {
        let (provider, _models) = Fixtures::provider(true, None);
        let req = Fixtures::req("openrouter", "vendor/a");
        assert_eq!(
            provider.capabilities(),
            ProviderCapabilities {
                measure_prompt: Measure::Estimated,
                tokenize: false,
                raw_prompt: false,
                logprobs: false,
                load_unload: false,
            }
        );
        assert_eq!(provider.state(), HostState::default());
        assert!(provider.current_link(&req).is_some());
        let link = provider.ensure_ready(&req).await.expect("link");
        let props = link.model_props().await.expect("props");
        assert_eq!(props["model_name"], "vendor/a");
        assert_eq!(props["n_ctx"], 8192);
        assert!(!provider.unload_if(&req).await.expect("unload"));
        let identity = provider.identity().expect("identity");
        assert_eq!(provider.identity(), Some(identity));
        let dispatcher = Fixtures::dispatcher(provider);
        assert_eq!(
            dispatcher.status(&req),
            Placement::Ready { host: "or1".into() }
        );
    }

    #[tokio::test]
    async fn provider_serves_unknown_list_but_checks_known_list() {
        let (provider, models) = Fixtures::provider(true, None);
        assert!(provider.serves(&Fixtures::req("openrouter", "anything/at-all")));
        assert!(!provider.serves(&Fixtures::req("llamacpp", "anything/at-all")));

        *models.write().unwrap() = Some(vec![Fixtures::model("vendor/a")]);
        assert!(provider.serves(&Fixtures::req("openrouter", "vendor/a")));
        assert!(!provider.serves(&Fixtures::req("openrouter", "vendor/b")));
        assert!(provider
            .current_link(&Fixtures::req("openrouter", "vendor/b"))
            .is_none());
        assert!(provider
            .ensure_ready(&Fixtures::req("openrouter", "vendor/b"))
            .await
            .is_err());
    }

    #[test]
    fn calibration_is_shared_per_model() {
        let (provider, _models) = Fixtures::provider(true, None);
        let a = provider.calibration("vendor/a");
        assert!(Arc::ptr_eq(&a, &provider.calibration("vendor/a")));
        assert!(!Arc::ptr_eq(&a, &provider.calibration("vendor/b")));
    }
}
