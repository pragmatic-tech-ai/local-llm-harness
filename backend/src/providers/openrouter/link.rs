//! `OpenRouterLink`: the [`WorkerLink`] a session's steps run through on OpenRouter.
//!
//! `generate` posts the request in OpenAI form and maps the event stream onto the worker's
//! notes, so the tool loop runs unchanged: `turn_started` (with an estimated prompt size), a
//! `turn_chunk` per content or reasoning delta, a `tool_call` per call once the stream ended
//! (as the worker relays them), then `turn_done {content, tool_calls, usage}`; or
//! `turn_rejected` when the prompt is too long, or `turn_error`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::{json, Map, Value};

use crate::engine::tool_format::ParsedCall;
use crate::engine::{ChatMessage, Role};
use crate::providers::openrouter::api::{ApiError, OpenRouterApi, OpenRouterModel, SseStream};
use crate::runtime::worker::{GenerateRequest, LinkError, Note, PromptSize, WorkerLink};

/// A model's prompt-size calibration: how many real tokens one estimated token turned out to
/// be. Shared by every link (and so every session) on one repository and model.
#[derive(Debug)]
pub struct Calibration {
    ratio: Mutex<f64>,
}

impl Default for Calibration {
    fn default() -> Self {
        Calibration {
            ratio: Mutex::new(1.0),
        }
    }
}

impl Calibration {
    const CHARS_PER_TOKEN: usize = 4;

    pub fn new() -> Self {
        Self::default()
    }

    /// The uncalibrated estimate: one token per four characters.
    pub fn base(raw_chars: usize) -> u32 {
        u32::try_from(raw_chars / Self::CHARS_PER_TOKEN).unwrap_or(u32::MAX)
    }

    /// The token estimate of a prompt of `raw_chars` characters, scaled by the last
    /// observation.
    pub fn estimate(&self, raw_chars: usize) -> u32 {
        let ratio = *self.ratio.lock().unwrap_or_else(|e| e.into_inner());
        let scaled = (f64::from(Self::base(raw_chars)) * ratio).round();
        scaled.min(f64::from(u32::MAX)) as u32
    }

    /// Records that a prompt whose uncalibrated estimate ([`Self::base`]) was `estimate`
    /// really had `actual` tokens. Zeros carry no information and are ignored.
    pub fn observe(&self, estimate: u32, actual: u32) {
        if estimate == 0 || actual == 0 {
            return;
        }
        *self.ratio.lock().unwrap_or_else(|e| e.into_inner()) =
            f64::from(actual) / f64::from(estimate);
    }
}

/// A link to one OpenRouter model with a fixed context window (the compaction window).
pub struct OpenRouterLink {
    api: Arc<OpenRouterApi>,
    model: OpenRouterModel,
    n_ctx: u32,
    calibration: Arc<Calibration>,
}

impl OpenRouterLink {
    const NOT_SUPPORTED: &'static str = "not supported by this provider";
    const F_MESSAGES: &'static str = "messages";
    const F_TOOLS: &'static str = "tools";
    const F_SEED: &'static str = "seed";
    const F_MAX_TOKENS: &'static str = "max_tokens";
    const F_REASONING: &'static str = "reasoning";
    const KIND_FUNCTION: &'static str = "function";
    /// The sampling value meaning "not set" for `seed` and `max_tokens`.
    const UNSET: i64 = -1;

    pub fn new(
        api: Arc<OpenRouterApi>,
        model: OpenRouterModel,
        n_ctx: u32,
        calibration: Arc<Calibration>,
    ) -> Self {
        OpenRouterLink {
            api,
            model,
            n_ctx,
            calibration,
        }
    }

    /// The `/chat/completions` body for `req`.
    fn body(&self, req: &GenerateRequest) -> Value {
        let sampling = &req.sampling;
        let mut body = Map::new();
        body.insert("model".into(), json!(self.model.id));
        body.insert(
            Self::F_MESSAGES.into(),
            Value::Array(req.messages.iter().map(Self::wire_message).collect()),
        );
        if !req.tools.is_empty() {
            body.insert(Self::F_TOOLS.into(), Value::Array(req.tools.clone()));
        }
        body.insert("stream".into(), json!(true));
        body.insert("usage".into(), json!({"include": true}));
        body.insert("temperature".into(), Self::wire_f32(sampling.temperature));
        body.insert("top_p".into(), Self::wire_f32(sampling.top_p));
        body.insert("top_k".into(), json!(sampling.top_k));
        body.insert(
            "repetition_penalty".into(),
            Self::wire_f32(sampling.repeat_penalty),
        );
        if sampling.seed != Self::UNSET {
            body.insert(Self::F_SEED.into(), json!(sampling.seed));
        }
        if i64::from(sampling.max_tokens) != Self::UNSET {
            body.insert(Self::F_MAX_TOKENS.into(), json!(sampling.max_tokens));
        }
        if req.enable_thinking && self.model.reasoning {
            body.insert(Self::F_REASONING.into(), json!({"enabled": true}));
        }
        Value::Object(body)
    }

    /// One of our messages in OpenAI form. Assistant tool-call arguments and tool results
    /// travel as JSON strings.
    fn wire_message(message: &ChatMessage) -> Value {
        match message.role {
            Role::System | Role::User => {
                json!({"role": message.role, "content": message.content})
            }
            Role::Assistant => {
                let calls: Vec<Value> = message
                    .tool_calls
                    .iter()
                    .flatten()
                    .map(|call| {
                        json!({
                            "id": call.id,
                            "type": Self::KIND_FUNCTION,
                            "function": {
                                "name": call.function.name,
                                "arguments": call.function.arguments.to_string(),
                            }
                        })
                    })
                    .collect();
                if calls.is_empty() {
                    return json!({"role": message.role, "content": message.content});
                }
                let content = if message.content.is_empty() {
                    Value::Null
                } else {
                    json!(message.content)
                };
                json!({"role": message.role, "content": content, "tool_calls": calls})
            }
            Role::Tool => {
                let content = match &message.tool_result {
                    Some(result) => result.to_string(),
                    None => message.content.clone(),
                };
                json!({
                    "role": message.role,
                    "tool_call_id": message.tool_call_id.clone().unwrap_or_default(),
                    "content": content,
                })
            }
        }
    }

    /// An `f32` setting as the decimal it was written as (`1.1`, not `1.100000023841858`).
    fn wire_f32(value: f32) -> Value {
        value
            .to_string()
            .parse::<f64>()
            .map(Value::from)
            .unwrap_or(Value::Null)
    }

    /// The characters of the serialized messages and tools: what the estimate counts.
    fn prompt_chars(body: &Value) -> usize {
        [Self::F_MESSAGES, Self::F_TOOLS]
            .iter()
            .filter_map(|field| body.get(*field))
            .map(|value| value.to_string().chars().count())
            .sum()
    }
}

#[async_trait]
impl WorkerLink for OpenRouterLink {
    async fn model_props(&self) -> Result<Value, LinkError> {
        Ok(json!({"n_ctx": self.n_ctx, "model_name": self.model.id, "supports_tools": true}))
    }

    async fn tokenize(&self, _text: &str) -> Result<Value, LinkError> {
        Err(LinkError(Self::NOT_SUPPORTED.to_string()))
    }

    /// The calibrated estimate; never fails.
    async fn measure_prompt(&self, req: &GenerateRequest) -> Result<PromptSize, LinkError> {
        let chars = Self::prompt_chars(&self.body(req));
        Ok(PromptSize {
            prompt_tokens: self.calibration.estimate(chars) as usize,
            n_ctx: self.n_ctx as usize,
        })
    }

    /// Never fails itself: every failure arrives as a `turn_rejected` or `turn_error` note
    /// after `turn_started`. The request is sent when the stream is first polled; dropping the
    /// stream drops the connection.
    async fn generate(
        &self,
        req: &GenerateRequest,
    ) -> Result<BoxStream<'static, Result<Note, LinkError>>, LinkError> {
        let body = self.body(req);
        let chars = Self::prompt_chars(&body);
        let turn = Turn::new(
            Arc::clone(&self.api),
            body,
            self.n_ctx,
            Calibration::base(chars),
            self.calibration.estimate(chars),
            Arc::clone(&self.calibration),
        );
        Ok(futures::stream::unfold(turn, Turn::step).boxed())
    }
}

/// Where a turn's HTTP call stands.
enum Phase {
    /// Not sent yet; holds the body.
    Connect(Value),
    Streaming(SseStream),
    Finished,
}

/// One tool call being assembled from its deltas.
#[derive(Default)]
struct CallDraft {
    id: String,
    name: String,
    arguments: String,
}

impl CallDraft {
    const F_ID: &'static str = "id";
    const F_FUNCTION: &'static str = "function";
    const F_NAME: &'static str = "name";
    const F_ARGUMENTS: &'static str = "arguments";
    const CALL_ID_PREFIX: &'static str = "call_";

    /// Folds in one `tool_calls[]` delta: the id and name are taken when first sent, argument
    /// fragments are appended.
    fn absorb(&mut self, delta: &Value) {
        if let Some(id) = delta.get(Self::F_ID).and_then(Value::as_str) {
            if self.id.is_empty() {
                self.id = id.to_string();
            }
        }
        let Some(function) = delta.get(Self::F_FUNCTION) else {
            return;
        };
        if let Some(name) = function.get(Self::F_NAME).and_then(Value::as_str) {
            if self.name.is_empty() {
                self.name = name.to_string();
            }
        }
        if let Some(fragment) = function.get(Self::F_ARGUMENTS).and_then(Value::as_str) {
            self.arguments.push_str(fragment);
        }
    }

    /// The finished call at zero-based `index`, in the worker's parsed-call shape. Empty
    /// arguments are `{}`; arguments that are not JSON keep their text as a string, with
    /// `parse_error` set.
    fn into_call(self, index: u64) -> ParsedCall {
        let id = if self.id.is_empty() {
            format!("{}{}", Self::CALL_ID_PREFIX, index + 1)
        } else {
            self.id
        };
        let (arguments, parse_error) = if self.arguments.trim().is_empty() {
            (Value::Object(Map::new()), None)
        } else {
            match serde_json::from_str::<Value>(&self.arguments) {
                Ok(value) => (value, None),
                Err(e) => (Value::String(self.arguments.clone()), Some(e.to_string())),
            }
        };
        ParsedCall {
            id,
            name: self.name,
            arguments,
            raw: self.arguments,
            parse_error,
        }
    }
}

/// The state of one `generate` stream.
struct Turn {
    api: Arc<OpenRouterApi>,
    phase: Phase,
    pending: VecDeque<Note>,
    content: String,
    calls: BTreeMap<u64, CallDraft>,
    usage: Option<Value>,
    finish_seen: bool,
    n_ctx: u32,
    /// The uncalibrated estimate of this prompt, which the usage's real count calibrates.
    base_estimate: u32,
    calibration: Arc<Calibration>,
}

impl Turn {
    const M_TURN_STARTED: &'static str = "turn_started";
    const M_TURN_CHUNK: &'static str = "turn_chunk";
    const M_TOOL_CALL: &'static str = "tool_call";
    const M_TURN_DONE: &'static str = "turn_done";
    const M_TURN_REJECTED: &'static str = "turn_rejected";
    const M_TURN_ERROR: &'static str = "turn_error";
    const F_ERROR: &'static str = "error";
    const F_USAGE: &'static str = "usage";
    const F_CHOICES: &'static str = "choices";
    const F_DELTA: &'static str = "delta";
    const F_CONTENT: &'static str = "content";
    const F_REASONING: &'static str = "reasoning";
    const F_TOOL_CALLS: &'static str = "tool_calls";
    const F_INDEX: &'static str = "index";
    const F_FINISH_REASON: &'static str = "finish_reason";
    const F_PROMPT_TOKENS: &'static str = "prompt_tokens";
    const F_COMPLETION_TOKENS: &'static str = "completion_tokens";
    const F_COMPLETION_DETAILS: &'static str = "completion_tokens_details";
    const F_REASONING_TOKENS: &'static str = "reasoning_tokens";
    const F_COST: &'static str = "cost";
    const STREAM_CUT: &'static str = "stream ended before the answer finished";

    fn new(
        api: Arc<OpenRouterApi>,
        body: Value,
        n_ctx: u32,
        base_estimate: u32,
        estimate: u32,
        calibration: Arc<Calibration>,
    ) -> Self {
        let started = Self::note(
            Self::M_TURN_STARTED,
            json!({"prompt": null, "prompt_tokens": estimate, "n_ctx": n_ctx, "estimated": true}),
        );
        Turn {
            api,
            phase: Phase::Connect(body),
            pending: VecDeque::from([started]),
            content: String::new(),
            calls: BTreeMap::new(),
            usage: None,
            finish_seen: false,
            n_ctx,
            base_estimate,
            calibration,
        }
    }

    fn note(method: &str, params: Value) -> Note {
        Note {
            method: method.to_string(),
            params,
        }
    }

    /// The next note, or `None` once the turn's last note was handed out.
    async fn step(mut self) -> Option<(Result<Note, LinkError>, Self)> {
        loop {
            if let Some(note) = self.pending.pop_front() {
                return Some((Ok(note), self));
            }
            match std::mem::replace(&mut self.phase, Phase::Finished) {
                Phase::Finished => return None,
                Phase::Connect(body) => match self.api.stream_chat(body).await {
                    Ok(stream) => self.phase = Phase::Streaming(stream),
                    Err(e) => self.fail(e),
                },
                Phase::Streaming(mut stream) => match stream.next_event().await {
                    Some(Ok(event)) => {
                        if self.absorb(&event) {
                            self.phase = Phase::Streaming(stream);
                        }
                    }
                    Some(Err(e)) => self.fail(e),
                    None => self.finish(stream.saw_done()),
                },
            }
        }
    }

    /// Folds in one stream event, queueing its notes. Returns false when the event ended the
    /// turn (an error object).
    fn absorb(&mut self, event: &Value) -> bool {
        if let Some(error) = event.get(Self::F_ERROR) {
            self.fail(ApiError::of_stream_object(error, self.api.key()));
            return false;
        }
        if let Some(usage) = event.get(Self::F_USAGE).filter(|u| u.is_object()) {
            self.take_usage(usage);
        }
        let Some(choice) = event
            .get(Self::F_CHOICES)
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return true;
        };
        if let Some(delta) = choice.get(Self::F_DELTA) {
            if let Some(text) = Self::text(delta, Self::F_REASONING) {
                self.pending.push_back(Self::chunk(text, true));
            }
            if let Some(text) = Self::text(delta, Self::F_CONTENT) {
                self.content.push_str(text);
                self.pending.push_back(Self::chunk(text, false));
            }
            if let Some(calls) = delta.get(Self::F_TOOL_CALLS).and_then(Value::as_array) {
                for (position, call) in calls.iter().enumerate() {
                    let index = call
                        .get(Self::F_INDEX)
                        .and_then(Value::as_u64)
                        .unwrap_or(position as u64);
                    self.calls.entry(index).or_default().absorb(call);
                }
            }
        }
        if choice
            .get(Self::F_FINISH_REASON)
            .is_some_and(|reason| !reason.is_null())
        {
            self.finish_seen = true;
        }
        true
    }

    /// A non-empty string field of a delta.
    fn text<'a>(delta: &'a Value, field: &str) -> Option<&'a str> {
        delta
            .get(field)
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
    }

    fn chunk(text: &str, reasoning: bool) -> Note {
        let mut params = json!({"text": text, "tokens": [], "stop": false});
        if reasoning {
            params[Self::F_REASONING] = json!(true);
        }
        Self::note(Self::M_TURN_CHUNK, params)
    }

    /// Keeps the usage for `turn_done` and calibrates the model's estimate with it.
    fn take_usage(&mut self, usage: &Value) {
        let count = |value: Option<&Value>| value.and_then(Value::as_u64).unwrap_or(0);
        let prompt = count(usage.get(Self::F_PROMPT_TOKENS));
        let reasoning = count(
            usage
                .get(Self::F_COMPLETION_DETAILS)
                .and_then(|d| d.get(Self::F_REASONING_TOKENS)),
        );
        self.usage = Some(json!({
            "prompt_tokens": prompt,
            "completion_tokens": count(usage.get(Self::F_COMPLETION_TOKENS)),
            "reasoning_tokens": reasoning,
            "cost": usage.get(Self::F_COST).cloned().unwrap_or(Value::Null),
        }));
        self.calibration.observe(
            self.base_estimate,
            u32::try_from(prompt).unwrap_or(u32::MAX),
        );
    }

    /// Ends the turn with `error`: a context-length refusal is `turn_rejected` (so the turn
    /// ends `window_full`), anything else `turn_error`.
    fn fail(&mut self, error: ApiError) {
        self.phase = Phase::Finished;
        let note = if error.is_context_length() {
            Self::note(
                Self::M_TURN_REJECTED,
                json!({"needed": 0, "total": self.n_ctx}),
            )
        } else {
            Self::note(Self::M_TURN_ERROR, json!({"message": error.to_string()}))
        };
        self.pending.push_back(note);
    }

    /// The stream ended: the calls (in index order) and `turn_done`, or a `turn_error` when it
    /// was cut before `[DONE]` or a finish reason.
    fn finish(&mut self, saw_done: bool) {
        if !saw_done && !self.finish_seen {
            self.fail(ApiError::Transport(Self::STREAM_CUT.to_string()));
            return;
        }
        let calls: Vec<ParsedCall> = std::mem::take(&mut self.calls)
            .into_iter()
            .map(|(index, draft)| draft.into_call(index))
            .collect();
        for call in &calls {
            let params = serde_json::to_value(call).unwrap_or(Value::Null);
            self.pending
                .push_back(Self::note(Self::M_TOOL_CALL, params));
        }
        let mut done = json!({"content": self.content, "tool_calls": calls});
        if let Some(usage) = self.usage.take() {
            done[Self::F_USAGE] = usage;
        }
        self.pending.push_back(Self::note(Self::M_TURN_DONE, done));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::types::{FunctionCall, ToolCall};
    use crate::engine::SamplingConfig;
    use crate::providers::openrouter::{Canned, Captured, FakeHttp};

    struct Fixtures;

    impl Fixtures {
        const KEY: &'static str = "sk-or-test-secret-1234";
        const N_CTX: u32 = 4096;

        fn model(reasoning: bool) -> OpenRouterModel {
            OpenRouterModel {
                id: "vendor/model-a".into(),
                name: "Model A".into(),
                context_length: 32768,
                prompt_price: 1.0,
                completion_price: 2.0,
                reasoning,
            }
        }

        fn request(enable_thinking: bool) -> GenerateRequest {
            let mut assistant = ChatMessage::new(Role::Assistant, "");
            assistant.tool_calls = Some(vec![ToolCall {
                id: "call_1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: json!({"path": "a.txt"}),
                },
            }]);
            let mut tool = ChatMessage::new(Role::Tool, "");
            tool.tool_call_id = Some("call_1".into());
            tool.tool_result = Some(json!({"text": "hi"}));
            GenerateRequest {
                messages: vec![
                    ChatMessage::new(Role::System, "be brief"),
                    ChatMessage::new(Role::User, "read a.txt"),
                    assistant,
                    tool,
                ],
                tools: vec![json!({"type": "function", "function": {"name": "read_file"}})],
                enable_thinking,
                sampling: SamplingConfig::default(),
            }
        }

        /// An SSE body: one `data:` event per value, then `[DONE]`.
        fn sse(events: &[serde_json::Value]) -> Vec<String> {
            let mut parts: Vec<String> = events.iter().map(|e| format!("data: {e}\n\n")).collect();
            parts.push("data: [DONE]\n\n".into());
            parts
        }

        fn delta(delta: serde_json::Value) -> serde_json::Value {
            json!({"choices": [{"index": 0, "delta": delta, "finish_reason": null}]})
        }

        fn finish(reason: &str) -> serde_json::Value {
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]})
        }

        fn usage(prompt: u64, completion: u64, reasoning: u64, cost: f64) -> serde_json::Value {
            json!({"choices": [], "usage": {
                "prompt_tokens": prompt,
                "completion_tokens": completion,
                "total_tokens": prompt + completion,
                "cost": cost,
                "completion_tokens_details": {"reasoning_tokens": reasoning}
            }})
        }

        fn canned(status: u16, parts: Vec<String>) -> Canned {
            Canned { status, parts }
        }

        async fn link_with(
            responses: Vec<Canned>,
            reasoning: bool,
            calibration: Arc<Calibration>,
        ) -> (
            OpenRouterLink,
            tokio::sync::mpsc::UnboundedReceiver<Captured>,
        ) {
            let (base, requests) = FakeHttp::serve(responses).await;
            let api = Arc::new(OpenRouterApi::new(base, Some(Self::KEY.into())));
            let link = OpenRouterLink::new(api, Self::model(reasoning), Self::N_CTX, calibration);
            (link, requests)
        }

        async fn link(
            responses: Vec<Canned>,
        ) -> (
            OpenRouterLink,
            tokio::sync::mpsc::UnboundedReceiver<Captured>,
        ) {
            Self::link_with(responses, false, Arc::new(Calibration::new())).await
        }

        async fn notes(link: &OpenRouterLink, req: &GenerateRequest) -> Vec<Note> {
            let stream = link.generate(req).await.expect("generate");
            stream.map(|n| n.expect("note")).collect().await
        }

        fn methods(notes: &[Note]) -> Vec<&str> {
            notes.iter().map(|n| n.method.as_str()).collect()
        }

        fn ok_stream() -> Canned {
            Self::canned(
                200,
                Self::sse(&[Self::delta(json!({"content": "ok"})), Self::finish("stop")]),
            )
        }
    }

    #[tokio::test]
    async fn request_body_maps_messages_tools_sampling_and_headers() {
        let (link, mut requests) =
            Fixtures::link(vec![Fixtures::ok_stream(), Fixtures::ok_stream()]).await;
        let req = Fixtures::request(false);
        Fixtures::notes(&link, &req).await;
        let captured = requests.recv().await.expect("request");
        assert!(
            captured.head.starts_with("POST /chat/completions "),
            "{}",
            captured.head
        );
        assert_eq!(
            captured.header("authorization").as_deref(),
            Some("Bearer sk-or-test-secret-1234")
        );
        assert_eq!(
            captured.header("x-title").as_deref(),
            Some("Pragmatic LLM Harness")
        );
        assert_eq!(captured.header("http-referer"), None);
        let body = captured.json();
        assert_eq!(body["model"], "vendor/model-a");
        assert_eq!(body["stream"], true);
        assert_eq!(body["usage"], json!({"include": true}));
        assert_eq!(
            body["messages"],
            json!([
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "read a.txt"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "read_file", "arguments": "{\"path\":\"a.txt\"}"}
                }]},
                {"role": "tool", "tool_call_id": "call_1", "content": "{\"text\":\"hi\"}"}
            ])
        );
        assert_eq!(body["tools"], json!(req.tools));
        assert_eq!(body["temperature"], json!(0.8));
        assert_eq!(body["top_p"], json!(0.95));
        assert_eq!(body["top_k"], json!(40));
        assert_eq!(body["repetition_penalty"], json!(1.1));
        assert!(body.get("repeat_penalty").is_none());
        assert!(body.get("seed").is_none(), "{body}");
        assert!(body.get("max_tokens").is_none(), "{body}");
        assert!(body.get("reasoning").is_none(), "{body}");

        let mut seeded = Fixtures::request(false);
        seeded.sampling.seed = 7;
        seeded.sampling.max_tokens = 100;
        Fixtures::notes(&link, &seeded).await;
        let body = requests.recv().await.expect("request").json();
        assert_eq!(body["seed"], 7);
        assert_eq!(body["max_tokens"], 100);
    }

    #[tokio::test]
    async fn reasoning_sent_only_when_thinking_and_model_supports_it() {
        let cases = [
            (true, true, true),
            (true, false, false),
            (false, true, false),
        ];
        for (thinking, model_reasons, sent) in cases {
            let (link, mut requests) = Fixtures::link_with(
                vec![Fixtures::ok_stream()],
                model_reasons,
                Arc::new(Calibration::new()),
            )
            .await;
            Fixtures::notes(&link, &Fixtures::request(thinking)).await;
            let body = requests.recv().await.expect("request").json();
            if sent {
                assert_eq!(body["reasoning"], json!({"enabled": true}));
            } else {
                assert!(
                    body.get("reasoning").is_none(),
                    "{thinking} {model_reasons}"
                );
            }
        }
    }

    #[tokio::test]
    async fn content_stream_becomes_started_chunks_done_with_usage() {
        let parts = Fixtures::sse(&[
            Fixtures::delta(json!({"role": "assistant", "content": "Hel"})),
            Fixtures::delta(json!({"content": "lo"})),
            Fixtures::finish("stop"),
            Fixtures::usage(12, 3, 0, 0.00042),
        ]);
        let (link, _requests) = Fixtures::link(vec![Fixtures::canned(200, parts)]).await;
        let req = Fixtures::request(false);
        let estimate = link
            .measure_prompt(&req)
            .await
            .expect("measure")
            .prompt_tokens;
        let notes = Fixtures::notes(&link, &req).await;
        assert_eq!(
            Fixtures::methods(&notes),
            ["turn_started", "turn_chunk", "turn_chunk", "turn_done"]
        );
        assert_eq!(
            notes[0].params,
            json!({"prompt": null, "prompt_tokens": estimate, "n_ctx": 4096, "estimated": true})
        );
        assert_eq!(
            notes[1].params,
            json!({"text": "Hel", "tokens": [], "stop": false})
        );
        assert_eq!(notes[2].params["text"], "lo");
        assert_eq!(
            notes[3].params,
            json!({
                "content": "Hello",
                "tool_calls": [],
                "usage": {"prompt_tokens": 12, "completion_tokens": 3, "reasoning_tokens": 0, "cost": 0.00042}
            })
        );
    }

    #[tokio::test]
    async fn reasoning_deltas_are_flagged_chunks_not_content() {
        let parts = Fixtures::sse(&[
            Fixtures::delta(json!({"reasoning": "let me think"})),
            Fixtures::delta(json!({"content": "ok"})),
            Fixtures::finish("stop"),
            Fixtures::usage(10, 7, 5, 0.001),
        ]);
        let (link, _requests) = Fixtures::link(vec![Fixtures::canned(200, parts)]).await;
        let notes = Fixtures::notes(&link, &Fixtures::request(true)).await;
        assert_eq!(
            Fixtures::methods(&notes),
            ["turn_started", "turn_chunk", "turn_chunk", "turn_done"]
        );
        assert_eq!(
            notes[1].params,
            json!({"text": "let me think", "tokens": [], "stop": false, "reasoning": true})
        );
        assert_eq!(
            notes[2].params,
            json!({"text": "ok", "tokens": [], "stop": false})
        );
        assert_eq!(notes[3].params["content"], "ok");
        assert_eq!(notes[3].params["usage"]["reasoning_tokens"], 5);
    }

    #[tokio::test]
    async fn tool_call_deltas_accumulate_by_index() {
        let call = |index: u64, id: Option<&str>, name: Option<&str>, args: &str| {
            let mut function = json!({"arguments": args});
            if let Some(name) = name {
                function["name"] = json!(name);
            }
            let mut entry = json!({"index": index, "function": function});
            if let Some(id) = id {
                entry["id"] = json!(id);
                entry["type"] = json!("function");
            }
            Fixtures::delta(json!({"tool_calls": [entry]}))
        };
        let mut parts = Fixtures::sse(&[
            call(0, Some("t0"), Some("read_file"), ""),
            call(1, Some("t1"), Some("note_add"), "{\"te"),
            call(0, None, None, "{\"pa"),
            call(1, None, None, "xt\":\"x\"}"),
            call(0, None, None, "th\":\"a.txt\"}"),
            Fixtures::finish("tool_calls"),
        ]);
        // Split one event's data line across two network chunks as well.
        let third = parts.remove(2);
        let (head, tail) = third.split_at(third.len() / 2);
        parts.insert(2, tail.to_string());
        parts.insert(2, head.to_string());
        let (link, _requests) = Fixtures::link(vec![Fixtures::canned(200, parts)]).await;
        let notes = Fixtures::notes(&link, &Fixtures::request(false)).await;
        assert_eq!(
            Fixtures::methods(&notes),
            ["turn_started", "tool_call", "tool_call", "turn_done"]
        );
        let calls: Vec<ParsedCall> =
            serde_json::from_value(notes[3].params["tool_calls"].clone()).expect("calls");
        assert_eq!(
            calls,
            vec![
                ParsedCall {
                    id: "t0".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "a.txt"}),
                    raw: "{\"path\":\"a.txt\"}".into(),
                    parse_error: None,
                },
                ParsedCall {
                    id: "t1".into(),
                    name: "note_add".into(),
                    arguments: json!({"text": "x"}),
                    raw: "{\"text\":\"x\"}".into(),
                    parse_error: None,
                },
            ]
        );
        assert_eq!(notes[1].params, serde_json::to_value(&calls[0]).unwrap());
        assert_eq!(notes[2].params, serde_json::to_value(&calls[1]).unwrap());
        assert_eq!(notes[3].params["content"], "");
    }

    #[tokio::test]
    async fn invalid_tool_arguments_set_parse_error() {
        let entry = json!({"index": 0, "id": "t0", "type": "function",
            "function": {"name": "read_file", "arguments": "{\"path\": "}});
        let parts = Fixtures::sse(&[
            Fixtures::delta(json!({"tool_calls": [entry]})),
            Fixtures::finish("tool_calls"),
        ]);
        let (link, _requests) = Fixtures::link(vec![Fixtures::canned(200, parts)]).await;
        let notes = Fixtures::notes(&link, &Fixtures::request(false)).await;
        let done = notes.last().expect("done");
        assert_eq!(done.method, "turn_done");
        let calls: Vec<ParsedCall> =
            serde_json::from_value(done.params["tool_calls"].clone()).expect("calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].raw, "{\"path\": ");
        assert!(calls[0].parse_error.is_some());
    }

    #[tokio::test]
    async fn stream_cut_before_the_end_is_turn_error() {
        let parts = vec![format!(
            "data: {}\n\n",
            Fixtures::delta(json!({"content": "Hel"}))
        )];
        let (link, _requests) = Fixtures::link(vec![Fixtures::canned(200, parts)]).await;
        let notes = Fixtures::notes(&link, &Fixtures::request(false)).await;
        assert_eq!(
            Fixtures::methods(&notes),
            ["turn_started", "turn_chunk", "turn_error"]
        );
        assert_eq!(
            notes[2].params["message"],
            "openrouter: stream ended before the answer finished"
        );
    }

    #[tokio::test]
    async fn context_length_400_becomes_turn_rejected() {
        let body = json!({"error": {"code": 400, "message":
            "This endpoint's maximum context length is 8192 tokens. However, you requested about 9000 tokens."}});
        let (link, _requests) =
            Fixtures::link(vec![Fixtures::canned(400, vec![body.to_string()])]).await;
        let notes = Fixtures::notes(&link, &Fixtures::request(false)).await;
        assert_eq!(Fixtures::methods(&notes), ["turn_started", "turn_rejected"]);
        assert_eq!(notes[1].params, json!({"needed": 0, "total": 4096}));
    }

    #[tokio::test]
    async fn other_http_error_becomes_turn_error_with_status_and_message() {
        let limited = json!({"error": {"code": 429, "message": "Rate limit exceeded"}});
        let invalid =
            json!({"error": {"code": 400, "message": "vendor/model-a is not a valid model ID"}});
        let (link, _requests) = Fixtures::link(vec![
            Fixtures::canned(429, vec![limited.to_string()]),
            Fixtures::canned(400, vec![invalid.to_string()]),
            Fixtures::canned(502, vec!["<html>bad gateway</html>".into()]),
        ])
        .await;
        let expected = [
            "openrouter: 429 Rate limit exceeded",
            "openrouter: 400 vendor/model-a is not a valid model ID",
            "openrouter: 502 Bad Gateway",
        ];
        for message in expected {
            let notes = Fixtures::notes(&link, &Fixtures::request(false)).await;
            assert_eq!(Fixtures::methods(&notes), ["turn_started", "turn_error"]);
            assert_eq!(notes[1].params, json!({"message": message}));
        }
    }

    #[tokio::test]
    async fn errors_never_echo_the_key() {
        // A 401 whose message echoes the key back.
        let unauthorized = json!({"error": {"code": 401,
            "message": format!("Invalid credentials: Bearer {}", Fixtures::KEY)}});
        // A stream that breaks with an error object mid-way.
        let mid_stream = vec![
            format!("data: {}\n\n", Fixtures::delta(json!({"content": "Hel"}))),
            format!(
                "data: {}\n\n",
                json!({"error": {"code": "server_error", "message": "upstream died"},
                    "choices": [{"index": 0, "delta": {}, "finish_reason": "error"}]})
            ),
        ];
        let (link, _requests) = Fixtures::link(vec![
            Fixtures::canned(401, vec![unauthorized.to_string()]),
            Fixtures::canned(200, mid_stream),
        ])
        .await;
        let mut messages = Vec::new();
        for _ in 0..2 {
            let notes = Fixtures::notes(&link, &Fixtures::request(false)).await;
            let last = notes.last().expect("note");
            assert_eq!(last.method, "turn_error");
            messages.push(last.params["message"].as_str().unwrap().to_string());
        }
        assert!(
            messages[0].starts_with("openrouter: 401 "),
            "{}",
            messages[0]
        );
        assert_eq!(messages[1], "openrouter: server_error upstream died");

        // A connect failure: nothing listens on the port.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr").port()
        };
        let api = Arc::new(OpenRouterApi::new(
            format!("http://127.0.0.1:{port}"),
            Some(Fixtures::KEY.into()),
        ));
        let link = OpenRouterLink::new(
            api,
            Fixtures::model(false),
            Fixtures::N_CTX,
            Arc::new(Calibration::new()),
        );
        let notes = Fixtures::notes(&link, &Fixtures::request(false)).await;
        assert_eq!(Fixtures::methods(&notes), ["turn_started", "turn_error"]);
        let message = notes[1].params["message"].as_str().unwrap().to_string();
        assert!(message.starts_with("openrouter: "), "{message}");
        messages.push(message);

        for message in &messages {
            assert!(!message.contains(Fixtures::KEY), "{message}");
            assert!(!message.contains("sk-or-"), "{message}");
            assert!(!message.contains(&format!("127.0.0.1:{port}")), "{message}");
        }
    }

    #[tokio::test]
    async fn estimate_calibrates_after_usage() {
        let calibration = Calibration::new();
        assert_eq!(calibration.estimate(400), 100);
        calibration.observe(100, 200);
        assert_eq!(calibration.estimate(400), 200);

        // Through the link: the first answered step's usage rescales the next estimate.
        let shared = Arc::new(Calibration::new());
        let req = Fixtures::request(false);
        let probe = OpenRouterLink::new(
            Arc::new(OpenRouterApi::new("http://127.0.0.1:1".into(), None)),
            Fixtures::model(false),
            Fixtures::N_CTX,
            Arc::clone(&shared),
        );
        let first = probe.measure_prompt(&req).await.expect("measure");
        assert_eq!(first.n_ctx, 4096);
        let actual = first.prompt_tokens as u64 * 2;
        let parts = Fixtures::sse(&[
            Fixtures::delta(json!({"content": "ok"})),
            Fixtures::finish("stop"),
            Fixtures::usage(actual, 1, 0, 0.0),
        ]);
        let (link, _requests) = Fixtures::link_with(
            vec![Fixtures::canned(200, parts)],
            false,
            Arc::clone(&shared),
        )
        .await;
        Fixtures::notes(&link, &req).await;
        let second = link.measure_prompt(&req).await.expect("measure");
        assert_eq!(second.prompt_tokens as u64, actual);
    }

    #[tokio::test]
    async fn tokenize_is_not_supported() {
        let (link, _requests) = Fixtures::link(vec![]).await;
        let err = link.tokenize("hello").await.unwrap_err();
        assert_eq!(err.0, "not supported by this provider");
    }

    #[tokio::test]
    async fn model_props_report_window_name_and_tools() {
        let (link, _requests) = Fixtures::link(vec![]).await;
        assert_eq!(
            link.model_props().await.expect("props"),
            json!({"n_ctx": 4096, "model_name": "vendor/model-a", "supports_tools": true})
        );
    }
}
