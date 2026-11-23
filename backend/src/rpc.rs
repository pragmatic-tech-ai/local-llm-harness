//! JSON-RPC over WebSocket.
//!
//! Wire envelope (one JSON object per text frame):
//! - request:      `{"id": N, "method": "...", "params": {...}}`
//! - response:     `{"id": N, "result": ...}` or `{"id": N, "error": {"code": C, "message": "..."}}`
//! - notification: `{"method": "...", "params": {...}}` (server -> client, no `id`)
//!
//! `send_turn` streams `turn_started`, `turn_chunk`*, then `turn_done` | `turn_error`
//! (or `turn_started` + `turn_rejected`); the request's own response is sent last.
//!
//! `generate` is the stateless variant: the caller supplies the whole conversation (and
//! optionally tools). It streams the same notifications, plus a `tool_call` note as soon as each
//! call completes, and ends with `turn_done {content, tool_calls}`.

use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::engine::tool_format::{GemmaToolParser, ParseEvent, ParsedCall};
use crate::engine::{ChatMessage, Engine, ModelProps, SamplingConfig};
use crate::session::{ChatSession, ContextFill, TurnRejected};

const ERR_PARSE: i64 = -32700;
const ERR_METHOD_NOT_FOUND: i64 = -32601;
const ERR_INVALID_PARAMS: i64 = -32602;
const ERR_INTERNAL: i64 = -32603;

type Ws = WebSocketStream<TcpStream>;
type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Deserialize)]
struct Request {
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Deserialize)]
struct TokenizeParams {
    text: String,
}

#[derive(Debug, Deserialize)]
struct SetSamplingParams {
    config: SamplingConfig,
}

#[derive(Debug, Deserialize)]
struct SendTurnParams {
    content: String,
    #[serde(default)]
    enable_thinking: bool,
}

#[derive(Debug, Deserialize)]
struct GenerateParams {
    messages: Vec<ChatMessage>,
    #[serde(default)]
    tools: Option<Vec<Value>>,
    #[serde(default)]
    enable_thinking: bool,
    #[serde(default)]
    sampling: Option<SamplingConfig>,
}

#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: impl ToString) -> Self {
        Self {
            code,
            message: message.to_string(),
        }
    }

    fn internal(message: impl ToString) -> Self {
        Self::new(ERR_INTERNAL, message)
    }
}

/// Why a request failed: a JSON-RPC error (reported to the client as an error response) or a
/// transport failure (the socket is dead, so the connection ends without a reply).
enum Failure {
    Rpc(RpcError),
    Transport(BoxError),
}

impl From<RpcError> for Failure {
    fn from(e: RpcError) -> Self {
        Self::Rpc(e)
    }
}

/// Accepts WebSocket connections forever; each connection gets its own `ChatSession`.
pub async fn serve(
    listener: TcpListener,
    engine: Arc<dyn Engine>,
    props: ModelProps,
    system: Option<String>,
) {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                eprintln!("accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        let connection = Connection {
            engine: Arc::clone(&engine),
            props: props.clone(),
            session: ChatSession::new(Arc::clone(&engine), props.clone(), system.clone()),
            sampling: SamplingConfig::default(),
        };
        tokio::spawn(async move {
            if let Err(e) = connection.run(stream).await {
                eprintln!("connection ended: {e}");
            }
        });
    }
}

/// One client connection. Requests are handled sequentially (single user, one turn at a time).
struct Connection {
    engine: Arc<dyn Engine>,
    props: ModelProps,
    session: ChatSession,
    sampling: SamplingConfig,
}

impl Connection {
    const TOOLS_UNSUPPORTED: &'static str = "tools are not supported by this engine";

    async fn run(mut self, stream: TcpStream) -> Result<(), BoxError> {
        let mut ws = tokio_tungstenite::accept_async(stream).await?;
        while let Some(frame) = ws.next().await {
            let Message::Text(text) = frame? else {
                continue;
            };
            self.handle_frame(&mut ws, text.as_str()).await?;
        }
        Ok(())
    }

    async fn handle_frame(&mut self, ws: &mut Ws, text: &str) -> Result<(), BoxError> {
        let request: Request = match serde_json::from_str(text) {
            Ok(r) => r,
            Err(e) => {
                let err = RpcError::new(ERR_PARSE, e);
                return Self::send_json(ws, Self::error_frame(Value::Null, &err)).await;
            }
        };
        let frame = match self.dispatch(ws, &request).await {
            Ok(result) => json!({"id": request.id, "result": result}),
            Err(Failure::Rpc(err)) => Self::error_frame(request.id, &err),
            Err(Failure::Transport(e)) => return Err(e),
        };
        Self::send_json(ws, frame).await
    }

    fn error_frame(id: Value, err: &RpcError) -> Value {
        json!({"id": id, "error": {"code": err.code, "message": err.message}})
    }

    async fn dispatch(&mut self, ws: &mut Ws, req: &Request) -> Result<Value, Failure> {
        match req.method.as_str() {
            // Send the clean client shape {n_ctx, model_name}; ModelProps's own Serialize goes
            // `into RawProps` (the llama-server /props shape) for round-tripping, which is NOT
            // what the frontend wants.
            "model_props" => Ok(json!({
                "n_ctx": self.props.n_ctx,
                "model_name": self.props.model_name,
                "supports_tools": self.engine.supports_tools(),
            })),
            "tokenize" => {
                let p: TokenizeParams = Self::params(req)?;
                let result = self
                    .engine
                    .tokenize(&p.text)
                    .await
                    .map_err(RpcError::internal)?;
                Ok(Self::to_value(&result)?)
            }
            "set_sampling" => {
                let p: SetSamplingParams = Self::params(req)?;
                self.sampling = p.config;
                Ok(Value::Null)
            }
            "send_turn" => {
                let p: SendTurnParams = Self::params(req)?;
                self.send_turn(ws, p).await?;
                Ok(Value::Null)
            }
            "generate" => {
                let p: GenerateParams = Self::params(req)?;
                self.generate(ws, p).await?;
                Ok(Value::Null)
            }
            other => {
                Err(RpcError::new(ERR_METHOD_NOT_FOUND, format!("unknown method: {other}")).into())
            }
        }
    }

    fn params<T: DeserializeOwned>(req: &Request) -> Result<T, RpcError> {
        serde_json::from_value(req.params.clone()).map_err(|e| RpcError::new(ERR_INVALID_PARAMS, e))
    }

    fn to_value<T: Serialize>(v: &T) -> Result<Value, RpcError> {
        serde_json::to_value(v).map_err(RpcError::internal)
    }

    async fn send_turn(&mut self, ws: &mut Ws, p: SendTurnParams) -> Result<(), Failure> {
        let turn = match self
            .session
            .begin_turn(&p.content, &self.sampling, p.enable_thinking)
            .await
        {
            Ok(t) => t,
            Err(failure) => {
                if let (Some(prompt), Some(fill)) = (&failure.prompt, failure.fill) {
                    Self::notify(ws, "turn_started", Self::started(prompt, fill)).await?;
                }
                return match failure.reason {
                    TurnRejected::WindowFull { needed, total } => {
                        let params = json!({"needed": needed, "total": total});
                        Self::notify(ws, "turn_rejected", params).await
                    }
                    TurnRejected::Engine(e) => Self::turn_error(ws, e).await,
                };
            }
        };
        Self::notify(ws, "turn_started", Self::started(&turn.prompt, turn.fill)).await?;
        let mut stream = turn.stream;
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    let params =
                        json!({"text": chunk.text, "tokens": chunk.tokens, "stop": chunk.stop});
                    Self::notify(ws, "turn_chunk", params).await?;
                }
                Err(e) => {
                    return Self::notify(ws, "turn_error", json!({"message": e.to_string()})).await;
                }
            }
        }
        Self::notify(ws, "turn_done", json!({})).await
    }

    /// Stateless turn: renders `p.messages` (with `p.tools`), checks the window, streams the
    /// reply and parses Gemma 4 tool calls out of it. Nothing is stored on the connection.
    ///
    /// Wire contract:
    /// - Non-empty `tools` on an engine whose `supports_tools()` is false yields a single
    ///   `turn_error {message: "tools are not supported by this engine"}` before anything else
    ///   (no `turn_started`, no render, no `complete`).
    /// - Every engine chunk is relayed raw as `turn_chunk {text, tokens, stop}`, including the
    ///   chunk that carries the response marker; that chunk keeps the engine's own `stop` flag
    ///   (normally false, since the engine is cut off mid-stream). Chunks after it are never
    ///   relayed. Clients must not treat `stop` as the end of the turn: `turn_done` (or
    ///   `turn_rejected` / `turn_error`) is what ends it.
    async fn generate(&mut self, ws: &mut Ws, p: GenerateParams) -> Result<(), Failure> {
        let cfg = p.sampling.unwrap_or_else(|| self.sampling.clone());
        let tools = p.tools.unwrap_or_default();
        if !tools.is_empty() && !self.engine.supports_tools() {
            return Self::turn_error(ws, Self::TOOLS_UNSUPPORTED).await;
        }
        let prompt = match self
            .engine
            .apply_template(&p.messages, true, p.enable_thinking, &tools)
            .await
        {
            Ok(prompt) => prompt,
            Err(e) => return Self::turn_error(ws, e).await,
        };
        let used = match self.engine.tokenize(&prompt).await {
            Ok(t) => t.count,
            Err(e) => return Self::turn_error(ws, e).await,
        };
        let fill = match ContextFill::check(used, self.props.n_ctx, cfg.max_tokens) {
            Ok(fill) => fill,
            Err((fill, TurnRejected::WindowFull { needed, total })) => {
                Self::notify(ws, "turn_started", Self::started(&prompt, fill)).await?;
                let params = json!({"needed": needed, "total": total});
                return Self::notify(ws, "turn_rejected", params).await;
            }
            Err((_, TurnRejected::Engine(e))) => return Self::turn_error(ws, e).await,
        };
        Self::notify(ws, "turn_started", Self::started(&prompt, fill)).await?;
        let mut stream = match self.engine.complete(&prompt, &cfg).await {
            Ok(stream) => stream,
            Err(e) => return Self::turn_error(ws, e).await,
        };

        let mut parser = GemmaToolParser::new();
        while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(chunk) => chunk,
                Err(e) => return Self::turn_error(ws, e).await,
            };
            // The chunk is relayed raw (model mechanics stay visible), even the one carrying
            // the response marker; nothing after it is relayed.
            let params = json!({"text": chunk.text, "tokens": chunk.tokens, "stop": chunk.stop});
            Self::notify(ws, "turn_chunk", params).await?;
            let mut response_marker = false;
            for event in parser.push(&chunk.text) {
                match event {
                    ParseEvent::Call(call) => Self::notify_call(ws, &call).await?,
                    ParseEvent::ResponseMarker => response_marker = true,
                }
            }
            if response_marker {
                // Dropping the stream stops the engine: the model is inventing tool output.
                break;
            }
        }
        drop(stream);
        for event in parser.finish() {
            if let ParseEvent::Call(call) = event {
                Self::notify_call(ws, &call).await?;
            }
        }
        let done = json!({"content": parser.content(), "tool_calls": parser.calls()});
        Self::notify(ws, "turn_done", done).await
    }

    async fn notify_call(ws: &mut Ws, call: &ParsedCall) -> Result<(), Failure> {
        let params = Self::to_value(call)?;
        Self::notify(ws, "tool_call", params).await
    }

    async fn turn_error(ws: &mut Ws, e: impl ToString) -> Result<(), Failure> {
        Self::notify(ws, "turn_error", json!({"message": e.to_string()})).await
    }

    fn started(prompt: &str, fill: ContextFill) -> Value {
        json!({"prompt": prompt, "prompt_tokens": fill.used, "n_ctx": fill.total})
    }

    async fn notify(ws: &mut Ws, method: &str, params: Value) -> Result<(), Failure> {
        Self::send_json(ws, json!({"method": method, "params": params}))
            .await
            .map_err(Failure::Transport)
    }

    async fn send_json(ws: &mut Ws, value: Value) -> Result<(), BoxError> {
        ws.send(Message::text(value.to_string())).await?;
        Ok(())
    }
}

/// Scripted engine and WebSocket client for exercising `serve` end to end; reused by other
/// modules' tests (`crate::rpc::test_support`).
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::engine::{CompletionChunk, TokenizeResult};
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tokio_tungstenite::MaybeTlsStream;

    /// An `Engine` that replays scripted chunks. `apply_template` records the `tools` it is
    /// given and returns `"PROMPT"`; `tokenize` reports a configurable count.
    pub(crate) struct FakeEngine {
        chunks: Vec<String>,
        token_count: usize,
        supports_tools: bool,
        recorded_tools: Mutex<Vec<Vec<Value>>>,
        pulled: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
    }

    /// The lazily produced reply stream: counts every chunk pulled and flags its own drop.
    struct LazyChunks {
        chunks: Vec<String>,
        index: usize,
        pulled: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
    }

    impl LazyChunks {
        fn next_chunk(mut self) -> Option<(crate::engine::Result<CompletionChunk>, Self)> {
            let text = self.chunks.get(self.index)?.clone();
            let stop = self.index + 1 == self.chunks.len();
            self.index += 1;
            self.pulled.fetch_add(1, Ordering::SeqCst);
            let chunk = CompletionChunk {
                text,
                tokens: vec![],
                stop,
            };
            Some((Ok(chunk), self))
        }
    }

    impl Drop for LazyChunks {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    impl FakeEngine {
        /// A fake that streams `chunks` (the last one flagged `stop`), tokenizes every text to
        /// 10 tokens and reports no tool support.
        pub(crate) fn new(chunks: &[&str]) -> Self {
            Self {
                chunks: chunks.iter().map(|c| c.to_string()).collect(),
                token_count: 10,
                supports_tools: false,
                recorded_tools: Mutex::new(Vec::new()),
                pulled: Arc::new(AtomicUsize::new(0)),
                dropped: Arc::new(AtomicBool::new(false)),
            }
        }

        /// How many chunks the consumer has pulled from the reply stream.
        pub(crate) fn pulled_chunks(&self) -> usize {
            self.pulled.load(Ordering::SeqCst)
        }

        /// Whether the reply stream has been dropped.
        pub(crate) fn stream_dropped(&self) -> bool {
            self.dropped.load(Ordering::SeqCst)
        }

        pub(crate) fn with_token_count(mut self, token_count: usize) -> Self {
            self.token_count = token_count;
            self
        }

        pub(crate) fn with_tools_support(mut self, supports_tools: bool) -> Self {
            self.supports_tools = supports_tools;
            self
        }

        /// The `tools` argument of every `apply_template` call so far.
        pub(crate) fn recorded_tools(&self) -> Vec<Vec<Value>> {
            self.recorded_tools.lock().expect("tools lock").clone()
        }
    }

    #[async_trait]
    impl Engine for FakeEngine {
        async fn props(&self) -> crate::engine::Result<ModelProps> {
            unreachable!()
        }

        async fn tokenize(&self, _text: &str) -> crate::engine::Result<TokenizeResult> {
            Ok(TokenizeResult {
                tokens: vec![],
                count: self.token_count,
            })
        }

        async fn detokenize(&self, _tokens: &[i64]) -> crate::engine::Result<String> {
            unreachable!()
        }

        async fn complete(
            &self,
            _prompt: &str,
            _cfg: &SamplingConfig,
        ) -> crate::engine::Result<BoxStream<'static, crate::engine::Result<CompletionChunk>>>
        {
            let state = LazyChunks {
                chunks: self.chunks.clone(),
                index: 0,
                pulled: Arc::clone(&self.pulled),
                dropped: Arc::clone(&self.dropped),
            };
            Ok(futures::stream::unfold(state, |s| async move { s.next_chunk() }).boxed())
        }

        async fn apply_template(
            &self,
            _messages: &[crate::engine::ChatMessage],
            _add_generation_prompt: bool,
            _enable_thinking: bool,
            tools: &[Value],
        ) -> crate::engine::Result<String> {
            self.recorded_tools
                .lock()
                .expect("tools lock")
                .push(tools.to_vec());
            Ok("PROMPT".to_string())
        }

        fn supports_tools(&self) -> bool {
            self.supports_tools
        }
    }

    /// A real `serve` loop on an ephemeral loopback port.
    pub(crate) struct TestServer;

    impl TestServer {
        /// Starts `serve` over `engine` with the given window size; returns its address.
        pub(crate) async fn spawn(engine: Arc<dyn Engine>, n_ctx: usize) -> std::net::SocketAddr {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let addr = listener.local_addr().expect("addr");
            let props = ModelProps {
                n_ctx,
                model_name: "fake".into(),
            };
            tokio::spawn(serve(listener, engine, props, None));
            addr
        }
    }

    /// A WebSocket client connected to a real `serve` loop on an ephemeral port.
    pub(crate) struct TestClient {
        ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
        next_id: u64,
    }

    impl TestClient {
        /// Starts `serve` over `engine` with the given window size and connects to it.
        pub(crate) async fn connect(engine: Arc<dyn Engine>, n_ctx: usize) -> Self {
            let addr = TestServer::spawn(engine, n_ctx).await;
            let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
                .await
                .expect("connect");
            Self { ws, next_id: 1 }
        }

        /// Sends one request; returns the notifications received before its response, then the
        /// response frame.
        pub(crate) async fn request(&mut self, method: &str, params: Value) -> (Vec<Value>, Value) {
            let id = self.next_id;
            self.next_id += 1;
            let frame = json!({"id": id, "method": method, "params": params});
            self.ws
                .send(Message::text(frame.to_string()))
                .await
                .expect("send");
            let mut notes = Vec::new();
            while let Some(frame) = self.ws.next().await {
                let Message::Text(text) = frame.expect("frame") else {
                    continue;
                };
                let value: Value = serde_json::from_str(text.as_str()).expect("json");
                if value.get("id") == Some(&json!(id)) {
                    return (notes, value);
                }
                notes.push(value);
            }
            panic!("connection closed before the response");
        }

        /// The `method` of each notification, in order.
        pub(crate) fn methods(notes: &[Value]) -> Vec<String> {
            notes
                .iter()
                .map(|n| n["method"].as_str().unwrap_or_default().to_string())
                .collect()
        }

        /// The params of every notification named `method`.
        pub(crate) fn params_of(notes: &[Value], method: &str) -> Vec<Value> {
            notes
                .iter()
                .filter(|n| n["method"] == method)
                .map(|n| n["params"].clone())
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{FakeEngine, TestClient};
    use super::*;

    const MARKER_CHUNKS: [&str; 5] = [
        "Checking.",
        "<|tool_call>call:get_time{}",
        "<tool_call|>",
        "<|tool_response>",
        "fake",
    ];

    struct Harness;

    impl Harness {
        fn messages() -> Value {
            json!([{"role": "user", "content": "hi"}])
        }

        async fn generate(engine: FakeEngine, n_ctx: usize, params: Value) -> (Vec<Value>, Value) {
            let mut client = TestClient::connect(Arc::new(engine), n_ctx).await;
            client.request("generate", params).await
        }
    }

    #[tokio::test]
    async fn generate_streams_chunks_then_done_with_content() {
        let engine = FakeEngine::new(&["Hello", " world"]);
        let params = json!({"messages": Harness::messages()});
        let (notes, resp) = Harness::generate(engine, 8192, params).await;
        assert_eq!(
            TestClient::methods(&notes),
            ["turn_started", "turn_chunk", "turn_chunk", "turn_done"]
        );
        assert_eq!(resp["result"], Value::Null);
        let done = &TestClient::params_of(&notes, "turn_done")[0];
        assert_eq!(done["content"], "Hello world");
        assert_eq!(done["tool_calls"], json!([]));
        let started = &TestClient::params_of(&notes, "turn_started")[0];
        assert_eq!(
            started,
            &json!({"prompt": "PROMPT", "prompt_tokens": 10, "n_ctx": 8192})
        );
    }

    #[tokio::test]
    async fn generate_emits_tool_call_and_stops_at_response_marker() {
        let engine = FakeEngine::new(&MARKER_CHUNKS);
        let params = json!({"messages": Harness::messages()});
        let (notes, _) = Harness::generate(engine, 8192, params).await;
        let calls = TestClient::params_of(&notes, "tool_call");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "call_1");
        assert_eq!(calls[0]["name"], "get_time");
        assert_eq!(calls[0]["arguments"], json!({}));
        assert_eq!(TestClient::params_of(&notes, "turn_chunk").len(), 4);
        let done = &TestClient::params_of(&notes, "turn_done")[0];
        assert_eq!(done["content"], "Checking.");
        assert_eq!(done["tool_calls"][0]["name"], "get_time");
    }

    #[tokio::test]
    async fn generate_stops_at_response_marker_without_call() {
        let engine = FakeEngine::new(&["Hi", "<|tool_response>", "response:x{}"]);
        let params = json!({"messages": Harness::messages()});
        let (notes, _) = Harness::generate(engine, 8192, params).await;
        assert_eq!(TestClient::params_of(&notes, "turn_chunk").len(), 2);
        let done = &TestClient::params_of(&notes, "turn_done")[0];
        assert_eq!(done["content"], "Hi");
        assert_eq!(done["tool_calls"], json!([]));
    }

    #[tokio::test]
    async fn generate_drops_the_stream_at_response_marker() {
        let engine = Arc::new(FakeEngine::new(&MARKER_CHUNKS));
        let mut client = TestClient::connect(engine.clone(), 8192).await;
        let params = json!({"messages": Harness::messages()});
        let (notes, _) = client.request("generate", params).await;
        assert_eq!(TestClient::params_of(&notes, "turn_chunk").len(), 4);
        // The marker is the 4th chunk; the 5th must never have been pulled.
        assert_eq!(engine.pulled_chunks(), 4);
        assert!(engine.stream_dropped());
    }

    #[tokio::test]
    async fn generate_with_tools_on_unsupporting_engine_errors_without_completing() {
        let engine = Arc::new(FakeEngine::new(&["ok"]));
        let mut client = TestClient::connect(engine.clone(), 8192).await;
        let tools = json!([{"type": "function", "function": {"name": "get_time"}}]);
        let params = json!({"messages": Harness::messages(), "tools": tools});
        let (notes, resp) = client.request("generate", params).await;
        assert_eq!(TestClient::methods(&notes), ["turn_error"]);
        assert_eq!(
            TestClient::params_of(&notes, "turn_error")[0]["message"],
            "tools are not supported by this engine"
        );
        assert_eq!(resp["result"], Value::Null);
        assert_eq!(engine.pulled_chunks(), 0);
        assert!(engine.recorded_tools().is_empty());
    }

    #[tokio::test]
    async fn generate_reports_unterminated_call_at_end_of_stream() {
        let engine = FakeEngine::new(&["Hi ", "<|tool_call>call:get_time{"]);
        let params = json!({"messages": Harness::messages()});
        let (notes, _) = Harness::generate(engine, 8192, params).await;
        let calls = TestClient::params_of(&notes, "tool_call");
        assert_eq!(calls.len(), 1);
        assert!(calls[0]["parse_error"].is_string());
        let methods = TestClient::methods(&notes);
        assert_eq!(methods.last().map(String::as_str), Some("turn_done"));
        let done = &TestClient::params_of(&notes, "turn_done")[0];
        assert_eq!(done["tool_calls"].as_array().map(Vec::len), Some(1));
    }

    #[tokio::test]
    async fn generate_rejects_when_window_full() {
        let engine = FakeEngine::new(&["x"]).with_token_count(9000);
        let params = json!({"messages": Harness::messages()});
        let (notes, resp) = Harness::generate(engine, 8192, params).await;
        assert_eq!(
            TestClient::methods(&notes),
            ["turn_started", "turn_rejected"]
        );
        assert_eq!(
            TestClient::params_of(&notes, "turn_rejected")[0],
            json!({"needed": 9000, "total": 8192})
        );
        assert_eq!(resp["result"], Value::Null);
    }

    #[tokio::test]
    async fn generate_passes_tools_to_template() {
        let tools = json!([{"type": "function", "function": {"name": "get_time"}}]);
        let engine = Arc::new(FakeEngine::new(&["ok"]).with_tools_support(true));
        let mut client = TestClient::connect(engine.clone(), 8192).await;
        let params = json!({"messages": Harness::messages(), "tools": tools});
        client.request("generate", params).await;
        assert_eq!(
            engine.recorded_tools(),
            vec![tools.as_array().cloned().unwrap()]
        );
    }

    #[tokio::test]
    async fn model_props_reports_supports_tools() {
        let engine = FakeEngine::new(&[]).with_tools_support(true);
        let mut client = TestClient::connect(Arc::new(engine), 8192).await;
        let (_, resp) = client.request("model_props", json!({})).await;
        assert_eq!(resp["result"]["supports_tools"], true);
    }
}
