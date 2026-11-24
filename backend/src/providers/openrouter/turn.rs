//! One `generate` stream on OpenRouter: the chat call's events folded into the worker's notes.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::engine::tool_format::ParsedCall;
use crate::providers::openrouter::api::{ApiError, OpenRouterApi, SseStream};
use crate::providers::openrouter::link::{Calibration, Wire};
use crate::runtime::worker::{LinkError, Note};

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
    const CALL_ID_PREFIX: &'static str = "call_";

    /// Folds in one `tool_calls[]` delta: the id and name are taken when first sent, argument
    /// fragments are appended.
    fn absorb(&mut self, delta: &Value) {
        if let Some(id) = delta.get(Wire::ID).and_then(Value::as_str) {
            if self.id.is_empty() {
                self.id = id.to_string();
            }
        }
        let Some(function) = delta.get(Wire::FUNCTION) else {
            return;
        };
        if let Some(name) = function.get(Wire::NAME).and_then(Value::as_str) {
            if self.name.is_empty() {
                self.name = name.to_string();
            }
        }
        if let Some(fragment) = function.get(Wire::ARGUMENTS).and_then(Value::as_str) {
            self.arguments.push_str(fragment);
        }
    }

    /// The finished call at zero-based `index`, in the worker's parsed-call shape. Empty
    /// arguments are `{}`; arguments that are not JSON are `null` (as the worker reports them),
    /// with their text in `raw` and `parse_error` set.
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
                Err(e) => (Value::Null, Some(e.to_string())),
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
pub(super) struct Turn {
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
    const F_CHOICES: &'static str = "choices";
    const F_DELTA: &'static str = "delta";
    const F_INDEX: &'static str = "index";
    const F_FINISH_REASON: &'static str = "finish_reason";
    const F_COMPLETION_DETAILS: &'static str = "completion_tokens_details";
    const F_PROMPT: &'static str = "prompt";
    const F_TEXT: &'static str = "text";
    const F_TOKENS: &'static str = "tokens";
    const F_STOP: &'static str = "stop";
    const F_NEEDED: &'static str = "needed";
    const F_TOTAL: &'static str = "total";
    const F_MESSAGE: &'static str = "message";
    const STREAM_CUT: &'static str = "stream ended before the answer finished";

    pub(super) fn new(
        api: Arc<OpenRouterApi>,
        body: Value,
        n_ctx: u32,
        base_estimate: u32,
        estimate: u32,
        calibration: Arc<Calibration>,
    ) -> Self {
        let started = Self::note(
            Self::M_TURN_STARTED,
            json!({
                (Self::F_PROMPT): null,
                (Wire::PROMPT_TOKENS): estimate,
                (Wire::N_CTX): n_ctx,
                (Wire::ESTIMATED): true,
            }),
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
    pub(super) async fn step(mut self) -> Option<(Result<Note, LinkError>, Self)> {
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
        if let Some(usage) = event.get(Wire::USAGE).filter(|u| u.is_object()) {
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
            if let Some(text) = Self::text(delta, Wire::REASONING) {
                self.pending.push_back(Self::chunk(text, true));
            }
            if let Some(text) = Self::text(delta, Wire::CONTENT) {
                self.content.push_str(text);
                self.pending.push_back(Self::chunk(text, false));
            }
            if let Some(calls) = delta.get(Wire::TOOL_CALLS).and_then(Value::as_array) {
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
        let mut params = json!({(Self::F_TEXT): text, (Self::F_TOKENS): [], (Self::F_STOP): false});
        if reasoning {
            params[Wire::REASONING] = json!(true);
        }
        Self::note(Self::M_TURN_CHUNK, params)
    }

    /// Keeps the usage for `turn_done` and calibrates the model's estimate with it. A count the
    /// usage does not report stays `null`, so nobody mistakes it for a real zero.
    fn take_usage(&mut self, usage: &Value) {
        let count = |value: Option<&Value>| value.and_then(Value::as_u64);
        let prompt = count(usage.get(Wire::PROMPT_TOKENS));
        let reasoning = count(
            usage
                .get(Self::F_COMPLETION_DETAILS)
                .and_then(|d| d.get(Wire::REASONING_TOKENS)),
        );
        self.usage = Some(json!({
            (Wire::PROMPT_TOKENS): prompt,
            (Wire::COMPLETION_TOKENS): count(usage.get(Wire::COMPLETION_TOKENS)),
            (Wire::REASONING_TOKENS): reasoning,
            (Wire::COST): usage.get(Wire::COST).cloned().unwrap_or(Value::Null),
        }));
        if let Some(prompt) = prompt {
            self.calibration.observe(
                self.base_estimate,
                u32::try_from(prompt).unwrap_or(u32::MAX),
            );
        }
    }

    /// Ends the turn with `error`: a context-length refusal is `turn_rejected` (so the turn
    /// ends `window_full`), anything else `turn_error`.
    fn fail(&mut self, error: ApiError) {
        self.phase = Phase::Finished;
        let note = if error.is_context_length() {
            Self::note(
                Self::M_TURN_REJECTED,
                json!({(Self::F_NEEDED): 0, (Self::F_TOTAL): self.n_ctx}),
            )
        } else {
            Self::note(
                Self::M_TURN_ERROR,
                json!({(Self::F_MESSAGE): error.to_string()}),
            )
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
        let mut done = json!({(Wire::CONTENT): self.content, (Wire::TOOL_CALLS): calls});
        if let Some(usage) = self.usage.take() {
            done[Wire::USAGE] = usage;
        }
        self.pending.push_back(Self::note(Self::M_TURN_DONE, done));
    }
}
