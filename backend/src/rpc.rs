//! JSON-RPC over WebSocket.
//!
//! Wire envelope (one JSON object per text frame):
//! - request:      `{"id": N, "method": "...", "params": {...}}`
//! - response:     `{"id": N, "result": ...}` or `{"id": N, "error": {"code": C, "message": "..."}}`
//! - notification: `{"method": "...", "params": {...}}` (server -> client, no `id`)
//!
//! `send_turn` streams `turn_started`, `turn_chunk`*, then `turn_done` | `turn_error`
//! (or `turn_started` + `turn_rejected`); the request's own response is sent last.

use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::engine::{Engine, ModelProps, SamplingConfig};
use crate::session::{ChatSession, TurnRejected};

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

/// Accepts WebSocket connections forever; each connection gets its own `ChatSession`.
pub async fn serve(
    listener: TcpListener,
    engine: Arc<dyn Engine>,
    props: ModelProps,
    system: Option<String>,
) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
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
            Err(err) => Self::error_frame(request.id, &err),
        };
        Self::send_json(ws, frame).await
    }

    fn error_frame(id: Value, err: &RpcError) -> Value {
        json!({"id": id, "error": {"code": err.code, "message": err.message}})
    }

    async fn dispatch(&mut self, ws: &mut Ws, req: &Request) -> Result<Value, RpcError> {
        match req.method.as_str() {
            "model_props" => Self::to_value(&self.props),
            "tokenize" => {
                let p: TokenizeParams = Self::params(req)?;
                let result = self
                    .engine
                    .tokenize(&p.text)
                    .await
                    .map_err(RpcError::internal)?;
                Self::to_value(&result)
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
            other => Err(RpcError::new(
                ERR_METHOD_NOT_FOUND,
                format!("unknown method: {other}"),
            )),
        }
    }

    fn params<T: DeserializeOwned>(req: &Request) -> Result<T, RpcError> {
        serde_json::from_value(req.params.clone()).map_err(|e| RpcError::new(ERR_INVALID_PARAMS, e))
    }

    fn to_value<T: Serialize>(v: &T) -> Result<Value, RpcError> {
        serde_json::to_value(v).map_err(RpcError::internal)
    }

    async fn send_turn(&mut self, ws: &mut Ws, p: SendTurnParams) -> Result<(), RpcError> {
        let (prompt, fill) = self
            .session
            .preview_turn(&p.content, p.enable_thinking)
            .await
            .map_err(RpcError::internal)?;
        Self::notify(
            ws,
            "turn_started",
            json!({"prompt": prompt, "prompt_tokens": fill.used, "n_ctx": fill.total}),
        )
        .await?;

        let mut stream = match self
            .session
            .send(&p.content, &self.sampling, p.enable_thinking)
            .await
        {
            Ok(s) => s,
            Err(TurnRejected::WindowFull { needed, total }) => {
                let params = json!({"needed": needed, "total": total});
                return Self::notify(ws, "turn_rejected", params).await;
            }
            Err(TurnRejected::Engine(e)) => {
                return Self::notify(ws, "turn_error", json!({"message": e.to_string()})).await;
            }
        };
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

    async fn notify(ws: &mut Ws, method: &str, params: Value) -> Result<(), RpcError> {
        Self::send_json(ws, json!({"method": method, "params": params}))
            .await
            .map_err(RpcError::internal)
    }

    async fn send_json(ws: &mut Ws, value: Value) -> Result<(), BoxError> {
        ws.send(Message::text(value.to_string())).await?;
        Ok(())
    }
}
