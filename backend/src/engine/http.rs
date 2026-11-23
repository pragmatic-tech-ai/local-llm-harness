use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::time::Duration;

use super::{
    ChatMessage, CompletionChunk, Engine, EngineError, ModelProps, Result, SamplingConfig,
    TokenInfo, TokenizeResult,
};

const PROPS_PATH: &str = "/props";
const TOKENIZE_PATH: &str = "/tokenize";
const COMPLETION_PATH: &str = "/completion";
const DETOKENIZE_PATH: &str = "/detokenize";
const APPLY_TEMPLATE_PATH: &str = "/apply-template";
/// Number of top alternatives per token requested via `n_probs`.
const TOP_LOGPROBS: u32 = 5;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const ERROR_BODY_MAX_CHARS: usize = 500;

#[derive(Deserialize)]
struct TokenizeResponse {
    tokens: Vec<i64>,
}

#[derive(Deserialize)]
struct ApplyTemplateResponse {
    prompt: String,
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

    /// Passes a successful response through; turns any non-2xx into a `Protocol` error that
    /// carries a bounded snippet of the body.
    async fn check_status(response: reqwest::Response) -> Result<reqwest::Response> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = response.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(ERROR_BODY_MAX_CHARS).collect();
        Err(EngineError::Protocol(format!(
            "unexpected status {status}: {snippet}"
        )))
    }

    async fn read_json<T: DeserializeOwned>(response: reqwest::Response) -> Result<T> {
        let response = Self::check_status(response).await?;
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

    async fn apply_template(
        &self,
        messages: &[ChatMessage],
        add_generation_prompt: bool,
        enable_thinking: bool,
        tools: &[serde_json::Value],
    ) -> Result<String> {
        if !tools.is_empty() {
            return Err(EngineError::Protocol(
                "tools are not supported by the http engine".into(),
            ));
        }
        let body = json!({
            "messages": messages,
            "add_generation_prompt": add_generation_prompt,
            "chat_template_kwargs": { "enable_thinking": enable_thinking },
        });
        let response = self
            .client
            .post(self.url(APPLY_TEMPLATE_PATH))
            .json(&body)
            .send()
            .await
            .map_err(Self::map_transport)?;
        let parsed: ApplyTemplateResponse = Self::read_json(response).await?;
        Ok(parsed.prompt)
    }

    async fn complete(
        &self,
        prompt: &str,
        cfg: &SamplingConfig,
    ) -> Result<BoxStream<'static, Result<CompletionChunk>>> {
        let body = json!({
            "prompt": prompt,
            "stream": true,
            "cache_prompt": true,
            "n_probs": TOP_LOGPROBS,
            "temperature": cfg.temperature,
            "top_p": cfg.top_p,
            "top_k": cfg.top_k,
            "repeat_penalty": cfg.repeat_penalty,
            "seed": cfg.seed,
            "n_predict": cfg.max_tokens,
        });
        let response = self
            .client
            .post(self.url(COMPLETION_PATH))
            .json(&body)
            .send()
            .await
            .map_err(Self::map_transport)?;
        let response = Self::check_status(response).await?;
        // Known limitation: there is no read/idle timeout, so a server that stalls without
        // closing the connection hangs the stream. A fixed timeout would kill valid streams from
        // a "thinking" model that pauses mid-generation; a stall-aware timeout is deferred to the
        // agent-loop phase.
        let state = StreamState {
            bytes: response.bytes_stream().boxed(),
            parser: SseParser::default(),
            pending: VecDeque::new(),
            finished: false,
        };
        Ok(futures::stream::unfold(state, StreamState::next_item).boxed())
    }
}

type ByteStream = BoxStream<'static, std::result::Result<bytes::Bytes, reqwest::Error>>;

struct StreamState {
    bytes: ByteStream,
    parser: SseParser,
    pending: VecDeque<Result<CompletionChunk>>,
    finished: bool,
}

impl StreamState {
    async fn next_item(mut self) -> Option<(Result<CompletionChunk>, Self)> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                if item.as_ref().map_or(true, |c| c.stop) {
                    self.finished = true;
                    self.pending.clear();
                }
                return Some((item, self));
            }
            if self.finished {
                return None;
            }
            match self.bytes.next().await {
                Some(Ok(chunk)) => self.pending.extend(self.parser.feed(&chunk)),
                Some(Err(_)) => self.pending.push_back(Err(EngineError::StreamInterrupted)),
                None => {
                    self.pending.extend(self.parser.finish());
                    // Body ended without a stop chunk: the server went away mid-generation.
                    self.pending.push_back(Err(EngineError::StreamInterrupted));
                }
            }
        }
    }
}

/// Incremental SSE parser. Bytes are buffered until a full line is available and only then
/// decoded, so a multi-byte UTF-8 character split across network chunks is never mangled.
#[derive(Default)]
struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    fn feed(&mut self, data: &[u8]) -> Vec<Result<CompletionChunk>> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            if let Some(item) = Self::parse_line(&line) {
                out.push(item);
            }
        }
        out
    }

    /// Flush a trailing line that had no terminating newline.
    fn finish(&mut self) -> Vec<Result<CompletionChunk>> {
        let line = std::mem::take(&mut self.buf);
        Self::parse_line(&line).into_iter().collect()
    }

    fn parse_line(line: &[u8]) -> Option<Result<CompletionChunk>> {
        let text = match std::str::from_utf8(line) {
            Ok(t) => t.trim_end_matches(['\n', '\r']),
            // Lines are decoded only once complete, and a character cannot span two complete SSE
            // events (each payload is standalone JSON), so for a correct server this is
            // effectively unreachable; treat it as a terminal protocol error.
            Err(e) => return Some(Err(EngineError::Protocol(format!("invalid utf-8: {e}")))),
        };
        let payload = text.strip_prefix("data:")?.trim_start();
        if payload.is_empty() || payload == "[DONE]" {
            return None;
        }
        Some(Self::parse_payload(payload))
    }

    fn parse_payload(payload: &str) -> Result<CompletionChunk> {
        let v: Value =
            serde_json::from_str(payload).map_err(|e| EngineError::Protocol(e.to_string()))?;
        let text = v
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let stop = v.get("stop").and_then(Value::as_bool).unwrap_or(false);
        let tokens = v
            .get("completion_probabilities")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Self::parse_token).collect())
            .unwrap_or_default();
        Ok(CompletionChunk { text, tokens, stop })
    }

    /// Accepts both llama.cpp shapes: `{token, logprob, top_logprobs:[{token, logprob}]}` and the
    /// older `{content, probs:[{tok_str, prob}]}`.
    fn parse_token(e: &Value) -> Option<TokenInfo> {
        let token = e
            .get("token")
            .or_else(|| e.get("content"))?
            .as_str()?
            .to_string();
        let top: Vec<(String, f32)> = e
            .get("top_logprobs")
            .or_else(|| e.get("probs"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|t| {
                        let name = t.get("token").or_else(|| t.get("tok_str"))?.as_str()?;
                        Some((name.to_string(), Self::logprob_of(t)?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Older servers omit the chosen token's own probability; recover it from the top list.
        // If the sampled token is absent from the top-N list (legacy shape), it was less likely
        // than every listed alternative: report the lowest listed logprob as an upper bound
        // (f32::MIN when no list exists) rather than dropping the token.
        let logprob = Self::logprob_of(e)
            .or_else(|| top.iter().find(|(t, _)| *t == token).map(|(_, lp)| *lp))
            .or_else(|| top.iter().map(|(_, lp)| *lp).reduce(f32::min))
            .unwrap_or(f32::MIN);
        Some(TokenInfo {
            token,
            logprob,
            top,
        })
    }

    fn logprob_of(v: &Value) -> Option<f32> {
        if let Some(lp) = v.get("logprob").and_then(Value::as_f64) {
            return Some(lp as f32);
        }
        v.get("prob").and_then(Value::as_f64).map(|p| p.ln() as f32)
    }
}
