//! Harness process manager: owns at most one live worker harness, drives its
//! Stopped/Loading/Ready/Stopping/Failed state machine, and broadcasts every change.

use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use super::catalog::{Catalog, RuntimeDef, RuntimeKind};
use super::discovery::{resolve_model, runtime_can_load, ResolvedModel};

/// Lifecycle state of the (single) harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HarnessState {
    Stopped,
    Loading,
    Ready,
    Stopping,
    Failed,
}

/// Client-facing description of a harness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HarnessInfo {
    pub id: String,
    pub runtime: String,
    pub model: String,
    pub state: HarnessState,
    pub url: Option<String>,
    /// The agent runtime's websocket, published alongside `url` while Ready.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_url: Option<String>,
    /// The explicit context override this harness was started with; `None` when started
    /// without one (the engine then picked its own default, which is not recorded).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n_ctx: Option<u32>,
    /// Which start of a harness this is: a fresh number for every harness that became Ready
    /// (`0` before that). Runtime-internal, so a reload of the same model is told apart; not on
    /// the wire.
    #[serde(skip)]
    pub instance: u64,
}

/// Why a manager operation failed.
#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("runtime '{runtime}' cannot load model '{model}'")]
    Incompatible { runtime: String, model: String },
    #[error("unknown runtime '{0}'")]
    UnknownRuntime(String),
    #[error("unknown model '{0}'")]
    UnknownModel(String),
    #[error("runtime '{runtime}' is misconfigured: missing {missing}")]
    MissingConfig {
        runtime: String,
        missing: &'static str,
    },
    #[error("failed to spawn worker: {0}")]
    Spawn(io::Error),
    #[error("worker did not become ready in time")]
    LoadTimeout,
    #[error("worker exited before becoming ready (code {0:?})")]
    WorkerExited(Option<i32>),
}

/// How a child process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildExit {
    pub code: Option<i32>,
}

/// One process to launch: executable, arguments, and extra environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildSpec {
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Every process needed to host one harness (one for candle/llamacpp, two for http).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnPlan {
    pub children: Vec<ChildSpec>,
}

impl SpawnPlan {
    const DEFAULT_NGL_WORKER: i32 = -1;
    const DEFAULT_NGL_SERVER: i32 = 28;
    const BIND_ALL: &'static str = "0.0.0.0";
    const LOOPBACK: &'static str = "127.0.0.1";
    const ENV_ENGINE: &'static str = "HARNESS_ENGINE";
    const ENV_MODEL: &'static str = "HARNESS_MODEL";
    const ENV_TOKENIZER: &'static str = "HARNESS_TOKENIZER";
    const ENV_N_CTX: &'static str = "HARNESS_N_CTX";
    const ENV_NGL: &'static str = "HARNESS_NGL";
    const ENV_SUPERVISOR: &'static str = "HARNESS_SUPERVISOR";
    const SUPERVISOR_OFF: &'static str = "false";
    const ENV_BIND: &'static str = "HARNESS_BIND";
    const ENV_SERVER_URL: &'static str = "HARNESS_SERVER_URL";
    const ENGINE_CANDLE: &'static str = "candle";
    const ENGINE_LLAMACPP: &'static str = "llamacpp";
    const ENGINE_HTTP: &'static str = "http";
    const MISSING_TOKENIZER: &'static str = "tokenizer";
    const MISSING_SERVER_EXE: &'static str = "server_exe";
    const MISSING_INTERNAL_PORT: &'static str = "internal_port";
    const ARG_MODEL: &'static str = "-m";
    const ARG_HOST: &'static str = "--host";
    const ARG_PORT: &'static str = "--port";
    const ARG_NGL: &'static str = "-ngl";
    const ARG_CTX: &'static str = "-c";
    const ARG_PARALLEL: &'static str = "--parallel";
    const PARALLEL_SLOTS: &'static str = "1";

    /// Env for a child. Always forces `HARNESS_SUPERVISOR=false` so a worker never inherits
    /// the supervisor's own mode flag (which would make it try to bind the supervisor port).
    fn env(pairs: &[(&str, String)]) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect();
        env.push((
            Self::ENV_SUPERVISOR.to_string(),
            Self::SUPERVISOR_OFF.to_string(),
        ));
        env
    }

    fn missing(def: &RuntimeDef, missing: &'static str) -> ManagerError {
        ManagerError::MissingConfig {
            runtime: def.name.clone(),
            missing,
        }
    }

    /// Build the launch plan for `def` loading `model`, per the catalog's port settings.
    pub fn build(
        def: &RuntimeDef,
        model: &ResolvedModel,
        catalog: &Catalog,
        n_ctx_override: Option<u32>,
    ) -> Result<SpawnPlan, ManagerError> {
        // A per-launch UI override wins over the catalog's `n_ctx`; an unset value (neither given)
        // means "let the engine default to the model's trained context length": omit
        // `HARNESS_N_CTX` for the in-process engines, and pass `-c 0` (llama.cpp's "use trained
        // context" convention) to the external llama-server.
        let n_ctx = n_ctx_override.or(def.n_ctx);
        let gguf = model.gguf.to_string_lossy().into_owned();
        let bind = format!("{}:{}", Self::BIND_ALL, catalog.worker_port);
        let children = match def.kind {
            RuntimeKind::Candle => {
                let tokenizer = model
                    .tokenizer
                    .as_ref()
                    .ok_or_else(|| Self::missing(def, Self::MISSING_TOKENIZER))?;
                let mut pairs = vec![
                    (Self::ENV_ENGINE, Self::ENGINE_CANDLE.to_string()),
                    (Self::ENV_MODEL, gguf),
                    (
                        Self::ENV_TOKENIZER,
                        tokenizer.to_string_lossy().into_owned(),
                    ),
                ];
                if let Some(n) = n_ctx {
                    pairs.push((Self::ENV_N_CTX, n.to_string()));
                }
                pairs.push((Self::ENV_BIND, bind));
                vec![ChildSpec {
                    exe: def.exe.clone(),
                    args: Vec::new(),
                    env: Self::env(&pairs),
                }]
            }
            RuntimeKind::Llamacpp => {
                let ngl = def.n_gpu_layers.unwrap_or(Self::DEFAULT_NGL_WORKER);
                let mut pairs = vec![
                    (Self::ENV_ENGINE, Self::ENGINE_LLAMACPP.to_string()),
                    (Self::ENV_MODEL, gguf),
                ];
                if let Some(n) = n_ctx {
                    pairs.push((Self::ENV_N_CTX, n.to_string()));
                }
                pairs.push((Self::ENV_NGL, ngl.to_string()));
                pairs.push((Self::ENV_BIND, bind));
                vec![ChildSpec {
                    exe: def.exe.clone(),
                    args: Vec::new(),
                    env: Self::env(&pairs),
                }]
            }
            RuntimeKind::Http => {
                let server_exe = def
                    .server_exe
                    .clone()
                    .ok_or_else(|| Self::missing(def, Self::MISSING_SERVER_EXE))?;
                let port = def
                    .internal_port
                    .ok_or_else(|| Self::missing(def, Self::MISSING_INTERNAL_PORT))?;
                let ngl = def.n_gpu_layers.unwrap_or(Self::DEFAULT_NGL_SERVER);
                // `-c 0` tells llama-server to use the model's trained context length; an explicit
                // catalog value is passed through as-is.
                let ctx_arg = n_ctx.unwrap_or(0).to_string();
                // The proxy reports this as `model_name`; use the clean display name (the gguf file
                // stem), never llama-server's absolute path.
                let display_name = model
                    .gguf
                    .file_stem()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let server = ChildSpec {
                    exe: server_exe,
                    args: [
                        Self::ARG_MODEL,
                        &gguf,
                        Self::ARG_HOST,
                        Self::LOOPBACK,
                        Self::ARG_PORT,
                        &port.to_string(),
                        Self::ARG_NGL,
                        &ngl.to_string(),
                        Self::ARG_CTX,
                        &ctx_arg,
                        Self::ARG_PARALLEL,
                        Self::PARALLEL_SLOTS,
                    ]
                    .iter()
                    .map(|a| (*a).to_string())
                    .collect(),
                    env: Self::env(&[]),
                };
                let proxy = ChildSpec {
                    exe: def.exe.clone(),
                    args: Vec::new(),
                    env: Self::env(&[
                        (Self::ENV_ENGINE, Self::ENGINE_HTTP.to_string()),
                        (Self::ENV_MODEL, display_name),
                        (
                            Self::ENV_SERVER_URL,
                            format!("http://{}:{}", Self::LOOPBACK, port),
                        ),
                        (Self::ENV_BIND, bind),
                    ]),
                };
                vec![server, proxy]
            }
        };
        Ok(SpawnPlan { children })
    }
}

/// A running child process.
#[async_trait]
pub trait WorkerHandle: Send {
    /// Request termination (non-blocking).
    fn kill(&mut self);
    fn try_wait(&mut self) -> Option<ChildExit>;
    /// Resolve once the process has exited.
    async fn wait(&mut self);
}

/// Starts child processes.
pub trait WorkerLauncher: Send + Sync {
    fn spawn(&self, spec: &ChildSpec) -> io::Result<Box<dyn WorkerHandle>>;
}

/// Decides when a freshly spawned harness is serving.
#[async_trait]
pub trait ReadinessProbe: Send + Sync {
    async fn await_ready(&self, url: &str, timeout: Duration) -> bool;
}

/// Tunables for the manager (injectable so tests can use tiny durations).
#[derive(Debug, Clone, Copy)]
pub struct ManagerConfig {
    /// Overall budget for a harness to become ready.
    pub ready_timeout: Duration,
    /// How often children are checked for early exit while loading.
    pub poll_interval: Duration,
    /// Extra time beyond `ready_timeout` before the outer guard gives up on a stuck probe.
    pub ready_grace: Duration,
    /// How often a Ready harness is checked for a crash.
    pub health_interval: Duration,
    /// Port of the agent runtime server, advertised as `chat_url`; `None` when none is hosted.
    pub chat_port: Option<u16>,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        ManagerConfig {
            ready_timeout: Duration::from_secs(180),
            poll_interval: Duration::from_millis(250),
            ready_grace: Duration::from_secs(5),
            health_interval: Duration::from_secs(2),
            chat_port: None,
        }
    }
}

struct Live {
    handles: Vec<Box<dyn WorkerHandle>>,
    generation: u64,
}

/// State shared with the background crash watcher (which must not depend on L/P).
struct Shared {
    live: Mutex<Option<Live>>,
    snapshot: StdMutex<Option<HarnessInfo>>,
    tx: broadcast::Sender<HarnessInfo>,
    generation: AtomicU64,
    watcher: StdMutex<Option<JoinHandle<()>>>,
}

impl Shared {
    fn publish(&self, info: HarnessInfo) {
        *self.snapshot.lock().unwrap() = Some(info.clone());
        let _ = self.tx.send(info);
    }

    fn current(&self) -> Option<HarnessInfo> {
        self.snapshot.lock().unwrap().clone()
    }

    fn with_state(info: &HarnessInfo, state: HarnessState) -> HarnessInfo {
        HarnessInfo {
            state,
            url: None,
            chat_url: None,
            ..info.clone()
        }
    }

    /// Kill every child, then confirm each has exited (so VRAM is released).
    async fn teardown(handles: &mut [Box<dyn WorkerHandle>]) {
        for h in handles.iter_mut() {
            h.kill();
        }
        for h in handles.iter_mut() {
            h.wait().await;
        }
    }

    fn set_watcher(&self, task: Option<JoinHandle<()>>) {
        let mut slot = self.watcher.lock().unwrap();
        if let Some(old) = slot.take() {
            old.abort();
        }
        *slot = task;
    }

    /// Check a Ready harness for an exited child. Returns true while still worth watching.
    async fn check_health(&self, generation: Option<u64>) -> bool {
        let mut live = self.live.lock().await;
        let Some(l) = live.as_mut() else {
            return false;
        };
        if generation.is_some_and(|g| g != l.generation) {
            return false;
        }
        if !l.handles.iter_mut().any(|h| h.try_wait().is_some()) {
            return true;
        }
        if let Some(mut dead) = live.take() {
            Self::teardown(&mut dead.handles).await;
        }
        if let Some(info) = self.current() {
            self.publish(Self::with_state(&info, HarnessState::Failed));
        }
        false
    }
}

/// Owns at most one harness and serialises start/stop.
pub struct HarnessManager<L: WorkerLauncher, P: ReadinessProbe> {
    catalog: Catalog,
    launcher: L,
    probe: P,
    config: ManagerConfig,
    shared: Arc<Shared>,
}

impl<L: WorkerLauncher, P: ReadinessProbe> HarnessManager<L, P> {
    const EVENT_CAPACITY: usize = 64;
    const LOCAL_HOST: &'static str = "127.0.0.1";

    pub fn new(catalog: Catalog, launcher: L, probe: P, config: ManagerConfig) -> Self {
        let (tx, _) = broadcast::channel(Self::EVENT_CAPACITY);
        HarnessManager {
            catalog,
            launcher,
            probe,
            config,
            shared: Arc::new(Shared {
                live: Mutex::new(None),
                snapshot: StdMutex::new(None),
                tx,
                generation: AtomicU64::new(0),
                watcher: StdMutex::new(None),
            }),
        }
    }

    /// The current harness (if any), including a Failed one until the next start/stop.
    pub fn list(&self) -> Vec<HarnessInfo> {
        self.shared.current().into_iter().collect()
    }

    /// The current harness (if any) in whatever state it is in.
    pub fn current(&self) -> Option<HarnessInfo> {
        self.shared.current()
    }

    /// The local worker URL while the current harness is Ready; `None` otherwise.
    pub fn ready_worker_url(&self) -> Option<String> {
        let info = self.shared.current()?;
        (info.state == HarnessState::Ready)
            .then(|| format!("ws://{}:{}", Self::LOCAL_HOST, self.catalog.worker_port))
    }

    /// Subscribe to `harness_changed` events.
    pub fn subscribe(&self) -> broadcast::Receiver<HarnessInfo> {
        self.shared.tx.subscribe()
    }

    /// Run one crash check now (the background watcher does this periodically).
    pub async fn poll_health(&self) {
        self.shared.check_health(None).await;
    }

    async fn spawn_all(
        &self,
        plan: &SpawnPlan,
    ) -> Result<Vec<Box<dyn WorkerHandle>>, ManagerError> {
        let mut handles: Vec<Box<dyn WorkerHandle>> = Vec::new();
        for spec in &plan.children {
            match self.launcher.spawn(spec) {
                Ok(h) => handles.push(h),
                Err(e) => {
                    Shared::teardown(&mut handles).await;
                    return Err(ManagerError::Spawn(e));
                }
            }
        }
        Ok(handles)
    }

    /// Wait for readiness, failing fast if any child exits first.
    async fn await_ready(&self, handles: &mut [Box<dyn WorkerHandle>]) -> Result<(), ManagerError> {
        let url = format!("ws://{}:{}", Self::LOCAL_HOST, self.catalog.worker_port);
        let probe = self.probe.await_ready(&url, self.config.ready_timeout);
        tokio::pin!(probe);
        let outer = self.config.ready_timeout + self.config.ready_grace;
        let watch = async {
            loop {
                tokio::select! {
                    ready = &mut probe => {
                        return if ready { Ok(()) } else { Err(ManagerError::LoadTimeout) };
                    }
                    _ = tokio::time::sleep(self.config.poll_interval) => {
                        for h in handles.iter_mut() {
                            if let Some(exit) = h.try_wait() {
                                return Err(ManagerError::WorkerExited(exit.code));
                            }
                        }
                    }
                }
            }
        };
        tokio::time::timeout(outer, watch)
            .await
            .unwrap_or(Err(ManagerError::LoadTimeout))
    }

    /// Start `model` on `runtime`, evicting any live harness first.
    pub async fn start(
        &self,
        runtime: &str,
        model: &str,
        n_ctx_override: Option<u32>,
    ) -> Result<HarnessInfo, ManagerError> {
        let def = self
            .catalog
            .runtimes
            .iter()
            .find(|r| r.name == runtime)
            .ok_or_else(|| ManagerError::UnknownRuntime(runtime.to_string()))?;
        let resolved = resolve_model(&self.catalog, model)
            .ok_or_else(|| ManagerError::UnknownModel(model.to_string()))?;
        if !runtime_can_load(def.kind, &resolved.meta, resolved.tokenizer.is_some()) {
            return Err(ManagerError::Incompatible {
                runtime: runtime.to_string(),
                model: model.to_string(),
            });
        }
        let plan = SpawnPlan::build(def, &resolved, &self.catalog, n_ctx_override)?;

        // Held for the whole start so start/stop/crash-handling are serialised.
        let mut live = self.shared.live.lock().await;
        if let Some(mut old) = live.take() {
            if let Some(info) = self.shared.current() {
                self.shared
                    .publish(Shared::with_state(&info, HarnessState::Stopping));
            }
            Shared::teardown(&mut old.handles).await;
        }

        let mut info = HarnessInfo {
            id: format!("{runtime}:{model}"),
            runtime: runtime.to_string(),
            model: model.to_string(),
            state: HarnessState::Loading,
            url: None,
            chat_url: None,
            n_ctx: n_ctx_override,
            instance: 0,
        };
        self.shared.publish(info.clone());

        let mut handles = match self.spawn_all(&plan).await {
            Ok(h) => h,
            Err(e) => {
                self.shared.set_watcher(None);
                self.shared
                    .publish(Shared::with_state(&info, HarnessState::Failed));
                return Err(e);
            }
        };
        if let Err(e) = self.await_ready(&mut handles).await {
            Shared::teardown(&mut handles).await;
            self.shared.set_watcher(None);
            self.shared
                .publish(Shared::with_state(&info, HarnessState::Failed));
            return Err(e);
        }

        let generation = self.shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *live = Some(Live {
            handles,
            generation,
        });
        info.state = HarnessState::Ready;
        info.instance = generation;
        info.url = Some(format!(
            "ws://{}:{}",
            self.catalog.advertise_host, self.catalog.worker_port
        ));
        info.chat_url = self
            .config
            .chat_port
            .map(|port| format!("ws://{}:{}", self.catalog.advertise_host, port));
        self.shared.publish(info.clone());
        self.start_watcher(generation);
        Ok(info)
    }

    fn start_watcher(&self, generation: u64) {
        let shared = Arc::clone(&self.shared);
        let interval = self.config.health_interval;
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                if !shared.check_health(Some(generation)).await {
                    break;
                }
            }
        });
        self.shared.set_watcher(Some(task));
    }

    /// Stop the current harness (if any) and clear it from the list.
    pub async fn stop(&self) -> Result<(), ManagerError> {
        let mut live = self.shared.live.lock().await;
        self.shared.set_watcher(None);
        let Some(info) = self.shared.current() else {
            return Ok(());
        };
        if let Some(mut old) = live.take() {
            self.shared
                .publish(Shared::with_state(&info, HarnessState::Stopping));
            Shared::teardown(&mut old.handles).await;
        }
        let _ = self
            .shared
            .tx
            .send(Shared::with_state(&info, HarnessState::Stopped));
        *self.shared.snapshot.lock().unwrap() = None;
        Ok(())
    }
}

impl<L: WorkerLauncher, P: ReadinessProbe> Drop for HarnessManager<L, P> {
    fn drop(&mut self) {
        self.shared.set_watcher(None);
        if let Ok(mut live) = self.shared.live.try_lock() {
            if let Some(mut old) = live.take() {
                for h in old.handles.iter_mut() {
                    h.kill();
                }
            }
        }
    }
}

/// Windows Job Object configured to kill every member when its last handle closes, so workers
/// die with the supervisor. A no-op on other platforms.
struct JobObject {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

// The job handle is only used for create/assign/close, all thread-safe kernel calls.
#[cfg(windows)]
unsafe impl Send for JobObject {}
#[cfg(windows)]
unsafe impl Sync for JobObject {}

#[cfg(windows)]
impl JobObject {
    fn create() -> io::Result<JobObject> {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        // SAFETY: plain FFI; the info struct is zero-initialised (valid) and sized correctly.
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                let err = io::Error::last_os_error();
                windows_sys::Win32::Foundation::CloseHandle(handle);
                return Err(err);
            }
            Ok(JobObject { handle })
        }
    }

    fn assign(&self, child: &tokio::process::Child) {
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        if let Some(raw) = child.raw_handle() {
            // SAFETY: both handles are valid for the duration of the call.
            let ok = unsafe { AssignProcessToJobObject(self.handle, raw as _) };
            if ok == 0 {
                eprintln!("job object assign failed: {}", io::Error::last_os_error());
            }
        }
    }
}

#[cfg(windows)]
impl Drop for JobObject {
    fn drop(&mut self) {
        // SAFETY: we own the handle; closing it kills any remaining members.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(not(windows))]
impl JobObject {
    fn create() -> io::Result<JobObject> {
        Ok(JobObject {})
    }

    fn assign(&self, _child: &tokio::process::Child) {}
}

/// Real launcher: spawns OS processes, each assigned to a kill-on-close Job Object (Windows).
///
/// H1 live-validation notes: the child is assigned to the job just after spawn (a tiny window
/// where it is not yet a member); on Windows < 8 nested jobs fail, though the explicit
/// kill-on-stop/drop path still applies.
pub struct ProcessLauncher {
    job: Option<JobObject>,
}

impl ProcessLauncher {
    pub fn new() -> Self {
        let job = match JobObject::create() {
            Ok(j) => Some(j),
            Err(e) => {
                eprintln!("job object unavailable, relying on explicit kill: {e}");
                None
            }
        };
        ProcessLauncher { job }
    }
}

impl Default for ProcessLauncher {
    fn default() -> Self {
        Self::new()
    }
}

struct ProcessHandle {
    child: tokio::process::Child,
}

#[async_trait]
impl WorkerHandle for ProcessHandle {
    fn kill(&mut self) {
        let _ = self.child.start_kill();
    }

    fn try_wait(&mut self) -> Option<ChildExit> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(ChildExit {
                code: status.code(),
            }),
            _ => None,
        }
    }

    async fn wait(&mut self) {
        let _ = self.child.wait().await;
    }
}

impl WorkerLauncher for ProcessLauncher {
    fn spawn(&self, spec: &ChildSpec) -> io::Result<Box<dyn WorkerHandle>> {
        let child = tokio::process::Command::new(&spec.exe)
            .args(&spec.args)
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        if let Some(job) = &self.job {
            job.assign(&child);
        }
        Ok(Box::new(ProcessHandle { child }))
    }
}

/// Real probe: connects over WebSocket and waits for a valid `model_props` response.
pub struct WsProbe;

impl WsProbe {
    const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);
    const RETRY_DELAY: Duration = Duration::from_millis(500);
    const REQUEST: &'static str = r#"{"id":1,"method":"model_props"}"#;
    const KEY_RESULT: &'static str = "result";
    const KEY_ID: &'static str = "id";
    const REQUEST_ID: i64 = 1;

    async fn attempt(url: &str) -> bool {
        let Ok((mut ws, _)) = tokio_tungstenite::connect_async(url).await else {
            return false;
        };
        if ws.send(Message::text(Self::REQUEST)).await.is_err() {
            return false;
        }
        while let Some(Ok(frame)) = ws.next().await {
            if let Message::Text(text) = frame {
                return serde_json::from_str::<serde_json::Value>(text.as_str())
                    .map(|v| {
                        v.get(Self::KEY_ID).and_then(|id| id.as_i64()) == Some(Self::REQUEST_ID)
                            && v.get(Self::KEY_RESULT).is_some()
                    })
                    .unwrap_or(false);
            }
        }
        false
    }
}

#[async_trait]
impl ReadinessProbe for WsProbe {
    async fn await_ready(&self, url: &str, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            if let Ok(true) = tokio::time::timeout(Self::ATTEMPT_TIMEOUT, Self::attempt(url)).await
            {
                return true;
            }
            tokio::time::sleep(Self::RETRY_DELAY).await;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    type Events = Arc<StdMutex<Vec<String>>>;
    type ExitFlag = Arc<StdMutex<Option<ChildExit>>>;

    struct FakeHandle {
        label: String,
        events: Events,
        exit: ExitFlag,
    }

    #[async_trait]
    impl WorkerHandle for FakeHandle {
        fn kill(&mut self) {
            self.events
                .lock()
                .unwrap()
                .push(format!("kill:{}", self.label));
            *self.exit.lock().unwrap() = Some(ChildExit { code: None });
        }

        fn try_wait(&mut self) -> Option<ChildExit> {
            *self.exit.lock().unwrap()
        }

        async fn wait(&mut self) {
            self.events
                .lock()
                .unwrap()
                .push(format!("wait:{}", self.label));
        }
    }

    #[derive(Clone, Default)]
    struct FakeLauncher {
        events: Events,
        specs: Arc<StdMutex<Vec<ChildSpec>>>,
        exits: Arc<StdMutex<Vec<ExitFlag>>>,
        exit_immediately: bool,
    }

    impl WorkerLauncher for FakeLauncher {
        fn spawn(&self, spec: &ChildSpec) -> io::Result<Box<dyn WorkerHandle>> {
            let label = spec.exe.file_name().unwrap().to_string_lossy().into_owned();
            self.events.lock().unwrap().push(format!("spawn:{label}"));
            self.specs.lock().unwrap().push(spec.clone());
            let initial = self.exit_immediately.then_some(ChildExit { code: Some(1) });
            let exit: ExitFlag = Arc::new(StdMutex::new(initial));
            self.exits.lock().unwrap().push(Arc::clone(&exit));
            Ok(Box::new(FakeHandle {
                label,
                events: Arc::clone(&self.events),
                exit,
            }))
        }
    }

    #[derive(Clone, Copy)]
    enum Mode {
        Ready,
        Never,
        Hang,
    }

    #[derive(Clone)]
    struct FakeProbe {
        mode: Mode,
        urls: Arc<StdMutex<Vec<String>>>,
    }

    impl FakeProbe {
        fn new(mode: Mode) -> Self {
            FakeProbe {
                mode,
                urls: Arc::default(),
            }
        }
    }

    #[async_trait]
    impl ReadinessProbe for FakeProbe {
        async fn await_ready(&self, url: &str, timeout: Duration) -> bool {
            self.urls.lock().unwrap().push(url.to_string());
            match self.mode {
                Mode::Ready => true,
                Mode::Never => {
                    tokio::time::sleep(timeout).await;
                    false
                }
                Mode::Hang => {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    false
                }
            }
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
        let d = std::env::temp_dir().join(format!("mgr-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("gem.gguf"), gguf("gemma3")).unwrap();
        std::fs::write(d.join("lla.gguf"), gguf("llama")).unwrap();
        std::fs::write(d.join("tokenizer.json"), b"{}").unwrap();
        d
    }

    fn def(name: &str, kind: RuntimeKind, exe: &str) -> RuntimeDef {
        RuntimeDef {
            name: name.to_string(),
            kind,
            exe: PathBuf::from(exe),
            requires_tokenizer: false,
            n_ctx: None,
            n_gpu_layers: None,
            internal_port: None,
            server_exe: None,
        }
    }

    fn catalog(dir: &Path) -> Catalog {
        let mut http = def("http", RuntimeKind::Http, "proxy.exe");
        http.internal_port = Some(8080);
        http.server_exe = Some(PathBuf::from("llama-server.exe"));
        Catalog {
            runtimes: vec![
                def("candle", RuntimeKind::Candle, "candle.exe"),
                def("llamacpp", RuntimeKind::Llamacpp, "llamacpp.exe"),
                http,
            ],
            model_dirs: vec![dir.to_path_buf()],
            advertise_host: "192.168.1.101".to_string(),
            worker_port: 8777,
            tools: None,
            agents: Default::default(),
            hosts: Vec::new(),
        }
    }

    fn config() -> ManagerConfig {
        ManagerConfig {
            ready_timeout: Duration::from_millis(60),
            ready_grace: Duration::from_millis(20),
            poll_interval: Duration::from_millis(5),
            health_interval: Duration::from_secs(3600),
            chat_port: None,
        }
    }

    fn manager(
        tag: &str,
        launcher: FakeLauncher,
        probe: FakeProbe,
    ) -> HarnessManager<FakeLauncher, FakeProbe> {
        HarnessManager::new(catalog(&model_dir(tag)), launcher, probe, config())
    }

    fn events(l: &FakeLauncher) -> Vec<String> {
        l.events.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn start_evicts_then_spawns() {
        let l = FakeLauncher::default();
        let m = manager("evict", l.clone(), FakeProbe::new(Mode::Ready));
        m.start("candle", "gem", None).await.unwrap();
        m.start("llamacpp", "gem", None).await.unwrap();
        assert_eq!(
            events(&l),
            vec![
                "spawn:candle.exe",
                "kill:candle.exe",
                "wait:candle.exe",
                "spawn:llamacpp.exe"
            ]
        );
        let list = m.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "llamacpp:gem");
    }

    #[tokio::test]
    async fn ready_worker_url_only_when_ready() {
        let m = manager("rwu", FakeLauncher::default(), FakeProbe::new(Mode::Ready));
        assert_eq!(m.ready_worker_url(), None);
        m.start("llamacpp", "lla", None).await.unwrap();
        assert_eq!(m.ready_worker_url().as_deref(), Some("ws://127.0.0.1:8777"));
        let failing = manager("rwu2", FakeLauncher::default(), FakeProbe::new(Mode::Never));
        failing.start("llamacpp", "lla", None).await.unwrap_err();
        assert_eq!(failing.ready_worker_url(), None);
    }

    #[tokio::test]
    async fn start_times_out_to_failed() {
        let l = FakeLauncher::default();
        let m = manager("timeout", l.clone(), FakeProbe::new(Mode::Never));
        let err = m.start("llamacpp", "lla", None).await.unwrap_err();
        assert!(matches!(err, ManagerError::LoadTimeout));
        let list = m.list();
        assert_eq!(list[0].state, HarnessState::Failed);
        assert_eq!(list[0].url, None);
        assert!(events(&l).contains(&"kill:llamacpp.exe".to_string()));
    }

    #[tokio::test]
    async fn start_fails_when_child_exits_early() {
        let l = FakeLauncher {
            exit_immediately: true,
            ..FakeLauncher::default()
        };
        let m = manager("early", l.clone(), FakeProbe::new(Mode::Hang));
        let err = m.start("llamacpp", "lla", None).await.unwrap_err();
        assert!(matches!(err, ManagerError::WorkerExited(Some(1))));
        assert_eq!(m.list()[0].state, HarnessState::Failed);
    }

    #[tokio::test]
    async fn start_fails_when_probe_never_returns() {
        let l = FakeLauncher::default();
        let m = manager("hang", l.clone(), FakeProbe::new(Mode::Hang));
        let started = std::time::Instant::now();
        let err = m.start("llamacpp", "lla", None).await.unwrap_err();
        assert!(matches!(err, ManagerError::LoadTimeout));
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(m.list()[0].state, HarnessState::Failed);
        assert!(events(&l).contains(&"kill:llamacpp.exe".to_string()));
    }

    #[tokio::test]
    async fn ready_url_uses_advertise_host() {
        let probe = FakeProbe::new(Mode::Ready);
        let m = manager("url", FakeLauncher::default(), probe.clone());
        let info = m.start("llamacpp", "lla", None).await.unwrap();
        assert_eq!(info.state, HarnessState::Ready);
        assert_eq!(info.url.as_deref(), Some("ws://192.168.1.101:8777"));
        assert_eq!(
            probe.urls.lock().unwrap().as_slice(),
            ["ws://127.0.0.1:8777".to_string()]
        );
    }

    #[tokio::test]
    async fn ready_info_carries_chat_url() {
        let cfg = ManagerConfig {
            chat_port: Some(8779),
            ..config()
        };
        let m = HarnessManager::new(
            catalog(&model_dir("chat")),
            FakeLauncher::default(),
            FakeProbe::new(Mode::Ready),
            cfg,
        );
        let info = m.start("llamacpp", "lla", None).await.unwrap();
        assert_eq!(info.chat_url.as_deref(), Some("ws://192.168.1.101:8779"));
        let plain = manager(
            "nochat",
            FakeLauncher::default(),
            FakeProbe::new(Mode::Ready),
        );
        let info = plain.start("llamacpp", "lla", None).await.unwrap();
        assert_eq!(info.chat_url, None);
    }

    #[tokio::test]
    async fn http_runtime_spawns_two_children() {
        let l = FakeLauncher::default();
        let m = manager("http", l.clone(), FakeProbe::new(Mode::Ready));
        m.start("http", "lla", None).await.unwrap();
        let specs = l.specs.lock().unwrap().clone();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].exe, PathBuf::from("llama-server.exe"));
        assert!(specs[0].args.windows(2).any(|w| w == ["--port", "8080"]));
        assert_eq!(specs[1].exe, PathBuf::from("proxy.exe"));
        assert!(specs[1]
            .env
            .contains(&("HARNESS_MODEL".to_string(), "lla".to_string())));
        assert!(!specs[1]
            .env
            .iter()
            .any(|(k, v)| k == "HARNESS_MODEL" && (v.contains('/') || v.contains('\\'))));
        assert!(specs[1].env.contains(&(
            "HARNESS_SERVER_URL".to_string(),
            "http://127.0.0.1:8080".to_string()
        )));
        m.stop().await.unwrap();
        let ev = events(&l);
        assert!(ev.contains(&"kill:llama-server.exe".to_string()));
        assert!(ev.contains(&"kill:proxy.exe".to_string()));
        assert!(m.list().is_empty());
    }

    #[tokio::test]
    async fn every_child_env_disables_supervisor_mode() {
        let flag = ("HARNESS_SUPERVISOR".to_string(), "false".to_string());
        for (runtime, model) in [("candle", "gem"), ("llamacpp", "lla"), ("http", "lla")] {
            let l = FakeLauncher::default();
            let m = manager("supenv", l.clone(), FakeProbe::new(Mode::Ready));
            m.start(runtime, model, None).await.unwrap();
            let specs = l.specs.lock().unwrap().clone();
            assert!(!specs.is_empty());
            for spec in specs {
                assert!(spec.env.contains(&flag), "{runtime}: {:?}", spec.exe);
            }
        }
    }

    #[test]
    fn in_process_n_ctx_omitted_when_unset_present_when_set() {
        let dir = model_dir("nctx");
        let cat = catalog(&dir);
        let model = resolve_model(&cat, "gem").expect("gem.gguf resolves");

        // Unset catalog n_ctx → no HARNESS_N_CTX env (engine defaults to the trained length).
        let mut lla = def("llamacpp", RuntimeKind::Llamacpp, "llamacpp.exe");
        let plan = SpawnPlan::build(&lla, &model, &cat, None).unwrap();
        assert!(!plan.children[0]
            .env
            .iter()
            .any(|(k, _)| k == "HARNESS_N_CTX"));

        // Explicit catalog n_ctx → HARNESS_N_CTX carries it verbatim.
        lla.n_ctx = Some(2048);
        let plan = SpawnPlan::build(&lla, &model, &cat, None).unwrap();
        assert!(plan.children[0]
            .env
            .contains(&("HARNESS_N_CTX".to_string(), "2048".to_string())));
    }

    #[test]
    fn http_server_ctx_arg_is_zero_when_unset() {
        let dir = model_dir("httpctx");
        let cat = catalog(&dir);
        let model = resolve_model(&cat, "gem").expect("gem.gguf resolves");

        let mut http = def("http", RuntimeKind::Http, "proxy.exe");
        http.internal_port = Some(8080);
        http.server_exe = Some(PathBuf::from("llama-server.exe"));

        // Unset → llama-server gets `-c 0` (use the model's trained context length).
        let plan = SpawnPlan::build(&http, &model, &cat, None).unwrap();
        assert!(plan.children[0].args.windows(2).any(|w| w == ["-c", "0"]));

        // Explicit → `-c <n>`.
        http.n_ctx = Some(2048);
        let plan = SpawnPlan::build(&http, &model, &cat, None).unwrap();
        assert!(plan.children[0].args.windows(2).any(|w| w == ["-c", "2048"]));
    }

    #[test]
    fn n_ctx_override_wins_over_catalog() {
        let dir = model_dir("override");
        let cat = catalog(&dir);
        let model = resolve_model(&cat, "gem").expect("gem.gguf resolves");

        // A per-launch override is used even when the catalog leaves n_ctx unset (llamacpp).
        let lla = def("llamacpp", RuntimeKind::Llamacpp, "llamacpp.exe");
        let plan = SpawnPlan::build(&lla, &model, &cat, Some(8192)).unwrap();
        assert!(plan.children[0]
            .env
            .contains(&("HARNESS_N_CTX".to_string(), "8192".to_string())));

        // And it beats an explicit catalog value.
        let mut lla_set = def("llamacpp", RuntimeKind::Llamacpp, "llamacpp.exe");
        lla_set.n_ctx = Some(2048);
        let plan = SpawnPlan::build(&lla_set, &model, &cat, Some(8192)).unwrap();
        assert!(plan.children[0]
            .env
            .contains(&("HARNESS_N_CTX".to_string(), "8192".to_string())));
        assert!(!plan.children[0]
            .env
            .contains(&("HARNESS_N_CTX".to_string(), "2048".to_string())));

        // For the http arm the override reaches llama-server's `-c`.
        let mut http = def("http", RuntimeKind::Http, "proxy.exe");
        http.internal_port = Some(8080);
        http.server_exe = Some(PathBuf::from("llama-server.exe"));
        let plan = SpawnPlan::build(&http, &model, &cat, Some(8192)).unwrap();
        assert!(plan.children[0].args.windows(2).any(|w| w == ["-c", "8192"]));
    }

    #[tokio::test]
    async fn start_rejects_incompatible_pair() {
        let l = FakeLauncher::default();
        let m = manager("incompat", l.clone(), FakeProbe::new(Mode::Ready));
        let err = m.start("candle", "lla", None).await.unwrap_err();
        assert!(matches!(err, ManagerError::Incompatible { .. }));
        assert!(events(&l).is_empty());
        assert!(m.list().is_empty());
    }

    #[tokio::test]
    async fn start_records_n_ctx_in_info() {
        let m = manager("nctxreq", FakeLauncher::default(), FakeProbe::new(Mode::Ready));
        let info = m.start("llamacpp", "lla", Some(4096)).await.unwrap();
        assert_eq!(info.n_ctx, Some(4096));
        assert_eq!(m.current().unwrap().n_ctx, Some(4096));
        let info = m.start("llamacpp", "lla", None).await.unwrap();
        assert_eq!(info.n_ctx, None);
    }

    #[tokio::test]
    async fn id_is_runtime_colon_model() {
        let m = manager("id", FakeLauncher::default(), FakeProbe::new(Mode::Ready));
        let info = m.start("candle", "gem", None).await.unwrap();
        assert_eq!(info.id, "candle:gem");
    }

    #[tokio::test]
    async fn crash_marks_failed_and_broadcasts() {
        let l = FakeLauncher::default();
        let m = manager("crash", l.clone(), FakeProbe::new(Mode::Ready));
        let mut rx = m.subscribe();
        m.start("llamacpp", "lla", None).await.unwrap();
        *l.exits.lock().unwrap()[0].lock().unwrap() = Some(ChildExit { code: Some(139) });
        m.poll_health().await;
        assert_eq!(m.list()[0].state, HarnessState::Failed);
        assert_eq!(m.list()[0].url, None);
        let mut last = None;
        while let Ok(info) = rx.try_recv() {
            last = Some(info);
        }
        assert_eq!(last.unwrap().state, HarnessState::Failed);
    }
}
