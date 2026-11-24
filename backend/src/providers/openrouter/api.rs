//! OpenRouter's HTTP API: the model list and the streaming chat call, with a small SSE reader.
//!
//! The key travels only in the `Authorization` header of the chat call. It is never formatted:
//! [`OpenRouterApi`]'s `Debug` prints `***`, reqwest errors are reported `without_url()`, and
//! every error text is scrubbed of the key in case a server echoes it back.

use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One model from OpenRouter's list. Prices are USD per million tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenRouterModel {
    pub id: String,
    pub name: String,
    pub context_length: u32,
    pub prompt_price: f64,
    pub completion_price: f64,
    /// The model accepts `reasoning` (it lists it in `supported_parameters`).
    pub reasoning: bool,
}

impl OpenRouterModel {
    const F_ID: &'static str = "id";
    const F_NAME: &'static str = "name";
    const F_CONTEXT_LENGTH: &'static str = "context_length";
    const F_PRICING: &'static str = "pricing";
    const F_PROMPT: &'static str = "prompt";
    const F_COMPLETION: &'static str = "completion";
    const F_SUPPORTED: &'static str = "supported_parameters";
    const P_TOOLS: &'static str = "tools";
    const P_REASONING: &'static str = "reasoning";
    const PER_MILLION: f64 = 1_000_000.0;
    /// Prices are rounded to a billionth of a dollar per million tokens, so `"0.000003"` reads
    /// `3.0`, not `3.0000000000000004`.
    const PRICE_PRECISION: f64 = 1_000_000_000.0;

    /// The model a `/models` list `entry` describes, if it supports tools.
    fn from_entry(entry: &Value) -> Option<Self> {
        let id = entry.get(Self::F_ID)?.as_str()?.to_string();
        let supported: Vec<&str> = entry
            .get(Self::F_SUPPORTED)
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if !supported.contains(&Self::P_TOOLS) {
            return None;
        }
        let pricing = entry.get(Self::F_PRICING);
        Some(OpenRouterModel {
            name: entry
                .get(Self::F_NAME)
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_string(),
            context_length: entry
                .get(Self::F_CONTEXT_LENGTH)
                .and_then(Value::as_u64)
                .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
                .unwrap_or(0),
            prompt_price: Self::per_million(pricing.and_then(|p| p.get(Self::F_PROMPT))),
            completion_price: Self::per_million(pricing.and_then(|p| p.get(Self::F_COMPLETION))),
            reasoning: supported.contains(&Self::P_REASONING),
            id,
        })
    }

    /// A per-token price (a decimal string or a number) as USD per million tokens.
    fn per_million(price: Option<&Value>) -> f64 {
        let per_token = match price {
            Some(Value::String(text)) => text.trim().parse::<f64>().unwrap_or(0.0),
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
            _ => 0.0,
        };
        (per_token * Self::PER_MILLION * Self::PRICE_PRECISION).round() / Self::PRICE_PRECISION
    }
}

/// Why an OpenRouter call failed. `Display` gives the user-visible text:
/// `openrouter: {status} {message}` or `openrouter: {error}`.
#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    /// An HTTP status, or a stream error object's code, with OpenRouter's message.
    Status { status: String, message: String },
    /// No answer, a broken stream or an unreadable one.
    Transport(String),
}

impl ApiError {
    const PREFIX: &'static str = "openrouter: ";
    const BAD_REQUEST: &'static str = "400";
    /// Phrases of a "prompt too long for the context" message, lowercase.
    const CONTEXT_PHRASES: [&'static str; 3] =
        ["context length", "context_length", "context window"];
    const SOURCE_SEPARATOR: &'static str = ": ";
    const F_ERROR: &'static str = "error";
    const F_MESSAGE: &'static str = "message";
    const F_CODE: &'static str = "code";
    const STREAM_ERROR: &'static str = "stream error";

    /// A 400 saying the prompt does not fit the model's context.
    pub fn is_context_length(&self) -> bool {
        match self {
            ApiError::Status { status, message } => {
                let message = message.to_lowercase();
                status == Self::BAD_REQUEST
                    && Self::CONTEXT_PHRASES.iter().any(|p| message.contains(p))
            }
            ApiError::Transport(_) => false,
        }
    }

    /// The error of a non-2xx response: OpenRouter's `error.message` when the body has one,
    /// otherwise the status's reason phrase. Nothing else from the body is kept.
    fn of_status(status: reqwest::StatusCode, body: &str, key: &ApiKey) -> Self {
        let message = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|v| {
                v.get(Self::F_ERROR)?
                    .get(Self::F_MESSAGE)?
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_else(|| status.canonical_reason().unwrap_or_default().to_string());
        ApiError::Status {
            status: status.as_u16().to_string(),
            message: key.scrub(message),
        }
    }

    /// A stream event's `error` object (`{code?, message?}`).
    pub(crate) fn of_stream_object(error: &Value, key: &ApiKey) -> Self {
        let message = error
            .get(Self::F_MESSAGE)
            .and_then(Value::as_str)
            .unwrap_or(Self::STREAM_ERROR)
            .to_string();
        let message = key.scrub(message);
        match error.get(Self::F_CODE) {
            Some(Value::String(code)) => ApiError::Status {
                status: key.scrub(code.clone()),
                message,
            },
            Some(Value::Number(code)) => ApiError::Status {
                status: code.to_string(),
                message,
            },
            _ => ApiError::Transport(message),
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Status { status, message } => {
                write!(f, "{}{status} {message}", Self::PREFIX)
            }
            ApiError::Transport(error) => write!(f, "{}{error}", Self::PREFIX),
        }
    }
}

impl std::error::Error for ApiError {}

/// The API key, kept out of every printed or reported text.
#[derive(Clone)]
pub(crate) struct ApiKey(Option<String>);

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(Self::MASK)
    }
}

impl ApiKey {
    const MASK: &'static str = "***";

    /// `text` with every occurrence of the key replaced by `***`.
    pub(crate) fn scrub(&self, text: String) -> String {
        match &self.0 {
            Some(key) if text.contains(key.as_str()) => text.replace(key.as_str(), Self::MASK),
            _ => text,
        }
    }

    /// A reqwest error's text and its sources', without the URL and scrubbed of the key.
    fn transport(&self, error: reqwest::Error) -> ApiError {
        let error = error.without_url();
        let mut text = error.to_string();
        let mut source = std::error::Error::source(&error);
        while let Some(cause) = source {
            text.push_str(ApiError::SOURCE_SEPARATOR);
            text.push_str(&cause.to_string());
            source = cause.source();
        }
        ApiError::Transport(self.scrub(text))
    }
}

/// Splits a `text/event-stream` byte stream into events' data. Lines end at `\n` (a trailing
/// `\r` is dropped); a blank line ends an event; `data:` lines are joined with `\n`; comment
/// lines (starting with `:`) and other fields are ignored. A `[DONE]` event ends the stream:
/// it is not returned, and nothing after it is read. Bytes are buffered per line, so a line
/// (or a UTF-8 character) split across chunks is reassembled.
#[derive(Debug, Default)]
pub struct SseReader {
    line: Vec<u8>,
    data: Vec<String>,
    done: bool,
}

impl SseReader {
    const DATA_FIELD: &'static [u8] = b"data:";
    const COMMENT: u8 = b':';
    const NEWLINE: u8 = b'\n';
    const CARRIAGE_RETURN: u8 = b'\r';
    const SPACE: u8 = b' ';
    const DONE: &'static str = "[DONE]";
    const DATA_JOIN: &'static str = "\n";

    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds bytes; returns the data of every event they complete.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut events = Vec::new();
        for &byte in chunk {
            if self.done {
                break;
            }
            if byte == Self::NEWLINE {
                let line = std::mem::take(&mut self.line);
                self.end_line(line, &mut events);
            } else {
                self.line.push(byte);
            }
        }
        events
    }

    /// The input ended: an unterminated last line and event are completed.
    pub fn finish(&mut self) -> Vec<String> {
        let mut events = Vec::new();
        if !self.done && !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            self.end_line(line, &mut events);
        }
        if !self.done {
            self.end_line(Vec::new(), &mut events);
        }
        events
    }

    /// Whether `[DONE]` arrived.
    pub fn is_done(&self) -> bool {
        self.done
    }

    fn end_line(&mut self, mut line: Vec<u8>, events: &mut Vec<String>) {
        if line.last() == Some(&Self::CARRIAGE_RETURN) {
            line.pop();
        }
        if line.is_empty() {
            if self.data.is_empty() {
                return;
            }
            let event = self.data.join(Self::DATA_JOIN);
            self.data.clear();
            if event == Self::DONE {
                self.done = true;
            } else {
                events.push(event);
            }
            return;
        }
        if line[0] == Self::COMMENT {
            return;
        }
        if let Some(mut value) = line.strip_prefix(Self::DATA_FIELD) {
            if value.first() == Some(&Self::SPACE) {
                value = &value[1..];
            }
            self.data.push(String::from_utf8_lossy(value).into_owned());
        }
    }
}

/// The events of one streaming chat call, as JSON.
pub struct SseStream {
    bytes: BoxStream<'static, reqwest::Result<Bytes>>,
    reader: SseReader,
    ready: VecDeque<String>,
    ended: bool,
    key: ApiKey,
}

impl SseStream {
    const MALFORMED_EVENT: &'static str = "malformed stream event";

    /// The next event; `None` once the stream ended (after `[DONE]`, or when the connection
    /// closed). A broken connection or an event that is not JSON is an `Err`, after which the
    /// stream ends.
    pub async fn next_event(&mut self) -> Option<Result<Value, ApiError>> {
        loop {
            if let Some(event) = self.ready.pop_front() {
                let parsed = serde_json::from_str(&event);
                if parsed.is_err() {
                    self.ended = true;
                    self.ready.clear();
                }
                return Some(
                    parsed.map_err(|_| ApiError::Transport(Self::MALFORMED_EVENT.to_string())),
                );
            }
            if self.ended || self.reader.is_done() {
                return None;
            }
            match self.bytes.next().await {
                Some(Ok(bytes)) => self.ready.extend(self.reader.push(&bytes)),
                Some(Err(error)) => {
                    self.ended = true;
                    return Some(Err(self.key.transport(error)));
                }
                None => {
                    self.ended = true;
                    self.ready.extend(self.reader.finish());
                }
            }
        }
    }

    /// Whether the stream ended with `[DONE]`.
    pub fn saw_done(&self) -> bool {
        self.reader.is_done()
    }
}

/// OpenRouter's HTTP API under one base url and key.
pub struct OpenRouterApi {
    base_url: String,
    key: ApiKey,
    client: reqwest::Client,
}

impl fmt::Debug for OpenRouterApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct(Self::DEBUG_NAME)
            .field(Self::DEBUG_BASE_URL, &self.base_url)
            .field(Self::DEBUG_KEY, &self.key)
            .finish()
    }
}

impl OpenRouterApi {
    pub const DEFAULT_BASE_URL: &'static str = "https://openrouter.ai/api/v1";
    const MODELS_PATH: &'static str = "/models";
    const CHAT_PATH: &'static str = "/chat/completions";
    const TITLE_HEADER: &'static str = "X-Title";
    const TITLE: &'static str = "Pragmatic LLM Harness";
    const F_DATA: &'static str = "data";
    const BAD_MODEL_LIST: &'static str = "the model list is not valid JSON";
    const URL_SEPARATOR: char = '/';
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
    /// Longest silence between two reads of a response (OpenRouter sends keep-alive comments
    /// while a model thinks).
    const READ_TIMEOUT: Duration = Duration::from_secs(180);
    const DEBUG_NAME: &'static str = "OpenRouterApi";
    const DEBUG_BASE_URL: &'static str = "base_url";
    const DEBUG_KEY: &'static str = "key";

    pub fn new(base_url: String, key: Option<String>) -> Self {
        OpenRouterApi {
            base_url: base_url.trim_end_matches(Self::URL_SEPARATOR).to_string(),
            key: ApiKey(key.filter(|k| !k.is_empty())),
            client: reqwest::Client::builder()
                .connect_timeout(Self::CONNECT_TIMEOUT)
                .read_timeout(Self::READ_TIMEOUT)
                .build()
                .expect("reqwest client construction"),
        }
    }

    /// Whether a key is set.
    pub fn has_key(&self) -> bool {
        self.key.0.is_some()
    }

    /// The models that support tools, in OpenRouter's order. The list needs no key, so none
    /// is sent.
    pub async fn models(&self) -> Result<Vec<OpenRouterModel>, String> {
        let url = format!("{}{}", self.base_url, Self::MODELS_PATH);
        let response = self
            .client
            .get(url)
            .header(Self::TITLE_HEADER, Self::TITLE)
            .send()
            .await
            .map_err(|e| self.key.transport(e).to_string())?;
        let response = self.check(response).await.map_err(|e| e.to_string())?;
        let bytes = response
            .bytes()
            .await
            .map_err(|e| self.key.transport(e).to_string())?;
        let list: Value = serde_json::from_slice(&bytes)
            .map_err(|_| ApiError::Transport(Self::BAD_MODEL_LIST.into()).to_string())?;
        Ok(list
            .get(Self::F_DATA)
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(OpenRouterModel::from_entry)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Sends `body` to `/chat/completions` and returns its event stream. A non-2xx answer is
    /// `ApiError::Status`; no answer at all is `ApiError::Transport`.
    pub async fn stream_chat(&self, body: Value) -> Result<SseStream, ApiError> {
        let url = format!("{}{}", self.base_url, Self::CHAT_PATH);
        let mut request = self
            .client
            .post(url)
            .header(Self::TITLE_HEADER, Self::TITLE)
            .json(&body);
        if let Some(key) = &self.key.0 {
            request = request.bearer_auth(key);
        }
        let response = request.send().await.map_err(|e| self.key.transport(e))?;
        let response = self.check(response).await?;
        Ok(SseStream {
            bytes: response.bytes_stream().boxed(),
            reader: SseReader::new(),
            ready: VecDeque::new(),
            ended: false,
            key: self.key.clone(),
        })
    }

    /// The key holder, for scrubbing errors found in a stream's events.
    pub(crate) fn key(&self) -> &ApiKey {
        &self.key
    }

    /// Passes a 2xx response through; anything else becomes `ApiError::Status`.
    async fn check(&self, response: reqwest::Response) -> Result<reqwest::Response, ApiError> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = response.text().await.unwrap_or_default();
        Err(ApiError::of_status(status, &body, &self.key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::openrouter::{Canned, FakeHttp};
    use serde_json::json;

    struct Fixtures;

    impl Fixtures {
        const KEY: &'static str = "sk-or-test-secret-1234";

        fn models_body() -> String {
            json!({"data": [
                {
                    "id": "anthropic/claude-x",
                    "name": "Claude X",
                    "context_length": 200000,
                    "pricing": {"prompt": "0.000003", "completion": "0.000015"},
                    "supported_parameters": ["tools", "reasoning", "temperature"]
                },
                {
                    "id": "plain/no-tools",
                    "name": "No Tools",
                    "context_length": 8192,
                    "pricing": {"prompt": "0", "completion": "0"},
                    "supported_parameters": ["temperature"]
                },
                {
                    "id": "google/gemini-y",
                    "name": "Gemini Y",
                    "context_length": 1000000,
                    "pricing": {"prompt": 0.0000001, "completion": "0.0000004"},
                    "supported_parameters": ["tools"]
                }
            ]})
            .to_string()
        }
    }

    #[tokio::test]
    async fn models_filtered_to_tools_and_prices_per_million() {
        let (base, mut requests) =
            FakeHttp::serve(vec![Canned::new(200, &[&Fixtures::models_body()])]).await;
        let api = OpenRouterApi::new(base, Some(Fixtures::KEY.into()));
        let models = api.models().await.expect("models");
        assert_eq!(
            models,
            vec![
                OpenRouterModel {
                    id: "anthropic/claude-x".into(),
                    name: "Claude X".into(),
                    context_length: 200000,
                    prompt_price: 3.0,
                    completion_price: 15.0,
                    reasoning: true,
                },
                OpenRouterModel {
                    id: "google/gemini-y".into(),
                    name: "Gemini Y".into(),
                    context_length: 1000000,
                    prompt_price: 0.1,
                    completion_price: 0.4,
                    reasoning: false,
                },
            ]
        );
        let request = requests.recv().await.expect("request");
        assert!(request.head.starts_with("GET /models "), "{}", request.head);
        // The list needs no key, so none is sent.
        assert_eq!(request.header("authorization"), None);
    }

    #[test]
    fn sse_reader_splits_events_and_skips_comments() {
        let mut reader = SseReader::new();
        let mut events = reader.push(b": OPENROUTER PROCESSING\n\ndata: {\"a\"");
        assert!(events.is_empty());
        events.extend(reader.push(b":1}\r\n\r\n: OPENROUTER PROCESSING\n\nda"));
        events.extend(reader.push(b"ta: {\"b\":2}\n\ndata: [DONE]\n\ndata: {\"late\":0}\n\n"));
        assert_eq!(
            events,
            vec![r#"{"a":1}"#.to_string(), r#"{"b":2}"#.to_string()]
        );
        assert!(reader.is_done());
    }

    #[tokio::test]
    async fn sse_lines_split_across_chunks_and_comments() {
        let parts = [
            ": OPENROUTER PROCESSING\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"He",
            "llo\"}}]}\n\n: OPENROUTER PROCESSING\n",
            "\ndata: {\"choices\":[{\"delta\":{\"content\":\"!\"}}]}\n",
            "\ndata: [DONE]\n\n",
        ];
        let (base, _requests) = FakeHttp::serve(vec![Canned::new(200, &parts)]).await;
        let api = OpenRouterApi::new(base, Some(Fixtures::KEY.into()));
        let mut stream = api.stream_chat(json!({})).await.expect("stream");
        let mut events = Vec::new();
        while let Some(event) = stream.next_event().await {
            events.push(event.expect("event"));
        }
        assert_eq!(
            events,
            vec![
                json!({"choices":[{"delta":{"content":"Hello"}}]}),
                json!({"choices":[{"delta":{"content":"!"}}]}),
            ]
        );
        assert!(stream.saw_done());
    }

    #[test]
    fn api_debug_hides_the_key() {
        let api = OpenRouterApi::new("http://x".into(), Some(Fixtures::KEY.into()));
        let printed = format!("{api:?}");
        assert!(!printed.contains(Fixtures::KEY), "{printed}");
        assert!(printed.contains("***"), "{printed}");
    }
}
