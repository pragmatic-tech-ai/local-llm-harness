# Context Compaction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When the prompt would cross a template's threshold, shrink the copy sent to the model with one of two template-chosen strategies, trim or summarize, and trace what was done.

**Architecture:**
- The worker gains `measure_prompt`.
- A new `runtime/compaction.rs` holds a `Compactor` with `Off`, `Trim` and `Summarize` strategies. They sit behind one per-step call that the ToolLoop makes before each generate.
- Summaries persist on the session record through a store handle.
- The step record gains an optional `compaction` field.
- The app adds two template-editor fields and a trace line.

**Tech Stack:**
- Rust: tokio, serde, async-trait, all vendored.
- App: Electron and mural TypeScript, tested with vitest.

**Spec:** `docs/specs/2026-11-24-context-compaction-design.md`. It builds on `docs/specs/2026-11-23-agent-templates-sessions-design.md` and `docs/specs/2026-11-24-agent-trace-design.md`.

## Global Constraints

- **Dependencies:** no new crates (the build is vendored and offline), Cargo.lock unchanged, and no npm install.
- **Where Rust runs:** only on the hive, through `scripts/hive-test.sh [--features F] [-- <cargo args>]`, ending with `scripts/hive-test.sh --restore`. Never run cargo on the dev machine.
- **Rust style:** idiomatic snake_case and rustfmt. Helpers are associated functions, with no new free functions (tests included). Reused strings are named consts.
- **TypeScript style:**
  - OOP only.
  - Allman braces and no semicolons.
  - PascalCase for public members.
  - Wire fields keep their snake_case.
  - Literals are `private static readonly` PascalCase constants.
  - VMs extend Observable.
  - `.mu` labels stay inline.
- **Template fields:**
  - `context_strategy` is `"off" | "trim" | "summarize"`, default `"off"`. Anything else is an error: `unknown context strategy: {v}`.
  - `compact_at_pct` is an integer, default `75`, valid from 10 to 95 inclusive. Outside that range the error is `compact_at_pct must be between 10 and 95`.
- **Budget:** `floor(n_ctx × compact_at_pct / 100)`. The reservation is `max(max_tokens, 0)`.
- **When compaction runs:** only when `prompt_tokens + reservation > budget` and the strategy is not `off`.
- **What is never compacted:** the system message, the current turn, and the summary note.
- **Fixed copy, held as consts:**
  - Stub: `[{tool} result elided: {bytes} bytes]`. `{bytes}` is the length of `serde_json::to_string(&tool_result)`.
  - Summary note: `[Summary of the earlier conversation]\n{summary}`.
  - Summary system prompt: `Summarize the conversation below for your own later use. Keep facts, file names, numbers, decisions and open questions. Write plain sentences, no preamble.`
- **The summary call:**
  - Tools and thinking are off.
  - Sampling is temperature 0.2 and top_p 0.9, with the session's seed.
  - `max_tokens` is `min(1024, n_ctx / 8)`.
  - Each tool result in the span text is truncated to 2,000 characters.
  - Span lines read `{role}: {content}`.
  - At most 4 chunked calls are made.
- **Kept tail:** the current turn plus the one turn before it. A turn is a user message and everything after it, up to the next user message.
- **Locks:** never hold the session store's lock or the hub's lock across a worker call.
- **Session schema:** session JSON stays at schema 1. The new `compaction` field is optional.
- **Trace version:** records stay `v: 1`. `StepRecord.compaction` is optional.
- **Commits:** use `git -c user.name="Eugene Napryaglo" -c user.email="napryaglo@users.noreply.github.com" commit`, and end each message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Never push, except in Task 7 after the owner's go.

## Review Focus

1. **The current turn's own tool results overflow the budget.** Trim can't help, so the step is sent as is. The worker's `window_full` ends the turn, and the trace's compaction record shows `after_tokens == before_tokens`. Covered in Tasks 3 and 5.
2. **A tool result shorter than its stub.** It is never replaced, because eliding must never grow the prompt. Covered in Task 3.
3. **The summary reply is empty, rejected, errors, or hits its `max_tokens`.** That step falls back to trim. `summary_error` names the cause, and the saved summary is unchanged. Covered in Task 4.
4. **`measure_prompt` fails** (a link error or an old worker without the method). The step is sent uncompacted, with no `compaction` record, and the turn continues. Covered in Task 5.
5. **Reset after a summary was saved.** `compaction` is cleared, and the next compaction makes a fresh summary from message 0. Covered in Tasks 1 and 5.

---

### Task 1: Template fields and the saved summary on the session

**Files:**
- Modify: `backend/src/agents/template.rs` (the fields, a `ContextStrategy` enum and validation)
- Modify: `backend/src/agents/session.rs` (`SessionRecord.compaction`)
- Modify: `backend/src/agents/session_store.rs` (`set_compaction`; `reset` clears it)

**Interfaces:**
- Produces:
  - `#[serde(rename_all = "lowercase")] pub enum ContextStrategy { Off, Trim, Summarize }`, with `Default = Off`.
  - On `AgentTemplate`:
    - `#[serde(default)] pub context_strategy: ContextStrategy`
    - `#[serde(default = "AgentTemplate::default_compact_at_pct")] pub compact_at_pct: u8`, where the default is 75
  - `pub struct CompactionState { pub summary: String, pub covers: usize, pub tokens: u32, pub at: u64 }`, which derives Serialize, Deserialize, Clone, Debug and PartialEq.
  - `SessionRecord.compaction: Option<CompactionState>`, with `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  - `SessionStore::set_compaction(&self, id: &str, state: Option<CompactionState>) -> Result<(), SessionError>`. It does not bump `updated`, and it does not broadcast.

- [ ] **Step 1: Write the failing tests**
  - In template.rs:
    - `context_fields_default_when_absent`: TOML without the fields gives `Off` and `75`.
    - `context_fields_round_trip`: `context_strategy = "summarize"` and `compact_at_pct = 60` round-trip.
    - `unknown_context_strategy_is_rejected`: the validation or parse error text contains `unknown context strategy: shrink`.
    - `compact_at_pct_out_of_range_is_rejected`: 9 and 96 give `compact_at_pct must be between 10 and 95`, while 10 and 95 pass.
  - In session_store.rs:
    - `set_compaction_persists_and_reopens`
    - `set_compaction_keeps_updated`
    - `reset_clears_compaction`
    - `old_session_json_without_compaction_loads`
- [ ] **Step 2:** Run `scripts/hive-test.sh -- agents::`. Expected: FAIL.
- [ ] **Step 3: Implement.** The wire shape follows from serde. Use the template validator's existing error style for the two messages.
- [ ] **Step 4:** Run `scripts/hive-test.sh`, then `--restore`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(agents): context strategy template fields and saved summary state`.

### Task 2: Worker `measure_prompt`

**Files:**
- Modify: `backend/src/rpc.rs` (the worker method)
- Modify: `backend/src/runtime/worker.rs` (`WorkerLink::measure_prompt` and `WsWorkerLink`)
- Modify: every test `WorkerLink` fake (`runtime/test_support.rs`, `tool_loop.rs` tests, `server.rs` tests)

**Interfaces:**
- Produces:
  - The worker method `measure_prompt {messages, tools?, enable_thinking}`, which returns `{prompt_tokens: usize, n_ctx: usize}`.
    - It takes the same message, tools and thinking params as `generate`.
    - It renders through the same template path `generate` uses, then tokenizes.
    - It does not generate and does not touch the engine's cache.
  - `WorkerLink::measure_prompt(&self, req: &GenerateRequest) -> Result<PromptSize, LinkError>`, with `pub struct PromptSize { pub prompt_tokens: usize, pub n_ctx: usize }`. It reuses `GenerateRequest`, and its sampling is ignored.
  - A worker without the method answers -32601. `WsWorkerLink` surfaces that as an `Err`, and later tasks treat it as "measure unavailable".

- [ ] **Step 1: Write the failing tests:**
  - in `rpc.rs`, `measure_prompt_matches_generate_turn_started_tokens`. Using the `FakeEngine` harness, `measure_prompt` on a set of messages returns the same `prompt_tokens` as the `turn_started` of a `generate` with the same messages, and returns `n_ctx` equal to the harness's;
  - `measure_prompt_bad_params_is_32602`;
  - in `worker.rs`, `ws_link_measure_prompt_round_trips` against `TestServer`.
- [ ] **Step 2:** Run `scripts/hive-test.sh -- measure_prompt`. Expected: FAIL.
- [ ] **Step 3: Implement.** Extract the render-and-count part of `generate` into one shared method, so the two cannot drift apart.
- [ ] **Step 4:** Run `scripts/hive-test.sh -- --tests`, then `scripts/hive-test.sh --features llamacpp`, then `--restore`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(rpc): measure_prompt renders and counts a prompt without generating`.

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

### Task 6: App: template fields and the trace line

**Files:**
- Modify: `app/src/renderer/src/agents.ts` (`AgentTemplate.context_strategy` and `compact_at_pct`; `StepRecord.compaction?: CompactionRecord | null`)
- Modify: `app/src/renderer/src/TemplateEditorViewModel.ts` and `views/TemplateEditor.mu`, with their tests
- Modify: `app/src/renderer/src/TraceViewModel.ts` (the step's compaction line) and `views/TracePanel.mu`, with their tests

**Interfaces:**
- Produces:
  - `TemplateEditorViewModel`:
    - `ContextChoices` (`Off`, `Trim`, `Summarize`, mapping to `off`, `trim`, `summarize`) and `Context`.
    - `CompactAtText`, which defaults to `75`. Outside 10 to 95, or failing to parse, it shows a field error and disables Save.
    - Both fields take part in `IsDirty` and `Revert`.
    - Save sends `context_strategy` and `compact_at_pct`.
  - `TraceStepViewModel.CompactionText` is `''` when there is no record. Otherwise it is exactly one of these, with thousands separated by U+2009 as elsewhere in the trace:
    - trim: `⟲ trim: {before} → {after} tokens · {e} results elided · {d} turns dropped`
    - summarize: `⟲ summarize: {before} → {after} tokens · covers {n} messages · {s} s`, with one decimal
    - reused: `⟲ summarize (reused): {before} → {after} tokens · covers {n} messages`
    - fallback: `⟲ summarize failed → trim: {before} → {after} tokens · {e} results elided · {d} turns dropped`, plus `  ({summary_error})` when set
  - `HasCompaction`, which the view uses for visibility.

- [ ] **Step 1: Write the failing tests:**
  - `editor_loads_and_saves_context_fields`
  - `compact_at_out_of_range_blocks_save`
  - `old_template_without_fields_shows_off_and_75`
  - `compaction_text_formats_each_variant` (four cases, with the spec's example numbers `7 420 → 5 980`)
  - `step_without_compaction_has_no_line`
- [ ] **Step 2–4:** Run vitest on the touched tests and see them fail, implement, then see them pass. Then run `npm test && npm run typecheck:web && npm run typecheck:node`.
- [ ] **Step 5: Commit** with the message `feat(app): context strategy in the template editor and compaction in the trace`.

### Task 7: Deploy and live proof (owner go required)

- [ ] **Step 1:** Run `scripts/hive-test.sh --restore`, then stop and ask the owner for the go to merge and push.
- [ ] **Step 2: After the go:**
  1. Merge to main and push.
  2. On the hive, run `git pull --ff-only`, then `stop-sup.ps1`, then `build-both.cmd`. Expect `CANDLE_EXIT=0 LLAMA_EXIT=0 SUP_EXIT=0`.
  3. Run `schtasks /Run /TN harness-supervisor` and check the runtime line in `supervisor.out`.
  4. Put a test file of about 6 KB of text in the sandbox: `C:\llm-harness\sandbox\big.txt`.
- [ ] **Step 3: Protocol proof**, with a scratch script in D:\tmp.
  - Create two templates on gemma-4 with only `read_file`, `n_ctx` 8192 and `compact_at_pct` 40: one with `trim`, one with `summarize`.
  - In each, have the agent read `big.txt` over three turns, then ask a question about the first turn.
  - **Trim:** a step with `compaction.strategy "trim"`, `elided_results ≥ 1`, and `after_tokens < before_tokens`.
  - **Summarize:** a step with `strategy "summarize"`, `summary_ms` set and `summarized_messages ≥ 1`. On a later turn it shows `reused_summary: true`.
  - **Off:** an `off` template given the same input ends `window_full`, or never compacts.
- [ ] **Step 4: Owner UI check:**
  - the Context combo and the "Compact at %" field in the editor;
  - the ⟲ lines in the trace.
- [ ] **Step 5:** Record the result in the spec status, the roadmap (step 3 shipped) and the memory note.
