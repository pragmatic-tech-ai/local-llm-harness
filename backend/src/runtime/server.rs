//! The agent runtime's JSON-RPC websocket server: one `RuntimeSession` per client connection,
//! `send_turn` drives the tool loop and streams its notifications while it runs.
//!
//! Requests on one connection run strictly one after another. A client that disconnects during
//! `send_turn` is only noticed when the next notification write fails; the loop then stops and
//! the turn is not committed. Cancelling a turn from the client is out of scope for this slice:
//! the design spec's non-goals and extension points do not include it, so it needs a spec
//! addition before it is built.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::net::TcpListener;

use super::registry::ToolRegistry;
use super::session::RuntimeSession;
use super::tool_loop::{LoopSink, SinkClosed, ToolLoop};
use super::worker::WorkerConnector;
use crate::rpc::{
    Failure, FrameLoop, Frames, Request, RpcDispatch, RpcError, SendTurnParams, SetSamplingParams,
    TokenizeParams, Ws, ERR_METHOD_NOT_FOUND,
};

/// Everything a connection needs besides its own session.
#[derive(Clone)]
pub struct RuntimeDeps {
    pub connector: Arc<dyn WorkerConnector>,
    pub registry: Arc<ToolRegistry>,
    pub tool_loop: Arc<ToolLoop>,
}

/// Accepts websocket connections forever; each gets its own session.
pub struct RuntimeServer;

impl RuntimeServer {
    const ACCEPT_RETRY: std::time::Duration = std::time::Duration::from_millis(100);

    pub async fn serve(listener: TcpListener, deps: RuntimeDeps) {
        loop {
            let stream = match listener.accept().await {
                Ok((stream, _)) => stream,
                Err(e) => {
                    eprintln!("runtime accept failed: {e}");
                    tokio::time::sleep(Self::ACCEPT_RETRY).await;
                    continue;
                }
            };
            let mut connection = RuntimeConnection::new(deps.clone());
            tokio::spawn(async move {
                if let Err(e) = FrameLoop::run(&mut connection, stream).await {
                    eprintln!("runtime connection ended: {e}");
                }
            });
        }
    }
}

/// Writes the loop's notifications to the client's socket as they happen.
struct WsSink<'a> {
    ws: &'a mut Ws,
}

#[async_trait]
impl LoopSink for WsSink<'_> {
    async fn notify(&mut self, method: &str, params: Value) -> Result<(), SinkClosed> {
        Frames::send(self.ws, Frames::notification(method, params))
            .await
            .map_err(|_| SinkClosed)
    }
}

/// One client connection. Requests are handled sequentially.
pub struct RuntimeConnection {
    deps: RuntimeDeps,
    session: RuntimeSession,
    closed: bool,
}

impl RuntimeConnection {
    const CLIENT_GONE: &'static str = "client connection closed";

    pub fn new(deps: RuntimeDeps) -> Self {
        Self {
            deps,
            session: RuntimeSession::new(),
            closed: false,
        }
    }

    /// Handles one request. Notifications go to `sink` while it runs; the returned value is the
    /// request's own response. A closed sink marks the connection finished.
    pub(crate) async fn handle_request(
        &mut self,
        method: &str,
        params: Value,
        sink: &mut dyn LoopSink,
    ) -> Result<Value, RpcError> {
        match method {
            "model_props" => {
                let link = self.deps.connector.connect().ok_or_else(Self::no_model)?;
                link.model_props()
                    .await
                    .map_err(|e| RpcError::internal(e.0))
            }
            "tokenize" => {
                let p: TokenizeParams = Frames::params(params)?;
                let link = self.deps.connector.connect().ok_or_else(Self::no_model)?;
                link.tokenize(&p.text)
                    .await
                    .map_err(|e| RpcError::internal(e.0))
            }
            "set_sampling" => {
                let p: SetSamplingParams = Frames::params(params)?;
                self.session.sampling = p.config;
                Ok(Value::Null)
            }
            "send_turn" => {
                let p: SendTurnParams = Frames::params(params)?;
                let ran = self
                    .deps
                    .tool_loop
                    .run(
                        &mut self.session,
                        &p.content,
                        p.enable_thinking,
                        self.deps.connector.as_ref(),
                        sink,
                    )
                    .await;
                match ran {
                    Ok(_) => Ok(Value::Null),
                    Err(SinkClosed) => {
                        self.closed = true;
                        Err(RpcError::internal(Self::CLIENT_GONE))
                    }
                }
            }
            "reset" => {
                self.session.reset();
                Ok(Value::Null)
            }
            "list_tools" => {
                serde_json::to_value(self.deps.registry.specs()).map_err(RpcError::internal)
            }
            other => Err(RpcError::new(
                ERR_METHOD_NOT_FOUND,
                format!("unknown method: {other}"),
            )),
        }
    }

    fn no_model() -> RpcError {
        RpcError::internal(ToolLoop::NO_MODEL)
    }
}

#[async_trait]
impl RpcDispatch for RuntimeConnection {
    /// A closed client ends the connection without a reply.
    async fn dispatch(&mut self, ws: &mut Ws, request: &Request) -> Result<Value, Failure> {
        let mut sink = WsSink { ws };
        let outcome = self
            .handle_request(&request.method, request.params.clone(), &mut sink)
            .await;
        if self.closed {
            return Err(Failure::Closed);
        }
        outcome.map_err(Failure::Rpc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::SamplingConfig;
    use crate::rpc::test_support::TestClient;
    use crate::rpc::{ERR_INVALID_PARAMS, ERR_PARSE};
    use crate::runtime::executor::ToolExecutor;
    use crate::runtime::policy::ReadOnlyPolicy;
    use crate::runtime::test_support::{FakeConnector, FakeLink, Notes, RecordingSink, Scripted};
    use crate::runtime::test_tools::TestSource;
    use serde_json::json;
    use std::time::Duration;

    struct Fixture {
        connection: RuntimeConnection,
        sink: RecordingSink,
    }

    impl Fixture {
        fn new(link: Option<Arc<FakeLink>>) -> Self {
            let registry =
                Arc::new(ToolRegistry::new(vec![TestSource::echo_boxed()]).expect("registry"));
            let executor = Arc::new(ToolExecutor::new(
                registry.clone(),
                Box::new(ReadOnlyPolicy),
                Duration::from_secs(5),
            ));
            let deps = RuntimeDeps {
                connector: Arc::new(FakeConnector::new(link)),
                registry: registry.clone(),
                tool_loop: Arc::new(ToolLoop::new(executor, registry, 8)),
            };
            Self {
                connection: RuntimeConnection::new(deps),
                sink: RecordingSink::new(),
            }
        }

        fn deps(link: Option<Arc<FakeLink>>) -> RuntimeDeps {
            Self::new(link).connection.deps
        }

        async fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
            self.connection
                .handle_request(method, params, &mut self.sink)
                .await
        }

        fn answer(text: &str) -> Scripted {
            Notes::script(vec![Notes::started(), Notes::done(text, vec![])])
        }
    }

    #[tokio::test]
    async fn list_tools_returns_specs() {
        let mut f = Fixture::new(None);
        let v = f.call("list_tools", Value::Null).await.unwrap();
        assert_eq!(v[0]["name"], "echo");
        assert_eq!(v.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn set_sampling_is_used_by_send_turn() {
        let link = FakeLink::new(true, vec![Fixture::answer("hi")]);
        let mut f = Fixture::new(Some(link.clone()));
        let config = SamplingConfig {
            temperature: 0.25,
            ..SamplingConfig::default()
        };
        f.call("set_sampling", json!({"config": config}))
            .await
            .unwrap();
        f.call("send_turn", json!({"content": "q"})).await.unwrap();
        assert_eq!(link.requests()[0].sampling, config);
    }

    #[tokio::test]
    async fn reset_clears_transcript() {
        let link = FakeLink::new(true, vec![Fixture::answer("a"), Fixture::answer("b")]);
        let mut f = Fixture::new(Some(link.clone()));
        f.call("send_turn", json!({"content": "q1"})).await.unwrap();
        f.call("send_turn", json!({"content": "q2"})).await.unwrap();
        assert_eq!(link.requests()[1].messages.len(), 3);
        f.call("reset", Value::Null).await.unwrap();
        assert!(f.connection.session.transcript().is_empty());
    }

    #[tokio::test]
    async fn model_props_without_harness_errors() {
        let mut f = Fixture::new(None);
        let err = f.call("model_props", Value::Null).await.unwrap_err();
        assert_eq!(err.message, "no model running");
        let err = f.call("tokenize", json!({"text": "x"})).await.unwrap_err();
        assert_eq!(err.message, "no model running");
    }

    #[tokio::test]
    async fn model_props_and_tokenize_are_proxied() {
        let link = FakeLink::new(true, vec![]);
        let mut f = Fixture::new(Some(link));
        let v = f.call("model_props", Value::Null).await.unwrap();
        assert_eq!(v, json!({"supports_tools": true}));
        let v = f.call("tokenize", json!({"text": "x"})).await.unwrap();
        assert_eq!(v, json!({"count": 0}));
    }

    #[tokio::test]
    async fn send_turn_result_comes_after_loop_done() {
        let link = FakeLink::new(true, vec![Fixture::answer("hi")]);
        let mut f = Fixture::new(Some(link));
        let v = f.call("send_turn", json!({"content": "q"})).await.unwrap();
        assert_eq!(v, Value::Null);
        assert_eq!(f.sink.notes.last().unwrap().method, "loop_done");
    }

    #[tokio::test]
    async fn unknown_method_and_bad_params_use_rpc_codes() {
        let mut f = Fixture::new(None);
        let err = f.call("nope", Value::Null).await.unwrap_err();
        assert_eq!(err.code, ERR_METHOD_NOT_FOUND);
        let err = f.call("send_turn", json!({})).await.unwrap_err();
        assert_eq!(err.code, ERR_INVALID_PARAMS);
    }

    #[tokio::test]
    async fn closed_sink_ends_the_connection() {
        let link = FakeLink::new(true, vec![Fixture::answer("hi")]);
        let mut f = Fixture::new(Some(link));
        f.sink = RecordingSink::closing_after(0);
        f.call("send_turn", json!({"content": "q"}))
            .await
            .unwrap_err();
        assert!(f.connection.closed);
    }

    #[tokio::test]
    async fn serve_streams_notifications_before_the_response() {
        let link = FakeLink::new(true, vec![Fixture::answer("hi")]);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(RuntimeServer::serve(listener, Fixture::deps(Some(link))));
        let mut client = TestClient::connect_to(addr).await;
        let (notes, resp) = client.request("send_turn", json!({"content": "q"})).await;
        assert_eq!(resp["result"], Value::Null);
        let methods = TestClient::methods(&notes);
        assert_eq!(methods.last().map(String::as_str), Some("loop_done"));
        assert!(methods.contains(&"turn_done".to_string()));
        let reply = client.send_raw("not json").await;
        assert_eq!(reply["id"], Value::Null);
        assert_eq!(reply["error"]["code"], ERR_PARSE);
    }
}
