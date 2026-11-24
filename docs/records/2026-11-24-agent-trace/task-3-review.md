### Spec Compliance

- ✅ `StepLink { link, load }`. `TurnLinks::link` returns `Result<StepLink, String>`, and `load` goes onto the step's meter (tool_loop.rs:297-305 area).
- ✅ `ToolLoop::run` takes `trace: &mut dyn TurnTrace` before `sink`, and the return type is unchanged.
- ✅ `ttft_ms` starts just before `generate`, at `meter.requested()` (tool_loop.rs:327). It ends at the first `turn_chunk`, measured in `observe` before the chunk is relayed (tool_loop.rs:142).
- ✅ `gen_ms` runs from the first chunk until `turn_done`, `turn_rejected` or `turn_error`. For a broken stream it runs until the stream broke, and for an aborted step until now (tool_loop.rs:188). Both are `None` without a chunk.
- ✅ `out_tokens` is the saturating sum of `tokens.len()`, and the counter is fed `(text, len)`.
- ✅ The counter comes from `ThinkCounters::for_prompt(turn_started.prompt)`, which is the controller's ruling. Before `turn_started` the meter uses `NoThinkCounter`.
- ✅ The worker really sends `prompt`, `prompt_tokens` and `n_ctx` in `turn_started`. I checked `rpc.rs:426` against `StepMeter`'s field consts.
- ✅ Tool `ms` is timed around `executor.execute` alone (tool_loop.rs:373). `bytes` is `serde_json::to_string(&result).len()`, and `ok` is `outcome.is_ok()`.
- ✅ The ends are all present:
  - `ToolCalls` is recorded after all tools have run.
  - `Budget` is recorded with `tools: []` before `break` (tool_loop.rs:368), so the calls never run.
  - `Answered` is recorded before `finish`.
  - `Rejected` carries the text.
  - `Error` covers a worker `turn_error` (using its `message`), a broken stream, a `generate` failure, a `model_props` failure and `BAD_TURN_DONE`.
  - `Aborted` is used on `SinkClosed` while relaying or at `tool_result`.
- ✅ A link failure at any step is recorded with step number `steps+1`, `prompt_tokens` and `n_ctx` `None`, `end: Error` and the link text.
- ✅ The aborted flush works as specified. `aborted()` (tool_loop.rs:426) calls `trace.step` once with the measurements so far, ignores the result, and returns `Err(closed)`. Every `sink.notify` inside a step goes through it: the relay in `read_step` and the `tool_result`. No step is ever reported twice:
  - A close during the trace's own notify leaves the real-end record already appended.
  - A close during the closing `turn_error`/`loop_done` comes after the step was reported.
- ✅ `NoTrace` leaves the loop unchanged. Every old exit maps to the same notes and the same result:
  - `Rejected` goes to `finish(WindowFull)`.
  - `WorkerError` goes to `finish(Error)`.
  - `Broken`, `model_props`, `generate` and `BAD_TURN_DONE` go to `fail`.
  - The budget case goes to `break` and then `StepLimit`.
  - A closed sink at relay or `tool_result` returns `Err`.
  - `NoTrace` never errors, so the new `?` after `trace.step` is inert.
  - The commit-only-after-`loop_done` behaviour is preserved.
- ✅ `SessionTrace` behaves as specified:
  - `new` is infallible, falling back to turn 0 with a pending warning (controller ruling). It cuts `user` to 200 chars and starts its `Instant`.
  - `deliver` (trace.rs:496) appends first, then sends the pending warning, then a warning for a failed append, then `trace_step` or `trace_turn` as `{id, record}`.
  - A closed sink is returned after the append has happened.
- ✅ The accumulators are correct (trace.rs:528):
  - `steps` is incremented once per record, aborted steps included.
  - `out_tokens` and `think_tokens` are saturating sums.
  - `peak_fill` is the max of `prompt_tokens/n_ctx`, skipped when either is `None` or `n_ctx == 0`.
- ✅ The consts `M_TRACE_STEP`, `M_TRACE_TURN`, `M_TRACE_WARNING` and `REASON_ABORTED` exist, and `reset(log)` appends a `ResetRecord`.
- ✅ No lock is held across the trace write.
  - `TraceLog::append` and `next_turn` take std mutex guards in inner scopes, and the guards drop before `deliver` reaches any `.await`. I checked `trace.rs:158-181` and `202-214` for this.
  - `appended` is a plain `io::Result`.
  - The loop holds no locks.
  - `SessionLinks::bound()` guards are temporaries.
- ✅ `server.rs` changes only what it needs to compile: `NoTrace` and `SessionLinks::plain`, with the identity check untouched.

### Strengths

- `StepMeter` is a clean place to keep the measurements. Each note is measured once, before it is relayed, so timings do not include the relay.
- The `failed_step` and `aborted` helpers keep the many exits in `run` uniform, and they make "one report per step" easy to audit.
- The tests go beyond the brief. They add a mid-loop link failure, an abort at `tool_result` that keeps the measured tools, a test that the trace adds no notes of its own, a pending warning that is sent only once, and the reset marker.

### Judging the implementer's concerns

- **No true RED run.** Accepted. The new tests cannot compile without the new signatures, so a pre-implementation RED would only be a compile failure. The mutation check is stronger evidence: making `observe` a no-op and skipping the append failed 9 tests.
- **The filter `-- runtime::`.** Accepted. Cargo takes one positional filter, so the brief's form is invalid. The full `--tests` run passed, with TEST_EXIT=0.
- **The temporary `allow(dead_code)` on `reset`.** Accepted, because it is commented and has a named owner. Task 4 must remove it.
- **`trace_step` before the loop's own `turn_error`/`loop_done`.** Accepted. The spec writes the step record "when the step's tools have finished", and the "Live and history overlap" detail needs no ordering guarantee. The order is also the natural one: step, then turn.
- **One step record on a no-harness turn while `loop_done.steps` is 0.** Accepted. The spec's Records section explicitly requires a step record for a step-0 link or load failure. The turn record's `steps` counts records as the brief says, and only `reason` is "verbatim".
- **Their own wording for the rejection text.** Accepted. The worker's `turn_rejected` carries only `{needed, total}` (I checked `rpc.rs:297-299`), so the runtime has to compose some text.

### Issues

#### Critical

None.

#### Important

None.

#### Minor

1. **tool_loop.rs tests: the "never reported twice" claim is untested.** The report claims that a sink closing inside `trace.step`, or during the closing `turn_error`/`loop_done`, produces exactly one record with the real end. The code is correct, but no test pins it.
   - Why it matters: this is the subtle half of the aborted-flush contract, and a later refactor could easily route those closes through `aborted()` and record the step twice.
   - Fix: add a test with a `RecordingTrace` whose `step` returns `Err(SinkClosed)`, or a sink closing at `loop_done`. Assert one record, a non-`Aborted` end, and `Err(SinkClosed)` from `run`.
2. **trace.rs:845 and the tests after it: the accumulators are only exercised with one step.** No test covers `peak_fill` taking the max over several steps, the skips for `n_ctx == 0` and `None`, or `out_tokens`/`think_tokens` summing across steps. These are exactly what Task 4's turn record relies on.
   - Fix: add one test that feeds two or three step records with different fills, including one with `n_ctx: None`. Assert the turn's `steps`, `peak_fill` (the max) and the token totals.
3. **tool_loop.rs:1608 and trace.rs:844: missing blank lines.** The new test functions follow the previous test's closing `}` with no blank line between them. rustfmt does not insert blank lines between items, so this is an inconsistency with the rest of the module.
   - Fix: add the blank line in both places.

### Assessment

**Spec compliance:** ✅
**Task quality:** Approved
**Reasoning:** The measurement points, the Budget end, the single aborted flush, append-before-notify and the accumulators all match the spec and the controller's rulings, and the loop is unchanged under `NoTrace`. The only gaps are test coverage for the "no double report" path and the multi-step accumulators, plus two missing blank lines.
