use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;

use super::{
    CompletionChunk, Engine, EngineError, ModelProps, Result, SamplingConfig, TokenizeResult,
};

const PROPS_PATH: &str = "/props";
const TOKENIZE_PATH: &str = "/tokenize";
const DETOKENIZE_PATH: &str = "/detokenize";

#[derive(Deserialize)]
struct TokenizeResponse {
    tokens: Vec<i64>,
}

#[derive(Deserialize)]
struct DetokenizeResponse {
    content: String,
}

/// `Engine` backed by llama.cpp's native HTTP server.
pub struct HttpEngine {
    base_url: String,
    client: reqwest::Client,
}

impl HttpEngine {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn map_transport(err: reqwest::Error) -> EngineError {
        if err.is_connect() {
            EngineError::ConnectionRefused
        } else {
            EngineError::Protocol(err.to_string())
        }
    }

    async fn read_json<T: DeserializeOwned>(response: reqwest::Response) -> Result<T> {
        let status = response.status();
        if !status.is_success() {
            return Err(EngineError::Protocol(format!("unexpected status {status}")));
        }
        let bytes = response.bytes().await.map_err(Self::map_transport)?;
        serde_json::from_slice(&bytes).map_err(|e| EngineError::Protocol(e.to_string()))
    }
}

#[async_trait]
impl Engine for HttpEngine {
    async fn props(&self) -> Result<ModelProps> {
        let response = self
            .client
            .get(self.url(PROPS_PATH))
            .send()
            .await
            .map_err(Self::map_transport)?;
        Self::read_json(response).await
    }

    async fn tokenize(&self, text: &str) -> Result<TokenizeResult> {
        let response = self
            .client
            .post(self.url(TOKENIZE_PATH))
            .json(&json!({ "content": text }))
            .send()
            .await
            .map_err(Self::map_transport)?;
        let parsed: TokenizeResponse = Self::read_json(response).await?;
        Ok(TokenizeResult {
            count: parsed.tokens.len(),
            tokens: parsed.tokens,
        })
    }

    async fn detokenize(&self, tokens: &[i64]) -> Result<String> {
        let response = self
            .client
            .post(self.url(DETOKENIZE_PATH))
            .json(&json!({ "tokens": tokens }))
            .send()
            .await
            .map_err(Self::map_transport)?;
        let parsed: DetokenizeResponse = Self::read_json(response).await?;
        Ok(parsed.content)
    }

    async fn complete(
        &self,
        _prompt: &str,
        _cfg: &SamplingConfig,
    ) -> Result<BoxStream<'static, Result<CompletionChunk>>> {
        todo!("streaming completion is implemented in Task 3")
    }
}
