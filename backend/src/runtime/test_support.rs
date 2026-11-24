//! Fakes shared by runtime tests: a scripted worker link/connector, a recording sink, a temp
//! dir, a session handle ([`FakeSession`]) and a complete `RuntimeDeps` builder
//! ([`TestRuntime`]).
//! Import as `crate::runtime::test_support::{FakeConnector, FakeLink, FakeSession, Notes,
//! RecordingSink, Scripted, TempDir, TestRuntime}`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::{json, Value};

use super::tool::SessionHandle;
use super::tool_loop::{LinkFailure, LoopSink, SinkClosed, StepLink, ToolLoop, TurnLinks};
use super::worker::{GenerateRequest, LinkError, Note, PromptSize, WorkerLink};

/// What one `generate` call yields: a stream of items, or an immediate failure.
pub(crate) type Scripted = Result<Vec<Result<Note, LinkError>>, LinkError>;

/// Builders for worker notes.
pub(crate) struct Notes;

impl Notes {
    pub(crate) fn note(method: &str, params: Value) -> Note {
        Note {
            method: method.into(),
            params,
        }
    }

    pub(crate) fn started() -> Note {
        Self::note("turn_started", json!({}))
    }

    pub(crate) fn chunk() -> Note {
        Self::note("turn_chunk", json!({"text": "x"}))
    }

    /// `turn_started` carrying a prompt and its fill, as the worker sends it.
    pub(crate) fn started_fill(prompt: &str, prompt_tokens: u32, n_ctx: u32) -> Note {
        Self::note(
            "turn_started",
            json!({"prompt": prompt, "prompt_tokens": prompt_tokens, "n_ctx": n_ctx}),
        )
    }

    /// `turn_chunk` with `tokens` token entries, as every engine sends it.
    pub(crate) fn chunk_of(text: &str, tokens: usize) -> Note {
        let token = json!({"token": "t", "logprob": 0.0, "top": []});
        Self::note(
            "turn_chunk",
            json!({"text": text, "tokens": vec![token; tokens], "stop": false}),
        )
    }

    pub(crate) fn done(content: &str, calls: Vec<Value>) -> Note {
        Self::note(
            "turn_done",
            json!({"content": content, "tool_calls": calls}),
        )
    }

    /// A parsed call as it appears in `turn_done.tool_calls`.
    pub(crate) fn call(id: &str, name: &str, arguments: Value) -> Value {
        json!({"id": id, "name": name, "arguments": arguments, "raw": ""})
    }

    pub(crate) fn rejected(needed: u64, total: u64) -> Note {
        Self::note("turn_rejected", json!({"needed": needed, "total": total}))
    }

    /// Wraps notes as a successful `generate` script.
    pub(crate) fn script(notes: Vec<Note>) -> Scripted {
        Ok(notes.into_iter().map(Ok).collect())
    }
}

/// A worker link replaying scripted `generate` results and recording every request.
pub(crate) struct FakeLink {
    props: Result<Value, LinkError>,
    scripts: Mutex<VecDeque<Scripted>>,
    repeat_last: bool,
    requests: Mutex<Vec<GenerateRequest>>,
    measure: Mutex<MeasureScript>,
    measured: Mutex<Vec<GenerateRequest>>,
}

/// How a [`FakeLink`] answers `measure_prompt`.
enum MeasureScript {
    /// Every call answers this.
    Fixed(Result<PromptSize, LinkError>),
    /// Each call answers what the function computes from the request.
    By(fn(&GenerateRequest) -> PromptSize),
}

impl FakeLink {
    const MEASURE_UNAVAILABLE: &'static str = "measure_prompt unavailable";

    /// Every `measure_prompt` call answers `size` (the default is an error).
    pub(crate) fn set_measure(&self, size: PromptSize) {
        *self.measure.lock().expect("measure") = MeasureScript::Fixed(Ok(size));
    }

    /// Every `measure_prompt` call answers `sizer(request)`.
    pub(crate) fn set_measure_by(&self, sizer: fn(&GenerateRequest) -> PromptSize) {
        *self.measure.lock().expect("measure") = MeasureScript::By(sizer);
    }

    /// Every `measure_prompt` request so far, in order.
    pub(crate) fn measured(&self) -> Vec<GenerateRequest> {
        self.measured.lock().expect("measured").clone()
    }

    fn unavailable() -> Mutex<MeasureScript> {
        Mutex::new(MeasureScript::Fixed(Err(LinkError(
            Self::MEASURE_UNAVAILABLE.into(),
        ))))
    }

    /// One script per `generate` call, in order; a call past the end fails the test.
    pub(crate) fn new(supports_tools: bool, scripts: Vec<Scripted>) -> Arc<Self> {
        Arc::new(Self {
            props: Ok(json!({"supports_tools": supports_tools})),
            scripts: Mutex::new(scripts.into()),
            repeat_last: false,
            requests: Mutex::new(Vec::new()),
            measure: Self::unavailable(),
            measured: Mutex::new(Vec::new()),
        })
    }

    /// Like `new`, with arbitrary `model_props` output.
    pub(crate) fn with_props(props: Value, scripts: Vec<Scripted>) -> Arc<Self> {
        Arc::new(Self {
            props: Ok(props),
            scripts: Mutex::new(scripts.into()),
            repeat_last: false,
            requests: Mutex::new(Vec::new()),
            measure: Self::unavailable(),
            measured: Mutex::new(Vec::new()),
        })
    }

    /// Every `generate` call replays `script`.
    pub(crate) fn repeating(supports_tools: bool, script: Scripted) -> Arc<Self> {
        Arc::new(Self {
            props: Ok(json!({"supports_tools": supports_tools})),
            scripts: Mutex::new(VecDeque::from([script])),
            repeat_last: true,
            requests: Mutex::new(Vec::new()),
            measure: Self::unavailable(),
            measured: Mutex::new(Vec::new()),
        })
    }

    /// `model_props` fails with `message`.
    pub(crate) fn failing_props(message: &str) -> Arc<Self> {
        Arc::new(Self {
            props: Err(LinkError(message.into())),
            scripts: Mutex::new(VecDeque::new()),
            repeat_last: false,
            requests: Mutex::new(Vec::new()),
            measure: Self::unavailable(),
            measured: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn requests(&self) -> Vec<GenerateRequest> {
        self.requests.lock().expect("requests").clone()
    }
}

#[async_trait]
impl WorkerLink for FakeLink {
    async fn model_props(&self) -> Result<Value, LinkError> {
        self.props.clone()
    }

    async fn tokenize(&self, _text: &str) -> Result<Value, LinkError> {
        Ok(json!({"count": 0}))
    }

    async fn measure_prompt(&self, req: &GenerateRequest) -> Result<PromptSize, LinkError> {
        self.measured.lock().expect("measured").push(req.clone());
        match &*self.measure.lock().expect("measure") {
            MeasureScript::Fixed(size) => size.clone(),
            MeasureScript::By(sizer) => Ok(sizer(req)),
        }
    }

    async fn generate(
        &self,
        req: &GenerateRequest,
    ) -> Result<BoxStream<'static, Result<Note, LinkError>>, LinkError> {
        self.requests.lock().expect("requests").push(req.clone());
        let script = self
            .scripts
            .lock()
            .expect("scripts")
            .pop_front()
            .expect("generate called more often than scripted");
        if self.repeat_last {
            self.scripts
                .lock()
                .expect("scripts")
                .push_back(script.clone());
        }
        Ok(futures::stream::iter(script?).boxed())
    }
}

/// A connector that hands out scripted links (or none), one per `connect`; the last entry
/// repeats forever.
pub(crate) struct FakeConnector {
    links: Mutex<VecDeque<Option<Arc<FakeLink>>>>,
    connects: Mutex<usize>,
}

impl FakeConnector {
    /// Always hands out `link` (or nothing).
    pub(crate) fn new(link: Option<Arc<FakeLink>>) -> Self {
        Self::sequence(vec![link])
    }

    /// Hands out `links` in order, one per `connect`, then keeps repeating the last.
    pub(crate) fn sequence(links: Vec<Option<Arc<FakeLink>>>) -> Self {
        assert!(!links.is_empty(), "a connector needs at least one entry");
        Self {
            links: Mutex::new(links.into()),
            connects: Mutex::new(0),
        }
    }

    /// The next scripted link (or none); counts the call.
    pub(crate) fn connect(&self) -> Option<Arc<dyn WorkerLink>> {
        *self.connects.lock().expect("connects") += 1;
        let mut links = self.links.lock().expect("links");
        let link = if links.len() > 1 {
            links.pop_front().expect("non-empty")
        } else {
            links.front().cloned().expect("non-empty")
        };
        link.map(|l| l as Arc<dyn WorkerLink>)
    }

    /// How many times `connect` has been called.
    pub(crate) fn connects(&self) -> usize {
        *self.connects.lock().expect("connects")
    }
}

#[async_trait]
impl TurnLinks for FakeConnector {
    /// A connector with no link reports "no model running", as the production adapter does.
    async fn link(&self, _step: usize) -> Result<StepLink, LinkFailure> {
        let link = self
            .connect()
            .ok_or_else(|| LinkFailure::from(ToolLoop::NO_MODEL.to_string()))?;
        Ok(StepLink { link, load: None })
    }
}

/// A sink that records notifications; optionally closes after accepting `n` of them.
pub(crate) struct RecordingSink {
    pub(crate) notes: Vec<Note>,
    close_after: Option<usize>,
}

impl RecordingSink {
    pub(crate) fn new() -> Self {
        Self {
            notes: Vec::new(),
            close_after: None,
        }
    }

    /// Accepts `n` notifications, then every further one reports `SinkClosed`.
    pub(crate) fn closing_after(n: usize) -> Self {
        Self {
            notes: Vec::new(),
            close_after: Some(n),
        }
    }

    pub(crate) fn methods(&self) -> Vec<&str> {
        self.notes.iter().map(|n| n.method.as_str()).collect()
    }
}

#[async_trait]
impl LoopSink for RecordingSink {
    async fn notify(&mut self, method: &str, params: Value) -> Result<(), SinkClosed> {
        if self.close_after.is_some_and(|n| self.notes.len() >= n) {
            return Err(SinkClosed);
        }
        self.notes.push(Notes::note(method, params));
        Ok(())
    }
}

/// A session handle whose notes, existence and saves the test controls. A deleted session
/// has no notes; [`Self::refusing_saves`] keeps reading its notes but fails every save.
pub(crate) struct FakeSession {
    notes: Mutex<Option<Vec<String>>>,
    refuse_saves: bool,
}

impl FakeSession {
    /// A live session with no notes.
    pub(crate) fn alive() -> Arc<Self> {
        Self::with(Some(Vec::new()), false)
    }

    /// A session that no longer exists.
    pub(crate) fn deleted() -> Arc<Self> {
        Self::with(None, false)
    }

    /// A live session whose notes read fine but whose every save fails.
    pub(crate) fn refusing_saves() -> Arc<Self> {
        Self::with(Some(Vec::new()), true)
    }

    /// Deletes the session (its notes go with it).
    pub(crate) fn delete(&self) {
        *self.notes.lock().expect("notes") = None;
    }

    /// Brings a deleted session back, with no notes.
    pub(crate) fn revive(&self) {
        *self.notes.lock().expect("notes") = Some(Vec::new());
    }

    fn with(notes: Option<Vec<String>>, refuse_saves: bool) -> Arc<Self> {
        Arc::new(Self {
            notes: Mutex::new(notes),
            refuse_saves,
        })
    }
}

impl SessionHandle for FakeSession {
    fn notes(&self) -> Option<Vec<String>> {
        self.notes.lock().expect("notes").clone()
    }

    fn set_notes(&self, notes: Vec<String>) -> bool {
        let mut guard = self.notes.lock().expect("notes");
        if guard.is_none() || self.refuse_saves {
            return false;
        }
        *guard = Some(notes);
        true
    }

    fn exists(&self) -> bool {
        self.notes.lock().expect("notes").is_some()
    }
}

/// A unique temp directory, removed on drop.
pub(crate) struct TempDir(pub(crate) std::path::PathBuf);

impl TempDir {
    pub(crate) fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("rt-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A complete `RuntimeDeps` over temp-dir stores (trace logs in the sessions directory), one
/// `FakeHost` and a registry holding the test `echo` tool. The template store seeds its `Default`
/// template (id [`Self::TEMPLATE_ID`]) and its `Worker` template (id [`Self::WORKER_ID`]) for
/// runtime [`Self::RUNTIME`] and model [`Self::MODEL`], which the host serves with `link`.
/// [`Self::sandboxed`] adds a sandbox and the builtin file and note tools.
pub(crate) struct TestRuntime {
    pub(crate) deps: super::server::RuntimeDeps,
    pub(crate) host: Arc<crate::agents::host::FakeHost>,
    pub(crate) dir: TempDir,
}

impl TestRuntime {
    pub(crate) const HOST: &'static str = "hive";
    pub(crate) const RUNTIME: &'static str = "llamacpp";
    pub(crate) const MODEL: &'static str = "tiny";
    pub(crate) const TEMPLATE_ID: &'static str = "default";
    pub(crate) const WORKER_ID: &'static str = "worker";
    const TEMPLATES_DIR: &'static str = "templates";
    const SESSIONS_DIR: &'static str = "sessions";
    const SANDBOX_DIR: &'static str = "sandbox";
    const TOOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    pub(crate) fn new(tag: &str, link: Arc<FakeLink>) -> Self {
        Self::over(TempDir::new(tag), link, false)
    }

    /// Like [`Self::new`], with a sandbox (`sandbox` in the temp dir) and the builtin tools.
    pub(crate) fn sandboxed(tag: &str, link: Arc<FakeLink>) -> Self {
        Self::over(TempDir::new(tag), link, true)
    }

    /// A fresh runtime over this one's data directory, as after a supervisor restart.
    pub(crate) fn reopen(self, link: Arc<FakeLink>) -> Self {
        let sandboxed = self.deps.sandbox.is_some();
        Self::over(self.dir, link, sandboxed)
    }

    /// The sandbox root of a [`Self::sandboxed`] runtime.
    pub(crate) fn sandbox_root(&self) -> std::path::PathBuf {
        self.deps
            .sandbox
            .as_ref()
            .expect("sandboxed runtime")
            .root()
            .to_path_buf()
    }

    /// The sessions directory (session files and their trace logs).
    pub(crate) fn sessions_dir(&self) -> std::path::PathBuf {
        self.dir.0.join(Self::SESSIONS_DIR)
    }

    fn over(dir: TempDir, link: Arc<FakeLink>, sandboxed: bool) -> Self {
        use super::builtin::{BuiltinSource, Sandbox};
        use super::executor::ToolExecutor;
        use super::policy::EnabledPolicy;
        use super::registry::ToolRegistry;
        use super::test_tools::TestSource;
        use crate::agents::dispatcher::Dispatcher;
        use crate::agents::host::FakeHost;
        use crate::providers::{Provider, StaticProviders};
        use crate::agents::session_store::SessionStore;
        use crate::agents::template_store::TemplateStore;

        let sandbox = sandboxed.then(|| {
            let root = dir.0.join(Self::SANDBOX_DIR);
            std::fs::create_dir_all(&root).expect("sandbox dir");
            Sandbox::new(&root).expect("sandbox")
        });
        let mut sources = vec![TestSource::echo_boxed()];
        if let Some(sandbox) = &sandbox {
            sources.push(Box::new(BuiltinSource::new(Some(sandbox.clone()))));
        }
        let registry = Arc::new(ToolRegistry::new(sources).expect("registry"));
        let executor = Arc::new(ToolExecutor::new(
            registry.clone(),
            Box::new(EnabledPolicy),
            Self::TOOL_TIMEOUT,
        ));
        let host = FakeHost::serving_with_link(Self::HOST, &[(Self::RUNTIME, Self::MODEL)], link);
        let providers = vec![(Self::HOST.to_string(), host.clone() as Arc<dyn Provider>)];
        let templates =
            TemplateStore::open(&dir.0.join(Self::TEMPLATES_DIR), Self::rules(&registry))
                .expect("template store");
        let sessions = SessionStore::open(&dir.0.join(Self::SESSIONS_DIR)).expect("session store");
        let deps = super::server::RuntimeDeps {
            templates: Arc::new(templates),
            sessions: Arc::new(sessions),
            dispatcher: Arc::new(Dispatcher::new(Arc::new(StaticProviders(providers)))),
            registry: registry.clone(),
            tool_loop: Arc::new(super::tool_loop::ToolLoop::new(executor, registry)),
            catalog: Arc::new(Self::catalog()),
            hub: Arc::new(super::hub::Hub::new()),
            traces: Arc::new(super::trace::TraceStore::new(
                dir.0.join(Self::SESSIONS_DIR),
            )),
            sandbox,
        };
        Self { deps, host, dir }
    }

    /// Runtime `llamacpp` and model `tiny` (servable by it), every registry tool and the host.
    pub(crate) fn rules(
        registry: &super::registry::ToolRegistry,
    ) -> crate::agents::template::TemplateRules {
        crate::agents::template::TemplateRules {
            runtimes: vec![Self::RUNTIME.to_string()],
            models: vec![(Self::MODEL.to_string(), vec![Self::RUNTIME.to_string()])],
            tools: registry.specs().into_iter().map(|s| s.name).collect(),
            hosts: vec![Self::HOST.to_string()],
            writing_tools: registry
                .specs()
                .into_iter()
                .filter(|s| s.side_effects)
                .map(|s| s.name)
                .collect(),
        }
    }

    pub(crate) fn catalog() -> super::server::CatalogView {
        super::server::CatalogView {
            runtimes: vec![json!({"name": Self::RUNTIME})],
            models: vec![json!({"name": Self::MODEL, "runtimes": [Self::RUNTIME]})],
            hosts: vec![Self::HOST.to_string()],
        }
    }

    /// The seeded template, as stored.
    pub(crate) fn template(&self) -> crate::agents::template::AgentTemplate {
        self.deps
            .templates
            .get(Self::TEMPLATE_ID)
            .expect("seed template")
            .0
    }

    /// Marks the seeded template's model loaded on the host (firing its signal).
    pub(crate) fn load(&self) {
        let t = self.template();
        self.host.set_state(crate::agents::host::HostState {
            loaded: Some((t.runtime, t.model, Some(t.n_ctx))),
            loading: false,
        });
    }
}
