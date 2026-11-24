//! What a session asks of a provider.
//!
//! A [`ModelRequirement`] names runtime, model, exact context size and the repository (the
//! provider) it must come from. The providers and their state types live in
//! [`crate::providers`].

use crate::agents::template::AgentTemplate;

/// What a session needs loaded: runtime, model and exact context size, from a named repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequirement {
    pub runtime: String,
    pub model: String,
    pub n_ctx: u32,
    pub repository: String,
}

impl ModelRequirement {
    /// The repository a template that names none uses: the Pragmatic provider.
    pub const DEFAULT_REPOSITORY: &'static str = "hive";

    /// What a template snapshot asks for.
    pub fn of(snapshot: &AgentTemplate) -> Self {
        ModelRequirement {
            runtime: snapshot.runtime.clone(),
            model: snapshot.model.clone(),
            n_ctx: snapshot.n_ctx,
            repository: snapshot
                .repository
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_REPOSITORY.to_string()),
        }
    }
}
