use std::sync::Arc;

use clap::ValueEnum;

use super::{Engine, HttpEngine, ModelProps};

/// Context size used for the in-process engine when the caller gives none.
#[cfg(feature = "llamacpp")]
const DEFAULT_N_CTX: u32 = 4096;
/// GPU-layer count used for the in-process engine when the caller gives none (-1 = all layers).
#[cfg(feature = "llamacpp")]
const DEFAULT_N_GPU_LAYERS: i32 = -1;

/// Which `Engine` implementation to construct at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EngineKind {
    /// Talk to an external llama-server over HTTP.
    Http,
    /// Run llama.cpp in-process (requires the `llamacpp` feature).
    Llamacpp,
    /// Run pure-Rust HuggingFace candle in-process (requires the `candle` feature).
    Candle,
}

/// Engine-selection inputs, independent of how they were parsed (CLI/env).
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub kind: EngineKind,
    pub server_url: Option<String>,
    pub model: Option<String>,
    pub n_ctx: Option<u32>,
    pub n_gpu_layers: Option<i32>,
    /// Path to a `tokenizer.json` (candle engine).
    pub tokenizer: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineBuildError {
    #[error("the http engine requires a server URL (--server-url / HARNESS_SERVER_URL)")]
    MissingServerUrl,
    #[error("the llamacpp engine requires a model path (--model / HARNESS_MODEL)")]
    MissingModel,
    #[error("the candle engine requires a tokenizer path (--tokenizer / HARNESS_TOKENIZER)")]
    MissingTokenizer,
    #[error("the llamacpp engine is not compiled in (rebuild with --features llamacpp)")]
    FeatureDisabled,
    #[error("failed to load the llamacpp engine: {0}")]
    Load(String),
    #[error("engine error: {0}")]
    Engine(#[from] super::EngineError),
}

/// Pure config check; performs no I/O.
pub fn validate(cfg: &EngineConfig) -> Result<(), EngineBuildError> {
    match cfg.kind {
        EngineKind::Http if cfg.server_url.is_none() => Err(EngineBuildError::MissingServerUrl),
        EngineKind::Http => Ok(()),
        EngineKind::Llamacpp => validate_llamacpp(cfg),
        EngineKind::Candle => validate_candle(cfg),
    }
}

/// `Candle` validation: a gguf model path and a tokenizer.json path are both required.
fn validate_candle(cfg: &EngineConfig) -> Result<(), EngineBuildError> {
    if cfg.model.is_none() {
        return Err(EngineBuildError::MissingModel);
    }
    if cfg.tokenizer.is_none() {
        return Err(EngineBuildError::MissingTokenizer);
    }
    Ok(())
}

/// `Llamacpp` validation when the feature is compiled in: a model path is required.
#[cfg(feature = "llamacpp")]
fn validate_llamacpp(cfg: &EngineConfig) -> Result<(), EngineBuildError> {
    if cfg.model.is_none() {
        return Err(EngineBuildError::MissingModel);
    }
    Ok(())
}

/// `Llamacpp` validation when the feature is absent: report that the engine isn't built in,
/// taking precedence over the missing-model check so the user learns to rebuild with
/// `--features llamacpp` rather than being asked for an unusable model path.
#[cfg(not(feature = "llamacpp"))]
fn validate_llamacpp(_cfg: &EngineConfig) -> Result<(), EngineBuildError> {
    Err(EngineBuildError::FeatureDisabled)
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
        EngineKind::Llamacpp => {
            let model = cfg.model.clone().ok_or(EngineBuildError::MissingModel)?;
            let n_ctx = cfg.n_ctx.unwrap_or(DEFAULT_N_CTX);
            let n_gpu_layers = cfg.n_gpu_layers.unwrap_or(DEFAULT_N_GPU_LAYERS);
            let engine = super::llamacpp::LocalLlamaEngine::load(&model, None, n_ctx, n_gpu_layers)
                .map_err(|e| EngineBuildError::Load(e.to_string()))?;
            let engine: Arc<dyn Engine> = Arc::new(engine);
            let props = engine.props().await?;
            Ok((engine, props))
        }
        #[cfg(not(feature = "llamacpp"))]
        EngineKind::Llamacpp => Err(EngineBuildError::FeatureDisabled),
        #[cfg(feature = "candle")]
        EngineKind::Candle => {
            // TODO(Task 3): wire CandleEngine::load
            Err(EngineBuildError::FeatureDisabled)
        }
        #[cfg(not(feature = "candle"))]
        EngineKind::Candle => Err(EngineBuildError::FeatureDisabled),
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
            tokenizer: None,
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

    #[cfg(feature = "llamacpp")]
    #[test]
    fn llamacpp_requires_model_path() {
        let mut c = cfg(EngineKind::Llamacpp);
        assert!(matches!(validate(&c), Err(EngineBuildError::MissingModel)));
        c.model = Some("m.gguf".into());
        assert!(validate(&c).is_ok());
    }

    #[cfg(not(feature = "llamacpp"))]
    #[test]
    fn llamacpp_reports_feature_disabled_when_absent() {
        // Without the feature the kind is rejected as not-built-in, and that takes precedence
        // over the missing-model check (model present or not).
        let mut c = cfg(EngineKind::Llamacpp);
        assert!(matches!(
            validate(&c),
            Err(EngineBuildError::FeatureDisabled)
        ));
        c.model = Some("m.gguf".into());
        assert!(matches!(
            validate(&c),
            Err(EngineBuildError::FeatureDisabled)
        ));
    }

    #[test]
    fn candle_requires_model_and_tokenizer() {
        let mut c = cfg(EngineKind::Candle);
        assert!(matches!(validate(&c), Err(EngineBuildError::MissingModel)));
        c.model = Some("m.gguf".into());
        assert!(matches!(
            validate(&c),
            Err(EngineBuildError::MissingTokenizer)
        ));
        c.tokenizer = Some("tokenizer.json".into());
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn engine_kind_parses_candle() {
        assert_eq!(
            EngineKind::from_str("candle", true).unwrap(),
            EngineKind::Candle
        );
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
