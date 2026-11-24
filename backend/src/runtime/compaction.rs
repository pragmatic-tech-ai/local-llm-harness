//! Context compaction: the seam a strategy measures prompts through, the shared turn and
//! summary-note helpers, the **trim** strategy (stub out old tool results, then drop old
//! turns, until the prompt fits a budget) and the **summarize** strategy (replace the older
//! span with a saved summary note, trimming on top or instead when needed).
//!
//! The [`Compactor`] is the front the tool loop calls once per step, before `generate`: it
//! measures the prompt through the step's worker link and runs the session's strategy only
//! when the prompt is over budget. The loop sends what it returns and puts the record on the
//! step's trace record; the transcript the loop commits is never compacted.
//!
//! The session's notes block rides on the system message of every prompt the `Compactor`
//! returns, after any summary note, and is part of every step measure. It is applied as a
//! last transform on the outgoing copy only: the strategies, their indexes and a saved
//! summary's `covers` all work on the un-noted transcript.

use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;

use super::notes::NotesBlock;
use super::tool_loop::{NoteUsage, ToolLoop};
use super::trace::{CompactionRecord, ToolTiming};
use super::worker::{GenerateRequest, LinkError, PromptSize, WorkerLink};
use crate::agents::template::{CompactionState, ContextStrategy, DEFAULT_COMPACT_AT_PCT};
use crate::engine::template::OpenChannel;
use crate::engine::{ChatMessage, Role, SamplingConfig};

/// Measures a candidate prompt exactly as the worker would render and tokenize it.
#[async_trait]
pub trait PromptMeasure: Send + Sync {
    async fn measure(&self, messages: &[ChatMessage]) -> Result<PromptSize, String>;
}

/// A compaction's output: the messages to send and what was done. `record` is `None` only on
/// the `Compactor`'s no-op paths (strategy off, under budget, measure failed); a strategy that
/// ran, such as [`Trim::compact`], always returns `Some`.
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

/// The note that stands in for a summarised span. It rides in the system message, so the
/// prompt keeps the user/assistant alternation strict chat templates (Gemma 3) enforce, and
/// like the system message it is never elided or dropped.
pub struct SummaryNote;

impl SummaryNote {
    /// The fixed first line of every summary note.
    pub const HEADER: &'static str = "[Summary of the earlier conversation]";
    /// What separates the session's system prompt from the note.
    const SEPARATOR: &'static str = "\n\n";

    /// The note text for `summary`: the header, a line feed, then the summary.
    pub fn text(summary: &str) -> String {
        format!("{}\n{summary}", Self::HEADER)
    }

    /// The system message of a session without one: the note alone.
    pub fn message(summary: &str) -> ChatMessage {
        ChatMessage::new(Role::System, Self::text(summary))
    }

    /// The system message carrying `summary`: `system`'s content, a blank line, then the note
    /// (just the note when `system` is `None` or empty).
    pub fn system(system: Option<&ChatMessage>, summary: &str) -> ChatMessage {
        match system {
            Some(system) if !system.content.is_empty() => {
                let mut out = system.clone();
                out.content = format!(
                    "{}{}{}",
                    system.content,
                    Self::SEPARATOR,
                    Self::text(summary)
                );
                out
            }
            Some(system) => {
                let mut out = system.clone();
                out.content = Self::text(summary);
                out
            }
            None => Self::message(summary),
        }
    }

    /// Whether `message` is a system message carrying a summary note.
    pub fn is_note(message: &ChatMessage) -> bool {
        let after_prompt = format!("{}{}", Self::SEPARATOR, Self::HEADER);
        message.role == Role::System
            && (message.content.starts_with(Self::HEADER)
                || message.content.contains(&after_prompt))
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
    /// returned messages have always been measured; the record carries that last measure as
    /// `after_tokens`.
    ///
    /// The record is always `Some`, even when `before` already fits (then all counts are zero
    /// and nothing is measured). Callers decide whether to compact at all: the `Compactor`
    /// calls this only when the prompt is over budget, so an all-zero record never reaches the
    /// trace.
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

    /// Whether `index` lies in the region trim may change: after a leading system message
    /// (which also carries any summary note), before the current turn, and not a summary note.
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
            let bytes = ToolTiming::result_bytes(result);
            let stub = Value::String(Trim::stub_text(
                Trim::tool_name(self.messages, index),
                bytes,
            ));
            if ToolTiming::result_bytes(&stub) >= bytes {
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

/// Where a fresh summary is saved. It is called once per summary, before the summarised prompt
/// is sent, outside any store or hub lock.
#[async_trait]
pub trait SummarySink: Send + Sync {
    async fn save(&self, state: CompactionState) -> Result<(), String>;
}

/// The summarize strategy: keep the previous and current turns verbatim and replace everything
/// between the system message and them with one summary note appended to the system message,
/// made by an extra `generate` on the worker link (chunked when the span is too big for one
/// call) and saved through a [`SummarySink`] so later steps and turns reuse it.
pub struct Summarize;

impl Summarize {
    /// The record's `strategy` value.
    pub const STRATEGY: &'static str = "summarize";
    /// The fixed system prompt of every summary call.
    pub const SYSTEM_PROMPT: &'static str = "Summarize the conversation below for your own later use. Keep facts, file names, numbers, decisions and open questions. Write plain sentences, no preamble.";
    /// The cap on a summary call's `max_tokens`.
    pub const MAX_TOKENS_CAP: usize = 1024;
    /// A summary call's `max_tokens` is at most `n_ctx / CTX_DIVISOR`.
    const CTX_DIVISOR: usize = 8;
    /// Each tool result is cut to this many characters in a span line.
    pub const RESULT_CHARS: usize = 2000;
    /// The most summary calls one step may make.
    pub const MAX_CALLS: usize = 4;
    const TEMPERATURE: f32 = 0.2;
    const TOP_P: f32 = 0.9;
    const TOP_K: u32 = 40;
    const REPEAT_PENALTY: f32 = 1.0;
    const ERR_NOTHING: &'static str = "nothing to summarize before the kept tail";
    const ERR_SAVE: &'static str = "saving the summary failed: ";
    const ERR_NOTE_MEASURE: &'static str = "measuring the summary note failed: ";
    const ERR_PROMPT_MEASURE: &'static str = "measuring the summarized prompt failed: ";

    /// A summary call's `max_tokens`: `min(1024, n_ctx / 8)`.
    pub fn max_tokens(n_ctx: usize) -> usize {
        Self::MAX_TOKENS_CAP.min(n_ctx / Self::CTX_DIVISOR)
    }

    /// Compacts `messages` (the committed transcript `..committed_len` plus the current turn's
    /// scratch from `current_turn_start`) so the prompt plus `reservation` fits `budget`.
    ///
    /// - **Reuse.** A `saved` summary whose `covers` lies after the system message and within
    ///   the committed transcript (and before the current turn) gives the system message with
    ///   the note appended (or the note as the system message), then `messages[covers..]`. If
    ///   that fits, it is sent with `reused_summary`.
    /// - **Summarise.** Otherwise the span from after the system message (or from
    ///   `saved.covers`, with the old summary text folded in) up to the start of the turn
    ///   before the current one is summarised over `link`, saved through `sink` with `covers`
    ///   set to the span's end index, and the summarised prompt is measured. If it is still
    ///   over budget, [`Trim`] runs on it and its counts are merged in. When there is nothing
    ///   new to summarise but a saved summary exists, trim runs on the reused prompt.
    /// - **Fallback.** Any summary failure, including a failed save, and any failed measure of
    ///   a summarised prompt (a template rejecting it, say) runs [`Trim`] on the original
    ///   messages with `fallback` true and the cause in `summary_error`.
    ///
    /// `measure` sizes the step's prompts (the step's tools and thinking flag);
    /// `summary_measure` sizes the summary calls (no tools, thinking off): their input budget,
    /// chunks and the saved `tokens`.
    ///
    /// `covers` is a transcript index: the summary replaces `messages[system_len..covers]`,
    /// and `summarized_messages` is that count. The record is always `Some`, with `strategy`
    /// `"summarize"` and `after_tokens` from the last measure. `Err` carries a measure error
    /// of the original messages (as [`Trim::compact`] does).
    #[allow(clippy::too_many_arguments)]
    pub async fn compact(
        messages: &[ChatMessage],
        committed_len: usize,
        current_turn_start: usize,
        saved: Option<&CompactionState>,
        budget: usize,
        reservation: usize,
        before: usize,
        n_ctx: usize,
        sampling: &SamplingConfig,
        link: &dyn WorkerLink,
        measure: &dyn PromptMeasure,
        summary_measure: &dyn PromptMeasure,
        sink: &dyn SummarySink,
    ) -> Result<Compacted, String> {
        let current = current_turn_start.min(messages.len());
        let step = SummarizeStep {
            messages,
            system_len: Self::system_len(messages),
            limit: committed_len.min(current),
            current,
            budget,
            reservation,
            before,
            measure,
        };
        let none = SummaryStats::default();
        let saved = saved.filter(|s| step.can_reuse(s));
        let mut reused_tokens = None;
        if let Some(state) = saved {
            let prompt = step.prompt(&state.summary, state.covers);
            let tokens = match measure.measure(&prompt).await {
                Ok(size) => size.prompt_tokens,
                Err(e) => return step.fallback(Self::prompt_error(&e), &none).await,
            };
            reused_tokens = Some(tokens);
            if Trim::fits(tokens, reservation, budget) {
                let mut record = CompactionRecord::new(Self::STRATEGY, before, tokens);
                record.reused_summary = true;
                record.summarized_messages = step.summarized(state.covers);
                return Ok(Compacted {
                    messages: prompt,
                    record: Some(record),
                });
            }
        }
        let span_start = saved.map_or(step.system_len, |s| s.covers);
        let span_end = step.tail_start().max(span_start);
        if span_end == span_start {
            let Some(state) = saved else {
                return step.fallback(Self::ERR_NOTHING.into(), &none).await;
            };
            return match step
                .on_top(&state.summary, state.covers, reused_tokens, &none)
                .await
            {
                Ok(mut out) => {
                    if let Some(record) = out.record.as_mut() {
                        record.reused_summary = true;
                    }
                    Ok(out)
                }
                Err(e) => step.fallback(Self::prompt_error(&e), &none).await,
            };
        }
        let mut summarizer = Summarizer::new(link, summary_measure, sampling, n_ctx);
        let old = saved.map(|s| s.summary.as_str());
        let lines: Vec<String> = messages[span_start..span_end]
            .iter()
            .map(SpanLine::render)
            .collect();
        let summary = match summarizer.run(old, &lines).await {
            Ok(summary) => summary,
            Err(error) => return step.fallback(error, &summarizer.stats).await,
        };
        let tokens = match summarizer.note_tokens(&summary).await {
            Ok(tokens) => tokens,
            Err(e) => {
                let error = format!("{}{e}", Self::ERR_NOTE_MEASURE);
                return step.fallback(error, &summarizer.stats).await;
            }
        };
        let state = CompactionState {
            summary,
            covers: span_end,
            tokens,
            at: Self::now_ms(),
        };
        if let Err(e) = sink.save(state.clone()).await {
            let error = format!("{}{e}", Self::ERR_SAVE);
            return step.fallback(error, &summarizer.stats).await;
        }
        match step
            .on_top(&state.summary, state.covers, None, &summarizer.stats)
            .await
        {
            Ok(out) => Ok(out),
            Err(e) => {
                step.fallback(Self::prompt_error(&e), &summarizer.stats)
                    .await
            }
        }
    }

    /// The `summary_error` of a failed measure of a summarised prompt.
    fn prompt_error(error: &str) -> String {
        format!("{}{error}", Self::ERR_PROMPT_MEASURE)
    }

    /// 1 when `messages` starts with the system message, otherwise 0.
    fn system_len(messages: &[ChatMessage]) -> usize {
        usize::from(messages.first().is_some_and(|m| m.role == Role::System))
    }

    /// Wall-clock Unix milliseconds.
    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }
}

/// One summarize step's inputs, shared by its paths.
struct SummarizeStep<'a> {
    messages: &'a [ChatMessage],
    /// 1 when index 0 is the system message, otherwise 0.
    system_len: usize,
    /// The end of the committed transcript that may be summarised (never past the current
    /// turn).
    limit: usize,
    /// The current turn's first index (clamped).
    current: usize,
    budget: usize,
    reservation: usize,
    before: usize,
    measure: &'a dyn PromptMeasure,
}

impl SummarizeStep<'_> {
    /// Whether `state` still covers a non-empty prefix of the committed transcript.
    fn can_reuse(&self, state: &CompactionState) -> bool {
        state.covers > self.system_len && state.covers <= self.limit
    }

    /// The start of the turn before the current one, capped at the committed transcript; the
    /// system length when there is no such turn.
    fn tail_start(&self) -> usize {
        Turns::starts(self.messages)
            .into_iter()
            .filter(|&s| s < self.current)
            .last()
            .map_or(self.system_len, |s| s.min(self.limit))
    }

    /// How many transcript messages a summary covering `..covers` replaces.
    fn summarized(&self, covers: usize) -> u32 {
        CompactionRecord::count(covers.saturating_sub(self.system_len))
    }

    /// The system message carrying the summary note (see [`SummaryNote::system`]), then
    /// `messages[covers..]`. The prompt always starts with that one system message, so the
    /// user/assistant turns after it alternate as in the transcript.
    fn prompt(&self, summary: &str, covers: usize) -> Vec<ChatMessage> {
        let system = self.messages[..self.system_len].first();
        let mut out = vec![SummaryNote::system(system, summary)];
        out.extend_from_slice(&self.messages[covers..]);
        out
    }

    /// Sends the summarised prompt, trimming it when it is still over budget. `measured` is
    /// the prompt's size when the caller has already measured it (the reused prompt), so it
    /// is not measured twice. `Err` is a failed measure of the summarised prompt.
    async fn on_top(
        &self,
        summary: &str,
        covers: usize,
        measured: Option<usize>,
        stats: &SummaryStats,
    ) -> Result<Compacted, String> {
        let prompt = self.prompt(summary, covers);
        let tokens = match measured {
            Some(tokens) => tokens,
            None => self.measure.measure(&prompt).await?.prompt_tokens,
        };
        let mut record = CompactionRecord::new(Summarize::STRATEGY, self.before, tokens);
        let messages = if Trim::fits(tokens, self.reservation, self.budget) {
            prompt
        } else {
            // The note's system message, then `messages[covers..]`.
            let current = 1 + (self.current - covers);
            let trimmed = Trim::compact(
                &prompt,
                current,
                self.budget,
                self.reservation,
                tokens,
                self.measure,
            )
            .await?;
            if let Some(trim) = trimmed.record {
                record.after_tokens = trim.after_tokens;
                record.elided_results = trim.elided_results;
                record.dropped_turns = trim.dropped_turns;
            }
            trimmed.messages
        };
        record.summarized_messages = self.summarized(covers);
        stats.apply(&mut record);
        Ok(Compacted {
            messages,
            record: Some(record),
        })
    }

    /// Trims the original messages after a summary failure, recording why.
    async fn fallback(&self, error: String, stats: &SummaryStats) -> Result<Compacted, String> {
        let trimmed = Trim::compact(
            self.messages,
            self.current,
            self.budget,
            self.reservation,
            self.before,
            self.measure,
        )
        .await?;
        let mut record = trimmed.record.unwrap_or_else(|| {
            CompactionRecord::new(Summarize::STRATEGY, self.before, self.before)
        });
        record.strategy = Summarize::STRATEGY.to_string();
        record.fallback = true;
        record.summary_error = Some(error);
        stats.apply(&mut record);
        Ok(Compacted {
            messages: trimmed.messages,
            record: Some(record),
        })
    }
}

/// One transcript message as a summary-call line: `{role}: {content}`.
struct SpanLine;

impl SpanLine {
    const SEPARATOR: &'static str = ": ";

    /// The line for `message`. A tool message's content is its result (a string as is,
    /// anything else as JSON) cut to [`Summarize::RESULT_CHARS`] characters. Other content
    /// loses its reasoning channels ([`OpenChannel::strip_blocks`]) and surrounding
    /// whitespace, as the chat template would drop them from a normal prompt; only this
    /// summary-call text changes, never the transcript. An assistant message's tool calls
    /// follow its text as `name(arguments)`.
    fn render(message: &ChatMessage) -> String {
        let role = serde_json::to_value(message.role)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let mut content = match (&message.role, &message.tool_result) {
            (Role::Tool, Some(Value::String(text))) => Self::cut(text),
            (Role::Tool, Some(result)) => Self::cut(&result.to_string()),
            _ => OpenChannel::strip_blocks(&message.content)
                .trim()
                .to_string(),
        };
        for call in message.tool_calls.iter().flatten() {
            if !content.is_empty() {
                content.push(' ');
            }
            content.push_str(&format!(
                "{}({})",
                call.function.name, call.function.arguments
            ));
        }
        format!("{role}{}{content}", Self::SEPARATOR)
    }

    fn cut(text: &str) -> String {
        text.chars().take(Summarize::RESULT_CHARS).collect()
    }
}

/// Totals over a step's summary calls, for the record.
#[derive(Default)]
struct SummaryStats {
    calls: usize,
    ms: u64,
    prompt_tokens: u32,
    out_tokens: u32,
    /// Summed provider cost of the calls that reported one.
    cost: Option<f64>,
}

impl SummaryStats {
    /// Swaps one call's counted tokens for the provider's, and adds its cost.
    fn apply_usage(&mut self, usage: NoteUsage, call_prompt: u32, call_out: u32) {
        if let Some(prompt) = usage.prompt_tokens {
            self.prompt_tokens = self
                .prompt_tokens
                .saturating_sub(call_prompt)
                .saturating_add(prompt);
        }
        if let Some(out) = usage.completion_tokens {
            self.out_tokens = self.out_tokens.saturating_sub(call_out).saturating_add(out);
        }
        if let Some(cost) = usage.cost {
            self.cost = Some(self.cost.unwrap_or(0.0) + cost);
        }
    }

    /// Fills the record's summary-call fields when at least one call was made.
    fn apply(&self, record: &mut CompactionRecord) {
        if self.calls == 0 {
            return;
        }
        record.summary_ms = Some(self.ms);
        record.summary_prompt_tokens = Some(self.prompt_tokens);
        record.summary_out_tokens = Some(self.out_tokens);
        record.summary_cost = self.cost;
    }
}

/// Makes the summary calls for one span: plans the chunks, calls `generate` for each, oldest
/// first, and keeps the running summary.
struct Summarizer<'a> {
    link: &'a dyn WorkerLink,
    measure: &'a dyn PromptMeasure,
    sampling: SamplingConfig,
    /// The most tokens a summary call may generate.
    max_tokens: usize,
    n_ctx: usize,
    stats: SummaryStats,
}

impl<'a> Summarizer<'a> {
    const LINE_BREAK: &'static str = "\n";
    const SUMMARY_BREAK: &'static str = "\n\n";
    const ERR_LINK: &'static str = "summary call failed: ";
    const ERR_MEASURE: &'static str = "measuring the summary span failed: ";
    const ERR_EMPTY: &'static str = "summary reply was empty";
    const ERR_TRUNCATED: &'static str = "summary stream ended without turn_done";
    const ERR_LINE_TOO_BIG: &'static str = "a message is too big for one summary call";
    const ERR_TOO_MANY: &'static str = "the span needs more summary calls than the limit of ";
    const ERR_MAX_TOKENS: &'static str = "summary hit its max_tokens before stopping: ";
    const ERR_REJECTED: &'static str = "summary call rejected (window full), needed/total: ";
    const ERR_ERRORED: &'static str = "summary call errored: ";
    /// Tokens the blank line after a running summary adds (and slack for a merge at it).
    const SUMMARY_BREAK_TOKENS: usize = 2;

    fn new(
        link: &'a dyn WorkerLink,
        measure: &'a dyn PromptMeasure,
        session: &SamplingConfig,
        n_ctx: usize,
    ) -> Self {
        let max_tokens = Summarize::max_tokens(n_ctx);
        // Fixed whatever the session samples with; only the seed is the session's.
        let sampling = SamplingConfig {
            temperature: Summarize::TEMPERATURE,
            top_p: Summarize::TOP_P,
            top_k: Summarize::TOP_K,
            repeat_penalty: Summarize::REPEAT_PENALTY,
            seed: session.seed,
            max_tokens: i32::try_from(max_tokens).unwrap_or(i32::MAX),
        };
        Self {
            link,
            measure,
            sampling,
            max_tokens,
            n_ctx,
            stats: SummaryStats::default(),
        }
    }

    /// Summarises `lines` (after the `old` summary, if any) in at most
    /// [`Summarize::MAX_CALLS`] calls; the last call's reply is the summary.
    async fn run(&mut self, old: Option<&str>, lines: &[String]) -> Result<String, String> {
        let chunks = self.plan(old, lines).await?;
        let mut running = old.map(str::to_string);
        for chunk in chunks {
            let content = Self::content(running.as_deref(), &lines[chunk]);
            running = Some(self.call(content).await?);
        }
        running.ok_or_else(|| Summarize::ERR_NOTHING.to_string())
    }

    /// The chunks of `lines`, oldest first: the whole span when one call fits, otherwise as
    /// many whole lines per call as fit. Each chunk's end is found by a binary search over
    /// its length, measuring the assembled call (about log2 of the remaining lines measures
    /// per chunk). A call fits when its measured prompt is at most `n_ctx - max_tokens`; the
    /// first call carries the `old` summary (if any), later calls make room for a running
    /// summary of up to `max_tokens` plus the blank line after it. Needing more than
    /// [`Summarize::MAX_CALLS`] chunks is an error, found before any call: planning stops as
    /// soon as four chunks leave lines over.
    async fn plan(
        &self,
        old: Option<&str>,
        lines: &[String],
    ) -> Result<Vec<std::ops::Range<usize>>, String> {
        let window = self.n_ctx.saturating_sub(self.max_tokens);
        if self.tokens(&Self::content(old, lines)).await? <= window {
            return Ok(vec![0..lines.len()]);
        }
        let mut chunks = Vec::new();
        let mut start = 0;
        while start < lines.len() {
            if chunks.len() == Summarize::MAX_CALLS {
                return Err(Self::too_many_calls());
            }
            let first = chunks.is_empty();
            // The whole span is known not to fit the first call.
            let (mut fit, mut over) = (start, if first { lines.len() } else { lines.len() + 1 });
            while over - fit > 1 {
                let mid = fit + (over - fit) / 2;
                if self.fits(first, old, &lines[start..mid], window).await? {
                    fit = mid;
                } else {
                    over = mid;
                }
            }
            if fit == start {
                return Err(Self::ERR_LINE_TOO_BIG.to_string());
            }
            chunks.push(start..fit);
            start = fit;
        }
        Ok(chunks)
    }

    /// Whether a call over `chunk` fits `window`: the first call with the `old` summary, a
    /// later one with room for a running summary of up to `max_tokens`.
    async fn fits(
        &self,
        first: bool,
        old: Option<&str>,
        chunk: &[String],
        window: usize,
    ) -> Result<bool, String> {
        if first {
            return Ok(self.tokens(&Self::content(old, chunk)).await? <= window);
        }
        let room = self.max_tokens + Self::SUMMARY_BREAK_TOKENS;
        Ok(self.tokens(&Self::content(None, chunk)).await? + room <= window)
    }

    fn too_many_calls() -> String {
        format!("{}{}", Self::ERR_TOO_MANY, Summarize::MAX_CALLS)
    }

    /// The summary-call user content: the running summary (if any), a blank line, then the
    /// span lines one per line.
    fn content(summary: Option<&str>, lines: &[String]) -> String {
        let lines = lines.join(Self::LINE_BREAK);
        match summary {
            Some(summary) => format!("{summary}{}{lines}", Self::SUMMARY_BREAK),
            None => lines,
        }
    }

    /// The summary-call messages for `content`: the fixed system prompt and one user message.
    fn messages(content: &str) -> Vec<ChatMessage> {
        vec![
            ChatMessage::new(Role::System, Summarize::SYSTEM_PROMPT),
            ChatMessage::new(Role::User, content),
        ]
    }

    /// The measured size of a summary call carrying `content`.
    async fn tokens(&self, content: &str) -> Result<usize, String> {
        self.measure
            .measure(&Self::messages(content))
            .await
            .map(|size| size.prompt_tokens)
            .map_err(|e| format!("{}{e}", Self::ERR_MEASURE))
    }

    /// The saved `tokens` of the note for `summary`: what its text adds to an empty summary
    /// call, as the summary call's own render counts it.
    async fn note_tokens(&self, summary: &str) -> Result<u32, String> {
        let text = SummaryNote::text(summary);
        let note = self.measure.measure(&Self::messages(&text)).await?;
        let empty = self.measure.measure(&Self::messages("")).await?;
        let tokens = note.prompt_tokens.saturating_sub(empty.prompt_tokens);
        Ok(CompactionRecord::count(tokens))
    }

    /// One summary call: tools and thinking off, the fixed sampling. The reply is
    /// `turn_done.content` without a leading channel opener, trimmed.
    ///
    /// Taking `turn_done.content` rather than concatenating the `turn_chunk` texts (as the
    /// brief worded it) is intended: it is the text the worker reports as final, and it is
    /// what `ToolLoop` reads too. Without tools the worker builds it from the same chunks.
    ///
    /// A reply counts as cut off when its generated tokens reach `max_tokens`: the usage's
    /// `completion_tokens` when the provider reports it (OpenRouter chunks carry no tokens),
    /// else the sum over `turn_chunk.tokens`. The chunk `stop` flag is not used: the in-process engines send a
    /// final `stop: true` chunk on the `max_tokens` cap as well as on end of sequence.
    async fn call(&mut self, content: String) -> Result<String, String> {
        let req = GenerateRequest {
            messages: Self::messages(&content),
            tools: Vec::new(),
            enable_thinking: false,
            sampling: self.sampling.clone(),
        };
        self.stats.calls += 1;
        let started = Instant::now();
        let reply = self.read(&req).await;
        let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.stats.ms = self.stats.ms.saturating_add(ms);
        reply
    }

    /// Reads one `generate` stream to its end note.
    async fn read(&mut self, req: &GenerateRequest) -> Result<String, String> {
        let link_error = |e: LinkError| format!("{}{e}", Self::ERR_LINK);
        let mut stream = self.link.generate(req).await.map_err(link_error)?;
        let mut out_tokens = 0usize;
        // This call's own share of the running totals, replaced by the provider's usage.
        let mut call_prompt = 0u32;
        let mut call_out = 0u32;
        while let Some(item) = stream.next().await {
            let note = item.map_err(link_error)?;
            let params = &note.params;
            match note.method.as_str() {
                ToolLoop::M_TURN_STARTED => {
                    let tokens = Self::number(params, ToolLoop::FIELD_PROMPT_TOKENS);
                    let tokens = CompactionRecord::count(tokens);
                    call_prompt = tokens;
                    self.stats.prompt_tokens = self.stats.prompt_tokens.saturating_add(tokens);
                }
                ToolLoop::M_TURN_CHUNK => {
                    let tokens = params
                        .get(ToolLoop::FIELD_TOKENS)
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    out_tokens = out_tokens.saturating_add(tokens);
                    let tokens = CompactionRecord::count(tokens);
                    call_out = call_out.saturating_add(tokens);
                    self.stats.out_tokens = self.stats.out_tokens.saturating_add(tokens);
                }
                ToolLoop::M_TURN_DONE => {
                    let usage = NoteUsage::of_done(params);
                    let generated = usage
                        .as_ref()
                        .and_then(|u| u.completion_tokens)
                        .map_or(out_tokens, |n| usize::try_from(n).unwrap_or(usize::MAX));
                    if let Some(usage) = usage {
                        self.stats.apply_usage(usage, call_prompt, call_out);
                    }
                    if generated >= self.max_tokens {
                        return Err(format!("{}{}", Self::ERR_MAX_TOKENS, self.max_tokens));
                    }
                    let content = params
                        .get(ToolLoop::FIELD_CONTENT)
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let summary = Self::without_opener(content).trim();
                    if summary.is_empty() {
                        return Err(Self::ERR_EMPTY.to_string());
                    }
                    return Ok(summary.to_string());
                }
                ToolLoop::M_TURN_REJECTED => {
                    return Err(format!(
                        "{}{}/{}",
                        Self::ERR_REJECTED,
                        Self::number(params, ToolLoop::FIELD_NEEDED),
                        Self::number(params, ToolLoop::FIELD_TOTAL)
                    ));
                }
                ToolLoop::M_TURN_ERROR => {
                    let message = params
                        .get(ToolLoop::FIELD_MESSAGE)
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    return Err(format!("{}{message}", Self::ERR_ERRORED));
                }
                _ => {}
            }
        }
        Err(Self::ERR_TRUNCATED.to_string())
    }

    fn number(params: &Value, field: &str) -> usize {
        params
            .get(field)
            .and_then(Value::as_u64)
            .map_or(0, |n| usize::try_from(n).unwrap_or(usize::MAX))
    }

    /// `content` without a leading `<|channel>name\n` opener the worker may prefix.
    fn without_opener(content: &str) -> &str {
        if !content.starts_with(OpenChannel::OPEN) {
            return content;
        }
        match content.find(Self::LINE_BREAK) {
            Some(end) if OpenChannel::at_tail(&content[..=end]).is_some() => &content[end + 1..],
            _ => content,
        }
    }
}

/// One turn's compaction front, called by the loop once per step before `generate`. It holds
/// the session's strategy, its threshold and its newest saved summary: a summary saved by an
/// earlier step of the turn is reused by the later ones without reading the store again.
pub struct Compactor {
    strategy: ContextStrategy,
    compact_at_pct: u8,
    saved: Option<CompactionState>,
    /// The length of the committed transcript the turn started from.
    committed_len: usize,
    /// The session's sampling; a summary call takes only its seed.
    sampling: SamplingConfig,
    sink: Arc<dyn SummarySink>,
    /// The session named in the measure-failure log line (empty when unknown).
    session: String,
    /// How many measure failures were logged this turn: at most one.
    logged_failures: usize,
}

impl Compactor {
    const PERCENT: usize = 100;
    /// The start of the once-per-turn log line for a failed `measure_prompt`.
    const MEASURE_FAILED: &'static str = "measure_prompt failed, compaction skipped for session";

    pub fn new(
        strategy: ContextStrategy,
        compact_at_pct: u8,
        saved: Option<CompactionState>,
        committed_len: usize,
        sampling: SamplingConfig,
        sink: Arc<dyn SummarySink>,
    ) -> Self {
        Self {
            strategy,
            compact_at_pct,
            saved,
            committed_len,
            sampling,
            sink,
            session: String::new(),
            logged_failures: 0,
        }
    }

    /// Names `session` in the measure-failure log line.
    pub fn for_session(mut self, session: &str) -> Self {
        self.session = session.to_string();
        self
    }

    /// How many measure failures this compactor has logged: 0, or 1 once any measure of the
    /// turn failed (later failures are not logged again).
    pub fn logged_failures(&self) -> usize {
        self.logged_failures
    }

    /// A compactor that never compacts (and never measures).
    pub fn off() -> Self {
        Self::new(
            ContextStrategy::Off,
            DEFAULT_COMPACT_AT_PCT,
            None,
            0,
            SamplingConfig::default(),
            Arc::new(DiscardSink),
        )
    }

    /// The newest summary this turn knows: the session's saved one, or one saved since.
    pub fn saved(&self) -> Option<&CompactionState> {
        self.saved.as_ref()
    }

    /// The prompt to send for one step over `messages` (the committed transcript plus the
    /// current turn's scratch from `current_turn_start`), measured through `link` with the
    /// step's `tools` and `enable_thinking`.
    ///
    /// The budget is `floor(n_ctx * compact_at_pct / 100)` and the reservation
    /// `max(max_tokens, 0)`. When the reservation alone reaches the budget, the target is the
    /// window instead (the budget becomes `n_ctx`): compaction then only removes what keeps
    /// the prompt plus the reservation out of `n_ctx`, and when the reservation fills the
    /// whole window nothing is compacted. The messages come back unchanged, with no record,
    /// when the strategy is off, when the prompt plus the reservation fits the target, when
    /// the reservation fills the window, or when a measure of the original prompt fails (an
    /// old worker without `measure_prompt`, or a link error): the step then goes out
    /// uncompacted and the turn continues (the turn's first failure is logged to stderr once,
    /// see [`Self::logged_failures`]). Otherwise the strategy's result is returned, with its
    /// record. A summary saved through the sink is also kept here, even when a later measure
    /// of the same step fails. Summary calls are measured on their own, with no tools and
    /// thinking off.
    ///
    /// `notes` is the session's notes block ([`NotesBlock::text`]). Every prompt returned, on
    /// every path above, carries it on the system message (see [`NotesBlock::attach`]), and
    /// every measure of a step prompt includes it; summary calls never see it. The strategies
    /// still work on the un-noted `messages`, so a saved summary's `covers` keeps indexing
    /// the stored transcript, and the transcript itself never holds the block.
    #[allow(clippy::too_many_arguments)]
    pub async fn prepare(
        &mut self,
        messages: &[ChatMessage],
        current_turn_start: usize,
        tools: &[Value],
        enable_thinking: bool,
        max_tokens: i32,
        notes: Option<&str>,
        link: &dyn WorkerLink,
    ) -> Compacted {
        let mut out = self
            .compact(
                messages,
                current_turn_start,
                tools,
                enable_thinking,
                max_tokens,
                notes,
                link,
            )
            .await;
        out.messages = NotesBlock::attach(&out.messages, notes);
        out
    }

    /// [`Self::prepare`] before the notes block goes on the returned messages; its measures
    /// already include the block.
    #[allow(clippy::too_many_arguments)]
    async fn compact(
        &mut self,
        messages: &[ChatMessage],
        current_turn_start: usize,
        tools: &[Value],
        enable_thinking: bool,
        max_tokens: i32,
        notes: Option<&str>,
        link: &dyn WorkerLink,
    ) -> Compacted {
        if self.strategy == ContextStrategy::Off {
            return Self::unchanged(messages);
        }
        let link_measure = LinkMeasure {
            link,
            tools,
            enable_thinking,
            sampling: &self.sampling,
        };
        let measure = NotedMeasure {
            inner: &link_measure,
            notes,
        };
        let size = match measure.measure(messages).await {
            Ok(size) => size,
            Err(error) => {
                self.measure_failed(&error);
                return Self::unchanged(messages);
            }
        };
        let reservation = usize::try_from(max_tokens.max(0)).unwrap_or(usize::MAX);
        let Some(budget) = Self::target(size.n_ctx, self.compact_at_pct, reservation) else {
            return Self::unchanged(messages);
        };
        let before = size.prompt_tokens;
        if Trim::fits(before, reservation, budget) {
            return Self::unchanged(messages);
        }
        let outcome = match self.strategy {
            ContextStrategy::Off => return Self::unchanged(messages),
            ContextStrategy::Trim => {
                Trim::compact(
                    messages,
                    current_turn_start,
                    budget,
                    reservation,
                    before,
                    &measure,
                )
                .await
            }
            ContextStrategy::Summarize => {
                let keeping = KeepingSink {
                    inner: self.sink.as_ref(),
                    kept: Mutex::new(None),
                };
                let summary_measure = LinkMeasure {
                    link,
                    tools: &[],
                    enable_thinking: false,
                    sampling: &self.sampling,
                };
                let outcome = Summarize::compact(
                    messages,
                    self.committed_len,
                    current_turn_start,
                    self.saved.as_ref(),
                    budget,
                    reservation,
                    before,
                    size.n_ctx,
                    &self.sampling,
                    link,
                    &measure,
                    &summary_measure,
                    &keeping,
                )
                .await;
                if let Some(state) = keeping.into_kept() {
                    self.saved = Some(state);
                }
                outcome
            }
        };
        match outcome {
            Ok(compacted) => compacted,
            Err(error) => {
                self.measure_failed(&error);
                Self::unchanged(messages)
            }
        }
    }

    /// The budget compaction aims for: `floor(n_ctx * pct / 100)`, or `n_ctx` when
    /// `reservation` alone reaches that; `None` when `reservation` fills the whole window.
    fn target(n_ctx: usize, pct: u8, reservation: usize) -> Option<usize> {
        let budget = n_ctx.saturating_mul(usize::from(pct)) / Self::PERCENT;
        if reservation < budget {
            Some(budget)
        } else if reservation < n_ctx {
            Some(n_ctx)
        } else {
            None
        }
    }

    /// Logs the turn's first measure failure to stderr; the step still goes out uncompacted
    /// and its trace record carries no compaction.
    fn measure_failed(&mut self, error: &str) {
        if self.logged_failures == 0 {
            eprintln!("{} {}: {error}", Self::MEASURE_FAILED, self.session);
            self.logged_failures += 1;
        }
    }

    /// `messages` as they are, with no record.
    fn unchanged(messages: &[ChatMessage]) -> Compacted {
        Compacted {
            messages: messages.to_vec(),
            record: None,
        }
    }
}

/// Measures through the step's worker link, with the tools and thinking flag its `generate`
/// will use.
struct LinkMeasure<'a> {
    link: &'a dyn WorkerLink,
    tools: &'a [Value],
    enable_thinking: bool,
    sampling: &'a SamplingConfig,
}

#[async_trait]
impl PromptMeasure for LinkMeasure<'_> {
    async fn measure(&self, messages: &[ChatMessage]) -> Result<PromptSize, String> {
        let req = GenerateRequest {
            messages: messages.to_vec(),
            tools: self.tools.to_vec(),
            enable_thinking: self.enable_thinking,
            sampling: self.sampling.clone(),
        };
        self.link.measure_prompt(&req).await.map_err(|e| e.0)
    }
}

/// Measures a step prompt as it will be sent: with the notes block on its system message.
struct NotedMeasure<'a> {
    inner: &'a dyn PromptMeasure,
    notes: Option<&'a str>,
}

#[async_trait]
impl PromptMeasure for NotedMeasure<'_> {
    async fn measure(&self, messages: &[ChatMessage]) -> Result<PromptSize, String> {
        let noted = NotesBlock::attach(messages, self.notes);
        self.inner.measure(&noted).await
    }
}

/// Passes each save on to the session's sink and keeps the state it saved.
struct KeepingSink<'a> {
    inner: &'a dyn SummarySink,
    kept: Mutex<Option<CompactionState>>,
}

impl KeepingSink<'_> {
    fn into_kept(self) -> Option<CompactionState> {
        self.kept.into_inner().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait]
impl SummarySink for KeepingSink<'_> {
    async fn save(&self, state: CompactionState) -> Result<(), String> {
        self.inner.save(state.clone()).await?;
        *self.kept.lock().unwrap_or_else(|e| e.into_inner()) = Some(state);
        Ok(())
    }
}

/// The sink of a compactor that never summarises.
struct DiscardSink;

#[async_trait]
impl SummarySink for DiscardSink {
    async fn save(&self, _state: CompactionState) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::types::{FunctionCall, ToolCall};
    use crate::runtime::test_support::{FakeLink, Notes, Scripted};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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
    async fn trim_never_touches_system_note_or_current_turn() {
        let system = ChatMessage::new(Role::System, "s".repeat(BIG));
        let note = SummaryNote::system(Some(&system), &"n".repeat(BIG));
        assert!(SummaryNote::is_note(&note));
        let mut messages = vec![note.clone()];
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
        let mut expected = vec![note];
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

    #[tokio::test]
    async fn trim_without_system_message_drops_first_user_turn_keeps_current() {
        let short = json!({"ok": true});
        let mut messages = vec![Fx::user(&"a".repeat(BIG)), Fx::assistant("ok")];
        messages.extend(Fx::tool_turn(1, short.clone()));
        let current = messages.len();
        let current_turn = Fx::tool_turn(2, Fx::big());
        messages.extend(current_turn.clone());
        assert_eq!(Turns::starts(&messages), vec![0, 2, 6]);
        let before = FakeMeasure::tokens(&messages);
        let budget = before + RESERVATION - 500;
        let out = Trim::compact(
            &messages,
            current,
            budget,
            RESERVATION,
            before,
            &FakeMeasure::new(),
        )
        .await
        .unwrap();
        let mut expected = Fx::tool_turn(1, short);
        expected.extend(current_turn);
        assert_eq!(out.messages, expected);
        let record = out.record.unwrap();
        assert_eq!(record.dropped_turns, 1);
        assert_eq!(record.elided_results, 0);
        assert!(record.after_tokens as usize <= budget - RESERVATION);
    }

    #[tokio::test]
    async fn stubbed_transcript_renders_stub_in_the_matching_tool_response() {
        const GEMMA4: &str = include_str!("../../tests/fixtures/gemma4-chat-template.jinja");
        const GET_TIME: &str = "get_time";
        let mut messages = vec![Fx::system()];
        messages.extend(Fx::tool_turn(0, Fx::big()));
        messages.push(Fx::user("q1"));
        messages.push(Fx::call("c1", GET_TIME));
        messages.push(Fx::result("c1", json!({"utc": "x"})));
        messages.push(Fx::assistant("done"));
        let current = messages.len();
        messages.push(Fx::user("now"));
        let before = FakeMeasure::tokens(&messages);
        let budget = before + RESERVATION - Fx::saving() / 2;
        let out = Trim::compact(
            &messages,
            current,
            budget,
            RESERVATION,
            before,
            &FakeMeasure::new(),
        )
        .await
        .unwrap();
        assert_eq!(out.record.unwrap().elided_results, 1);

        let template = crate::engine::template::ChatTemplate::from_jinja(GEMMA4).unwrap();
        let prompt = template.render(&out.messages, true, false, &[]).unwrap();
        let bytes = serde_json::to_string(&Fx::big()).unwrap().len();
        let stubbed = format!(
            "<|tool_call>call:read_file{{}}<tool_call|><|tool_response>response:read_file{{value:<|\"|>[read_file result elided: {bytes} bytes]<|\"|>}}<tool_response|>"
        );
        assert!(prompt.contains(&stubbed), "{prompt}");
        let kept = "<|tool_call>call:get_time{}<tool_call|><|tool_response>response:get_time{utc:<|\"|>x<|\"|>}<tool_response|>";
        assert!(prompt.contains(kept), "{prompt}");
        assert!(!prompt.contains(&"x".repeat(BIG)), "{prompt}");
    }

    #[test]
    fn turns_starts_marks_each_user_message() {
        let mut messages = vec![Fx::system()];
        messages.extend(Fx::tool_turn(0, json!(1)));
        messages.push(Fx::user("q"));
        messages.push(Fx::assistant("a"));
        assert_eq!(Turns::starts(&messages), vec![1, 5]);
        assert!(Turns::starts(&[]).is_empty());
    }

    #[test]
    fn summary_note_rides_in_the_system_message() {
        let note = SummaryNote::system(Some(&Fx::system()), "s");
        assert_eq!(note.role, Role::System);
        assert_eq!(
            note.content,
            "you are helpful\n\n[Summary of the earlier conversation]\ns"
        );
        let alone = SummaryNote::system(None, "s");
        assert_eq!(alone, SummaryNote::message("s"));
        assert_eq!(alone.role, Role::System);
        assert_eq!(alone.content, "[Summary of the earlier conversation]\ns");
        let empty = ChatMessage::new(Role::System, "");
        assert_eq!(SummaryNote::system(Some(&empty), "s"), alone);
        assert!(SummaryNote::is_note(&note));
        assert!(SummaryNote::is_note(&alone));
        assert!(!SummaryNote::is_note(&Fx::system()));
        let as_user = ChatMessage::new(Role::User, SummaryNote::text("s"));
        assert!(!SummaryNote::is_note(&as_user));
    }

    // ---- summarize ----

    const N_CTX: usize = 65_536;
    const SEED: i64 = 42;
    const SUMMARY: &str = "S";
    const OLD: &str = "OLD";

    /// A summary sink that records every saved state, or refuses them all.
    struct RecordingSink {
        saved: std::sync::Mutex<Vec<CompactionState>>,
        fail: bool,
    }

    #[async_trait]
    impl SummarySink for RecordingSink {
        async fn save(&self, state: CompactionState) -> Result<(), String> {
            if self.fail {
                return Err("disk full".into());
            }
            self.saved.lock().unwrap().push(state);
            Ok(())
        }
    }

    impl RecordingSink {
        fn new() -> Self {
            Self {
                saved: std::sync::Mutex::new(Vec::new()),
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                fail: true,
                ..Self::new()
            }
        }

        fn saved(&self) -> Vec<CompactionState> {
            self.saved.lock().unwrap().clone()
        }
    }

    /// Summarize fixtures.
    struct Sx;

    impl Sx {
        /// The session's sampling: everything but the fixed fields must be copied.
        fn sampling() -> SamplingConfig {
            SamplingConfig {
                seed: SEED,
                top_k: 7,
                repeat_penalty: 1.3,
                ..SamplingConfig::default()
            }
        }

        /// A summary reply: `turn_started` (10 prompt tokens), one chunk of `tokens`, then
        /// `turn_done` with `content`.
        fn reply(content: &str, tokens: usize) -> Scripted {
            Notes::script(vec![
                Notes::started_fill("p", 10, N_CTX as u32),
                Notes::chunk_of(content, tokens),
                Notes::done(content, vec![]),
            ])
        }

        fn link(scripts: Vec<Scripted>) -> Arc<FakeLink> {
            FakeLink::new(false, scripts)
        }

        /// The system message, five tool turns (starts 1, 5, 9, 13, 17) and the current turn
        /// at 21.
        fn transcript() -> (Vec<ChatMessage>, usize) {
            let mut messages = vec![Fx::system()];
            for n in 0..5 {
                messages.extend(Fx::tool_turn(n, Fx::big()));
            }
            let current = messages.len();
            messages.push(Fx::user("now"));
            (messages, current)
        }

        fn state(summary: &str, covers: usize) -> CompactionState {
            CompactionState {
                summary: summary.into(),
                covers,
                tokens: 1,
                at: 1,
            }
        }

        /// The user content of a recorded summary request.
        fn content(req: &GenerateRequest) -> &str {
            &req.messages[1].content
        }

        async fn run(
            messages: &[ChatMessage],
            current: usize,
            saved: Option<&CompactionState>,
            budget: usize,
            n_ctx: usize,
            link: &FakeLink,
            sink: &RecordingSink,
        ) -> Compacted {
            let measure = FakeMeasure::new();
            Self::run_measured(
                messages, current, saved, budget, n_ctx, link, sink, &measure,
            )
            .await
        }

        /// Like [`Self::run`], measuring the step's prompts through `measure` (summary calls
        /// through a fresh [`FakeMeasure`]).
        #[allow(clippy::too_many_arguments)]
        async fn run_measured(
            messages: &[ChatMessage],
            current: usize,
            saved: Option<&CompactionState>,
            budget: usize,
            n_ctx: usize,
            link: &FakeLink,
            sink: &RecordingSink,
            measure: &dyn PromptMeasure,
        ) -> Compacted {
            let summary_measure = FakeMeasure::new();
            Self::run_both(
                messages,
                current,
                saved,
                budget,
                n_ctx,
                link,
                sink,
                measure,
                &summary_measure,
            )
            .await
        }

        /// Like [`Self::run_measured`], measuring summary calls through `summary_measure`.
        #[allow(clippy::too_many_arguments)]
        async fn run_both(
            messages: &[ChatMessage],
            current: usize,
            saved: Option<&CompactionState>,
            budget: usize,
            n_ctx: usize,
            link: &FakeLink,
            sink: &RecordingSink,
            measure: &dyn PromptMeasure,
            summary_measure: &dyn PromptMeasure,
        ) -> Compacted {
            let before = FakeMeasure::tokens(messages);
            Summarize::compact(
                messages,
                current,
                current,
                saved,
                budget,
                RESERVATION,
                before,
                n_ctx,
                &Self::sampling(),
                link,
                measure,
                summary_measure,
                sink,
            )
            .await
            .unwrap()
        }

        /// What trim alone makes of `messages`.
        async fn trimmed(messages: &[ChatMessage], current: usize, budget: usize) -> Compacted {
            let before = FakeMeasure::tokens(messages);
            Trim::compact(
                messages,
                current,
                budget,
                RESERVATION,
                before,
                &FakeMeasure::new(),
            )
            .await
            .unwrap()
        }
    }

    #[tokio::test]
    async fn summary_cost_recorded() {
        let (messages, current) = Sx::transcript();
        let budget = FakeMeasure::tokens(&messages);
        let reply = Notes::script(vec![
            Notes::started_fill("p", 10, N_CTX as u32),
            Notes::chunk_of("S", 3),
            Notes::note(
                "turn_done",
                json!({"content": "S", "tool_calls": [], "usage": {
                    "prompt_tokens": 77, "completion_tokens": 6, "reasoning_tokens": 0, "cost": 0.002
                }}),
            ),
        ]);
        let link = Sx::link(vec![reply]);
        let sink = RecordingSink::new();
        let out = Sx::run(&messages, current, None, budget, N_CTX, &link, &sink).await;
        let record = out.record.unwrap();
        assert_eq!(record.summary_cost, Some(0.002));
        assert_eq!(record.summary_prompt_tokens, Some(77));
        assert_eq!(record.summary_out_tokens, Some(6));
    }

    #[tokio::test]
    async fn summarize_replaces_span_keeps_tail_and_saves() {
        let (messages, current) = Sx::transcript();
        let budget = FakeMeasure::tokens(&messages);
        // The worker prefixes an opener; it is stripped and the reply trimmed.
        let link = Sx::link(vec![Sx::reply("<|channel>thought\n S \n", 3)]);
        let sink = RecordingSink::new();
        let out = Sx::run(&messages, current, None, budget, N_CTX, &link, &sink).await;
        let mut expected = vec![SummaryNote::system(Some(&Fx::system()), SUMMARY)];
        expected.extend_from_slice(&messages[17..]);
        assert_eq!(out.messages, expected);
        assert_eq!(
            out.messages[0].content,
            "you are helpful\n\n[Summary of the earlier conversation]\nS"
        );
        let saved = sink.saved();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].summary, SUMMARY);
        assert_eq!(saved[0].covers, 17);
        // The note text's own tokens, as the summary call's render counts them.
        let note = Summarizer::messages(&SummaryNote::text(SUMMARY));
        let empty = Summarizer::messages("");
        assert_eq!(
            saved[0].tokens as usize,
            FakeMeasure::tokens(&note) - FakeMeasure::tokens(&empty)
        );
        assert!(saved[0].at > 1_700_000_000_000);
        let record = out.record.unwrap();
        assert_eq!(record.strategy, "summarize");
        assert_eq!(record.summarized_messages, 16);
        assert!(record.summary_ms.is_some());
        assert_eq!(record.summary_prompt_tokens, Some(10));
        assert_eq!(record.summary_out_tokens, Some(3));
        assert!(!record.reused_summary);
        assert!(!record.fallback);
        assert_eq!(record.summary_error, None);
        assert_eq!((record.elided_results, record.dropped_turns), (0, 0));
        assert_eq!(record.before_tokens as usize, budget);
        assert_eq!(record.after_tokens as usize, FakeMeasure::tokens(&expected));
    }

    #[tokio::test]
    async fn summary_request_uses_fixed_prompt_sampling_and_no_tools() {
        const SMALL_CTX: usize = 4096;
        let (messages, current) = Sx::transcript();
        let budget = FakeMeasure::tokens(&messages);
        let link = Sx::link(vec![Sx::reply(SUMMARY, 1)]);
        let sink = RecordingSink::new();
        Sx::run(&messages, current, None, budget, SMALL_CTX, &link, &sink).await;
        let requests = link.requests();
        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, Role::System);
        assert_eq!(
            req.messages[0].content,
            "Summarize the conversation below for your own later use. Keep facts, file names, numbers, decisions and open questions. Write plain sentences, no preamble."
        );
        assert_eq!(req.messages[1].role, Role::User);
        assert!(req.tools.is_empty());
        assert!(!req.enable_thinking);
        assert_eq!(req.sampling.temperature, 0.2);
        assert_eq!(req.sampling.top_p, 0.9);
        assert_eq!(req.sampling.seed, SEED);
        assert_eq!(req.sampling.max_tokens, 512);
        assert_eq!(req.sampling.max_tokens as usize, (SMALL_CTX / 8).min(1024));
        assert_eq!(Summarize::max_tokens(N_CTX), 1024);
        // Fixed, not the session's top_k 7 and repeat_penalty 1.3.
        assert_eq!((req.sampling.top_k, req.sampling.repeat_penalty), (40, 1.0));
        let lines: Vec<&str> = Sx::content(req).lines().collect();
        // Four summarised turns of four messages each.
        assert_eq!(lines.len(), 16);
        assert_eq!(lines[0], "user: q0");
        assert_eq!(lines[1], "assistant: read_file({})");
        let result = serde_json::to_string(&Fx::big()).unwrap();
        assert_eq!(lines[2], format!("tool: {}", &result[..2000]));
        assert_eq!(lines[3], "assistant: done");
        assert_eq!(lines[12], "user: q3");
        assert!(!Sx::content(req).contains("q4"));
    }

    #[tokio::test]
    async fn saved_summary_is_reused_without_a_call() {
        let (messages, current) = Sx::transcript();
        let budget = FakeMeasure::tokens(&messages);
        let saved = Sx::state(OLD, 17);
        let link = Sx::link(vec![]);
        let sink = RecordingSink::new();
        let out = Sx::run(
            &messages,
            current,
            Some(&saved),
            budget,
            N_CTX,
            &link,
            &sink,
        )
        .await;
        assert!(link.requests().is_empty());
        assert!(sink.saved().is_empty());
        let mut expected = vec![SummaryNote::system(Some(&Fx::system()), OLD)];
        expected.extend_from_slice(&messages[17..]);
        assert_eq!(out.messages, expected);
        let record = out.record.unwrap();
        assert!(record.reused_summary);
        assert_eq!(record.summary_ms, None);
        assert_eq!(record.summarized_messages, 16);
        assert_eq!(record.summary_prompt_tokens, None);
        assert_eq!(record.after_tokens as usize, FakeMeasure::tokens(&expected));
    }

    #[tokio::test]
    async fn resummary_folds_old_summary_and_new_messages() {
        let (messages, current) = Sx::transcript();
        // Reusing the summary over 1..9 leaves turns 9, 13 and 17 verbatim: too big. A new
        // summary up to 17 leaves only the previous turn.
        let mut two_turns = vec![SummaryNote::system(Some(&Fx::system()), OLD)];
        two_turns.extend_from_slice(&messages[13..]);
        let budget = FakeMeasure::tokens(&two_turns) + RESERVATION;
        let saved = Sx::state(OLD, 9);
        let link = Sx::link(vec![Sx::reply(SUMMARY, 1)]);
        let sink = RecordingSink::new();
        let out = Sx::run(
            &messages,
            current,
            Some(&saved),
            budget,
            N_CTX,
            &link,
            &sink,
        )
        .await;
        let requests = link.requests();
        assert_eq!(requests.len(), 1);
        let content = Sx::content(&requests[0]);
        assert!(content.starts_with("OLD\n\nuser: q2\n"), "{content}");
        assert!(content.contains("user: q3"));
        assert!(!content.contains("q1"));
        assert!(!content.contains("q4"));
        let saved_now = sink.saved();
        assert_eq!(saved_now.len(), 1);
        assert_eq!(saved_now[0].covers, 17);
        assert!(saved_now[0].covers > saved.covers);
        let mut expected = vec![SummaryNote::system(Some(&Fx::system()), SUMMARY)];
        expected.extend_from_slice(&messages[17..]);
        assert_eq!(out.messages, expected);
        let record = out.record.unwrap();
        assert!(!record.reused_summary);
        assert_eq!(record.summarized_messages, 16);
    }

    impl Sx {
        /// A window whose summary input fits one long turn per call.
        const CHUNK_CTX: usize = 2048;

        /// The system message, `turns` long user turns (each a letter repeated 4,000 times,
        /// then `done`), a short previous turn and the current turn.
        fn long_turns(turns: usize) -> (Vec<ChatMessage>, usize) {
            Self::long_turns_of(&vec![BIG; turns])
        }

        /// Like [`Self::long_turns`], with the user text of turn `n` `lens[n]` long.
        fn long_turns_of(lens: &[usize]) -> (Vec<ChatMessage>, usize) {
            let mut messages = vec![Fx::system()];
            for (n, len) in lens.iter().enumerate() {
                let letter = char::from(b'a' + n as u8);
                messages.push(Fx::user(&letter.to_string().repeat(*len)));
                messages.push(Fx::assistant("done"));
            }
            messages.push(Fx::user("prev"));
            messages.push(Fx::assistant("done"));
            let current = messages.len();
            messages.push(Fx::user("now"));
            (messages, current)
        }
    }

    #[tokio::test]
    async fn long_span_is_chunked_oldest_first_up_to_four_calls() {
        let (messages, current) = Sx::long_turns(4);
        let budget = FakeMeasure::tokens(&messages);
        let replies = (1..=4).map(|k| Sx::reply(&format!("S{k}"), 1)).collect();
        let link = Sx::link(replies);
        let sink = RecordingSink::new();
        let out = Sx::run(
            &messages,
            current,
            None,
            budget,
            Sx::CHUNK_CTX,
            &link,
            &sink,
        )
        .await;
        let requests = link.requests();
        assert_eq!(requests.len(), 4);
        for (k, req) in requests.iter().enumerate() {
            let letter = char::from(b'a' + k as u8).to_string().repeat(BIG);
            let content = Sx::content(req);
            let expected = format!("user: {letter}\nassistant: done");
            if k == 0 {
                assert_eq!(content, expected);
            } else {
                assert_eq!(content, format!("S{k}\n\n{expected}"));
            }
        }
        let saved = sink.saved();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].summary, "S4");
        assert_eq!(saved[0].covers, 9);
        let mut expected = vec![SummaryNote::system(Some(&Fx::system()), "S4")];
        expected.extend_from_slice(&messages[9..]);
        assert_eq!(out.messages, expected);
        let record = out.record.unwrap();
        assert_eq!(record.summarized_messages, 8);
        assert_eq!(record.summary_prompt_tokens, Some(40));
        assert_eq!(record.summary_out_tokens, Some(4));
        assert!(record.summary_ms.is_some());
        assert!(!record.fallback);
    }

    #[tokio::test]
    async fn span_needing_five_chunks_falls_back_to_trim() {
        // The sixth turn is too big for any call. Planning stops when four chunks leave
        // lines over, so the reason is the call limit, not the oversized message.
        let (messages, current) = Sx::long_turns_of(&[BIG, BIG, BIG, BIG, BIG, 5 * BIG]);
        let budget = FakeMeasure::tokens(&messages) - 1500;
        // No script: the plan fails before any call.
        let link = Sx::link(vec![]);
        let sink = RecordingSink::new();
        let out = Sx::run(
            &messages,
            current,
            None,
            budget,
            Sx::CHUNK_CTX,
            &link,
            &sink,
        )
        .await;
        assert!(link.requests().is_empty());
        assert!(sink.saved().is_empty());
        let trimmed = Sx::trimmed(&messages, current, budget).await;
        assert_eq!(out.messages, trimmed.messages);
        let record = out.record.unwrap();
        assert!(record.fallback);
        assert_eq!(record.strategy, "summarize");
        let error = record.summary_error.unwrap();
        assert_eq!(
            error,
            "the span needs more summary calls than the limit of 4"
        );
        assert_eq!(record.summary_ms, None);
        assert_eq!(record.dropped_turns, trimmed.record.unwrap().dropped_turns);
        assert!(record.dropped_turns > 0);
    }

    #[tokio::test]
    async fn summary_failure_falls_back_to_trim_with_reason() {
        let max = Summarize::max_tokens(N_CTX);
        let rows: Vec<(&str, Scripted, &str)> = vec![
            (
                "link error",
                Err(LinkError("socket down".into())),
                "socket down",
            ),
            (
                "turn_error",
                Notes::script(vec![
                    Notes::started_fill("p", 10, N_CTX as u32),
                    Notes::note("turn_error", json!({"message": "boom"})),
                ]),
                "boom",
            ),
            (
                "turn_rejected",
                Notes::script(vec![Notes::rejected(70_000, 65_536)]),
                "rejected",
            ),
            ("empty reply", Sx::reply("  \n ", 2), "empty"),
            ("max_tokens hit", Sx::reply("cut off", max), "max_tokens"),
            (
                "max_tokens hit by the usage count",
                Notes::script(vec![
                    Notes::started_fill("p", 10, N_CTX as u32),
                    Notes::chunk_of("cut off", 0),
                    Notes::note(
                        "turn_done",
                        json!({"content": "cut off", "tool_calls": [], "usage": {
                            "prompt_tokens": 10, "completion_tokens": max
                        }}),
                    ),
                ]),
                "max_tokens",
            ),
            (
                "stream ends without turn_done",
                Notes::script(vec![
                    Notes::started_fill("p", 10, N_CTX as u32),
                    Notes::chunk_of("half", 1),
                ]),
                "ended without turn_done",
            ),
        ];
        let (messages, current) = Sx::transcript();
        let budget = FakeMeasure::tokens(&messages) - 1500;
        let trimmed = Sx::trimmed(&messages, current, budget).await;
        for (row, script, reason) in rows {
            let link = Sx::link(vec![script]);
            let sink = RecordingSink::new();
            // A saved summary that no longer fits is re-made, fails, and stays as it was.
            let saved = Sx::state(OLD, 5);
            let out = Sx::run(
                &messages,
                current,
                Some(&saved),
                budget,
                N_CTX,
                &link,
                &sink,
            )
            .await;
            assert_eq!(link.requests().len(), 1, "{row}");
            assert!(sink.saved().is_empty(), "{row}");
            assert_eq!(out.messages, trimmed.messages, "{row}");
            assert_ne!(out.messages, messages, "{row}");
            let record = out.record.unwrap();
            assert_eq!(record.strategy, "summarize", "{row}");
            assert!(record.fallback, "{row}");
            let error = record.summary_error.unwrap_or_default();
            assert!(error.contains(reason), "{row}: {error}");
            assert_eq!(
                record.elided_results,
                trimmed.record.as_ref().unwrap().elided_results,
                "{row}"
            );
            assert_eq!(
                record.after_tokens,
                trimmed.record.as_ref().unwrap().after_tokens,
                "{row}"
            );
        }
    }

    #[tokio::test]
    async fn failed_save_falls_back_to_trim() {
        let (messages, current) = Sx::transcript();
        let budget = FakeMeasure::tokens(&messages) - 1500;
        let link = Sx::link(vec![Sx::reply(SUMMARY, 1)]);
        let sink = RecordingSink::failing();
        let out = Sx::run(&messages, current, None, budget, N_CTX, &link, &sink).await;
        let trimmed = Sx::trimmed(&messages, current, budget).await;
        assert_eq!(out.messages, trimmed.messages);
        let record = out.record.unwrap();
        assert!(record.fallback);
        assert!(record.summary_error.unwrap().contains("disk full"));
        // The call was made, so its cost is still recorded.
        assert_eq!(record.summary_prompt_tokens, Some(10));
    }

    #[tokio::test]
    async fn still_over_budget_after_summary_is_trimmed_too() {
        let mut messages = vec![Fx::system()];
        for n in 0..3 {
            messages.extend(Fx::tool_turn(n, json!({"ok": n})));
        }
        // The previous turn (13..17) carries a big result.
        messages.extend(Fx::tool_turn(3, Fx::big()));
        let current = messages.len();
        messages.push(Fx::user("now"));
        let mut expected = vec![SummaryNote::system(Some(&Fx::system()), SUMMARY)];
        let mut prev = Fx::tool_turn(3, Fx::big());
        prev[2].tool_result = Some(Fx::stub(&Fx::big()));
        expected.extend(prev);
        expected.push(Fx::user("now"));
        let budget = FakeMeasure::tokens(&expected) + RESERVATION;
        let link = Sx::link(vec![Sx::reply(SUMMARY, 1)]);
        let sink = RecordingSink::new();
        let out = Sx::run(&messages, current, None, budget, N_CTX, &link, &sink).await;
        assert_eq!(out.messages, expected);
        assert_eq!(sink.saved()[0].covers, 13);
        let record = out.record.unwrap();
        assert_eq!(record.strategy, "summarize");
        assert!(!record.fallback);
        assert_eq!(record.elided_results, 1);
        assert_eq!(record.dropped_turns, 0);
        assert_eq!(record.summarized_messages, 12);
        assert!(record.summary_ms.is_some());
        assert_eq!(record.after_tokens as usize, FakeMeasure::tokens(&expected));
    }

    /// A summary-call measure that fails on the note text and otherwise counts like
    /// `FakeMeasure`.
    struct NoteFailingMeasure(FakeMeasure);

    #[async_trait]
    impl PromptMeasure for NoteFailingMeasure {
        async fn measure(&self, messages: &[ChatMessage]) -> Result<PromptSize, String> {
            let note = messages
                .iter()
                .any(|m| m.role == Role::User && m.content.starts_with(SummaryNote::HEADER));
            if note {
                return Err("tokenizer gone".into());
            }
            self.0.measure(messages).await
        }
    }

    #[tokio::test]
    async fn empty_span_with_saved_summary_trims_the_reused_prompt() {
        let (messages, current) = Sx::transcript();
        // The saved summary already covers everything before the previous turn, but the
        // reused prompt is still over budget: trim runs on it, with no summary call.
        let saved = Sx::state(OLD, 17);
        let mut expected = vec![SummaryNote::system(Some(&Fx::system()), OLD)];
        let mut prev = Fx::tool_turn(4, Fx::big());
        prev[2].tool_result = Some(Fx::stub(&Fx::big()));
        expected.extend(prev);
        expected.push(Fx::user("now"));
        let budget = FakeMeasure::tokens(&expected) + RESERVATION;
        let link = Sx::link(vec![]);
        let sink = RecordingSink::new();
        let measure = FakeMeasure::new();
        let out = Sx::run_measured(
            &messages,
            current,
            Some(&saved),
            budget,
            N_CTX,
            &link,
            &sink,
            &measure,
        )
        .await;
        assert!(link.requests().is_empty());
        assert!(sink.saved().is_empty());
        assert_eq!(out.messages, expected);
        // One measure of the reused prompt, one after the stub: never the same prompt twice.
        assert_eq!(measure.calls.load(Ordering::SeqCst), 2);
        let record = out.record.unwrap();
        assert!(record.reused_summary);
        assert!(!record.fallback);
        assert_eq!(record.summary_error, None);
        assert_eq!(record.summary_ms, None);
        assert_eq!(record.summarized_messages, 16);
        assert_eq!(record.elided_results, 1);
        assert_eq!(record.after_tokens as usize, FakeMeasure::tokens(&expected));
    }

    #[tokio::test]
    async fn nothing_to_summarize_falls_back_to_trim() {
        // Only the previous turn precedes the current one, and there is no saved summary.
        let mut messages = vec![Fx::system()];
        messages.extend(Fx::tool_turn(0, Fx::big()));
        let current = messages.len();
        messages.push(Fx::user("now"));
        let budget = FakeMeasure::tokens(&messages) - 500;
        let link = Sx::link(vec![]);
        let sink = RecordingSink::new();
        let out = Sx::run(&messages, current, None, budget, N_CTX, &link, &sink).await;
        assert!(link.requests().is_empty());
        assert!(sink.saved().is_empty());
        let trimmed = Sx::trimmed(&messages, current, budget).await;
        assert_eq!(out.messages, trimmed.messages);
        assert_ne!(out.messages, messages);
        let record = out.record.unwrap();
        assert!(record.fallback);
        assert_eq!(
            record.summary_error.as_deref(),
            Some("nothing to summarize before the kept tail")
        );
        assert_eq!(record.elided_results, 1);
    }

    #[tokio::test]
    async fn failed_note_measure_falls_back_to_trim() {
        let (messages, current) = Sx::transcript();
        let budget = FakeMeasure::tokens(&messages) - 1500;
        let link = Sx::link(vec![Sx::reply(SUMMARY, 1)]);
        let sink = RecordingSink::new();
        let summary_measure = NoteFailingMeasure(FakeMeasure::new());
        let out = Sx::run_both(
            &messages,
            current,
            None,
            budget,
            N_CTX,
            &link,
            &sink,
            &FakeMeasure::new(),
            &summary_measure,
        )
        .await;
        assert_eq!(link.requests().len(), 1);
        assert!(sink.saved().is_empty());
        let trimmed = Sx::trimmed(&messages, current, budget).await;
        assert_eq!(out.messages, trimmed.messages);
        let record = out.record.unwrap();
        assert!(record.fallback);
        assert_eq!(
            record.summary_error.as_deref(),
            Some("measuring the summary note failed: tokenizer gone")
        );
        assert_eq!(record.summary_prompt_tokens, Some(10));
    }

    /// A step measure that rejects any prompt starting with the summary note, as a chat
    /// template raising on it would, and otherwise counts like `FakeMeasure`.
    struct NoteRejectingMeasure(FakeMeasure);

    impl NoteRejectingMeasure {
        const REJECTED: &'static str = "template rejected the prompt";
    }

    #[async_trait]
    impl PromptMeasure for NoteRejectingMeasure {
        async fn measure(&self, messages: &[ChatMessage]) -> Result<PromptSize, String> {
            if messages.first().is_some_and(SummaryNote::is_note) {
                return Err(Self::REJECTED.into());
            }
            self.0.measure(messages).await
        }
    }

    #[tokio::test]
    async fn rejected_summarized_prompt_falls_back_to_trim_with_reason() {
        let (messages, current) = Sx::transcript();
        let budget = FakeMeasure::tokens(&messages) - 1500;
        let trimmed = Sx::trimmed(&messages, current, budget).await;
        let expected = format!(
            "measuring the summarized prompt failed: {}",
            NoteRejectingMeasure::REJECTED
        );
        // A fresh summary is made and saved, then its prompt is rejected.
        let link = Sx::link(vec![Sx::reply(SUMMARY, 1)]);
        let sink = RecordingSink::new();
        let measure = NoteRejectingMeasure(FakeMeasure::new());
        let out = Sx::run_measured(
            &messages, current, None, budget, N_CTX, &link, &sink, &measure,
        )
        .await;
        assert_eq!(link.requests().len(), 1);
        assert_eq!(sink.saved().len(), 1);
        assert_eq!(out.messages, trimmed.messages);
        let record = out.record.unwrap();
        assert!(record.fallback);
        assert_eq!(record.summary_error.as_deref(), Some(expected.as_str()));
        assert_eq!(record.summary_prompt_tokens, Some(10));
        // A saved summary whose reused prompt is rejected: no call, the same fallback.
        let saved = Sx::state(OLD, 17);
        let link = Sx::link(vec![]);
        let sink = RecordingSink::new();
        let out = Sx::run_measured(
            &messages,
            current,
            Some(&saved),
            budget,
            N_CTX,
            &link,
            &sink,
            &measure,
        )
        .await;
        assert!(link.requests().is_empty());
        assert!(sink.saved().is_empty());
        assert_eq!(out.messages, trimmed.messages);
        let record = out.record.unwrap();
        assert!(record.fallback);
        assert!(!record.reused_summary);
        assert_eq!(record.summary_error.as_deref(), Some(expected.as_str()));
        assert_eq!(record.summary_ms, None);
    }

    /// A chat template that, like Gemma 3's, folds the system message into the first user
    /// turn and raises unless the remaining roles alternate user/assistant.
    const STRICT_ALTERNATION: &str = concat!(
        "{{ bos_token }}",
        "{%- if messages[0]['role'] == 'system' -%}",
        "{%- set first_user_prefix = messages[0]['content'] + '\\n\\n' -%}",
        "{%- set loop_messages = messages[1:] -%}",
        "{%- else -%}",
        "{%- set first_user_prefix = '' -%}",
        "{%- set loop_messages = messages -%}",
        "{%- endif -%}",
        "{%- for message in loop_messages -%}",
        "{%- if (message['role'] == 'user') != (loop.index0 % 2 == 0) -%}",
        "{{ raise_exception('Conversation roles must alternate user/assistant/user/assistant/...') }}",
        "{%- endif -%}",
        "{%- if message['role'] == 'assistant' -%}{%- set role = 'model' -%}",
        "{%- else -%}{%- set role = message['role'] -%}{%- endif -%}",
        "{{ '<start_of_turn>' + role + '\\n' + (first_user_prefix if loop.first else '') }}",
        "{{ message['content'] | trim }}<end_of_turn>\n",
        "{%- endfor -%}",
        "{%- if add_generation_prompt -%}<start_of_turn>model\n{%- endif -%}",
    );

    #[tokio::test]
    async fn summarized_prompt_renders_through_gemma4_and_strict_alternation() {
        const GEMMA4: &str = include_str!("../../tests/fixtures/gemma4-chat-template.jinja");
        let gemma4 = crate::engine::template::ChatTemplate::from_jinja(GEMMA4).unwrap();
        let strict = crate::engine::template::ChatTemplate::from_jinja(STRICT_ALTERNATION).unwrap();
        let (with_system, current) = Sx::long_turns(4);
        let without_system = with_system[1..].to_vec();
        for (messages, current) in [(with_system, current), (without_system, current - 1)] {
            let budget = FakeMeasure::tokens(&messages);
            let link = Sx::link(vec![Sx::reply(SUMMARY, 1)]);
            let sink = RecordingSink::new();
            let out = Sx::run(&messages, current, None, budget, N_CTX, &link, &sink).await;
            let record = out.record.unwrap();
            assert!(!record.fallback, "{:?}", record.summary_error);
            assert!(SummaryNote::is_note(&out.messages[0]));
            assert_eq!(out.messages.len(), 4);
            for template in [&gemma4, &strict] {
                let prompt = template.render(&out.messages, true, false, &[]).unwrap();
                assert!(prompt.contains(SummaryNote::HEADER), "{prompt}");
            }
        }
        // The old shape, a separate user note before the kept tail, breaks the alternation.
        let old_shape = vec![
            Fx::system(),
            ChatMessage::new(Role::User, SummaryNote::text(SUMMARY)),
            Fx::user("prev"),
            Fx::assistant("done"),
        ];
        let error = strict.render(&old_shape, true, false, &[]).unwrap_err();
        assert!(error.to_string().contains("must alternate"), "{error}");
    }

    #[tokio::test]
    async fn span_lines_drop_thinking_channels_but_the_transcript_keeps_them() {
        const THOUGHT: &str = "<|channel>thought\nlong private reasoning<channel|>The answer.";
        const OPEN_TAIL: &str = "Partial.<|channel>thought\nunfinished reasoning";
        let (mut messages, current) = Sx::transcript();
        messages[4].content = THOUGHT.into();
        messages[8].content = OPEN_TAIL.into();
        let budget = FakeMeasure::tokens(&messages);
        let link = Sx::link(vec![Sx::reply(SUMMARY, 1)]);
        let sink = RecordingSink::new();
        Sx::run(&messages, current, None, budget, N_CTX, &link, &sink).await;
        let requests = link.requests();
        let content = Sx::content(&requests[0]);
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines[3], "assistant: The answer.");
        assert_eq!(lines[7], "assistant: Partial.");
        assert!(!content.contains(OpenChannel::OPEN), "{content}");
        assert!(!content.contains(OpenChannel::CLOSE), "{content}");
        assert!(!content.contains("reasoning"), "{content}");
        assert_eq!(messages[4].content, THOUGHT);
        assert_eq!(messages[8].content, OPEN_TAIL);
    }

    #[tokio::test]
    async fn chunk_planning_binary_searches_each_chunk_end() {
        const LINES: usize = 40;
        let lines: Vec<String> = (0..LINES)
            .map(|_| format!("user: {}", "x".repeat(200)))
            .collect();
        let link = Sx::link(vec![]);
        let measure = FakeMeasure::new();
        let summarizer = Summarizer::new(link.as_ref(), &measure, &Sx::sampling(), Sx::CHUNK_CTX);
        let chunks = summarizer.plan(None, &lines).await.unwrap();
        assert_eq!(chunks, vec![0..33, 33..LINES]);
        // One whole-span measure, then a binary search per chunk end: 6 for the first chunk
        // (over 0..40) and 3 for the second (over 33..41), not one measure per line.
        assert_eq!(measure.calls.load(Ordering::SeqCst), 10);
        let window = Sx::CHUNK_CTX - Summarize::max_tokens(Sx::CHUNK_CTX);
        let first = Summarizer::messages(&Summarizer::content(None, &lines[chunks[0].clone()]));
        assert!(FakeMeasure::tokens(&first) <= window);
        let more = Summarizer::messages(&Summarizer::content(None, &lines[0..34]));
        assert!(FakeMeasure::tokens(&more) > window);
    }

    #[test]
    fn target_is_the_window_when_the_reservation_reaches_the_budget() {
        // 75% of 8192 is 6144.
        assert_eq!(Compactor::target(8192, 75, 1000), Some(6144));
        assert_eq!(Compactor::target(8192, 75, 6144), Some(8192));
        assert_eq!(Compactor::target(8192, 75, 6500), Some(8192));
        assert_eq!(Compactor::target(8192, 75, 8192), None);
        assert_eq!(Compactor::target(8192, 75, 9000), None);
    }

    /// Notes-block fixtures over [`Compactor::prepare`].
    struct Nx;

    impl Nx {
        /// The window [`Self::by_bytes`] reports; the default budget is 1500.
        const N_CTX: usize = 2000;
        /// The length of each long question in [`Self::long_turns`].
        const LONG: usize = 600;
        const NOTES: &'static str = "[Your notes]\n1. keep going";
        const NO_LIMIT: i32 = -1;

        /// One token per byte of the request's serialized messages.
        fn by_bytes(req: &GenerateRequest) -> PromptSize {
            PromptSize {
                prompt_tokens: serde_json::to_string(&req.messages).unwrap().len(),
                n_ctx: Self::N_CTX,
            }
        }

        fn compactor(
            strategy: ContextStrategy,
            saved: Option<CompactionState>,
            committed_len: usize,
            sink: Arc<RecordingSink>,
        ) -> Compactor {
            Compactor::new(
                strategy,
                DEFAULT_COMPACT_AT_PCT,
                saved,
                committed_len,
                SamplingConfig::default(),
                sink,
            )
        }

        /// `turns` plain turns with long questions.
        fn long_turns(turns: usize) -> Vec<ChatMessage> {
            (0..turns)
                .flat_map(|n| {
                    let q = char::from(b'a' + n as u8).to_string().repeat(Self::LONG);
                    [Fx::user(&q), Fx::assistant("ok")]
                })
                .collect()
        }

        /// `content` with the notes block after it and a blank line.
        fn noted(content: &str) -> String {
            format!("{content}\n\n{}", Self::NOTES)
        }

        /// The step measures `link` made (the summary calls left out).
        fn step_measures(link: &FakeLink) -> Vec<GenerateRequest> {
            link.measured()
                .into_iter()
                .filter(|r| r.messages[0].content != Summarize::SYSTEM_PROMPT)
                .collect()
        }

        async fn prepare(
            compactor: &mut Compactor,
            messages: &[ChatMessage],
            current: usize,
            link: &FakeLink,
        ) -> Compacted {
            compactor
                .prepare(
                    messages,
                    current,
                    &[],
                    false,
                    Self::NO_LIMIT,
                    Some(Self::NOTES),
                    link,
                )
                .await
        }
    }

    #[tokio::test]
    async fn notes_block_follows_summary_note() {
        let mut messages = vec![Fx::system()];
        messages.extend(Nx::long_turns(3));
        let current = messages.len();
        messages.push(Fx::user("now"));
        let link = Sx::link(vec![Sx::reply(SUMMARY, 1)]);
        link.set_measure_by(Nx::by_bytes);
        let sink = Arc::new(RecordingSink::new());
        let mut compactor = Nx::compactor(ContextStrategy::Summarize, None, current, sink.clone());
        let out = Nx::prepare(&mut compactor, &messages, current, &link).await;
        let covers = sink.saved()[0].covers;
        assert_eq!(covers, 5);
        assert_eq!(
            out.messages[0].content,
            Nx::noted("you are helpful\n\n[Summary of the earlier conversation]\nS")
        );
        assert_eq!(out.messages[1..], messages[covers..]);
        let record = out.record.unwrap();
        assert_eq!(record.strategy, Summarize::STRATEGY);
        // Every step measure carries the block, the last one is the prompt sent; the summary
        // calls never carry it.
        let steps = Nx::step_measures(&link);
        assert!(steps.len() >= 2);
        assert!(steps
            .iter()
            .all(|r| r.messages[0].content.ends_with(Nx::NOTES)));
        let last = steps.last().unwrap();
        assert_eq!(last.messages, out.messages);
        assert_eq!(
            record.after_tokens as usize,
            Nx::by_bytes(last).prompt_tokens
        );
        let calls = link.requests();
        assert_eq!(calls.len(), 1);
        assert!(!calls[0].messages[1].content.contains(NotesBlock::HEADER));
        let measured = link.measured();
        let summary_measures: Vec<_> = measured
            .iter()
            .filter(|r| r.messages[0].content == Summarize::SYSTEM_PROMPT)
            .collect();
        assert!(
            !summary_measures.is_empty(),
            "the summary call is measured too"
        );
        assert!(summary_measures
            .iter()
            .all(|r| !r.messages[1].content.contains(NotesBlock::HEADER)));
    }

    #[tokio::test]
    async fn notes_block_is_measured_under_trim() {
        let mut messages = vec![Fx::system()];
        messages.extend(Fx::tool_turn(0, Fx::big()));
        let current = messages.len();
        messages.push(Fx::user("now"));
        let link = Sx::link(vec![]);
        link.set_measure_by(Nx::by_bytes);
        let sink = Arc::new(RecordingSink::new());
        let mut compactor = Nx::compactor(ContextStrategy::Trim, None, current, sink);
        let out = Nx::prepare(&mut compactor, &messages, current, &link).await;
        assert_eq!(out.messages[0].content, Nx::noted("you are helpful"));
        assert_eq!(out.messages[3].tool_result, Some(Fx::stub(&Fx::big())));
        let measured = link.measured();
        assert!(measured.len() >= 2, "the original and the trimmed prompt");
        assert!(measured
            .iter()
            .all(|r| r.messages[0].content == Nx::noted("you are helpful")));
        let record = out.record.unwrap();
        assert_eq!(record.strategy, Trim::STRATEGY);
        let last = measured.last().unwrap();
        assert_eq!(last.messages, out.messages);
        assert_eq!(
            record.after_tokens as usize,
            Nx::by_bytes(last).prompt_tokens
        );
    }

    #[tokio::test]
    async fn notes_block_without_system_prompt_keeps_summary_reuse() {
        // Turn 1: no system prompt; the span before the previous turn is summarised.
        let mut stored = Nx::long_turns(3);
        let current = stored.len();
        let mut messages = stored.clone();
        messages.push(Fx::user("now"));
        let link = Sx::link(vec![Sx::reply(SUMMARY, 1)]);
        link.set_measure_by(Nx::by_bytes);
        let sink = Arc::new(RecordingSink::new());
        let mut compactor = Nx::compactor(ContextStrategy::Summarize, None, current, sink.clone());
        let out = Nx::prepare(&mut compactor, &messages, current, &link).await;
        let saved = sink.saved();
        assert_eq!(saved.len(), 1);
        // A stored-transcript index: the turn before the current one starts at 4.
        assert_eq!(saved[0].covers, 4);
        assert_eq!(out.messages[0].role, Role::System);
        assert_eq!(
            out.messages[0].content,
            Nx::noted("[Summary of the earlier conversation]\nS")
        );
        assert_eq!(out.messages[1..], messages[4..]);
        assert!(!out.record.unwrap().reused_summary);

        // Turn 2 starts from the committed transcript and the saved summary.
        stored.push(Fx::user("now"));
        stored.push(Fx::assistant("done"));
        let current = stored.len();
        let mut messages = stored.clone();
        messages.push(Fx::user("next"));
        let link = Sx::link(vec![]);
        link.set_measure_by(Nx::by_bytes);
        let mut compactor = Nx::compactor(
            ContextStrategy::Summarize,
            Some(saved[0].clone()),
            current,
            sink.clone(),
        );
        let out = Nx::prepare(&mut compactor, &messages, current, &link).await;
        assert!(link.requests().is_empty(), "the summary is reused");
        assert_eq!(sink.saved().len(), 1);
        let record = out.record.unwrap();
        assert!(record.reused_summary);
        assert_eq!(record.summarized_messages, 4);
        assert_eq!(
            out.messages[0].content,
            Nx::noted("[Summary of the earlier conversation]\nS")
        );
        assert_eq!(out.messages[1..], messages[4..]);
    }

    #[tokio::test]
    async fn off_strategy_still_carries_notes_block() {
        let link = Sx::link(vec![]);
        let messages = vec![Fx::user("q")];
        let out = Nx::prepare(&mut Compactor::off(), &messages, 0, &link).await;
        assert_eq!(
            out.messages,
            vec![ChatMessage::new(Role::System, Nx::NOTES), Fx::user("q")]
        );
        assert_eq!(out.record, None);
        let messages = vec![Fx::system(), Fx::user("q")];
        let out = Nx::prepare(&mut Compactor::off(), &messages, 1, &link).await;
        assert_eq!(out.messages[0].content, Nx::noted("you are helpful"));
        assert_eq!(out.messages[1..], messages[1..]);
        assert!(link.measured().is_empty());
        // No notes: the messages go out as they are.
        let out = Compactor::off()
            .prepare(&messages, 1, &[], false, Nx::NO_LIMIT, None, link.as_ref())
            .await;
        assert_eq!(out.messages, messages);
    }

    #[tokio::test]
    async fn under_budget_and_failed_measure_still_carry_notes_block() {
        let messages = vec![Fx::system(), Fx::user("q")];
        let sink = Arc::new(RecordingSink::new());
        // No measure set: measure_prompt fails, as on an old worker.
        let failing = Sx::link(vec![]);
        let mut compactor = Nx::compactor(ContextStrategy::Trim, None, 1, sink.clone());
        let out = Nx::prepare(&mut compactor, &messages, 1, &failing).await;
        assert_eq!(out.messages[0].content, Nx::noted("you are helpful"));
        assert_eq!(out.record, None);
        assert_eq!(compactor.logged_failures(), 1);
        let fitting = Sx::link(vec![]);
        fitting.set_measure_by(Nx::by_bytes);
        let mut compactor = Nx::compactor(ContextStrategy::Summarize, None, 1, sink);
        let out = Nx::prepare(&mut compactor, &messages, 1, &fitting).await;
        assert_eq!(out.messages[0].content, Nx::noted("you are helpful"));
        assert_eq!(out.record, None);
        assert_eq!(
            fitting.measured()[0].messages[0].content,
            Nx::noted("you are helpful")
        );
    }
}
