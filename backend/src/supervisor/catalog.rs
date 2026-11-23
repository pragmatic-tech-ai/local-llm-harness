//! Catalog config: which runtimes the supervisor can launch and where models live.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Which backend binary flavour a runtime entry launches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    Candle,
    Llamacpp,
    Http,
}

/// One launchable runtime (a backend executable plus its default parameters).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDef {
    pub name: String,
    pub kind: RuntimeKind,
    pub exe: PathBuf,
    #[serde(default)]
    pub requires_tokenizer: bool,
    #[serde(default)]
    pub n_ctx: Option<u32>,
    #[serde(default)]
    pub n_gpu_layers: Option<i32>,
    #[serde(default)]
    pub internal_port: Option<u16>,
    /// For `http`: the llama-server binary (`exe` is then the http-proxy backend).
    #[serde(default)]
    pub server_exe: Option<PathBuf>,
}

/// Errors loading or parsing a catalog.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("failed to read catalog {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid catalog: {0}")]
    Parse(#[from] toml::de::Error),
}

/// Agent runtime tool settings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    /// Directory the file tools may read; nothing outside it is reachable.
    pub sandbox: PathBuf,
}

/// The parsed supervisor catalog.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    #[serde(default, rename = "runtime")]
    pub runtimes: Vec<RuntimeDef>,
    #[serde(default)]
    pub model_dirs: Vec<PathBuf>,
    #[serde(default = "Catalog::default_advertise_host")]
    pub advertise_host: String,
    #[serde(default = "Catalog::default_worker_port")]
    pub worker_port: u16,
    #[serde(default)]
    pub tools: Option<ToolsConfig>,
}

impl Catalog {
    const DEFAULT_ADVERTISE_HOST: &'static str = "192.168.1.101";
    const DEFAULT_WORKER_PORT: u16 = 8777;

    fn default_advertise_host() -> String {
        Self::DEFAULT_ADVERTISE_HOST.to_string()
    }

    fn default_worker_port() -> u16 {
        Self::DEFAULT_WORKER_PORT
    }

    /// Parse a catalog from TOML text.
    pub fn from_toml(text: &str) -> Result<Catalog, CatalogError> {
        Ok(toml::from_str(text)?)
    }

    /// Read and parse a catalog file.
    pub fn load(path: &Path) -> Result<Catalog, CatalogError> {
        let text = std::fs::read_to_string(path).map_err(|source| CatalogError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_toml(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
advertise_host = "10.0.0.5"
worker_port = 9000
model_dirs = ["C:/llm-harness/models", "C:/Users/visor/.lmstudio/models"]

[[runtime]]
name = "candle"
kind = "candle"
exe = "C:/b/backend-candle.exe"
requires_tokenizer = true
n_ctx = 4096

[[runtime]]
name = "llamacpp"
kind = "llamacpp"
exe = "C:/b/backend-llamacpp.exe"
n_ctx = 4096
n_gpu_layers = 28

[[runtime]]
name = "http"
kind = "http"
exe = "C:/b/backend.exe"
n_ctx = 4096
n_gpu_layers = 28
internal_port = 8080
"#;

    #[test]
    fn parses_runtimes_and_dirs() {
        let c = Catalog::from_toml(SAMPLE).unwrap();
        assert_eq!(c.advertise_host, "10.0.0.5");
        assert_eq!(c.worker_port, 9000);
        assert_eq!(
            c.model_dirs,
            vec![
                PathBuf::from("C:/llm-harness/models"),
                PathBuf::from("C:/Users/visor/.lmstudio/models")
            ]
        );
        assert_eq!(c.runtimes.len(), 3);
        assert_eq!(
            c.runtimes[0],
            RuntimeDef {
                name: "candle".into(),
                kind: RuntimeKind::Candle,
                exe: PathBuf::from("C:/b/backend-candle.exe"),
                requires_tokenizer: true,
                n_ctx: Some(4096),
                n_gpu_layers: None,
                internal_port: None,
                server_exe: None,
            }
        );
        assert_eq!(c.runtimes[1].kind, RuntimeKind::Llamacpp);
        assert_eq!(c.runtimes[1].n_gpu_layers, Some(28));
        assert!(!c.runtimes[1].requires_tokenizer);
        assert_eq!(c.runtimes[2].kind, RuntimeKind::Http);
        assert_eq!(c.runtimes[2].internal_port, Some(8080));
    }

    #[test]
    fn missing_advertise_host_defaults_to_hive() {
        let c = Catalog::from_toml("model_dirs = []\n").unwrap();
        assert_eq!(c.advertise_host, "192.168.1.101");
        assert_eq!(c.worker_port, 8777);
        assert!(c.runtimes.is_empty());
    }

    #[test]
    fn catalog_parses_tools_sandbox() {
        let c = Catalog::from_toml(
            "[tools]
sandbox = \"C:/llm-harness/sandbox\"
",
        )
        .unwrap();
        assert_eq!(
            c.tools,
            Some(ToolsConfig {
                sandbox: PathBuf::from("C:/llm-harness/sandbox")
            })
        );
    }

    #[test]
    fn catalog_without_tools_section_is_none() {
        let c = Catalog::from_toml(
            "model_dirs = []
",
        )
        .unwrap();
        assert_eq!(c.tools, None);
    }

    #[test]
    fn unknown_runtime_kind_errors() {
        let text = "[[runtime]]\nname = \"x\"\nkind = \"bogus\"\nexe = \"x.exe\"\n";
        assert!(matches!(
            Catalog::from_toml(text),
            Err(CatalogError::Parse(_))
        ));
    }

    #[test]
    fn load_reads_and_parses_a_file() {
        let path =
            std::env::temp_dir().join(format!("catalog-load-ok-{}.toml", std::process::id()));
        std::fs::write(&path, "worker_port = 1234\n").unwrap();
        let loaded = Catalog::load(&path);
        std::fs::remove_file(&path).ok();
        let c = loaded.unwrap();
        assert_eq!(c.worker_port, 1234);
        assert_eq!(c.advertise_host, "192.168.1.101");
    }

    #[test]
    fn load_missing_file_is_io_error() {
        let path =
            std::env::temp_dir().join(format!("catalog-missing-{}.toml", std::process::id()));
        assert!(matches!(Catalog::load(&path), Err(CatalogError::Io { .. })));
    }

    #[test]
    fn unknown_key_errors() {
        assert!(matches!(
            Catalog::from_toml("model_dir = []\n"),
            Err(CatalogError::Parse(_))
        ));
    }
}
