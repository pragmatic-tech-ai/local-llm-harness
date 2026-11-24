# Task 5 report: loop and server wiring

Status: DONE. Commit 6d5c4fe8 `feat(runtime): compact the prompt per step by the session's context strategy`.

## Built

- `backend/src/runtime/compaction.rs`: `pub struct Compactor` (the per-turn front), plus the private
  `LinkMeasure` (the `PromptMeasure` adapter over `WorkerLink::measure_prompt`), `KeepingSink` (passes
  a save on to the session's sink and keeps the state) and `DiscardSink` (the sink of `Compactor::off()`).
  Module docs updated.
- `backend/src/runtime/tool_loop.rs`: `TurnConfig` gains `context_strategy` and `compact_at_pct`.
  `ToolLoop::run` takes `compactor: &mut Compactor` (after `config`). `StepMeter` carries the step's
  `compaction`, which goes to `StepRecord.compaction`. Module and `run` docs updated.
- `backend/src/runtime/server.rs`: `SessionCompactionSink` (store-backed `SummarySink`),
  `SessionView::turn_config` and `SessionView::compactor`. `run_turn` builds both per turn. Module doc
  paragraph on compaction added.
- `backend/src/runtime/test_support.rs`: `FakeLink::set_measure` loses `#[allow(dead_code)]`. New
  `FakeLink::set_measure_by(fn(&GenerateRequest) -> PromptSize)` and `FakeLink::measured()` (every
  measure request), backed by a private `MeasureScript` enum.

## Signatures (for later tasks)

```rust
// runtime/compaction.rs
pub struct Compactor { /* private */ }
impl Compactor {
    pub fn new(
        strategy: ContextStrategy,
        compact_at_pct: u8,
        saved: Option<CompactionState>,
        committed_len: usize,
        sampling: SamplingConfig,          // ruling: replaces `seed: i64`
        sink: Arc<dyn SummarySink>,
    ) -> Self;
    pub fn off() -> Self;                   // never measures, never compacts
    pub fn saved(&self) -> Option<&CompactionState>;
    pub async fn prepare(
        &mut self,
        messages: &[ChatMessage],
        current_turn_start: usize,
        tools: &[Value],
        enable_thinking: bool,
        max_tokens: i32,
        link: &dyn WorkerLink,
    ) -> Compacted;
}

// runtime/tool_loop.rs
pub struct TurnConfig { ..., pub context_strategy: ContextStrategy, pub compact_at_pct: u8 }
pub async fn run(&self, transcript, content, config: &TurnConfig, compactor: &mut Compactor,
                 links, trace, sink) -> Result<(LoopEnd, Option<Vec<ChatMessage>>), SinkClosed>;
```

## Behavior (as built)

- `prepare`:
  - `Off`: returns the messages unchanged, `record: None`, and never measures.
  - Otherwise it measures `messages` once through `LinkMeasure`, which uses the step's tools,
    `enable_thinking` and the session sampling (the sampling is ignored by the worker).
  - `budget = n_ctx * compact_at_pct / 100` (floor). `reservation = max(max_tokens, 0)`.
  - If the prompt fits, or the measure fails, the messages come back unchanged with no record
    (Review Focus 4).
  - Otherwise it runs `Trim::compact`, or `Summarize::compact` with `self.saved`,
    `self.committed_len` and `&self.sampling`. A strategy `Err`, which is a measure failure on the
    send path, also gives the unchanged messages with no record.
  - A summary saved through `KeepingSink` replaces `self.saved` even when that step later returns
    `Err`, so later steps of the turn reuse it without reading the store.
- Loop:
  - `current_turn_start = transcript.len()`, the index of the user message in the scratch.
  - `prepare` runs after `model_props` (so the tools are known) and before `meter.requested()`, so a
    summary call does not count toward TTFT.
  - `GenerateRequest.messages = compacted.messages`, and `meter.compaction = compacted.record`. The
    scratch is untouched, so the committed transcript is always whole.
  - Summary-call notes are consumed inside `Summarizer`, and the loop's note order is unchanged.
- Server:
  - `run_turn` builds `TurnConfig` from the snapshot, then
    `Compactor::new(snapshot.context_strategy, snapshot.compact_at_pct, rec.compaction, rec.transcript.len(), config.sampling, SessionCompactionSink)`.
  - `SessionCompactionSink::save` returns `Ok(())` for a session that is gone. That covers a session
    gone before the write, and one deleted between the check and the write. Otherwise it calls
    `SessionStore::set_compaction(id, Some(state))`, which takes only the store's own short lock.
  - No hub or store lock is held across `measure_prompt` or the summary generate.
  - `reset` clears `compaction` through `SessionStore::reset` (Task 1). This is now covered end to end.

## Trace JSON for Task 6

A `trace_step` notification whose step was trimmed. Field order is as serialized. `compaction` is
absent (not `null`) on steps where compaction did not run.

```json
{"method":"trace_step","params":{"id":"s-1-0","record":{"v":1,"kind":"step","turn":3,"step":1,"at":1764000000000,"prompt_tokens":5980,"n_ctx":8192,"out_tokens":42,"think_tokens":0,"load_ms":null,"ttft_ms":120,"gen_ms":900,"end":"answered","tools":[],"error":null,"compaction":{"strategy":"trim","before_tokens":7420,"after_tokens":5980,"elided_results":3,"dropped_turns":0,"summarized_messages":0,"summary_ms":null,"reused_summary":false,"fallback":false,"summary_prompt_tokens":null,"summary_out_tokens":null,"summary_error":null}}}}
```

A fresh summary step's `compaction`, from the shape the server test asserts:
`{"strategy":"summarize","before_tokens":7420,"after_tokens":3100,"elided_results":0,"dropped_turns":0,"summarized_messages":14,"summary_ms":18200,"reused_summary":false,"fallback":false,"summary_prompt_tokens":2950,"summary_out_tokens":210,"summary_error":null}`.

When the summary is reused, `reused_summary` is true and `summary_ms` and the summary token fields
are null. On a fallback, `strategy` is `"summarize"`, `fallback` is true, `summary_error` is the
cause, and the trim counts are set. The paged `get_trace` steps have the same shape.

## Tests

tool_loop.rs:
- `off_strategy_sends_full_transcript_and_no_compaction_record`: also checks that nothing is measured.
- `under_budget_sends_unchanged`: the measure request equals the generate request's messages and tools, and has `enable_thinking`.
- `trim_step_sends_compacted_messages_and_records_it`: sends `[echo result elided: 3011 bytes]`. The record has before/after equal to the two measures, elided 1 and dropped 0. The committed transcript keeps the full result, and the notes are unchanged.
- `measure_failure_sends_uncompacted_and_continues`: 2 steps, both full, with no records.
- `current_turn_overflow_ends_window_full_with_record`: `Rejected` step, a trim record with 1900 → 1900 and counts 0, and one measure.
- `summary_saved_mid_turn_is_reused_by_later_steps` (extra): one summary call and one save. Step 1 is summarized (not reused); step 2 has `reused_summary`, and `Compactor::saved()` holds the state. The summary call's notes are not relayed.

server.rs:
- `summarize_session_saves_summary_and_reuses_it_next_turn`: turn 1 saves `{S, covers 2}` and has one `turn_started` on the wire. Turn 2 has `reused_summary: true`, 3 generate requests in total, and the stored transcript is whole.
- `reset_clears_saved_summary`: cleared in memory and on disk. The next summary call's span starts at message 0 (`user: aaa…`) with no old summary folded in.
- `template_edit_after_create_does_not_change_session_strategy`: created with trim, the template is then set to off, and the turn still records trim.
- `compaction_sink_leaves_a_deleted_session_alone` (extra).

The existing loop tests now pass `Compactor::off()`, through a `Fixture.compactor` field or inline, and are otherwise unchanged.

## TDD evidence

- RED: `prepare` was stubbed to return the messages unchanged. `scripts/hive-test.sh -- runtime::`
  then gave `FAILED. 213 passed; 8 failed`. The 8 failures were the 5 loop tests other than off, the
  mid-turn reuse test, and the summarize, reset and template server tests. The off and sink tests
  passed against the stub, as expected.
- GREEN:
  - `scripts/hive-test.sh -- runtime::`: `ok. 221 passed; 0 failed`.
  - `scripts/hive-test.sh -- --tests`: lib `468 passed`, main 7, and integration 10/6/2/5, all ok.
  - `scripts/hive-test.sh --features llamacpp`: lib `476 passed; 0 failed`, with no warnings.
  - After the test literals were hoisted to consts, `-- runtime::` again gave `221 passed`.
  - `scripts/hive-test.sh --restore` printed RESTORED.
- `rustfmt --edition 2021 --check` is clean on all 4 files. `git diff --check` is clean, line endings
  are LF, and Cargo.lock is unchanged.

## Self-review / concerns

1. `TurnConfig.context_strategy` and `compact_at_pct` are carried as the brief asks. The loop itself
   does not read them, because the compactor does the work. The server builds the compactor from
   them, so the two cannot drift.
2. `SessionCompactionSink::save` does a synchronous atomic file write on the async task, the same as
   `commit` and the other store calls in `run_turn`.
3. Measure-failure paths are silent: no log line, because an old worker would log on every step.
   They are visible only as the missing `compaction` field.

## Fix round 1

1. **The `TurnConfig` docs.** The docs on `context_strategy` and `compact_at_pct` now say that the
   loop does not read them. They configure the `Compactor` the caller builds and passes to `run`,
   which is `SessionView::compactor` in the server.
2. **The measure-failure log (ruling).**
   - `Compactor` gains a `session` field, set by `pub fn for_session(self, &str) -> Self`. The server
     calls it with `rec.id`.
   - It also gains a `logged_failures` counter, read through `pub fn logged_failures(&self) -> usize`.
   - The private `measure_failed` writes
     `measure_prompt failed, compaction skipped for session {id}: {error}` to stderr, only on the
     first failure of the turn. It covers both the initial measure and a strategy `Err`, which is a
     measure failure on the send path.
   - The step still goes out uncompacted, and its trace record has no `compaction`.
   - `measure_failure_sends_uncompacted_and_continues` asserts `logged_failures() == 1` after 2
     failed steps.
3. **The reservation test.** `max_tokens_reservation_pushes_a_fitting_prompt_over_budget` measures a
   prompt of 1400 against a budget of 1500. With `max_tokens` 500 it gets a trim record with
   `before_tokens` 1400. With `max_tokens` -1 it gets no record. This also proves that the loop
   passes `config.sampling.max_tokens`.
4. **Test constants.**
   - The compaction loop tests use `Compaction::CALL_ID`.
   - The server tests use `Compacting::OLD`, `KEPT` and `LATE`.

Evidence: `scripts/hive-test.sh -- --tests` gave lib `469 passed; 0 failed`, plus main 7 and
integration 10/6/2/5, all ok with no warnings. `--restore` printed RESTORED. `rustfmt --check` and
`git diff --check` are clean, and the line endings are LF.
