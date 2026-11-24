### Task 5: Loop and server wiring

**Files:**
- Modify: `backend/src/runtime/tool_loop.rs` (`TurnConfig`, one compaction call per step)
- Modify: `backend/src/runtime/compaction.rs` (the `Compactor` front)
- Modify: `backend/src/runtime/server.rs` (build the `Compactor` per turn with the session's saved state and a store-backed `SummarySink`; `reset` clears it through Task 1)

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces:
  - `TurnConfig` gains `pub context_strategy: ContextStrategy` and `pub compact_at_pct: u8`, filled from the session snapshot.
  - `pub struct Compactor`, with this signature:

    ```rust
    Compactor::new(
        strategy: ContextStrategy,
        compact_at_pct: u8,
        saved: Option<CompactionState>,
        committed_len: usize,
        seed: i64,
        sink: Arc<dyn SummarySink>,
    ) -> Compactor
    ```

    And `async fn prepare(&mut self, messages: &[ChatMessage], current_turn_start: usize, tools: &[Value], enable_thinking: bool, max_tokens: i32, link: &dyn WorkerLink) -> Compacted`.
    - It measures through the link's `measure_prompt`.
    - It returns the messages unchanged, with no record, when the strategy is `Off`, when the prompt is under the budget, or when the measure fails (Review Focus 4).
    - It keeps the newest saved state in `self`, so later steps in the same turn reuse it.
  - `ToolLoop::run` gets a `&mut Compactor` parameter (`Compactor::off()` for callers that don't compact). Before each step's `generate`, it calls `prepare` and sends `Compacted.messages`. It puts `Compacted.record` into that step's `StepRecord.compaction`. The scratch transcript that `run` returns is never affected.
  - `SessionCompactionSink` in server.rs: `save` calls `SessionStore::set_compaction(id, Some(state))`. It runs outside the hub and store locks, and only for a session that still exists.

- [ ] **Step 1: Write the failing tests:**
  - In tool_loop.rs:
    - `off_strategy_sends_full_transcript_and_no_compaction_record`
    - `under_budget_sends_unchanged`
    - `trim_step_sends_compacted_messages_and_records_it`. The fake worker records the request messages. The record lands in the step's trace record, and the committed transcript is still complete.
    - `measure_failure_sends_uncompacted_and_continues` (Review Focus 4)
    - `current_turn_overflow_ends_window_full_with_record` (Review Focus 1)
  - In server.rs:
    - `summarize_session_saves_summary_and_reuses_it_next_turn`: turn 1 saves the summary, and turn 2 records `reused_summary: true`.
    - `reset_clears_saved_summary` (Review Focus 5)
    - `template_edit_after_create_does_not_change_session_strategy`
- [ ] **Step 2:** Run `scripts/hive-test.sh -- runtime::`. Expected: FAIL.
- [ ] **Step 3: Implement.** Existing loop tests pass `Compactor::off()` and stay otherwise unchanged. Update the module docs.
- [ ] **Step 4:** Run `scripts/hive-test.sh -- --tests`, then `scripts/hive-test.sh --features llamacpp`, then `--restore`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(runtime): compact the prompt per step by the session's context strategy`.

