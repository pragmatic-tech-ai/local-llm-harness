//! The dispatcher: routes a session's model requirement to the provider its repository names,
//! and decides when the model loads.
//!
//! Routing is exact: the requirement's `repository` picks one provider from the
//! [`ProviderSource`], and there is no fallback to another. A session opens read-only (`status`
//! never loads anything) and the model loads only on an explicit [`Dispatcher::load`] or on the
//! first message ([`Dispatcher::link_for_turn`]).

use std::sync::{Arc, Mutex};

use futures::stream::{self, StreamExt};
use serde::Serialize;
use tokio::sync::broadcast;

use crate::agents::host::{HarnessIdentity, ModelRequirement};
use crate::agents::session::SessionRecord;
use crate::providers::{Provider, ProviderCapabilities, ProviderSource};
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

pub struct Dispatcher {
    source: Arc<dyn ProviderSource>,
    events: broadcast::Sender<()>,
    /// The single task merging every provider's signal into `events`, started by the first
    /// `subscribe` and aborted when the dispatcher is dropped.
    merger: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Dispatcher {
    const EVENT_CAPACITY: usize = 16;
    const ERR_REPOSITORY_NOT_FOUND: &'static str = "repository not found: ";
    const ERR_MODEL_NOT_IN_REPOSITORY: &'static str = "model not in repository ";

    pub fn new(source: Arc<dyn ProviderSource>) -> Self {
        Dispatcher {
            source,
            events: broadcast::channel(Self::EVENT_CAPACITY).0,
            merger: Mutex::new(None),
        }
    }

    /// The requirement a session's template snapshot asks for.
    pub fn requirement(record: &SessionRecord) -> ModelRequirement {
        ModelRequirement::of(&record.snapshot)
    }

    /// The provider `req`'s repository names, whether or not it serves the model.
    fn named_provider(&self, req: &ModelRequirement) -> Result<Arc<dyn Provider>, DispatchError> {
        self.source.provider(&req.repository).ok_or_else(|| {
            DispatchError(format!(
                "{}{}",
                Self::ERR_REPOSITORY_NOT_FOUND,
                req.repository
            ))
        })
    }

    /// The provider that serves `req`: exactly the one its repository names, unless that
    /// provider is unavailable or does not serve the model.
    fn provider_for(&self, req: &ModelRequirement) -> Result<Arc<dyn Provider>, DispatchError> {
        let provider = self.named_provider(req)?;
        if let Some(reason) = provider.unavailable() {
            return Err(DispatchError(reason));
        }
        if provider.serves(req) {
            Ok(provider)
        } else {
            Err(DispatchError(format!(
                "{}{}: {}",
                Self::ERR_MODEL_NOT_IN_REPOSITORY,
                req.repository,
                req.model
            )))
        }
    }

    /// What the provider behind `req`'s repository supports; `None` when there is no such
    /// repository.
    pub fn capabilities(&self, req: &ModelRequirement) -> Option<ProviderCapabilities> {
        self.named_provider(req).ok().map(|p| p.capabilities())
    }

    /// Where `req` stands. Never loads anything. While the provider is mid-load (even of `req`
    /// itself) the answer is `NeedsLoad` with `evicts: None`, because nothing is loaded yet; the
    /// three states are kept and callers learn about progress through `subscribe`.
    pub fn status(&self, req: &ModelRequirement) -> Placement {
        let provider = match self.provider_for(req) {
            Ok(provider) => provider,
            Err(e) => return Placement::Unavailable { reason: e.0 },
        };
        let host_name = provider.name().to_string();
        if provider.current_link(req).is_some() {
            Placement::Ready { host: host_name }
        } else {
            Placement::NeedsLoad {
                host: host_name,
                evicts: provider.state().loaded.map(|(_, model, _)| model),
            }
        }
    }

    /// A link to the model if it is loaded right now; no side effects. Used for loop steps after
    /// the first, so an eviction mid-turn surfaces as an error.
    pub fn current_link(&self, req: &ModelRequirement) -> Option<Arc<dyn WorkerLink>> {
        self.provider_for(req).ok()?.current_link(req)
    }

    /// The harness the provider chosen for `req` serves right now (whatever model it is), if any.
    /// A turn records it with its first link and compares it before every later step.
    pub fn identity(&self, req: &ModelRequirement) -> Option<HarnessIdentity> {
        self.provider_for(req).ok()?.identity()
    }

    /// Re-reads which models every provider serves, then signals subscribers so placements that
    /// changed with it (a model dropped on disk or removed) are announced.
    pub fn refresh(&self) {
        for provider in self.source.providers() {
            provider.refresh();
        }
        let _ = self.events.send(());
    }

    /// Load the model (evicting what is loaded on the chosen provider) and return its link.
    pub async fn load(&self, req: &ModelRequirement) -> Result<Arc<dyn WorkerLink>, DispatchError> {
        let provider = self.provider_for(req)?;
        provider
            .ensure_ready(req)
            .await
            .map_err(|e| DispatchError(e.0))
    }

    /// The link for a turn's first step. If the model is not loaded, calls `notify_loading` once
    /// and loads it. No lock is held across the load, so `status` calls from other connections
    /// are never blocked by it.
    pub async fn link_for_turn(
        &self,
        req: &ModelRequirement,
        notify_loading: impl FnOnce(),
    ) -> Result<Arc<dyn WorkerLink>, DispatchError> {
        let provider = self.provider_for(req)?;
        if let Some(link) = provider.current_link(req) {
            return Ok(link);
        }
        notify_loading();
        provider
            .ensure_ready(req)
            .await
            .map_err(|e| DispatchError(e.0))
    }

    /// Unload `req`'s model from its provider. Does nothing if that provider has some other model
    /// (runtime or model name differs) loaded, so one session cannot evict another's model; the
    /// check and the stop are atomic on the provider (`Provider::unload_if`).
    pub async fn unload(&self, req: &ModelRequirement) -> Result<(), DispatchError> {
        let provider = self.provider_for(req)?;
        provider
            .unload_if(req)
            .await
            .map(|_| ())
            .map_err(|e| DispatchError(e.0))
    }

    /// Fires whenever any provider's state changes. Must be called inside a tokio runtime.
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        // Take the receiver first so the caller cannot miss an event sent once the merger runs.
        let rx = self.events.subscribe();
        let mut merger = self.merger.lock().unwrap_or_else(|e| e.into_inner());
        if merger.is_none() {
            let sources: Vec<_> = self
                .source
                .providers()
                .iter()
                .map(|p| {
                    Box::pin(stream::unfold(p.subscribe(), |mut rx| async move {
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
    use crate::agents::template::{
        AgentTemplate, ContextStrategy, WorkspaceConfig, DEFAULT_COMPACT_AT_PCT,
    };
    use crate::providers::{Measure, Provider, ProviderCapabilities, StaticProviders};
    use crate::runtime::test_support::FakeLink;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct Fixtures;

    impl Fixtures {
        fn req(model: &str, n_ctx: u32, repository: &str) -> ModelRequirement {
            ModelRequirement {
                runtime: "llamacpp".to_string(),
                model: model.to_string(),
                n_ctx,
                repository: repository.to_string(),
            }
        }

        fn loaded(model: &str, n_ctx: u32) -> HostState {
            HostState {
                loaded: Some(("llamacpp".to_string(), model.to_string(), Some(n_ctx))),
                loading: false,
            }
        }

        fn dispatcher(hosts: &[&Arc<FakeHost>]) -> Dispatcher {
            Dispatcher::new(Self::source(hosts))
        }

        fn source(hosts: &[&Arc<FakeHost>]) -> Arc<StaticProviders> {
            Arc::new(StaticProviders(
                hosts
                    .iter()
                    .map(|h| (h.name().to_string(), Arc::clone(h) as Arc<dyn Provider>))
                    .collect(),
            ))
        }

        fn same_link(a: &Arc<dyn WorkerLink>, b: &Arc<FakeLink>) -> bool {
            std::ptr::eq(Arc::as_ptr(a) as *const (), Arc::as_ptr(b) as *const ())
        }
    }

    #[test]
    fn status_never_loads() {
        let hive = FakeHost::serving("hive", &[("llamacpp", "gemma-3")]);
        let d = Fixtures::dispatcher(&[&hive]);
        for _ in 0..5 {
            d.status(&Fixtures::req("gemma-3", 4096, "hive"));
        }
        assert_eq!(hive.ensure_ready_calls(), 0);
    }

    #[tokio::test]
    async fn identity_follows_reloads_and_refresh_signals() {
        let hive = FakeHost::serving("hive", &[("llamacpp", "gemma-4")]);
        let d = Fixtures::dispatcher(&[&hive]);
        let req = Fixtures::req("gemma-4", 8192, "hive");
        assert_eq!(d.identity(&req), None);
        hive.set_state(Fixtures::loaded("gemma-4", 8192));
        let first = d.identity(&req).expect("loaded");
        hive.set_state(Fixtures::loaded("gemma-4", 8192));
        assert_ne!(d.identity(&req), Some(first));
        let mut rx = d.subscribe();
        d.refresh();
        tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("refresh signals")
            .unwrap();
    }

    #[test]
    fn needs_load_reports_eviction() {
        let hive = FakeHost::serving("hive", &[("llamacpp", "gemma-3")]);
        hive.set_state(Fixtures::loaded("gemma-4", 8192));
        let d = Fixtures::dispatcher(&[&hive]);
        assert_eq!(
            d.status(&Fixtures::req("gemma-3", 4096, "hive")),
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
            d.status(&Fixtures::req("gemma-3", 4096, "hive")),
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
            d.status(&Fixtures::req("gemma-4", 4096, "hive")),
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
        let req = Fixtures::req("gemma-4", 8192, "hive");
        assert_eq!(
            d.status(&req),
            Placement::Ready {
                host: "hive".into()
            }
        );
        assert!(d.current_link(&req).is_some());
        assert!(d
            .current_link(&Fixtures::req("gemma-4", 4096, "hive"))
            .is_none());
    }

    #[tokio::test]
    async fn link_for_turn_loads_and_notifies_once() {
        let link = FakeLink::new(false, Vec::new());
        let hive = FakeHost::serving_with_link("hive", &[("llamacpp", "gemma-3")], link.clone());
        let d = Fixtures::dispatcher(&[&hive]);
        let req = Fixtures::req("gemma-3", 4096, "hive");
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
    async fn explicit_load_and_load_failure() {
        let hive = FakeHost::serving("hive", &[("llamacpp", "gemma-3")]);
        let d = Fixtures::dispatcher(&[&hive]);
        let req = Fixtures::req("gemma-3", 4096, "hive");
        d.load(&req).await.unwrap();
        assert_eq!(
            d.status(&req),
            Placement::Ready {
                host: "hive".into()
            }
        );
        hive.fail_ensure(Some("boom"));
        assert_eq!(
            d.load(&Fixtures::req("gemma-3", 8192, "hive")).await.err(),
            Some(DispatchError("boom".into()))
        );
    }

    #[tokio::test]
    async fn routes_only_to_named_repository() {
        let a = FakeHost::serving("a", &[("llamacpp", "gemma-3")]);
        let b = FakeHost::serving("b", &[("llamacpp", "gemma-3")]);
        let d = Fixtures::dispatcher(&[&a, &b]);
        let req = Fixtures::req("gemma-3", 4096, "b");
        assert_eq!(
            d.status(&req),
            Placement::NeedsLoad {
                host: "b".into(),
                evicts: None
            }
        );
        d.load(&req).await.unwrap();
        d.link_for_turn(&req, || {}).await.unwrap();
        assert_eq!(b.ensure_ready_calls(), 1);
        assert_eq!(a.ensure_ready_calls(), 0);
        assert_eq!(a.state(), HostState::default());
    }

    #[test]
    fn capabilities_come_from_the_provider() {
        let a = FakeHost::serving("a", &[("llamacpp", "m")]);
        let b = FakeHost::serving("b", &[("llamacpp", "m")]);
        let limited = ProviderCapabilities {
            measure_prompt: Measure::Estimated,
            tokenize: false,
            raw_prompt: false,
            logprobs: false,
            load_unload: false,
        };
        b.set_capabilities(limited);
        let d = Fixtures::dispatcher(&[&a, &b]);
        assert_eq!(
            d.capabilities(&Fixtures::req("m", 1, "a")),
            Some(ProviderCapabilities::FULL)
        );
        assert_eq!(d.capabilities(&Fixtures::req("m", 1, "b")), Some(limited));
        assert_eq!(d.capabilities(&Fixtures::req("m", 1, "c")), None);
    }

    #[tokio::test]
    async fn missing_repository_is_unavailable_with_reason() {
        let hive = FakeHost::serving("hive", &[("llamacpp", "gemma-3")]);
        let d = Fixtures::dispatcher(&[&hive]);
        let req = Fixtures::req("gemma-3", 4096, "nowhere");
        let reason = "repository not found: nowhere".to_string();
        assert_eq!(
            d.status(&req),
            Placement::Unavailable {
                reason: reason.clone()
            }
        );
        assert_eq!(
            d.link_for_turn(&req, || {}).await.err(),
            Some(DispatchError(reason))
        );
        assert!(d.capabilities(&req).is_none());
        assert_eq!(hive.ensure_ready_calls(), 0);
    }

    #[tokio::test]
    async fn unserved_model_is_unavailable_with_reason() {
        let hive = FakeHost::with_link("hive", FakeLink::new(false, Vec::new()));
        let empty = FakeHost::new("empty");
        let d = Fixtures::dispatcher(&[&hive, &empty]);
        let req = Fixtures::req("x", 4096, "hive");
        let reason = "model not in repository hive: x".to_string();
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
        d.unload(&Fixtures::req("gemma-3", 4096, "hive"))
            .await
            .unwrap();
        assert_eq!(hive.unload_calls(), 0);
        d.unload(&Fixtures::req("gemma-4", 4096, "hive"))
            .await
            .unwrap();
        assert_eq!(hive.unload_calls(), 1);
        // Nothing left to unload: a repeat is a no-op.
        d.unload(&Fixtures::req("gemma-4", 4096, "hive"))
            .await
            .unwrap();
        assert_eq!(hive.unload_calls(), 1);
    }

    #[test]
    fn status_while_loading_reports_needs_load_without_eviction() {
        let hive = FakeHost::serving("hive", &[("llamacpp", "gemma-3")]);
        hive.set_state(HostState {
            loaded: None,
            loading: true,
        });
        let d = Fixtures::dispatcher(&[&hive]);
        assert_eq!(
            d.status(&Fixtures::req("gemma-3", 4096, "hive")),
            Placement::NeedsLoad {
                host: "hive".into(),
                evicts: None
            }
        );
    }

    #[tokio::test]
    async fn concurrent_turns_for_different_models_serialise_and_evict() {
        let hive = FakeHost::serving("hive", &[("llamacpp", "gemma-3"), ("llamacpp", "gemma-4")]);
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        hive.set_gate(Arc::clone(&gate));
        let d = Arc::new(Fixtures::dispatcher(&[&hive]));
        let mut turns = Vec::new();
        for model in ["gemma-3", "gemma-4"] {
            let d = Arc::clone(&d);
            turns.push(tokio::spawn(async move {
                d.link_for_turn(&Fixtures::req(model, 4096, "hive"), || {})
                    .await
                    .map(|_| ())
            }));
        }
        // One load holds the lock at the gate; the other is queued behind it.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(hive.load_started(), 1);
        gate.add_permits(2);
        for turn in turns {
            tokio::time::timeout(Duration::from_secs(5), turn)
                .await
                .expect("a turn never finished")
                .unwrap()
                .unwrap();
        }
        let loads = hive.loads();
        assert_eq!(loads.len(), 2);
        assert_eq!(loads[0].1, None);
        assert_eq!(loads[1].1, Some(loads[0].0.clone()));
        assert_ne!(loads[0].0, loads[1].0);
        assert!(loads.iter().all(|l| l.2), "each load matched its own req");
    }

    #[tokio::test]
    async fn subscribe_with_no_hosts_returns_a_receiver() {
        let d = Dispatcher::new(Arc::new(StaticProviders(Vec::new())));
        let mut rx = d.subscribe();
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn host_signal_reaches_subscribers() {
        let a = FakeHost::serving("a", &[("llamacpp", "m")]);
        let b = FakeHost::serving("b", &[("llamacpp", "m")]);
        let d = Fixtures::dispatcher(&[&a, &b]);
        let mut rx1 = d.subscribe();
        let mut rx2 = d.subscribe();
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
            repository: None,
            context_strategy: ContextStrategy::default(),
            compact_at_pct: DEFAULT_COMPACT_AT_PCT,
            workspace: WorkspaceConfig::default(),
        };
        let record = SessionRecord::new("s1".into(), "T".into(), "t", template, 0);
        assert_eq!(
            Dispatcher::requirement(&record),
            Fixtures::req("gemma-3", 4096, "hive")
        );
        let mut elsewhere = record.snapshot.clone();
        elsewhere.repository = Some("lab".into());
        assert_eq!(ModelRequirement::of(&elsewhere).repository, "lab");
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
