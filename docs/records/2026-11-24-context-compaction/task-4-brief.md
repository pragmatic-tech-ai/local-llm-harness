### Task 4: The summarize strategy

**Files:**
- Modify: `backend/src/runtime/compaction.rs`

**Interfaces:**
- Consumes: Task 2's `WorkerLink::generate` and `GenerateRequest`, and Task 3's `Trim`, `Turns`, `Compacted` and `CompactionRecord`.
- Produces:
  - `#[async_trait] pub trait SummarySink: Send + Sync { async fn save(&self, state: CompactionState) -> Result<(), String>; }`
  - `pub struct Summarize`, with this signature:

    ```rust
    Summarize::compact(
        messages: &[ChatMessage],
        committed_len: usize,
        current_turn_start: usize,
        saved: Option<&CompactionState>,
        budget: usize,
        reservation: usize,
        before: usize,
        n_ctx: usize,
        seed: i64,
        link: &dyn WorkerLink,
        measure: &dyn PromptMeasure,
        sink: &dyn SummarySink,
    ) -> Result<Compacted, String>
    ```

    `strategy` is always `"summarize"`.
- How a step is handled:
  1. **Reuse.** If `saved` covers a prefix of the committed transcript, build: the system message, then the summary note, then the messages after `covers`. If that fits the budget, return it with `reused_summary` true and `summary_ms` None.
  2. **Summarise.** Otherwise the span runs from after the system message, or after `saved.covers` with the old summary text prepended, up to the kept tail. The kept tail is the turn before the current one, onwards, so the span always ends at or before `committed_len`.
     - Render the span as span lines.
     - Make one call when it fits the input budget, or chunked calls oldest first, at most 4. The input budget is `n_ctx − summary max_tokens − the measured overhead of the system prompt plus an empty user message`.
     - The call uses the fixed sampling and `max_tokens`, with tools and thinking off. The reply is the concatenated `turn_chunk` text up to `turn_done`, trimmed.
     - On success: `sink.save(CompactionState{summary, covers: span_end, tokens: <measured note tokens>, at})`. Then build the prompt and measure it. If it is still over the budget, run `Trim` on it and merge the counts.
  3. **Fallback.** Any summary failure (a link error, `turn_error`, `turn_rejected`, an empty reply, a reply that hit `max_tokens` with `stop` false, more than 4 chunks needed, or a failed `sink.save`) triggers `Trim::compact` on the original messages. Set `fallback` true and `summary_error` to the cause. `saved` stays unchanged.

- [ ] **Step 1: Write the failing tests.** Use `FakeMeasure`, a scripted fake `WorkerLink` whose `generate` yields chunk notes and then `turn_done`, and a `RecordingSink`.
  - `summarize_replaces_span_keeps_tail_and_saves`: 5 earlier turns plus a current one. The sent messages are the system message, the note `[Summary of the earlier conversation]\nS`, the previous turn and the current turn. The sink got `covers` equal to the index where the previous turn starts. The record has `summarized_messages` equal to that count, `summary_ms` set to `Some`, and the summary token counts filled in.
  - `summary_request_uses_fixed_prompt_sampling_and_no_tools`: the fake records the `GenerateRequest`. Its system content equals the fixed prompt, temperature is 0.2, top_p is 0.9, the seed is the session's, `enable_thinking` is false, there are no tools, `max_tokens` is `min(1024, n_ctx/8)`, and span lines are `user: ...` with tool results cut to 2,000 characters.
  - `saved_summary_is_reused_without_a_call`: the fake records 0 generate calls, and `reused_summary` is true.
  - `resummary_folds_old_summary_and_new_messages`: the request's user content starts with the old summary text, and the new `covers` is greater than the old one.
  - `long_span_is_chunked_oldest_first_up_to_four_calls`, and `span_needing_five_chunks_falls_back_to_trim`.
  - `summary_failure_falls_back_to_trim_with_reason` (Review Focus 3). This is table-driven over link error, `turn_error`, `turn_rejected`, empty reply and max-tokens hit. In every row, `fallback` is true, `summary_error` is set, the sink is never called, and the messages are trimmed.
  - `still_over_budget_after_summary_is_trimmed_too`
- [ ] **Step 2:** Run `scripts/hive-test.sh -- compaction`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4:** Run `scripts/hive-test.sh`, then `--restore`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(runtime): summarize strategy with saved, reused and chunked summaries`.

