use std::sync::Arc;

use async_trait::async_trait;
use backend::engine::{
    ChatMessage, CompletionChunk, Engine, ModelProps, Result, SamplingConfig, TokenizeResult,
};
use backend::rpc;
use futures::stream::BoxStream;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

struct FakeEngine {
    token_count: usize,
}

#[async_trait]
impl Engine for FakeEngine {
    async fn props(&self) -> Result<ModelProps> {
        Ok(props())
    }
    async fn tokenize(&self, _text: &str) -> Result<TokenizeResult> {
        Ok(TokenizeResult {
            tokens: vec![1, 2, 3],
            count: self.token_count,
        })
    }
    async fn detokenize(&self, _tokens: &[i64]) -> Result<String> {
        Ok(String::new())
    }
    async fn complete(
        &self,
        _prompt: &str,
        _cfg: &SamplingConfig,
    ) -> Result<BoxStream<'static, Result<CompletionChunk>>> {
        let chunks = ["Hel", "lo"].iter().enumerate().map(|(i, t)| {
            Ok(CompletionChunk {
                text: t.to_string(),
                tokens: vec![],
                stop: i == 1,
            })
        });
        Ok(futures::stream::iter(chunks).boxed())
    }
    async fn apply_template(
        &self,
        _messages: &[ChatMessage],
        _add_generation_prompt: bool,
        _enable_thinking: bool,
    ) -> Result<String> {
        Ok("RENDERED".to_string())
    }
}

fn props() -> ModelProps {
    ModelProps {
        n_ctx: 100,
        model_name: "fake".into(),
    }
}

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn connect(token_count: usize) -> Client {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine: Arc<dyn Engine> = Arc::new(FakeEngine { token_count });
    tokio::spawn(rpc::serve(listener, engine, props(), None));
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .unwrap();
    ws
}

async fn send(ws: &mut Client, id: u64, method: &str, params: Value) {
    let req = json!({"id": id, "method": method, "params": params});
    ws.send(Message::text(req.to_string())).await.unwrap();
}

async fn recv(ws: &mut Client) -> Value {
    loop {
        if let Message::Text(t) = ws.next().await.unwrap().unwrap() {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

/// Reads frames until the response carrying `id`; returns (notifications, response).
async fn collect(ws: &mut Client, id: u64) -> (Vec<Value>, Value) {
    let mut notes = Vec::new();
    loop {
        let msg = recv(ws).await;
        if msg.get("id") == Some(&json!(id)) {
            return (notes, msg);
        }
        notes.push(msg);
    }
}

fn methods(notes: &[Value]) -> Vec<&str> {
    notes
        .iter()
        .map(|n| n["method"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn ws_tokenize_roundtrips() {
    let mut ws = connect(3).await;
    send(&mut ws, 1, "tokenize", json!({"text": "hi"})).await;
    let (notes, resp) = collect(&mut ws, 1).await;
    assert!(notes.is_empty());
    assert_eq!(resp["result"], json!({"tokens": [1, 2, 3], "count": 3}));
}

#[tokio::test]
async fn ws_send_turn_streams_started_then_done() {
    let mut ws = connect(7).await;
    let params = json!({"content": "hi", "enable_thinking": true});
    send(&mut ws, 1, "send_turn", params).await;
    let (notes, resp) = collect(&mut ws, 1).await;
    assert!(resp.get("error").is_none());
    assert_eq!(
        methods(&notes),
        ["turn_started", "turn_chunk", "turn_chunk", "turn_done"]
    );
    assert_eq!(
        notes[0]["params"],
        json!({"prompt": "RENDERED", "prompt_tokens": 7, "n_ctx": 100})
    );
    assert_eq!(notes[1]["params"]["text"], "Hel");
    assert_eq!(notes[1]["params"]["stop"], false);
    assert_eq!(notes[2]["params"]["text"], "lo");
    assert_eq!(notes[2]["params"]["stop"], true);
}

#[tokio::test]
async fn ws_window_full_emits_rejected() {
    let mut ws = connect(500).await;
    let params = json!({"content": "huge", "enable_thinking": false});
    send(&mut ws, 1, "send_turn", params).await;
    let (notes, _resp) = collect(&mut ws, 1).await;
    assert!(!methods(&notes).contains(&"turn_chunk"));
    let rejected = notes
        .iter()
        .find(|n| n["method"] == "turn_rejected")
        .unwrap();
    assert_eq!(rejected["params"], json!({"needed": 500, "total": 100}));
}
