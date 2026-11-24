### Task 3: Compaction seam and the trim strategy

**Files:**
- Create: `backend/src/runtime/compaction.rs`
- Modify: `backend/src/runtime/mod.rs`
- Modify: `backend/src/runtime/trace.rs` (`CompactionRecord`, plus `StepRecord.compaction`)

**Interfaces:**
- Consumes: Task 1's `ContextStrategy` and `CompactionState`.
- Produces:
  - `#[async_trait] pub trait PromptMeasure: Send + Sync { async fn measure(&self, messages: &[ChatMessage]) -> Result<PromptSize, String>; }`
  - The trace record, with fields exactly as in the spec:

    ```rust
    pub struct CompactionRecord {
        pub strategy: String,
        pub before_tokens: u32,
        pub after_tokens: u32,
        pub elided_results: u32,
        pub dropped_turns: u32,
        pub summarized_messages: u32,
        pub summary_ms: Option<u64>,
        pub reused_summary: bool,
        pub fallback: bool,
        pub summary_prompt_tokens: Option<u32>,
        pub summary_out_tokens: Option<u32>,
        pub summary_error: Option<String>,
    }
    ```

    `StepRecord` gains `#[serde(default, skip_serializing_if = "Option::is_none")] pub compaction: Option<CompactionRecord>`.
  - `pub struct Compacted { pub messages: Vec<ChatMessage>, pub record: Option<CompactionRecord> }`. `record` is `None` when nothing was done.
  - `pub struct Trim`, with `Trim::compact(messages: &[ChatMessage], current_turn_start: usize, budget: usize, reservation: usize, before: usize, measure: &dyn PromptMeasure) -> Result<Compacted, String>`. It always returns a record, with `strategy` set to `"trim"`.
  - `pub struct Turns`, with `Turns::starts(messages: &[ChatMessage]) -> Vec<usize>`, which gives the index of each user message. Both strategies use it.
- The trim algorithm, as the spec describes:
  - Elide the tool results of earlier turns, oldest first, and skip any result whose stub would be no shorter than the result itself.
  - Re-measure until the prompt fits under `budget`.
  - If it still doesn't fit, drop the oldest earlier turns whole.
  - Never touch index 0 when it is the system message. Never touch `current_turn_start..`, and never touch a summary note, which is a user message starting with the summary header.
  - The prompt that is sent must have been measured. `after_tokens` comes from that last measure.

- [ ] **Step 1: Write the failing tests** in `compaction.rs`. Use a `FakeMeasure` that counts one token per 4 characters of rendered content plus 4 per message.
  - `trim_elides_oldest_results_until_under_budget`: three earlier `read_file` results of 4,000 characters each, under a budget that needs two elided. The two oldest become the exact stub `[read_file result elided: {n} bytes]`, the third stays, `elided_results` is 2, `dropped_turns` is 0, and `after_tokens` is at most the budget minus the reservation.
  - `trim_drops_oldest_turns_when_elision_is_not_enough`
  - `trim_never_touches_system_current_turn_or_summary_note`
  - `short_result_is_not_replaced_by_a_longer_stub` (Review Focus 2)
  - `current_turn_overflow_returns_unchanged_with_record` (Review Focus 1): `after_tokens` equals `before_tokens`, and both counts are 0.
  - `turns_starts_marks_each_user_message`
  - `step_record_without_compaction_has_no_field_and_old_lines_parse`
- [ ] **Step 2:** Run `scripts/hive-test.sh -- compaction`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4:** Run `scripts/hive-test.sh`, then `--restore`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(runtime): compaction seam and trim strategy`.

