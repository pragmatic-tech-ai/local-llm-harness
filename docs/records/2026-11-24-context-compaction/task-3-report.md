# Task 3 report: compaction seam and trim strategy

Status: DONE_WITH_CONCERNS. Commit ff5ab907 `feat(runtime): compaction seam and trim strategy`.

## Built

- `backend/src/runtime/compaction.rs` (new, `pub mod compaction` in runtime/mod.rs): the seam
  trait, `Compacted`, `Turns`, `SummaryNote`, `Trim` and a private `TrimPlan`.
- `backend/src/runtime/trace.rs`: `CompactionRecord` (fields exactly as the spec), with
  `new` and `count` helpers, plus `StepRecord.compaction`
  (`#[serde(default, skip_serializing_if = "Option::is_none")]`).
- `backend/src/runtime/tool_loop.rs`: `StepRecord` literal sets `compaction: None`. Task 5
  fills it in.

## Public signatures for Tasks 4 and 5

runtime/compaction.rs:
```rust
#[async_trait]
pub trait PromptMeasure: Send + Sync {
    async fn measure(&self, messages: &[ChatMessage]) -> Result<PromptSize, String>;
}
#[derive(Debug, Clone, PartialEq)]
pub struct Compacted { pub messages: Vec<ChatMessage>, pub record: Option<CompactionRecord> }
pub struct Turns;
impl Turns { pub fn starts(messages: &[ChatMessage]) -> Vec<usize> }   // every Role::User index, summary notes included
pub struct SummaryNote;
impl SummaryNote {
    pub const HEADER: &'static str = "[Summary of the earlier conversation]";
    pub fn message(summary: &str) -> ChatMessage;    // User, "{HEADER}\n{summary}"
    pub fn is_note(message: &ChatMessage) -> bool;   // User && content.starts_with(HEADER)
}
pub struct Trim;
impl Trim {
    pub const STRATEGY: &'static str = "trim";
    pub async fn compact(messages: &[ChatMessage], current_turn_start: usize, budget: usize,
        reservation: usize, before: usize, measure: &dyn PromptMeasure) -> Result<Compacted, String>;
}
```
Note that `Trim::compact` is `async`, because the measure is async. The brief's signature
did not say so. A measure error is returned as `Err` unchanged. Task 5 decides what to do with
it (Review Focus 4).

runtime/trace.rs:
```rust
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CompactionRecord { /* 12 fields exactly as the spec */ }
impl CompactionRecord {
    pub fn new(strategy: &str, before: usize, after: usize) -> Self; // counts 0, summary fields None/false
    pub fn count(n: usize) -> u32;                                    // saturating
}
StepRecord { ..., #[serde(default, skip_serializing_if = "Option::is_none")] pub compaction: Option<CompactionRecord> }
```

## Trim behavior (as built)

- A prompt fits when `tokens + reservation <= budget` (saturating add). If `before` already fits,
  nothing is measured, and the messages come back unchanged with a zero record.
- The touchable region is `0..current_turn_start` (clamped to the transcript). It excludes
  index 0 when that is `Role::System`, and it excludes summary notes.
- Elision goes oldest first. A candidate is a `Role::Tool` message in the region with
  `tool_result: Some`. The stub is `[{tool} result elided: {bytes} bytes]`:
  - `bytes` is `serde_json::to_string(&result).len()`.
  - `tool` is the `function.name` of the nearest earlier `tool_calls` entry with the same id,
    or `tool` when no call matches.
  - The replacement is a clone of the message with `tool_result = Some(Value::String(stub))`.
    Role, `tool_call_id` and the empty `content` are kept. `template_view` hands
    `tool_result` over as `content`. The Gemma template renders a string body as
    `response:{name}{value:<|"|>stub<|"|>}`, so call and result still pair up.
  - A candidate is skipped when the serialized stub is no shorter than the serialized
    result (Review Focus 2).
  - After each stub the prompt is measured again, and elision stops as soon as it fits.
- Dropping comes next, oldest first. A turn runs from a user start to the next user start
  (capped at `current_turn_start`). A turn whose start is a summary note, or the system
  message, is never dropped. After each drop the prompt is measured again.
- The returned messages are always the ones measured last. `after_tokens` is that measure,
  or `before` when nothing changed. If nothing in the region can be removed, as when only the
  current turn is overflowing, the messages come back unchanged with `after == before`, both
  counts 0, and no measure calls (Review Focus 1).
- `elided_results` counts the stubs in the prompt that is sent. A stub whose turn was later
  dropped is not counted. `dropped_turns` counts the dropped turns.

## Tests

In compaction.rs:
- `trim_elides_oldest_results_until_under_budget`
- `trim_drops_oldest_turns_when_elision_is_not_enough`
- `trim_never_touches_system_current_turn_or_summary_note`
- `short_result_is_not_replaced_by_a_longer_stub` (also asserts that only 1 measure call was made)
- `current_turn_overflow_returns_unchanged_with_record`
- `turns_starts_marks_each_user_message`

The `FakeMeasure` counts `content_chars/4 + 4` per message over `template_view()["content"]`.

In trace.rs:
- `step_record_without_compaction_has_no_field_and_old_lines_parse`
- `compaction_record_counts_saturate`

The existing test `live_trace_step_is_byte_equal_to_the_paged_step` (server.rs) still passes.

## TDD evidence

- RED: I neutered `Trim::compact` (skipped both loops) and `Turns::starts` (returned empty),
  then ran `scripts/hive-test.sh -- compaction`. Result: `FAILED. 7 passed; 5 failed`. The 5
  failures were the elide, drop, never-touches, short-result and turns-starts tests. The
  current-turn and trace tests passed against the stub, which is expected.
- GREEN: `scripts/hive-test.sh` gave lib `444 passed; 0 failed`. `scripts/hive-test.sh -- --tests`
  passed everywhere (lib 444, integration 7/10/6/2/5). `--restore` printed RESTORED.
- `rustfmt --edition 2021 --check` passes on all touched files. LF endings; `git diff --check`
  is clean.

## Concerns

1. Partial trim policy. Suppose earlier material exists, but even removing all of it leaves the
   prompt over budget. Trim then sends the most-trimmed prompt it measured, not the original.
   My reasoning is the spec line "builds a compacted message list, measures it again, and sends
   it; window_full if it still doesn't fit n_ctx". The cost is that history is thrown away when
   the original might still have fit `n_ctx` (budget < n_ctx). Only the pure current-turn
   overflow comes back unchanged. If the owner wants "trim can't reach budget means send the
   original", the change is small: return the input when the final measure doesn't fit.
2. Trim measures again after every stub and every drop, which is one `measure_prompt` RPC each.
   That is correct, but it can mean many RPCs on long transcripts. Estimating per message
   (the spec's builder option) would be an optimisation for later.

## Fix round 1

1. (Important) The byte count now has one home: `pub fn ToolTiming::result_bytes(result: &Value) -> usize`
   in runtime/trace.rs. `ToolLoop` calls it (`as u64` at the call site), and so does `Trim`.
   `ToolLoop::result_bytes` and `Trim::bytes` are deleted.
2. Docs follow the ruling. `Compacted.record` is `None` only on the `Compactor`'s no-op paths
   (off, under budget, measure failed). `Trim::compact` always returns `Some`, including the
   all-zero record when `before` already fits. The `Compactor` only calls it over budget.
3. Added `trim_without_system_message_drops_first_user_turn_keeps_current`. It checks
   `Turns::starts == [0, 2, 6]`, that the index-0 user turn is dropped, and that the current
   turn is kept verbatim (`dropped_turns` 1, `elided_results` 0).
4. Added `stubbed_transcript_renders_stub_in_the_matching_tool_response` in compaction.rs.
   It loads `tests/fixtures/gemma4-chat-template.jinja` into `ChatTemplate` and renders the
   trimmed transcript. It asserts that
   `<|tool_call>call:read_file{}<tool_call|><|tool_response>response:read_file{value:<|"|>[read_file result elided: 4014 bytes]<|"|>}<tool_response|>`
   appears contiguously, that the untouched `get_time` call and response still pair, and that
   the elided body is absent.

Evidence: `scripts/hive-test.sh` reported lib `446 passed; 0 failed`, then `--restore` printed
RESTORED. `rustfmt --check` passes, and `git diff --check` is clean.
