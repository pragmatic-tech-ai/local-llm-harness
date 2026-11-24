# Task 4 report: the summarize strategy

Status: DONE_WITH_CONCERNS. Commit 327724ae `feat(runtime): summarize strategy with saved, reused and chunked summaries`.

## Built

- `backend/src/runtime/compaction.rs`: `SummarySink`, `Summarize`, and the private helpers
  `SummarizeStep` (one step's inputs and its reuse / on-top / fallback paths), `Summarizer` (chunk
  plan, summary calls, stream reading), `SpanLine` (span-line rendering) and `SummaryStats`
  (totals over the calls).
- `backend/src/runtime/tool_loop.rs`: `ToolLoop::M_TURN_STARTED/CHUNK/DONE/REJECTED/ERROR` are now
  `pub(crate)`, so the summary reader shares the note names and does not copy them.

## Public signatures for Task 5

```rust
// runtime/compaction.rs
#[async_trait]
pub trait SummarySink: Send + Sync {
    async fn save(&self, state: CompactionState) -> Result<(), String>;
}

pub struct Summarize;
impl Summarize {
    pub const STRATEGY: &'static str = "summarize";
    pub const SYSTEM_PROMPT: &'static str = "Summarize the conversation below for your own later use. Keep facts, file names, numbers, decisions and open questions. Write plain sentences, no preamble.";
    pub const MAX_TOKENS_CAP: usize = 1024;
    pub const RESULT_CHARS: usize = 2000;
    pub const MAX_CALLS: usize = 4;
    pub fn max_tokens(n_ctx: usize) -> usize;            // min(1024, n_ctx / 8)
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
        sampling: &SamplingConfig,      // DEVIATION: the brief has `seed: i64`
        link: &dyn WorkerLink,
        measure: &dyn PromptMeasure,
        sink: &dyn SummarySink,
    ) -> Result<Compacted, String>;
}
```

Deviation: the controller ruled that the summary request copies the session's other sampling
fields (top_k, repeat_penalty), overriding only temperature 0.2, top_p 0.9 and max_tokens. That
needs the whole `SamplingConfig`, so the `seed: i64` parameter became `sampling: &SamplingConfig`,
and the seed is `sampling.seed`. Task 5 should pass `&config.sampling`, and its
`Compactor::new(..., seed: i64, ...)` should take the `SamplingConfig` the same way.

`Err` is returned only when a measure fails on the path that sends a prompt: the reuse measure,
the summarised-prompt measure, or a measure inside `Trim`. This is the same contract as
`Trim::compact`. Task 5 treats it as "send uncompacted" (Review Focus 4). The record is always
`Some`, with `strategy == "summarize"`.

## Behavior (as built)

- **Index conventions.** `system_len` is 1 when index 0 is `Role::System`, otherwise 0.
  `covers` is a transcript index, the span's exclusive end, which is what the brief's
  `covers: span_end` gives. The summary replaces `messages[system_len..covers]`, and the verbatim
  part is `messages[covers..]`. `summarized_messages = covers - system_len`, the number of
  messages replaced. With a system message, that is one less than `covers`: the main test has
  `covers` 17 and `summarized_messages` 16. If "that count" in the brief meant `covers` itself,
  that is a one-line change in `SummarizeStep::summarized`.
- **Reuse.** `saved` is usable when `system_len < covers <= min(committed_len, current_turn_start)`.
  The prompt is the system message, then `SummaryNote::message(summary)`, then `messages[covers..]`.
  If that measure fits, it is returned with `reused_summary` true, no summary fields, and
  `after_tokens` set to the measure. A `saved` summary that cannot be used is ignored and never
  overwritten, except by a successful new save.
- **Span.** The span runs from `saved.covers` (when usable) or `system_len`, up to the start of
  the last user turn before `current_turn_start`, capped at `committed_len`. The old summary text
  is folded in as the start of the first call's user content.
  - Empty span with a usable saved summary: there is nothing new to summarise. Trim runs on top
    of the reused prompt, with `reused_summary` true and `fallback` false.
  - Empty span without a saved summary: fallback, with the reason
    `nothing to summarize before the kept tail`.
- **Span lines.** Each line is `{role}: {content}`, and the lines are joined with `\n`:
  - A tool message's content is its `tool_result`: a string as it is, anything else as compact
    JSON, cut to 2,000 characters.
  - An assistant message's tool calls are appended as `name(arguments)`, so a call line reads
    `assistant: read_file({})`.
  - Running-summary content is `{summary}\n\n{lines}`.
- **Chunking.** The plan is made before any call:
  - If the whole request measures at most `n_ctx - max_tokens`, one call is made.
  - Otherwise `overhead` is the measure of `[System(SYSTEM_PROMPT), User("")]`, and
    `input = n_ctx - max_tokens - overhead`. Each line's cost is
    `measure([sys, user(line)]) - overhead`, so N measures.
  - Lines are packed greedily into whole-message chunks. The first chunk reserves the old
    summary's cost; later chunks reserve `max_tokens`, the largest running summary that can
    succeed.
  - A plan of more than 4 chunks fails with `the span needs {n} summary calls, more than 4`,
    and no call is made. A single line bigger than a chunk also fails.
  - The calls run oldest first. Each later call gets the running summary, then the next chunk.
- **The call.** `GenerateRequest {messages: [System(SYSTEM_PROMPT), User(content)], tools: [],
  enable_thinking: false, sampling: {temperature 0.2, top_p 0.9, max_tokens, ..session}}`.
- **Reading the reply.**
  - `turn_started.prompt_tokens` and the `turn_chunk.tokens` lengths are summed into the stats.
  - On `turn_done`, `content` loses a leading `<|channel>name\n` opener (checked with
    `OpenChannel::at_tail`) and is trimmed.
  - Failures:
    - `turn_rejected`: `summary call rejected: needs {needed} of {total} tokens`
    - `turn_error`: `summary call errored: {message}`
    - a link or stream error: `summary call failed: {e}`
    - a stream ending without `turn_done`: `summary stream ended without turn_done`
    - an empty reply: `summary reply was empty`
- **Detecting the max_tokens hit (chosen and documented on `Summarizer::call`).** A reply counts
  as hit when its generated token count, summed over `turn_chunk.tokens`, reaches `max_tokens`,
  and the error is `summary hit its max_tokens ({n}) before stopping`. The `stop` flag is not
  used, because the llamacpp and candle engines send a final `stop: true` chunk on the max_tokens
  cap as well as on EOS. The cost: a reply that ends naturally on exactly `max_tokens` tokens is
  treated as cut off.
- **Saving.**
  - The state is `CompactionState {summary, covers: span_end, tokens, at}`.
  - `tokens` is `measure([note]).prompt_tokens`. A failure to measure the note is a summary
    failure.
  - `at` is wall-clock Unix ms.
  - `sink.save` is called once, before the summarised prompt is measured. A failed save is a
    summary failure.
- **After the summary.** The prompt is the system message, the note, then `messages[covers..]`,
  and it is measured.
  - If it fits, it is sent.
  - If it does not, `Trim::compact` runs on it with
    `current = system_len + 1 + (current_turn_start - covers)`. The note is never touched,
    because Trim skips notes. The record takes `elided_results`, `dropped_turns` and
    `after_tokens` from the trim.
  - `summarized_messages`, `summary_ms`, `summary_prompt_tokens` and `summary_out_tokens` are set.
- **Fallback.** `Trim::compact` runs on the original messages. Its record is re-labelled
  `strategy "summarize"`, with `fallback` true and `summary_error` set to the cause. The summary
  stats are filled in when at least one call was made: a failed save still shows the call's
  tokens and time, and a plan failure shows None. The sink is not called, so the saved state
  stays as it was.
- Compaction holds no locks. The sink is called through `&dyn SummarySink`, and Task 5's
  `SessionCompactionSink` keeps it outside the store and hub locks.

## Tests (compaction.rs)

These use the existing `FakeMeasure`, `test_support::FakeLink` (scripted `generate`, recorded
requests) and a local `RecordingSink` (records states, or fails with `disk full`).

- `summarize_replaces_span_keeps_tail_and_saves`: the opener is stripped and the reply trimmed to `S`. Covers 17, 16 summarised, prompt tokens 10, out tokens 3.
- `summary_request_uses_fixed_prompt_sampling_and_no_tools`: n_ctx 4096 gives max_tokens 512. Also checks top_k and repeat_penalty copied, the exact span lines, and the 2,000-character cut.
- `saved_summary_is_reused_without_a_call`
- `resummary_folds_old_summary_and_new_messages`: content starts `OLD\n\nuser: q2\n`, and covers goes from 9 to 17.
- `long_span_is_chunked_oldest_first_up_to_four_calls`: 4 calls, exact chunk contents, running summary `S{k}` prepended, prompt tokens summed to 40.
- `span_needing_five_chunks_falls_back_to_trim`: 0 calls, the sink is not called, messages equal trim's.
- `summary_failure_falls_back_to_trim_with_reason`: table over link error, turn_error, turn_rejected, empty reply and max_tokens hit. Each row starts from a usable saved summary that no longer fits.
- `failed_save_falls_back_to_trim`
- `still_over_budget_after_summary_is_trimmed_too`: the previous turn's big result is stubbed after the note, elided 1, summarised 12.

## TDD evidence

- RED: the full code and tests were in place, with an early `return Err("unimplemented")` at the top of `Summarize::compact`. `scripts/hive-test.sh -- compaction` gave `FAILED. 14 passed; 9 failed`, and the 9 failures were exactly the new summarize tests.
- GREEN: `scripts/hive-test.sh -- compaction` gave `ok. 23 passed; 0 failed`. Then `scripts/hive-test.sh` (whole lib) gave `ok. 455 passed; 0 failed`, TEST_EXIT=0, with no warnings in the output. `scripts/hive-test.sh --restore` printed RESTORED.
- `rustfmt --edition 2021 --check` passes on both files, `git diff --check` is clean, and line endings are LF. Cargo.lock is unchanged and there are no new crates.

## Concerns

1. Signature deviation: `sampling: &SamplingConfig` replaces `seed: i64`, per the controller's ruling, because the session's other fields are copied. This also contradicts the spec's "fixed whatever the session's sampling is" rationale for top_k and repeat_penalty. If the owner wants those fixed too, use `SamplingConfig::default()` and take `seed` again.
2. `covers` is an absolute transcript index (brief), while the spec says it is a count "from after the system message". With a system message they differ by one. `summarized_messages` reports the true replaced count, `covers - system_len`. Task 5 and the app's "covers N messages" line should use `summarized_messages`.
3. Chunk planning costs one measure per span line, only when the whole span does not fit in one call. The plan is conservative: later chunks reserve `max_tokens` for the running summary, so a span that would barely fit in 4 real calls can be refused. Line costs are summed, not re-measured joined. If the estimate is off, the worker's `window_full` rejection still causes a fallback.
4. Detecting the max_tokens hit uses the token count, not `stop` (see above).

## Fix round 1

Commit 821514cc `fix(runtime): summarize chunk costs, shared consts, single reuse measure, more tests`.

1. **Chunk planning.**
   - Each line now costs its measured tokens plus `Summarizer::JOIN_TOKENS` (1), which covers the `\n` join and any merge at the boundary.
   - The first chunk with an old summary, and every later chunk, also reserve `SUMMARY_BREAK_TOKENS` (2) for the `\n\n` after the running summary.
   - Planning returns as soon as a 4th chunk closes with lines still left, so no further lines are measured. The error is now the const `the span needs more summary calls than the limit of 4`.
   - `span_needing_five_chunks_falls_back_to_trim` now has an oversized 6th turn. If planning measured past the limit, the error would be "a message is too big"; the test asserts the limit error, which proves the early stop.
2. **Shared constants.**
   - `ToolLoop::FIELD_CONTENT/MESSAGE/NEEDED/TOTAL` are now `pub(crate)` and used by the summary reader.
   - `FIELD_PROMPT_TOKENS` and `FIELD_TOKENS` moved from the private `StepMeter` to `ToolLoop` as `pub(crate)`. `StepMeter` and `Summarizer` both use them.
   - The max_tokens, rejected and errored messages, and the too-many message, are now consts: `ERR_MAX_TOKENS`, `ERR_REJECTED`, `ERR_ERRORED` and `ERR_TOO_MANY`.
3. **Reply source (ruling).** The doc on `Summarizer::call` now states that taking `turn_done.content`, not the concatenated `turn_chunk` text, is an intended difference from the brief. It is the worker's final text, and ToolLoop reads the same field.
4. **New tests.**
   - `empty_span_with_saved_summary_trims_the_reused_prompt`: `reused_summary` true, no call, the previous turn's result stubbed, and exactly 2 measures.
   - `nothing_to_summarize_falls_back_to_trim`
   - `failed_note_measure_falls_back_to_trim`: `NoteFailingMeasure` gives the error `measuring the summary note failed: tokenizer gone`, and the sink is not called.
   - A new row in `summary_failure_falls_back_to_trim_with_reason`: "stream ends without turn_done".
   - A test helper `Sx::run_measured` takes the measure.
5. **Duplicate measure.** `SummarizeStep::on_top` takes `measured: Option<usize>`. The empty-span path passes the reuse measure, so the identical prompt is not measured twice. The new test pins this at 2 measure calls.

Evidence:
- `scripts/hive-test.sh -- compaction`: `ok. 26 passed; 0 failed`.
- `scripts/hive-test.sh`: `ok. 458 passed; 0 failed`, TEST_EXIT=0, no warnings.
- `--restore` printed RESTORED.
- `rustfmt --check` and `git diff --check` are clean.
