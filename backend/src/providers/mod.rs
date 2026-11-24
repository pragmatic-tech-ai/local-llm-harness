//! Model providers: where a session's model comes from.
//!
//! A [`Provider`] answers "can you serve this runtime+model", reports what it currently has
//! loaded, hands out a [`WorkerLink`] to the loaded model, and loads/unloads on demand. Each
//! provider fronts one named repository; the
//! [`Dispatcher`](crate::agents::dispatcher::Dispatcher) routes a [`ModelRequirement`] to exactly
//! the provider its `repository` names, with no fallback to another one. A [`ProviderSource`]
//! hands providers to the dispatcher by repository name.
//!
//! [`Provider::capabilities`] says which optional features a provider offers
//! ([`ProviderCapabilities`]), so callers can degrade instead of calling something unsupported.
//! [`Provider::identity`] names the exact harness a link reaches, so a turn can tell a reload of
//! its model (a new instance) from the harness it started on.
//!
//! [`PragmaticProvider`] is the first provider: the in-process harness manager.

pub mod openrouter;
pub mod pragmatic;

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::agents::host::{HarnessIdentity, HostError, HostState, ModelRequirement};
use crate::runtime::worker::WorkerLink;

pub use pragmatic::{LocalHarness, PragmaticProvider};

/// How exactly a provider can measure a prompt's size in tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Measure {
    Exact,
    Estimated,
}

/// The optional features a provider offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub measure_prompt: Measure,
    pub tokenize: bool,
    pub raw_prompt: bool,
    pub logprobs: bool,
    pub load_unload: bool,
}

impl ProviderCapabilities {
    /// Everything: exact measuring, tokenizing, raw prompts, logprobs and load/unload.
    pub const FULL: ProviderCapabilities = ProviderCapabilities {
        measure_prompt: Measure::Exact,
        tokenize: true,
        raw_prompt: true,
        logprobs: true,
        load_unload: true,
    };
}

/// A source of models behind one repository name.
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    /// What this provider supports beyond running a chat.
    fn capabilities(&self) -> ProviderCapabilities;
    /// Why this provider can serve nothing right now (an OpenRouter repository without a key),
    /// or `None` when it can. The dispatcher reports the reason as the placement.
    fn unavailable(&self) -> Option<String> {
        None
    }
    /// Whether this provider can ever serve `req`'s runtime and model (cheap; no loading). The
    /// repository is not checked: routing by repository is the dispatcher's job.
    fn serves(&self, req: &ModelRequirement) -> bool;
    fn state(&self) -> HostState;
    /// The Ready harness this provider serves now, if any.
    fn identity(&self) -> Option<HarnessIdentity>;
    /// Re-reads which models this provider can serve (models added or removed on disk).
    /// Providers with a fixed model list keep the default, which does nothing.
    fn refresh(&self) {}
    /// A link to the loaded model, only if it matches `req` exactly (runtime, model, n_ctx).
    fn current_link(&self, req: &ModelRequirement) -> Option<Arc<dyn WorkerLink>>;
    /// A link to a model matching `req`, loading it (and evicting what is loaded) if needed.
    async fn ensure_ready(&self, req: &ModelRequirement) -> Result<Arc<dyn WorkerLink>, HostError>;
    /// Unload only if the loaded model is `req`'s runtime and model (any n_ctx). The check and the
    /// stop happen atomically under the provider's loading lock, so a concurrent load cannot slip
    /// in between. Returns whether anything was unloaded.
    async fn unload_if(&self, req: &ModelRequirement) -> Result<bool, HostError>;
    /// Fires on any state change. Must be called inside a tokio runtime.
    fn subscribe(&self) -> broadcast::Receiver<()>;
}

/// Hands the dispatcher its providers, by repository name.
pub trait ProviderSource: Send + Sync {
    /// The provider behind `repository`, if there is one.
    fn provider(&self, repository: &str) -> Option<Arc<dyn Provider>>;
    /// Every provider, for refreshing and for merging their change signals.
    fn providers(&self) -> Vec<Arc<dyn Provider>>;
}

/// A fixed list of `(repository name, provider)` pairs. Temporary: a registry-backed source
/// replaces it for `main.rs` once repositories are configurable.
pub struct StaticProviders(pub Vec<(String, Arc<dyn Provider>)>);

impl ProviderSource for StaticProviders {
    fn provider(&self, repository: &str) -> Option<Arc<dyn Provider>> {
        self.0
            .iter()
            .find(|(name, _)| name == repository)
            .map(|(_, provider)| Arc::clone(provider))
    }

    fn providers(&self) -> Vec<Arc<dyn Provider>> {
        self.0.iter().map(|(_, p)| Arc::clone(p)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_serialize_with_wire_names() {
        let json = serde_json::to_value(ProviderCapabilities::FULL).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"measure_prompt":"exact","tokenize":true,"raw_prompt":true,"logprobs":true,"load_unload":true})
        );
        let estimated: Measure = serde_json::from_str("\"estimated\"").unwrap();
        assert_eq!(estimated, Measure::Estimated);
    }
}
