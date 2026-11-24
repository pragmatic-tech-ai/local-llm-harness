//! Context compaction: the seam a strategy measures prompts through, the shared turn and
//! summary-note helpers, the **trim** strategy (stub out old tool results, then drop old
//! turns, until the prompt fits a budget) and the **summarize** strategy (replace the older
//! span with a saved summary note, trimming on top or instead when needed).

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;

use super::tool_loop::ToolLoop;
use super::trace::{CompactionRecord, ToolTiming};
use super::worker::{GenerateRequest, LinkError, PromptSize, WorkerLink};
use crate::agents::template::CompactionState;
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
/// between the system message and them with one summary note, made by an extra `generate` on
/// the worker link (chunked when the span is too big for one call) and saved through a
/// [`SummarySink`] so later steps and turns reuse it.
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
    const ERR_NOTHING: &'static str = "nothing to summarize before the kept tail";
    const ERR_SAVE: &'static str = "saving the summary failed: ";
    const ERR_NOTE_MEASURE: &'static str = "measuring the summary note failed: ";

    /// A summary call's `max_tokens`: `min(1024, n_ctx / 8)`.
    pub fn max_tokens(n_ctx: usize) -> usize {
        Self::MAX_TOKENS_CAP.min(n_ctx / Self::CTX_DIVISOR)
    }

    /// Compacts `messages` (the committed transcript `..committed_len` plus the current turn's
    /// scratch from `current_turn_start`) so the prompt plus `reservation` fits `budget`.
    ///
    /// - **Reuse.** A `saved` summary whose `covers` lies after the system message and within
    ///   the committed transcript (and before the current turn) gives the system message, the
    ///   note, then `messages[covers..]`. If that fits, it is sent with `reused_summary`.
    /// - **Summarise.** Otherwise the span from after the system message (or from
    ///   `saved.covers`, with the old summary text folded in) up to the start of the turn
    ///   before the current one is summarised over `link`, saved through `sink` with `covers`
    ///   set to the span's end index, and the summarised prompt is measured. If it is still
    ///   over budget, [`Trim`] runs on it and its counts are merged in. When there is nothing
    ///   new to summarise but a saved summary exists, trim runs on the reused prompt.
    /// - **Fallback.** Any summary failure, including a failed save, runs [`Trim`] on the
    ///   original messages with `fallback` true and the cause in `summary_error`.
    ///
    /// `covers` is a transcript index: the summary replaces `messages[system_len..covers]`,
    /// and `summarized_messages` is that count. The record is always `Some`, with `strategy`
    /// `"summarize"` and `after_tokens` from the last measure. `Err` carries a measure error
    /// on the path that sends a prompt (as [`Trim::compact`] does).
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
        let saved = saved.filter(|s| step.can_reuse(s));
        if let Some(state) = saved {
            let prompt = step.prompt(&state.summary, state.covers);
            let tokens = measure.measure(&prompt).await?.prompt_tokens;
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
            return match saved {
                Some(state) => {
                    let mut out = step
                        .on_top(&state.summary, state.covers, &SummaryStats::default())
                        .await?;
                    if let Some(record) = out.record.as_mut() {
                        record.reused_summary = true;
                    }
                    Ok(out)
                }
                None => {
                    step.fallback(Self::ERR_NOTHING.into(), &SummaryStats::default())
                        .await
                }
            };
        }
        let mut summarizer = Summarizer::new(link, measure, sampling, n_ctx);
        let old = saved.map(|s| s.summary.as_str());
        let lines: Vec<String> = messages[span_start..span_end]
            .iter()
            .map(SpanLine::render)
            .collect();
        let summary = match summarizer.run(old, &lines).await {
            Ok(summary) => summary,
            Err(error) => return step.fallback(error, &summarizer.stats).await,
        };
        let note = [SummaryNote::message(&summary)];
        let tokens = match measure.measure(&note).await {
            Ok(size) => CompactionRecord::count(size.prompt_tokens),
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
        step.on_top(&state.summary, state.covers, &summarizer.stats)
            .await
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

    /// The system message (if any), the summary note, then `messages[covers..]`.
    fn prompt(&self, summary: &str, covers: usize) -> Vec<ChatMessage> {
        let mut out = self.messages[..self.system_len].to_vec();
        out.push(SummaryNote::message(summary));
        out.extend_from_slice(&self.messages[covers..]);
        out
    }

    /// Sends the summarised prompt, trimming it when it is still over budget.
    async fn on_top(
        &self,
        summary: &str,
        covers: usize,
        stats: &SummaryStats,
    ) -> Result<Compacted, String> {
        let prompt = self.prompt(summary, covers);
        let tokens = self.measure.measure(&prompt).await?.prompt_tokens;
        let mut record = CompactionRecord::new(Summarize::STRATEGY, self.before, tokens);
        let messages = if Trim::fits(tokens, self.reservation, self.budget) {
            prompt
        } else {
            let current = self.system_len + 1 + (self.current - covers);
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
    /// anything else as JSON) cut to [`Summarize::RESULT_CHARS`] characters; an assistant
    /// message's tool calls follow its text as `name(arguments)`.
    fn render(message: &ChatMessage) -> String {
        let role = serde_json::to_value(message.role)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let mut content = match (&message.role, &message.tool_result) {
            (Role::Tool, Some(Value::String(text))) => Self::cut(text),
            (Role::Tool, Some(result)) => Self::cut(&result.to_string()),
            _ => message.content.clone(),
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
}

impl SummaryStats {
    /// Fills the record's summary-call fields when at least one call was made.
    fn apply(&self, record: &mut CompactionRecord) {
        if self.calls == 0 {
            return;
        }
        record.summary_ms = Some(self.ms);
        record.summary_prompt_tokens = Some(self.prompt_tokens);
        record.summary_out_tokens = Some(self.out_tokens);
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
    const FIELD_PROMPT_TOKENS: &'static str = "prompt_tokens";
    const FIELD_TOKENS: &'static str = "tokens";
    const FIELD_CONTENT: &'static str = "content";
    const FIELD_MESSAGE: &'static str = "message";
    const FIELD_NEEDED: &'static str = "needed";
    const FIELD_TOTAL: &'static str = "total";
    const LINE_BREAK: &'static str = "\n";
    const SUMMARY_BREAK: &'static str = "\n\n";
    const ERR_LINK: &'static str = "summary call failed: ";
    const ERR_MEASURE: &'static str = "measuring the summary span failed: ";
    const ERR_EMPTY: &'static str = "summary reply was empty";
    const ERR_TRUNCATED: &'static str = "summary stream ended without turn_done";
    const ERR_LINE_TOO_BIG: &'static str = "a message is too big for one summary call";

    fn new(
        link: &'a dyn WorkerLink,
        measure: &'a dyn PromptMeasure,
        session: &SamplingConfig,
        n_ctx: usize,
    ) -> Self {
        let max_tokens = Summarize::max_tokens(n_ctx);
        let sampling = SamplingConfig {
            temperature: Summarize::TEMPERATURE,
            top_p: Summarize::TOP_P,
            max_tokens: i32::try_from(max_tokens).unwrap_or(i32::MAX),
            ..session.clone()
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
    /// many whole lines per call as fit the input budget. The first chunk makes room for the
    /// `old` summary; later chunks for a running summary of up to `max_tokens`. More than
    /// [`Summarize::MAX_CALLS`] chunks is an error, found before any call.
    async fn plan(
        &self,
        old: Option<&str>,
        lines: &[String],
    ) -> Result<Vec<std::ops::Range<usize>>, String> {
        let window = self.n_ctx.saturating_sub(self.max_tokens);
        let whole = self.tokens(&Self::content(old, lines)).await?;
        if whole <= window {
            return Ok(vec![0..lines.len()]);
        }
        let overhead = self.tokens("").await?;
        let input = window.saturating_sub(overhead);
        let mut room = match old {
            Some(old) => input.saturating_sub(self.cost(old, overhead).await?),
            None => input,
        };
        let mut chunks = Vec::new();
        let (mut start, mut used) = (0, 0);
        for (index, line) in lines.iter().enumerate() {
            let cost = self.cost(line, overhead).await?;
            if used + cost > room && index > start {
                chunks.push(start..index);
                (start, used) = (index, 0);
                room = input.saturating_sub(self.max_tokens);
            }
            if cost > room {
                return Err(Self::ERR_LINE_TOO_BIG.to_string());
            }
            used += cost;
        }
        chunks.push(start..lines.len());
        if chunks.len() > Summarize::MAX_CALLS {
            return Err(Self::too_many_calls(chunks.len()));
        }
        Ok(chunks)
    }

    fn too_many_calls(needed: usize) -> String {
        format!(
            "the span needs {needed} summary calls, more than {}",
            Summarize::MAX_CALLS
        )
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

    /// The tokens `text` adds to an empty summary call (whose size is `overhead`).
    async fn cost(&self, text: &str, overhead: usize) -> Result<usize, String> {
        Ok(self.tokens(text).await?.saturating_sub(overhead))
    }

    /// One summary call: tools and thinking off, the fixed sampling. The reply is
    /// `turn_done.content` without a leading channel opener, trimmed.
    ///
    /// A reply counts as cut off when its generated tokens (summed over `turn_chunk.tokens`)
    /// reach `max_tokens`. The chunk `stop` flag is not used: the in-process engines send a
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
        while let Some(item) = stream.next().await {
            let note = item.map_err(link_error)?;
            let params = &note.params;
            match note.method.as_str() {
                ToolLoop::M_TURN_STARTED => {
                    let tokens = Self::number(params, Self::FIELD_PROMPT_TOKENS);
                    let tokens = CompactionRecord::count(tokens);
                    self.stats.prompt_tokens = self.stats.prompt_tokens.saturating_add(tokens);
                }
                ToolLoop::M_TURN_CHUNK => {
                    let tokens = params
                        .get(Self::FIELD_TOKENS)
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    out_tokens = out_tokens.saturating_add(tokens);
                    let tokens = CompactionRecord::count(tokens);
                    self.stats.out_tokens = self.stats.out_tokens.saturating_add(tokens);
                }
                ToolLoop::M_TURN_DONE => {
                    if out_tokens >= self.max_tokens {
                        return Err(format!(
                            "summary hit its max_tokens ({}) before stopping",
                            self.max_tokens
                        ));
                    }
                    let content = params
                        .get(Self::FIELD_CONTENT)
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
                        "summary call rejected: needs {} of {} tokens",
                        Self::number(params, Self::FIELD_NEEDED),
                        Self::number(params, Self::FIELD_TOTAL)
                    ));
                }
                ToolLoop::M_TURN_ERROR => {
                    let message = params
                        .get(Self::FIELD_MESSAGE)
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    return Err(format!("summary call errored: {message}"));
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
                &FakeMeasure::new(),
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
    async fn summarize_replaces_span_keeps_tail_and_saves() {
        let (messages, current) = Sx::transcript();
        let budget = FakeMeasure::tokens(&messages);
        // The worker prefixes an opener; it is stripped and the reply trimmed.
        let link = Sx::link(vec![Sx::reply("<|channel>thought\n S \n", 3)]);
        let sink = RecordingSink::new();
        let out = Sx::run(&messages, current, None, budget, N_CTX, &link, &sink).await;
        let mut expected = vec![Fx::system(), SummaryNote::message(SUMMARY)];
        expected.extend_from_slice(&messages[17..]);
        assert_eq!(out.messages, expected);
        assert_eq!(
            out.messages[1].content,
            "[Summary of the earlier conversation]\nS"
        );
        let saved = sink.saved();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].summary, SUMMARY);
        assert_eq!(saved[0].covers, 17);
        assert_eq!(
            saved[0].tokens as usize,
            FakeMeasure::tokens(&[SummaryNote::message(SUMMARY)])
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
        assert_eq!((req.sampling.top_k, req.sampling.repeat_penalty), (7, 1.3));
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
        let mut expected = vec![Fx::system(), SummaryNote::message(OLD)];
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
        let mut two_turns = vec![Fx::system(), SummaryNote::message(OLD)];
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
        let mut expected = vec![Fx::system(), SummaryNote::message(SUMMARY)];
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
            let mut messages = vec![Fx::system()];
            for n in 0..turns {
                let letter = char::from(b'a' + n as u8);
                messages.push(Fx::user(&letter.to_string().repeat(BIG)));
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
        let mut expected = vec![Fx::system(), SummaryNote::message("S4")];
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
        let (messages, current) = Sx::long_turns(5);
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
        assert!(error.contains("5 summary calls"), "{error}");
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
        let mut expected = vec![Fx::system(), SummaryNote::message(SUMMARY)];
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
}
