//! `OpenRouterLink`: the [`WorkerLink`] a session's steps run through on OpenRouter.
//!
//! `generate` posts the request in OpenAI form and maps the event stream onto the worker's
//! notes, so the tool loop runs unchanged: `turn_started` (with an estimated prompt size), a
//! `turn_chunk` per content or reasoning delta, a `tool_call` per call once the stream ended
//! (as the worker relays them), then `turn_done {content, tool_calls, usage}`; or
//! `turn_rejected` when the prompt is too long, or `turn_error`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::{json, Map, Value};

use crate::engine::{ChatMessage, Role};
use crate::providers::openrouter::api::{OpenRouterApi, OpenRouterModel};
use crate::providers::openrouter::turn::Turn;
use crate::runtime::worker::{GenerateRequest, LinkError, Note, PromptSize, WorkerLink};

/// The wire keys the chat body and the notes share: one home each.
pub(super) struct Wire;

impl Wire {
    pub(super) const MODEL: &'static str = "model";
    pub(super) const MESSAGES: &'static str = "messages";
    pub(super) const TOOLS: &'static str = "tools";
    pub(super) const STREAM: &'static str = "stream";
    pub(super) const USAGE: &'static str = "usage";
    pub(super) const INCLUDE: &'static str = "include";
    pub(super) const TEMPERATURE: &'static str = "temperature";
    pub(super) const TOP_P: &'static str = "top_p";
    pub(super) const TOP_K: &'static str = "top_k";
    pub(super) const REPETITION_PENALTY: &'static str = "repetition_penalty";
    pub(super) const SEED: &'static str = "seed";
    pub(super) const MAX_TOKENS: &'static str = "max_tokens";
    pub(super) const REASONING: &'static str = "reasoning";
    pub(super) const ENABLED: &'static str = "enabled";
    pub(super) const ROLE: &'static str = "role";
    pub(super) const CONTENT: &'static str = "content";
    pub(super) const TOOL_CALLS: &'static str = "tool_calls";
    pub(super) const TOOL_CALL_ID: &'static str = "tool_call_id";
    pub(super) const ID: &'static str = "id";
    pub(super) const TYPE: &'static str = "type";
    pub(super) const FUNCTION: &'static str = "function";
    pub(super) const NAME: &'static str = "name";
    pub(super) const ARGUMENTS: &'static str = "arguments";
    pub(super) const N_CTX: &'static str = "n_ctx";
    pub(super) const MODEL_NAME: &'static str = "model_name";
    pub(super) const SUPPORTS_TOOLS: &'static str = "supports_tools";
    pub(super) const PROMPT_TOKENS: &'static str = "prompt_tokens";
    pub(super) const COMPLETION_TOKENS: &'static str = "completion_tokens";
    pub(super) const REASONING_TOKENS: &'static str = "reasoning_tokens";
    pub(super) const COST: &'static str = "cost";
    pub(super) const ESTIMATED: &'static str = "estimated";
}

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
        body.insert(Wire::MODEL.into(), json!(self.model.id));
        body.insert(
            Wire::MESSAGES.into(),
            Value::Array(req.messages.iter().map(Self::wire_message).collect()),
        );
        if !req.tools.is_empty() {
            body.insert(Wire::TOOLS.into(), Value::Array(req.tools.clone()));
        }
        body.insert(Wire::STREAM.into(), json!(true));
        body.insert(Wire::USAGE.into(), json!({(Wire::INCLUDE): true}));
        body.insert(
            Wire::TEMPERATURE.into(),
            Self::wire_f32(sampling.temperature),
        );
        body.insert(Wire::TOP_P.into(), Self::wire_f32(sampling.top_p));
        body.insert(Wire::TOP_K.into(), json!(sampling.top_k));
        body.insert(
            Wire::REPETITION_PENALTY.into(),
            Self::wire_f32(sampling.repeat_penalty),
        );
        if sampling.seed != Self::UNSET {
            body.insert(Wire::SEED.into(), json!(sampling.seed));
        }
        if i64::from(sampling.max_tokens) != Self::UNSET {
            body.insert(Wire::MAX_TOKENS.into(), json!(sampling.max_tokens));
        }
        if req.enable_thinking && self.model.reasoning {
            body.insert(Wire::REASONING.into(), json!({(Wire::ENABLED): true}));
        }
        Value::Object(body)
    }

    /// One of our messages in OpenAI form. Assistant tool-call arguments and tool results
    /// travel as JSON strings.
    fn wire_message(message: &ChatMessage) -> Value {
        match message.role {
            Role::System | Role::User => {
                json!({(Wire::ROLE): message.role, (Wire::CONTENT): message.content})
            }
            Role::Assistant => {
                let calls: Vec<Value> = message
                    .tool_calls
                    .iter()
                    .flatten()
                    .map(|call| {
                        json!({
                            (Wire::ID): call.id,
                            (Wire::TYPE): Self::KIND_FUNCTION,
                            (Wire::FUNCTION): {
                                (Wire::NAME): call.function.name,
                                (Wire::ARGUMENTS): call.function.arguments.to_string(),
                            }
                        })
                    })
                    .collect();
                if calls.is_empty() {
                    return json!({(Wire::ROLE): message.role, (Wire::CONTENT): message.content});
                }
                let content = if message.content.is_empty() {
                    Value::Null
                } else {
                    json!(message.content)
                };
                json!({(Wire::ROLE): message.role, (Wire::CONTENT): content, (Wire::TOOL_CALLS): calls})
            }
            Role::Tool => {
                let content = match &message.tool_result {
                    Some(result) => result.to_string(),
                    None => message.content.clone(),
                };
                json!({
                    (Wire::ROLE): message.role,
                    (Wire::TOOL_CALL_ID): message.tool_call_id.clone().unwrap_or_default(),
                    (Wire::CONTENT): content,
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
        [Wire::MESSAGES, Wire::TOOLS]
            .iter()
            .filter_map(|field| body.get(*field))
            .map(|value| value.to_string().chars().count())
            .sum()
    }
}

#[async_trait]
impl WorkerLink for OpenRouterLink {
    async fn model_props(&self) -> Result<Value, LinkError> {
        Ok(json!({
            (Wire::N_CTX): self.n_ctx,
            (Wire::MODEL_NAME): self.model.id,
            (Wire::SUPPORTS_TOOLS): true,
        }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tool_format::ParsedCall;
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
    async fn absent_usage_counts_are_null_and_do_not_calibrate() {
        let parts = Fixtures::sse(&[
            Fixtures::delta(json!({"content": "ok"})),
            Fixtures::finish("stop"),
            json!({"choices": [], "usage": {"cost": 0.002}}),
        ]);
        let calibration = Arc::new(Calibration::new());
        let (link, _requests) = Fixtures::link_with(
            vec![Fixtures::canned(200, parts)],
            false,
            Arc::clone(&calibration),
        )
        .await;
        let notes = Fixtures::notes(&link, &Fixtures::request(false)).await;
        assert_eq!(
            notes.last().expect("done").params["usage"],
            json!({"prompt_tokens": null, "completion_tokens": null, "reasoning_tokens": null, "cost": 0.002})
        );
        assert_eq!(calibration.estimate(400), 100);
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
        assert_eq!(calls[0].arguments, Value::Null);
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
        // A stream that breaks with an error object mid-way, echoing the key in its code and
        // its message.
        let mid_stream = vec![
            format!("data: {}\n\n", Fixtures::delta(json!({"content": "Hel"}))),
            format!(
                "data: {}\n\n",
                json!({"error": {"code": format!("bad_key_{}", Fixtures::KEY),
                        "message": format!("upstream died for Bearer {}", Fixtures::KEY)},
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
        assert_eq!(
            messages[1],
            "openrouter: bad_key_*** upstream died for Bearer ***"
        );

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
