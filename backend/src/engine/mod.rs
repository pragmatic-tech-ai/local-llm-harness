#[cfg(feature = "candle")]
pub mod candle;
pub mod factory;
pub mod http;
#[cfg(feature = "llamacpp")]
pub mod llamacpp;
pub mod logprobs;
pub mod template;
pub mod tool_format;
pub mod types;

use async_trait::async_trait;
use futures::stream::BoxStream;

use tool_format::ToolCallFormat;

#[cfg(feature = "candle")]
pub use candle::CandleEngine;
pub use factory::{
    build_engine, build_engine_with, validate, EngineBuildError, EngineConfig, EngineKind,
    HttpWarmup,
};
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
        tools: &[serde_json::Value],
    ) -> Result<String>;

    /// The tool-call syntax this engine's chat template renders tools in, if any.
    fn tool_format(&self) -> Option<&'static dyn ToolCallFormat> {
        None
    }

    /// Whether this engine can render tool declarations into its prompt.
    fn supports_tools(&self) -> bool {
        self.tool_format().is_some()
    }
}
