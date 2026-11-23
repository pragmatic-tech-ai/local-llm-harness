//! Model hosts: machines that serve models over our worker protocol.
//!
//! A [`ModelHost`] answers "can you serve this runtime+model", reports what it currently has
//! loaded, hands out a [`WorkerLink`] to the loaded model, and loads/unloads on demand. The
//! dispatcher (a later task) picks a host per [`ModelRequirement`].
//!
//! [`LocalSupervisorHost`] is the first host: it wraps the in-process harness manager. It holds
//! the manager behind the small [`LocalHarness`] trait (implemented for every
//! `HarnessManager<L, P>`) rather than being generic over `L, P`: that keeps the host a plain
//! non-generic type, and lets the tests drive it with a trivial recording fake instead of
//! spawning fake processes.

use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use tokio::sync::{broadcast, Mutex};

use crate::runtime::worker::{WorkerLink, WsWorkerLink};
use crate::supervisor::catalog::Catalog;
use crate::supervisor::discovery::{discover, DiscoveredModel};
use crate::supervisor::manager::{
    HarnessInfo, HarnessManager, HarnessState, ManagerError, ReadinessProbe, WorkerLauncher,
};

/// What a session needs loaded: runtime, model and exact context size, optionally on a named host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequirement {
    pub runtime: String,
    pub model: String,
    pub n_ctx: u32,
    pub host: Option<String>,
}

/// What a host has loaded right now. `loaded` is `(runtime, model, n_ctx)` and is only set while
/// the harness is Ready; `n_ctx` is `None` when it was started without an explicit override.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostState {
    pub loaded: Option<(String, String, Option<u32>)>,
    pub loading: bool,
}

/// A host operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HostError(pub String);

/// A machine that serves models.
#[async_trait]
pub trait ModelHost: Send + Sync {
    fn name(&self) -> &str;
    /// Whether this host can ever serve `req` (cheap; no loading).
    fn serves(&self, req: &ModelRequirement) -> bool;
    fn state(&self) -> HostState;
    /// A link to the loaded model, only if it matches `req` exactly (runtime, model, n_ctx).
    fn current_link(&self, req: &ModelRequirement) -> Option<Arc<dyn WorkerLink>>;
    /// A link to a model matching `req`, loading it (and evicting what is loaded) if needed.
    async fn ensure_ready(&self, req: &ModelRequirement) -> Result<Arc<dyn WorkerLink>, HostError>;
    async fn unload(&self) -> Result<(), HostError>;
    /// Fires on any state change. Must be called inside a tokio runtime.
    fn subscribe(&self) -> broadcast::Receiver<()>;
}

/// The slice of `HarnessManager` the local host needs.
#[async_trait]
pub trait LocalHarness: Send + Sync {
    fn current(&self) -> Option<HarnessInfo>;
    fn ready_worker_url(&self) -> Option<String>;
    async fn start(
        &self,
        runtime: &str,
        model: &str,
        n_ctx: Option<u32>,
    ) -> Result<HarnessInfo, ManagerError>;
    async fn stop(&self) -> Result<(), ManagerError>;
    fn subscribe(&self) -> broadcast::Receiver<HarnessInfo>;
}

#[async_trait]
impl<L, P> LocalHarness for HarnessManager<L, P>
where
    L: WorkerLauncher + 'static,
    P: ReadinessProbe + 'static,
{
    fn current(&self) -> Option<HarnessInfo> {
        HarnessManager::current(self)
    }

    fn ready_worker_url(&self) -> Option<String> {
        HarnessManager::ready_worker_url(self)
    }

    async fn start(
        &self,
        runtime: &str,
        model: &str,
        n_ctx: Option<u32>,
    ) -> Result<HarnessInfo, ManagerError> {
        HarnessManager::start(self, runtime, model, n_ctx).await
    }

    async fn stop(&self) -> Result<(), ManagerError> {
        HarnessManager::stop(self).await
    }

    fn subscribe(&self) -> broadcast::Receiver<HarnessInfo> {
        HarnessManager::subscribe(self)
    }
}

/// The host backed by this machine's harness manager.
pub struct LocalSupervisorHost {
    name: String,
    harness: Arc<dyn LocalHarness>,
    catalog: Catalog,
    /// Discovery result, cached because discovery reads GGUF headers from disk.
    models: StdMutex<Vec<DiscoveredModel>>,
    /// Serialises `ensure_ready` so concurrent callers don't restart each other's harness.
    loading: Mutex<()>,
}

impl LocalSupervisorHost {
    const EVENT_CAPACITY: usize = 16;
    const ERR_NOT_READY: &'static str = "harness did not become ready for the requested model";

    pub fn new<H: LocalHarness + 'static>(
        name: String,
        manager: Arc<H>,
        catalog: &Catalog,
    ) -> Self {
        LocalSupervisorHost {
            name,
            harness: manager,
            models: StdMutex::new(discover(catalog)),
            catalog: catalog.clone(),
            loading: Mutex::new(()),
        }
    }

    /// Re-run model discovery (call after models are added or removed on disk).
    pub fn refresh(&self) {
        *self.models.lock().unwrap() = discover(&self.catalog);
    }

    fn matches(info: &HarnessInfo, req: &ModelRequirement) -> bool {
        info.state == HarnessState::Ready
            && info.runtime == req.runtime
            && info.model == req.model
            && info.n_ctx == Some(req.n_ctx)
    }

    fn link_if_matching(&self, req: &ModelRequirement) -> Option<Arc<dyn WorkerLink>> {
        let info = self.harness.current()?;
        if !Self::matches(&info, req) {
            return None;
        }
        let url = self.harness.ready_worker_url()?;
        Some(Arc::new(WsWorkerLink::new(url)))
    }
}

#[async_trait]
impl ModelHost for LocalSupervisorHost {
    fn name(&self) -> &str {
        &self.name
    }

    fn serves(&self, req: &ModelRequirement) -> bool {
        if req.host.as_deref().is_some_and(|h| h != self.name) {
            return false;
        }
        self.models
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.display_name == req.model && m.runtimes.contains(&req.runtime))
    }

    fn state(&self) -> HostState {
        match self.harness.current() {
            Some(info) => HostState {
                loading: info.state == HarnessState::Loading,
                loaded: (info.state == HarnessState::Ready)
                    .then(|| (info.runtime, info.model, info.n_ctx)),
            },
            None => HostState::default(),
        }
    }

    fn current_link(&self, req: &ModelRequirement) -> Option<Arc<dyn WorkerLink>> {
        self.link_if_matching(req)
    }

    async fn ensure_ready(&self, req: &ModelRequirement) -> Result<Arc<dyn WorkerLink>, HostError> {
        let _serial = self.loading.lock().await;
        if let Some(link) = self.link_if_matching(req) {
            return Ok(link);
        }
        self.harness
            .start(&req.runtime, &req.model, Some(req.n_ctx))
            .await
            .map_err(|e| HostError(e.to_string()))?;
        self.link_if_matching(req)
            .ok_or_else(|| HostError(Self::ERR_NOT_READY.to_string()))
    }

    async fn unload(&self) -> Result<(), HostError> {
        self.harness
            .stop()
            .await
            .map_err(|e| HostError(e.to_string()))
    }

    /// Forwards the manager's events as unit signals. One forwarder task per subscription; it
    /// ends when the manager closes or the receiver is dropped (noticed on the next event).
    fn subscribe(&self) -> broadcast::Receiver<()> {
        let (tx, rx) = broadcast::channel(Self::EVENT_CAPACITY);
        let mut source = self.harness.subscribe();
        tokio::spawn(async move {
            loop {
                match source.recv().await {
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {
                        if tx.send(()).is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        rx
    }
}

/// A scripted host for dispatcher tests.
///
/// API: `FakeHost::new(name)`, `FakeHost::serving(name, &[(runtime, model)])`,
/// `FakeHost::with_link(name, link)`, `.link()`, `.set_state(HostState)` (fires the signal),
/// `.fire()`, `.fail_ensure(Option<&str>)`, `.ensure_ready_calls()`, `.unload_calls()`, plus the
/// `ModelHost` trait. All constructors return `Arc<FakeHost>`.
#[cfg(test)]
pub(crate) struct FakeHost {
    name: String,
    serves: Vec<(String, String)>,
    link: Arc<crate::runtime::test_support::FakeLink>,
    state: StdMutex<HostState>,
    ensure_error: StdMutex<Option<String>>,
    ensure_calls: std::sync::atomic::AtomicUsize,
    unload_calls: std::sync::atomic::AtomicUsize,
    tx: broadcast::Sender<()>,
}

#[cfg(test)]
#[allow(dead_code)] // used by the dispatcher tests (next task)
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
        })
    }

    /// The link every call hands out (use `.requests()` to inspect traffic).
    pub(crate) fn link(&self) -> Arc<crate::runtime::test_support::FakeLink> {
        Arc::clone(&self.link)
    }

    /// Replace the reported state and fire the change signal.
    pub(crate) fn set_state(&self, state: HostState) {
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

#[cfg(test)]
#[async_trait]
impl ModelHost for FakeHost {
    fn name(&self) -> &str {
        &self.name
    }

    fn serves(&self, req: &ModelRequirement) -> bool {
        req.host.as_deref().map_or(true, |h| h == self.name)
            && self
                .serves
                .iter()
                .any(|(r, m)| *r == req.runtime && *m == req.model)
    }

    fn state(&self) -> HostState {
        self.state.lock().unwrap().clone()
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
        if !self.loaded_matches(req) {
            self.set_state(HostState {
                loaded: Some((req.runtime.clone(), req.model.clone(), Some(req.n_ctx))),
                loading: false,
            });
        }
        Ok(Arc::clone(&self.link) as Arc<dyn WorkerLink>)
    }

    async fn unload(&self) -> Result<(), HostError> {
        self.unload_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.set_state(HostState::default());
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<()> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::catalog::{RuntimeDef, RuntimeKind};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type Start = (String, String, Option<u32>);

    /// Records `start` calls and tracks one harness, like the real manager's bookkeeping.
    struct FakeHarness {
        info: StdMutex<Option<HarnessInfo>>,
        starts: StdMutex<Vec<Start>>,
        stops: AtomicUsize,
        tx: broadcast::Sender<HarnessInfo>,
    }

    impl FakeHarness {
        fn new() -> Arc<Self> {
            Arc::new(FakeHarness {
                info: StdMutex::new(None),
                starts: StdMutex::new(Vec::new()),
                stops: AtomicUsize::new(0),
                tx: broadcast::channel(16).0,
            })
        }

        fn info(
            runtime: &str,
            model: &str,
            state: HarnessState,
            n_ctx: Option<u32>,
        ) -> HarnessInfo {
            HarnessInfo {
                id: format!("{runtime}:{model}"),
                runtime: runtime.to_string(),
                model: model.to_string(),
                state,
                url: None,
                chat_url: None,
                n_ctx,
            }
        }

        fn set(&self, info: Option<HarnessInfo>) {
            *self.info.lock().unwrap() = info;
        }

        fn starts(&self) -> Vec<Start> {
            self.starts.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LocalHarness for FakeHarness {
        fn current(&self) -> Option<HarnessInfo> {
            self.info.lock().unwrap().clone()
        }

        fn ready_worker_url(&self) -> Option<String> {
            self.current()
                .filter(|i| i.state == HarnessState::Ready)
                .map(|_| "ws://127.0.0.1:8777".to_string())
        }

        async fn start(
            &self,
            runtime: &str,
            model: &str,
            n_ctx: Option<u32>,
        ) -> Result<HarnessInfo, ManagerError> {
            self.starts
                .lock()
                .unwrap()
                .push((runtime.to_string(), model.to_string(), n_ctx));
            let info = Self::info(runtime, model, HarnessState::Ready, n_ctx);
            self.set(Some(info.clone()));
            let _ = self.tx.send(info.clone());
            Ok(info)
        }

        async fn stop(&self) -> Result<(), ManagerError> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.set(None);
            Ok(())
        }

        fn subscribe(&self) -> broadcast::Receiver<HarnessInfo> {
            self.tx.subscribe()
        }
    }

    struct Fixtures;

    impl Fixtures {
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

        fn catalog(tag: &str) -> Catalog {
            let dir = std::env::temp_dir().join(format!("host-{}-{}", tag, std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("gemma.gguf"), Self::gguf("gemma3")).unwrap();
            Catalog {
                runtimes: vec![RuntimeDef {
                    name: "llamacpp".to_string(),
                    kind: RuntimeKind::Llamacpp,
                    exe: PathBuf::from("llamacpp.exe"),
                    requires_tokenizer: false,
                    n_ctx: None,
                    n_gpu_layers: None,
                    internal_port: None,
                    server_exe: None,
                }],
                model_dirs: vec![dir],
                advertise_host: "127.0.0.1".to_string(),
                worker_port: 8777,
                tools: None,
                agents: Default::default(),
                hosts: Vec::new(),
            }
        }

        fn req(model: &str, n_ctx: u32) -> ModelRequirement {
            ModelRequirement {
                runtime: "llamacpp".to_string(),
                model: model.to_string(),
                n_ctx,
                host: None,
            }
        }

        fn host(tag: &str) -> (LocalSupervisorHost, Arc<FakeHarness>) {
            let harness = FakeHarness::new();
            let host = LocalSupervisorHost::new(
                "hive".to_string(),
                Arc::clone(&harness),
                &Self::catalog(tag),
            );
            (host, harness)
        }
    }

    #[tokio::test]
    async fn current_link_requires_exact_match() {
        let (host, harness) = Fixtures::host("link");
        harness.set(Some(FakeHarness::info(
            "llamacpp",
            "gemma",
            HarnessState::Ready,
            Some(8192),
        )));
        assert!(host.current_link(&Fixtures::req("gemma", 8192)).is_some());
        assert!(host.current_link(&Fixtures::req("gemma", 4096)).is_none());
        assert!(host.current_link(&Fixtures::req("other", 8192)).is_none());
        let mut other_runtime = Fixtures::req("gemma", 8192);
        other_runtime.runtime = "candle".to_string();
        assert!(host.current_link(&other_runtime).is_none());
    }

    #[tokio::test]
    async fn harness_without_n_ctx_never_matches_and_loading_is_not_a_link() {
        let (host, harness) = Fixtures::host("noctx");
        harness.set(Some(FakeHarness::info(
            "llamacpp",
            "gemma",
            HarnessState::Ready,
            None,
        )));
        assert!(host.current_link(&Fixtures::req("gemma", 8192)).is_none());
        harness.set(Some(FakeHarness::info(
            "llamacpp",
            "gemma",
            HarnessState::Loading,
            Some(8192),
        )));
        assert!(host.current_link(&Fixtures::req("gemma", 8192)).is_none());
        assert!(host.state().loading);
        assert_eq!(host.state().loaded, None);
    }

    #[tokio::test]
    async fn ensure_ready_starts_when_mismatched() {
        let (host, harness) = Fixtures::host("start");
        harness.set(Some(FakeHarness::info(
            "llamacpp",
            "gemma",
            HarnessState::Ready,
            Some(4096),
        )));
        host.ensure_ready(&Fixtures::req("gemma", 8192))
            .await
            .unwrap();
        assert_eq!(
            harness.starts(),
            vec![("llamacpp".to_string(), "gemma".to_string(), Some(8192))]
        );
        assert_eq!(
            host.state().loaded,
            Some(("llamacpp".into(), "gemma".into(), Some(8192)))
        );
    }

    #[tokio::test]
    async fn ensure_ready_reuses_matching_harness() {
        let (host, harness) = Fixtures::host("reuse");
        host.ensure_ready(&Fixtures::req("gemma", 8192))
            .await
            .unwrap();
        host.ensure_ready(&Fixtures::req("gemma", 8192))
            .await
            .unwrap();
        assert_eq!(harness.starts().len(), 1);
    }

    #[tokio::test]
    async fn serves_uses_discovery_and_refresh_picks_up_new_models() {
        let (host, _harness) = Fixtures::host("serves");
        assert!(host.serves(&Fixtures::req("gemma", 8192)));
        assert!(!host.serves(&Fixtures::req("missing", 8192)));
        let mut pinned = Fixtures::req("gemma", 8192);
        pinned.host = Some("elsewhere".to_string());
        assert!(!host.serves(&pinned));
        pinned.host = Some("hive".to_string());
        assert!(host.serves(&pinned));
        let dir = host.catalog.model_dirs[0].clone();
        std::fs::write(dir.join("late.gguf"), Fixtures::gguf("llama")).unwrap();
        assert!(!host.serves(&Fixtures::req("late", 8192)));
        host.refresh();
        assert!(host.serves(&Fixtures::req("late", 8192)));
    }

    #[tokio::test]
    async fn unload_stops_and_subscribe_forwards_changes() {
        let (host, harness) = Fixtures::host("sub");
        let mut rx = host.subscribe();
        host.ensure_ready(&Fixtures::req("gemma", 8192))
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        host.unload().await.unwrap();
        assert_eq!(harness.stops.load(Ordering::SeqCst), 1);
        assert_eq!(host.state(), HostState::default());
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
        fake.unload().await.unwrap();
        assert_eq!(fake.unload_calls(), 1);
    }
}
