//! The agent template: the declarative, file-backed definition of an agent.

use serde::{Deserialize, Serialize};

use crate::engine::SamplingConfig;

/// Default per-run step budget when a template omits `step_budget`.
const DEFAULT_STEP_BUDGET: usize = 8;

const ERR_UNKNOWN_TOOL: &str = "unknown tool: ";
const ERR_UNKNOWN_RUNTIME: &str = "unknown runtime: ";
const ERR_UNKNOWN_MODEL: &str = "unknown model: ";
const ERR_STEP_BUDGET: &str = "step_budget must be at least 1";
const ERR_UNKNOWN_HOST: &str = "unknown host: ";

/// An agent definition, stored as one TOML file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentTemplate {
    pub name: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default = "AgentTemplate::default_step_budget")]
    pub step_budget: usize,
    pub runtime: String,
    pub model: String,
    pub n_ctx: u32,
    #[serde(default)]
    pub enable_thinking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling: Option<SamplingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

impl AgentTemplate {
    fn default_step_budget() -> usize {
        DEFAULT_STEP_BUDGET
    }
}

/// What a template may reference: the known runtimes, models (with the runtimes able to load
/// each), tools and hosts.
#[derive(Debug, Clone, Default)]
pub struct TemplateRules {
    pub runtimes: Vec<String>,
    pub models: Vec<(String, Vec<String>)>,
    pub tools: Vec<String>,
    pub hosts: Vec<String>,
}

/// Where a template store reads its rules on every use. A fixed [`TemplateRules`] is one; the
/// supervisor's catalog re-runs model discovery each time, so a model dropped on disk is known
/// without a restart.
pub trait RulesSource: Send + Sync {
    fn rules(&self) -> TemplateRules;
}

impl RulesSource for TemplateRules {
    fn rules(&self) -> TemplateRules {
        self.clone()
    }
}

impl<T: RulesSource + ?Sized> RulesSource for std::sync::Arc<T> {
    fn rules(&self) -> TemplateRules {
        (**self).rules()
    }
}

impl TemplateRules {
    pub fn validate(&self, t: &AgentTemplate) -> Result<(), String> {
        if t.step_budget < 1 {
            return Err(ERR_STEP_BUDGET.to_string());
        }
        if let Some(tool) = t.tools.iter().find(|n| !self.tools.contains(n)) {
            return Err(format!("{ERR_UNKNOWN_TOOL}{tool}"));
        }
        if !self.runtimes.contains(&t.runtime) {
            return Err(format!("{ERR_UNKNOWN_RUNTIME}{}", t.runtime));
        }
        let Some((_, runtimes)) = self.models.iter().find(|(m, _)| *m == t.model) else {
            return Err(format!("{ERR_UNKNOWN_MODEL}{}", t.model));
        };
        if !runtimes.contains(&t.runtime) {
            return Err(format!(
                "runtime {} cannot load model {}",
                t.runtime, t.model
            ));
        }
        if let Some(host) = t.host.as_ref().filter(|h| !self.hosts.contains(h)) {
            return Err(format!("{ERR_UNKNOWN_HOST}{host}"));
        }
        Ok(())
    }

    /// A small fixed rule set for tests: runtimes `llamacpp`/`candle`, models `gemma-4-e4b`
    /// (llamacpp only) and `tiny` (both), tools `read_file`/`web_search`, host `hive`.
    #[cfg(test)]
    pub fn for_tests() -> Self {
        Self {
            runtimes: vec!["llamacpp".into(), "candle".into()],
            models: vec![
                ("gemma-4-e4b".into(), vec!["llamacpp".into()]),
                ("tiny".into(), vec!["candle".into(), "llamacpp".into()]),
            ],
            tools: vec!["read_file".into(), "web_search".into()],
            hosts: vec!["hive".into()],
        }
    }
}
