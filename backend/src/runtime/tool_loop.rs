//! The tool loop: generate, run the requested tools, generate again, until the model answers.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde::Serialize;
use serde_json::{json, Map, Value};

use super::executor::ToolExecutor;
use super::registry::ToolRegistry;
use super::session::RuntimeSession;
use super::tool::ToolCallRequest;
use super::worker::{GenerateRequest, LinkError, Note, WorkerConnector};
use crate::engine::tool_format::ParsedCall;
use crate::engine::types::{FunctionCall, ToolCall};
use crate::engine::{ChatMessage, Role};

/// The client went away; nothing more can be reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkClosed;

impl fmt::Display for SinkClosed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("client sink closed")
    }
}

impl std::error::Error for SinkClosed {}

/// Where the loop reports progress (the client connection, in production).
#[async_trait]
pub trait LoopSink: Send {
    async fn notify(&mut self, method: &str, params: Value) -> Result<(), SinkClosed>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Answered,
    StepLimit,
    WindowFull,
    Error,
    NoHarness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopEnd {
    pub steps: usize,
    pub reason: EndReason,
}

/// How one generate step ended.
enum StepEnd {
    /// `turn_done` params.
    Done(Value),
    /// `turn_rejected` (already relayed).
    Rejected,
    /// The worker's own `turn_error` (already relayed).
    WorkerError,
    /// The stream broke or ended early; the message still has to be reported.
    Broken(String),
}

pub struct ToolLoop {
    executor: Arc<ToolExecutor>,
    registry: Arc<ToolRegistry>,
    step_cap: usize,
}

impl ToolLoop {
    const M_TURN_DONE: &'static str = "turn_done";
    const M_TURN_REJECTED: &'static str = "turn_rejected";
    const M_TURN_ERROR: &'static str = "turn_error";
    const M_TOOL_RESULT: &'static str = "tool_result";
    const M_LOOP_DONE: &'static str = "loop_done";
    const STREAM_ENDED: &'static str = "worker stream ended unexpectedly";
    const BAD_TURN_DONE: &'static str = "worker sent a malformed turn_done";
    const KIND_FUNCTION: &'static str = "function";
    const SUPPORTS_TOOLS: &'static str = "supports_tools";

    pub fn new(executor: Arc<ToolExecutor>, registry: Arc<ToolRegistry>, step_cap: usize) -> Self {
        Self {
            executor,
            registry,
            step_cap,
        }
    }

    /// Runs one user message to completion. The user message and every step's messages are
    /// committed to `session` only when the loop ends `Answered`; any other end (including a
    /// closed sink) leaves the transcript as it was. A closed sink aborts at once with
    /// `Err(SinkClosed)`; dropping the in-flight stream makes the worker stop generating.
    ///
    /// A failing `model_props` ends the loop with `Error` and a `turn_error` carrying the
    /// link error: without it we cannot know whether to send tools, and guessing would either
    /// break a tool-less model or silently disable tools.
    pub async fn run(
        &self,
        session: &mut RuntimeSession,
        content: &str,
        enable_thinking: bool,
        connector: &dyn WorkerConnector,
        sink: &mut dyn LoopSink,
    ) -> Result<LoopEnd, SinkClosed> {
        let Some(link) = connector.connect() else {
            return Self::finish(sink, 0, EndReason::NoHarness).await;
        };
        let props = match link.model_props().await {
            Ok(props) => props,
            Err(e) => return Self::fail(sink, 0, &e.0).await,
        };
        let supports_tools = props
            .get(Self::SUPPORTS_TOOLS)
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let tools = if supports_tools {
            self.registry.openai_tools()
        } else {
            Vec::new()
        };

        let mut scratch = session.transcript.clone();
        scratch.push(ChatMessage::new(Role::User, content));

        let mut steps = 0;
        while steps < self.step_cap {
            steps += 1;
            let request = GenerateRequest {
                messages: scratch.clone(),
                tools: tools.clone(),
                enable_thinking,
                sampling: session.sampling.clone(),
            };
            let stream = match link.generate(&request).await {
                Ok(stream) => stream,
                Err(e) => return Self::fail(sink, steps, &e.0).await,
            };
            let done = match Self::read_step(stream, sink).await? {
                StepEnd::Done(done) => done,
                StepEnd::Rejected => return Self::finish(sink, steps, EndReason::WindowFull).await,
                StepEnd::WorkerError => return Self::finish(sink, steps, EndReason::Error).await,
                StepEnd::Broken(message) => return Self::fail(sink, steps, &message).await,
            };
            let Some((text, calls)) = Self::parse_done(&done) else {
                return Self::fail(sink, steps, Self::BAD_TURN_DONE).await;
            };
            scratch.push(Self::assistant_message(&text, &calls));
            if calls.is_empty() {
                session.transcript = scratch;
                return Self::finish(sink, steps, EndReason::Answered).await;
            }
            if steps == self.step_cap {
                break;
            }
            for call in &calls {
                let outcome = self.executor.execute(&Self::request_of(call)).await;
                let result = outcome.result_value();
                let params = json!({
                    "id": call.id,
                    "name": call.name,
                    "ok": outcome.is_ok(),
                    "content": result,
                });
                sink.notify(Self::M_TOOL_RESULT, params).await?;
                scratch.push(Self::tool_message(&call.id, result));
            }
        }
        Self::finish(sink, steps, EndReason::StepLimit).await
    }

    /// Relays notes verbatim until one ends the step; a closed sink drops the stream.
    async fn read_step(
        mut stream: BoxStream<'static, Result<Note, LinkError>>,
        sink: &mut dyn LoopSink,
    ) -> Result<StepEnd, SinkClosed> {
        loop {
            match stream.next().await {
                Some(Ok(note)) => {
                    sink.notify(&note.method, note.params.clone()).await?;
                    match note.method.as_str() {
                        Self::M_TURN_DONE => return Ok(StepEnd::Done(note.params)),
                        Self::M_TURN_REJECTED => return Ok(StepEnd::Rejected),
                        Self::M_TURN_ERROR => return Ok(StepEnd::WorkerError),
                        _ => {}
                    }
                }
                Some(Err(e)) => return Ok(StepEnd::Broken(e.0)),
                None => return Ok(StepEnd::Broken(Self::STREAM_ENDED.into())),
            }
        }
    }

    /// `(content, calls)` from `turn_done` params; `None` if the calls are malformed.
    fn parse_done(done: &Value) -> Option<(String, Vec<ParsedCall>)> {
        let text = done
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let calls = match done.get("tool_calls") {
            None | Some(Value::Null) => Vec::new(),
            Some(raw) => serde_json::from_value(raw.clone()).ok()?,
        };
        Some((text, calls))
    }

    fn assistant_message(text: &str, calls: &[ParsedCall]) -> ChatMessage {
        let mut message = ChatMessage::new(Role::Assistant, text);
        if !calls.is_empty() {
            message.tool_calls = Some(
                calls
                    .iter()
                    .map(|c| ToolCall {
                        id: c.id.clone(),
                        kind: Self::KIND_FUNCTION.into(),
                        function: FunctionCall {
                            name: c.name.clone(),
                            arguments: Self::prompt_arguments(c),
                        },
                    })
                    .collect(),
            );
        }
        message
    }

    /// The arguments as the prompt should show them: `{}` for anything unusable.
    fn prompt_arguments(call: &ParsedCall) -> Value {
        if call.parse_error.is_none() && call.arguments.is_object() {
            call.arguments.clone()
        } else {
            Value::Object(Map::new())
        }
    }

    fn request_of(call: &ParsedCall) -> ToolCallRequest {
        ToolCallRequest {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
            parse_error: call.parse_error.clone(),
        }
    }

    fn tool_message(call_id: &str, result: Value) -> ChatMessage {
        let mut message = ChatMessage::new(Role::Tool, result.to_string());
        message.tool_call_id = Some(call_id.to_string());
        message.tool_result = Some(result);
        message
    }

    /// Reports a loop-level failure, then ends the loop with `Error`.
    async fn fail(
        sink: &mut dyn LoopSink,
        steps: usize,
        message: &str,
    ) -> Result<LoopEnd, SinkClosed> {
        sink.notify(Self::M_TURN_ERROR, json!({ "message": message }))
            .await?;
        Self::finish(sink, steps, EndReason::Error).await
    }

    async fn finish(
        sink: &mut dyn LoopSink,
        steps: usize,
        reason: EndReason,
    ) -> Result<LoopEnd, SinkClosed> {
        sink.notify(
            Self::M_LOOP_DONE,
            json!({ "steps": steps, "reason": reason }),
        )
        .await?;
        Ok(LoopEnd { steps, reason })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::policy::ReadOnlyPolicy;
    use crate::runtime::test_support::{FakeConnector, FakeLink, Notes, RecordingSink, Scripted};
    use crate::runtime::test_tools::TestSource;
    use std::time::Duration;

    const CAP: usize = 8;

    struct Fixture {
        tool_loop: ToolLoop,
        registry: Arc<ToolRegistry>,
        session: RuntimeSession,
        sink: RecordingSink,
    }

    impl Fixture {
        fn new() -> Self {
            let registry =
                Arc::new(ToolRegistry::new(vec![TestSource::echo_boxed()]).expect("registry"));
            let executor = Arc::new(ToolExecutor::new(
                registry.clone(),
                Box::new(ReadOnlyPolicy),
                Duration::from_secs(5),
            ));
            Self {
                tool_loop: ToolLoop::new(executor, registry.clone(), CAP),
                registry,
                session: RuntimeSession::new(),
                sink: RecordingSink::new(),
            }
        }

        async fn run(&mut self, link: Option<Arc<FakeLink>>) -> Result<LoopEnd, SinkClosed> {
            let connector = FakeConnector::new(link);
            self.tool_loop
                .run(&mut self.session, "q", false, &connector, &mut self.sink)
                .await
        }

        fn echo_call(id: &str) -> Value {
            Notes::call(id, "echo", json!({"text": "x"}))
        }

        fn step(content: &str, calls: Vec<Value>) -> Scripted {
            Notes::script(vec![
                Notes::started(),
                Notes::chunk(),
                Notes::done(content, calls),
            ])
        }

        fn last(&self) -> &Note {
            self.sink.notes.last().expect("a note")
        }

        fn tool_results(&self) -> Vec<&Note> {
            self.sink
                .notes
                .iter()
                .filter(|n| n.method == "tool_result")
                .collect()
        }
    }

    #[tokio::test]
    async fn answered_without_tools() {
        let mut f = Fixture::new();
        let link = FakeLink::new(true, vec![Fixture::step("hi", vec![])]);
        let end = f.run(Some(link)).await.unwrap();
        assert_eq!(
            end,
            LoopEnd {
                steps: 1,
                reason: EndReason::Answered
            }
        );
        let t = f.session.transcript();
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].role, t[0].content.as_str()), (Role::User, "q"));
        assert_eq!((t[1].role, t[1].content.as_str()), (Role::Assistant, "hi"));
        assert!(t[1].tool_calls.is_none());
        assert_eq!(f.last().method, "loop_done");
        assert_eq!(f.last().params, json!({"steps": 1, "reason": "answered"}));
        assert_eq!(
            f.sink.methods(),
            ["turn_started", "turn_chunk", "turn_done", "loop_done"]
        );
    }

    #[tokio::test]
    async fn executes_call_then_answers() {
        let mut f = Fixture::new();
        let link = FakeLink::new(
            true,
            vec![
                Fixture::step("", vec![Fixture::echo_call("call_1")]),
                Fixture::step("done", vec![]),
            ],
        );
        let end = f.run(Some(link.clone())).await.unwrap();
        assert_eq!(end.reason, EndReason::Answered);
        assert_eq!(end.steps, 2);
        let results = f.tool_results();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].params,
            json!({"id": "call_1", "name": "echo", "ok": true, "content": {"text": "x"}})
        );
        let methods = f.sink.methods();
        let first_done = methods.iter().position(|m| *m == "turn_done").unwrap();
        let result_at = methods.iter().position(|m| *m == "tool_result").unwrap();
        let second_start = methods.iter().rposition(|m| *m == "turn_started").unwrap();
        assert!(first_done < result_at && result_at < second_start);

        let t = f.session.transcript();
        assert_eq!(t.len(), 4);
        assert_eq!(t[0].role, Role::User);
        let calls = t[1].tool_calls.as_ref().expect("calls");
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].kind, "function");
        assert_eq!(calls[0].function.arguments, json!({"text": "x"}));
        assert_eq!(t[2].role, Role::Tool);
        assert_eq!(t[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(t[2].tool_result, Some(json!({"text": "x"})));
        assert_eq!(
            (t[3].role, t[3].content.as_str()),
            (Role::Assistant, "done")
        );
        let requests = link.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].messages.len(), 1);
        assert_eq!(requests[1].messages.len(), 3);
    }

    #[tokio::test]
    async fn unknown_tool_result_is_error() {
        let mut f = Fixture::new();
        let call = Notes::call("c", "nope", json!({}));
        let link = FakeLink::new(
            true,
            vec![Fixture::step("", vec![call]), Fixture::step("ok", vec![])],
        );
        f.run(Some(link)).await.unwrap();
        let params = &f.tool_results()[0].params;
        assert_eq!(params["ok"], false);
        assert_eq!(params["content"], json!({"error": "unknown tool: nope"}));
    }

    #[tokio::test]
    async fn parse_error_result_is_error() {
        let mut f = Fixture::new();
        let call = json!({
            "id": "c", "name": "echo", "arguments": "garbage", "raw": "", "parse_error": "bad"
        });
        let link = FakeLink::new(
            true,
            vec![Fixture::step("", vec![call]), Fixture::step("ok", vec![])],
        );
        f.run(Some(link)).await.unwrap();
        let params = &f.tool_results()[0].params;
        assert_eq!(params["ok"], false);
        assert_eq!(
            params["content"],
            json!({"error": "could not parse tool call: bad"})
        );
        let t = f.session.transcript();
        let args = &t[1].tool_calls.as_ref().unwrap()[0].function.arguments;
        assert_eq!(args, &json!({}));
    }

    #[tokio::test]
    async fn stops_at_step_limit() {
        let mut f = Fixture::new();
        let link = FakeLink::repeating(true, Fixture::step("", vec![Fixture::echo_call("c")]));
        let end = f.run(Some(link.clone())).await.unwrap();
        assert_eq!(
            end,
            LoopEnd {
                steps: 8,
                reason: EndReason::StepLimit
            }
        );
        assert_eq!(link.requests().len(), 8);
        // No tool_result after the 8th step: 7 results, and the last turn_done is followed
        // directly by loop_done.
        assert_eq!(f.tool_results().len(), 7);
        let last_done = f
            .sink
            .methods()
            .iter()
            .rposition(|m| *m == "turn_done")
            .unwrap();
        assert_eq!(last_done, f.sink.notes.len() - 2);
        assert!(f.session.transcript().is_empty());
        assert_eq!(f.last().params["reason"], "step_limit");
    }

    #[tokio::test]
    async fn window_full_rolls_back() {
        let mut f = Fixture::new();
        f.session
            .transcript
            .push(ChatMessage::new(Role::User, "old"));
        let link = FakeLink::new(
            true,
            vec![Notes::script(vec![Notes::started(), Notes::rejected(9, 8)])],
        );
        let end = f.run(Some(link)).await.unwrap();
        assert_eq!(
            end,
            LoopEnd {
                steps: 1,
                reason: EndReason::WindowFull
            }
        );
        assert_eq!(f.session.transcript().len(), 1);
        assert_eq!(f.last().params["reason"], "window_full");
        assert!(f.sink.methods().contains(&"turn_rejected"));
    }

    #[tokio::test]
    async fn worker_turn_error_ends_with_error_and_no_extra_note() {
        let mut f = Fixture::new();
        let err = Notes::note("turn_error", json!({"message": "boom"}));
        let link = FakeLink::new(true, vec![Notes::script(vec![Notes::started(), err])]);
        let end = f.run(Some(link)).await.unwrap();
        assert_eq!(end.reason, EndReason::Error);
        assert_eq!(
            f.sink.methods(),
            ["turn_started", "turn_error", "loop_done"]
        );
        assert!(f.session.transcript().is_empty());
    }

    #[tokio::test]
    async fn no_harness() {
        let mut f = Fixture::new();
        let end = f.run(None).await.unwrap();
        assert_eq!(
            end,
            LoopEnd {
                steps: 0,
                reason: EndReason::NoHarness
            }
        );
        assert_eq!(f.sink.notes.len(), 1);
        assert_eq!(f.last().params, json!({"steps": 0, "reason": "no_harness"}));
        assert!(f.session.transcript().is_empty());
    }

    #[tokio::test]
    async fn stream_ending_without_done_is_error() {
        let mut f = Fixture::new();
        let link = FakeLink::new(
            true,
            vec![Notes::script(vec![Notes::started(), Notes::chunk()])],
        );
        let end = f.run(Some(link)).await.unwrap();
        assert_eq!(
            end,
            LoopEnd {
                steps: 1,
                reason: EndReason::Error
            }
        );
        let at = f.sink.notes.len() - 2;
        assert_eq!(f.sink.notes[at].method, "turn_error");
        assert_eq!(
            f.sink.notes[at].params["message"],
            "worker stream ended unexpectedly"
        );
        assert!(f.session.transcript().is_empty());
    }

    #[tokio::test]
    async fn stream_err_item_is_error_with_link_text() {
        let mut f = Fixture::new();
        let items = vec![Ok(Notes::started()), Err(LinkError("worker idle".into()))];
        let link = FakeLink::new(true, vec![Ok(items)]);
        let end = f.run(Some(link)).await.unwrap();
        assert_eq!(end.reason, EndReason::Error);
        let at = f.sink.notes.len() - 2;
        assert_eq!(f.sink.notes[at].method, "turn_error");
        assert_eq!(f.sink.notes[at].params["message"], "worker idle");
        assert!(f.session.transcript().is_empty());
    }

    #[tokio::test]
    async fn generate_failure_is_error() {
        let mut f = Fixture::new();
        let link = FakeLink::new(true, vec![Err(LinkError("refused".into()))]);
        let end = f.run(Some(link)).await.unwrap();
        assert_eq!(
            end,
            LoopEnd {
                steps: 1,
                reason: EndReason::Error
            }
        );
        assert_eq!(f.sink.methods(), ["turn_error", "loop_done"]);
        assert_eq!(f.sink.notes[0].params["message"], "refused");
    }

    #[tokio::test]
    async fn model_props_failure_is_error() {
        let mut f = Fixture::new();
        let end = f
            .run(Some(FakeLink::failing_props("props down")))
            .await
            .unwrap();
        assert_eq!(
            end,
            LoopEnd {
                steps: 0,
                reason: EndReason::Error
            }
        );
        assert_eq!(f.sink.notes[0].method, "turn_error");
        assert_eq!(f.sink.notes[0].params["message"], "props down");
        assert_eq!(f.last().params["reason"], "error");
    }

    #[tokio::test]
    async fn closed_sink_aborts_and_leaves_transcript() {
        let mut f = Fixture::new();
        f.sink = RecordingSink::closing_after(1);
        let link = FakeLink::new(true, vec![Fixture::step("hi", vec![])]);
        let result = f.run(Some(link.clone())).await;
        assert_eq!(result, Err(SinkClosed));
        assert!(f.session.transcript().is_empty());
        assert_eq!(link.requests().len(), 1);
        assert_eq!(f.sink.notes.len(), 1);
    }

    #[tokio::test]
    async fn closed_sink_during_tool_results_aborts() {
        let mut f = Fixture::new();
        // started, chunk, done are accepted; the tool_result notification is refused.
        f.sink = RecordingSink::closing_after(3);
        let link = FakeLink::new(
            true,
            vec![
                Fixture::step("", vec![Fixture::echo_call("c")]),
                Fixture::step("done", vec![]),
            ],
        );
        let result = f.run(Some(link.clone())).await;
        assert_eq!(result, Err(SinkClosed));
        assert_eq!(link.requests().len(), 1);
        assert!(f.session.transcript().is_empty());
    }

    #[tokio::test]
    async fn tools_sent_only_when_supported() {
        let mut f = Fixture::new();
        let off = FakeLink::new(false, vec![Fixture::step("hi", vec![])]);
        f.run(Some(off.clone())).await.unwrap();
        assert!(off.requests()[0].tools.is_empty());

        let mut f2 = Fixture::new();
        let on = FakeLink::new(true, vec![Fixture::step("hi", vec![])]);
        f2.run(Some(on.clone())).await.unwrap();
        assert_eq!(on.requests()[0].tools, f2.registry.openai_tools());
        assert!(!on.requests()[0].tools.is_empty());
    }

    #[tokio::test]
    async fn sampling_and_thinking_forwarded() {
        let mut f = Fixture::new();
        f.session.sampling.temperature = 0.25;
        f.session.sampling.seed = 7;
        let link = FakeLink::new(true, vec![Fixture::step("hi", vec![])]);
        let connector = FakeConnector::new(Some(link.clone()));
        f.tool_loop
            .run(&mut f.session, "q", true, &connector, &mut f.sink)
            .await
            .unwrap();
        let req = &link.requests()[0];
        assert!(req.enable_thinking);
        assert_eq!(req.sampling, f.session.sampling);
        assert_eq!(req.sampling.seed, 7);
    }

    #[tokio::test]
    async fn answered_loop_extends_prior_transcript() {
        let mut f = Fixture::new();
        f.session
            .transcript
            .push(ChatMessage::new(Role::User, "old"));
        f.session
            .transcript
            .push(ChatMessage::new(Role::Assistant, "older"));
        let link = FakeLink::new(true, vec![Fixture::step("hi", vec![])]);
        f.run(Some(link.clone())).await.unwrap();
        assert_eq!(link.requests()[0].messages.len(), 3);
        assert_eq!(f.session.transcript().len(), 4);
    }
}
