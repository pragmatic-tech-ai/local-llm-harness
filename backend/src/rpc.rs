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
//!
//! `measure_prompt` takes the same params as `generate` and renders and tokenizes the prompt
//! the same way, but generates nothing and sends no notifications: it answers
//! `{prompt_tokens, n_ctx}`.

use std::sync::Arc;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::engine::template::OpenChannel;
use crate::engine::tool_format::{ParseEvent, ParsedCall, PlainTextParser, ToolCallParser};
use crate::engine::{ChatMessage, Engine, ModelProps, SamplingConfig};
use crate::session::{ChatSession, ContextFill, TurnRejected};

pub(crate) const ERR_PARSE: i64 = -32700;
pub(crate) const ERR_METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const ERR_INVALID_PARAMS: i64 = -32602;
pub(crate) const ERR_INTERNAL: i64 = -32603;

pub(crate) type Ws = WebSocketStream<TcpStream>;
pub(crate) type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Deserialize)]
pub(crate) struct Request {
    pub(crate) id: Value,
    pub(crate) method: String,
    #[serde(default)]
    pub(crate) params: Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TokenizeParams {
    pub(crate) text: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SetSamplingParams {
    pub(crate) config: SamplingConfig,
}

/// The worker's `send_turn` params.
#[derive(Debug, Deserialize)]
pub(crate) struct SendTurnParams {
    pub(crate) content: String,
    #[serde(default)]
    pub(crate) enable_thinking: bool,
}

#[derive(Debug, Deserialize)]
struct PromptParams {
    messages: Vec<ChatMessage>,
    #[serde(default)]
    tools: Option<Vec<Value>>,
    #[serde(default)]
    enable_thinking: bool,
}

/// The worker's `generate` params. `measure_prompt` takes just the `PromptParams` part (an
/// extra `sampling` field is ignored), so the two cannot drift.
#[derive(Debug, Deserialize)]
struct GenerateParams {
    #[serde(flatten)]
    prompt: PromptParams,
    #[serde(default)]
    sampling: Option<SamplingConfig>,
}

/// A prompt rendered through the chat template, with its token count.
struct RenderedPrompt {
    text: String,
    used: usize,
}

#[derive(Debug)]
pub(crate) struct RpcError {
    pub(crate) code: i64,
    pub(crate) message: String,
}

impl RpcError {
    pub(crate) fn new(code: i64, message: impl ToString) -> Self {
        Self {
            code,
            message: message.to_string(),
        }
    }

    pub(crate) fn internal(message: impl ToString) -> Self {
        Self::new(ERR_INTERNAL, message)
    }
}

/// Frame construction and writing shared by every JSON-RPC server in the crate.
pub(crate) struct Frames;

impl Frames {
    pub(crate) fn result(id: Value, result: Value) -> Value {
        json!({"id": id, "result": result})
    }

    pub(crate) fn error(id: Value, err: &RpcError) -> Value {
        json!({"id": id, "error": {"code": err.code, "message": err.message}})
    }

    pub(crate) fn notification(method: &str, params: Value) -> Value {
        json!({"method": method, "params": params})
    }

    /// Decodes one request envelope; a malformed one yields the parse-error frame to send back
    /// (with a null id).
    pub(crate) fn decode(text: &str) -> Result<Request, Value> {
        serde_json::from_str(text)
            .map_err(|e| Self::error(Value::Null, &RpcError::new(ERR_PARSE, e)))
    }

    /// The response frame for request `id`.
    pub(crate) fn reply(id: Value, outcome: Result<Value, RpcError>) -> Value {
        match outcome {
            Ok(result) => Self::result(id, result),
            Err(err) => Self::error(id, &err),
        }
    }

    /// Decodes request params, mapping a mismatch to an invalid-params error.
    pub(crate) fn params<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
        serde_json::from_value(params).map_err(|e| RpcError::new(ERR_INVALID_PARAMS, e))
    }

    pub(crate) async fn send(ws: &mut Ws, value: Value) -> Result<(), BoxError> {
        ws.send(Message::text(value.to_string())).await?;
        Ok(())
    }
}

/// Why a request failed: a JSON-RPC error (reported to the client as an error response) or a
/// transport failure (the socket is dead, so the connection ends with that error and no
/// reply).
pub(crate) enum Failure {
    Rpc(RpcError),
    Transport(BoxError),
}

impl From<RpcError> for Failure {
    fn from(e: RpcError) -> Self {
        Self::Rpc(e)
    }
}

/// One JSON-RPC server's request handling; `FrameLoop` does the socket and envelope work.
#[async_trait]
pub(crate) trait RpcDispatch: Send {
    /// Handles one request. Notifications may be written to `ws` while it runs; the returned
    /// value becomes the request's response.
    async fn dispatch(&mut self, ws: &mut Ws, request: &Request) -> Result<Value, Failure>;
}

/// The worker's websocket frame loop: accept, read text frames, decode the envelope, dispatch,
/// answer.
pub(crate) struct FrameLoop;

impl FrameLoop {
    pub(crate) async fn run<D: RpcDispatch>(
        dispatcher: &mut D,
        stream: TcpStream,
    ) -> Result<(), BoxError> {
        let mut ws = tokio_tungstenite::accept_async(stream).await?;
        while let Some(frame) = ws.next().await {
            let Message::Text(text) = frame? else {
                continue;
            };
            Self::handle_frame(dispatcher, &mut ws, text.as_str()).await?;
        }
        Ok(())
    }

    /// Answers one frame.
    async fn handle_frame<D: RpcDispatch>(
        dispatcher: &mut D,
        ws: &mut Ws,
        text: &str,
    ) -> Result<(), BoxError> {
        let request = match Frames::decode(text) {
            Ok(r) => r,
            Err(frame) => return Frames::send(ws, frame).await,
        };
        let frame = match dispatcher.dispatch(ws, &request).await {
            Ok(result) => Frames::reply(request.id, Ok(result)),
            Err(Failure::Rpc(err)) => Frames::reply(request.id, Err(err)),
            Err(Failure::Transport(e)) => return Err(e),
        };
        Frames::send(ws, frame).await
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
        let mut connection = Connection {
            engine: Arc::clone(&engine),
            props: props.clone(),
            session: ChatSession::new(Arc::clone(&engine), props.clone(), system.clone()),
            sampling: SamplingConfig::default(),
        };
        tokio::spawn(async move {
            if let Err(e) = FrameLoop::run(&mut connection, stream).await {
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

#[async_trait]
impl RpcDispatch for Connection {
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
                let p: TokenizeParams = Frames::params(req.params.clone())?;
                let result = self
                    .engine
                    .tokenize(&p.text)
                    .await
                    .map_err(RpcError::internal)?;
                Ok(Self::to_value(&result)?)
            }
            "set_sampling" => {
                let p: SetSamplingParams = Frames::params(req.params.clone())?;
                self.sampling = p.config;
                Ok(Value::Null)
            }
            "send_turn" => {
                let p: SendTurnParams = Frames::params(req.params.clone())?;
                self.send_turn(ws, p).await?;
                Ok(Value::Null)
            }
            "generate" => {
                let p: GenerateParams = Frames::params(req.params.clone())?;
                self.generate(ws, p).await?;
                Ok(Value::Null)
            }
            "measure_prompt" => {
                let p: PromptParams = Frames::params(req.params.clone())?;
                let tools = p.tools.unwrap_or_default();
                let rendered = self
                    .render_and_count(&p.messages, &tools, p.enable_thinking)
                    .await
                    .map_err(RpcError::internal)?;
                Ok(json!({"prompt_tokens": rendered.used, "n_ctx": self.props.n_ctx}))
            }
            other => {
                Err(RpcError::new(ERR_METHOD_NOT_FOUND, format!("unknown method: {other}")).into())
            }
        }
    }
}

impl Connection {
    const TOOLS_UNSUPPORTED: &'static str = "tools are not supported by this engine";

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
    /// reply and, when tools were offered, parses tool calls out of it with the engine's
    /// `ToolCallFormat`. Nothing is stored on the connection.
    ///
    /// Wire contract:
    /// - Non-empty `tools` on an engine whose `supports_tools()` is false yields a single
    ///   `turn_error {message: "tools are not supported by this engine"}` before anything else
    ///   (no `turn_started`, no render, no `complete`).
    /// - Without tools the reply is plain text: no `tool_call` notes, no cut at a response
    ///   marker, `turn_done.tool_calls` is empty.
    /// - If the rendered prompt ends inside an open reasoning channel (Gemma 4 opens
    ///   `<|channel>thought\n` itself after a tool response when thinking is on), the model's
    ///   output starts inside it; `turn_done.content` is prefixed with that opener so the text
    ///   is self-contained and the template can strip the reasoning from history. Relayed
    ///   `turn_chunk`s stay raw.
    /// - Every engine chunk is relayed raw as `turn_chunk {text, tokens, stop}`, including the
    ///   chunk that carries the response marker; that chunk keeps the engine's own `stop` flag
    ///   (normally false, since the engine is cut off mid-stream). Chunks after it are never
    ///   relayed. Clients must not treat `stop` as the end of the turn: `turn_done` (or
    ///   `turn_rejected` / `turn_error`) is what ends it.
    async fn generate(&mut self, ws: &mut Ws, p: GenerateParams) -> Result<(), Failure> {
        let cfg = p.sampling.unwrap_or_else(|| self.sampling.clone());
        let tools = p.prompt.tools.unwrap_or_default();
        let rendered = match self
            .render_and_count(&p.prompt.messages, &tools, p.prompt.enable_thinking)
            .await
        {
            Ok(rendered) => rendered,
            Err(message) => return Self::turn_error(ws, message).await,
        };
        let RenderedPrompt { text: prompt, used } = rendered;
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

        let mut parser = self.parser_for(&tools);
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
        let opener = OpenChannel::at_tail(&prompt).unwrap_or_default();
        let content = format!("{opener}{}", parser.content());
        let done = json!({"content": content, "tool_calls": parser.calls()});
        Self::notify(ws, "turn_done", done).await
    }

    /// Renders `messages` through the engine's chat template (with `tools` and thinking, ready
    /// for generation) and counts the result's tokens. The one path both `generate` and
    /// `measure_prompt` use, so their counts cannot drift. Touches no engine cache.
    async fn render_and_count(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        enable_thinking: bool,
    ) -> Result<RenderedPrompt, String> {
        if !tools.is_empty() && !self.engine.supports_tools() {
            return Err(Self::TOOLS_UNSUPPORTED.to_string());
        }
        let text = self
            .engine
            .apply_template(messages, true, enable_thinking, tools)
            .await
            .map_err(|e| e.to_string())?;
        let used = self
            .engine
            .tokenize(&text)
            .await
            .map_err(|e| e.to_string())?
            .count;
        Ok(RenderedPrompt { text, used })
    }

    /// The engine's tool-call parser when tools were offered; plain text otherwise.
    fn parser_for(&self, tools: &[Value]) -> Box<dyn ToolCallParser> {
        match self.engine.tool_format() {
            Some(format) if !tools.is_empty() => format.new_parser(),
            _ => Box::new(PlainTextParser::new()),
        }
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
        Frames::send(ws, Frames::notification(method, params))
            .await
            .map_err(Failure::Transport)
    }
}

/// Scripted engine and WebSocket client for exercising `serve` end to end; reused by other
/// modules' tests (`crate::rpc::test_support`).
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::engine::template::ChatTemplate;
    use crate::engine::tool_format::{GemmaToolFormat, ToolCallFormat};
    use crate::engine::{CompletionChunk, TokenizeResult};
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tokio_tungstenite::MaybeTlsStream;

    /// An `Engine` that replays scripted chunks. `apply_template` records the `tools` it is
    /// given and returns `"PROMPT"` (or renders through a real template, see
    /// `with_template`); `tokenize` reports a configurable count.
    pub(crate) struct FakeEngine {
        chunks: Vec<String>,
        token_count: usize,
        count_chars: bool,
        supports_tools: bool,
        template: Option<ChatTemplate>,
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
                count_chars: false,
                supports_tools: false,
                template: None,
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

        /// `tokenize` counts one token per character, so the count depends on the rendered text.
        pub(crate) fn with_char_tokens(mut self) -> Self {
            self.count_chars = true;
            self
        }

        /// With tool support the fake speaks the Gemma 4 tool-call format.
        pub(crate) fn with_tools_support(mut self, supports_tools: bool) -> Self {
            self.supports_tools = supports_tools;
            self
        }

        /// `apply_template` renders through this real chat template instead of `"PROMPT"`.
        pub(crate) fn with_template(mut self, src: &str) -> Self {
            self.template = Some(ChatTemplate::from_jinja(src).expect("template"));
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

        async fn tokenize(&self, text: &str) -> crate::engine::Result<TokenizeResult> {
            let count = if self.count_chars {
                text.chars().count()
            } else {
                self.token_count
            };
            Ok(TokenizeResult {
                tokens: vec![],
                count,
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
            messages: &[crate::engine::ChatMessage],
            add_generation_prompt: bool,
            enable_thinking: bool,
            tools: &[Value],
        ) -> crate::engine::Result<String> {
            self.recorded_tools
                .lock()
                .expect("tools lock")
                .push(tools.to_vec());
            let Some(template) = &self.template else {
                return Ok("PROMPT".to_string());
            };
            template
                .render(messages, add_generation_prompt, enable_thinking, tools)
                .map_err(|e| crate::engine::EngineError::Protocol(e.to_string()))
        }

        fn tool_format(&self) -> Option<&'static dyn ToolCallFormat> {
            if self.supports_tools {
                Some(&GemmaToolFormat as &'static dyn ToolCallFormat)
            } else {
                None
            }
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
            Self::connect_to(addr).await
        }

        /// Connects to a server already listening on `addr`.
        pub(crate) async fn connect_to(addr: std::net::SocketAddr) -> Self {
            let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
                .await
                .expect("connect");
            Self { ws, next_id: 1 }
        }

        /// Sends `text` verbatim and returns the first frame the server answers with.
        pub(crate) async fn send_raw(&mut self, text: &str) -> Value {
            self.ws.send(Message::text(text)).await.expect("send");
            while let Some(frame) = self.ws.next().await {
                if let Message::Text(reply) = frame.expect("frame") {
                    return serde_json::from_str(reply.as_str()).expect("json");
                }
            }
            panic!("connection closed before a reply");
        }

        /// Sends one request; returns the notifications received before its response, then the
        /// response frame.
        pub(crate) async fn request(&mut self, method: &str, params: Value) -> (Vec<Value>, Value) {
            let id = self.send_request(method, params).await;
            self.response_to(id).await
        }

        /// Sends one request without waiting for its response; returns its id.
        pub(crate) async fn send_request(&mut self, method: &str, params: Value) -> u64 {
            let id = self.next_id;
            self.next_id += 1;
            let frame = json!({"id": id, "method": method, "params": params});
            self.ws
                .send(Message::text(frame.to_string()))
                .await
                .expect("send");
            id
        }

        /// Reads frames until the response to request `id`; returns the other frames read
        /// before it, then the response frame.
        pub(crate) async fn response_to(&mut self, id: u64) -> (Vec<Value>, Value) {
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

        /// Reads frames until a notification named `method` arrives; returns every frame read,
        /// that one included. Fails the test after a few seconds.
        pub(crate) async fn notes_until(&mut self, method: &str) -> Vec<Value> {
            const WAIT: std::time::Duration = std::time::Duration::from_secs(5);
            let mut notes = Vec::new();
            loop {
                let frame = tokio::time::timeout(WAIT, self.ws.next())
                    .await
                    .expect("timed out waiting for a notification")
                    .expect("connection closed")
                    .expect("frame");
                let Message::Text(text) = frame else {
                    continue;
                };
                let value: Value = serde_json::from_str(text.as_str()).expect("json");
                let done = value["method"] == method;
                notes.push(value);
                if done {
                    return notes;
                }
            }
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
        const GEMMA4: &'static str = include_str!("../tests/fixtures/gemma4-chat-template.jinja");

        fn messages() -> Value {
            json!([{"role": "user", "content": "hi"}])
        }

        fn tools() -> Value {
            json!([{"type": "function", "function": {"name": "get_time"}}])
        }

        /// A tool-capable fake replaying `chunks`.
        fn tool_engine(chunks: &[&str]) -> FakeEngine {
            FakeEngine::new(chunks).with_tools_support(true)
        }

        /// `generate` params offering the `get_time` tool.
        fn with_tools() -> Value {
            json!({"messages": Self::messages(), "tools": Self::tools()})
        }

        /// The prompt of the (single) `turn_started` in `notes`.
        fn prompt_of(notes: &[Value]) -> String {
            TestClient::params_of(notes, "turn_started")[0]["prompt"]
                .as_str()
                .expect("prompt")
                .to_string()
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
        let engine = Harness::tool_engine(&MARKER_CHUNKS);
        let (notes, _) = Harness::generate(engine, 8192, Harness::with_tools()).await;
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
        let engine = Harness::tool_engine(&["Hi", "<|tool_response>", "response:x{}"]);
        let (notes, _) = Harness::generate(engine, 8192, Harness::with_tools()).await;
        assert_eq!(TestClient::params_of(&notes, "turn_chunk").len(), 2);
        let done = &TestClient::params_of(&notes, "turn_done")[0];
        assert_eq!(done["content"], "Hi");
        assert_eq!(done["tool_calls"], json!([]));
    }

    #[tokio::test]
    async fn generate_drops_the_stream_at_response_marker() {
        let engine = Arc::new(Harness::tool_engine(&MARKER_CHUNKS));
        let mut client = TestClient::connect(engine.clone(), 8192).await;
        let (notes, _) = client.request("generate", Harness::with_tools()).await;
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
        let engine = Harness::tool_engine(&["Hi ", "<|tool_call>call:get_time{"]);
        let (notes, _) = Harness::generate(engine, 8192, Harness::with_tools()).await;
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
    async fn generate_without_tools_does_not_parse_calls() {
        let engine = Arc::new(Harness::tool_engine(&MARKER_CHUNKS));
        let mut client = TestClient::connect(engine.clone(), 8192).await;
        let params = json!({"messages": Harness::messages()});
        let (notes, _) = client.request("generate", params).await;
        assert!(TestClient::params_of(&notes, "tool_call").is_empty());
        assert_eq!(TestClient::params_of(&notes, "turn_chunk").len(), 5);
        assert_eq!(engine.pulled_chunks(), 5);
        let done = &TestClient::params_of(&notes, "turn_done")[0];
        assert_eq!(done["content"], MARKER_CHUNKS.concat());
        assert_eq!(done["tool_calls"], json!([]));
    }

    #[tokio::test]
    async fn generate_restores_the_open_thinking_channel_in_turn_done() {
        let chunks = ["The clock reads 14:05.", "<channel|>", "It's 14:05."];
        let engine = Harness::tool_engine(&chunks).with_template(Harness::GEMMA4);
        let mut client = TestClient::connect(Arc::new(engine), 8192).await;
        let tools = json!([{"type": "function", "function": {
            "name": "get_time", "description": "Current time",
            "parameters": {"type": "object", "properties": {}}}}]);
        let mut messages = vec![
            json!({"role": "user", "content": "time?"}),
            json!({"role": "assistant", "content": "", "tool_calls": [{"id": "call_1",
                "type": "function", "function": {"name": "get_time", "arguments": {}}}]}),
            json!({"role": "tool", "content": "", "tool_call_id": "call_1",
                "tool_result": {"utc": "2026-11-23T13:05:00Z"}}),
        ];
        let params = json!({"messages": messages, "tools": tools, "enable_thinking": true});
        let (notes, _) = client.request("generate", params).await;
        let step1 = Harness::prompt_of(&notes);
        assert!(
            step1.ends_with("<tool_response|><|channel>thought\n"),
            "{step1}"
        );
        let relayed = TestClient::params_of(&notes, "turn_chunk");
        assert_eq!(relayed[0]["text"], "The clock reads 14:05.");
        let done = &TestClient::params_of(&notes, "turn_done")[0];
        assert_eq!(
            done["content"],
            "<|channel>thought\nThe clock reads 14:05.<channel|>It's 14:05."
        );

        // The answer goes into history as content; the next prompt must not show the reasoning.
        messages.push(json!({"role": "assistant", "content": done["content"]}));
        messages.push(json!({"role": "user", "content": "thanks"}));
        let params = json!({"messages": messages, "tools": tools, "enable_thinking": true});
        let (notes, _) = client.request("generate", params).await;
        let step2 = Harness::prompt_of(&notes);
        assert!(!step2.contains("The clock reads"), "{step2}");
        assert!(
            step2.ends_with(
                "<tool_response|>It's 14:05.<turn|>\n<|turn>user\nthanks<turn|>\n<|turn>model\n"
            ),
            "{step2}"
        );
    }

    #[tokio::test]
    async fn measure_prompt_matches_generate_turn_started_tokens() {
        let engine = Harness::tool_engine(&["ok"])
            .with_template(Harness::GEMMA4)
            .with_char_tokens();
        let mut client = TestClient::connect(Arc::new(engine), 100_000).await;
        let mut seen = Vec::new();
        for (tools, thinking) in [(true, true), (true, false), (false, true), (false, false)] {
            let mut params = json!({"messages": Harness::messages(), "enable_thinking": thinking});
            if tools {
                params["tools"] = Harness::tools();
            }
            let (notes, resp) = client.request("measure_prompt", params.clone()).await;
            assert!(notes.is_empty());
            let (gen_notes, _) = client.request("generate", params).await;
            let started = &TestClient::params_of(&gen_notes, "turn_started")[0];
            let measured = resp["result"]["prompt_tokens"].as_u64().expect("count");
            assert_eq!(measured, started["prompt_tokens"].as_u64().unwrap());
            assert_eq!(
                measured as usize,
                started["prompt"].as_str().unwrap().chars().count()
            );
            assert_eq!(resp["result"]["n_ctx"], started["n_ctx"]);
            assert_eq!(resp["result"]["n_ctx"], 100_000);
            seen.push(measured);
        }
        // Each input changes the rendered prompt, so each case counts differently.
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 4, "tools and thinking must reach the render");
    }

    #[tokio::test]
    async fn measure_prompt_with_tools_on_unsupporting_engine_matches_generate() {
        let engine = Arc::new(FakeEngine::new(&["ok"]));
        let mut client = TestClient::connect(engine.clone(), 8192).await;
        let params = json!({"messages": Harness::messages(), "tools": Harness::tools()});
        let (_, resp) = client.request("measure_prompt", params.clone()).await;
        assert_eq!(resp["error"]["code"], ERR_INTERNAL);
        let (gen_notes, _) = client.request("generate", params).await;
        assert_eq!(
            resp["error"]["message"],
            TestClient::params_of(&gen_notes, "turn_error")[0]["message"]
        );
        assert!(engine.recorded_tools().is_empty());
    }

    #[tokio::test]
    async fn measure_prompt_does_not_generate() {
        let engine = Arc::new(FakeEngine::new(&["ok"]));
        let mut client = TestClient::connect(engine.clone(), 8192).await;
        let params = json!({"messages": Harness::messages()});
        let (notes, resp) = client.request("measure_prompt", params).await;
        assert!(notes.is_empty());
        assert_eq!(resp["result"], json!({"prompt_tokens": 10, "n_ctx": 8192}));
        assert_eq!(engine.pulled_chunks(), 0);
    }

    #[tokio::test]
    async fn measure_prompt_bad_params_is_32602() {
        let engine = FakeEngine::new(&[]);
        let mut client = TestClient::connect(Arc::new(engine), 8192).await;
        let (_, resp) = client
            .request("measure_prompt", json!({"messages": "nope"}))
            .await;
        assert_eq!(resp["error"]["code"], ERR_INVALID_PARAMS);
    }

    #[tokio::test]
    async fn model_props_reports_supports_tools() {
        let engine = FakeEngine::new(&[]).with_tools_support(true);
        let mut client = TestClient::connect(Arc::new(engine), 8192).await;
        let (_, resp) = client.request("model_props", json!({})).await;
        assert_eq!(resp["result"]["supports_tools"], true);
    }
}
