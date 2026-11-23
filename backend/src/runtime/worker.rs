//! The runtime's client side of the worker protocol: calls the worker's `generate`,
//! `model_props` and `tokenize` over a websocket.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::engine::{ChatMessage, SamplingConfig};

type ClientWs = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A worker notification (`{"method", "params"}`, no `id`).
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub method: String,
    pub params: Value,
}

/// The params of the worker's `generate` call.
#[derive(Debug, Clone, Serialize)]
pub struct GenerateRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<Value>,
    pub enable_thinking: bool,
    pub sampling: SamplingConfig,
}

/// Why a worker call failed.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkError(pub String);

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LinkError {}

/// A connection to the worker, as the tool loop sees it.
#[async_trait]
pub trait WorkerLink: Send + Sync {
    async fn model_props(&self) -> Result<Value, LinkError>;
    async fn tokenize(&self, text: &str) -> Result<Value, LinkError>;
    /// Streams every notification; ends when the response frame arrives. An error frame or an
    /// unparseable frame is an `Err` item; a socket closed before the response just ends the
    /// stream. Dropping the stream closes the socket, which makes the worker drop its engine
    /// stream.
    ///
    /// A stream that ends without a `turn_done`, `turn_rejected` or `turn_error` note was
    /// truncated (the worker died or the socket dropped mid-turn); the consumer must treat that
    /// as an error.
    async fn generate(
        &self,
        req: &GenerateRequest,
    ) -> Result<BoxStream<'static, Result<Note, LinkError>>, LinkError>;
}

/// Websocket `WorkerLink`: one connection per call, request id `1`.
pub struct WsWorkerLink {
    url: String,
    connect_timeout: Duration,
    call_timeout: Duration,
    idle_timeout: Duration,
}

enum Frame {
    Note(Note),
    Response(Value),
}

/// State of the `generate` stream; owns the socket.
struct NoteStream {
    ws: ClientWs,
    finished: bool,
    idle_timeout: Duration,
}

impl WsWorkerLink {
    const REQUEST_ID: u64 = 1;
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
    const CALL_TIMEOUT: Duration = Duration::from_secs(30);
    const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
    const ERR_CONNECT_TIMEOUT: &'static str = "worker connect timed out";

    pub fn new(url: String) -> Self {
        Self {
            url,
            connect_timeout: Self::CONNECT_TIMEOUT,
            call_timeout: Self::CALL_TIMEOUT,
            idle_timeout: Self::STREAM_IDLE_TIMEOUT,
        }
    }

    fn call_timeout_message(timeout: Duration) -> String {
        format!("worker did not answer within {}s", timeout.as_secs_f64())
    }

    fn idle_timeout_message(timeout: Duration) -> String {
        format!("worker idle for {}s", timeout.as_secs_f64())
    }

    /// Overrides the connect, whole-call and per-frame idle timeouts (tests use milliseconds).
    pub fn with_timeouts(mut self, connect: Duration, call: Duration, idle: Duration) -> Self {
        self.connect_timeout = connect;
        self.call_timeout = call;
        self.idle_timeout = idle;
        self
    }

    /// Opens a socket and sends the single request.
    async fn open(&self, method: &str, params: Value) -> Result<ClientWs, LinkError> {
        let connecting = tokio_tungstenite::connect_async(self.url.as_str());
        let (mut ws, _) = tokio::time::timeout(self.connect_timeout, connecting)
            .await
            .map_err(|_| LinkError(Self::ERR_CONNECT_TIMEOUT.into()))?
            .map_err(|e| LinkError(format!("connect {}: {e}", self.url)))?;
        let frame = json!({"id": Self::REQUEST_ID, "method": method, "params": params});
        ws.send(Message::text(frame.to_string()))
            .await
            .map_err(|e| LinkError(format!("send {method}: {e}")))?;
        Ok(ws)
    }

    /// Interprets a text frame as a note or the response; an error response is an `Err`.
    fn classify(text: &str) -> Result<Frame, LinkError> {
        let value: Value =
            serde_json::from_str(text).map_err(|e| LinkError(format!("unparseable frame: {e}")))?;
        if value.get("id") == Some(&json!(Self::REQUEST_ID)) {
            if let Some(err) = value.get("error") {
                let message = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("worker error");
                return Err(LinkError(message.to_string()));
            }
            let result = value.get("result").cloned().unwrap_or(Value::Null);
            return Ok(Frame::Response(result));
        }
        match value.get("method").and_then(Value::as_str) {
            Some(method) => Ok(Frame::Note(Note {
                method: method.to_string(),
                params: value.get("params").cloned().unwrap_or(Value::Null),
            })),
            None => Err(LinkError("frame is neither a note nor a response".into())),
        }
    }

    /// Reads frames until the response; its `result` is returned, notes are skipped.
    async fn call(&self, method: &str, params: Value) -> Result<Value, LinkError> {
        tokio::time::timeout(self.call_timeout, self.exchange(method, params))
            .await
            .map_err(|_| LinkError(Self::call_timeout_message(self.call_timeout)))?
    }

    async fn exchange(&self, method: &str, params: Value) -> Result<Value, LinkError> {
        let mut ws = self.open(method, params).await?;
        while let Some(frame) = ws.next().await {
            let frame = frame.map_err(|e| LinkError(format!("receive: {e}")))?;
            let Message::Text(text) = frame else {
                continue;
            };
            // Any unparseable frame aborts the call: with one request per socket there is no
            // way to tell a corrupt note from a corrupt response, so the exchange is untrusted.
            if let Frame::Response(result) = Self::classify(text.as_str())? {
                return Ok(result);
            }
        }
        Err(LinkError(format!(
            "worker closed before answering {method}"
        )))
    }

    /// One step of the `generate` stream: the next item, or `None` once finished.
    async fn next_note(mut state: NoteStream) -> Option<(Result<Note, LinkError>, NoteStream)> {
        if state.finished {
            return None;
        }
        loop {
            let next = tokio::time::timeout(state.idle_timeout, state.ws.next()).await;
            let Ok(next) = next else {
                state.finished = true;
                return Some((
                    Err(LinkError(Self::idle_timeout_message(state.idle_timeout))),
                    state,
                ));
            };
            let frame = next?;
            let frame = match frame {
                Ok(frame) => frame,
                Err(e) => {
                    state.finished = true;
                    return Some((Err(LinkError(format!("receive: {e}"))), state));
                }
            };
            let Message::Text(text) = frame else {
                continue;
            };
            match Self::classify(text.as_str()) {
                Ok(Frame::Note(note)) => return Some((Ok(note), state)),
                Ok(Frame::Response(_)) => return None,
                Err(e) => {
                    state.finished = true;
                    return Some((Err(e), state));
                }
            }
        }
    }
}

#[async_trait]
impl WorkerLink for WsWorkerLink {
    async fn model_props(&self) -> Result<Value, LinkError> {
        self.call("model_props", json!({})).await
    }

    async fn tokenize(&self, text: &str) -> Result<Value, LinkError> {
        self.call("tokenize", json!({"text": text})).await
    }

    async fn generate(
        &self,
        req: &GenerateRequest,
    ) -> Result<BoxStream<'static, Result<Note, LinkError>>, LinkError> {
        let params = serde_json::to_value(req).map_err(|e| LinkError(e.to_string()))?;
        let ws = self.open("generate", params).await?;
        let state = NoteStream {
            ws,
            finished: false,
            idle_timeout: self.idle_timeout,
        };
        Ok(futures::stream::unfold(state, Self::next_note).boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Role;
    use std::sync::Arc;
    use crate::rpc::test_support::{FakeEngine, TestServer};

    const MS: Duration = Duration::from_millis(100);
    const GENEROUS: Duration = Duration::from_secs(5);

    /// What a raw scripted server does after accepting a websocket.
    enum Script {
        Silent,
        Frames(Vec<&'static str>),
        FramesThenSilent(Vec<&'static str>),
    }

    struct Fixture;

    impl Fixture {
        /// Accepts one websocket, reads the request, plays `script`. The returned receiver
        /// fires once the server side sees the client close (or the stream end).
        async fn raw_server(script: Script) -> (WsWorkerLink, tokio::sync::oneshot::Receiver<()>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let addr = listener.local_addr().expect("addr");
            let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.expect("accept");
                let mut ws = tokio_tungstenite::accept_async(tcp)
                    .await
                    .expect("handshake");
                let _ = ws.next().await;
                let (frames, keep_open) = match script {
                    Script::Silent => (vec![], true),
                    Script::Frames(f) => (f, false),
                    Script::FramesThenSilent(f) => (f, true),
                };
                for f in frames {
                    ws.send(Message::text(f)).await.expect("send");
                }
                if !keep_open {
                    let _ = ws.close(None).await;
                    while let Some(Ok(_)) = ws.next().await {}
                    return;
                }
                while let Some(Ok(_)) = ws.next().await {}
                let _ = closed_tx.send(());
            });
            (
                WsWorkerLink::new(format!("ws://{addr}"))
                    .with_timeouts(GENEROUS, GENEROUS, GENEROUS),
                closed_rx,
            )
        }

        async fn link(engine: FakeEngine) -> WsWorkerLink {
            let addr = TestServer::spawn(Arc::new(engine), 8192).await;
            WsWorkerLink::new(format!("ws://{addr}"))
        }

        fn request() -> GenerateRequest {
            GenerateRequest {
                messages: vec![ChatMessage::new(Role::User, "hi")],
                tools: vec![],
                enable_thinking: false,
                sampling: SamplingConfig::default(),
            }
        }
    }

    #[tokio::test]
    async fn ws_link_generate_yields_notes_in_order() {
        let link = Fixture::link(FakeEngine::new(&["Hello"])).await;
        let stream = link.generate(&Fixture::request()).await.expect("generate");
        let notes: Vec<Note> = stream.map(|n| n.expect("note")).collect().await;
        let methods: Vec<&str> = notes.iter().map(|n| n.method.as_str()).collect();
        assert_eq!(methods, ["turn_started", "turn_chunk", "turn_done"]);
        assert_eq!(notes[2].params["content"], "Hello");
    }

    #[tokio::test]
    async fn ws_link_model_props_round_trips() {
        let link = Fixture::link(FakeEngine::new(&[])).await;
        let props = link.model_props().await.expect("props");
        assert!(props.get("supports_tools").is_some());
    }

    #[tokio::test]
    async fn ws_link_tokenize_round_trips() {
        let link = Fixture::link(FakeEngine::new(&[]).with_token_count(7)).await;
        let result = link.tokenize("abc").await.expect("tokenize");
        assert_eq!(result["count"], 7);
    }

    #[tokio::test]
    async fn ws_link_reports_connect_failure() {
        let link = WsWorkerLink::new("ws://127.0.0.1:1".into());
        assert!(link.model_props().await.is_err());
    }

    #[tokio::test]
    async fn ws_link_error_frame_becomes_err() {
        let link = Fixture::link(FakeEngine::new(&[])).await;
        let err = link.call("no_such_method", json!({})).await.unwrap_err();
        assert!(err.0.contains("no_such_method"), "{}", err.0);
    }

    #[tokio::test]
    async fn call_times_out_when_worker_never_answers() {
        let (link, _closed) = Fixture::raw_server(Script::Silent).await;
        let link = link.with_timeouts(GENEROUS, MS, GENEROUS);
        let err = link.model_props().await.unwrap_err();
        assert_eq!(err.0, WsWorkerLink::call_timeout_message(MS));
        assert_eq!(err.0, "worker did not answer within 0.1s");
    }

    #[tokio::test]
    async fn stream_errors_when_worker_goes_idle() {
        let note = r#"{"method":"turn_started","params":{}}"#;
        let (link, _closed) = Fixture::raw_server(Script::FramesThenSilent(vec![note])).await;
        let idle = MS * 3;
        let link = link.with_timeouts(GENEROUS, GENEROUS, idle);
        let mut stream = link.generate(&Fixture::request()).await.expect("generate");
        assert_eq!(stream.next().await.unwrap().unwrap().method, "turn_started");
        let err = stream.next().await.unwrap().unwrap_err();
        assert_eq!(err.0, WsWorkerLink::idle_timeout_message(idle));
        assert_eq!(err.0, "worker idle for 0.3s");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn unparseable_frame_becomes_err() {
        let (link, _closed) = Fixture::raw_server(Script::Frames(vec!["not json"])).await;
        let mut stream = link.generate(&Fixture::request()).await.expect("generate");
        let err = stream.next().await.unwrap().unwrap_err();
        assert!(err.0.contains("unparseable"), "{}", err.0);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn socket_closed_before_response_ends_stream_without_error() {
        let note = r#"{"method":"turn_started","params":{}}"#;
        let (link, _closed) = Fixture::raw_server(Script::Frames(vec![note])).await;
        let stream = link.generate(&Fixture::request()).await.expect("generate");
        let items: Vec<_> = stream.collect().await;
        assert_eq!(items.len(), 1);
        assert!(items[0].is_ok());
    }

    #[tokio::test]
    async fn dropping_stream_closes_socket() {
        let note = r#"{"method":"turn_started","params":{}}"#;
        let (link, closed) = Fixture::raw_server(Script::FramesThenSilent(vec![note])).await;
        let link = link.with_timeouts(GENEROUS, GENEROUS, GENEROUS);
        let mut stream = link.generate(&Fixture::request()).await.expect("generate");
        assert!(stream.next().await.unwrap().is_ok());
        drop(stream);
        tokio::time::timeout(Duration::from_secs(5), closed)
            .await
            .expect("server saw close")
            .expect("signal");
    }
}
