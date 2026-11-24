use std::sync::Arc;
use std::time::Duration;

use clap::ValueEnum;

use super::{Engine, HttpEngine, ModelProps};

/// GPU-layer count used for the in-process engine when the caller gives none (-1 = all layers).
#[cfg(feature = "llamacpp")]
const DEFAULT_N_GPU_LAYERS: i32 = -1;
/// Context size used for the candle engine when the caller gives none.
#[cfg(feature = "candle")]
const DEFAULT_CANDLE_N_CTX: u32 = 4096;

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
    #[error("the {engine} engine is not compiled in (rebuild with --features {feature})")]
    FeatureDisabled {
        engine: &'static str,
        feature: &'static str,
    },
    #[error("failed to load the {engine} engine: {source}")]
    Load {
        engine: &'static str,
        source: super::EngineError,
    },
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

/// `Candle` validation when the feature is compiled in: a gguf model path and a tokenizer.json path
/// are both required.
#[cfg(feature = "candle")]
fn validate_candle(cfg: &EngineConfig) -> Result<(), EngineBuildError> {
    if cfg.model.is_none() {
        return Err(EngineBuildError::MissingModel);
    }
    if cfg.tokenizer.is_none() {
        return Err(EngineBuildError::MissingTokenizer);
    }
    Ok(())
}

/// `Candle` validation when the feature is absent: report that the engine isn't built in, taking
/// precedence over the missing-model/tokenizer checks so the user learns to rebuild with
/// `--features candle` rather than being asked for unusable paths. Mirrors `validate_llamacpp`.
#[cfg(not(feature = "candle"))]
fn validate_candle(_cfg: &EngineConfig) -> Result<(), EngineBuildError> {
    Err(EngineBuildError::FeatureDisabled {
        engine: "candle",
        feature: "candle",
    })
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
    Err(EngineBuildError::FeatureDisabled {
        engine: "llamacpp",
        feature: "llamacpp",
    })
}

/// How patiently the http engine waits for an upstream server that is still warming up
/// (llama-server answers 503 while loading a model, and refuses connections before it listens).
#[derive(Debug, Clone, Copy)]
pub struct HttpWarmup {
    /// Pause between `props` attempts.
    pub retry_delay: Duration,
    /// Total time to keep retrying before returning the last error.
    pub window: Duration,
}

impl HttpWarmup {
    const DEFAULT_RETRY_DELAY: Duration = Duration::from_millis(1500);
    /// Kept under the supervisor's 180s readiness timeout so the proxy fails (or binds) first.
    const DEFAULT_WINDOW: Duration = Duration::from_secs(170);

    /// Fetch `props`, retrying on any error until the window elapses.
    async fn fetch_props(&self, engine: &dyn Engine) -> Result<ModelProps, super::EngineError> {
        let deadline = tokio::time::Instant::now() + self.window;
        loop {
            match engine.props().await {
                Ok(props) => return Ok(props),
                Err(e) if tokio::time::Instant::now() + self.retry_delay >= deadline => {
                    return Err(e)
                }
                Err(_) => tokio::time::sleep(self.retry_delay).await,
            }
        }
    }
}

impl Default for HttpWarmup {
    fn default() -> Self {
        HttpWarmup {
            retry_delay: Self::DEFAULT_RETRY_DELAY,
            window: Self::DEFAULT_WINDOW,
        }
    }
}

pub async fn build_engine(
    cfg: &EngineConfig,
) -> Result<(Arc<dyn Engine>, ModelProps), EngineBuildError> {
    build_engine_with(cfg, HttpWarmup::default()).await
}

/// `build_engine` with an explicit http warm-up policy (tests use a short one).
pub async fn build_engine_with(
    cfg: &EngineConfig,
    warmup: HttpWarmup,
) -> Result<(Arc<dyn Engine>, ModelProps), EngineBuildError> {
    validate(cfg)?;
    match cfg.kind {
        EngineKind::Http => {
            let url = cfg
                .server_url
                .clone()
                .ok_or(EngineBuildError::MissingServerUrl)?;
            let engine: Arc<dyn Engine> = Arc::new(HttpEngine::new(url));
            let props = warmup.fetch_props(engine.as_ref()).await?;
            Ok((engine, props))
        }
        #[cfg(feature = "llamacpp")]
        EngineKind::Llamacpp => {
            let model = cfg.model.clone().ok_or(EngineBuildError::MissingModel)?;
            let n_gpu_layers = cfg.n_gpu_layers.unwrap_or(DEFAULT_N_GPU_LAYERS);
            // `None` here means "default to the model's trained context length" (resolved in
            // `load` once the GGUF is open); an explicit value is honored, capped at that length.
            let engine =
                super::llamacpp::LocalLlamaEngine::load(&model, None, cfg.n_ctx, n_gpu_layers)
                    .map_err(|source| EngineBuildError::Load {
                        engine: "llamacpp",
                        source,
                    })?;
            let engine: Arc<dyn Engine> = Arc::new(engine);
            let props = engine.props().await?;
            Ok((engine, props))
        }
        #[cfg(not(feature = "llamacpp"))]
        EngineKind::Llamacpp => Err(EngineBuildError::FeatureDisabled {
            engine: "llamacpp",
            feature: "llamacpp",
        }),
        #[cfg(feature = "candle")]
        EngineKind::Candle => {
            let model = cfg.model.clone().ok_or(EngineBuildError::MissingModel)?;
            let tokenizer = cfg
                .tokenizer
                .clone()
                .ok_or(EngineBuildError::MissingTokenizer)?;
            let n_ctx = cfg.n_ctx.unwrap_or(DEFAULT_CANDLE_N_CTX) as usize;
            let engine =
                super::candle::CandleEngine::load(&model, &tokenizer, n_ctx).map_err(|source| {
                    EngineBuildError::Load {
                        engine: "candle",
                        source,
                    }
                })?;
            let engine: Arc<dyn Engine> = Arc::new(engine);
            let props = engine.props().await?;
            Ok((engine, props))
        }
        #[cfg(not(feature = "candle"))]
        EngineKind::Candle => Err(EngineBuildError::FeatureDisabled {
            engine: "candle",
            feature: "candle",
        }),
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
            Err(EngineBuildError::FeatureDisabled { .. })
        ));
        c.model = Some("m.gguf".into());
        assert!(matches!(
            validate(&c),
            Err(EngineBuildError::FeatureDisabled { .. })
        ));
    }

    #[cfg(feature = "candle")]
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

    #[cfg(not(feature = "candle"))]
    #[test]
    fn candle_reports_feature_disabled_when_absent() {
        // Without the feature the kind is rejected as not-built-in, and that takes precedence
        // over the missing-model/tokenizer checks (paths present or not).
        let mut c = cfg(EngineKind::Candle);
        assert!(matches!(
            validate(&c),
            Err(EngineBuildError::FeatureDisabled { .. })
        ));
        c.model = Some("m.gguf".into());
        c.tokenizer = Some("tokenizer.json".into());
        assert!(matches!(
            validate(&c),
            Err(EngineBuildError::FeatureDisabled { .. })
        ));
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
