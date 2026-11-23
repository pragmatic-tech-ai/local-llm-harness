//! The runtime's connection hub: every client connection's outbound channel, which session each
//! connection is attached to, and which sessions are running a turn.
//!
//! Routing follows the spec's notification rules: list-level notifications (`sessions_changed`,
//! `templates_changed`, `placement_changed`) go to every connection through [`Hub::broadcast`];
//! a session's turn notifications and `session_loading` go only to the connections attached to
//! it through [`Hub::to_session`]. Every frame for a connection, responses included, goes
//! through its one channel, so frames never interleave and a notification caused by another
//! connection never blocks the sender.
//!
//! `busy` is runtime-only state (never persisted): a session id is in it while a turn (or a
//! reset) runs. Callers take it through [`Hub::hold_busy`], which broadcasts
//! `session_busy {id, busy: true}`; its guard clears it and broadcasts `busy: false` on every
//! exit path, panics included.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::rpc::Frames;

/// Identifies one registered connection.
pub type ConnId = u64;

/// One registered connection.
struct Conn {
    tx: mpsc::UnboundedSender<Value>,
    session: Option<String>,
}

#[derive(Default)]
struct HubState {
    next: ConnId,
    conns: HashMap<ConnId, Conn>,
    busy: HashSet<String>,
}

#[derive(Default)]
pub struct Hub {
    state: Mutex<HubState>,
}

impl Hub {
    /// Broadcast `{id, busy}` when a session's busy flag is taken or released by a guard.
    pub const SESSION_BUSY: &'static str = "session_busy";

    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a connection; the receiver yields every frame meant for it, in order.
    pub fn register(&self) -> (ConnId, mpsc::UnboundedReceiver<Value>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut state = self.lock();
        state.next += 1;
        let id = state.next;
        state.conns.insert(id, Conn { tx, session: None });
        (id, rx)
    }

    /// Removes a connection; its receiver ends once the frames already queued are drained.
    pub fn unregister(&self, conn: ConnId) {
        self.lock().conns.remove(&conn);
    }

    /// Attaches `conn` to `session` (or detaches it with `None`). A connection is attached to
    /// at most one session.
    pub fn attach(&self, conn: ConnId, session: Option<String>) {
        if let Some(c) = self.lock().conns.get_mut(&conn) {
            c.session = session;
        }
    }

    /// The session `conn` is attached to.
    pub fn attached(&self, conn: ConnId) -> Option<String> {
        self.lock().conns.get(&conn)?.session.clone()
    }

    /// Detaches every connection attached to `session`.
    pub fn detach_session(&self, session: &str) {
        for c in self.lock().conns.values_mut() {
            if c.session.as_deref() == Some(session) {
                c.session = None;
            }
        }
    }

    /// Queues `frame` for `conn`; `false` when the connection is gone.
    pub fn send(&self, conn: ConnId, frame: Value) -> bool {
        let mut state = self.lock();
        let sent = state
            .conns
            .get(&conn)
            .is_some_and(|c| c.tx.send(frame).is_ok());
        if !sent {
            state.conns.remove(&conn);
        }
        sent
    }

    /// Sends a notification to every connection.
    pub fn broadcast(&self, method: &str, params: Value) {
        let frame = Frames::notification(method, params);
        let mut state = self.lock();
        state.conns.retain(|_, c| c.tx.send(frame.clone()).is_ok());
    }

    /// Sends a notification to the connections attached to `session`; returns how many it
    /// reached. Connections whose channel has closed are dropped from the hub.
    pub fn to_session(&self, session: &str, method: &str, params: Value) -> usize {
        let frame = Frames::notification(method, params);
        let mut state = self.lock();
        let mut reached = 0;
        state.conns.retain(|_, c| {
            if c.session.as_deref() != Some(session) {
                return true;
            }
            let alive = c.tx.send(frame.clone()).is_ok();
            reached += usize::from(alive);
            alive
        });
        reached
    }

    /// Marks `session` busy; `false` when it already is.
    pub fn try_busy(&self, session: &str) -> bool {
        self.lock().busy.insert(session.to_string())
    }

    pub fn clear_busy(&self, session: &str) {
        self.lock().busy.remove(session);
    }

    pub fn is_busy(&self, session: &str) -> bool {
        self.lock().busy.contains(session)
    }

    /// Marks `session` busy for the guard's lifetime; `None` when it already is. Taking and
    /// releasing are broadcast as `session_busy`.
    pub fn hold_busy(self: &Arc<Self>, session: &str) -> Option<BusyGuard> {
        if !self.try_busy(session) {
            return None;
        }
        self.broadcast_busy(session, true);
        Some(BusyGuard {
            hub: Arc::clone(self),
            session: session.to_string(),
        })
    }

    fn broadcast_busy(&self, session: &str, busy: bool) {
        self.broadcast(Self::SESSION_BUSY, json!({ "id": session, "busy": busy }));
    }

    /// Locks the state, recovering it if a panicking thread poisoned the mutex.
    fn lock(&self) -> MutexGuard<'_, HubState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Clears a session's busy flag when dropped.
pub struct BusyGuard {
    hub: Arc<Hub>,
    session: String,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.hub.clear_busy(&self.session);
        self.hub.broadcast_busy(&self.session, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "s-1";
    const OTHER: &str = "s-2";
    const NOTE: &str = "turn_started";
    const LIST_NOTE: &str = "sessions_changed";
    const METHOD: &str = "method";
    const PARAMS: &str = "params";

    #[test]
    fn broadcast_reaches_every_connection() {
        let hub = Hub::new();
        let (_, mut a) = hub.register();
        let (_, mut b) = hub.register();
        hub.broadcast(LIST_NOTE, json!({}));
        assert_eq!(a.try_recv().unwrap()[METHOD], LIST_NOTE);
        assert_eq!(b.try_recv().unwrap()[METHOD], LIST_NOTE);
    }

    #[test]
    fn to_session_reaches_only_attached_connections() {
        let hub = Hub::new();
        let (a_id, mut a) = hub.register();
        let (b_id, mut b) = hub.register();
        let (_, mut c) = hub.register();
        hub.attach(a_id, Some(SESSION.into()));
        hub.attach(b_id, Some(OTHER.into()));
        assert_eq!(hub.to_session(SESSION, NOTE, json!({})), 1);
        assert_eq!(a.try_recv().unwrap()[METHOD], NOTE);
        assert!(b.try_recv().is_err());
        assert!(c.try_recv().is_err());
    }

    #[test]
    fn closed_receivers_are_not_counted_and_are_dropped() {
        let hub = Hub::new();
        let (a_id, a) = hub.register();
        let (b_id, mut b) = hub.register();
        hub.attach(a_id, Some(SESSION.into()));
        hub.attach(b_id, Some(SESSION.into()));
        drop(a);
        assert_eq!(hub.to_session(SESSION, NOTE, json!({})), 1);
        assert!(b.try_recv().is_ok());
        assert_eq!(hub.attached(a_id), None);
        assert!(!hub.send(a_id, json!({})));
    }

    #[test]
    fn detach_session_detaches_all_its_connections() {
        let hub = Hub::new();
        let (a_id, _a) = hub.register();
        let (b_id, _b) = hub.register();
        hub.attach(a_id, Some(SESSION.into()));
        hub.attach(b_id, Some(OTHER.into()));
        hub.detach_session(SESSION);
        assert_eq!(hub.attached(a_id), None);
        assert_eq!(hub.attached(b_id).as_deref(), Some(OTHER));
        assert_eq!(hub.to_session(SESSION, NOTE, json!({})), 0);
    }

    #[test]
    fn busy_guard_clears_on_drop_and_on_panic() {
        let hub = Arc::new(Hub::new());
        let guard = hub.hold_busy(SESSION).expect("free");
        assert!(hub.hold_busy(SESSION).is_none());
        assert!(!hub.try_busy(SESSION));
        drop(guard);
        assert!(!hub.is_busy(SESSION));
        let shared = Arc::clone(&hub);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = shared.hold_busy(SESSION).expect("free");
            panic!("turn panicked");
        }));
        assert!(panicked.is_err());
        assert!(!hub.is_busy(SESSION));
    }

    #[test]
    fn busy_guard_broadcasts_taking_and_releasing() {
        let hub = Arc::new(Hub::new());
        let (_, mut other) = hub.register();
        let guard = hub.hold_busy(SESSION).expect("free");
        assert!(hub.hold_busy(SESSION).is_none());
        drop(guard);
        let frames: Vec<Value> = std::iter::from_fn(|| other.try_recv().ok()).collect();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0][METHOD], Hub::SESSION_BUSY);
        assert_eq!(frames[0][PARAMS], json!({"id": SESSION, "busy": true}));
        assert_eq!(frames[1][PARAMS], json!({"id": SESSION, "busy": false}));
    }
}
