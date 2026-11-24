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
const ERR_UNKNOWN_STRATEGY: &str = "unknown context strategy: ";
const ERR_COMPACT_PCT: &str = "compact_at_pct must be between 10 and 95";
pub(crate) const DEFAULT_COMPACT_AT_PCT: u8 = 75;
const MIN_COMPACT_AT_PCT: u8 = 10;
const MAX_COMPACT_AT_PCT: u8 = 95;

/// How an agent keeps its prompt inside the context window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ContextStrategy {
    #[default]
    Off,
    Trim,
    Summarize,
}

impl ContextStrategy {
    /// Every strategy with its wire name: the one table behind serialization and parsing.
    const ALL: [(Self, &'static str); 3] = [
        (Self::Off, "off"),
        (Self::Trim, "trim"),
        (Self::Summarize, "summarize"),
    ];

    /// The wire name.
    pub fn as_str(&self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(s, _)| s == self)
            .map(|(_, n)| *n)
            .unwrap_or_default()
    }

    fn parse(v: &str) -> Option<Self> {
        Self::ALL.iter().find(|(_, n)| *n == v).map(|(s, _)| *s)
    }
}

impl Serialize for ContextStrategy {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ContextStrategy {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = String::deserialize(d)?;
        Self::parse(&v)
            .ok_or_else(|| serde::de::Error::custom(format!("{ERR_UNKNOWN_STRATEGY}{v}")))
    }
}

/// A saved summary of the older part of a session's transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactionState {
    pub summary: String,
    /// A transcript index: the summary replaces the messages after the system message, up to
    /// but not including this one.
    pub covers: usize,
    /// The note text's own tokens, as the summary call renders it.
    pub tokens: u32,
    pub at: u64,
}

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
    #[serde(default)]
    pub context_strategy: ContextStrategy,
    #[serde(
        default = "AgentTemplate::default_compact_at_pct",
        deserialize_with = "AgentTemplate::deserialize_compact_at_pct"
    )]
    pub compact_at_pct: u8,
}

impl AgentTemplate {
    fn default_step_budget() -> usize {
        DEFAULT_STEP_BUDGET
    }

    fn default_compact_at_pct() -> u8 {
        DEFAULT_COMPACT_AT_PCT
    }

    /// Reads any integer, then range-checks it, so 300 or -1 give the range message rather
    /// than a serde type error.
    fn deserialize_compact_at_pct<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
        let v = i64::deserialize(d)?;
        u8::try_from(v)
            .ok()
            .filter(|p| (MIN_COMPACT_AT_PCT..=MAX_COMPACT_AT_PCT).contains(p))
            .ok_or_else(|| serde::de::Error::custom(ERR_COMPACT_PCT))
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
        if !(MIN_COMPACT_AT_PCT..=MAX_COMPACT_AT_PCT).contains(&t.compact_at_pct) {
            return Err(ERR_COMPACT_PCT.to_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "name = \"T\"\nruntime = \"llamacpp\"\nmodel = \"tiny\"\nn_ctx = 4096\n";

    #[test]
    fn context_fields_default_when_absent() {
        let t: AgentTemplate = toml::from_str(BASE).unwrap();
        assert_eq!(t.context_strategy, ContextStrategy::Off);
        assert_eq!(t.compact_at_pct, 75);
    }

    #[test]
    fn context_fields_round_trip() {
        let text = format!("{BASE}context_strategy = \"summarize\"\ncompact_at_pct = 60\n");
        let t: AgentTemplate = toml::from_str(&text).unwrap();
        assert_eq!(t.context_strategy, ContextStrategy::Summarize);
        assert_eq!(t.compact_at_pct, 60);
        let again: AgentTemplate = toml::from_str(&toml::to_string_pretty(&t).unwrap()).unwrap();
        assert_eq!(again, t);
    }

    #[test]
    fn unknown_context_strategy_is_rejected() {
        let text = format!("{BASE}context_strategy = \"shrink\"\n");
        let e = toml::from_str::<AgentTemplate>(&text)
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown context strategy: shrink"), "{e}");
        let j = serde_json::from_value::<ContextStrategy>(serde_json::json!("shrink"))
            .unwrap_err()
            .to_string();
        assert!(j.contains("unknown context strategy: shrink"), "{j}");
    }

    #[test]
    fn wide_out_of_range_compact_at_pct_gives_the_range_message() {
        for bad in ["300", "-1", "0"] {
            let text = format!("{BASE}compact_at_pct = {bad}\n");
            let e = toml::from_str::<AgentTemplate>(&text)
                .unwrap_err()
                .to_string();
            assert!(e.contains(ERR_COMPACT_PCT), "{e}");
            let j = serde_json::from_str::<AgentTemplate>(&format!(
                r#"{{"name":"T","runtime":"r","model":"m","n_ctx":1,"compact_at_pct":{bad}}}"#
            ))
            .unwrap_err()
            .to_string();
            assert!(j.contains(ERR_COMPACT_PCT), "{j}");
        }
    }

    #[test]
    fn compact_at_pct_out_of_range_is_rejected() {
        let rules = TemplateRules::for_tests();
        let mut t: AgentTemplate = toml::from_str(BASE).unwrap();
        for bad in [9u8, 96] {
            t.compact_at_pct = bad;
            assert_eq!(
                rules.validate(&t).unwrap_err(),
                "compact_at_pct must be between 10 and 95"
            );
        }
        for ok in [10u8, 95] {
            t.compact_at_pct = ok;
            assert!(rules.validate(&t).is_ok());
        }
    }
}
