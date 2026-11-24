# Task 3 report: ToolLoop measures steps into a TurnTrace

Commit: 209a57f1 feat(runtime): tool loop measures every step into a turn trace (BASE 859cfa87).

## What was built
- `tool_loop.rs`: `StepLink`; `TurnLinks::link` returns `Result<StepLink, String>`; a private `StepMeter` per step (fill, tokens, think counter, ttft/gen clocks, tool timings, load); `ToolLoop::run` gains `trace: &mut dyn TurnTrace` before `sink` and reports every step exactly once. `StepEnd::Rejected`/`WorkerError` now carry their text.
- `trace.rs`: `TurnTrace`, `NoTrace`, `SessionTrace`.
- `server.rs` (compile-only): `SessionLinks` and `SwapBeforeSecondStep` return `StepLink { load: None }` (`SessionLinks::plain`); `run_turn` and the swap test pass `&mut NoTrace`. The model-identity check is unchanged.
- `test_support.rs`: `FakeConnector` returns `StepLink`; new `Notes::started_fill(prompt, prompt_tokens, n_ctx)` and `Notes::chunk_of(text, tokens)` (n `TokenInfo` entries).

## Public signatures for Task 4
`crate::runtime::tool_loop`:
- `#[derive(Clone)] pub struct StepLink { pub link: Arc<dyn WorkerLink>, pub load: Option<Duration> }`
- `TurnLinks::link(&self, step: usize) -> Result<StepLink, String>` (async_trait)
- `ToolLoop::run(&self, transcript: &[ChatMessage], content: &str, config: &TurnConfig, links: &dyn TurnLinks, trace: &mut dyn TurnTrace, sink: &mut dyn LoopSink) -> Result<(LoopEnd, Option<Vec<ChatMessage>>), SinkClosed>`

`crate::runtime::trace`:
- `#[async_trait] pub trait TurnTrace: Send { async fn step(&mut self, record: StepRecord, sink: &mut dyn LoopSink) -> Result<(), SinkClosed>; }`
- `pub struct NoTrace;` (accepts everything)
- `pub struct SessionTrace`
  - `pub fn new(log: Arc<TraceLog>, session: String, user: &str) -> SessionTrace`. It does not fail. If `next_turn` errors, it uses turn 0 and keeps the error as a pending warning. The user message is cut to 200 chars, and the `Instant` starts here.
  - `pub async fn finish(self, reason: &str, committed: bool, sink: &mut dyn LoopSink) -> Result<(), SinkClosed>`
  - `pub(crate) fn reset(log: &TraceLog) -> io::Result<()>`, which appends `ResetRecord { at: now }`. It carries a temporary `#[allow(dead_code)]` with a comment. Task 4 should remove that once `reset_session` calls it.
  - `impl TurnTrace for SessionTrace`: sets `turn` and `at` (Unix ms), and accumulates steps, out_tokens, think_tokens and peak_fill (max prompt_tokens/n_ctx with n_ctx > 0).
  - Consts: `SessionTrace::M_TRACE_STEP = "trace_step"`, `M_TRACE_TURN = "trace_turn"`, `M_TRACE_WARNING = "trace_warning"`, `REASON_ABORTED = "aborted"`.
- How notes are delivered by `step` and `finish`:
  1. Append.
  2. Send the pending warning, if any, as `trace_warning {id, message}`.
  3. If the append failed, send `trace_warning {id, message}`.
  4. Send `trace_step|trace_turn {id, record}`. The record is the bare StepRecord/TurnRecord JSON, with no `v` or `kind`.
  - Any `SinkClosed` is returned at once, but the append has already happened.

## Loop behaviour (for Task 4's notification-order expectations)
- Where the step record goes: each step is reported after its tools, and before the loop's own closing notes. So with a real trace, `trace_step` comes before `turn_error` and `loop_done` that the loop synthesizes. For worker `turn_error` and `turn_rejected`, it comes after the relayed note and before `loop_done`. With `NoTrace`, the notes are byte-identical to before.
- Step numbers are 1-based.
- A failure before `turn_started` is recorded as `end: error` with `prompt_tokens`/`n_ctx` null. This covers a link Err (at any step), `model_props` failure and `generate` failure. `steps` in `loop_done` stays as before, so on a NoHarness turn the trace has 1 step record while `loop_done.steps` is 0. That is by design (spec).
- Rejected error text: `"window full: {needed} tokens needed, {total} available"`. A worker error records `turn_error.message`. A broken stream records the link text or `STREAM_ENDED`. A malformed done records `BAD_TURN_DONE`.
- Aborted steps:
  - A sink closed while relaying, or at a `tool_result`, flushes one `Aborted` record. It holds the tokens and tools measured so far, and an open generation is timed up to now. The result of that flush is ignored.
  - A sink that closes during a step's own `trace.step` or the closing notes is not reported twice.
- `ttft` runs from just before `generate` until the first `turn_chunk`. `gen` runs from the first chunk until the done/error/rejected note, or until the stream broke. Both are None without a chunk. Notes are measured before they are relayed.
- Think counter: `ThinkCounters::for_prompt(turn_started.prompt)`. It falls back to `NoThinkCounter` until `turn_started` arrives.

## Tests (TDD evidence)
- New in tool_loop.rs (`RecordingTrace`, `StepLinks` test types):
  - each_step_is_traced_with_fill_tokens_and_end
  - budget_cut_step_is_traced_as_budget
  - rejected_and_worker_error_steps_carry_text (also covers broken stream and generate failure)
  - link_failure_step_is_traced_with_no_fill
  - link_failure_mid_loop_is_traced_as_its_own_step
  - load_time_is_recorded_on_the_step_that_loaded
  - sink_closed_mid_step_flushes_an_aborted_step
  - sink_closed_at_tool_result_flushes_the_tools_run_so_far
  - thinking_tokens_reach_the_record
  - trace_adds_no_notes_of_its_own
- New in trace.rs:
  - session_trace_appends_then_notifies_and_finishes
  - session_trace_numbers_turns_and_cuts_the_user_message
  - session_trace_write_failure_warns_and_still_notifies (the directory is created after `new`, so only the append fails)
  - session_trace_over_an_unreadable_log_uses_turn_zero_and_warns_once (pending warning)
  - session_trace_appends_even_when_sink_is_closed
  - reset_appends_a_marker
- Existing tests: unchanged except the mechanical changes. The fixture passes `&mut self.trace` (a RecordingTrace), the direct `run` calls pass `&mut NoTrace`, and the fakes return `StepLink`.
- RED: the code and tests were written together, so there was no pre-implementation RED run. RED was shown by mutation instead. With `StepMeter::observe` made a no-op and `SessionTrace` append skipped, `scripts/hive-test.sh -- runtime::` gave `FAILED. 157 passed; 9 failed` (the 4 measuring loop tests and the 5 SessionTrace tests). The sources were then restored and cmp-verified.
- GREEN:
  - `scripts/hive-test.sh -- runtime::` gave 166 passed.
  - `scripts/hive-test.sh -- --tests` gave lib 399 passed, plus integration tests 7/10/6/2/5 passed, TEST_EXIT=0. The only warning was `reset` unused, which got the temporary allow, and the run was repeated clean.
  - `--restore` done.
- The brief's `-- tool_loop trace` form is rejected by cargo (one filter only), so I used `-- runtime::`.

## Self-review / concerns
- When the sink closes inside the `trace.step` notify of a normal end, the step is already appended with its real end. The loop returns Err without an extra Aborted record, so no step is ever reported twice.
- The rejection text is composed by the runtime (the worker sends only needed/total). Its wording is my call.
- rustfmt was run on the 4 touched files, and only my lines changed. LF endings. No new crates.

## Fix round 1
Commit: test(runtime): pin single step report on late sink close and multi-step trace totals.
- Minor 1: `RecordingTrace` gained `close: bool`. When it is set, `step` stores the record and then returns `Err(SinkClosed)`. New tests in tool_loop.rs:
  - `sink_closed_in_the_trace_notify_keeps_the_real_end_once`: one `Answered` record, no `loop_done`, `Err`.
  - `sink_closed_at_loop_done_keeps_the_real_end_once`: the sink is `closing_after(4)`; one `Answered` record and `Err`.
  - `sink_closed_at_closing_turn_error_keeps_the_real_end_once`: a link error with `closing_after(0)`; one `Error` record with the text "boom", and `Err`.
- Minor 2: new test `session_trace_totals_add_up_over_steps` in trace.rs. It sends six steps:
  - Fills of 0.1, 0.4 and 0.2 count.
  - Fills of 900/None, 900/0 and None/1000 are skipped.
  - The test asserts `steps` 6, `peak_fill` 0.4, and the summed out and think tokens.
- Minor 3: added the blank lines before `each_step_is_traced_with_fill_tokens_and_end` and `session_trace_appends_then_notifies_and_finishes`.
- Tests: `scripts/hive-test.sh -- --tests` passed: lib 403, plus integration tests 7/10/6/2/5, TEST_EXIT=0, no warnings. `--restore` done. These tests pin behaviour that already worked, so there was no RED.
