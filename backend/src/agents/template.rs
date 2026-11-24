//! The agent template: the declarative, file-backed definition of an agent.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::agents::host::ModelRequirement;
use crate::engine::SamplingConfig;
use crate::runtime::builtin::Sandbox;

/// Default per-run step budget when a template omits `step_budget`.
const DEFAULT_STEP_BUDGET: usize = 8;

const ERR_UNKNOWN_TOOL: &str = "unknown tool: ";
const ERR_UNKNOWN_RUNTIME: &str = "unknown runtime: ";
const ERR_UNKNOWN_MODEL: &str = "unknown model: ";
const ERR_STEP_BUDGET: &str = "step_budget must be at least 1";
const ERR_UNKNOWN_REPOSITORY: &str = "unknown repository: ";
const ERR_LIST_UNAVAILABLE_PREFIX: &str = "model list unavailable for repository ";
const ERR_CTX_EXCEEDS_PREFIX: &str = "n_ctx exceeds the model's context length (";
const OPENROUTER_RUNTIME: &str = "openrouter";
const ERR_UNKNOWN_STRATEGY: &str = "unknown context strategy: ";
const ERR_COMPACT_PCT: &str = "compact_at_pct must be between 10 and 95";
const ERR_UNKNOWN_WORKSPACE_MODE: &str = "unknown workspace mode: ";
const ERR_WORKSPACE_NAME_REQUIRED: &str = "workspace name is required for named mode";
const ERR_WORKSPACE_NAME_INVALID: &str = "workspace name must be 1-64 letters, digits, _ or -";
const ERR_WORKSPACE_NAME_RESERVED: &str = "workspace name is a reserved Windows device name";
const MAX_WORKSPACE_NAME_LEN: usize = 64;
const SESSIONS_FOLDER: &str = "sessions";
const SHARED_FOLDER: &str = "shared";
const NAMED_FOLDER: &str = "workspaces";
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

/// Where an agent's writing tools keep their files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorkspaceMode {
    /// A private folder per session.
    #[default]
    Session,
    /// One folder shared by every agent that asks for it.
    Shared,
    /// A folder chosen by name.
    Named,
}

impl WorkspaceMode {
    /// Every mode with its wire name: the one table behind serialization and parsing.
    const ALL: [(Self, &'static str); 3] = [
        (Self::Session, "session"),
        (Self::Shared, "shared"),
        (Self::Named, "named"),
    ];

    /// The wire name.
    pub fn as_str(&self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(m, _)| m == self)
            .map(|(_, n)| *n)
            .unwrap_or_default()
    }

    fn parse(v: &str) -> Option<Self> {
        Self::ALL.iter().find(|(_, n)| *n == v).map(|(m, _)| *m)
    }
}

impl Serialize for WorkspaceMode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for WorkspaceMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = String::deserialize(d)?;
        Self::parse(&v)
            .ok_or_else(|| serde::de::Error::custom(format!("{ERR_UNKNOWN_WORKSPACE_MODE}{v}")))
    }
}

/// A template's workspace: the mode, plus the folder name for `named`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    #[serde(default)]
    pub mode: WorkspaceMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl WorkspaceConfig {
    /// The folder for this workspace, relative to the sandbox.
    pub fn folder(&self, session_id: &str) -> PathBuf {
        match self.mode {
            WorkspaceMode::Session => [SESSIONS_FOLDER, session_id].iter().collect(),
            WorkspaceMode::Shared => PathBuf::from(SHARED_FOLDER),
            WorkspaceMode::Named => [NAMED_FOLDER, self.name.as_deref().unwrap_or_default()]
                .iter()
                .collect(),
        }
    }

    /// The same config with the name dropped unless the mode is `named`.
    pub fn normalized(self) -> Self {
        match self.mode {
            WorkspaceMode::Named => self,
            _ => Self { name: None, ..self },
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.mode != WorkspaceMode::Named {
            return Ok(());
        }
        let name = self.name.as_deref().unwrap_or_default();
        if name.is_empty() {
            return Err(ERR_WORKSPACE_NAME_REQUIRED.to_string());
        }
        let ok = name.chars().count() <= MAX_WORKSPACE_NAME_LEN
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !ok {
            return Err(ERR_WORKSPACE_NAME_INVALID.to_string());
        }
        // The folder must be creatable: the file tools refuse device names such as `con`.
        if Sandbox::is_reserved_device_name(name) {
            return Err(ERR_WORKSPACE_NAME_RESERVED.to_string());
        }
        Ok(())
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
    /// The repository the model comes from; `host` is the old name and still reads. A missing
    /// value means `hive`.
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "host")]
    pub repository: Option<String>,
    #[serde(default)]
    pub context_strategy: ContextStrategy,
    #[serde(
        default = "AgentTemplate::default_compact_at_pct",
        deserialize_with = "AgentTemplate::deserialize_compact_at_pct"
    )]
    pub compact_at_pct: u8,
    #[serde(default)]
    pub workspace: WorkspaceConfig,
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

/// How a repository serves its models, which decides how a template naming it is validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepositoryKind {
    #[serde(rename = "pragmatic")]
    Pragmatic,
    #[serde(rename = "openrouter")]
    OpenRouter,
}

/// One model a repository offers: the runtimes able to load it and its context length, if
/// known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleModel {
    pub id: String,
    pub runtimes: Vec<String>,
    pub context_length: Option<u32>,
}

/// A repository a template may name, with its models. `None` means the list is unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryRules {
    pub name: String,
    pub kind: RepositoryKind,
    pub models: Option<Vec<RuleModel>>,
}

/// What a template may reference: the known runtimes, tools and repositories (each with the
/// models it offers).
#[derive(Debug, Clone, Default)]
pub struct TemplateRules {
    pub runtimes: Vec<String>,
    pub tools: Vec<String>,
    pub repositories: Vec<RepositoryRules>,
    /// The names of the registry tools that have side effects.
    pub writing_tools: Vec<String>,
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

impl RuleModel {
    /// A model loadable by `runtimes`, with no known context length.
    pub fn of(id: &str, runtimes: &[&str]) -> Self {
        Self {
            id: id.to_string(),
            runtimes: runtimes.iter().map(|r| r.to_string()).collect(),
            context_length: None,
        }
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
        self.validate_model(t)?;
        if !(MIN_COMPACT_AT_PCT..=MAX_COMPACT_AT_PCT).contains(&t.compact_at_pct) {
            return Err(ERR_COMPACT_PCT.to_string());
        }
        t.workspace.validate()?;
        Ok(())
    }

    /// The runtime, model and context checks, against the repository the template names.
    fn validate_model(&self, t: &AgentTemplate) -> Result<(), String> {
        let name = t
            .repository
            .as_deref()
            .unwrap_or(ModelRequirement::DEFAULT_REPOSITORY);
        let Some(repo) = self.repositories.iter().find(|r| r.name == name) else {
            return Err(format!("{ERR_UNKNOWN_REPOSITORY}{name}"));
        };
        match repo.kind {
            RepositoryKind::Pragmatic => {
                if !self.runtimes.contains(&t.runtime) {
                    return Err(format!("{ERR_UNKNOWN_RUNTIME}{}", t.runtime));
                }
                let found = repo
                    .models
                    .iter()
                    .flatten()
                    .find(|m| m.id == t.model)
                    .ok_or_else(|| format!("{ERR_UNKNOWN_MODEL}{}", t.model))?;
                if !found.runtimes.contains(&t.runtime) {
                    return Err(format!(
                        "runtime {} cannot load model {}",
                        t.runtime, t.model
                    ));
                }
                Ok(())
            }
            RepositoryKind::OpenRouter => {
                if t.runtime != OPENROUTER_RUNTIME {
                    return Err(format!("{ERR_UNKNOWN_RUNTIME}{}", t.runtime));
                }
                let Some(models) = repo.models.as_ref() else {
                    return Err(format!("{ERR_LIST_UNAVAILABLE_PREFIX}{name}"));
                };
                let found = models
                    .iter()
                    .find(|m| m.id == t.model)
                    .ok_or_else(|| format!("{ERR_UNKNOWN_MODEL}{}", t.model))?;
                match found.context_length {
                    Some(n) if t.n_ctx > n => Err(format!("{ERR_CTX_EXCEEDS_PREFIX}{n})")),
                    _ => Ok(()),
                }
            }
        }
    }

    /// The models of the first Pragmatic repository, preferring `hive`: what a seed template
    /// picks from.
    pub fn pragmatic_models(&self) -> &[RuleModel] {
        self.repositories
            .iter()
            .filter(|r| r.kind == RepositoryKind::Pragmatic)
            .min_by_key(|r| r.name != ModelRequirement::DEFAULT_REPOSITORY)
            .and_then(|r| r.models.as_deref())
            .unwrap_or_default()
    }

    /// A small fixed rule set for tests: runtimes `llamacpp`/`candle`, models `gemma-4-e4b`
    /// (llamacpp only) and `tiny` (both), tools `read_file`/`web_search`, host `hive`.
    #[cfg(test)]
    pub fn for_tests() -> Self {
        Self {
            runtimes: vec!["llamacpp".into(), "candle".into()],
            tools: vec!["read_file".into(), "web_search".into()],
            repositories: vec![RepositoryRules {
                name: "hive".into(),
                kind: RepositoryKind::Pragmatic,
                models: Some(vec![
                    RuleModel::of("gemma-4-e4b", &["llamacpp"]),
                    RuleModel::of("tiny", &["candle", "llamacpp"]),
                ]),
            }],
            writing_tools: vec![],
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

    #[test]
    fn workspace_defaults_to_session_when_missing() {
        let t: AgentTemplate = toml::from_str(BASE).unwrap();
        assert_eq!(t.workspace.mode, WorkspaceMode::Session);
        assert_eq!(t.workspace.name, None);
    }

    #[test]
    fn workspace_named_round_trips() {
        let text = format!("{BASE}[workspace]\nmode = \"named\"\nname = \"research\"\n");
        let t: AgentTemplate = toml::from_str(&text).unwrap();
        assert_eq!(t.workspace.mode, WorkspaceMode::Named);
        assert_eq!(t.workspace.name.as_deref(), Some("research"));
        let again: AgentTemplate = toml::from_str(&toml::to_string_pretty(&t).unwrap()).unwrap();
        assert_eq!(again, t);
    }

    #[test]
    fn unknown_workspace_mode_is_rejected() {
        let text = format!("{BASE}[workspace]\nmode = \"bogus\"\n");
        let e = toml::from_str::<AgentTemplate>(&text)
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown workspace mode: bogus"), "{e}");
    }

    #[test]
    fn named_without_name_is_rejected() {
        let rules = TemplateRules::for_tests();
        let mut t: AgentTemplate = toml::from_str(BASE).unwrap();
        t.workspace.mode = WorkspaceMode::Named;
        for name in [None, Some(String::new())] {
            t.workspace.name = name;
            assert_eq!(
                rules.validate(&t).unwrap_err(),
                "workspace name is required for named mode"
            );
        }
    }

    #[test]
    fn bad_workspace_name_is_rejected() {
        let rules = TemplateRules::for_tests();
        let mut t: AgentTemplate = toml::from_str(BASE).unwrap();
        t.workspace.mode = WorkspaceMode::Named;
        for bad in ["a b", "x/y", &"a".repeat(65)] {
            t.workspace.name = Some(bad.to_string());
            assert_eq!(
                rules.validate(&t).unwrap_err(),
                "workspace name must be 1-64 letters, digits, _ or -"
            );
        }
        t.workspace.name = Some("aZ0_-".repeat(12) + "aZ0_");
        assert_eq!(t.workspace.name.as_ref().unwrap().len(), 64);
        assert!(rules.validate(&t).is_ok());
    }

    #[test]
    fn reserved_device_workspace_name_is_rejected() {
        let rules = TemplateRules::for_tests();
        let mut t: AgentTemplate = toml::from_str(BASE).unwrap();
        t.workspace.mode = WorkspaceMode::Named;
        for bad in ["con", "NUL", "Aux", "prn", "com1", "LPT9"] {
            t.workspace.name = Some(bad.to_string());
            assert_eq!(
                rules.validate(&t).unwrap_err(),
                "workspace name is a reserved Windows device name",
                "{bad}"
            );
        }
        for good in ["console", "com10", "lpt", "nul_x"] {
            t.workspace.name = Some(good.to_string());
            assert!(rules.validate(&t).is_ok(), "{good}");
        }
    }

    #[test]
    fn name_on_other_mode_is_dropped_by_normalized() {
        let w = WorkspaceConfig {
            mode: WorkspaceMode::Shared,
            name: Some("x".into()),
        };
        assert_eq!(w.normalized().name, None);
        let n = WorkspaceConfig {
            mode: WorkspaceMode::Named,
            name: Some("x".into()),
        };
        assert_eq!(n.clone().normalized(), n);
    }

    #[test]
    fn folder_maps_each_mode() {
        let named = WorkspaceConfig {
            mode: WorkspaceMode::Named,
            name: Some("research".into()),
        };
        let shared = WorkspaceConfig {
            mode: WorkspaceMode::Shared,
            name: None,
        };
        let session = WorkspaceConfig::default();
        assert_eq!(
            session.folder("s-1-0"),
            ["sessions", "s-1-0"].iter().collect::<std::path::PathBuf>()
        );
        assert_eq!(shared.folder("s-1-0"), std::path::PathBuf::from("shared"));
        assert_eq!(
            named.folder("s-1-0"),
            ["workspaces", "research"]
                .iter()
                .collect::<std::path::PathBuf>()
        );
    }

    struct Fixtures;

    impl Fixtures {
        const OR_MODEL: &'static str = "vendor/model";
        const OR_CTX: u32 = 8192;

        fn openrouter(models: Option<Vec<RuleModel>>) -> TemplateRules {
            let mut rules = TemplateRules::for_tests();
            rules.repositories.push(RepositoryRules {
                name: "or".into(),
                kind: RepositoryKind::OpenRouter,
                models,
            });
            rules
        }

        fn or_models() -> Option<Vec<RuleModel>> {
            Some(vec![RuleModel {
                id: Self::OR_MODEL.into(),
                runtimes: vec!["openrouter".into()],
                context_length: Some(Self::OR_CTX),
            }])
        }

        fn or_template(runtime: &str, model: &str, n_ctx: u32) -> AgentTemplate {
            let text = format!(
                "name = \"T\"
repository = \"or\"
runtime = \"{runtime}\"
model = \"{model}\"
n_ctx = {n_ctx}
"
            );
            toml::from_str(&text).unwrap()
        }
    }

    #[test]
    fn legacy_host_field_routes_to_hive() {
        let t: AgentTemplate = toml::from_str(&format!(
            "{BASE}host = \"hive\"
"
        ))
        .unwrap();
        assert_eq!(t.repository.as_deref(), Some("hive"));
        assert_eq!(ModelRequirement::of(&t).repository, "hive");
        assert!(!toml::to_string_pretty(&t).unwrap().contains("host"));
    }

    #[test]
    fn missing_repository_means_hive() {
        let t: AgentTemplate = toml::from_str(BASE).unwrap();
        assert_eq!(t.repository, None);
        assert_eq!(ModelRequirement::of(&t).repository, "hive");
        assert_eq!(TemplateRules::for_tests().validate(&t), Ok(()));
    }

    #[test]
    fn unknown_repository_is_rejected() {
        let t: AgentTemplate = toml::from_str(&format!(
            "{BASE}repository = \"far\"
"
        ))
        .unwrap();
        assert_eq!(
            TemplateRules::for_tests().validate(&t),
            Err("unknown repository: far".to_string())
        );
    }

    #[test]
    fn openrouter_model_validates_runtime_model_and_ctx() {
        let rules = Fixtures::openrouter(Fixtures::or_models());
        let ok = Fixtures::or_template("openrouter", Fixtures::OR_MODEL, Fixtures::OR_CTX);
        assert_eq!(rules.validate(&ok), Ok(()));
        let wrong_runtime = Fixtures::or_template("llamacpp", Fixtures::OR_MODEL, 4096);
        assert_eq!(
            rules.validate(&wrong_runtime),
            Err("unknown runtime: llamacpp".to_string())
        );
        let wrong_model = Fixtures::or_template("openrouter", "nope", 4096);
        assert_eq!(
            rules.validate(&wrong_model),
            Err("unknown model: nope".to_string())
        );
        let too_big = Fixtures::or_template("openrouter", Fixtures::OR_MODEL, Fixtures::OR_CTX + 1);
        assert_eq!(
            rules.validate(&too_big),
            Err("n_ctx exceeds the model's context length (8192)".to_string())
        );
    }

    #[test]
    fn openrouter_unknown_list_refuses_save() {
        let rules = Fixtures::openrouter(None);
        let t = Fixtures::or_template("openrouter", Fixtures::OR_MODEL, 4096);
        assert_eq!(
            rules.validate(&t),
            Err("model list unavailable for repository or".to_string())
        );
    }

    #[test]
    fn pragmatic_validation_unchanged() {
        let rules = TemplateRules::for_tests();
        let t: AgentTemplate = toml::from_str(BASE).unwrap();
        assert_eq!(rules.validate(&t), Ok(()));
        let mut bad_runtime = t.clone();
        bad_runtime.runtime = "nope".into();
        assert_eq!(
            rules.validate(&bad_runtime),
            Err("unknown runtime: nope".to_string())
        );
        let mut bad_model = t.clone();
        bad_model.model = "nope".into();
        assert_eq!(
            rules.validate(&bad_model),
            Err("unknown model: nope".to_string())
        );
        let mut cannot = t;
        cannot.model = "gemma-4-e4b".into();
        cannot.runtime = "candle".into();
        assert_eq!(
            rules.validate(&cannot),
            Err("runtime candle cannot load model gemma-4-e4b".to_string())
        );
    }
}
