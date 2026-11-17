pub mod http;
pub mod types;

use async_trait::async_trait;
use futures::stream::BoxStream;

pub use http::HttpEngine;
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
}
