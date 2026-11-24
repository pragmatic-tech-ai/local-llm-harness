//! Where the runtime reads the catalog the template editor offers and templates are validated
//! against.
//!
//! Runtimes, tools and hosts are fixed at startup. Models are re-discovered on every read, so a
//! model file dropped into a model directory shows up in `list_catalog`, passes `save_template`
//! validation and can start a session without a supervisor restart.

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

use crate::agents::template::{RulesSource, TemplateRules};
use crate::supervisor::catalog::Catalog;
use crate::supervisor::control::CatalogListing;
use crate::supervisor::discovery::discover;

/// What `list_catalog` returns: the runtimes, models (with the runtimes able to load each) and
/// host names the template editor offers.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CatalogView {
    pub runtimes: Vec<Value>,
    pub models: Vec<Value>,
    pub hosts: Vec<String>,
}

/// A source of the editor's catalog view, read on every `list_catalog`.
pub trait CatalogSource: Send + Sync {
    fn view(&self) -> CatalogView;
}

/// A fixed view is its own source.
impl CatalogSource for CatalogView {
    fn view(&self) -> CatalogView {
        self.clone()
    }
}

/// The supervisor's catalog: runtimes, tools and hosts as configured at startup, models
/// discovered from the model directories on every call.
pub struct DiscoveredCatalog {
    catalog: Arc<Catalog>,
    tools: Vec<String>,
    writing_tools: Vec<String>,
    hosts: Vec<String>,
}

impl DiscoveredCatalog {
    pub fn new(
        catalog: Arc<Catalog>,
        tools: Vec<String>,
        writing_tools: Vec<String>,
        hosts: Vec<String>,
    ) -> Self {
        Self {
            catalog,
            tools,
            writing_tools,
            hosts,
        }
    }
}

impl CatalogSource for DiscoveredCatalog {
    fn view(&self) -> CatalogView {
        CatalogView {
            runtimes: CatalogListing::runtimes(&self.catalog),
            models: CatalogListing::models(&self.catalog),
            hosts: self.hosts.clone(),
        }
    }
}

impl RulesSource for DiscoveredCatalog {
    fn rules(&self) -> TemplateRules {
        TemplateRules {
            runtimes: self
                .catalog
                .runtimes
                .iter()
                .map(|r| r.name.clone())
                .collect(),
            models: discover(&self.catalog)
                .into_iter()
                .map(|m| (m.display_name, m.runtimes))
                .collect(),
            tools: self.tools.clone(),
            writing_tools: self.writing_tools.clone(),
            hosts: self.hosts.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::test_support::TempDir;
    use crate::supervisor::catalog::{RuntimeDef, RuntimeKind};
    use std::path::PathBuf;

    const HOST: &str = "hive";
    const TOOL: &str = "read_file";
    const NAME: &str = "name";

    struct Fixtures;

    impl Fixtures {
        /// A minimal GGUF header naming `arch`.
        fn gguf(arch: &str) -> Vec<u8> {
            let mut b = b"GGUF".to_vec();
            b.extend_from_slice(&3u32.to_le_bytes());
            b.extend_from_slice(&0u64.to_le_bytes());
            b.extend_from_slice(&1u64.to_le_bytes());
            let key = "general.architecture";
            b.extend_from_slice(&(key.len() as u64).to_le_bytes());
            b.extend_from_slice(key.as_bytes());
            b.extend_from_slice(&8u32.to_le_bytes());
            b.extend_from_slice(&(arch.len() as u64).to_le_bytes());
            b.extend_from_slice(arch.as_bytes());
            b
        }

        fn catalog(dir: &std::path::Path) -> Arc<Catalog> {
            Arc::new(Catalog {
                runtimes: vec![RuntimeDef {
                    name: "llamacpp".to_string(),
                    kind: RuntimeKind::Llamacpp,
                    exe: PathBuf::from("llamacpp.exe"),
                    requires_tokenizer: false,
                    n_ctx: None,
                    n_gpu_layers: None,
                    internal_port: None,
                    server_exe: None,
                }],
                model_dirs: vec![dir.to_path_buf()],
                advertise_host: "127.0.0.1".to_string(),
                worker_port: 8777,
                tools: None,
                agents: Default::default(),
                hosts: Vec::new(),
            })
        }
    }

    #[test]
    fn a_model_dropped_on_disk_is_seen_without_a_restart() {
        let dir = TempDir::new("catalog-source");
        std::fs::write(dir.0.join("gemma.gguf"), Fixtures::gguf("gemma3")).unwrap();
        let source = DiscoveredCatalog::new(
            Fixtures::catalog(&dir.0),
            vec![TOOL.to_string()],
            vec![],
            vec![HOST.to_string()],
        );
        let before = source.view();
        assert_eq!(before.models.len(), 1);
        assert_eq!(before.hosts, [HOST]);
        assert_eq!(source.rules().models.len(), 1);
        std::fs::write(dir.0.join("late.gguf"), Fixtures::gguf("llama")).unwrap();
        let after = source.view();
        assert_eq!(after.models.len(), 2);
        assert!(after.models.iter().any(|m| m[NAME] == "late"));
        let rules = source.rules();
        assert!(rules.models.iter().any(|(m, _)| m == "late"));
        assert_eq!(rules.tools, [TOOL]);
        assert!(rules.writing_tools.is_empty());
        assert_eq!(rules.hosts, [HOST]);
        assert_eq!(rules.runtimes, ["llamacpp"]);
    }
}
