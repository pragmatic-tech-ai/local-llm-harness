use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

use super::{
    CompletionChunk, Engine, EngineError, ModelProps, Result, SamplingConfig, TokenizeResult,
};

const PROPS_PATH: &str = "/props";
const TOKENIZE_PATH: &str = "/tokenize";
const DETOKENIZE_PATH: &str = "/detokenize";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const ERROR_BODY_MAX_CHARS: usize = 500;

#[derive(Deserialize)]
struct TokenizeResponse {
    tokens: Vec<i64>,
}

#[derive(Deserialize)]
struct DetokenizeResponse {
    content: String,
}

/// `Engine` backed by llama.cpp's native HTTP server.
///
/// The client is built without TLS support (local plain-http use only), so an
/// `https://` base URL will fail at runtime. Only the connect phase is bounded by a
/// timeout; there is deliberately no overall timeout so streaming responses can be long-lived.
pub struct HttpEngine {
    base_url: String,
    client: reqwest::Client,
}

impl HttpEngine {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .expect("reqwest client construction"),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// Connect-phase failures generally (refused, DNS failure, unreachable host, connect
    /// timeout) all map to `ConnectionRefused`; the harness only needs "server not reachable".
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
            let body = response.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(ERROR_BODY_MAX_CHARS).collect();
            return Err(EngineError::Protocol(format!(
                "unexpected status {status}: {snippet}"
            )));
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
