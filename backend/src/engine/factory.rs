use std::sync::Arc;

use clap::ValueEnum;

use super::{Engine, HttpEngine, ModelProps};

/// Which `Engine` implementation to construct at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EngineKind {
    /// Talk to an external llama-server over HTTP.
    Http,
    /// Run llama.cpp in-process (requires the `llamacpp` feature).
    Llamacpp,
}

/// Engine-selection inputs, independent of how they were parsed (CLI/env).
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub kind: EngineKind,
    pub server_url: Option<String>,
    pub model: Option<String>,
    pub n_ctx: Option<u32>,
    pub n_gpu_layers: Option<i32>,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineBuildError {
    #[error("the http engine requires a server URL (--server-url / HARNESS_SERVER_URL)")]
    MissingServerUrl,
    #[error("the llamacpp engine requires a model path (--model / HARNESS_MODEL)")]
    MissingModel,
    #[error("the llamacpp engine is not compiled in (rebuild with --features llamacpp)")]
    FeatureDisabled,
    #[error("engine error: {0}")]
    Engine(#[from] super::EngineError),
}

/// Pure config check; performs no I/O.
pub fn validate(cfg: &EngineConfig) -> Result<(), EngineBuildError> {
    match cfg.kind {
        EngineKind::Http if cfg.server_url.is_none() => Err(EngineBuildError::MissingServerUrl),
        EngineKind::Llamacpp if cfg.model.is_none() => Err(EngineBuildError::MissingModel),
        _ => Ok(()),
    }
}

pub async fn build_engine(
    cfg: &EngineConfig,
) -> Result<(Arc<dyn Engine>, ModelProps), EngineBuildError> {
    validate(cfg)?;
    match cfg.kind {
        EngineKind::Http => {
            let url = cfg
                .server_url
                .clone()
                .ok_or(EngineBuildError::MissingServerUrl)?;
            let engine: Arc<dyn Engine> = Arc::new(HttpEngine::new(url));
            let props = engine.props().await?;
            Ok((engine, props))
        }
        #[cfg(feature = "llamacpp")]
        EngineKind::Llamacpp => unimplemented!("wired in a later task"),
        #[cfg(not(feature = "llamacpp"))]
        EngineKind::Llamacpp => Err(EngineBuildError::FeatureDisabled),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(kind: EngineKind) -> EngineConfig {
        EngineConfig {
            kind,
            server_url: None,
            model: None,
            n_ctx: None,
            n_gpu_layers: None,
        }
    }

    #[test]
    fn http_requires_server_url() {
        let mut c = cfg(EngineKind::Http);
        assert!(matches!(
            validate(&c),
            Err(EngineBuildError::MissingServerUrl)
        ));
        c.server_url = Some("http://127.0.0.1:8080".into());
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn llamacpp_requires_model_path() {
        let mut c = cfg(EngineKind::Llamacpp);
        assert!(matches!(validate(&c), Err(EngineBuildError::MissingModel)));
        c.model = Some("m.gguf".into());
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn engine_kind_parses_from_str() {
        assert_eq!(
            EngineKind::from_str("http", true).unwrap(),
            EngineKind::Http
        );
        assert_eq!(
            EngineKind::from_str("llamacpp", true).unwrap(),
            EngineKind::Llamacpp
        );
        assert!(EngineKind::from_str("bogus", true).is_err());
    }
}
