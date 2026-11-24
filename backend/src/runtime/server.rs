//! The agent runtime's JSON-RPC websocket server (`:8779`), session-aware.
//!
//! Conversations are persisted sessions started from agent templates. A connection attaches to
//! at most one session (`attach_session`); the chat methods (`send_turn`, `set_sampling`,
//! `set_thinking`, `model_props`, `tokenize`, `list_tools`, `reset`) act on it and fail
//! `no session attached` otherwise. Template, catalog and session-list methods need no
//! attachment.
//!
//! Every frame for a connection (responses and notifications) goes through its one outbound
//! channel in the [`Hub`], drained by a writer task, so frames never interleave and a
//! notification caused by another connection never blocks. Requests on one connection run
//! concurrently and responses are matched by id: each request is started as it arrives and
//! finishes in its own task, so a running turn or model load never holds up the template,
//! session-list or catalog methods behind it. Attaching and the chat methods do their part
//! that reads or changes the attachment, the session's overrides or its busy flag when they
//! are started, so they take effect in arrival order: a `set_sampling` sent before `send_turn`
//! is applied first, and an `attach_session` sent after `send_turn` does not retarget it. A
//! request whose handling panics is answered with an internal error (`request failed`); a
//! busy flag it held is released as it unwinds.
//! List-level notifications (`sessions_changed`, `templates_changed`, `placement_changed`,
//! `session_busy`) reach every connection; a turn's notifications and `session_loading` reach
//! only the connections attached to its session. A committed turn broadcasts
//! `sessions_changed` (its `updated` time and list order changed).
//!
//! A turn and a `reset` each hold the session's busy flag, so they exclude each other.
//! `unload_session_model` is allowed while a turn runs: the turn's next step finds no model and
//! ends `Error` with `no model running`. A turn is bound to the harness its first step ran on:
//! if the host serves another harness by a later step (a reload of the same model included),
//! the turn ends `Error` with `model changed`.
//!
//! A turn keeps running while at least one connection attached to its session is still there,
//! even if the connection that sent it went away or attached elsewhere (the others keep
//! receiving its notifications and the transcript is committed). When no attached connection
//! remains, the turn aborts like a closed sink: the in-flight generation is dropped and nothing
//! is committed. A gone client is only noticed when a write to it fails. Cancelling a turn from
//! the client is out of scope.
//!
//! The catalog is read through [`CatalogSource`] and the template rules through the template
//! store's source, so `list_catalog`, `save_template` and `create_session` see models dropped
//! on disk; the last two (and `list_catalog`) also refresh the hosts' model lists.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use futures::stream::SplitSink;
use futures::{FutureExt, SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;

pub use super::catalog_source::{CatalogSource, CatalogView};
use super::hub::{ConnId, Hub};
use super::registry::ToolRegistry;
use super::tool_loop::{LoopSink, SinkClosed, StepLink, ToolLoop, TurnConfig, TurnLinks};
use super::trace::NoTrace;
use super::worker::WorkerLink;
use crate::agents::dispatcher::{Dispatcher, Placement};
use crate::agents::host::{HarnessIdentity, ModelRequirement};
use crate::agents::session::{SessionRecord, SessionSummary};
use crate::agents::session_store::{SessionError, SessionStore};
use crate::agents::template::AgentTemplate;
use crate::agents::template_store::{TemplateError, TemplateStore};
use crate::engine::SamplingConfig;
use crate::rpc::{
    BoxError, Frames, RpcError, SetSamplingParams, TokenizeParams, Ws, ERR_METHOD_NOT_FOUND,
};

/// A started request's remainder, run in its own task.
type Pending = Pin<Box<dyn Future<Output = Result<Value, RpcError>> + Send + 'static>>;

/// Everything the runtime server shares between connections.
#[derive(Clone)]
pub struct RuntimeDeps {
    pub templates: Arc<TemplateStore>,
    pub sessions: Arc<SessionStore>,
    pub dispatcher: Arc<Dispatcher>,
    pub registry: Arc<ToolRegistry>,
    pub tool_loop: Arc<ToolLoop>,
    pub catalog: Arc<dyn CatalogSource>,
    pub hub: Arc<Hub>,
}

/// Accepts websocket connections forever.
pub struct RuntimeServer;

impl RuntimeServer {
    const ACCEPT_RETRY: std::time::Duration = std::time::Duration::from_millis(100);
    /// Broadcast after a session is created, renamed or deleted.
    pub const SESSIONS_CHANGED: &'static str = "sessions_changed";
    /// Broadcast after any template or group change.
    pub const TEMPLATES_CHANGED: &'static str = "templates_changed";
    /// Broadcast `{id, placement}` when a session's placement changes.
    pub const PLACEMENT_CHANGED: &'static str = "placement_changed";
    /// Sent `{id}` to a session's connections when its turn has to load the model first.
    pub const SESSION_LOADING: &'static str = "session_loading";

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
            let (connection, frames) = RuntimeConnection::new(deps.clone());
            tokio::spawn(async move {
                if let Err(e) = connection.run(stream, frames).await {
                    eprintln!("runtime connection ended: {e}");
                }
            });
        }
    }
}

/// The links for one turn of one session. Step 0 may load the model (announcing it with
/// `session_loading`) and records the harness it got; later steps only use the model if that
/// same harness still serves it, so an eviction or a model swap mid-turn ends the turn with an
/// error.
struct SessionLinks<'a> {
    dispatcher: &'a Dispatcher,
    hub: &'a Hub,
    session: &'a str,
    req: ModelRequirement,
    /// The harness step 0 ran on.
    bound: Mutex<Option<HarnessIdentity>>,
}

impl<'a> SessionLinks<'a> {
    /// Reported when a later step finds another harness than the one the turn started on.
    const MODEL_CHANGED: &'static str = "model changed";

    fn new(
        dispatcher: &'a Dispatcher,
        hub: &'a Hub,
        session: &'a str,
        req: ModelRequirement,
    ) -> Self {
        Self {
            dispatcher,
            hub,
            session,
            req,
            bound: Mutex::new(None),
        }
    }

    fn bound(&self) -> std::sync::MutexGuard<'_, Option<HarnessIdentity>> {
        self.bound.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A step link with no load time measured (Task 4 measures the step-0 load).
    fn plain(link: Arc<dyn WorkerLink>) -> StepLink {
        StepLink { link, load: None }
    }
}

#[async_trait]
impl TurnLinks for SessionLinks<'_> {
    async fn link(&self, step: usize) -> Result<StepLink, String> {
        if step == 0 {
            let link = self
                .dispatcher
                .link_for_turn(&self.req, || {
                    self.hub.to_session(
                        self.session,
                        RuntimeServer::SESSION_LOADING,
                        json!({ "id": self.session }),
                    );
                })
                .await
                .map_err(|e| e.0)?;
            *self.bound() = self.dispatcher.identity(&self.req);
            return Ok(Self::plain(link));
        }
        let bound = self.bound().clone();
        if let (Some(bound), Some(now)) = (bound, self.dispatcher.identity(&self.req)) {
            if bound != now {
                return Err(Self::MODEL_CHANGED.to_string());
            }
        }
        self.dispatcher
            .current_link(&self.req)
            .map(Self::plain)
            .ok_or_else(|| ToolLoop::NO_MODEL.to_string())
    }
}

/// Delivers a turn's notifications to every connection attached to its session; closed once
/// none is left.
struct SessionSink<'a> {
    hub: &'a Hub,
    session: &'a str,
}

#[async_trait]
impl LoopSink for SessionSink<'_> {
    async fn notify(&mut self, method: &str, params: Value) -> Result<(), SinkClosed> {
        match self.hub.to_session(self.session, method, params) {
            0 => Err(SinkClosed),
            _ => Ok(()),
        }
    }
}

/// A `list_sessions` row: the summary's fields plus the live placement and busy flag.
#[derive(Debug, Serialize)]
struct SessionRow {
    #[serde(flatten)]
    summary: SessionSummary,
    placement: Placement,
    busy: bool,
}

/// Session-derived values the server computes.
struct SessionView;

impl SessionView {
    /// The override, else the snapshot's sampling, else the default.
    fn sampling(rec: &SessionRecord) -> SamplingConfig {
        rec.sampling
            .clone()
            .or_else(|| rec.snapshot.sampling.clone())
            .unwrap_or_default()
    }

    /// The override, else the snapshot's setting.
    fn thinking(rec: &SessionRecord) -> bool {
        rec.enable_thinking.unwrap_or(rec.snapshot.enable_thinking)
    }

    fn row(deps: &RuntimeDeps, summary: SessionSummary, req: &ModelRequirement) -> SessionRow {
        SessionRow {
            placement: deps.dispatcher.status(req),
            busy: deps.hub.is_busy(&summary.id),
            summary,
        }
    }

    fn row_of(deps: &RuntimeDeps, rec: &SessionRecord) -> SessionRow {
        Self::row(deps, rec.summary(), &Dispatcher::requirement(rec))
    }

    /// Every session's placement, in list order (no transcript is cloned).
    fn placements(sessions: &SessionStore, dispatcher: &Dispatcher) -> Vec<(String, Placement)> {
        sessions
            .list_with_requirements()
            .into_iter()
            .map(|(summary, req)| (summary.id, dispatcher.status(&req)))
            .collect()
    }
}

/// Watches the dispatcher's signal and broadcasts `placement_changed` for every session whose
/// placement differs from the one recorded in the hub. `create_session` seeds the new
/// session's placement there, so a change before the next signal is still announced. Stops when
/// the dispatcher is dropped.
pub struct PlacementWatcher {
    sessions: Arc<SessionStore>,
    dispatcher: Weak<Dispatcher>,
    hub: Arc<Hub>,
}

impl PlacementWatcher {
    /// Subscribes and records the current placements before returning, so no change after this
    /// call is missed. Must be called inside a tokio runtime.
    pub fn spawn(deps: &RuntimeDeps) -> tokio::task::JoinHandle<()> {
        let mut events = deps.dispatcher.subscribe();
        let mut watcher = Self::new(deps);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                        if !watcher.publish() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    }

    /// A watcher that has recorded the current placements (silently).
    fn new(deps: &RuntimeDeps) -> Self {
        deps.hub
            .update_placements(|| SessionView::placements(&deps.sessions, &deps.dispatcher));
        PlacementWatcher {
            sessions: Arc::clone(&deps.sessions),
            dispatcher: Arc::downgrade(&deps.dispatcher),
            hub: Arc::clone(&deps.hub),
        }
    }

    /// Broadcasts the changes since the recorded placements; `false` once the dispatcher is
    /// gone.
    fn publish(&mut self) -> bool {
        let Some(dispatcher) = self.dispatcher.upgrade() else {
            return false;
        };
        let changed = self
            .hub
            .update_placements(|| SessionView::placements(&self.sessions, &dispatcher));
        for (id, placement) in changed {
            self.hub.broadcast(
                RuntimeServer::PLACEMENT_CHANGED,
                json!({ "id": id, "placement": placement }),
            );
        }
        true
    }
}

#[derive(Deserialize)]
struct IdParams {
    id: String,
}

#[derive(Deserialize)]
struct PathParams {
    path: String,
}

#[derive(Deserialize)]
struct SaveTemplateParams {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    path: String,
    template: AgentTemplate,
}

#[derive(Deserialize)]
struct RenameGroupParams {
    path: String,
    new_name: String,
}

#[derive(Deserialize)]
struct MoveTemplateParams {
    id: String,
    path: String,
}

#[derive(Deserialize)]
struct CreateSessionParams {
    template_id: String,
}

#[derive(Deserialize)]
struct RenameSessionParams {
    id: String,
    title: String,
}

#[derive(Deserialize)]
struct ContentParams {
    content: String,
}

#[derive(Deserialize)]
struct SetThinkingParams {
    enable: bool,
}

/// One client connection. Each request is started as it arrives (see the module docs) and
/// finished in its own task; dropping the connection unregisters it from the hub.
pub struct RuntimeConnection {
    deps: RuntimeDeps,
    conn: ConnId,
}

impl RuntimeConnection {
    const M_LIST_TEMPLATES: &'static str = "list_templates";
    const M_GET_TEMPLATE: &'static str = "get_template";
    const M_SAVE_TEMPLATE: &'static str = "save_template";
    const M_DELETE_TEMPLATE: &'static str = "delete_template";
    const M_CREATE_GROUP: &'static str = "create_group";
    const M_RENAME_GROUP: &'static str = "rename_group";
    const M_DELETE_GROUP: &'static str = "delete_group";
    const M_MOVE_TEMPLATE: &'static str = "move_template";
    const M_LIST_CATALOG: &'static str = "list_catalog";
    const M_LIST_ALL_TOOLS: &'static str = "list_all_tools";
    const M_LIST_SESSIONS: &'static str = "list_sessions";
    const M_CREATE_SESSION: &'static str = "create_session";
    const M_ATTACH_SESSION: &'static str = "attach_session";
    const M_RENAME_SESSION: &'static str = "rename_session";
    const M_DELETE_SESSION: &'static str = "delete_session";
    const M_LOAD_SESSION_MODEL: &'static str = "load_session_model";
    const M_UNLOAD_SESSION_MODEL: &'static str = "unload_session_model";
    const M_SEND_TURN: &'static str = "send_turn";
    const M_SET_SAMPLING: &'static str = "set_sampling";
    const M_SET_THINKING: &'static str = "set_thinking";
    const M_MODEL_PROPS: &'static str = "model_props";
    const M_TOKENIZE: &'static str = "tokenize";
    const M_LIST_TOOLS: &'static str = "list_tools";
    const M_RESET: &'static str = "reset";
    const ERR_NO_SESSION: &'static str = "no session attached";
    const ERR_BUSY: &'static str = "session is busy";
    const ERR_UNKNOWN_METHOD: &'static str = "unknown method: ";
    /// The sender's own session was deleted while its turn ran.
    const ERR_DELETED: &'static str = "session deleted";
    /// The client was shown the answer, but committing it failed.
    const ERR_NOT_SAVED: &'static str = "answer shown but not saved: ";
    const CLIENT_GONE: &'static str = "client connection closed";
    /// The sender attached to another session while its turn ran, leaving the turn no sink.
    const ERR_DETACHED: &'static str = "session detached";
    /// A request's handling panicked.
    const ERR_REQUEST_FAILED: &'static str = "request failed";

    /// Registers a connection with the hub; the receiver yields every frame to write to it.
    pub fn new(deps: RuntimeDeps) -> (Self, mpsc::UnboundedReceiver<Value>) {
        let (conn, frames) = deps.hub.register();
        (Self { deps, conn }, frames)
    }

    /// Runs the websocket: a writer task drains `frames` to the socket while this task reads
    /// requests. Each request is started here, in arrival order, and finished in its own task,
    /// which queues the response behind whatever notifications were queued before it.
    pub async fn run(
        self,
        stream: TcpStream,
        frames: mpsc::UnboundedReceiver<Value>,
    ) -> Result<(), BoxError> {
        let ws = tokio_tungstenite::accept_async(stream).await?;
        let (write, mut read) = ws.split();
        tokio::spawn(Self::write_frames(write, frames));
        while let Some(frame) = read.next().await {
            let Message::Text(text) = frame? else {
                continue;
            };
            let request = match Frames::decode(text.as_str()) {
                Ok(r) => r,
                Err(reply) => {
                    if !self.deps.hub.send(self.conn, reply) {
                        break;
                    }
                    continue;
                }
            };
            let pending = Self::guarded(self.start(&request.method, request.params));
            let id = request.id;
            let hub = Arc::clone(&self.deps.hub);
            let conn = self.conn;
            tokio::spawn(async move {
                // A gone connection just drops the reply.
                hub.send(conn, Frames::reply(id, pending.await));
            });
        }
        Ok(())
    }

    /// Writes queued frames until the channel ends (the connection unregistered) or the socket
    /// fails; then the receiver drops, so the hub sees the connection as gone.
    async fn write_frames(
        mut write: SplitSink<Ws, Message>,
        mut frames: mpsc::UnboundedReceiver<Value>,
    ) {
        while let Some(frame) = frames.recv().await {
            if write.send(Message::text(frame.to_string())).await.is_err() {
                return;
            }
        }
        let _ = write.close().await;
    }

    /// Handles one request to completion (tests drive the connection through this); `run`
    /// starts it the same way and awaits the rest in a task.
    #[cfg(test)]
    pub(crate) async fn dispatch(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        Self::guarded(self.start(method, params)).await
    }

    /// `pending`, with a panic in it answered as an internal error (`request failed`) instead
    /// of leaving the request unanswered. Unwinding drops the future's state first, so a busy
    /// guard it held is released.
    fn guarded(pending: Pending) -> Pending {
        Box::pin(async move {
            std::panic::AssertUnwindSafe(pending)
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err(RpcError::internal(Self::ERR_REQUEST_FAILED)))
        })
    }

    /// Starts one request. Attaching and the chat methods do everything that reads or changes
    /// this connection's attachment, the session's overrides or its busy flag now; the returned
    /// future does the rest (the turn, a worker call). Every other method runs entirely in the
    /// future, on the shared state only.
    fn start(&self, method: &str, params: Value) -> Pending {
        match method {
            Self::M_ATTACH_SESSION => Self::done(self.attach_session(params)),
            Self::M_SET_SAMPLING => Self::done(self.set_sampling(params)),
            Self::M_SET_THINKING => Self::done(self.set_thinking(params)),
            Self::M_LIST_TOOLS => Self::done(self.list_tools()),
            Self::M_RESET => Self::done(self.reset()),
            Self::M_SEND_TURN => self.start_turn(params).unwrap_or_else(Self::failed),
            Self::M_MODEL_PROPS => match self.attached_link() {
                Ok(link) => Box::pin(async move {
                    link.model_props()
                        .await
                        .map_err(|e| RpcError::internal(e.0))
                }),
                Err(e) => Self::failed(e),
            },
            Self::M_TOKENIZE => match Frames::params::<TokenizeParams>(params)
                .and_then(|p| Ok((p, self.attached_link()?)))
            {
                Ok((p, link)) => Box::pin(async move {
                    link.tokenize(&p.text)
                        .await
                        .map_err(|e| RpcError::internal(e.0))
                }),
                Err(e) => Self::failed(e),
            },
            _ => {
                let shared = SharedRequests {
                    deps: self.deps.clone(),
                };
                let method = method.to_string();
                Box::pin(async move { shared.handle(&method, params).await })
            }
        }
    }

    fn done(outcome: Result<Value, RpcError>) -> Pending {
        Box::pin(std::future::ready(outcome))
    }

    fn failed(e: RpcError) -> Pending {
        Self::done(Err(e))
    }

    fn attach_session(&self, params: Value) -> Result<Value, RpcError> {
        let p: IdParams = Frames::params(params)?;
        let rec = SharedRequests::record(&self.deps, &p.id)?;
        self.deps.hub.attach(self.conn, Some(rec.id.clone()));
        let row = SessionView::row_of(&self.deps, &rec);
        Ok(json!({
            "placement": row.placement.clone(),
            "summary": row,
            "snapshot": rec.snapshot,
            "transcript": rec.transcript,
            "sampling": SessionView::sampling(&rec),
            "enable_thinking": SessionView::thinking(&rec),
        }))
    }

    fn set_sampling(&self, params: Value) -> Result<Value, RpcError> {
        let rec = self.attached()?;
        let p: SetSamplingParams = Frames::params(params)?;
        self.set_overrides(&rec, Some(p.config), rec.enable_thinking)
    }

    fn set_thinking(&self, params: Value) -> Result<Value, RpcError> {
        let rec = self.attached()?;
        let p: SetThinkingParams = Frames::params(params)?;
        self.set_overrides(&rec, rec.sampling.clone(), Some(p.enable))
    }

    fn list_tools(&self) -> Result<Value, RpcError> {
        let rec = self.attached()?;
        let tools: Vec<_> = self
            .deps
            .registry
            .specs()
            .into_iter()
            .filter(|s| rec.snapshot.tools.contains(&s.name))
            .collect();
        SharedRequests::to_value(tools)
    }

    fn reset(&self) -> Result<Value, RpcError> {
        let id = self.attached_session()?;
        // Held for the reset, so a reset and a turn exclude each other.
        let _busy = self
            .deps
            .hub
            .hold_busy(&id)
            .ok_or_else(|| RpcError::internal(Self::ERR_BUSY))?;
        let rec = SharedRequests::record(&self.deps, &id).map_err(|_| Self::no_session())?;
        self.deps
            .sessions
            .reset(&rec.id)
            .map_err(RpcError::internal)?;
        Ok(Value::Null)
    }

    /// Starts one user message on the attached session: takes its busy flag and reads its
    /// record now; the returned future runs the turn, holding busy until it ends.
    fn start_turn(&self, params: Value) -> Result<Pending, RpcError> {
        let id = self.attached_session()?;
        let p: ContentParams = Frames::params(params)?;
        let busy = self
            .deps
            .hub
            .hold_busy(&id)
            .ok_or_else(|| RpcError::internal(Self::ERR_BUSY))?;
        // Read after taking busy, so no other turn or reset changes it underneath.
        let rec = SharedRequests::record(&self.deps, &id).map_err(|_| Self::no_session())?;
        let deps = self.deps.clone();
        let conn = self.conn;
        Ok(Box::pin(async move {
            let _busy = busy;
            Self::run_turn(&deps, conn, &id, rec, &p.content).await
        }))
    }

    /// Runs the turn sent by `conn`. The transcript is committed only when the loop ends
    /// `Answered`, after `loop_done`.
    async fn run_turn(
        deps: &RuntimeDeps,
        conn: ConnId,
        id: &str,
        rec: SessionRecord,
        content: &str,
    ) -> Result<Value, RpcError> {
        let config = TurnConfig {
            step_budget: rec.snapshot.step_budget,
            tools: rec.snapshot.tools.clone(),
            enable_thinking: SessionView::thinking(&rec),
            sampling: SessionView::sampling(&rec),
        };
        let links = SessionLinks::new(
            &deps.dispatcher,
            &deps.hub,
            id,
            Dispatcher::requirement(&rec),
        );
        let mut sink = SessionSink {
            hub: &deps.hub,
            session: id,
        };
        let ran = deps
            .tool_loop
            .run(
                &rec.transcript,
                content,
                &config,
                &links,
                &mut NoTrace,
                &mut sink,
            )
            .await;
        match ran {
            Ok((_, Some(transcript))) => {
                deps.sessions
                    .commit(id, transcript)
                    .map_err(|e| RpcError::internal(format!("{}{e}", Self::ERR_NOT_SAVED)))?;
                // The commit bumped `updated`, so the list order changed.
                SharedRequests::sessions_changed(deps)
            }
            Ok((_, None)) => Ok(Value::Null),
            // Deleting a session detaches its connections, which closes the turn's sink.
            Err(SinkClosed) if !deps.sessions.contains(id) => {
                Err(RpcError::internal(Self::ERR_DELETED))
            }
            // The sender is still connected but attached elsewhere, and no one else was.
            Err(SinkClosed) if deps.hub.attached(conn).is_some_and(|s| s != id) => {
                Err(RpcError::internal(Self::ERR_DETACHED))
            }
            Err(SinkClosed) => Err(RpcError::internal(Self::CLIENT_GONE)),
        }
    }

    fn set_overrides(
        &self,
        rec: &SessionRecord,
        sampling: Option<SamplingConfig>,
        enable_thinking: Option<bool>,
    ) -> Result<Value, RpcError> {
        self.deps
            .sessions
            .set_overrides(&rec.id, sampling, enable_thinking)
            .map_err(RpcError::internal)?;
        Ok(Value::Null)
    }

    fn attached_id(&self) -> Result<String, RpcError> {
        self.deps
            .hub
            .attached(self.conn)
            .ok_or_else(Self::no_session)
    }

    /// The attached session's id, only if the session still exists (so no busy flag is ever
    /// taken, or broadcast, for a deleted id). Callers re-read the record once they hold busy.
    fn attached_session(&self) -> Result<String, RpcError> {
        let id = self.attached_id()?;
        if !self.deps.sessions.contains(&id) {
            return Err(Self::no_session());
        }
        Ok(id)
    }

    /// The attached session's record; a session deleted meanwhile counts as none attached.
    fn attached(&self) -> Result<SessionRecord, RpcError> {
        let id = self.attached_id()?;
        self.deps.sessions.get(&id).ok_or_else(Self::no_session)
    }

    /// The link to the attached session's model, only if it is loaded now.
    fn attached_link(&self) -> Result<Arc<dyn WorkerLink>, RpcError> {
        let rec = self.attached()?;
        self.deps
            .dispatcher
            .current_link(&Dispatcher::requirement(&rec))
            .ok_or_else(|| RpcError::internal(ToolLoop::NO_MODEL))
    }

    fn no_session() -> RpcError {
        RpcError::internal(Self::ERR_NO_SESSION)
    }
}

impl Drop for RuntimeConnection {
    fn drop(&mut self) {
        self.deps.hub.unregister(self.conn);
    }
}

/// The methods that need no attachment (templates, catalog, session list and model loads):
/// they touch only the shared state, so they run entirely in their own task and never wait
/// behind a turn or a load on the same connection.
struct SharedRequests {
    deps: RuntimeDeps,
}

impl SharedRequests {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let deps = &self.deps;
        match method {
            RuntimeConnection::M_LIST_TEMPLATES => Self::to_value(deps.templates.tree()),
            RuntimeConnection::M_GET_TEMPLATE => self.get_template(params),
            RuntimeConnection::M_SAVE_TEMPLATE => self.save_template(params),
            RuntimeConnection::M_DELETE_TEMPLATE => {
                let p: IdParams = Frames::params(params)?;
                self.templates_changed(deps.templates.delete(&p.id))
            }
            RuntimeConnection::M_CREATE_GROUP => {
                let p: PathParams = Frames::params(params)?;
                self.templates_changed(deps.templates.create_group(&p.path))
            }
            RuntimeConnection::M_RENAME_GROUP => {
                let p: RenameGroupParams = Frames::params(params)?;
                self.templates_changed(deps.templates.rename_group(&p.path, &p.new_name))
            }
            RuntimeConnection::M_DELETE_GROUP => {
                let p: PathParams = Frames::params(params)?;
                self.templates_changed(deps.templates.delete_group(&p.path))
            }
            RuntimeConnection::M_MOVE_TEMPLATE => {
                let p: MoveTemplateParams = Frames::params(params)?;
                self.templates_changed(deps.templates.move_template(&p.id, &p.path))
            }
            RuntimeConnection::M_LIST_CATALOG => {
                // Models dropped on disk become servable too, not only listed.
                deps.dispatcher.refresh();
                Self::to_value(deps.catalog.view())
            }
            RuntimeConnection::M_LIST_ALL_TOOLS => Self::to_value(deps.registry.specs()),
            RuntimeConnection::M_LIST_SESSIONS => self.list_sessions(),
            RuntimeConnection::M_CREATE_SESSION => self.create_session(params),
            RuntimeConnection::M_RENAME_SESSION => {
                let p: RenameSessionParams = Frames::params(params)?;
                deps.sessions
                    .rename(&p.id, &p.title)
                    .map_err(RpcError::internal)?;
                Self::sessions_changed(deps)
            }
            RuntimeConnection::M_DELETE_SESSION => {
                let p: IdParams = Frames::params(params)?;
                deps.sessions.delete(&p.id).map_err(RpcError::internal)?;
                deps.hub.detach_session(&p.id);
                Self::sessions_changed(deps)
            }
            RuntimeConnection::M_LOAD_SESSION_MODEL => self.load_session_model(params).await,
            // Allowed while the session is busy: the running turn keeps its current step, and
            // its next step finds no model and ends `Error` with `no model running`.
            RuntimeConnection::M_UNLOAD_SESSION_MODEL => {
                let p: IdParams = Frames::params(params)?;
                let rec = Self::record(deps, &p.id)?;
                deps.dispatcher
                    .unload(&Dispatcher::requirement(&rec))
                    .await
                    .map_err(RpcError::internal)?;
                Ok(Value::Null)
            }
            other => Err(RpcError::new(
                ERR_METHOD_NOT_FOUND,
                format!("{}{other}", RuntimeConnection::ERR_UNKNOWN_METHOD),
            )),
        }
    }

    /// The template, its group path, and why it fails validation (`null` when it passes). Only
    /// a file that does not parse (or a duplicate id) is an error.
    fn get_template(&self, params: Value) -> Result<Value, RpcError> {
        let p: IdParams = Frames::params(params)?;
        let (template, path) = self.deps.templates.get(&p.id).map_err(RpcError::internal)?;
        let error = self.deps.templates.check(&template);
        Ok(json!({ "id": p.id, "path": path, "template": template, "error": error }))
    }

    fn save_template(&self, params: Value) -> Result<Value, RpcError> {
        let p: SaveTemplateParams = Frames::params(params)?;
        let id = self
            .deps
            .templates
            .save(p.id.as_deref(), &p.path, &p.template)
            .map_err(RpcError::internal)?;
        self.deps
            .hub
            .broadcast(RuntimeServer::TEMPLATES_CHANGED, json!({}));
        Ok(json!({ "id": id }))
    }

    /// Broadcasts `templates_changed` after a successful template-store change.
    fn templates_changed(&self, outcome: Result<(), TemplateError>) -> Result<Value, RpcError> {
        outcome.map_err(RpcError::internal)?;
        self.deps
            .hub
            .broadcast(RuntimeServer::TEMPLATES_CHANGED, json!({}));
        Ok(Value::Null)
    }

    fn sessions_changed(deps: &RuntimeDeps) -> Result<Value, RpcError> {
        deps.hub
            .broadcast(RuntimeServer::SESSIONS_CHANGED, json!({}));
        Ok(Value::Null)
    }

    fn list_sessions(&self) -> Result<Value, RpcError> {
        let rows: Vec<SessionRow> = self
            .deps
            .sessions
            .list_with_requirements()
            .into_iter()
            .map(|(summary, req)| SessionView::row(&self.deps, summary, &req))
            .collect();
        Self::to_value(rows)
    }

    /// Creates a session from a template that passes validation now (a template the tree lists
    /// with an error is refused with that error).
    fn create_session(&self, params: Value) -> Result<Value, RpcError> {
        let p: CreateSessionParams = Frames::params(params)?;
        // A model dropped on disk is servable for the new session's placement.
        self.deps.dispatcher.refresh();
        let (template, _) = self
            .deps
            .templates
            .get(&p.template_id)
            .map_err(RpcError::internal)?;
        if let Some(e) = self.deps.templates.check(&template) {
            return Err(RpcError::internal(e));
        }
        let rec = self
            .deps
            .sessions
            .create(&p.template_id, &template)
            .map_err(RpcError::internal)?;
        let row = SessionView::row_of(&self.deps, &rec);
        self.deps.hub.seed_placement(&rec.id, row.placement.clone());
        Self::sessions_changed(&self.deps)?;
        Self::to_value(row)
    }

    async fn load_session_model(&self, params: Value) -> Result<Value, RpcError> {
        let p: IdParams = Frames::params(params)?;
        let rec = Self::record(&self.deps, &p.id)?;
        let req = Dispatcher::requirement(&rec);
        self.deps
            .dispatcher
            .load(&req)
            .await
            .map_err(RpcError::internal)?;
        Self::to_value(self.deps.dispatcher.status(&req))
    }

    fn record(deps: &RuntimeDeps, id: &str) -> Result<SessionRecord, RpcError> {
        deps.sessions
            .get(id)
            .ok_or_else(|| RpcError::internal(SessionError::unknown(id)))
    }

    fn to_value<T: Serialize>(v: T) -> Result<Value, RpcError> {
        serde_json::to_value(v).map_err(RpcError::internal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::host::HostState;
    use crate::rpc::test_support::TestClient;
    use crate::rpc::{ERR_INVALID_PARAMS, ERR_PARSE};
    use crate::runtime::test_support::{FakeLink, Notes, Scripted, TestRuntime};
    use std::time::Duration;
    use tokio::sync::mpsc::UnboundedReceiver;

    type C = RuntimeConnection;

    const WAIT: Duration = Duration::from_secs(5);
    const GROUP: &str = "Examples";
    const CONTENT: &str = "q";
    const ANSWER: &str = "hi";
    const TURN_STARTED: &str = "turn_started";
    const TURN_DONE: &str = "turn_done";
    const TURN_ERROR: &str = "turn_error";
    const LOOP_DONE: &str = "loop_done";
    const METHOD: &str = "method";
    const PARAMS: &str = "params";
    const RESULT: &str = "result";
    const LOAD_ERROR: &str = "boom";

    /// One in-process connection and the frames queued for it.
    struct Client {
        conn: RuntimeConnection,
        rx: UnboundedReceiver<Value>,
    }

    impl Client {
        fn new(rt: &TestRuntime) -> Self {
            let (conn, rx) = RuntimeConnection::new(rt.deps.clone());
            Self { conn, rx }
        }

        async fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
            self.conn.dispatch(method, params).await
        }

        async fn send(&mut self) -> Result<Value, RpcError> {
            self.call(C::M_SEND_TURN, json!({ "content": CONTENT }))
                .await
        }

        /// Every frame queued so far.
        fn drain(&mut self) -> Vec<Value> {
            std::iter::from_fn(|| self.rx.try_recv().ok()).collect()
        }

        fn methods(frames: &[Value]) -> Vec<String> {
            TestClient::methods(frames)
        }

        fn position(methods: &[String], method: &str) -> usize {
            methods
                .iter()
                .position(|m| m == method)
                .unwrap_or_else(|| panic!("{method} missing from {methods:?}"))
        }

        /// Closes this connection's channel as a gone client would.
        fn hang_up(&mut self) {
            self.rx = mpsc::unbounded_channel().1;
        }

        /// Creates a session from the seeded template; returns its id.
        async fn create(&mut self) -> String {
            let row = self
                .call(
                    C::M_CREATE_SESSION,
                    json!({ "template_id": TestRuntime::TEMPLATE_ID }),
                )
                .await
                .unwrap();
            row["id"].as_str().unwrap().to_string()
        }

        async fn attach(&mut self, id: &str) -> Value {
            self.call(C::M_ATTACH_SESSION, json!({ "id": id }))
                .await
                .unwrap()
        }

        /// Creates a session from the seeded template and attaches to it; returns its id.
        async fn open_session(&mut self) -> String {
            let id = self.create().await;
            self.attach(&id).await;
            id
        }

        fn answer(text: &str) -> Scripted {
            Notes::script(vec![Notes::started(), Notes::done(text, vec![])])
        }

        fn answering() -> Arc<FakeLink> {
            FakeLink::new(true, vec![Self::answer(ANSWER)])
        }

        fn idle() -> Arc<FakeLink> {
            FakeLink::new(true, vec![])
        }

        /// A real server over `rt` on an ephemeral loopback port.
        async fn serve(rt: &TestRuntime) -> std::net::SocketAddr {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(RuntimeServer::serve(listener, rt.deps.clone()));
            addr
        }

        /// The links one turn of `rec` would use.
        fn links<'a>(rt: &'a TestRuntime, rec: &'a SessionRecord) -> SessionLinks<'a> {
            SessionLinks::new(
                &rt.deps.dispatcher,
                &rt.deps.hub,
                &rec.id,
                Dispatcher::requirement(rec),
            )
        }

        /// Starts `send_turn` on this connection and finishes it in a task, as the socket loop
        /// does, so the test can act while the turn runs.
        fn spawn_send(&self) -> tokio::task::JoinHandle<Result<Value, RpcError>> {
            tokio::spawn(
                self.conn
                    .start(C::M_SEND_TURN, json!({ "content": CONTENT })),
            )
        }

        /// A host that holds every load until the returned gate gets a permit.
        fn gated(rt: &TestRuntime) -> Arc<tokio::sync::Semaphore> {
            let gate = Arc::new(tokio::sync::Semaphore::new(0));
            rt.host.set_gate(Arc::clone(&gate));
            gate
        }

        /// Waits until a load has taken the host's loading lock (and is held at its gate).
        async fn until_loading(rt: &TestRuntime) {
            tokio::time::timeout(WAIT, async {
                while rt.host.load_started() == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the turn never started its load");
        }
    }

    /// Turn links that reload the session's model (a new harness instance) right before
    /// step 1, as another session's load could.
    struct SwapBeforeSecondStep<'a> {
        links: SessionLinks<'a>,
        rt: &'a TestRuntime,
    }

    #[async_trait]
    impl TurnLinks for SwapBeforeSecondStep<'_> {
        async fn link(&self, step: usize) -> Result<StepLink, String> {
            if step == 1 {
                self.rt.load();
            }
            self.links.link(step).await
        }
    }

    #[tokio::test]
    async fn chat_without_attach_fails() {
        let rt = TestRuntime::new("noattach", Client::idle());
        let mut a = Client::new(&rt);
        let calls = [
            (C::M_SEND_TURN, json!({ "content": CONTENT })),
            (
                C::M_SET_SAMPLING,
                json!({ "config": SamplingConfig::default() }),
            ),
            (C::M_SET_THINKING, json!({ "enable": true })),
            (C::M_MODEL_PROPS, Value::Null),
            (C::M_TOKENIZE, json!({ "text": CONTENT })),
            (C::M_LIST_TOOLS, Value::Null),
            (C::M_RESET, Value::Null),
        ];
        for (method, params) in calls {
            let err = a.call(method, params).await.unwrap_err();
            assert_eq!(err.message, C::ERR_NO_SESSION, "{method}");
        }
    }

    #[tokio::test]
    async fn create_attach_send_commits() {
        let rt = TestRuntime::new("commit", Client::answering());
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        a.drain();
        let v = a.send().await.unwrap();
        assert_eq!(v, Value::Null);
        let methods = Client::methods(&a.drain());
        // Committed after loop_done; the commit is announced, then busy ends.
        let done = Client::position(&methods, LOOP_DONE);
        assert_eq!(
            &methods[done + 1..],
            [RuntimeServer::SESSIONS_CHANGED, Hub::SESSION_BUSY]
        );
        // Persisted: a fresh store over the same directory sees the committed transcript.
        let reopened = SessionStore::open(&rt.dir.0.join("sessions")).unwrap();
        let transcript = reopened.get(&id).unwrap().transcript;
        assert_eq!(transcript.len(), 2);
        assert_eq!(transcript[1].content, ANSWER);
        let attached = a.attach(&id).await;
        assert_eq!(attached["transcript"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn send_turn_result_comes_after_loop_done_on_the_wire() {
        let rt = TestRuntime::new("wire", Client::answering());
        rt.load();
        let mut a = TestClient::connect_to(Client::serve(&rt).await).await;
        let (_, row) = a
            .request(
                C::M_CREATE_SESSION,
                json!({ "template_id": TestRuntime::TEMPLATE_ID }),
            )
            .await;
        let id = row[RESULT]["id"].clone();
        a.request(C::M_ATTACH_SESSION, json!({ "id": id })).await;
        let (notes, resp) = a
            .request(C::M_SEND_TURN, json!({ "content": CONTENT }))
            .await;
        assert_eq!(resp[RESULT], Value::Null);
        assert!(TestClient::methods(&notes).contains(&LOOP_DONE.to_string()));
        let reply = a.send_raw("not json").await;
        assert_eq!(reply["id"], Value::Null);
        assert_eq!(reply["error"]["code"], ERR_PARSE);
    }

    #[tokio::test]
    async fn busy_session_rejects_second_turn() {
        let rt = TestRuntime::new("busy", Client::idle());
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        let turn = rt.deps.hub.hold_busy(&id).expect("free");
        let err = a.send().await.unwrap_err();
        assert_eq!(err.message, C::ERR_BUSY);
        let sessions = a.call(C::M_LIST_SESSIONS, Value::Null).await.unwrap();
        assert_eq!(sessions[0]["busy"], true);
        drop(turn);
        assert!(rt.host.link().requests().is_empty());
    }

    #[tokio::test]
    async fn reset_and_turn_exclude_each_other() {
        let rt = TestRuntime::new("resetbusy", Client::idle());
        let mut a = Client::new(&rt);
        let mut b = Client::new(&rt);
        let id = a.open_session().await;
        // A running turn holds the session: reset is refused.
        let turn = rt.deps.hub.hold_busy(&id).unwrap();
        let err = a.call(C::M_RESET, Value::Null).await.unwrap_err();
        assert_eq!(err.message, C::ERR_BUSY);
        drop(turn);
        b.drain();
        // The reset itself takes (and releases) busy, so no turn can start under it.
        a.call(C::M_RESET, Value::Null).await.unwrap();
        let frames = b.drain();
        assert_eq!(
            Client::methods(&frames),
            [Hub::SESSION_BUSY, Hub::SESSION_BUSY]
        );
        assert_eq!(frames[0][PARAMS], json!({ "id": id, "busy": true }));
        assert!(!rt.deps.hub.is_busy(&id));
    }

    #[tokio::test]
    async fn busy_and_commit_are_broadcast() {
        let rt = TestRuntime::new("busycast", Client::answering());
        rt.load();
        let mut a = Client::new(&rt);
        let mut b = Client::new(&rt);
        let id = a.open_session().await;
        b.drain();
        a.send().await.unwrap();
        let frames = b.drain();
        assert_eq!(
            Client::methods(&frames),
            [
                Hub::SESSION_BUSY,
                RuntimeServer::SESSIONS_CHANGED,
                Hub::SESSION_BUSY
            ]
        );
        assert_eq!(frames[0][PARAMS], json!({ "id": id, "busy": true }));
        assert_eq!(frames[2][PARAMS], json!({ "id": id, "busy": false }));
        assert!(!rt.deps.hub.is_busy(&id));
    }

    #[tokio::test]
    async fn send_turn_on_needs_load_notifies_loading() {
        let rt = TestRuntime::new("loading", Client::answering());
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        a.drain();
        a.send().await.unwrap();
        let frames = a.drain();
        let methods = Client::methods(&frames);
        let loading = Client::position(&methods, RuntimeServer::SESSION_LOADING);
        assert!(loading < Client::position(&methods, TURN_STARTED));
        assert_eq!(
            TestClient::params_of(&frames, RuntimeServer::SESSION_LOADING)[0],
            json!({ "id": id })
        );
        assert_eq!(rt.host.ensure_ready_calls(), 1);
    }

    #[tokio::test]
    async fn send_turn_on_loaded_model_does_not_notify_loading() {
        let rt = TestRuntime::new("loaded", Client::answering());
        rt.load();
        let mut a = Client::new(&rt);
        a.open_session().await;
        a.send().await.unwrap();
        let methods = Client::methods(&a.drain());
        assert!(!methods.contains(&RuntimeServer::SESSION_LOADING.to_string()));
        assert_eq!(rt.host.ensure_ready_calls(), 0);
    }

    #[tokio::test]
    async fn failed_load_reports_turn_error_then_no_harness() {
        let rt = TestRuntime::new("loadfail", Client::idle());
        rt.host.fail_ensure(Some(LOAD_ERROR));
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        a.drain();
        assert_eq!(a.send().await.unwrap(), Value::Null);
        let frames = a.drain();
        let methods = Client::methods(&frames);
        let error = Client::position(&methods, TURN_ERROR);
        assert!(Client::position(&methods, RuntimeServer::SESSION_LOADING) < error);
        assert_eq!(methods[error + 1], LOOP_DONE);
        assert_eq!(frames[error][PARAMS], json!({ "message": LOAD_ERROR }));
        assert_eq!(
            frames[error + 1][PARAMS],
            json!({ "steps": 0, "reason": "no_harness" })
        );
        assert!(rt.deps.sessions.get(&id).unwrap().transcript.is_empty());
    }

    #[tokio::test]
    async fn two_connections_attached_both_receive_notes() {
        let rt = TestRuntime::new("twoconn", Client::answering());
        rt.load();
        let addr = Client::serve(&rt).await;
        let mut a = TestClient::connect_to(addr).await;
        let mut b = TestClient::connect_to(addr).await;
        let (_, row) = a
            .request(
                C::M_CREATE_SESSION,
                json!({ "template_id": TestRuntime::TEMPLATE_ID }),
            )
            .await;
        let id = row[RESULT]["id"].clone();
        a.request(C::M_ATTACH_SESSION, json!({ "id": id })).await;
        let (_, attached) = b.request(C::M_ATTACH_SESSION, json!({ "id": id })).await;
        assert_eq!(attached[RESULT]["summary"]["id"], id);
        let (notes, resp) = a
            .request(C::M_SEND_TURN, json!({ "content": CONTENT }))
            .await;
        assert_eq!(resp[RESULT], Value::Null);
        assert!(TestClient::methods(&notes).contains(&TURN_DONE.to_string()));
        let seen = TestClient::methods(&b.notes_until(LOOP_DONE).await);
        assert!(seen.contains(&TURN_STARTED.to_string()), "{seen:?}");
        assert!(seen.contains(&TURN_DONE.to_string()), "{seen:?}");
    }

    #[tokio::test]
    async fn turn_finishes_for_others_when_its_connection_is_gone() {
        let rt = TestRuntime::new("gone", Client::answering());
        rt.load();
        let mut a = Client::new(&rt);
        let mut b = Client::new(&rt);
        let id = a.open_session().await;
        b.attach(&id).await;
        a.hang_up();
        a.send().await.unwrap();
        assert!(Client::methods(&b.drain()).contains(&LOOP_DONE.to_string()));
        assert_eq!(rt.deps.sessions.get(&id).unwrap().transcript.len(), 2);
    }

    #[tokio::test]
    async fn turn_aborts_when_no_attached_connection_remains() {
        let rt = TestRuntime::new("abort", Client::answering());
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        a.hang_up();
        let err = a.send().await.unwrap_err();
        assert_eq!(err.message, C::CLIENT_GONE);
        assert!(rt.deps.sessions.get(&id).unwrap().transcript.is_empty());
        assert!(!rt.deps.hub.is_busy(&id));
    }

    #[tokio::test]
    async fn list_level_changes_broadcast_to_all() {
        let rt = TestRuntime::new("broadcast", Client::idle());
        let mut a = Client::new(&rt);
        let mut b = Client::new(&rt);
        let id = a.create().await;
        assert_eq!(
            Client::methods(&b.drain()),
            [RuntimeServer::SESSIONS_CHANGED]
        );
        let title = "Renamed";
        a.call(C::M_RENAME_SESSION, json!({ "id": id, "title": title }))
            .await
            .unwrap();
        assert_eq!(
            Client::methods(&b.drain()),
            [RuntimeServer::SESSIONS_CHANGED]
        );
        let mut template = rt.template();
        template.name = "Second".into();
        a.call(
            C::M_SAVE_TEMPLATE,
            json!({ "path": GROUP, "template": template }),
        )
        .await
        .unwrap();
        a.call(C::M_CREATE_GROUP, json!({ "path": "Team" }))
            .await
            .unwrap();
        assert_eq!(
            Client::methods(&b.drain()),
            [
                RuntimeServer::TEMPLATES_CHANGED,
                RuntimeServer::TEMPLATES_CHANGED
            ]
        );
        let sessions = b.call(C::M_LIST_SESSIONS, Value::Null).await.unwrap();
        assert_eq!(sessions[0]["title"], title);
        assert_eq!(sessions[0]["placement"]["state"], "needs_load");
    }

    #[tokio::test]
    async fn deleted_session_detaches_connections() {
        let rt = TestRuntime::new("delete", Client::answering());
        rt.load();
        let mut a = Client::new(&rt);
        let mut b = Client::new(&rt);
        let mut c = Client::new(&rt);
        let id = a.open_session().await;
        b.attach(&id).await;
        let other = c.open_session().await;
        b.drain();
        a.call(C::M_DELETE_SESSION, json!({ "id": id }))
            .await
            .unwrap();
        assert_eq!(
            Client::methods(&b.drain()),
            [RuntimeServer::SESSIONS_CHANGED]
        );
        let err = b.send().await.unwrap_err();
        assert_eq!(err.message, C::ERR_NO_SESSION);
        let err = a.call(C::M_LIST_TOOLS, Value::Null).await.unwrap_err();
        assert_eq!(err.message, C::ERR_NO_SESSION);
        // A connection attached to another session is unaffected: its turn still runs.
        c.drain();
        assert_eq!(c.send().await.unwrap(), Value::Null);
        assert!(Client::methods(&c.drain()).contains(&LOOP_DONE.to_string()));
        assert_eq!(rt.deps.sessions.get(&other).unwrap().transcript.len(), 2);
    }

    #[tokio::test]
    async fn requests_on_one_connection_answer_while_its_turn_runs() {
        let rt = TestRuntime::new("concurrent", Client::answering());
        let gate = Client::gated(&rt);
        let mut a = TestClient::connect_to(Client::serve(&rt).await).await;
        let (_, row) = a
            .request(
                C::M_CREATE_SESSION,
                json!({ "template_id": TestRuntime::TEMPLATE_ID }),
            )
            .await;
        let id = row[RESULT]["id"].clone();
        a.request(C::M_ATTACH_SESSION, json!({ "id": id })).await;
        // The turn needs a load, which the host holds mid-step until the gate opens.
        let turn = a
            .send_request(C::M_SEND_TURN, json!({ "content": CONTENT }))
            .await;
        a.notes_until(RuntimeServer::SESSION_LOADING).await;
        let (_, tree) = a.request(C::M_LIST_TEMPLATES, Value::Null).await;
        assert_eq!(tree[RESULT][0]["name"], GROUP);
        let (_, sessions) = a.request(C::M_LIST_SESSIONS, Value::Null).await;
        assert_eq!(sessions[RESULT][0]["busy"], true);
        let mut template = rt.template();
        template.name = "Meanwhile".into();
        let (_, saved) = a
            .request(
                C::M_SAVE_TEMPLATE,
                json!({ "path": GROUP, "template": template }),
            )
            .await;
        assert_eq!(saved[RESULT]["id"], "meanwhile");
        gate.add_permits(1);
        let (notes, done) = a.response_to(turn).await;
        assert_eq!(done[RESULT], Value::Null);
        assert!(TestClient::methods(&notes).contains(&LOOP_DONE.to_string()));
    }

    #[tokio::test]
    async fn chat_methods_take_effect_in_arrival_order() {
        let rt = TestRuntime::new("order", Client::answering());
        rt.load();
        let a = Client::new(&rt);
        let id = rt
            .deps
            .sessions
            .create(TestRuntime::TEMPLATE_ID, &rt.template())
            .unwrap()
            .id;
        let config = SamplingConfig {
            temperature: 0.75,
            ..SamplingConfig::default()
        };
        // Started back to back, finished in any order: the attach and the override sent before
        // the turn are what the turn sees.
        let attach = a.conn.start(C::M_ATTACH_SESSION, json!({ "id": id }));
        let sampling = a.conn.start(C::M_SET_SAMPLING, json!({ "config": config }));
        let turn = a.spawn_send();
        turn.await.unwrap().unwrap();
        attach.await.unwrap();
        sampling.await.unwrap();
        assert_eq!(rt.host.link().requests()[0].sampling, config);
    }

    #[tokio::test]
    async fn own_session_deleted_mid_turn_reports_session_deleted() {
        let rt = TestRuntime::new("delturn", Client::answering());
        let gate = Client::gated(&rt);
        let mut a = Client::new(&rt);
        let mut b = Client::new(&rt);
        let id = a.open_session().await;
        let turn = a.spawn_send();
        Client::until_loading(&rt).await;
        b.call(C::M_DELETE_SESSION, json!({ "id": id }))
            .await
            .unwrap();
        gate.add_permits(1);
        let err = turn.await.unwrap().unwrap_err();
        assert_eq!(err.message, C::ERR_DELETED);
    }

    #[tokio::test]
    async fn a_panicking_request_is_answered_and_releases_busy() {
        // `idle` has no generate script, so the turn's generate call panics.
        let rt = TestRuntime::new("panic", Client::idle());
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        let err = a.send().await.unwrap_err();
        assert_eq!(err.code, crate::rpc::ERR_INTERNAL);
        assert_eq!(err.message, C::ERR_REQUEST_FAILED);
        assert!(!rt.deps.hub.is_busy(&id));
        // The session is usable again: the next turn can take busy.
        assert!(rt.deps.hub.hold_busy(&id).is_some());
    }

    #[tokio::test]
    async fn a_turn_left_by_attaching_elsewhere_reports_session_detached() {
        let rt = TestRuntime::new("detached", Client::answering());
        let gate = Client::gated(&rt);
        let mut a = Client::new(&rt);
        let first = a.open_session().await;
        let second = a.create().await;
        a.attach(&first).await;
        let turn = a.spawn_send();
        Client::until_loading(&rt).await;
        a.attach(&second).await;
        gate.add_permits(1);
        let err = turn.await.unwrap().unwrap_err();
        assert_eq!(err.message, C::ERR_DETACHED);
        assert!(rt.deps.sessions.get(&first).unwrap().transcript.is_empty());
    }

    #[tokio::test]
    async fn failed_commit_says_the_answer_was_not_saved() {
        let rt = TestRuntime::new("nosave", Client::answering());
        rt.load();
        let mut a = Client::new(&rt);
        a.open_session().await;
        // The store can no longer write its files.
        std::fs::remove_dir_all(rt.dir.0.join("sessions")).unwrap();
        let err = a.send().await.unwrap_err();
        assert!(err.message.starts_with(C::ERR_NOT_SAVED), "{}", err.message);
    }

    #[tokio::test]
    async fn template_failing_validation_is_flagged_and_cannot_start_a_session() {
        let rt = TestRuntime::new("badtpl", Client::idle());
        let mut a = Client::new(&rt);
        let file = rt.dir.0.join("templates").join(GROUP).join("bad.toml");
        let body = format!(
            "name = \"Bad\"\nruntime = \"{}\"\nmodel = \"{}\"\nn_ctx = 4096\nstep_budget = 0\n",
            TestRuntime::RUNTIME,
            TestRuntime::MODEL
        );
        std::fs::write(file, body).unwrap();
        let budget = "step_budget must be at least 1";
        let tree = a.call(C::M_LIST_TEMPLATES, Value::Null).await.unwrap();
        let bad = tree[0]["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == "bad")
            .expect("listed")
            .clone();
        assert_eq!(bad["error"], budget);
        assert_eq!(bad["name"], "Bad");
        // Readable, with the reason it is flagged.
        let got = a
            .call(C::M_GET_TEMPLATE, json!({ "id": "bad" }))
            .await
            .unwrap();
        assert_eq!(got["template"]["step_budget"], 0);
        assert_eq!(got["error"], budget);
        let fine = a
            .call(C::M_GET_TEMPLATE, json!({ "id": TestRuntime::TEMPLATE_ID }))
            .await
            .unwrap();
        assert_eq!(fine["error"], Value::Null);
        let err = a
            .call(C::M_CREATE_SESSION, json!({ "template_id": "bad" }))
            .await
            .unwrap_err();
        assert_eq!(err.message, budget);
        assert!(rt.deps.sessions.list().is_empty());
    }

    #[tokio::test]
    async fn a_reloaded_model_ends_the_turn_with_model_changed() {
        let rt = TestRuntime::new("swap", Client::idle());
        rt.load();
        let rec = rt
            .deps
            .sessions
            .create(TestRuntime::TEMPLATE_ID, &rt.template())
            .unwrap();
        let links = Client::links(&rt, &rec);
        assert!(links.link(0).await.is_ok());
        assert!(links.link(1).await.is_ok());
        // Same model, new harness instance: not the harness the turn started on.
        rt.load();
        assert_eq!(
            links.link(2).await.err().unwrap(),
            SessionLinks::MODEL_CHANGED
        );
    }

    #[tokio::test]
    async fn a_model_swap_between_steps_reports_model_changed() {
        let call = Notes::call("c", "echo", json!({ "text": CONTENT }));
        let link = FakeLink::new(
            true,
            vec![Notes::script(vec![
                Notes::started(),
                Notes::done("", vec![call]),
            ])],
        );
        let rt = TestRuntime::new("swaploop", link);
        rt.load();
        let rec = rt
            .deps
            .sessions
            .create(TestRuntime::TEMPLATE_ID, &rt.template())
            .unwrap();
        let links = SwapBeforeSecondStep {
            links: Client::links(&rt, &rec),
            rt: &rt,
        };
        let config = TurnConfig {
            step_budget: rec.snapshot.step_budget,
            tools: rec.snapshot.tools.clone(),
            enable_thinking: false,
            sampling: SamplingConfig::default(),
        };
        let mut sink = crate::runtime::test_support::RecordingSink::new();
        let (end, transcript) = rt
            .deps
            .tool_loop
            .run(
                &rec.transcript,
                CONTENT,
                &config,
                &links,
                &mut NoTrace,
                &mut sink,
            )
            .await
            .unwrap();
        assert_eq!(end.reason, crate::runtime::tool_loop::EndReason::Error);
        assert!(transcript.is_none());
        let methods = sink.methods();
        let n = methods.len();
        assert_eq!(&methods[n - 2..], [TURN_ERROR, LOOP_DONE]);
        assert_eq!(
            sink.notes[n - 2].params,
            json!({ "message": SessionLinks::MODEL_CHANGED })
        );
        assert_eq!(sink.notes[n - 1].params["reason"], "error");
    }

    #[tokio::test]
    async fn placement_changed_on_eviction() {
        let rt = TestRuntime::new("evict", Client::idle());
        rt.load();
        let mut a = Client::new(&rt);
        let mut b = Client::new(&rt);
        let id = a.open_session().await;
        let sessions = b.call(C::M_LIST_SESSIONS, Value::Null).await.unwrap();
        assert_eq!(sessions[0]["placement"]["state"], "ready");
        let _watcher = PlacementWatcher::spawn(&rt.deps);
        b.drain();
        rt.host.set_state(HostState::default());
        let frame = tokio::time::timeout(WAIT, b.rx.recv())
            .await
            .expect("placement_changed")
            .unwrap();
        assert_eq!(frame[METHOD], RuntimeServer::PLACEMENT_CHANGED);
        assert_eq!(
            frame[PARAMS],
            json!({ "id": id, "placement": { "state": "needs_load", "host": TestRuntime::HOST, "evicts": null } })
        );
    }

    #[tokio::test]
    async fn placement_watcher_reports_only_changes() {
        let rt = TestRuntime::new("diff", Client::idle());
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        let mut watcher = PlacementWatcher::new(&rt.deps);
        a.drain();
        assert!(watcher.publish());
        assert!(a.drain().is_empty());
        rt.load();
        assert!(watcher.publish());
        let frames = a.drain();
        assert_eq!(Client::methods(&frames), [RuntimeServer::PLACEMENT_CHANGED]);
        assert_eq!(frames[0][PARAMS]["id"], id.as_str());
        assert_eq!(frames[0][PARAMS]["placement"]["state"], "ready");
    }

    #[tokio::test]
    async fn placement_watcher_compares_new_sessions_with_their_seed() {
        let rt = TestRuntime::new("newsess", Client::idle());
        let mut watcher = PlacementWatcher::new(&rt.deps);
        let mut a = Client::new(&rt);
        let id = a.create().await;
        a.drain();
        // Seeded `needs_load` by create_session: an unchanged signal announces nothing.
        assert!(watcher.publish());
        assert!(a.drain().is_empty());
        rt.load();
        assert!(watcher.publish());
        let frames = a.drain();
        assert_eq!(Client::methods(&frames), [RuntimeServer::PLACEMENT_CHANGED]);
        assert_eq!(frames[0][PARAMS]["id"], id.as_str());
    }

    #[tokio::test]
    async fn eviction_right_after_create_is_announced() {
        let rt = TestRuntime::new("seedevict", Client::idle());
        rt.load();
        let mut watcher = PlacementWatcher::new(&rt.deps);
        let mut a = Client::new(&rt);
        let row = a
            .call(
                C::M_CREATE_SESSION,
                json!({ "template_id": TestRuntime::TEMPLATE_ID }),
            )
            .await
            .unwrap();
        assert_eq!(row["placement"]["state"], "ready");
        a.drain();
        // Evicted before the watcher ever saw the session.
        rt.host.set_state(HostState::default());
        assert!(watcher.publish());
        let frames = a.drain();
        assert_eq!(Client::methods(&frames), [RuntimeServer::PLACEMENT_CHANGED]);
        assert_eq!(frames[0][PARAMS]["id"], row["id"]);
        assert_eq!(frames[0][PARAMS]["placement"]["state"], "needs_load");
    }

    #[tokio::test]
    async fn session_unknown_to_the_hub_is_announced() {
        let rt = TestRuntime::new("unseeded", Client::idle());
        let mut watcher = PlacementWatcher::new(&rt.deps);
        let mut a = Client::new(&rt);
        // Created behind the server's back: nothing seeded, so the first signal announces it.
        rt.deps
            .sessions
            .create(TestRuntime::TEMPLATE_ID, &rt.template())
            .unwrap();
        assert!(watcher.publish());
        assert_eq!(
            Client::methods(&a.drain()),
            [RuntimeServer::PLACEMENT_CHANGED]
        );
    }

    #[tokio::test]
    async fn chat_on_a_session_deleted_underneath_broadcasts_no_busy() {
        let rt = TestRuntime::new("delreset", Client::idle());
        let mut a = Client::new(&rt);
        let mut b = Client::new(&rt);
        let id = a.open_session().await;
        // Deleted in the store only, so this connection still names it as attached.
        rt.deps.sessions.delete(&id).unwrap();
        b.drain();
        for method in [C::M_RESET, C::M_SEND_TURN] {
            let err = a
                .call(method, json!({ "content": CONTENT }))
                .await
                .unwrap_err();
            assert_eq!(err.message, C::ERR_NO_SESSION, "{method}");
        }
        assert!(b.drain().is_empty());
        assert!(!rt.deps.hub.is_busy(&id));
    }

    #[tokio::test]
    async fn list_all_tools_and_catalog() {
        let rt = TestRuntime::new("catalog", Client::idle());
        let mut a = Client::new(&rt);
        let tools = a.call(C::M_LIST_ALL_TOOLS, Value::Null).await.unwrap();
        assert_eq!(tools.as_array().unwrap().len(), 1);
        assert_eq!(tools[0]["name"], "echo");
        let catalog = a.call(C::M_LIST_CATALOG, Value::Null).await.unwrap();
        assert_eq!(
            catalog,
            serde_json::to_value(TestRuntime::catalog()).unwrap()
        );
        a.open_session().await;
        let session_tools = a.call(C::M_LIST_TOOLS, Value::Null).await.unwrap();
        assert_eq!(session_tools, tools);
    }

    #[tokio::test]
    async fn list_tools_is_the_sessions_tools() {
        let rt = TestRuntime::new("sesstools", Client::idle());
        let mut template = rt.template();
        template.tools.clear();
        rt.deps
            .templates
            .save(Some(TestRuntime::TEMPLATE_ID), GROUP, &template)
            .unwrap();
        let mut a = Client::new(&rt);
        a.open_session().await;
        let tools = a.call(C::M_LIST_TOOLS, Value::Null).await.unwrap();
        assert_eq!(tools, json!([]));
    }

    #[tokio::test]
    async fn save_template_validation_error_is_rpc_error() {
        let rt = TestRuntime::new("invalid", Client::idle());
        let mut a = Client::new(&rt);
        let mut template = rt.template();
        template.tools = vec!["nope".into()];
        let err = a
            .call(
                C::M_SAVE_TEMPLATE,
                json!({ "path": GROUP, "template": template }),
            )
            .await
            .unwrap_err();
        assert_eq!(err.message, "unknown tool: nope");
        assert!(a.drain().is_empty());
    }

    #[tokio::test]
    async fn template_methods_round_trip() {
        let rt = TestRuntime::new("templates", Client::idle());
        let mut a = Client::new(&rt);
        let (team, crew) = ("Team", "Crew");
        let got = a
            .call(C::M_GET_TEMPLATE, json!({ "id": TestRuntime::TEMPLATE_ID }))
            .await
            .unwrap();
        assert_eq!(got["id"], TestRuntime::TEMPLATE_ID);
        assert_eq!(got["path"], GROUP);
        assert_eq!(got["template"]["model"], TestRuntime::MODEL);
        let mut template = rt.template();
        template.name = "Helper".into();
        let saved = a
            .call(
                C::M_SAVE_TEMPLATE,
                json!({ "path": GROUP, "template": template }),
            )
            .await
            .unwrap();
        let id = saved["id"].as_str().unwrap().to_string();
        a.call(C::M_CREATE_GROUP, json!({ "path": team }))
            .await
            .unwrap();
        a.call(C::M_MOVE_TEMPLATE, json!({ "id": id, "path": team }))
            .await
            .unwrap();
        a.call(C::M_RENAME_GROUP, json!({ "path": team, "new_name": crew }))
            .await
            .unwrap();
        let tree = a.call(C::M_LIST_TEMPLATES, Value::Null).await.unwrap();
        let group = tree
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["name"] == crew)
            .expect("renamed group");
        assert_eq!(group["children"][0]["id"], id.as_str());
        let err = a
            .call(C::M_DELETE_GROUP, json!({ "path": crew }))
            .await
            .unwrap_err();
        assert_eq!(err.message, "group is not empty");
        a.call(C::M_DELETE_TEMPLATE, json!({ "id": id }))
            .await
            .unwrap();
        a.call(C::M_DELETE_GROUP, json!({ "path": crew }))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn deleting_a_template_leaves_its_sessions_running() {
        let rt = TestRuntime::new("deltpl", Client::answering());
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        a.call(
            C::M_DELETE_TEMPLATE,
            json!({ "id": TestRuntime::TEMPLATE_ID }),
        )
        .await
        .unwrap();
        a.send().await.unwrap();
        assert_eq!(rt.deps.sessions.get(&id).unwrap().transcript.len(), 2);
    }

    #[tokio::test]
    async fn overrides_persist_and_drive_the_turn() {
        let rt = TestRuntime::new("overrides", Client::answering());
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        let config = SamplingConfig {
            temperature: 0.25,
            ..SamplingConfig::default()
        };
        a.call(C::M_SET_SAMPLING, json!({ "config": config }))
            .await
            .unwrap();
        a.call(C::M_SET_THINKING, json!({ "enable": true }))
            .await
            .unwrap();
        let rec = rt.deps.sessions.get(&id).unwrap();
        assert_eq!(rec.sampling.as_ref(), Some(&config));
        assert_eq!(rec.enable_thinking, Some(true));
        let attached = a.attach(&id).await;
        assert_eq!(attached["enable_thinking"], true);
        assert_eq!(attached["sampling"], serde_json::to_value(&config).unwrap());
        a.send().await.unwrap();
        let request = &rt.host.link().requests()[0];
        assert_eq!(request.sampling, config);
        assert!(request.enable_thinking);
        assert_eq!(request.tools.len(), 1);
    }

    #[tokio::test]
    async fn turn_uses_snapshot_thinking_and_sampling_without_overrides() {
        let rt = TestRuntime::new("snapshot", Client::answering());
        let mut template = rt.template();
        template.enable_thinking = true;
        template.sampling = Some(SamplingConfig {
            temperature: 0.5,
            ..SamplingConfig::default()
        });
        template.system_prompt = "be brief".into();
        rt.deps
            .templates
            .save(Some(TestRuntime::TEMPLATE_ID), GROUP, &template)
            .unwrap();
        rt.load();
        let mut a = Client::new(&rt);
        a.open_session().await;
        a.send().await.unwrap();
        let request = &rt.host.link().requests()[0];
        assert!(request.enable_thinking);
        assert_eq!(request.sampling.temperature, 0.5);
        assert_eq!(request.messages.len(), 2);
    }

    #[tokio::test]
    async fn reset_restores_the_initial_transcript() {
        let rt = TestRuntime::new("reset", Client::answering());
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        a.send().await.unwrap();
        a.call(C::M_RESET, Value::Null).await.unwrap();
        assert!(rt.deps.sessions.get(&id).unwrap().transcript.is_empty());
    }

    #[tokio::test]
    async fn model_props_and_tokenize_need_the_sessions_model() {
        let rt = TestRuntime::new("props", Client::idle());
        let mut a = Client::new(&rt);
        a.open_session().await;
        let text = json!({ "text": CONTENT });
        let err = a.call(C::M_MODEL_PROPS, Value::Null).await.unwrap_err();
        assert_eq!(err.message, ToolLoop::NO_MODEL);
        let err = a.call(C::M_TOKENIZE, text.clone()).await.unwrap_err();
        assert_eq!(err.message, ToolLoop::NO_MODEL);
        rt.load();
        let v = a.call(C::M_MODEL_PROPS, Value::Null).await.unwrap();
        assert_eq!(v, json!({ "supports_tools": true }));
        let v = a.call(C::M_TOKENIZE, text).await.unwrap();
        assert_eq!(v, json!({ "count": 0 }));
    }

    #[tokio::test]
    async fn load_and_unload_session_model() {
        let rt = TestRuntime::new("loadunload", Client::idle());
        let mut a = Client::new(&rt);
        let id = a.create().await;
        let placement = a
            .call(C::M_LOAD_SESSION_MODEL, json!({ "id": id }))
            .await
            .unwrap();
        assert_eq!(placement["state"], "ready");
        assert_eq!(rt.host.ensure_ready_calls(), 1);
        a.call(C::M_UNLOAD_SESSION_MODEL, json!({ "id": id }))
            .await
            .unwrap();
        assert_eq!(rt.host.unload_calls(), 1);
        let missing = "nope";
        let err = a
            .call(C::M_LOAD_SESSION_MODEL, json!({ "id": missing }))
            .await
            .unwrap_err();
        assert_eq!(err.message, SessionError::unknown(missing).0);
    }

    #[tokio::test]
    async fn later_steps_without_a_model_end_with_no_model_running() {
        let rt = TestRuntime::new("links", Client::idle());
        let rec = rt
            .deps
            .sessions
            .create(TestRuntime::TEMPLATE_ID, &rt.template())
            .unwrap();
        let links = Client::links(&rt, &rec);
        let err = links.link(1).await.err().unwrap();
        assert_eq!(err, ToolLoop::NO_MODEL);
        assert_eq!(rt.host.ensure_ready_calls(), 0);
        rt.load();
        assert!(links.link(1).await.is_ok());
    }

    #[tokio::test]
    async fn unload_during_a_turn_ends_its_next_step() {
        let rt = TestRuntime::new("unloadbusy", Client::idle());
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        let rec = rt.deps.sessions.get(&id).unwrap();
        let links = Client::links(&rt, &rec);
        // The turn holds busy and has its first link...
        let _turn = rt.deps.hub.hold_busy(&id).unwrap();
        assert!(links.link(0).await.is_ok());
        // ...when another client unloads the session's model: allowed while busy.
        let mut b = Client::new(&rt);
        b.call(C::M_UNLOAD_SESSION_MODEL, json!({ "id": id }))
            .await
            .unwrap();
        assert_eq!(rt.host.unload_calls(), 1);
        // The turn's next step finds no model, which the loop reports as `turn_error`.
        assert_eq!(links.link(1).await.err().unwrap(), ToolLoop::NO_MODEL);
    }

    #[tokio::test]
    async fn unknown_method_and_bad_params_use_rpc_codes() {
        let rt = TestRuntime::new("codes", Client::idle());
        let mut a = Client::new(&rt);
        let err = a.call("nope", Value::Null).await.unwrap_err();
        assert_eq!(err.code, ERR_METHOD_NOT_FOUND);
        let err = a.call(C::M_CREATE_SESSION, json!({})).await.unwrap_err();
        assert_eq!(err.code, ERR_INVALID_PARAMS);
        a.open_session().await;
        let err = a.call(C::M_SEND_TURN, json!({})).await.unwrap_err();
        assert_eq!(err.code, ERR_INVALID_PARAMS);
    }
}
