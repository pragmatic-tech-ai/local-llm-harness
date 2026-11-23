//! Fakes shared by runtime tests: a scripted worker link/connector and a recording sink.
//! Import as `crate::runtime::test_support::{FakeConnector, FakeLink, RecordingSink, Notes}`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::{json, Value};

use super::tool_loop::{LoopSink, SinkClosed};
use super::worker::{GenerateRequest, LinkError, Note, WorkerConnector, WorkerLink};

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
}

impl FakeLink {
    /// One script per `generate` call, in order; a call past the end fails the test.
    pub(crate) fn new(supports_tools: bool, scripts: Vec<Scripted>) -> Arc<Self> {
        Arc::new(Self {
            props: Ok(json!({"supports_tools": supports_tools})),
            scripts: Mutex::new(scripts.into()),
            repeat_last: false,
            requests: Mutex::new(Vec::new()),
        })
    }

    /// Every `generate` call replays `script`.
    pub(crate) fn repeating(supports_tools: bool, script: Scripted) -> Arc<Self> {
        Arc::new(Self {
            props: Ok(json!({"supports_tools": supports_tools})),
            scripts: Mutex::new(VecDeque::from([script])),
            repeat_last: true,
            requests: Mutex::new(Vec::new()),
        })
    }

    /// `model_props` fails with `message`.
    pub(crate) fn failing_props(message: &str) -> Arc<Self> {
        Arc::new(Self {
            props: Err(LinkError(message.into())),
            scripts: Mutex::new(VecDeque::new()),
            repeat_last: false,
            requests: Mutex::new(Vec::new()),
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

/// A connector that hands out one link, or none.
pub(crate) struct FakeConnector {
    link: Option<Arc<FakeLink>>,
}

impl FakeConnector {
    pub(crate) fn new(link: Option<Arc<FakeLink>>) -> Self {
        Self { link }
    }
}

impl WorkerConnector for FakeConnector {
    fn connect(&self) -> Option<Arc<dyn WorkerLink>> {
        self.link.clone().map(|l| l as Arc<dyn WorkerLink>)
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
