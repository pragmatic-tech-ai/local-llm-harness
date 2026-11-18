#[cfg(feature = "candle")]
pub mod candle;
pub mod factory;
pub mod http;
#[cfg(feature = "llamacpp")]
pub mod llamacpp;
pub mod logprobs;
pub mod template;
pub mod types;

use async_trait::async_trait;
use futures::stream::BoxStream;

#[cfg(feature = "candle")]
pub use candle::CandleEngine;
pub use factory::{build_engine, validate, EngineBuildError, EngineConfig, EngineKind};
pub use http::HttpEngine;
#[cfg(feature = "llamacpp")]
pub use llamacpp::LocalLlamaEngine;
pub use types::{
    ChatMessage, CompletionChunk, ModelProps, Role, SamplingConfig, TokenInfo, TokenizeResult,
};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("connection refused")]
    ConnectionRefused,
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("stream interrupted")]
    StreamInterrupted,
}

pub type Result<T> = std::result::Result<T, EngineError>;

#[async_trait]
pub trait Engine: Send + Sync {
    async fn props(&self) -> Result<ModelProps>;
    async fn tokenize(&self, text: &str) -> Result<TokenizeResult>;
    async fn detokenize(&self, tokens: &[i64]) -> Result<String>;
    async fn complete(
        &self,
        prompt: &str,
        cfg: &SamplingConfig,
    ) -> Result<BoxStream<'static, Result<CompletionChunk>>>;
    /// Renders `messages` into the model's prompt string using the server's own chat template.
    async fn apply_template(
        &self,
        messages: &[ChatMessage],
        add_generation_prompt: bool,
        enable_thinking: bool,
    ) -> Result<String>;
}
