# Agent Trace Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record a per-step and per-turn trace for every agent turn, persist it per session on the hive, stream it live, and show it in the instrument panel.

**Architecture:**
- A new `runtime/trace.rs` owns three things: the record types; an append-only JSONL `TraceLog` per session, kept by a `TraceStore`; and a `SessionTrace`, which appends a record and then notifies.
- `ToolLoop::run` measures each step (fill, tokens, think tokens, time to first token, generation time, tools, load time) and hands it to a `TurnTrace`. The server finishes the turn record after the commit.
- The app adds `GetTrace` and three signals to `RuntimeClient`, plus a `TraceViewModel` rendered in the instrument panel.

**Tech Stack:**
- Rust: tokio, serde_json, async-trait, all vendored.
- Electron + mural TypeScript, with vitest.

**Spec:** `docs/specs/2026-11-24-agent-trace-design.md`, which builds on `docs/specs/2026-11-23-agent-templates-sessions-design.md`.

## Global Constraints

- No new crates (the build is vendored and offline), Cargo.lock is unchanged, and there is no npm install.
- Rust builds and tests run only on the hive, through `scripts/hive-test.sh [--features F] [-- <cargo args>]`. Every run finishes with `scripts/hive-test.sh --restore`. Never run cargo on the dev machine.
- Rust is idiomatic snake_case and rustfmt'd. Helpers are associated functions: no new free functions, tests included. Reused strings are named consts.
- TypeScript rules:
  - OOP, with no module-level functions or mutable state.
  - Allman braces and no semicolons.
  - PascalCase for public members and interfaces. Wire-mirroring data fields keep their snake_case.
  - Literals are `private static readonly` PascalCase constants, and `.mu` labels stay inline.
  - VMs extend Observable.
- Every record carries `"v": 1`. Readers skip unknown kinds and fields, and ignore a partial last line.
- The log path is `{agents.data_dir}/sessions/{id}.trace.jsonl`: UTF-8, one object per line, LF, append-only. Each record is written with one append and one flush, with no fsync.
- Timing uses `std::time::Instant`. Only `at` (Unix ms) uses the wall clock.
- Tracing never fails a turn. A log write failure is reported as `trace_warning {id, message}`, and the turn continues.
- Writing the trace must not hold the session store's lock or the hub's lock.
- Commit identity: `git -c user.name="Eugene Napryaglo" -c user.email="napryaglo@users.noreply.github.com" commit`. The message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Never push, except in Task 7 after the owner's go.

## Review Focus

1. **A log that can't be written** (a read-only file, or the path is a directory): the turn still answers and commits, and every attached client gets `trace_warning`. Covered in Task 4.
2. **A turn with thinking on**, where the prompt ends with a channel opener: `think_tokens` > 0 and ≤ `out_tokens`, and an answer-only step has `think_tokens` 0. Covered in Task 2.
3. **Reconnect or re-attach in the middle of a turn**: a live record for a step already loaded from `get_trace` replaces it, with no duplicate row. Covered in Task 6.
4. **A supervisor restart after a turn was cut short by a crash** (steps but no turn record): turn numbers continue above the highest step's turn, and the app shows that turn as `incomplete`, not `running…`. Covered in Tasks 1 and 6.
5. **`get_trace` paging past the end** (`before` ≤ 1, or `before` above the newest turn): an empty page and `more: false` in the first case, the newest page in the second, and never an error. Covered in Task 1.

---

### Task 1: Trace records, log and store

**Files:**
- Create: `backend/src/runtime/trace.rs` (records, `TraceLog`, `TraceStore`, `TracePage`, `TraceTurn`)
- Modify: `backend/src/runtime/mod.rs` (`pub mod trace;`)

**Interfaces:**
- Produces:
  - Records:
    - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)] pub struct StepRecord { pub turn: u64, pub step: u32, pub at: u64, pub prompt_tokens: Option<u32>, pub n_ctx: Option<u32>, pub out_tokens: u32, pub think_tokens: u32, pub load_ms: Option<u64>, pub ttft_ms: Option<u64>, pub gen_ms: Option<u64>, pub end: StepOutcome, pub tools: Vec<ToolTiming>, pub error: Option<String> }`
    - `pub enum StepOutcome { ToolCalls, Answered, Budget, Rejected, Error, Aborted }` (not `StepEnd`, which is already a private enum in `tool_loop.rs`), serialized snake_case.
    - `pub struct ToolTiming { pub name: String, pub id: String, pub ok: bool, pub ms: u64, pub bytes: u64 }`
    - `pub struct TurnRecord { pub turn: u64, pub at: u64, pub steps: u32, pub reason: String, pub total_ms: u64, pub out_tokens: u32, pub think_tokens: u32, pub peak_fill: f64, pub committed: bool, pub user: String }`
    - `pub struct ResetRecord { pub at: u64 }`
    - `pub enum TraceRecord { Step(StepRecord), Turn(TurnRecord), Reset(ResetRecord) }`. It serializes with `"v":1` and `"kind":"step"|"turn"|"reset"` flattened in; use a small wrapper struct rather than a hand-written `Serialize`, if serde's internally-tagged enum plus a `v` field allows it.
  - Pages:
    - `pub struct TraceTurn { pub turn: u64, pub steps: Vec<StepRecord>, pub summary: Option<TurnRecord>, pub reset_before: bool }`
    - `pub struct TracePage { pub turns: Vec<TraceTurn>, pub more: bool, pub reset_pending: bool }`
  - `TraceLog`:
    - `TraceLog::open(path: PathBuf) -> TraceLog`. This is lazy: nothing is read until it is used.
    - `append(&self, record: &TraceRecord) -> io::Result<()>`
    - `next_turn(&self) -> u64`: the highest `turn` in any step or turn record, plus 1, scanned once and then cached. A missing file gives 1.
    - `page(&self, before: Option<u64>, limit: Option<usize>) -> io::Result<TracePage>`. `limit` defaults to 20, and its cap is `TraceLog::MAX_LIMIT = 100`.
    - `delete(&self) -> io::Result<()>`. A missing file is fine.
  - `TraceStore`:
    - `TraceStore::new(sessions_dir: PathBuf) -> TraceStore`
    - `log(&self, id: &str) -> Arc<TraceLog>`, cached per id.
    - `delete(&self, id: &str) -> io::Result<()>`, which also evicts the cache entry.
  - `TRACE_FILE_SUFFIX = ".trace.jsonl"`. `SessionStore` only loads `*.json`; check that `.trace.jsonl` files are not picked up by its loader, and add the test below.

- [ ] **Step 1: Write the failing tests** in `trace.rs`'s `mod tests` (using a temp-dir guard struct):
  - `records_round_trip_with_version_and_kind`: one record of each kind is appended. The raw lines contain `"v":1` and the right `"kind"`, and `page` returns them.
  - `partial_last_line_is_ignored`: two steps of turn 1 are appended, then `{"v":1,"kind":"st` is written raw with no LF. `page(None, None)` has one turn with 2 steps, and `next_turn()` is 2.
  - `unknown_kind_and_fields_are_skipped`: a raw line `{"v":1,"kind":"future","x":1}`, then a step with an extra field `"new":5`. The step is read, and nothing fails.
  - `page_is_newest_first_with_limit_and_cap`: 25 turns, each with one step and a summary. `page(None, None)` returns turns 25..=6 and `more: true`. `page(Some(6), None)` returns 5..=1 and `more: false`. `page(None, Some(500))` returns 25 turns.
  - `paging_past_the_ends`: `page(Some(1), None)` gives no turns and `more: false`. `page(Some(99), None)` on a log with 3 turns gives the 3 newest. A missing file gives an empty page.
  - `reset_flags`: the log holds turn 1, a reset, turn 2, then a reset. Turn 2 has `reset_before: true` and turn 1 has `false`, and the first page has `reset_pending: true`. `page(Some(2), None)` has `reset_pending: false`, and its turn 1 has `reset_before: false`.
  - `next_turn_counts_crash_cut_turns`: turn 1 has a summary, and turn 2 has a step but no summary. `next_turn()` is 3, and turn 2's `summary` is `None`.
  - `store_delete_removes_file_and_cache`
  - `session_store_ignores_trace_files` (in `agents/session_store.rs` tests): a planted `s-1-0.trace.jsonl` in the sessions dir is not loaded as a session, and it does not log a broken-session error.
- [ ] **Step 2:** Run `scripts/hive-test.sh -- trace`. Expected: FAIL to compile, because there is no module.
- [ ] **Step 3: Implement** in `trace.rs`.
  - Append is `OpenOptions::new().create(true).append(true)`, writing `serde_json::to_string(record) + "\n"`, then `flush`.
  - `page` reads the whole file, parses each line, groups the steps and summaries by turn, and marks resets in log order.
  - The cache is a `Mutex<Option<u64>>` for the next turn number. It is bumped by `append` of a step or turn record whose turn is ≥ the cached value.
- [ ] **Step 4:** Run `scripts/hive-test.sh`. Expected: the whole lib passes. Then run `scripts/hive-test.sh --restore`.
- [ ] **Step 5: Commit** `feat(runtime): trace records, per-session trace log and store`.

### Task 2: Thinking-token counter and per-engine token counts

**Files:**
- Modify: `backend/src/engine/tool_format.rs`. The counter sits next to the format seam that already knows the Gemma channel markers.
- Modify: `backend/src/engine/http.rs` tests

**Interfaces:**
- Produces:
  - `pub trait ThinkCounter: Send { fn push(&mut self, text: &str, tokens: usize); fn think_tokens(&self) -> usize; }`
  - `pub struct GemmaThinkCounter`, built with `GemmaThinkCounter::new(prompt: &str)`. It starts inside a thinking channel when `OpenChannel::at_tail(prompt)` is a thinking opener, and when the opener is `<|channel>thought`.
  - `pub struct NoThinkCounter`, which always returns 0.
  - The existing format type gains `fn think_counter(&self, prompt: &str) -> Box<dyn ThinkCounter>`. Gemma 4 returns the Gemma counter; formats without channels return `NoThinkCounter`.
  - Rule: tokens are counted as thinking while the counter is inside a thinking channel. A chunk is attributed to the state the counter was in **at the start of the chunk**. The state then flips on each opener (`<|channel>thought`) or closer (`<channel|>`) found in the chunk text. The counter keeps a short carry of the previous chunk's tail, so a marker split across chunks is still seen.

- [ ] **Step 1: Write the failing tests** in `tool_format.rs`:
  - `think_counter_counts_inside_thought_channel`: chunks `"<|channel>thought\n"`(1), `"let me"`(2), `"<channel|>"`(1), `"Answer."`(2). This gives think 3 (the opener chunk plus "let me") and total 6.
  - `think_counter_starts_inside_when_prompt_ends_with_opener`: a prompt ending `"<|channel>thought\n"`, then chunks `"hmm"`(1), `"<channel|>Hi"`(2). This gives think 3, because the closer chunk started inside.
  - `marker_split_across_chunks_is_seen`: chunks `"<chan"`, `"nel|>"` after an opener close the channel.
  - `answer_only_step_has_zero_think`: no markers gives 0.
  - `no_think_counter_is_always_zero`.
- [ ] **Step 2:** Add the test `http_chunk_has_one_token_info_per_generated_token` in `engine/http.rs` tests. It parses an SSE payload with `completion_probabilities` of 2 entries and asserts `tokens.len() == 2`. It also asserts that the request body built for `complete` has `n_probs` > 0. The in-process engines (llamacpp, candle) push exactly one `TokenInfo` per sampled token (`llamacpp.rs:286`, `candle.rs:290`). Their tests need a real model, so Task 7's live proof checks them: `out_tokens` > 0 on both runtimes.
- [ ] **Step 3:** Run `scripts/hive-test.sh -- tool_format http`. Expected: FAIL.
- [ ] **Step 4: Implement**, then run `scripts/hive-test.sh` (whole lib) and `scripts/hive-test.sh --features llamacpp` (the counter is in a shared module). Expected: PASS. Then run `--restore`.
- [ ] **Step 5: Commit** `feat(engine): thinking-token counter behind the tool-format seam`.

### Task 3: ToolLoop measures steps and reports them to a TurnTrace

**Files:**
- Modify: `backend/src/runtime/tool_loop.rs`
- Modify: `backend/src/runtime/trace.rs` (`TurnTrace`, `NoTrace`, `SessionTrace`)
- Modify: `backend/src/runtime/server.rs`, only to keep it compiling: pass `NoTrace` and adapt `SessionLinks` to `StepLink`. The real wiring is Task 4.

**Interfaces:**
- Consumes: Task 1 records, and Task 2 `think_counter(prompt)`.
- Produces:
  - `pub struct StepLink { pub link: Arc<dyn WorkerLink>, pub load: Option<Duration> }`. `TurnLinks::link` now returns `Result<StepLink, String>`. `load` is `Some` only when that call performed a model load.
  - `#[async_trait] pub trait TurnTrace: Send { async fn step(&mut self, record: StepRecord, sink: &mut dyn LoopSink) -> Result<(), SinkClosed>; }`. The loop fills `turn: 0` and `at: 0`, and the trace sets both.
  - `pub struct NoTrace`, which accepts everything.
  - `ToolLoop::run(transcript, content, config, links, trace: &mut dyn TurnTrace, sink)`. The trace is a new parameter before `sink`, and the return type is unchanged.
  - `SessionTrace`, used in Task 4:
    - `SessionTrace::new(log: Arc<TraceLog>, session: String, user: &str) -> SessionTrace` takes `turn = log.next_turn()`, truncates `user` to 200 chars, and starts an `Instant`.
    - `async fn finish(self, reason: &str, committed: bool, sink: &mut dyn LoopSink) -> Result<(), SinkClosed>`
    - `pub(crate) fn reset(log: &TraceLog) -> io::Result<()>`
    - The trace accumulates the step count, `out_tokens`, `think_tokens` and `peak_fill` from the step records it sees.
  - How `SessionTrace::step` and `finish` behave:
    - They append first.
    - If the append fails, they notify `trace_warning {id, message}`.
    - They always then notify `trace_step {id, record}` or `trace_turn {id, record}`.
    - A `SinkClosed` from notify is returned, but the append has already happened.
  - Consts: `M_TRACE_STEP = "trace_step"`, `M_TRACE_TURN = "trace_turn"`, `M_TRACE_WARNING = "trace_warning"`, `REASON_ABORTED = "aborted"`.
- Measurement rules, all in `ToolLoop`:
  - `ttft_ms` runs from just before the worker `generate` request until the first `turn_chunk`.
  - `gen_ms` runs from the first `turn_chunk` until `turn_done`, `turn_error` or `turn_rejected`.
  - Both are `None` if no chunk arrived.
  - `out_tokens` is the sum of `turn_chunk.tokens` array lengths, and the counter is fed `(text, len)`.
  - `prompt_tokens` and `n_ctx` come from `turn_started`.
  - A tool's `ms` is measured around the executor call. `bytes` is the length of `serde_json::to_string(&result)`.
  - `end` is one of:
    - `ToolCalls`, when the tools ran;
    - `Budget`, when calls came back on the last budgeted step and were not run, with `tools: []`;
    - `Answered`;
    - `Rejected`, with `error` set to the rejection text;
    - `Error`, for a worker error or a broken stream, with `error` set;
    - `Aborted`.
  - A link failure at any step is recorded as a step with `prompt_tokens` and `n_ctx` `None`, `end: Error` and `error` set to the link text.
  - On `SinkClosed` anywhere inside a step, the loop calls `trace.step(...)` once with what it has measured and `end: Aborted`, ignores that call's result, and returns `Err(SinkClosed)`.

- [ ] **Step 1: Write the failing tests** in `tool_loop.rs`, using a `RecordingTrace` test type that collects `StepRecord`s, plus the existing fakes:
  - `each_step_is_traced_with_fill_tokens_and_end`: a two-step tool run. Step 1 has `end ToolCalls`, `prompt_tokens` and `n_ctx` from the fake `turn_started`, and `out_tokens` equal to the fake's chunk-token total. It also has one `ToolTiming {name, id: "call_1", ok: true, bytes: <len>}`. Step 2 is `Answered`. Both have `ttft_ms` and `gen_ms` set to `Some`.
  - `budget_cut_step_is_traced_as_budget`: with a budget of 1 and a model that calls a tool, there is one record, `end Budget` with `tools` empty.
  - `rejected_and_worker_error_steps_carry_text`
  - `link_failure_step_is_traced_with_no_fill`: a step-0 link `Err("boom")` gives `end Error`, `error "boom"` and `prompt_tokens None`.
  - `load_time_is_recorded_on_the_step_that_loaded`: `StepLink.load = Some(1500ms)` at step 0 gives `load_ms Some(1500)`. Step 1 has `None`.
  - `sink_closed_mid_step_flushes_an_aborted_step`: the sink closes after the first chunk. One record with `end Aborted` and `out_tokens` > 0, and `run` returns `Err(SinkClosed)`.
  - `thinking_tokens_reach_the_record`: the fake streams `"<|channel>thought\n"`, `"x"`, `"<channel|>"`, `"ok"`, giving `think_tokens` > 0 and `think_tokens` ≤ `out_tokens`.
  - `session_trace_appends_then_notifies_and_finishes` (in `trace.rs`): a `SessionTrace` over a temp log. `step` produces one log line and a `trace_step` note with `{id, record}`, where `record.turn == 1`. `finish("answered", true)` produces a turn line with `steps 1`, `peak_fill` = `prompt_tokens/n_ctx` and `committed true`, plus a `trace_turn` note.
  - `session_trace_write_failure_warns_and_still_notifies`: the log path is a directory. The notes are `trace_warning`, then `trace_step`.
  - `session_trace_appends_even_when_sink_is_closed`
- [ ] **Step 2:** Run `scripts/hive-test.sh -- tool_loop trace`. Expected: FAIL.
- [ ] **Step 3: Implement.** Existing loop tests must keep passing unchanged, apart from the new `trace` argument (pass `&mut NoTrace`) and `StepLink` in the fake links.
- [ ] **Step 4:** Run `scripts/hive-test.sh -- --tests`. Expected: PASS. Then run `--restore`.
- [ ] **Step 5: Commit** `feat(runtime): tool loop measures every step into a turn trace`.

### Task 4: Server and supervisor wiring, and `get_trace`

**Files:**
- Modify: `backend/src/runtime/server.rs` (deps, `run_turn`, `SessionLinks`, `get_trace`, `delete_session`, `reset`)
- Modify: `backend/src/main.rs` (`AgentsStack`: build the `TraceStore` on the sessions dir)

**Interfaces:**
- Consumes: Tasks 1 and 3.
- Produces:
  - `RuntimeDeps.traces: Arc<TraceStore>`.
  - Wire method `get_trace {id, before?: u64, limit?: usize}`, which needs no attach. It returns `TracePage` as JSON: `{turns:[{turn, steps, summary, reset_before}], more, reset_pending}`. An unknown session gives `unknown session: {id}`, code -32603. The method name const is `M_GET_TRACE = "get_trace"`.
  - `run_turn`:
    - It builds a `SessionTrace` and passes it to `run`.
    - After the commit decision, it calls `finish(reason, committed, &mut sink)`. `reason` is the `EndReason` in snake_case, or `"aborted"` on `Err(SinkClosed)`. `committed` is true only when the commit succeeded.
    - It ignores `finish`'s `SinkClosed`.
    - The `trace_turn` note goes out before the `send_turn` result, and before `sessions_changed`.
  - `SessionLinks::link` returns `StepLink`. At step 0, `load` is the measured duration of `link_for_turn` when `notify_loading` fired; otherwise it is `None`.
  - `delete_session` also calls `traces.delete(id)`. A failure there is logged to stderr and does not fail the delete.
  - `reset` appends a `ResetRecord` after a successful reset. A failure there is logged and ignored.

- [ ] **Step 1: Write the failing tests** in `server.rs`, using the existing `TestRuntime`/`Client` rigs:
  - `turn_streams_trace_step_and_trace_turn_before_result`: an answered turn's attached notes include `trace_step` (with `id` and `record.step == 1`), then `trace_turn` (`reason "answered"`, `committed true`), with `trace_turn` before the response. `get_trace` then returns one turn with one step and that summary.
  - `failed_turn_is_traced_uncommitted`: a `window_full` rejection gives a turn record with `reason "window_full"` and `committed false`, plus a step with `end "rejected"`.
  - `aborted_turn_is_in_the_log`: the client disconnects mid-turn. `get_trace` from a second client shows the turn with an `aborted` step and `summary.reason "aborted"`.
  - `first_message_load_records_load_ms`: on a `needs_load` session, step 1 has `load_ms` as `Some`. On a loaded model, step 1 has `load_ms` `None`.
  - `unwritable_log_warns_and_turn_still_commits`: the session's `.trace.jsonl` is pre-created as a directory. The client gets `trace_warning`, and the transcript is committed.
  - `get_trace_unknown_session_errors`, and `get_trace_needs_no_attach`.
  - `delete_removes_trace_and_reset_appends_marker`: after reset, `get_trace` has `reset_pending true`. After delete, the file is gone.
  - `turn_numbers_continue_after_restart`: a new `TestRuntime` over the same data dir continues at turn 2.
- [ ] **Step 2:** Run `scripts/hive-test.sh -- server`. Expected: FAIL.
- [ ] **Step 3: Implement.** Update the module docs' protocol list, and the spec's "Change to the shipped spec" line in `docs/specs/2026-11-23-agent-templates-sessions-design.md` (timing is now persisted in the trace log).
- [ ] **Step 4:** Run `scripts/hive-test.sh -- --tests`, then `scripts/hive-test.sh --features llamacpp` (main.rs is touched; use a long timeout), then `--restore`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(runtime): persist and stream the agent trace; get_trace`.

### Task 5: App client for the trace

**Files:**
- Modify: `app/src/renderer/src/agents.ts` (types)
- Modify: `app/src/renderer/src/RuntimeClient.ts`
- Modify: `app/src/renderer/src/RuntimeClient.test.ts`

**Interfaces:**
- Produces:
  - Types: `StepRecord`, `ToolTiming`, `TurnRecord`, `TraceTurn` and `TracePage`. They mirror Task 4's JSON field for field, in snake_case. `StepRecord.end` is `'tool_calls' | 'answered' | 'budget' | 'rejected' | 'error' | 'aborted'`.
  - `RuntimeClient.GetTrace(id: string, before?: number, limit?: number): Promise<TracePage>`. It omits undefined params.
  - Signals: `OnTraceStep: TypedSignal<{id: string, record: StepRecord}>`, `OnTraceTurn: TypedSignal<{id: string, record: TurnRecord}>` and `OnTraceWarning: TypedSignal<{id: string, message: string}>`.

- [ ] **Step 1: Write the failing tests:**
  - Add `GetTrace` rows to the existing table test: one with no paging, which sends `{id}`, and one with `before`/`limit`. Each checks the wire name, the params and the result mapping.
  - `trace_notifications_raise_signals`.
- [ ] **Step 2–4:** Run `cd app && npx vitest run src/renderer/src/RuntimeClient.test.ts`: it fails first, then implement, then it passes. Then run `npm test && npm run typecheck:web`.
- [ ] **Step 5: Commit** `feat(app): runtime client for the agent trace`.

### Task 6: Trace section in the instrument panel

**Files:**
- Create:
  - `app/src/renderer/src/TraceViewModel.ts`, which holds `TraceViewModel`, `TraceTurnViewModel` and `TraceStepViewModel`, or one file each if the app's pattern is one class per file
  - their test file
  - `app/src/renderer/src/views/TracePanel.mu`
- Modify:
  - `app/src/renderer/src/SessionViewModel.ts` (owns a `TraceViewModel` and drives it on attach, session switch and reconnect)
  - `app/src/renderer/src/views/InstrumentPanel.mu`, or the session view that hosts it: place `TracePanel` below the context bar, in its own scroll region

**Interfaces:**
- Consumes: Task 5.
- Produces:
  - `TraceViewModel`:
    - `Turns: ObservableCollection<TraceTurnViewModel>`, newest first
    - `LoadOlderCommand`, which `CanExecute` while `more`
    - `WarningText` and `HasWarning`
    - `ShowSession(client: RuntimeClient | null, id: string | null): Promise<void>`. This clears the list and loads the first page. It is called on attach, on switch and after reconnect.
    - It applies live signals only for its current id.
    - It deduplicates by `(turn, step)`, and a live record replaces the paged one.
    - A turn row appears at its first `trace_step`, and its summary fills in on `trace_turn`.
    - `reset_pending` shows a divider above the newest turn.
  - `TraceTurnViewModel`:
    - `Expanded`, which defaults to true for the newest turn and false for the others
    - `HeaderText`, `Steps` and `ResetBefore`.
    - `HeaderText` takes one of these forms:
      - `Turn {n} · {reason} · {steps} steps · {s} s · peak {pct}%`, where `{s}` has one decimal and `{pct}` is an integer;
      - `Turn {n} · running…` while it is the live turn with no summary;
      - `Turn {n} · incomplete` when there is no summary and it is not live.
  - `TraceStepViewModel`:
    - `Text`: `{step}  ctx {used}/{total} ({pct}%)`. Numbers use a thin-space thousands separator, as in the sketch (`3 102/8 192`). If fill is null, it is `{step}  —`.
    - `FillFraction`, for the bar.
    - `StatsText`: `out {n}`, then ` (think {k})` only when k > 0, then `  load {s} s` when present, then `  ttft {s} s  gen {s} s  {r} t/s` with one decimal each, omitting any absent part.
    - `Tools`: one `└ {name}  ok|failed  {ms} ms  {bytes} B` string each.
    - `ErrorText`, set when `end` is rejected, error or aborted. `aborted` shows as `aborted`.

- [ ] **Step 1: Write the failing tests:**
  - `first_page_lists_turns_newest_first_with_headers`: header text exactly as specified, for an answered turn of 2 steps, 21400 ms and peak 0.41: `Turn 3 · answered · 2 steps · 21.4 s · peak 41%`.
  - `step_text_formats_fill_tokens_and_timing`: `1  ctx 3 102/8 192 (38%)`, and stats `out 38  ttft 1.9 s  gen 2.6 s  14.6 t/s`. A think of 5 shows `out 38 (think 5)`.
  - `tool_rows_format`: `└ read_file  ok  4 ms  26 B`.
  - `live_step_then_turn_updates_in_place`: the header goes from `running…` to the final header.
  - `live_record_replaces_paged_duplicate`, which covers Review Focus 3.
  - `turn_without_summary_and_not_live_is_incomplete`, which covers Review Focus 4.
  - `load_older_appends_and_disables_at_end`
  - `reset_dividers_and_pending_reset`
  - `warning_shows_and_clears_on_switch`
  - `signals_for_other_sessions_are_ignored`
  - In the `SessionViewModel` tests: `attach_loads_trace_and_reconnect_reloads`.
- [ ] **Step 2–4:** Run vitest on the new tests: they fail first, then implement, then they pass. Then run `npm test && npm run typecheck:web && npm run typecheck:node`.
- [ ] **Step 5: Commit** `feat(app): agent trace in the instrument panel`.

### Task 7: Deploy and live proof (owner go required)

- [ ] **Step 1:** Run `scripts/hive-test.sh --restore`, then stop and ask the owner for the go to merge and push.
- [ ] **Step 2: After the go:**
  1. Merge to main and push.
  2. On the hive: `git pull --ff-only`, then `C:\llm-harness\harness-build\stop-sup.ps1`, then `build-both.cmd`. Expect `CANDLE_EXIT=0 LLAMA_EXIT=0 SUP_EXIT=0`.
  3. Run `schtasks /Run /TN harness-supervisor`, and check that `supervisor.out` shows the runtime listening line.
- [ ] **Step 3: Protocol proof** (a scratch script in D:\tmp):
  - A Researcher turn ("What does notes.txt say?") gives `trace_step` × 2: step 1 has a `read_file` tool timing and `ttft_ms`/`gen_ms`, and both have `out_tokens` > 0. Then `trace_turn` arrives with `answered`, `committed`.
  - `get_trace` returns it.
  - A thinking-on turn has `think_tokens` > 0.
  - A candle session turn has `out_tokens` > 0, which covers the in-process engines' token counts.
  - The first message on a `needs_load` session has `load_ms`.
- [ ] **Step 4: Owner UI check:**
  - the Trace section's headers, step rows and tool rows;
  - live fill-in during a turn;
  - Load older;
  - the reset divider;
  - traces present after an app restart.
- [ ] **Step 5:** Record the result in the spec status, the roadmap (step 2 shipped) and the memory note.
