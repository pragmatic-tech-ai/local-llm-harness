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
//! exit path, panics included. Each flag change and its broadcast happen under one lock.
//!
//! The hub also records the last announced placement per session: `create_session` seeds it,
//! and the placement watcher replaces it (under one lock) on every dispatcher signal and
//! announces the differences.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::agents::dispatcher::Placement;
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
    /// The last placement announced (or seeded) per session; see [`Hub::update_placements`].
    placements: Mutex<HashMap<String, Placement>>,
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
        Self::broadcast_in(&mut self.lock(), method, params);
    }

    /// `broadcast` with the state already locked.
    fn broadcast_in(state: &mut HubState, method: &str, params: Value) {
        let frame = Frames::notification(method, params);
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
    /// The flag change and its broadcast happen under one lock, so the `session_busy` frames
    /// every connection sees are in the same order as the flag changes.
    pub fn hold_busy(self: &Arc<Self>, session: &str) -> Option<BusyGuard> {
        let mut state = self.lock();
        if !state.busy.insert(session.to_string()) {
            return None;
        }
        Self::broadcast_busy(&mut state, session, true);
        Some(BusyGuard {
            hub: Arc::clone(self),
            session: session.to_string(),
        })
    }

    /// Clears the flag and broadcasts `busy: false` under one lock.
    fn release_busy(&self, session: &str) {
        let mut state = self.lock();
        state.busy.remove(session);
        Self::broadcast_busy(&mut state, session, false);
    }

    fn broadcast_busy(state: &mut HubState, session: &str, busy: bool) {
        Self::broadcast_in(
            state,
            Self::SESSION_BUSY,
            json!({ "id": session, "busy": busy }),
        );
    }

    /// Records `placement` as announced for a new session unless one is already recorded
    /// (a placement watcher update that ran first wins).
    pub fn seed_placement(&self, session: &str, placement: Placement) {
        self.lock_placements()
            .entry(session.to_string())
            .or_insert(placement);
    }

    /// Replaces the recorded placements with `compute()`'s, under one lock so a concurrent
    /// seed is never lost, and returns those that differ from what was recorded (including
    /// sessions with nothing recorded). Sessions missing from `compute()` are forgotten.
    pub fn update_placements(
        &self,
        compute: impl FnOnce() -> Vec<(String, Placement)>,
    ) -> Vec<(String, Placement)> {
        let mut recorded = self.lock_placements();
        let now = compute();
        let changed = now
            .iter()
            .filter(|(id, p)| recorded.get(id) != Some(p))
            .cloned()
            .collect();
        *recorded = now.into_iter().collect();
        changed
    }

    fn lock_placements(&self) -> MutexGuard<'_, HashMap<String, Placement>> {
        self.placements.lock().unwrap_or_else(|e| e.into_inner())
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
        self.hub.release_busy(&self.session);
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

    struct Fixture;

    impl Fixture {
        fn ready() -> Placement {
            Placement::Ready { host: OTHER.into() }
        }
    }

    #[test]
    fn seeded_placement_survives_and_only_changes_are_returned() {
        let hub = Hub::new();
        hub.seed_placement(SESSION, Fixture::ready());
        // A watcher update computed elsewhere keeps the seed when it agrees...
        assert!(hub
            .update_placements(|| vec![(SESSION.into(), Fixture::ready())])
            .is_empty());
        // ...and an update that ran first is not overwritten by a later seed.
        let gone = Placement::Unavailable {
            reason: NOTE.into(),
        };
        assert_eq!(
            hub.update_placements(|| vec![(SESSION.into(), gone.clone())]),
            vec![(SESSION.to_string(), gone.clone())]
        );
        hub.seed_placement(SESSION, Fixture::ready());
        assert!(hub
            .update_placements(|| vec![(SESSION.into(), gone.clone())])
            .is_empty());
        // Nothing recorded counts as a change.
        assert_eq!(
            hub.update_placements(|| vec![(OTHER.into(), Fixture::ready())])
                .len(),
            1
        );
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
