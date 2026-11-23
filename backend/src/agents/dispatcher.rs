//! The dispatcher: decides which model host serves a session's model requirement, and when the
//! model loads.
//!
//! Placement is a replaceable [`PlacementPolicy`]. The shipped [`ExplicitLoadPolicy`] is the
//! "explicit load" rule: a session opens read-only (`status` never loads anything) and the model
//! loads only on an explicit [`Dispatcher::load`] or on the first message
//! ([`Dispatcher::link_for_turn`]).

use std::sync::{Arc, Mutex};

use futures::stream::{self, StreamExt};
use serde::Serialize;
use tokio::sync::broadcast;

use crate::agents::host::{ModelHost, ModelRequirement};
use crate::agents::session::SessionRecord;
use crate::runtime::worker::WorkerLink;

/// Where a requirement stands right now. Serialized as `{"state":"ready","host":..}`,
/// `{"state":"needs_load","host":..,"evicts":..}` or `{"state":"unavailable","reason":..}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Placement {
    Ready {
        host: String,
    },
    NeedsLoad {
        host: String,
        /// The model name currently loaded on `host` (what a load would evict), if any.
        evicts: Option<String>,
    },
    Unavailable {
        reason: String,
    },
}

/// A dispatch operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct DispatchError(pub String);

/// Chooses a host for a requirement and says whether a turn may trigger a load.
pub trait PlacementPolicy: Send + Sync {
    fn choose_host<'a>(
        &self,
        req: &ModelRequirement,
        hosts: &'a [Arc<dyn ModelHost>],
    ) -> Option<&'a Arc<dyn ModelHost>>;

    /// Whether sending a message may load the model (otherwise an explicit load is required).
    fn turn_may_load(&self) -> bool;
}

/// The preferred host if it serves the requirement, otherwise the first host that does; the
/// first message may load.
pub struct ExplicitLoadPolicy;

impl PlacementPolicy for ExplicitLoadPolicy {
    fn choose_host<'a>(
        &self,
        req: &ModelRequirement,
        hosts: &'a [Arc<dyn ModelHost>],
    ) -> Option<&'a Arc<dyn ModelHost>> {
        if let Some(preferred) = req.host.as_deref() {
            if let Some(host) = hosts
                .iter()
                .find(|h| h.name() == preferred && h.serves(req))
            {
                return Some(host);
            }
        }
        hosts.iter().find(|h| h.serves(req))
    }

    fn turn_may_load(&self) -> bool {
        true
    }
}

pub struct Dispatcher {
    hosts: Vec<Arc<dyn ModelHost>>,
    policy: Box<dyn PlacementPolicy>,
    events: broadcast::Sender<()>,
    /// The single task merging every host's signal into `events`, started by the first
    /// `subscribe` and aborted when the dispatcher is dropped.
    merger: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Dispatcher {
    const EVENT_CAPACITY: usize = 16;
    const NO_HOST_PREFIX: &'static str = "no host serves ";
    const ERR_NOT_LOADED: &'static str = "model not loaded";

    pub fn new(hosts: Vec<Arc<dyn ModelHost>>, policy: Box<dyn PlacementPolicy>) -> Self {
        Dispatcher {
            hosts,
            policy,
            events: broadcast::channel(Self::EVENT_CAPACITY).0,
            merger: Mutex::new(None),
        }
    }

    /// The requirement a session's template snapshot asks for.
    pub fn requirement(record: &SessionRecord) -> ModelRequirement {
        ModelRequirement {
            runtime: record.snapshot.runtime.clone(),
            model: record.snapshot.model.clone(),
            n_ctx: record.snapshot.n_ctx,
            host: record.snapshot.host.clone(),
        }
    }

    fn unavailable_reason(req: &ModelRequirement) -> String {
        format!("{}{}/{}", Self::NO_HOST_PREFIX, req.runtime, req.model)
    }

    fn host_for(&self, req: &ModelRequirement) -> Result<&Arc<dyn ModelHost>, DispatchError> {
        self.policy
            .choose_host(req, &self.hosts)
            .ok_or_else(|| DispatchError(Self::unavailable_reason(req)))
    }

    /// Where `req` stands. Never loads anything.
    pub fn status(&self, req: &ModelRequirement) -> Placement {
        let Some(host) = self.policy.choose_host(req, &self.hosts) else {
            return Placement::Unavailable {
                reason: Self::unavailable_reason(req),
            };
        };
        let host_name = host.name().to_string();
        if host.current_link(req).is_some() {
            Placement::Ready { host: host_name }
        } else {
            Placement::NeedsLoad {
                host: host_name,
                evicts: host.state().loaded.map(|(_, model, _)| model),
            }
        }
    }

    /// A link to the model if it is loaded right now; no side effects. Used for loop steps after
    /// the first, so an eviction mid-turn surfaces as an error.
    pub fn current_link(&self, req: &ModelRequirement) -> Option<Arc<dyn WorkerLink>> {
        self.policy.choose_host(req, &self.hosts)?.current_link(req)
    }

    /// Load the model (evicting what is loaded on the chosen host) and return its link.
    pub async fn load(&self, req: &ModelRequirement) -> Result<Arc<dyn WorkerLink>, DispatchError> {
        let host = self.host_for(req)?;
        host.ensure_ready(req).await.map_err(|e| DispatchError(e.0))
    }

    /// The link for a turn's first step. If the model is not loaded, either refuses (policy
    /// forbids loading on a turn) or calls `notify_loading` once and loads it. No lock is held
    /// across the load, so `status` calls from other connections are never blocked by it.
    pub async fn link_for_turn(
        &self,
        req: &ModelRequirement,
        notify_loading: impl FnOnce(),
    ) -> Result<Arc<dyn WorkerLink>, DispatchError> {
        let host = self.host_for(req)?;
        if let Some(link) = host.current_link(req) {
            return Ok(link);
        }
        if !self.policy.turn_may_load() {
            return Err(DispatchError(Self::ERR_NOT_LOADED.to_string()));
        }
        notify_loading();
        host.ensure_ready(req).await.map_err(|e| DispatchError(e.0))
    }

    /// Unload `req`'s model from its host. Does nothing if that host has some other model
    /// (runtime or model name differs) loaded, so one session cannot evict another's model.
    pub async fn unload(&self, req: &ModelRequirement) -> Result<(), DispatchError> {
        let host = self.host_for(req)?;
        let holds_it = host
            .state()
            .loaded
            .is_some_and(|(runtime, model, _)| runtime == req.runtime && model == req.model);
        if !holds_it {
            return Ok(());
        }
        host.unload().await.map_err(|e| DispatchError(e.0))
    }

    /// Fires whenever any host's state changes. Must be called inside a tokio runtime.
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        // Take the receiver first so the caller cannot miss an event sent once the merger runs.
        let rx = self.events.subscribe();
        let mut merger = self.merger.lock().unwrap_or_else(|e| e.into_inner());
        if merger.is_none() {
            let sources: Vec<_> = self
                .hosts
                .iter()
                .map(|h| {
                    Box::pin(stream::unfold(h.subscribe(), |mut rx| async move {
                        match rx.recv().await {
                            Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => Some(((), rx)),
                            Err(broadcast::error::RecvError::Closed) => None,
                        }
                    }))
                })
                .collect();
            let tx = self.events.clone();
            *merger = Some(tokio::spawn(async move {
                let mut merged = stream::select_all(sources);
                while merged.next().await.is_some() {
                    let _ = tx.send(());
                }
            }));
        }
        rx
    }
}

impl Drop for Dispatcher {
    fn drop(&mut self) {
        if let Some(task) = self.merger.lock().unwrap_or_else(|e| e.into_inner()).take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::host::{FakeHost, HostState};
    use crate::agents::template::AgentTemplate;
    use crate::runtime::test_support::FakeLink;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct Fixtures;

    impl Fixtures {
        fn req(model: &str, n_ctx: u32, host: Option<&str>) -> ModelRequirement {
            ModelRequirement {
                runtime: "llamacpp".to_string(),
                model: model.to_string(),
                n_ctx,
                host: host.map(str::to_string),
            }
        }

        fn loaded(model: &str, n_ctx: u32) -> HostState {
            HostState {
                loaded: Some(("llamacpp".to_string(), model.to_string(), Some(n_ctx))),
                loading: false,
            }
        }

        fn dispatcher(hosts: &[&Arc<FakeHost>]) -> Dispatcher {
            let hosts: Vec<Arc<dyn ModelHost>> = hosts
                .iter()
                .map(|h| Arc::clone(h) as Arc<dyn ModelHost>)
                .collect();
            Dispatcher::new(hosts, Box::new(ExplicitLoadPolicy))
        }

        fn same_link(a: &Arc<dyn WorkerLink>, b: &Arc<FakeLink>) -> bool {
            std::ptr::eq(Arc::as_ptr(a) as *const (), Arc::as_ptr(b) as *const ())
        }
    }

    struct NoTurnLoadPolicy;

    impl PlacementPolicy for NoTurnLoadPolicy {
        fn choose_host<'a>(
            &self,
            req: &ModelRequirement,
            hosts: &'a [Arc<dyn ModelHost>],
        ) -> Option<&'a Arc<dyn ModelHost>> {
            ExplicitLoadPolicy.choose_host(req, hosts)
        }

        fn turn_may_load(&self) -> bool {
            false
        }
    }

    #[test]
    fn status_never_loads() {
        let hive = FakeHost::serving("hive", &[("llamacpp", "gemma-3")]);
        let d = Fixtures::dispatcher(&[&hive]);
        for _ in 0..5 {
            d.status(&Fixtures::req("gemma-3", 4096, None));
        }
        assert_eq!(hive.ensure_ready_calls(), 0);
    }

    #[test]
    fn needs_load_reports_eviction() {
        let hive = FakeHost::serving("hive", &[("llamacpp", "gemma-3")]);
        hive.set_state(Fixtures::loaded("gemma-4", 8192));
        let d = Fixtures::dispatcher(&[&hive]);
        assert_eq!(
            d.status(&Fixtures::req("gemma-3", 4096, None)),
            Placement::NeedsLoad {
                host: "hive".into(),
                evicts: Some("gemma-4".into())
            }
        );
    }

    #[test]
    fn needs_load_with_nothing_loaded_evicts_nothing() {
        let hive = FakeHost::serving("hive", &[("llamacpp", "gemma-3")]);
        let d = Fixtures::dispatcher(&[&hive]);
        assert_eq!(
            d.status(&Fixtures::req("gemma-3", 4096, None)),
            Placement::NeedsLoad {
                host: "hive".into(),
                evicts: None
            }
        );
    }

    #[test]
    fn n_ctx_mismatch_needs_load() {
        let hive = FakeHost::serving("hive", &[("llamacpp", "gemma-4")]);
        hive.set_state(Fixtures::loaded("gemma-4", 8192));
        let d = Fixtures::dispatcher(&[&hive]);
        assert_eq!(
            d.status(&Fixtures::req("gemma-4", 4096, None)),
            Placement::NeedsLoad {
                host: "hive".into(),
                evicts: Some("gemma-4".into())
            }
        );
    }

    #[test]
    fn ready_when_matching() {
        let hive = FakeHost::serving("hive", &[("llamacpp", "gemma-4")]);
        hive.set_state(Fixtures::loaded("gemma-4", 8192));
        let d = Fixtures::dispatcher(&[&hive]);
        let req = Fixtures::req("gemma-4", 8192, None);
        assert_eq!(
            d.status(&req),
            Placement::Ready {
                host: "hive".into()
            }
        );
        assert!(d.current_link(&req).is_some());
        assert!(d
            .current_link(&Fixtures::req("gemma-4", 4096, None))
            .is_none());
    }

    #[tokio::test]
    async fn link_for_turn_loads_and_notifies_once() {
        let link = FakeLink::new(false, Vec::new());
        let hive = FakeHost::serving_with_link("hive", &[("llamacpp", "gemma-3")], link.clone());
        let d = Fixtures::dispatcher(&[&hive]);
        let req = Fixtures::req("gemma-3", 4096, None);
        let notified = AtomicUsize::new(0);
        let got = d
            .link_for_turn(&req, || {
                notified.fetch_add(1, Ordering::SeqCst);
            })
            .await
            .unwrap();
        assert!(Fixtures::same_link(&got, &link));
        assert!(Fixtures::same_link(&got, &hive.link()));
        assert_eq!(notified.load(Ordering::SeqCst), 1);
        assert_eq!(hive.ensure_ready_calls(), 1);
        // Already loaded: no notification, no second load.
        d.link_for_turn(&req, || {
            notified.fetch_add(1, Ordering::SeqCst);
        })
        .await
        .unwrap();
        assert_eq!(notified.load(Ordering::SeqCst), 1);
        assert_eq!(hive.ensure_ready_calls(), 1);
    }

    #[tokio::test]
    async fn link_for_turn_refuses_when_policy_forbids_loading() {
        let hive = FakeHost::serving("hive", &[("llamacpp", "gemma-3")]);
        let d = Dispatcher::new(
            vec![Arc::clone(&hive) as Arc<dyn ModelHost>],
            Box::new(NoTurnLoadPolicy),
        );
        let notified = AtomicUsize::new(0);
        let err = d
            .link_for_turn(&Fixtures::req("gemma-3", 4096, None), || {
                notified.fetch_add(1, Ordering::SeqCst);
            })
            .await
            .err();
        assert_eq!(err, Some(DispatchError("model not loaded".into())));
        assert_eq!(notified.load(Ordering::SeqCst), 0);
        assert_eq!(hive.ensure_ready_calls(), 0);
    }

    #[tokio::test]
    async fn explicit_load_and_load_failure() {
        let hive = FakeHost::serving("hive", &[("llamacpp", "gemma-3")]);
        let d = Fixtures::dispatcher(&[&hive]);
        let req = Fixtures::req("gemma-3", 4096, None);
        d.load(&req).await.unwrap();
        assert_eq!(
            d.status(&req),
            Placement::Ready {
                host: "hive".into()
            }
        );
        hive.fail_ensure(Some("boom"));
        assert_eq!(
            d.load(&Fixtures::req("gemma-3", 8192, None)).await.err(),
            Some(DispatchError("boom".into()))
        );
    }

    #[test]
    fn preferred_host_wins() {
        let a = FakeHost::serving("a", &[("llamacpp", "gemma-3")]);
        let b = FakeHost::serving("b", &[("llamacpp", "gemma-3")]);
        let d = Fixtures::dispatcher(&[&a, &b]);
        assert_eq!(
            d.status(&Fixtures::req("gemma-3", 4096, Some("b"))),
            Placement::NeedsLoad {
                host: "b".into(),
                evicts: None
            }
        );
        assert_eq!(
            d.status(&Fixtures::req("gemma-3", 4096, None)),
            Placement::NeedsLoad {
                host: "a".into(),
                evicts: None
            }
        );
    }

    #[tokio::test]
    async fn unserved_model_is_unavailable() {
        let hive = FakeHost::with_link("hive", FakeLink::new(false, Vec::new()));
        let empty = FakeHost::new("empty");
        let d = Fixtures::dispatcher(&[&hive, &empty]);
        let req = Fixtures::req("x", 4096, None);
        let reason = "no host serves llamacpp/x".to_string();
        assert_eq!(
            d.status(&req),
            Placement::Unavailable {
                reason: reason.clone()
            }
        );
        assert!(d.current_link(&req).is_none());
        assert_eq!(
            d.load(&req).await.err(),
            Some(DispatchError(reason.clone()))
        );
        assert_eq!(
            d.link_for_turn(&req, || {}).await.err(),
            Some(DispatchError(reason))
        );
        assert_eq!(hive.ensure_ready_calls(), 0);
    }

    #[tokio::test]
    async fn unload_only_evicts_the_requirements_own_model() {
        let hive = FakeHost::serving("hive", &[("llamacpp", "gemma-3"), ("llamacpp", "gemma-4")]);
        hive.set_state(Fixtures::loaded("gemma-4", 8192));
        let d = Fixtures::dispatcher(&[&hive]);
        d.unload(&Fixtures::req("gemma-3", 4096, None))
            .await
            .unwrap();
        assert_eq!(hive.unload_calls(), 0);
        d.unload(&Fixtures::req("gemma-4", 4096, None))
            .await
            .unwrap();
        assert_eq!(hive.unload_calls(), 1);
    }

    #[tokio::test]
    async fn host_signal_reaches_subscribers() {
        let a = FakeHost::serving("a", &[("llamacpp", "m")]);
        let b = FakeHost::serving("b", &[("llamacpp", "m")]);
        let d = Fixtures::dispatcher(&[&a, &b]);
        let mut rx1 = d.subscribe();
        let mut rx2 = d.subscribe();
        tokio::task::yield_now().await;
        b.fire();
        for rx in [&mut rx1, &mut rx2] {
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect("signal arrives")
                .unwrap();
        }
        a.fire();
        tokio::time::timeout(Duration::from_secs(1), rx1.recv())
            .await
            .expect("signal arrives")
            .unwrap();
    }

    #[test]
    fn requirement_comes_from_the_snapshot() {
        let template = AgentTemplate {
            name: "t".into(),
            system_prompt: String::new(),
            tools: Vec::new(),
            step_budget: 3,
            runtime: "llamacpp".into(),
            model: "gemma-3".into(),
            n_ctx: 4096,
            enable_thinking: false,
            sampling: None,
            host: Some("hive".into()),
        };
        let record = SessionRecord::new("s1".into(), "T".into(), "t", template, 0);
        assert_eq!(
            Dispatcher::requirement(&record),
            Fixtures::req("gemma-3", 4096, Some("hive"))
        );
    }

    #[test]
    fn placement_serializes_with_a_state_tag() {
        let ready = serde_json::to_value(Placement::Ready { host: "h".into() }).unwrap();
        assert_eq!(ready, serde_json::json!({"state":"ready","host":"h"}));
        let needs = serde_json::to_value(Placement::NeedsLoad {
            host: "h".into(),
            evicts: Some("m".into()),
        })
        .unwrap();
        assert_eq!(
            needs,
            serde_json::json!({"state":"needs_load","host":"h","evicts":"m"})
        );
        let gone = serde_json::to_value(Placement::Unavailable { reason: "r".into() }).unwrap();
        assert_eq!(
            gone,
            serde_json::json!({"state":"unavailable","reason":"r"})
        );
    }
}
