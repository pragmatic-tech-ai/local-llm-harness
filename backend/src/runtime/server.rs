//! The agent runtime's JSON-RPC websocket server (`:8779`), session-aware.
//!
//! Conversations are persisted sessions started from agent templates. A connection attaches to
//! at most one session (`attach_session`); the chat methods (`send_turn`, `set_sampling`,
//! `set_thinking`, `model_props`, `tokenize`, `list_tools`, `reset`) act on it and fail
//! `no session attached` otherwise. Template, catalog and session-list methods need no
//! attachment.
//!
//! Every turn is traced into the session's trace log (`{id}.trace.jsonl`, [`TraceStore`]),
//! answered or not: the attached connections get `trace_step {id, record}` per loop step and
//! `trace_turn {id, record}` once the commit is decided (before the `send_turn` result and
//! `sessions_changed`), and `trace_warning {id, message}` when the log could not be written; the
//! turn goes on either way. A turn cut off because no connection is left is still logged, with
//! reason `aborted`. `get_trace {id, before?, limit?}` needs no attachment and pages the log
//! newest turn first (`{turns, more, reset_pending}`). Live and paged records are the same:
//! each in its log-line shape, `v` and `kind` included. `reset` appends a reset marker;
//! `delete_session` deletes the log.
//!
//! Each turn compacts its prompt by the session's snapshot (`context_strategy`,
//! `compact_at_pct`; editing the template later does not change a session). Its [`Compactor`]
//! starts from the session's saved summary, and a fresh summary is saved back on the session
//! straight away, whatever the turn's outcome, through the store's own short lock (never the
//! hub's); a session deleted meanwhile is left alone. A step's `trace_step` record carries
//! `compaction` when compaction ran. `reset` clears the saved summary with the transcript.
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
use std::time::Instant;

use async_trait::async_trait;
use futures::stream::SplitSink;
use futures::{FutureExt, SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;

pub use super::catalog_source::{CatalogSource, CatalogView};
use super::compaction::{Compactor, SummarySink};
use super::hub::{ConnId, Hub};
use super::registry::ToolRegistry;
use super::tool_loop::{
    LinkFailure, LoopSink, SinkClosed, StepLink, ToolLoop, TurnConfig, TurnLinks,
};
use super::trace::{NoTrace, SessionTrace, TraceLog, TraceStore, TurnTrace};
use super::worker::WorkerLink;
use crate::agents::dispatcher::{Dispatcher, Placement};
use crate::agents::host::{HarnessIdentity, ModelRequirement};
use crate::agents::session::{SessionRecord, SessionSummary};
use crate::agents::session_store::{SessionError, SessionStore};
use crate::agents::template::{AgentTemplate, CompactionState};
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
    /// The sessions' trace logs, next to their session files.
    pub traces: Arc<TraceStore>,
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

    /// A step link that loaded nothing.
    fn plain(link: Arc<dyn WorkerLink>) -> StepLink {
        StepLink { link, load: None }
    }
}

#[async_trait]
impl TurnLinks for SessionLinks<'_> {
    async fn link(&self, step: usize) -> Result<StepLink, LinkFailure> {
        if step == 0 {
            // The load time is the whole lookup, recorded only when it had to load (whether
            // the load then succeeded or failed).
            let started = Instant::now();
            let mut loaded = false;
            let link = self
                .dispatcher
                .link_for_turn(&self.req, || {
                    loaded = true;
                    self.hub.to_session(
                        self.session,
                        RuntimeServer::SESSION_LOADING,
                        json!({ "id": self.session }),
                    );
                })
                .await
                .map_err(|e| LinkFailure {
                    message: e.0,
                    load: loaded.then(|| started.elapsed()),
                })?;
            *self.bound() = self.dispatcher.identity(&self.req);
            return Ok(StepLink {
                link,
                load: loaded.then(|| started.elapsed()),
            });
        }
        let bound = self.bound().clone();
        if let (Some(bound), Some(now)) = (bound, self.dispatcher.identity(&self.req)) {
            if bound != now {
                return Err(LinkFailure::from(Self::MODEL_CHANGED.to_string()));
            }
        }
        self.dispatcher
            .current_link(&self.req)
            .map(Self::plain)
            .ok_or_else(|| LinkFailure::from(ToolLoop::NO_MODEL.to_string()))
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

/// Saves a turn's fresh summary on its session. Called by compaction outside the hub's lock;
/// the store's write takes only its own short lock. A session deleted meanwhile is left alone.
struct SessionCompactionSink {
    sessions: Arc<SessionStore>,
    session: String,
}

#[async_trait]
impl SummarySink for SessionCompactionSink {
    async fn save(&self, state: CompactionState) -> Result<(), String> {
        if !self.sessions.contains(&self.session) {
            return Ok(());
        }
        match self.sessions.set_compaction(&self.session, Some(state)) {
            Ok(()) => Ok(()),
            // Deleted between the check and the write.
            Err(_) if !self.sessions.contains(&self.session) => Ok(()),
            Err(e) => Err(e.to_string()),
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

    /// What one turn of `rec` runs with: the snapshot's budget and tools, and the session's
    /// sampling and thinking.
    fn turn_config(rec: &SessionRecord) -> TurnConfig {
        TurnConfig {
            step_budget: rec.snapshot.step_budget,
            tools: rec.snapshot.tools.clone(),
            enable_thinking: Self::thinking(rec),
            sampling: Self::sampling(rec),
        }
    }

    /// The compactor of one turn of `rec` under `config`: the snapshot's context strategy and
    /// threshold, starting from the session's saved summary and saving a fresh one back on the
    /// session.
    fn compactor(deps: &RuntimeDeps, rec: &SessionRecord, config: &TurnConfig) -> Compactor {
        let sink = SessionCompactionSink {
            sessions: Arc::clone(&deps.sessions),
            session: rec.id.clone(),
        };
        Compactor::new(
            rec.snapshot.context_strategy,
            rec.snapshot.compact_at_pct,
            rec.compaction.clone(),
            rec.transcript.len(),
            config.sampling.clone(),
            Arc::new(sink),
        )
        .for_session(&rec.id)
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
struct GetTraceParams {
    id: String,
    #[serde(default)]
    before: Option<u64>,
    #[serde(default)]
    limit: Option<usize>,
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
    const M_GET_TRACE: &'static str = "get_trace";
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
    /// Logged when the reset marker could not be appended to a session's trace.
    const ERR_RESET_MARKER: &'static str = "trace reset marker not written for";
    /// Logged when a deleted session's trace log could not be removed.
    const ERR_TRACE_DELETE: &'static str = "trace log not deleted for";

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
        // Taken before the record is read and the session reset, for the reason `start_turn`
        // gives: `delete_session` (which takes no busy flag) deletes the session before its
        // trace, so a delete racing this reset either makes the read or the reset fail, or
        // retires this handle and drops the marker. Taking it after the reset could recreate
        // a deleted session's log.
        let log = self.deps.traces.log(&id);
        let reset = SharedRequests::record(&self.deps, &id)
            .map_err(|_| Self::no_session())
            .and_then(|rec| {
                self.deps
                    .sessions
                    .reset(&rec.id)
                    .map_err(RpcError::internal)
            });
        if let Err(e) = reset {
            SharedRequests::forget_trace_if_gone(&self.deps, &id);
            return Err(e);
        }
        // The trace keeps its history; a failed marker only loses the divider.
        if let Err(e) = log.and_then(|log| SessionTrace::reset(&log)) {
            eprintln!("{} {id}: {e}", Self::ERR_RESET_MARKER);
        }
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
        // Taken before the record is read: a delete that lands after the read retires this
        // handle, so the turn's records never recreate a deleted session's log.
        let log = self.deps.traces.log(&id).map_err(|e| e.to_string());
        // Read after taking busy, so no other turn or reset changes it underneath.
        let rec = SharedRequests::record(&self.deps, &id).map_err(|_| {
            SharedRequests::forget_trace_if_gone(&self.deps, &id);
            Self::no_session()
        })?;
        let deps = self.deps.clone();
        let conn = self.conn;
        Ok(Box::pin(async move {
            let _busy = busy;
            Self::run_turn(&deps, conn, &id, rec, &p.content, log).await
        }))
    }

    /// Runs the turn sent by `conn`, traced into `log` (untraced, after one `trace_warning`,
    /// when there is no log or it cannot be read to number the turn). The transcript is
    /// committed only when the loop ends `Answered`, after `loop_done`; the turn record follows
    /// the commit decision and goes out before the result and `sessions_changed`.
    async fn run_turn(
        deps: &RuntimeDeps,
        conn: ConnId,
        id: &str,
        rec: SessionRecord,
        content: &str,
        log: Result<Arc<TraceLog>, String>,
    ) -> Result<Value, RpcError> {
        let config = SessionView::turn_config(&rec);
        let mut compactor = SessionView::compactor(deps, &rec, &config);
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
        let started = match log {
            Ok(log) => SessionTrace::start(log, id.to_string(), content)
                .await
                .map_err(|e| e.to_string()),
            Err(message) => Err(message),
        };
        let mut session_trace = match started {
            Ok(trace) => Some(trace),
            Err(message) => {
                // Best effort: a closed sink is noticed by the loop's first note.
                let params = SessionTrace::warning(id, &message);
                let _ = sink.notify(SessionTrace::M_TRACE_WARNING, params).await;
                None
            }
        };
        let mut no_trace = NoTrace;
        let trace: &mut dyn TurnTrace = match session_trace.as_mut() {
            Some(t) => t,
            None => &mut no_trace,
        };
        let ran = deps
            .tool_loop
            .run(
                &rec.transcript,
                content,
                &config,
                &mut compactor,
                &links,
                trace,
                &mut sink,
            )
            .await;
        let (reason, transcript) = match ran {
            Ok((end, transcript)) => (Ok(end.reason), transcript),
            Err(SinkClosed) => (Err(SinkClosed), None),
        };
        let saved = transcript.map(|t| deps.sessions.commit(id, t));
        if let Some(trace) = session_trace {
            let name =
                reason.map_or_else(|_| SessionTrace::REASON_ABORTED.to_string(), |r| r.name());
            let committed = matches!(saved, Some(Ok(())));
            // The record is in the log whatever the sink says.
            let _ = trace.finish(&name, committed, &mut sink).await;
        }
        match (reason, saved) {
            // The commit bumped `updated`, so the list order changed.
            (Ok(_), Some(Ok(()))) => SharedRequests::sessions_changed(deps),
            (Ok(_), Some(Err(e))) => Err(RpcError::internal(format!("{}{e}", Self::ERR_NOT_SAVED))),
            (Ok(_), None) => Ok(Value::Null),
            // Deleting a session detaches its connections, which closes the turn's sink.
            (Err(SinkClosed), _) if !deps.sessions.contains(id) => {
                Err(RpcError::internal(Self::ERR_DELETED))
            }
            // The sender is still connected but attached elsewhere, and no one else was.
            (Err(SinkClosed), _) if deps.hub.attached(conn).is_some_and(|s| s != id) => {
                Err(RpcError::internal(Self::ERR_DETACHED))
            }
            (Err(SinkClosed), _) => Err(RpcError::internal(Self::CLIENT_GONE)),
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
                // The session is gone either way; a leftover log is only logged.
                if let Err(e) = deps.traces.delete(&p.id) {
                    eprintln!("{} {}: {e}", RuntimeConnection::ERR_TRACE_DELETE, p.id);
                }
                Self::sessions_changed(deps)
            }
            RuntimeConnection::M_GET_TRACE => self.get_trace(params).await,
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

    /// A page of the session's trace, newest turn first, records in their log-line shape. The
    /// page reads the whole log, so it runs on the blocking pool.
    async fn get_trace(&self, params: Value) -> Result<Value, RpcError> {
        let p: GetTraceParams = Frames::params(params)?;
        // Taken before the check, so a session deleted in between never leaves a cached handle.
        let log = self.deps.traces.log(&p.id);
        if !self.deps.sessions.contains(&p.id) {
            Self::forget_trace_if_gone(&self.deps, &p.id);
            return Err(RpcError::internal(SessionError::unknown(&p.id)));
        }
        let log = log.map_err(RpcError::internal)?;
        let page = tokio::task::spawn_blocking(move || log.page(p.before, p.limit))
            .await
            .map_err(RpcError::internal)?
            .map_err(RpcError::internal)?;
        Ok(page.wire())
    }

    /// Evicts (and retires) the trace handle of a session that is not in the store, so a
    /// request that raced its delete leaves no cached handle or stray log behind. A session
    /// still in the store is left alone: its log is never touched here. Ids are never reused,
    /// so a session found gone stays gone.
    fn forget_trace_if_gone(deps: &RuntimeDeps, id: &str) {
        if deps.sessions.contains(id) {
            return;
        }
        if let Err(e) = deps.traces.delete(id) {
            // An id that is no plain file name never had a log; anything else is logged.
            if e.kind() != std::io::ErrorKind::InvalidInput {
                eprintln!("{} {id}: {e}", RuntimeConnection::ERR_TRACE_DELETE);
            }
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
    use crate::agents::template::ContextStrategy;
    use crate::engine::{ChatMessage, Role};
    use crate::rpc::test_support::TestClient;
    use crate::rpc::{ERR_INVALID_PARAMS, ERR_PARSE};
    use crate::runtime::compaction::{Summarize, SummaryNote, Trim};
    use crate::runtime::test_support::{FakeLink, Notes, Scripted, TestRuntime};
    use crate::runtime::trace::TRACE_FILE_SUFFIX;
    use crate::runtime::worker::{GenerateRequest, PromptSize};
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
    const RECORD: &str = "record";
    const TURNS: &str = "turns";
    const STEPS: &str = "steps";
    const SUMMARY: &str = "summary";
    const REASON: &str = "reason";
    const COMMITTED: &str = "committed";
    const END: &str = "end";
    const KIND: &str = "kind";
    const TURN: &str = "turn";
    const LOAD_MS: &str = "load_ms";
    const V: &str = "v";
    /// A step record's field, and its `kind`.
    const STEP: &str = "step";
    const RESET_PENDING: &str = "reset_pending";
    const WINDOW_FULL: &str = "window_full";

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

        /// The first page of `id`'s trace.
        async fn trace(&mut self, id: &str) -> Value {
            self.call(C::M_GET_TRACE, json!({ "id": id }))
                .await
                .unwrap()
        }

        /// The trace log file of session `id`.
        fn trace_file(rt: &TestRuntime, id: &str) -> std::path::PathBuf {
            rt.sessions_dir().join(format!("{id}{TRACE_FILE_SUFFIX}"))
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
        async fn link(&self, step: usize) -> Result<StepLink, LinkFailure> {
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
            [
                SessionTrace::M_TRACE_TURN,
                RuntimeServer::SESSIONS_CHANGED,
                Hub::SESSION_BUSY
            ]
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
            LinkFailure::from(SessionLinks::MODEL_CHANGED.to_string())
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
                &mut Compactor::off(),
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
    async fn save_template_unknown_context_strategy_is_rpc_error() {
        let rt = TestRuntime::new("badstrategy", Client::idle());
        let mut a = Client::new(&rt);
        let mut template = serde_json::to_value(rt.template()).unwrap();
        template["context_strategy"] = json!("shrink");
        let err = a
            .call(
                C::M_SAVE_TEMPLATE,
                json!({ "path": GROUP, "template": template }),
            )
            .await
            .unwrap_err();
        assert!(
            err.message.contains("unknown context strategy: shrink"),
            "{}",
            err.message
        );
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
        assert_eq!(err, LinkFailure::from(ToolLoop::NO_MODEL.to_string()));
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
        assert_eq!(
            links.link(1).await.err().unwrap(),
            LinkFailure::from(ToolLoop::NO_MODEL.to_string())
        );
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

    #[tokio::test]
    async fn turn_streams_trace_step_and_trace_turn_before_result() {
        let rt = TestRuntime::new("tracewire", Client::answering());
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
        // Everything in `notes` arrived before the response.
        let methods = TestClient::methods(&notes);
        let step = Client::position(&methods, SessionTrace::M_TRACE_STEP);
        let turn = Client::position(&methods, SessionTrace::M_TRACE_TURN);
        assert!(step < turn, "{methods:?}");
        assert!(turn < Client::position(&methods, RuntimeServer::SESSIONS_CHANGED));
        let steps = TestClient::params_of(&notes, SessionTrace::M_TRACE_STEP);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0]["id"], id);
        assert_eq!(steps[0][RECORD][STEP], 1);
        let summary = &TestClient::params_of(&notes, SessionTrace::M_TRACE_TURN)[0];
        assert_eq!(summary["id"], id);
        assert_eq!(summary[RECORD][REASON], "answered");
        assert_eq!(summary[RECORD][COMMITTED], true);
        let (_, page) = a.request(C::M_GET_TRACE, json!({ "id": id })).await;
        let page = &page[RESULT];
        assert_eq!(
            (page["more"].clone(), page[RESET_PENDING].clone()),
            (json!(false), json!(false))
        );
        let turns = page[TURNS].as_array().unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0][STEPS].as_array().unwrap().len(), 1);
        assert_eq!(turns[0][STEPS][0][KIND], STEP);
        assert_eq!(turns[0][SUMMARY][KIND], "turn");
        assert_eq!(turns[0][SUMMARY][V], 1);
        for field in [TURN, REASON, COMMITTED, "total_ms", "user"] {
            assert_eq!(turns[0][SUMMARY][field], summary[RECORD][field], "{field}");
        }
    }

    #[tokio::test]
    async fn failed_turn_is_traced_uncommitted() {
        let link = FakeLink::new(
            true,
            vec![Notes::script(vec![Notes::started(), Notes::rejected(9, 8)])],
        );
        let rt = TestRuntime::new("tracefail", link);
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        a.drain();
        assert_eq!(a.send().await.unwrap(), Value::Null);
        let frames = a.drain();
        let summary = &TestClient::params_of(&frames, SessionTrace::M_TRACE_TURN)[0][RECORD];
        assert_eq!(summary[REASON], WINDOW_FULL);
        assert_eq!(summary[COMMITTED], false);
        let page = a.trace(&id).await;
        assert_eq!(page[TURNS][0][STEPS][0][END], "rejected");
        assert_eq!(page[TURNS][0][SUMMARY][REASON], WINDOW_FULL);
        assert!(rt.deps.sessions.get(&id).unwrap().transcript.is_empty());
    }

    #[tokio::test]
    async fn aborted_turn_is_in_the_log() {
        let rt = TestRuntime::new("traceabort", Client::answering());
        let gate = Client::gated(&rt);
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        let turn = a.spawn_send();
        Client::until_loading(&rt).await;
        a.hang_up();
        gate.add_permits(1);
        let err = turn.await.unwrap().unwrap_err();
        assert_eq!(err.message, C::CLIENT_GONE);
        let mut b = Client::new(&rt);
        let page = b.trace(&id).await;
        let turn = &page[TURNS][0];
        let steps = turn[STEPS].as_array().unwrap();
        assert_eq!(steps.last().unwrap()[END], "aborted");
        assert_eq!(turn[SUMMARY][REASON], SessionTrace::REASON_ABORTED);
        assert_eq!(turn[SUMMARY][COMMITTED], false);
    }

    #[tokio::test]
    async fn first_message_load_records_load_ms() {
        let link = FakeLink::repeating(true, Client::answer(ANSWER));
        let rt = TestRuntime::new("traceload", link);
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        // The first turn loads the model, the second finds it loaded.
        a.send().await.unwrap();
        a.send().await.unwrap();
        assert_eq!(rt.host.ensure_ready_calls(), 1);
        let page = a.trace(&id).await;
        let (second, first) = (&page[TURNS][0], &page[TURNS][1]);
        assert_eq!(
            (first[TURN].clone(), second[TURN].clone()),
            (json!(1), json!(2))
        );
        assert!(first[STEPS][0][LOAD_MS].is_u64(), "{first}");
        assert_eq!(second[STEPS][0][LOAD_MS], Value::Null);
    }

    #[tokio::test]
    async fn failed_first_message_load_keeps_its_load_ms() {
        let rt = TestRuntime::new("traceloadfail", Client::idle());
        rt.host.fail_ensure(Some(LOAD_ERROR));
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        assert_eq!(a.send().await.unwrap(), Value::Null);
        assert_eq!(rt.host.ensure_ready_calls(), 1);
        let page = a.trace(&id).await;
        let step = &page[TURNS][0][STEPS][0];
        assert_eq!(step[END], "error");
        assert_eq!(step["error"], LOAD_ERROR);
        assert!(step[LOAD_MS].is_u64(), "{step}");
        assert_eq!(page[TURNS][0][SUMMARY][REASON], "no_harness");
    }

    #[tokio::test]
    async fn unwritable_log_numbers_each_turn_and_both_commit() {
        let link = FakeLink::repeating(true, Client::answer(ANSWER));
        let rt = TestRuntime::new("tracewarn", link);
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        // A log that can be read (so turns are numbered) but not written.
        let file = Client::trace_file(&rt, &id);
        std::fs::write(&file, "").unwrap();
        let mut perms = std::fs::metadata(&file).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&file, perms.clone()).unwrap();
        a.drain();
        let mut turns = Vec::new();
        for _ in 0..2 {
            assert_eq!(a.send().await.unwrap(), Value::Null);
            let frames = a.drain();
            let warnings = TestClient::params_of(&frames, SessionTrace::M_TRACE_WARNING);
            assert!(!warnings.is_empty(), "{:?}", Client::methods(&frames));
            assert_eq!(warnings[0]["id"], id.as_str());
            assert!(!warnings[0]["message"].as_str().unwrap().is_empty());
            let summary = &TestClient::params_of(&frames, SessionTrace::M_TRACE_TURN)[0][RECORD];
            assert_eq!(summary[COMMITTED], true);
            let step = &TestClient::params_of(&frames, SessionTrace::M_TRACE_STEP)[0][RECORD];
            assert_eq!(step[TURN], summary[TURN]);
            turns.push(summary[TURN].clone());
        }
        // Each turn reserved its own number although neither reached the file.
        assert_eq!(turns, [json!(1), json!(2)]);
        assert_eq!(rt.deps.sessions.get(&id).unwrap().transcript.len(), 4);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "");
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&file, perms).unwrap();
    }

    #[tokio::test]
    async fn unreadable_log_runs_the_turn_untraced_with_one_warning() {
        let rt = TestRuntime::new("traceunread", Client::answering());
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        // A directory in the log's place: the turn number cannot be read.
        std::fs::create_dir(Client::trace_file(&rt, &id)).unwrap();
        a.drain();
        assert_eq!(a.send().await.unwrap(), Value::Null);
        let frames = a.drain();
        let methods = Client::methods(&frames);
        let warnings = TestClient::params_of(&frames, SessionTrace::M_TRACE_WARNING);
        assert_eq!(warnings.len(), 1, "{methods:?}");
        assert_eq!(warnings[0]["id"], id.as_str());
        assert!(!warnings[0]["message"].as_str().unwrap().is_empty());
        for traced in [SessionTrace::M_TRACE_STEP, SessionTrace::M_TRACE_TURN] {
            assert!(!methods.iter().any(|m| m == traced), "{methods:?}");
        }
        assert!(methods.iter().any(|m| m == LOOP_DONE), "{methods:?}");
        assert_eq!(rt.deps.sessions.get(&id).unwrap().transcript.len(), 2);
    }

    #[tokio::test]
    async fn get_trace_unknown_session_errors() {
        let rt = TestRuntime::new("traceunknown", Client::idle());
        let mut a = Client::new(&rt);
        let missing = "nope";
        let err = a
            .call(C::M_GET_TRACE, json!({ "id": missing }))
            .await
            .unwrap_err();
        assert_eq!(err.code, crate::rpc::ERR_INTERNAL);
        assert_eq!(err.message, SessionError::unknown(missing).0);
    }

    #[tokio::test]
    async fn get_trace_needs_no_attach() {
        let rt = TestRuntime::new("tracenoattach", Client::idle());
        let mut a = Client::new(&rt);
        let id = a.create().await;
        assert_eq!(
            a.trace(&id).await,
            json!({ "turns": [], "more": false, "reset_pending": false })
        );
        let page = a
            .call(C::M_GET_TRACE, json!({ "id": id, "before": 5, "limit": 3 }))
            .await
            .unwrap();
        assert_eq!(page[TURNS], json!([]));
    }

    #[tokio::test]
    async fn delete_removes_trace_and_reset_appends_marker() {
        let rt = TestRuntime::new("tracedelete", Client::answering());
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        a.send().await.unwrap();
        a.call(C::M_RESET, Value::Null).await.unwrap();
        let page = a.trace(&id).await;
        assert_eq!(page[RESET_PENDING], true);
        assert_eq!(page[TURNS].as_array().unwrap().len(), 1);
        let file = Client::trace_file(&rt, &id);
        assert!(file.is_file());
        a.call(C::M_DELETE_SESSION, json!({ "id": id }))
            .await
            .unwrap();
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn turn_numbers_continue_after_restart() {
        let rt = TestRuntime::new("tracerestart", Client::answering());
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        a.send().await.unwrap();
        drop(a);
        let rt = rt.reopen(Client::answering());
        rt.load();
        let mut b = Client::new(&rt);
        b.attach(&id).await;
        b.drain();
        b.send().await.unwrap();
        let frames = b.drain();
        let summary = &TestClient::params_of(&frames, SessionTrace::M_TRACE_TURN)[0][RECORD];
        assert_eq!(summary[TURN], 2);
        let page = b.trace(&id).await;
        let turns: Vec<&Value> = page[TURNS]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| &t[TURN])
            .collect();
        assert_eq!(turns, [&json!(2), &json!(1)]);
    }

    #[tokio::test]
    async fn live_trace_step_is_byte_equal_to_the_paged_step() {
        let rt = TestRuntime::new("tracesame", Client::answering());
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        a.drain();
        a.send().await.unwrap();
        let frames = a.drain();
        let live = &TestClient::params_of(&frames, SessionTrace::M_TRACE_STEP)[0][RECORD];
        let live_turn = &TestClient::params_of(&frames, SessionTrace::M_TRACE_TURN)[0][RECORD];
        let page = a.trace(&id).await;
        assert_eq!(live[V], 1);
        assert_eq!(live[KIND], STEP);
        assert_eq!(
            serde_json::to_string(live).unwrap(),
            serde_json::to_string(&page[TURNS][0][STEPS][0]).unwrap()
        );
        assert_eq!(
            serde_json::to_string(live_turn).unwrap(),
            serde_json::to_string(&page[TURNS][0][SUMMARY]).unwrap()
        );
    }

    #[tokio::test]
    async fn get_trace_on_a_gone_session_forgets_its_log_only() {
        let rt = TestRuntime::new("tracegone", Client::answering());
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        a.send().await.unwrap();
        // A stray log of a session the store does not hold is evicted and removed...
        let gone = "s-0-0";
        let stray = Client::trace_file(&rt, gone);
        std::fs::write(&stray, "").unwrap();
        let err = a
            .call(C::M_GET_TRACE, json!({ "id": gone }))
            .await
            .unwrap_err();
        assert_eq!(err.message, SessionError::unknown(gone).0);
        assert!(!stray.exists());
        // ...while a live session's log is left alone.
        let page = a.trace(&id).await;
        assert_eq!(page[TURNS].as_array().unwrap().len(), 1);
        assert!(Client::trace_file(&rt, &id).is_file());
    }

    /// Session-level compaction fixtures.
    struct Compacting;

    impl Compacting {
        /// The window the measures report; the default budget is 75% of it.
        const N_CTX: usize = 3000;
        /// The length of each long question in [`Self::long_turns`].
        const LONG: usize = 1200;
        const SUMMARY: &'static str = "S";
        /// A summary saved before a reset.
        const OLD: &'static str = "OLD";
        /// A summary saved on a live session.
        const KEPT: &'static str = "kept";
        /// A summary saved after the session was deleted.
        const LATE: &'static str = "late";
        const COMPACTION: &'static str = "compaction";
        const STRATEGY: &'static str = "strategy";
        const REUSED: &'static str = "reused_summary";
        const SUMMARIZED: &'static str = "summarized_messages";
        const SUMMARY_MS: &'static str = "summary_ms";

        /// One token per byte of the request's serialized messages.
        fn by_bytes(req: &GenerateRequest) -> PromptSize {
            PromptSize {
                prompt_tokens: serde_json::to_string(&req.messages).unwrap().len(),
                n_ctx: Self::N_CTX,
            }
        }

        /// Two earlier plain turns with long questions, as a committed transcript.
        fn long_turns() -> Vec<ChatMessage> {
            vec![
                ChatMessage::new(Role::User, "a".repeat(Self::LONG)),
                ChatMessage::new(Role::Assistant, "r1"),
                ChatMessage::new(Role::User, "b".repeat(Self::LONG)),
                ChatMessage::new(Role::Assistant, "r2"),
            ]
        }

        /// A summary reply of [`Self::SUMMARY`], one token long.
        fn summary_reply() -> Scripted {
            Notes::script(vec![
                Notes::started(),
                Notes::chunk_of(Self::SUMMARY, 1),
                Notes::done(Self::SUMMARY, vec![]),
            ])
        }

        /// A link answering a summary call and then `answers` turns, measured by bytes.
        fn summarizing(answers: usize) -> Arc<FakeLink> {
            let mut scripts = vec![Self::summary_reply()];
            scripts.extend((0..answers).map(|_| Client::answer(ANSWER)));
            let link = FakeLink::new(true, scripts);
            link.set_measure_by(Self::by_bytes);
            link
        }

        /// Saves the seeded template with `strategy`.
        fn set_strategy(rt: &TestRuntime, strategy: ContextStrategy) {
            let mut template = rt.template();
            template.context_strategy = strategy;
            rt.deps
                .templates
                .save(Some(TestRuntime::TEMPLATE_ID), GROUP, &template)
                .unwrap();
        }

        /// The `compaction` of each `trace_step` in `frames`.
        fn records(frames: &[Value]) -> Vec<Value> {
            TestClient::params_of(frames, SessionTrace::M_TRACE_STEP)
                .iter()
                .map(|p| p[RECORD][Self::COMPACTION].clone())
                .collect()
        }

        fn state(summary: &str, covers: usize) -> CompactionState {
            CompactionState {
                summary: summary.into(),
                covers,
                tokens: 1,
                at: 1,
            }
        }
    }

    #[tokio::test]
    async fn summarize_session_saves_summary_and_reuses_it_next_turn() {
        let rt = TestRuntime::new("summarize", Compacting::summarizing(2));
        Compacting::set_strategy(&rt, ContextStrategy::Summarize);
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        rt.deps
            .sessions
            .commit(&id, Compacting::long_turns())
            .unwrap();
        a.drain();

        a.send().await.unwrap();
        let frames = a.drain();
        let saved = rt.deps.sessions.get(&id).unwrap().compaction;
        let saved = saved.expect("turn 1 saves the summary");
        assert_eq!(
            (saved.summary.as_str(), saved.covers),
            (Compacting::SUMMARY, 2)
        );
        let records = Compacting::records(&frames);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0][Compacting::STRATEGY], Summarize::STRATEGY);
        assert_eq!(records[0][Compacting::REUSED], false);
        assert_eq!(records[0][Compacting::SUMMARIZED], 2);
        assert!(records[0][Compacting::SUMMARY_MS].is_u64());
        // The summary call's own notes stay inside the compactor.
        let methods = Client::methods(&frames);
        assert_eq!(methods.iter().filter(|m| *m == TURN_STARTED).count(), 1);

        a.send().await.unwrap();
        let records = Compacting::records(&a.drain());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0][Compacting::REUSED], true);
        assert_eq!(records[0][Compacting::SUMMARY_MS], Value::Null);
        let requests = rt.host.link().requests();
        assert_eq!(requests.len(), 3, "one summary call over two turns");
        assert_eq!(
            requests[2].messages[0],
            SummaryNote::message(Compacting::SUMMARY)
        );
        // The reused summary is unchanged, and the stored transcript is whole.
        let rec = rt.deps.sessions.get(&id).unwrap();
        assert_eq!(rec.compaction, Some(saved));
        assert_eq!(rec.transcript.len(), 8);
        assert_eq!(rec.transcript[..4], Compacting::long_turns()[..]);
    }

    #[tokio::test]
    async fn reset_clears_saved_summary() {
        let rt = TestRuntime::new("resetsummary", Compacting::summarizing(1));
        Compacting::set_strategy(&rt, ContextStrategy::Summarize);
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        rt.deps
            .sessions
            .set_compaction(&id, Some(Compacting::state(Compacting::OLD, 2)))
            .unwrap();
        a.call(C::M_RESET, Value::Null).await.unwrap();
        assert_eq!(rt.deps.sessions.get(&id).unwrap().compaction, None);
        let reopened = SessionStore::open(&rt.sessions_dir()).unwrap();
        assert_eq!(reopened.get(&id).unwrap().compaction, None);

        // The next compaction summarises afresh from message 0, with no old summary folded in.
        rt.deps
            .sessions
            .commit(&id, Compacting::long_turns())
            .unwrap();
        a.send().await.unwrap();
        let summary_call = &rt.host.link().requests()[0];
        let span = &summary_call.messages[1].content;
        assert!(span.starts_with("user: aaa"), "{span}");
        assert!(!span.contains(Compacting::OLD));
        let saved = rt.deps.sessions.get(&id).unwrap().compaction.unwrap();
        assert_eq!(
            (saved.summary.as_str(), saved.covers),
            (Compacting::SUMMARY, 2)
        );
    }

    #[tokio::test]
    async fn template_edit_after_create_does_not_change_session_strategy() {
        let link = Client::answering();
        link.set_measure(PromptSize {
            prompt_tokens: Compacting::N_CTX - 100,
            n_ctx: Compacting::N_CTX,
        });
        let rt = TestRuntime::new("snapstrategy", link);
        Compacting::set_strategy(&rt, ContextStrategy::Trim);
        rt.load();
        let mut a = Client::new(&rt);
        let id = a.open_session().await;
        Compacting::set_strategy(&rt, ContextStrategy::Off);
        a.drain();
        a.send().await.unwrap();
        let records = Compacting::records(&a.drain());
        assert_eq!(records[0][Compacting::STRATEGY], Trim::STRATEGY);
        let rec = rt.deps.sessions.get(&id).unwrap();
        assert_eq!(rec.snapshot.context_strategy, ContextStrategy::Trim);
        assert_eq!(rt.host.link().measured().len(), 1);
    }

    #[tokio::test]
    async fn compaction_sink_leaves_a_deleted_session_alone() {
        let rt = TestRuntime::new("sinkgone", Client::idle());
        let rec = rt
            .deps
            .sessions
            .create(TestRuntime::TEMPLATE_ID, &rt.template())
            .unwrap();
        let sink = SessionCompactionSink {
            sessions: Arc::clone(&rt.deps.sessions),
            session: rec.id.clone(),
        };
        sink.save(Compacting::state(Compacting::KEPT, 1))
            .await
            .unwrap();
        let kept = rt.deps.sessions.get(&rec.id).unwrap().compaction;
        assert_eq!(kept, Some(Compacting::state(Compacting::KEPT, 1)));
        rt.deps.sessions.delete(&rec.id).unwrap();
        assert_eq!(
            sink.save(Compacting::state(Compacting::LATE, 1)).await,
            Ok(())
        );
        assert!(!rt.deps.sessions.contains(&rec.id));
    }
}
