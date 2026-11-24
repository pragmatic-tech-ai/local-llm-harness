//! Test support for providers: a scripted [`FakeHost`] provider.

use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use tokio::sync::{broadcast, Mutex};

use crate::agents::host::ModelRequirement;
use crate::providers::{HarnessIdentity, HostError, HostState, Provider, ProviderCapabilities};
use crate::runtime::worker::WorkerLink;

/// A scripted provider for dispatcher tests.
///
/// API: `FakeHost::new(name)`, `FakeHost::serving(name, &[(runtime, model)])`,
/// `FakeHost::with_link(name, link)`, `.link()`, `.set_state(HostState)` (fires the signal;
/// a loaded state is a new harness instance),
/// `.fire()`, `.fail_ensure(Option<&str>)`, `.ensure_ready_calls()`, `.unload_calls()`, plus the
/// `Provider` trait (capabilities are `FULL` unless `.set_capabilities` says otherwise; `serves`
/// ignores the repository, which the dispatcher routes on). All constructors return `Arc<FakeHost>`.
pub(crate) struct FakeHost {
    name: String,
    serves: Vec<(String, String)>,
    link: Arc<crate::runtime::test_support::FakeLink>,
    state: StdMutex<HostState>,
    ensure_error: StdMutex<Option<String>>,
    ensure_calls: std::sync::atomic::AtomicUsize,
    unload_calls: std::sync::atomic::AtomicUsize,
    tx: broadcast::Sender<()>,
    /// Serialises loads and unloads like the real host's loading lock.
    loading: Mutex<()>,
    /// When set, each `ensure_ready` waits for a permit (while holding the loading lock).
    gate: StdMutex<Option<Arc<tokio::sync::Semaphore>>>,
    /// (model, model evicted, requirement matched the host state when the load returned).
    loads: StdMutex<Vec<(String, Option<String>, bool)>>,
    started: std::sync::atomic::AtomicUsize,
    /// The (runtime, model) a scripted in-progress load is for (`HostState` has no model for it).
    loading_model: StdMutex<Option<(String, String)>>,
    /// The harness instance of the loaded state; bumped by every `set_state` that loads.
    instance: std::sync::atomic::AtomicU64,
    capabilities: StdMutex<ProviderCapabilities>,
}

impl FakeHost {
    /// A host that serves nothing, with an empty (tool-less) `FakeLink`.
    pub(crate) fn new(name: &str) -> Arc<Self> {
        Self::serving(name, &[])
    }

    /// A host serving each `(runtime, model)`, with an empty (tool-less) `FakeLink`.
    pub(crate) fn serving(name: &str, serves: &[(&str, &str)]) -> Arc<Self> {
        Self::build(
            name,
            serves,
            crate::runtime::test_support::FakeLink::new(false, Vec::new()),
        )
    }

    /// A host serving nothing that hands out `link` from `current_link` / `ensure_ready`.
    pub(crate) fn with_link(
        name: &str,
        link: Arc<crate::runtime::test_support::FakeLink>,
    ) -> Arc<Self> {
        Self::build(name, &[], link)
    }

    /// A host serving each `(runtime, model)` that hands out `link`.
    pub(crate) fn serving_with_link(
        name: &str,
        serves: &[(&str, &str)],
        link: Arc<crate::runtime::test_support::FakeLink>,
    ) -> Arc<Self> {
        Self::build(name, serves, link)
    }

    fn build(
        name: &str,
        serves: &[(&str, &str)],
        link: Arc<crate::runtime::test_support::FakeLink>,
    ) -> Arc<Self> {
        Arc::new(FakeHost {
            name: name.to_string(),
            serves: serves
                .iter()
                .map(|(r, m)| (r.to_string(), m.to_string()))
                .collect(),
            link,
            state: StdMutex::new(HostState::default()),
            ensure_error: StdMutex::new(None),
            ensure_calls: std::sync::atomic::AtomicUsize::new(0),
            unload_calls: std::sync::atomic::AtomicUsize::new(0),
            tx: broadcast::channel(16).0,
            loading: Mutex::new(()),
            gate: StdMutex::new(None),
            loads: StdMutex::new(Vec::new()),
            started: std::sync::atomic::AtomicUsize::new(0),
            loading_model: StdMutex::new(None),
            instance: std::sync::atomic::AtomicU64::new(0),
            capabilities: StdMutex::new(ProviderCapabilities::FULL),
        })
    }

    /// Report `capabilities` from now on.
    pub(crate) fn set_capabilities(&self, capabilities: ProviderCapabilities) {
        *self.capabilities.lock().unwrap() = capabilities;
    }

    /// Make every later `ensure_ready` wait for a permit on `gate` once it holds the lock.
    pub(crate) fn set_gate(&self, gate: Arc<tokio::sync::Semaphore>) {
        *self.gate.lock().unwrap() = Some(gate);
    }

    /// `(model, evicted model, matched at return)` for each load that reached the end.
    pub(crate) fn loads(&self) -> Vec<(String, Option<String>, bool)> {
        self.loads.lock().unwrap().clone()
    }

    /// Script a load in progress of `runtime`/`model` (state `loading`, nothing loaded).
    pub(crate) fn set_loading(&self, runtime: &str, model: &str) {
        *self.loading_model.lock().unwrap() = Some((runtime.to_string(), model.to_string()));
        self.set_state(HostState {
            loaded: None,
            loading: true,
        });
    }

    /// How many loads have taken the loading lock (before any gate).
    pub(crate) fn load_started(&self) -> usize {
        self.started.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The link every call hands out (use `.requests()` to inspect traffic).
    pub(crate) fn link(&self) -> Arc<crate::runtime::test_support::FakeLink> {
        Arc::clone(&self.link)
    }

    /// Replace the reported state and fire the change signal. A loaded state is a fresh
    /// harness instance, even when it names the model already loaded (a reload).
    pub(crate) fn set_state(&self, state: HostState) {
        if state.loaded.is_some() {
            self.instance
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        *self.state.lock().unwrap() = state;
        self.fire();
    }

    /// Fire the change signal without changing state.
    pub(crate) fn fire(&self) {
        let _ = self.tx.send(());
    }

    /// Make `ensure_ready` fail with `message` from now on (`None` clears).
    pub(crate) fn fail_ensure(&self, message: Option<&str>) {
        *self.ensure_error.lock().unwrap() = message.map(str::to_string);
    }

    pub(crate) fn ensure_ready_calls(&self) -> usize {
        self.ensure_calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn unload_calls(&self) -> usize {
        self.unload_calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn loaded_matches(&self, req: &ModelRequirement) -> bool {
        self.state.lock().unwrap().loaded
            == Some((req.runtime.clone(), req.model.clone(), Some(req.n_ctx)))
    }
}

#[async_trait]
impl Provider for FakeHost {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> ProviderCapabilities {
        *self.capabilities.lock().unwrap()
    }

    fn serves(&self, req: &ModelRequirement) -> bool {
        self.serves
            .iter()
            .any(|(r, m)| *r == req.runtime && *m == req.model)
    }

    fn state(&self) -> HostState {
        self.state.lock().unwrap().clone()
    }

    fn identity(&self) -> Option<HarnessIdentity> {
        let (runtime, model, n_ctx) = self.state.lock().unwrap().loaded.clone()?;
        Some(HarnessIdentity {
            runtime,
            model,
            n_ctx,
            instance: self.instance.load(std::sync::atomic::Ordering::SeqCst),
        })
    }

    fn current_link(&self, req: &ModelRequirement) -> Option<Arc<dyn WorkerLink>> {
        self.loaded_matches(req)
            .then(|| Arc::clone(&self.link) as Arc<dyn WorkerLink>)
    }

    /// Counts the call; fails if scripted to; otherwise marks `req` loaded (firing the signal)
    /// unless it already is, and returns the link.
    async fn ensure_ready(&self, req: &ModelRequirement) -> Result<Arc<dyn WorkerLink>, HostError> {
        self.ensure_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(message) = self.ensure_error.lock().unwrap().clone() {
            return Err(HostError(message));
        }
        let _serial = self.loading.lock().await;
        self.started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let evicted = if self.loaded_matches(req) {
            None
        } else {
            let previous = self.state.lock().unwrap().loaded.clone().map(|l| l.1);
            self.set_state(HostState {
                loaded: Some((req.runtime.clone(), req.model.clone(), Some(req.n_ctx))),
                loading: false,
            });
            previous
        };
        let gate = self.gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            tokio::time::timeout(std::time::Duration::from_secs(5), gate.acquire())
                .await
                .expect("the test never released the gate")
                .unwrap()
                .forget();
        }
        let matched = self.loaded_matches(req);
        self.loads
            .lock()
            .unwrap()
            .push((req.model.clone(), evicted, matched));
        Ok(Arc::clone(&self.link) as Arc<dyn WorkerLink>)
    }

    async fn unload_if(&self, req: &ModelRequirement) -> Result<bool, HostError> {
        let _serial = self.loading.lock().await;
        let holds_it = self
            .state
            .lock()
            .unwrap()
            .loaded
            .as_ref()
            .is_some_and(|l| l.0 == req.runtime && l.1 == req.model)
            || self
                .loading_model
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|l| l.0 == req.runtime && l.1 == req.model);
        if !holds_it {
            return Ok(false);
        }
        *self.loading_model.lock().unwrap() = None;
        self.unload_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.set_state(HostState::default());
        Ok(true)
    }

    fn subscribe(&self) -> broadcast::Receiver<()> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixtures;

    impl Fixtures {
        fn req(model: &str, n_ctx: u32) -> ModelRequirement {
            ModelRequirement {
                runtime: "llamacpp".to_string(),
                model: model.to_string(),
                n_ctx,
                repository: ModelRequirement::DEFAULT_REPOSITORY.to_string(),
            }
        }
    }

    #[tokio::test]
    async fn fake_host_unload_if_also_stops_a_loading_model() {
        let fake = FakeHost::serving("h", &[("llamacpp", "gemma")]);
        fake.set_loading("llamacpp", "gemma");
        assert!(!fake.unload_if(&Fixtures::req("other", 1)).await.unwrap());
        assert!(fake.unload_if(&Fixtures::req("gemma", 1)).await.unwrap());
        assert_eq!(fake.unload_calls(), 1);
        assert_eq!(fake.state(), HostState::default());
    }

    #[tokio::test]
    async fn fake_host_scripts_serves_state_and_counts() {
        let fake = FakeHost::serving("h", &[("llamacpp", "gemma")]);
        assert!(fake.serves(&Fixtures::req("gemma", 1)));
        assert!(!fake.serves(&Fixtures::req("nope", 1)));
        assert!(fake.current_link(&Fixtures::req("gemma", 1)).is_none());
        let mut rx = fake.subscribe();
        fake.ensure_ready(&Fixtures::req("gemma", 1)).await.unwrap();
        assert_eq!(fake.ensure_ready_calls(), 1);
        assert!(rx.try_recv().is_ok());
        assert!(fake.current_link(&Fixtures::req("gemma", 1)).is_some());
        fake.fail_ensure(Some("boom"));
        assert_eq!(
            fake.ensure_ready(&Fixtures::req("gemma", 1)).await.err(),
            Some(HostError("boom".into()))
        );
        assert!(fake.unload_if(&Fixtures::req("gemma", 1)).await.unwrap());
        assert_eq!(fake.unload_calls(), 1);
    }
}
