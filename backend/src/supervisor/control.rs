//! Supervisor control RPC: a WebSocket JSON-RPC server for listing runtimes/models and
//! starting/stopping the harness.
//!
//! The wire envelope deliberately mirrors `rpc.rs` (request `{id, method, params}`, response
//! `{id, result}` / `{id, error: {code, message}}`, notification `{method, params}`, same error
//! codes) but is implemented independently, to avoid destabilizing the proven chat path.
//!
//! `stop` requires an `id` but otherwise ignores it: the manager owns a single harness, so
//! stopping always stops that one (the id is accepted for forward compatibility).

use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast::error::RecvError;
use tokio_tungstenite::tungstenite::Message;

use super::catalog::{Catalog, RuntimeKind};
use super::discovery::discover;
use super::manager::{HarnessManager, ManagerError, ReadinessProbe, WorkerLauncher};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A JSON-RPC error (code + message).
#[derive(Debug)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    const ERR_PARSE: i64 = -32700;
    const ERR_METHOD_NOT_FOUND: i64 = -32601;
    const ERR_INVALID_PARAMS: i64 = -32602;
    const ERR_INTERNAL: i64 = -32603;

    fn new(code: i64, message: impl ToString) -> Self {
        Self {
            code,
            message: message.to_string(),
        }
    }

    fn internal(message: impl ToString) -> Self {
        Self::new(Self::ERR_INTERNAL, message)
    }

    fn invalid_params(message: impl ToString) -> Self {
        Self::new(Self::ERR_INVALID_PARAMS, message)
    }

    fn from_manager(e: ManagerError) -> Self {
        match e {
            ManagerError::Incompatible { .. }
            | ManagerError::UnknownRuntime(_)
            | ManagerError::UnknownModel(_) => Self::invalid_params(e),
            other => Self::internal(other),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Request {
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Deserialize)]
struct StartParams {
    runtime: String,
    model: String,
}

#[derive(Debug, Deserialize)]
struct StopParams {
    #[allow(dead_code)]
    id: String,
}

/// The control server, generic over how workers are launched and probed.
pub struct ControlServer<L: WorkerLauncher + 'static, P: ReadinessProbe + 'static> {
    manager: Arc<HarnessManager<L, P>>,
    catalog: Arc<Catalog>,
}

impl<L: WorkerLauncher + 'static, P: ReadinessProbe + 'static> ControlServer<L, P> {
    const METHOD_LIST_RUNTIMES: &'static str = "list_runtimes";
    const METHOD_LIST_MODELS: &'static str = "list_models";
    const METHOD_LIST_HARNESSES: &'static str = "list_harnesses";
    const METHOD_START: &'static str = "start";
    const METHOD_STOP: &'static str = "stop";
    const NOTIFY_CHANGED: &'static str = "harness_changed";
    const KIND_CANDLE: &'static str = "candle";
    const KIND_LLAMACPP: &'static str = "llamacpp";
    const KIND_HTTP: &'static str = "http";

    pub fn new(manager: Arc<HarnessManager<L, P>>, catalog: Arc<Catalog>) -> Self {
        Self { manager, catalog }
    }

    /// Handle one request. This is the unit-tested seam (no socket involved).
    pub async fn dispatch(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            Self::METHOD_LIST_RUNTIMES => Ok(self.list_runtimes()),
            Self::METHOD_LIST_MODELS => Ok(self.list_models()),
            Self::METHOD_LIST_HARNESSES => {
                serde_json::to_value(self.manager.list()).map_err(RpcError::internal)
            }
            Self::METHOD_START => {
                let p: StartParams =
                    serde_json::from_value(params).map_err(RpcError::invalid_params)?;
                let info = self
                    .manager
                    .start(&p.runtime, &p.model)
                    .await
                    .map_err(RpcError::from_manager)?;
                serde_json::to_value(info).map_err(RpcError::internal)
            }
            Self::METHOD_STOP => {
                // Ignoring the id value is intentional for the single-harness design; a
                // multi-harness manager would validate it here (forward-compat).
                let _p: StopParams =
                    serde_json::from_value(params).map_err(RpcError::invalid_params)?;
                self.manager.stop().await.map_err(RpcError::from_manager)?;
                Ok(json!({}))
            }
            other => Err(RpcError::new(
                RpcError::ERR_METHOD_NOT_FOUND,
                format!("unknown method: {other}"),
            )),
        }
    }

    fn kind_name(kind: RuntimeKind) -> &'static str {
        match kind {
            RuntimeKind::Candle => Self::KIND_CANDLE,
            RuntimeKind::Llamacpp => Self::KIND_LLAMACPP,
            RuntimeKind::Http => Self::KIND_HTTP,
        }
    }

    fn list_runtimes(&self) -> Value {
        let items: Vec<Value> = self
            .catalog
            .runtimes
            .iter()
            .map(|r| {
                json!({
                    "name": r.name,
                    "kind": Self::kind_name(r.kind),
                    "available": r.exe.exists(),
                })
            })
            .collect();
        Value::Array(items)
    }

    /// The hand-built JSON is a DELIBERATE field whitelist: it must never echo a path, so it is
    /// intentionally NOT a blanket `Serialize` of `DiscoveredModel` (a future path field can't leak).
    fn list_models(&self) -> Value {
        let items: Vec<Value> = discover(&self.catalog)
            .into_iter()
            .map(|m| {
                json!({
                    "display_name": m.display_name,
                    "runtimes": m.runtimes,
                    "size_hint": m.size_hint,
                })
            })
            .collect();
        Value::Array(items)
    }

    fn error_frame(id: Value, err: &RpcError) -> Value {
        json!({"id": id, "error": {"code": err.code, "message": err.message}})
    }

    /// Turn one text frame into the response frame to send back.
    async fn respond(&self, text: &str) -> Value {
        let request: Request = match serde_json::from_str(text) {
            Ok(r) => r,
            Err(e) => {
                return Self::error_frame(Value::Null, &RpcError::new(RpcError::ERR_PARSE, e));
            }
        };
        match self.dispatch(&request.method, request.params).await {
            Ok(result) => json!({"id": request.id, "result": result}),
            Err(err) => Self::error_frame(request.id, &err),
        }
    }

    async fn run_connection(&self, stream: TcpStream) -> Result<(), BoxError> {
        let mut ws = tokio_tungstenite::accept_async(stream).await?;
        let mut events = self.manager.subscribe();
        loop {
            tokio::select! {
                frame = ws.next() => {
                    let Some(frame) = frame else { return Ok(()) };
                    if let Message::Text(text) = frame? {
                        let response = self.respond(text.as_str()).await;
                        ws.send(Message::text(response.to_string())).await?;
                    }
                }
                event = events.recv() => {
                    match event {
                        Ok(info) => {
                            let note = json!({
                                "method": Self::NOTIFY_CHANGED,
                                "params": info,
                            });
                            ws.send(Message::text(note.to_string())).await?;
                        }
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => return Ok(()),
                    }
                }
            }
        }
    }

    /// Accept connections forever.
    pub async fn serve(self: Arc<Self>, listener: TcpListener) {
        loop {
            let stream = match listener.accept().await {
                Ok((stream, _)) => stream,
                Err(e) => {
                    eprintln!("control accept failed: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            };
            let server = Arc::clone(&self);
            tokio::spawn(async move {
                if let Err(e) = server.run_connection(stream).await {
                    eprintln!("control connection ended: {e}");
                }
            });
        }
    }
}

/// Accept control connections forever, serving `manager` and `catalog`.
pub async fn serve<L: WorkerLauncher + 'static, P: ReadinessProbe + 'static>(
    listener: TcpListener,
    manager: Arc<HarnessManager<L, P>>,
    catalog: Arc<Catalog>,
) {
    Arc::new(ControlServer::new(manager, catalog))
        .serve(listener)
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::catalog::RuntimeDef;
    use crate::supervisor::manager::{ChildExit, ChildSpec, ManagerConfig, WorkerHandle};
    use async_trait::async_trait;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    struct NullHandle;

    #[async_trait]
    impl WorkerHandle for NullHandle {
        fn kill(&mut self) {}
        fn try_wait(&mut self) -> Option<ChildExit> {
            None
        }
        async fn wait(&mut self) {}
    }

    struct NullLauncher;

    impl WorkerLauncher for NullLauncher {
        fn spawn(&self, _spec: &ChildSpec) -> io::Result<Box<dyn WorkerHandle>> {
            Ok(Box::new(NullHandle))
        }
    }

    struct ReadyProbe;

    #[async_trait]
    impl ReadinessProbe for ReadyProbe {
        async fn await_ready(&self, _url: &str, _timeout: Duration) -> bool {
            true
        }
    }

    fn gguf(arch: &str) -> Vec<u8> {
        let mut b = b"GGUF".to_vec();
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes());
        b.extend_from_slice(&1u64.to_le_bytes());
        let key = "general.architecture";
        b.extend_from_slice(&(key.len() as u64).to_le_bytes());
        b.extend_from_slice(key.as_bytes());
        b.extend_from_slice(&8u32.to_le_bytes());
        b.extend_from_slice(&(arch.len() as u64).to_le_bytes());
        b.extend_from_slice(arch.as_bytes());
        b
    }

    fn model_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ctl-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("gem.gguf"), gguf("gemma3")).unwrap();
        std::fs::write(d.join("lla.gguf"), gguf("llama")).unwrap();
        std::fs::write(d.join("tokenizer.json"), b"{}").unwrap();
        d
    }

    fn def(name: &str, kind: RuntimeKind, exe: &Path) -> RuntimeDef {
        RuntimeDef {
            name: name.to_string(),
            kind,
            exe: exe.to_path_buf(),
            requires_tokenizer: false,
            n_ctx: None,
            n_gpu_layers: None,
            internal_port: None,
            server_exe: None,
        }
    }

    fn catalog(dir: &Path) -> Catalog {
        // "candle" exe exists on disk (the model dir itself); "llamacpp" exe does not.
        Catalog {
            runtimes: vec![
                def("candle", RuntimeKind::Candle, dir),
                def(
                    "llamacpp",
                    RuntimeKind::Llamacpp,
                    &dir.join("no-such-llamacpp.exe"),
                ),
            ],
            model_dirs: vec![dir.to_path_buf()],
            advertise_host: "192.168.1.101".to_string(),
            worker_port: 8777,
        }
    }

    fn server(tag: &str) -> ControlServer<NullLauncher, ReadyProbe> {
        let cat = catalog(&model_dir(tag));
        let config = ManagerConfig {
            ready_timeout: Duration::from_millis(60),
            ready_grace: Duration::from_millis(20),
            poll_interval: Duration::from_millis(5),
            health_interval: Duration::from_secs(3600),
        };
        let manager = HarnessManager::new(cat.clone(), NullLauncher, ReadyProbe, config);
        ControlServer::new(Arc::new(manager), Arc::new(cat))
    }

    #[tokio::test]
    async fn list_runtimes_reports_availability() {
        let s = server("rt");
        let v = s.dispatch("list_runtimes", Value::Null).await.unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr[0]["name"], "candle");
        assert_eq!(arr[0]["kind"], "candle");
        assert_eq!(arr[0]["available"], true);
        assert_eq!(arr[1]["name"], "llamacpp");
        assert_eq!(arr[1]["available"], false);
    }

    #[tokio::test]
    async fn start_returns_info_with_url() {
        let s = server("start");
        let v = s
            .dispatch("start", json!({"runtime": "candle", "model": "gem"}))
            .await
            .unwrap();
        assert_eq!(v["id"], "candle:gem");
        assert_eq!(v["state"], "ready");
        assert_eq!(v["url"], "ws://192.168.1.101:8777");
    }

    #[tokio::test]
    async fn start_rejects_incompatible_pair() {
        let s = server("incompat");
        let err = s
            .dispatch("start", json!({"runtime": "candle", "model": "lla"}))
            .await
            .unwrap_err();
        assert_eq!(err.code, -32602);
        assert!(!err.message.is_empty());
    }

    #[tokio::test]
    async fn stop_calls_manager() {
        let s = server("stop");
        s.dispatch("start", json!({"runtime": "candle", "model": "gem"}))
            .await
            .unwrap();
        let v = s
            .dispatch("stop", json!({"id": "candle:gem"}))
            .await
            .unwrap();
        assert_eq!(v, json!({}));
        let list = s.dispatch("list_harnesses", Value::Null).await.unwrap();
        assert_eq!(list, json!([]));
    }

    #[tokio::test]
    async fn unknown_method_errors() {
        let s = server("unk");
        let err = s.dispatch("bogus", Value::Null).await.unwrap_err();
        assert_eq!(err.code, -32601);
    }

    #[tokio::test]
    async fn list_models_omits_absolute_paths() {
        let s = server("models");
        let v = s.dispatch("list_models", Value::Null).await.unwrap();
        assert!(!v.as_array().unwrap().is_empty());
        let text = v.to_string();
        assert!(!text.contains('/') && !text.contains('\\'), "{text}");
        assert!(!text.contains(":\\") && !text.contains("ctl-"), "{text}");
    }
}
