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
use super::tool::ToolCallRequest;
use super::worker::{GenerateRequest, LinkError, Note, WorkerLink};
use crate::engine::tool_format::ParsedCall;
use crate::engine::types::{FunctionCall, ToolCall};
use crate::engine::{ChatMessage, Role, SamplingConfig};

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

/// What one session configures for its turns.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnConfig {
    /// The maximum number of generate calls per user message; `0` means no generate call is
    /// made and every run ends `StepLimit` with 0 steps.
    pub step_budget: usize,
    /// Names of the registry tools this session may use.
    pub tools: Vec<String>,
    pub enable_thinking: bool,
    pub sampling: SamplingConfig,
}

/// Where the loop gets the worker link for each step.
#[async_trait]
pub trait TurnLinks: Send + Sync {
    /// The link to use for zero-based `step`. The `Err` text is reported as `turn_error`
    /// (except at step 0, where it ends the loop `NoHarness`).
    async fn link(&self, step: usize) -> Result<Arc<dyn WorkerLink>, String>;
}

pub struct ToolLoop {
    executor: Arc<ToolExecutor>,
    registry: Arc<ToolRegistry>,
}

impl ToolLoop {
    const M_TOOL_CALL: &'static str = "tool_call";
    const M_TURN_DONE: &'static str = "turn_done";
    const M_TURN_REJECTED: &'static str = "turn_rejected";
    const M_TURN_ERROR: &'static str = "turn_error";
    const M_TOOL_RESULT: &'static str = "tool_result";
    const M_LOOP_DONE: &'static str = "loop_done";
    const STREAM_ENDED: &'static str = "worker stream ended unexpectedly";
    const BAD_TURN_DONE: &'static str = "worker sent a malformed turn_done";
    const KIND_FUNCTION: &'static str = "function";
    const SUPPORTS_TOOLS: &'static str = "supports_tools";
    const FIELD_ID: &'static str = "id";
    const CALL_ID_PREFIX: &'static str = "call_";
    const FIELD_TOOL_CALLS: &'static str = "tool_calls";
    const FIELD_CONTENT: &'static str = "content";
    /// Reported when no harness is Ready (shared with the runtime server).
    pub(crate) const NO_MODEL: &'static str = "no model running";

    pub fn new(executor: Arc<ToolExecutor>, registry: Arc<ToolRegistry>) -> Self {
        Self { executor, registry }
    }

    /// Runs one user message to completion on top of `transcript`. The new transcript (the
    /// user message and every step's messages) is returned only when the loop ends `Answered`,
    /// after `loop_done` was delivered; the caller commits it. Any other end (including a
    /// closed sink) returns `None`, so the caller's transcript stays as it was. A closed sink aborts at once with
    /// `Err(SinkClosed)`; dropping the in-flight stream makes the worker stop generating.
    ///
    /// The runtime is the authority on call ids. The worker numbers calls from `call_1` on
    /// every `generate`, so ids repeat across steps and the chat template could bind a later
    /// result to an earlier call. Each call is renamed `call_{k}`, with k counting every tool
    /// call already in the scratch transcript plus its position in the step. The relayed
    /// `tool_call` notes and `turn_done.tool_calls` are the one exception to verbatim relay:
    /// only their `id`s are rewritten (by position), so the wire is self-consistent.
    ///
    /// Every step asks `links` for the current link and re-reads its `model_props`, so a
    /// harness swapped between steps is used (with its own tool support). No link at the first
    /// step ends `NoHarness`; one that disappears mid-loop ends `Error` with a `turn_error`
    /// carrying the link's text, since the user message was already in flight. Only the tools
    /// named in `config.tools` are offered to the model and executed.
    ///
    /// A failing `model_props` ends the loop with `Error` and a `turn_error` carrying the
    /// link error: without it we cannot know whether to send tools, and guessing would either
    /// break a tool-less model or silently disable tools.
    pub async fn run(
        &self,
        transcript: &[ChatMessage],
        content: &str,
        config: &TurnConfig,
        links: &dyn TurnLinks,
        sink: &mut dyn LoopSink,
    ) -> Result<(LoopEnd, Option<Vec<ChatMessage>>), SinkClosed> {
        let mut scratch = transcript.to_vec();
        scratch.push(ChatMessage::new(Role::User, content));

        let mut steps = 0;
        while steps < config.step_budget {
            let link = match links.link(steps).await {
                Ok(link) => link,
                Err(message) => {
                    if steps == 0 {
                        return Self::ended(Self::finish(sink, 0, EndReason::NoHarness).await);
                    }
                    return Self::ended(Self::fail(sink, steps, &message).await);
                }
            };
            let tools = match link.model_props().await {
                Ok(props) => self.tools_for(&props, &config.tools),
                Err(e) => return Self::ended(Self::fail(sink, steps, &e.0).await),
            };
            steps += 1;
            let request = GenerateRequest {
                messages: scratch.clone(),
                tools,
                enable_thinking: config.enable_thinking,
                sampling: config.sampling.clone(),
            };
            let stream = match link.generate(&request).await {
                Ok(stream) => stream,
                Err(e) => return Self::ended(Self::fail(sink, steps, &e.0).await),
            };
            let base = Self::calls_in(&scratch);
            let done = match Self::read_step(stream, base, sink).await? {
                StepEnd::Done(done) => done,
                StepEnd::Rejected => {
                    return Self::ended(Self::finish(sink, steps, EndReason::WindowFull).await)
                }
                StepEnd::WorkerError => {
                    return Self::ended(Self::finish(sink, steps, EndReason::Error).await)
                }
                StepEnd::Broken(message) => {
                    return Self::ended(Self::fail(sink, steps, &message).await)
                }
            };
            let Some((text, calls)) = Self::parse_done(&done) else {
                return Self::ended(Self::fail(sink, steps, Self::BAD_TURN_DONE).await);
            };
            scratch.extend(Self::assistant_messages(&text, &calls));
            if calls.is_empty() {
                // Commit only once the client has been told; a closed sink keeps the old state.
                let end = Self::finish(sink, steps, EndReason::Answered).await?;
                return Ok((end, Some(scratch)));
            }
            if steps == config.step_budget {
                break;
            }
            for call in &calls {
                let outcome = self
                    .executor
                    .execute(&Self::request_of(call), &config.tools)
                    .await;
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
        Self::ended(Self::finish(sink, steps, EndReason::StepLimit).await)
    }

    /// An end that commits nothing.
    fn ended(
        end: Result<LoopEnd, SinkClosed>,
    ) -> Result<(LoopEnd, Option<Vec<ChatMessage>>), SinkClosed> {
        end.map(|end| (end, None))
    }

    /// Relays notes verbatim until one ends the step; a closed sink drops the stream.
    async fn read_step(
        mut stream: BoxStream<'static, Result<Note, LinkError>>,
        base: usize,
        sink: &mut dyn LoopSink,
    ) -> Result<StepEnd, SinkClosed> {
        let mut seen_calls = 0;
        loop {
            match stream.next().await {
                Some(Ok(mut note)) => {
                    if note.method == Self::M_TOOL_CALL {
                        if let Some(fields) = note.params.as_object_mut() {
                            let id = Self::call_id(base + seen_calls);
                            fields.insert(Self::FIELD_ID.into(), Value::String(id));
                        }
                        seen_calls += 1;
                    }
                    if note.method == Self::M_TURN_DONE {
                        Self::rename_done_calls(&mut note.params, base);
                    }
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

    /// The tools to offer the harness behind `props`: the enabled ones, or none when the
    /// model does not support tools.
    fn tools_for(&self, props: &Value, enabled: &[String]) -> Vec<Value> {
        let supports_tools = props
            .get(Self::SUPPORTS_TOOLS)
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if supports_tools {
            self.registry.openai_tools_for(enabled)
        } else {
            Vec::new()
        }
    }

    /// Renames each `turn_done.tool_calls[i].id` to its runtime id; other shapes are left for
    /// `parse_done` to reject.
    fn rename_done_calls(params: &mut Value, base: usize) {
        let Some(calls) = params
            .get_mut(Self::FIELD_TOOL_CALLS)
            .and_then(Value::as_array_mut)
        else {
            return;
        };
        for (i, call) in calls.iter_mut().enumerate() {
            if let Some(fields) = call.as_object_mut() {
                let id = Value::String(Self::call_id(base + i));
                fields.insert(Self::FIELD_ID.into(), id);
            }
        }
    }

    /// Number of tool calls already in `messages`.
    fn calls_in(messages: &[ChatMessage]) -> usize {
        messages
            .iter()
            .filter_map(|m| m.tool_calls.as_ref())
            .map(Vec::len)
            .sum()
    }

    /// The runtime id of the call at zero-based index `index` within the turn.
    fn call_id(index: usize) -> String {
        format!("{}{}", Self::CALL_ID_PREFIX, index + 1)
    }

    /// `(content, calls)` from `turn_done` params; `None` if the calls are malformed.
    fn parse_done(done: &Value) -> Option<(String, Vec<ParsedCall>)> {
        let text = done
            .get(Self::FIELD_CONTENT)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let calls = match done.get(Self::FIELD_TOOL_CALLS) {
            None | Some(Value::Null) => Vec::new(),
            Some(raw) => serde_json::from_value(raw.clone()).ok()?,
        };
        Some((text, calls))
    }

    /// The transcript entries for one step's output. Text before calls becomes its own
    /// assistant message, followed by a content-less one carrying the calls: chat templates
    /// render an assistant message as calls, then the scanned tool responses, then content,
    /// and content after the responses closes the model turn (Gemma 4 emits `<turn|>`), which
    /// would leave the next step's prompt with no open model turn. Split, the text renders
    /// before the calls inside one continuing model turn.
    fn assistant_messages(text: &str, calls: &[ParsedCall]) -> Vec<ChatMessage> {
        if calls.is_empty() || text.trim().is_empty() {
            return vec![Self::assistant_message(text, calls)];
        }
        vec![
            ChatMessage::new(Role::Assistant, text),
            Self::assistant_message("", calls),
        ]
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

    /// The result travels once, as `tool_result` (what the template formats); `content` stays
    /// empty.
    fn tool_message(call_id: &str, result: Value) -> ChatMessage {
        let mut message = ChatMessage::new(Role::Tool, "");
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
    use crate::engine::template::ChatTemplate;
    use crate::runtime::policy::ReadOnlyPolicy;
    use crate::runtime::test_support::{FakeConnector, FakeLink, Notes, RecordingSink, Scripted};
    use crate::runtime::test_tools::TestSource;
    use crate::runtime::tool::ToolSource;
    use std::time::Duration;

    const CAP: usize = 8;
    const GEMMA4: &str = include_str!("../../tests/fixtures/gemma4-chat-template.jinja");

    struct Fixture {
        tool_loop: ToolLoop,
        registry: Arc<ToolRegistry>,
        /// The committed transcript, as the caller of `run` keeps it.
        transcript: Vec<ChatMessage>,
        config: TurnConfig,
        sink: RecordingSink,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_sources(vec![TestSource::echo_boxed()], &["echo"])
        }

        fn with_sources(sources: Vec<Box<dyn ToolSource>>, enabled: &[&str]) -> Self {
            let registry = Arc::new(ToolRegistry::new(sources).expect("registry"));
            let executor = Arc::new(ToolExecutor::new(
                registry.clone(),
                Box::new(ReadOnlyPolicy),
                Duration::from_secs(5),
            ));
            Self {
                tool_loop: ToolLoop::new(executor, registry.clone()),
                registry,
                transcript: Vec::new(),
                config: TurnConfig {
                    step_budget: CAP,
                    tools: enabled.iter().map(|n| n.to_string()).collect(),
                    enable_thinking: false,
                    sampling: SamplingConfig::default(),
                },
                sink: RecordingSink::new(),
            }
        }

        async fn run(&mut self, link: Option<Arc<FakeLink>>) -> Result<LoopEnd, SinkClosed> {
            let connector = FakeConnector::new(link);
            self.run_with(&connector).await
        }

        async fn run_with(&mut self, connector: &dyn TurnLinks) -> Result<LoopEnd, SinkClosed> {
            let (end, committed) = self
                .tool_loop
                .run(
                    &self.transcript,
                    "q",
                    &self.config,
                    connector,
                    &mut self.sink,
                )
                .await?;
            if let Some(transcript) = committed {
                self.transcript = transcript;
            }
            Ok(end)
        }

        async fn run_thinking(&mut self, link: Arc<FakeLink>) -> Result<LoopEnd, SinkClosed> {
            self.config.enable_thinking = true;
            let connector = FakeConnector::new(Some(link));
            self.run_with(&connector).await
        }

        /// `messages` rendered through the real Gemma 4 template, as the worker would.
        fn render(&self, messages: &[ChatMessage], enable_thinking: bool) -> String {
            let template = ChatTemplate::from_jinja(GEMMA4).expect("template");
            template
                .render(
                    messages,
                    true,
                    enable_thinking,
                    &self.registry.openai_tools_for(&self.config.tools),
                )
                .expect("render")
        }

        fn echo_call(id: &str) -> Value {
            Notes::call(id, "echo", json!({"text": "x"}))
        }

        fn step(content: &str, calls: Vec<Value>) -> Scripted {
            let mut notes = vec![Notes::started(), Notes::chunk()];
            notes.extend(calls.iter().map(|c| Notes::note("tool_call", c.clone())));
            notes.push(Notes::done(content, calls));
            Notes::script(notes)
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
        let t = &f.transcript;
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

        let t = &f.transcript;
        assert_eq!(t.len(), 4);
        assert_eq!(t[0].role, Role::User);
        let calls = t[1].tool_calls.as_ref().expect("calls");
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].kind, "function");
        assert_eq!(calls[0].function.arguments, json!({"text": "x"}));
        assert_eq!(t[2].role, Role::Tool);
        assert_eq!(t[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(t[2].tool_result, Some(json!({"text": "x"})));
        // The result travels once, as tool_result.
        assert_eq!(t[2].content, "");
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
        let t = &f.transcript;
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
        assert!(f.transcript.is_empty());
        assert_eq!(f.last().params["reason"], "step_limit");
    }

    #[tokio::test]
    async fn window_full_rolls_back() {
        let mut f = Fixture::new();
        f.transcript = vec![ChatMessage::new(Role::User, "old")];
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
        assert_eq!(f.transcript.len(), 1);
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
        assert!(f.transcript.is_empty());
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
        assert!(f.transcript.is_empty());
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
        assert!(f.transcript.is_empty());
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
        assert!(f.transcript.is_empty());
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
        assert!(f.transcript.is_empty());
        assert_eq!(link.requests().len(), 1);
        assert_eq!(f.sink.notes.len(), 1);
    }

    #[tokio::test]
    async fn closed_sink_during_tool_results_aborts() {
        let mut f = Fixture::new();
        // started, chunk, tool_call, done are accepted; the tool_result notification is refused.
        f.sink = RecordingSink::closing_after(4);
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
        assert!(f.transcript.is_empty());
        assert_eq!(
            f.sink.methods(),
            ["turn_started", "turn_chunk", "tool_call", "turn_done"]
        );
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
        f.config.sampling.temperature = 0.25;
        f.config.sampling.seed = 7;
        let link = FakeLink::new(true, vec![Fixture::step("hi", vec![])]);
        f.config.enable_thinking = true;
        let connector = FakeConnector::new(Some(link.clone()));
        f.run_with(&connector).await.unwrap();
        let req = &link.requests()[0];
        assert!(req.enable_thinking);
        assert_eq!(req.sampling, f.config.sampling);
        assert_eq!(req.sampling.seed, 7);
    }

    #[tokio::test]
    async fn answered_loop_extends_prior_transcript() {
        let mut f = Fixture::new();
        f.transcript = vec![
            ChatMessage::new(Role::User, "old"),
            ChatMessage::new(Role::Assistant, "older"),
        ];
        let link = FakeLink::new(true, vec![Fixture::step("hi", vec![])]);
        f.run(Some(link.clone())).await.unwrap();
        assert_eq!(link.requests()[0].messages.len(), 3);
        assert_eq!(f.transcript.len(), 4);
    }

    #[tokio::test]
    async fn call_ids_are_unique_across_steps_and_consistent() {
        let mut f = Fixture::new();
        let link = FakeLink::new(
            true,
            vec![
                Fixture::step(
                    "",
                    vec![Notes::call("call_1", "echo", json!({"text": "a"}))],
                ),
                Fixture::step("", vec![Notes::call("call_1", "nope", json!({}))]),
                Fixture::step("done", vec![]),
            ],
        );
        let end = f.run(Some(link)).await.unwrap();
        assert_eq!(end.reason, EndReason::Answered);
        let relayed: Vec<&Note> = f
            .sink
            .notes
            .iter()
            .filter(|n| n.method == "tool_call")
            .collect();
        let relayed_ids: Vec<&Value> = relayed.iter().map(|n| &n.params["id"]).collect();
        assert_eq!(relayed_ids, [&json!("call_1"), &json!("call_2")]);
        let results = f.tool_results();
        let result_ids: Vec<&Value> = results.iter().map(|n| &n.params["id"]).collect();
        assert_eq!(result_ids, relayed_ids);
        let done_ids: Vec<&Value> = f
            .sink
            .notes
            .iter()
            .filter(|n| n.method == "turn_done")
            .filter_map(|n| n.params["tool_calls"].as_array())
            .flatten()
            .map(|c| &c["id"])
            .collect();
        assert_eq!(done_ids, relayed_ids);
        let t = &f.transcript;
        assert_eq!(t[1].tool_calls.as_ref().unwrap()[0].id, "call_1");
        assert_eq!(t[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(t[3].tool_calls.as_ref().unwrap()[0].id, "call_2");
        assert_eq!(t[4].tool_call_id.as_deref(), Some("call_2"));

        let template = ChatTemplate::from_jinja(GEMMA4).unwrap();
        let prompt = template
            .render(t, true, false, &f.registry.openai_tools())
            .unwrap();
        let echo_at = prompt.find("<|tool_response>response:echo").expect("echo");
        let nope_at = prompt.find("<|tool_response>response:nope").expect("nope");
        assert!(echo_at < nope_at, "{prompt}");
    }

    #[tokio::test]
    async fn preamble_text_stays_inside_an_open_model_turn() {
        let mut f = Fixture::new();
        let link = FakeLink::new(
            true,
            vec![
                Fixture::step("Let me check.", vec![Fixture::echo_call("call_1")]),
                Fixture::step("done", vec![]),
            ],
        );
        assert_eq!(
            f.run(Some(link.clone())).await.unwrap().reason,
            EndReason::Answered
        );
        let t = &f.transcript;
        assert_eq!(t.len(), 5);
        assert_eq!(
            (t[1].role, t[1].content.as_str()),
            (Role::Assistant, "Let me check.")
        );
        assert!(t[1].tool_calls.is_none());
        assert_eq!((t[2].role, t[2].content.as_str()), (Role::Assistant, ""));
        assert_eq!(t[2].tool_calls.as_ref().unwrap()[0].id, "call_1");
        assert_eq!(t[3].role, Role::Tool);
        assert_eq!(t[4].content, "done");

        let step2 = f.render(&link.requests()[1].messages, false);
        assert!(!step2.ends_with("<turn|>\n"), "{step2}");
        assert!(!step2.contains("Let me check.<turn|>"), "{step2}");
        let call_and_response = concat!(
            "<|tool_call>call:echo{text:<|\"|>x<|\"|>}<tool_call|>",
            "<|tool_response>response:echo{text:<|\"|>x<|\"|>}<tool_response|>",
        );
        let open_turn =
            format!("<|turn>user\nq<turn|>\n<|turn>model\nLet me check.{call_and_response}");
        assert!(step2.ends_with(&open_turn), "{step2}");

        // The next user message: the whole loop is one closed model turn.
        let next = FakeLink::new(true, vec![Fixture::step("ok", vec![])]);
        f.run(Some(next.clone())).await.unwrap();
        let later = f.render(&next.requests()[0].messages, false);
        let closed_turn = format!(
            "<|turn>model\nLet me check.{call_and_response}done<turn|>\n\
             <|turn>user\nq<turn|>\n<|turn>model\n<|channel>thought\n<channel|>"
        );
        assert!(later.ends_with(&closed_turn), "{later}");
        assert_eq!(later.matches("<|turn>model\n").count(), 2, "{later}");
    }

    #[tokio::test]
    async fn thinking_preamble_reasoning_stays_hidden_and_turn_open() {
        let mut f = Fixture::new();
        let step1 = "<|channel>thought\nR-secret reasoning<channel|>Let me check.";
        let link = FakeLink::new(
            true,
            vec![
                Fixture::step(step1, vec![Fixture::echo_call("call_1")]),
                Fixture::step("done", vec![]),
            ],
        );
        assert_eq!(
            f.run_thinking(link.clone()).await.unwrap().reason,
            EndReason::Answered
        );
        let step2 = f.render(&link.requests()[1].messages, true);
        assert!(!step2.contains("R-secret"), "{step2}");
        assert!(!step2.contains("Let me check.<turn|>"), "{step2}");
        let open_turn = concat!(
            "<|turn>user\nq<turn|>\n<|turn>model\nLet me check.",
            "<|tool_call>call:echo{text:<|\"|>x<|\"|>}<tool_call|>",
            "<|tool_response>response:echo{text:<|\"|>x<|\"|>}<tool_response|>",
            "<|channel>thought\n",
        );
        assert!(step2.ends_with(open_turn), "{step2}");
    }

    #[tokio::test]
    async fn link_and_props_are_resolved_per_step() {
        let mut f = Fixture::new();
        let first = FakeLink::new(true, vec![Fixture::step("", vec![Fixture::echo_call("c")])]);
        let second = FakeLink::new(false, vec![Fixture::step("done", vec![])]);
        let connector = FakeConnector::sequence(vec![Some(first.clone()), Some(second.clone())]);
        let end = f.run_with(&connector).await.unwrap();
        assert_eq!(
            end,
            LoopEnd {
                steps: 2,
                reason: EndReason::Answered
            }
        );
        assert_eq!(connector.connects(), 2);
        assert_eq!(first.requests().len(), 1);
        assert!(!first.requests()[0].tools.is_empty());
        assert_eq!(second.requests().len(), 1);
        assert!(second.requests()[0].tools.is_empty());
        assert_eq!(second.requests()[0].messages.len(), 3);
    }

    #[tokio::test]
    async fn harness_gone_mid_loop_is_error() {
        let mut f = Fixture::new();
        let first = FakeLink::new(true, vec![Fixture::step("", vec![Fixture::echo_call("c")])]);
        let connector = FakeConnector::sequence(vec![Some(first), None]);
        let end = f.run_with(&connector).await.unwrap();
        assert_eq!(
            end,
            LoopEnd {
                steps: 1,
                reason: EndReason::Error
            }
        );
        let at = f.sink.notes.len() - 2;
        assert_eq!(f.sink.notes[at].method, "turn_error");
        assert_eq!(f.sink.notes[at].params["message"], "no model running");
        assert_eq!(f.last().params, json!({"steps": 1, "reason": "error"}));
        assert!(f.transcript.is_empty());
    }

    #[tokio::test]
    async fn model_props_failure_mid_loop_is_error() {
        let mut f = Fixture::new();
        let first = FakeLink::new(true, vec![Fixture::step("", vec![Fixture::echo_call("c")])]);
        let connector =
            FakeConnector::sequence(vec![Some(first), Some(FakeLink::failing_props("gone"))]);
        let end = f.run_with(&connector).await.unwrap();
        assert_eq!(end.reason, EndReason::Error);
        assert_eq!(end.steps, 1);
        let at = f.sink.notes.len() - 2;
        assert_eq!(f.sink.notes[at].params["message"], "gone");
    }

    #[tokio::test]
    async fn two_calls_in_one_step_keep_order() {
        let mut f = Fixture::new();
        let calls = vec![
            Notes::call("call_1", "echo", json!({"text": "a"})),
            Notes::call("call_2", "echo", json!({"text": "b"})),
        ];
        let link = FakeLink::new(
            true,
            vec![Fixture::step("", calls), Fixture::step("done", vec![])],
        );
        f.run(Some(link)).await.unwrap();
        let results = f.tool_results();
        let contents: Vec<&Value> = results.iter().map(|n| &n.params["content"]).collect();
        assert_eq!(contents, [&json!({"text": "a"}), &json!({"text": "b"})]);
        let t = &f.transcript;
        assert_eq!(t.len(), 5);
        assert_eq!(t[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(t[2].tool_result, Some(json!({"text": "a"})));
        assert_eq!(t[3].tool_call_id.as_deref(), Some("call_2"));
        assert_eq!(t[3].tool_result, Some(json!({"text": "b"})));
    }

    #[tokio::test]
    async fn non_object_arguments_are_committed_as_empty_object() {
        let mut f = Fixture::new();
        let call = Notes::call("call_1", "echo", json!([1]));
        let link = FakeLink::new(
            true,
            vec![Fixture::step("", vec![call]), Fixture::step("ok", vec![])],
        );
        f.run(Some(link)).await.unwrap();
        let args = &f.transcript[1].tool_calls.as_ref().unwrap()[0]
            .function
            .arguments;
        assert_eq!(args, &json!({}));
    }

    #[tokio::test]
    async fn missing_supports_tools_means_no_tools() {
        let mut f = Fixture::new();
        let link = FakeLink::with_props(json!({}), vec![Fixture::step("hi", vec![])]);
        f.run(Some(link.clone())).await.unwrap();
        assert!(link.requests()[0].tools.is_empty());
    }

    #[tokio::test]
    async fn malformed_turn_done_calls_is_error() {
        let mut f = Fixture::new();
        let bad = Notes::note("turn_done", json!({"content": "", "tool_calls": "nope"}));
        let link = FakeLink::new(true, vec![Notes::script(vec![Notes::started(), bad])]);
        let end = f.run(Some(link)).await.unwrap();
        assert_eq!(end.reason, EndReason::Error);
        let at = f.sink.notes.len() - 2;
        assert_eq!(f.sink.notes[at].method, "turn_error");
        assert_eq!(f.sink.notes[at].params["message"], ToolLoop::BAD_TURN_DONE);
        assert!(f.transcript.is_empty());
    }

    #[tokio::test]
    async fn sink_closed_at_loop_done_leaves_transcript() {
        let mut f = Fixture::new();
        // started, chunk, done accepted; loop_done refused.
        f.sink = RecordingSink::closing_after(3);
        let link = FakeLink::new(true, vec![Fixture::step("hi", vec![])]);
        assert_eq!(f.run(Some(link)).await, Err(SinkClosed));
        assert!(f.transcript.is_empty());
    }

    #[tokio::test]
    async fn sink_closed_at_failure_turn_error_leaves_transcript() {
        let mut f = Fixture::new();
        // started, chunk accepted; the synthesized turn_error is refused.
        f.sink = RecordingSink::closing_after(2);
        let link = FakeLink::new(
            true,
            vec![Notes::script(vec![Notes::started(), Notes::chunk()])],
        );
        assert_eq!(f.run(Some(link)).await, Err(SinkClosed));
        assert!(f.transcript.is_empty());
    }

    #[tokio::test]
    async fn step_budget_comes_from_config() {
        let mut f = Fixture::new();
        f.config.step_budget = 2;
        let link = FakeLink::repeating(true, Fixture::step("", vec![Fixture::echo_call("c")]));
        let end = f.run(Some(link.clone())).await.unwrap();
        assert_eq!(
            end,
            LoopEnd {
                steps: 2,
                reason: EndReason::StepLimit
            }
        );
        assert_eq!(link.requests().len(), 2);
    }

    #[tokio::test]
    async fn only_enabled_tools_are_sent() {
        let sources = vec![TestSource::echo_boxed(), TestSource::writer_boxed()];
        let mut f = Fixture::with_sources(sources, &["echo"]);
        let link = FakeLink::new(true, vec![Fixture::step("hi", vec![])]);
        f.run(Some(link.clone())).await.unwrap();
        let requests = link.requests();
        let names: Vec<&str> = requests[0]
            .tools
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["echo"]);
    }

    #[tokio::test]
    async fn call_outside_subset_is_refused() {
        let sources = vec![TestSource::echo_boxed(), TestSource::writer_boxed()];
        let mut f = Fixture::with_sources(sources, &["echo"]);
        let call = Notes::call("c", "writer", json!({}));
        let link = FakeLink::new(
            true,
            vec![Fixture::step("", vec![call]), Fixture::step("ok", vec![])],
        );
        f.run(Some(link)).await.unwrap();
        let params = &f.tool_results()[0].params;
        assert_eq!(params["ok"], false);
        assert_eq!(
            params["content"],
            json!({"error": "tool not enabled for this agent: writer"})
        );
    }

    #[tokio::test]
    async fn system_prompt_renders_in_system_turn() {
        let mut f = Fixture::new();
        f.transcript = vec![ChatMessage::new(Role::System, "You are terse.")];
        let link = FakeLink::new(true, vec![Fixture::step("hi", vec![])]);
        f.run(Some(link.clone())).await.unwrap();
        let rendered = f.render(&link.requests()[0].messages, false);
        // The prompt opens the system turn, ahead of the tool declarations, which close it.
        let system_turn = "<|turn>system
You are terse.<|tool>declaration:echo{";
        assert!(rendered.starts_with(system_turn), "{rendered}");
        let prompt_at = rendered.find("You are terse.").expect("prompt");
        let tool_end = rendered.find("<tool|><turn|>").expect("system turn end");
        let user_at = rendered.find("<|turn>user").expect("user turn");
        assert!(prompt_at < tool_end && tool_end < user_at, "{rendered}");
    }

    #[tokio::test]
    async fn run_returns_transcript_only_when_answered() {
        let f = Fixture::new();
        let answering = FakeLink::new(true, vec![Fixture::step("hi", vec![])]);
        let links = FakeConnector::new(Some(answering));
        let mut sink = RecordingSink::new();
        let (end, committed) = f
            .tool_loop
            .run(&[], "q", &f.config, &links, &mut sink)
            .await
            .unwrap();
        assert_eq!(end.reason, EndReason::Answered);
        let committed = committed.expect("answered returns the transcript");
        assert_eq!(committed.len(), 2);
        assert_eq!(committed[1].content, "hi");

        let looping = FakeLink::repeating(true, Fixture::step("", vec![Fixture::echo_call("c")]));
        let links = FakeConnector::new(Some(looping));
        let (end, committed) = f
            .tool_loop
            .run(&[], "q", &f.config, &links, &mut RecordingSink::new())
            .await
            .unwrap();
        assert_eq!(end.reason, EndReason::StepLimit);
        assert!(committed.is_none());

        let (end, committed) = f
            .tool_loop
            .run(
                &[],
                "q",
                &f.config,
                &FakeConnector::new(None),
                &mut RecordingSink::new(),
            )
            .await
            .unwrap();
        assert_eq!(end.reason, EndReason::NoHarness);
        assert!(committed.is_none());
    }
}
