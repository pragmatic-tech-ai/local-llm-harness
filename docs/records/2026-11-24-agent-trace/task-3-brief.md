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

