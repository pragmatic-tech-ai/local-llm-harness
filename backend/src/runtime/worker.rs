//! The runtime's client side of the worker protocol: calls the worker's `generate`,
//! `model_props` and `tokenize` over a websocket, and finds the ready worker via the supervisor.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::engine::{ChatMessage, SamplingConfig};
use crate::supervisor::manager::{HarnessManager, ReadinessProbe, WorkerLauncher};

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
    async fn generate(
        &self,
        req: &GenerateRequest,
    ) -> Result<BoxStream<'static, Result<Note, LinkError>>, LinkError>;
}

/// Hands out a link to the current ready worker, if there is one.
pub trait WorkerConnector: Send + Sync {
    fn connect(&self) -> Option<Arc<dyn WorkerLink>>;
}

/// The supervisor's answer to "where is the ready worker".
pub trait ReadyWorkerUrl: Send + Sync {
    fn ready_worker_url(&self) -> Option<String>;
}

impl<L: WorkerLauncher, P: ReadinessProbe> ReadyWorkerUrl for HarnessManager<L, P> {
    fn ready_worker_url(&self) -> Option<String> {
        HarnessManager::ready_worker_url(self)
    }
}

/// Websocket `WorkerLink`: one connection per call, request id `1`.
pub struct WsWorkerLink {
    url: String,
}

enum Frame {
    Note(Note),
    Response(Value),
}

/// State of the `generate` stream; owns the socket.
struct NoteStream {
    ws: ClientWs,
    finished: bool,
}

impl WsWorkerLink {
    const REQUEST_ID: u64 = 1;

    pub fn new(url: String) -> Self {
        Self { url }
    }

    /// Opens a socket and sends the single request.
    async fn open(&self, method: &str, params: Value) -> Result<ClientWs, LinkError> {
        let (mut ws, _) = tokio_tungstenite::connect_async(self.url.as_str())
            .await
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
                let message = err["message"].as_str().unwrap_or("worker error");
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
        let mut ws = self.open(method, params).await?;
        while let Some(frame) = ws.next().await {
            let frame = frame.map_err(|e| LinkError(format!("receive: {e}")))?;
            let Message::Text(text) = frame else {
                continue;
            };
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
            let frame = state.ws.next().await?;
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
        };
        Ok(futures::stream::unfold(state, Self::next_note).boxed())
    }
}

/// Connects to whichever worker the supervisor reports as ready.
pub struct SupervisorConnector {
    source: Arc<dyn ReadyWorkerUrl>,
}

impl SupervisorConnector {
    pub fn new(source: Arc<dyn ReadyWorkerUrl>) -> Self {
        Self { source }
    }
}

impl WorkerConnector for SupervisorConnector {
    fn connect(&self) -> Option<Arc<dyn WorkerLink>> {
        let url = self.source.ready_worker_url()?;
        Some(Arc::new(WsWorkerLink::new(url)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Role;
    use crate::rpc::test_support::{FakeEngine, TestServer};

    struct NoWorker;

    impl ReadyWorkerUrl for NoWorker {
        fn ready_worker_url(&self) -> Option<String> {
            None
        }
    }

    struct Fixture;

    impl Fixture {
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
        assert!(!err.0.is_empty());
    }

    #[test]
    fn connector_returns_none_without_ready_worker() {
        let connector = SupervisorConnector::new(Arc::new(NoWorker));
        assert!(connector.connect().is_none());
    }
}
