//! Context compaction: the seam a strategy measures prompts through, the shared turn and
//! summary-note helpers, and the **trim** strategy (stub out old tool results, then drop old
//! turns, until the prompt fits a budget).

use async_trait::async_trait;
use serde_json::Value;

use super::trace::CompactionRecord;
use super::worker::PromptSize;
use crate::engine::{ChatMessage, Role};

/// Measures a candidate prompt exactly as the worker would render and tokenize it.
#[async_trait]
pub trait PromptMeasure: Send + Sync {
    async fn measure(&self, messages: &[ChatMessage]) -> Result<PromptSize, String>;
}

/// A strategy's output: the messages to send and what was done (`None` when nothing was).
#[derive(Debug, Clone, PartialEq)]
pub struct Compacted {
    pub messages: Vec<ChatMessage>,
    pub record: Option<CompactionRecord>,
}

/// Turn boundaries: a turn is a user message and everything up to the next user message.
pub struct Turns;

impl Turns {
    /// The index of each user message, in order.
    pub fn starts(messages: &[ChatMessage]) -> Vec<usize> {
        messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == Role::User)
            .map(|(i, _)| i)
            .collect()
    }
}

/// The user message that stands in for a summarised span. It is never elided or dropped.
pub struct SummaryNote;

impl SummaryNote {
    /// The fixed first line of every summary note.
    pub const HEADER: &'static str = "[Summary of the earlier conversation]";

    /// The note carrying `summary`: the header, a line feed, then the summary.
    pub fn message(summary: &str) -> ChatMessage {
        ChatMessage::new(Role::User, format!("{}\n{summary}", Self::HEADER))
    }

    /// Whether `message` is a summary note.
    pub fn is_note(message: &ChatMessage) -> bool {
        message.role == Role::User && message.content.starts_with(Self::HEADER)
    }
}

/// The trim strategy. It stores nothing; each call works from the full transcript.
pub struct Trim;

impl Trim {
    /// The record's `strategy` value.
    pub const STRATEGY: &'static str = "trim";
    /// The stub's tool name when no matching call is found.
    const UNKNOWN_TOOL: &'static str = "tool";

    /// Compacts `messages` so the prompt plus `reservation` fits `budget`.
    ///
    /// `before` is the measured size of `messages`. Earlier tool results are stubbed, oldest
    /// first (never one whose stub is no shorter), re-measuring after each; if that is not
    /// enough, the oldest earlier turns are dropped whole. Index 0 when it is the system
    /// message, `current_turn_start..` and summary notes are never touched. When nothing
    /// earlier can be removed the messages come back unchanged with `after == before`. The
    /// returned messages have always been measured; the record (always present) carries that
    /// last measure as `after_tokens`.
    pub async fn compact(
        messages: &[ChatMessage],
        current_turn_start: usize,
        budget: usize,
        reservation: usize,
        before: usize,
        measure: &dyn PromptMeasure,
    ) -> Result<Compacted, String> {
        let mut plan = TrimPlan::new(messages, current_turn_start);
        let mut after = before;
        if !Self::fits(after, reservation, budget) {
            for (index, stub) in plan.elidable() {
                plan.stub(index, stub);
                after = measure.measure(&plan.build()).await?.prompt_tokens;
                if Self::fits(after, reservation, budget) {
                    break;
                }
            }
        }
        if !Self::fits(after, reservation, budget) {
            for turn in plan.droppable_turns() {
                plan.drop_turn(turn);
                after = measure.measure(&plan.build()).await?.prompt_tokens;
                if Self::fits(after, reservation, budget) {
                    break;
                }
            }
        }
        let mut record = CompactionRecord::new(Self::STRATEGY, before, after);
        record.elided_results = CompactionRecord::count(plan.elided());
        record.dropped_turns = CompactionRecord::count(plan.dropped_turns);
        Ok(Compacted {
            messages: plan.build(),
            record: Some(record),
        })
    }

    /// Whether a prompt of `tokens` plus `reservation` fits `budget`.
    fn fits(tokens: usize, reservation: usize, budget: usize) -> bool {
        tokens.saturating_add(reservation) <= budget
    }

    /// The stub text that replaces a `tool` result of `bytes` serialized bytes.
    fn stub_text(tool: &str, bytes: usize) -> String {
        format!("[{tool} result elided: {bytes} bytes]")
    }

    /// The serialized length of a tool result (the trace's tool-timing `bytes`).
    fn bytes(result: &Value) -> usize {
        serde_json::to_string(result).map_or(0, |s| s.len())
    }

    /// The name of the call `messages[index]` answers, from the nearest earlier message
    /// carrying a call with its `tool_call_id`.
    fn tool_name(messages: &[ChatMessage], index: usize) -> &str {
        let Some(id) = messages[index].tool_call_id.as_deref() else {
            return Self::UNKNOWN_TOOL;
        };
        messages[..index]
            .iter()
            .rev()
            .filter_map(|m| m.tool_calls.as_ref())
            .flatten()
            .find(|call| call.id == id)
            .map_or(Self::UNKNOWN_TOOL, |call| call.function.name.as_str())
    }
}

/// Trim's working state over the original transcript: per-message replacements and drops.
struct TrimPlan<'a> {
    messages: &'a [ChatMessage],
    /// The first message of the current turn (clamped to the transcript).
    current: usize,
    replaced: Vec<Option<ChatMessage>>,
    dropped: Vec<bool>,
    dropped_turns: usize,
}

impl<'a> TrimPlan<'a> {
    fn new(messages: &'a [ChatMessage], current_turn_start: usize) -> Self {
        Self {
            messages,
            current: current_turn_start.min(messages.len()),
            replaced: vec![None; messages.len()],
            dropped: vec![false; messages.len()],
            dropped_turns: 0,
        }
    }

    /// Whether `index` lies in the region trim may change: after a leading system message,
    /// before the current turn, and not a summary note.
    fn touchable(&self, index: usize) -> bool {
        let system = index == 0 && self.messages[0].role == Role::System;
        index < self.current && !system && !SummaryNote::is_note(&self.messages[index])
    }

    /// Earlier tool results worth stubbing, oldest first, each with its replacement message
    /// (same role and `tool_call_id`, the stub text as its `tool_result`).
    fn elidable(&self) -> Vec<(usize, ChatMessage)> {
        let mut out = Vec::new();
        for (index, message) in self.messages.iter().enumerate() {
            let Some(result) = &message.tool_result else {
                continue;
            };
            if message.role != Role::Tool || !self.touchable(index) {
                continue;
            }
            let bytes = Trim::bytes(result);
            let stub = Value::String(Trim::stub_text(
                Trim::tool_name(self.messages, index),
                bytes,
            ));
            if Trim::bytes(&stub) >= bytes {
                continue;
            }
            let mut replacement = message.clone();
            replacement.tool_result = Some(stub);
            out.push((index, replacement));
        }
        out
    }

    /// Earlier turns that may be dropped, oldest first, as index ranges. A turn ends at the
    /// next user message or the current turn, so a call never loses its result.
    fn droppable_turns(&self) -> Vec<std::ops::Range<usize>> {
        let starts: Vec<usize> = Turns::starts(self.messages)
            .into_iter()
            .filter(|&s| s < self.current)
            .collect();
        starts
            .iter()
            .enumerate()
            .filter(|(_, s)| self.touchable(**s))
            .map(|(k, &s)| s..starts.get(k + 1).copied().unwrap_or(self.current))
            .collect()
    }

    fn stub(&mut self, index: usize, replacement: ChatMessage) {
        self.replaced[index] = Some(replacement);
    }

    fn drop_turn(&mut self, turn: std::ops::Range<usize>) {
        for index in turn {
            self.dropped[index] = true;
        }
        self.dropped_turns += 1;
    }

    /// Stubs in messages that are still sent.
    fn elided(&self) -> usize {
        (0..self.messages.len())
            .filter(|&i| !self.dropped[i] && self.replaced[i].is_some())
            .count()
    }

    /// The prompt as planned so far.
    fn build(&self) -> Vec<ChatMessage> {
        self.messages
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.dropped[*i])
            .map(|(i, m)| self.replaced[i].clone().unwrap_or_else(|| m.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::types::{FunctionCall, ToolCall};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const READ_FILE: &str = "read_file";
    const RESERVATION: usize = 100;
    const BIG: usize = 4000;

    /// One token per 4 characters of rendered content plus 4 per message.
    struct FakeMeasure {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl PromptMeasure for FakeMeasure {
        async fn measure(&self, messages: &[ChatMessage]) -> Result<PromptSize, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(PromptSize {
                prompt_tokens: Self::tokens(messages),
                n_ctx: 1_000_000,
            })
        }
    }

    impl FakeMeasure {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }

        fn tokens(messages: &[ChatMessage]) -> usize {
            messages.iter().map(Self::message_tokens).sum()
        }

        fn message_tokens(message: &ChatMessage) -> usize {
            let chars = match &message.template_view()["content"] {
                Value::String(s) => s.len(),
                other => other.to_string().len(),
            };
            chars / 4 + 4
        }
    }

    /// Transcript builders.
    struct Fx;

    impl Fx {
        fn system() -> ChatMessage {
            ChatMessage::new(Role::System, "you are helpful")
        }

        fn user(text: &str) -> ChatMessage {
            ChatMessage::new(Role::User, text)
        }

        fn assistant(text: &str) -> ChatMessage {
            ChatMessage::new(Role::Assistant, text)
        }

        fn call(id: &str, name: &str) -> ChatMessage {
            let mut m = ChatMessage::new(Role::Assistant, "");
            m.tool_calls = Some(vec![ToolCall {
                id: id.into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: name.into(),
                    arguments: json!({}),
                },
            }]);
            m
        }

        fn result(id: &str, value: Value) -> ChatMessage {
            let mut m = ChatMessage::new(Role::Tool, "");
            m.tool_call_id = Some(id.into());
            m.tool_result = Some(value);
            m
        }

        fn big() -> Value {
            json!({ "content": "x".repeat(BIG) })
        }

        /// An earlier turn: user, `read_file` call, its result, an answer.
        fn tool_turn(n: usize, result: Value) -> Vec<ChatMessage> {
            let id = format!("c{n}");
            vec![
                Self::user(&format!("q{n}")),
                Self::call(&id, READ_FILE),
                Self::result(&id, result),
                Self::assistant("done"),
            ]
        }

        fn stub(result: &Value) -> Value {
            let bytes = serde_json::to_string(result).unwrap().len();
            Value::String(format!("[read_file result elided: {bytes} bytes]"))
        }

        /// Tokens one stub saves over its big result.
        fn saving() -> usize {
            let big = Self::result("c", Self::big());
            let mut stubbed = big.clone();
            stubbed.tool_result = Some(Self::stub(&Self::big()));
            FakeMeasure::message_tokens(&big) - FakeMeasure::message_tokens(&stubbed)
        }
    }

    #[tokio::test]
    async fn trim_elides_oldest_results_until_under_budget() {
        let mut messages = vec![Fx::system()];
        for n in 0..3 {
            messages.extend(Fx::tool_turn(n, Fx::big()));
        }
        let current = messages.len();
        messages.push(Fx::user("now"));
        let before = FakeMeasure::tokens(&messages);
        let budget = before + RESERVATION - Fx::saving() - Fx::saving() / 2;
        let measure = FakeMeasure::new();
        let out = Trim::compact(&messages, current, budget, RESERVATION, before, &measure)
            .await
            .unwrap();
        let stub = Fx::stub(&Fx::big());
        assert_eq!(out.messages.len(), messages.len());
        assert_eq!(out.messages[3].tool_result, Some(stub.clone()));
        assert_eq!(out.messages[7].tool_result, Some(stub.clone()));
        assert_eq!(out.messages[11].tool_result, Some(Fx::big()));
        // The stubbed messages still pair with their calls.
        assert_eq!(out.messages[3].role, Role::Tool);
        assert_eq!(out.messages[3].tool_call_id.as_deref(), Some("c0"));
        assert_eq!(out.messages[7].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(
            stub,
            Value::String(format!(
                "[read_file result elided: {} bytes]",
                serde_json::to_string(&Fx::big()).unwrap().len()
            ))
        );
        let record = out.record.unwrap();
        assert_eq!(record.strategy, "trim");
        assert_eq!(record.elided_results, 2);
        assert_eq!(record.dropped_turns, 0);
        assert_eq!(record.before_tokens as usize, before);
        assert_eq!(
            record.after_tokens as usize,
            FakeMeasure::tokens(&out.messages)
        );
        assert!(record.after_tokens as usize <= budget - RESERVATION);
    }

    #[tokio::test]
    async fn trim_drops_oldest_turns_when_elision_is_not_enough() {
        let mut messages = vec![Fx::system()];
        messages.push(Fx::user(&"a".repeat(BIG)));
        messages.push(Fx::assistant("ok"));
        messages.extend(Fx::tool_turn(1, Fx::big()));
        messages.extend(Fx::tool_turn(2, json!({"content": "y".repeat(BIG)})));
        let current = messages.len();
        messages.push(Fx::user("now"));
        let before = FakeMeasure::tokens(&messages);
        // Stubbing both results is not enough; dropping the first (big user) turn is.
        let budget = before + RESERVATION - 2 * Fx::saving() - 500;
        let measure = FakeMeasure::new();
        let out = Trim::compact(&messages, current, budget, RESERVATION, before, &measure)
            .await
            .unwrap();
        let mut expected = vec![Fx::system()];
        for (n, v) in [(1, Fx::big()), (2, json!({"content": "y".repeat(BIG)}))] {
            let mut turn = Fx::tool_turn(n, v.clone());
            turn[2].tool_result = Some(Fx::stub(&v));
            expected.extend(turn);
        }
        expected.push(Fx::user("now"));
        assert_eq!(out.messages, expected);
        let record = out.record.unwrap();
        assert_eq!(record.elided_results, 2);
        assert_eq!(record.dropped_turns, 1);
        assert_eq!(record.after_tokens as usize, FakeMeasure::tokens(&expected));
        assert!(record.after_tokens as usize <= budget - RESERVATION);
    }

    #[tokio::test]
    async fn trim_never_touches_system_current_turn_or_summary_note() {
        let system = ChatMessage::new(Role::System, "s".repeat(BIG));
        let note = SummaryNote::message(&"n".repeat(BIG));
        let mut messages = vec![system.clone(), note.clone()];
        messages.extend(Fx::tool_turn(1, Fx::big()));
        let current = messages.len();
        let current_turn = Fx::tool_turn(2, Fx::big());
        messages.extend(current_turn.clone());
        let before = FakeMeasure::tokens(&messages);
        // Unreachable: everything trim may remove is removed, the rest stays verbatim.
        let out = Trim::compact(
            &messages,
            current,
            RESERVATION + 1,
            RESERVATION,
            before,
            &FakeMeasure::new(),
        )
        .await
        .unwrap();
        let mut expected = vec![system, note];
        expected.extend(current_turn);
        assert_eq!(out.messages, expected);
        let record = out.record.unwrap();
        assert_eq!(record.dropped_turns, 1);
        // The earlier result was stubbed, then its turn dropped: no stub is sent.
        assert_eq!(record.elided_results, 0);
        assert_eq!(record.after_tokens as usize, FakeMeasure::tokens(&expected));
    }

    #[tokio::test]
    async fn short_result_is_not_replaced_by_a_longer_stub() {
        let short = json!({"ok": true});
        let mut messages = vec![Fx::system()];
        messages.extend(Fx::tool_turn(0, short.clone()));
        messages.extend(Fx::tool_turn(1, Fx::big()));
        let current = messages.len();
        messages.push(Fx::user("now"));
        let before = FakeMeasure::tokens(&messages);
        let budget = before + RESERVATION - Fx::saving() / 2;
        let measure = FakeMeasure::new();
        let out = Trim::compact(&messages, current, budget, RESERVATION, before, &measure)
            .await
            .unwrap();
        assert_eq!(out.messages[3].tool_result, Some(short));
        assert_eq!(out.messages[7].tool_result, Some(Fx::stub(&Fx::big())));
        let record = out.record.unwrap();
        assert_eq!(record.elided_results, 1);
        assert_eq!(record.dropped_turns, 0);
        // Only the big result was tried.
        assert_eq!(measure.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn current_turn_overflow_returns_unchanged_with_record() {
        let mut messages = vec![Fx::system()];
        messages.extend(Fx::tool_turn(0, Fx::big()));
        let before = FakeMeasure::tokens(&messages);
        let measure = FakeMeasure::new();
        let out = Trim::compact(
            &messages,
            1,
            RESERVATION + 10,
            RESERVATION,
            before,
            &measure,
        )
        .await
        .unwrap();
        assert_eq!(out.messages, messages);
        let record = out.record.unwrap();
        assert_eq!(record.strategy, "trim");
        assert_eq!(record.after_tokens, record.before_tokens);
        assert_eq!(record.before_tokens as usize, before);
        assert_eq!(record.elided_results, 0);
        assert_eq!(record.dropped_turns, 0);
        assert_eq!(measure.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn turns_starts_marks_each_user_message() {
        let mut messages = vec![Fx::system()];
        messages.extend(Fx::tool_turn(0, json!(1)));
        messages.push(SummaryNote::message("s"));
        messages.push(Fx::user("q"));
        messages.push(Fx::assistant("a"));
        assert_eq!(Turns::starts(&messages), vec![1, 5, 6]);
        assert!(Turns::starts(&[]).is_empty());
        assert!(SummaryNote::is_note(&messages[5]));
        assert!(!SummaryNote::is_note(&messages[6]));
        assert_eq!(
            messages[5].content,
            "[Summary of the earlier conversation]\ns"
        );
    }
}
