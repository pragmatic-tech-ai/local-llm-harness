# Task 4 report: server and supervisor wiring, and `get_trace`

BASE 82b2e4dd. Commit: d9d70a3e feat(runtime): persist and stream the agent trace; get_trace.

## What was built

- `backend/src/runtime/server.rs`
  - `RuntimeDeps.traces: Arc<TraceStore>`.
  - `SessionLinks::link(0)` returns `StepLink { link, load }`. `load` is `Some(elapsed)`, measured
    from just before `Dispatcher::link_for_turn` until it returns, only when its `notify_loading`
    callback fired (a `loaded` flag set inside the callback). Otherwise `None`. Later steps are
    unchanged (`load: None`).
  - `start_turn` takes the session's trace log (`deps.traces.log(id)`) after taking busy and
    BEFORE reading the record. Reason: `delete_session` deletes the session first and the trace
    second, so if the record read succeeded, the trace delete comes after `log()` and retires this
    handle; the turn's appends can never recreate a deleted session's log. Errors are carried as
    `Result<Arc<TraceLog>, String>` into the future.
  - `run_turn(deps, conn, id, rec, content, log)`:
    - `Ok(log)`: builds `SessionTrace::new(log, id, content)` and passes it to `ToolLoop::run`.
    - `Err(message)` (an invalid id, never expected for a stored session): sends one
      `trace_warning {id, message}` to the attached connections (best effort, result ignored)
      and runs the turn with `NoTrace`.
    - After the loop: commit if the loop returned a transcript; then `finish(reason, committed,
      &mut sink)` with `reason = EndReason::name()` (snake_case, exactly `loop_done.reason`) or
      `"aborted"` on `Err(SinkClosed)`; `committed` is true only when the commit succeeded. The
      `SinkClosed` from `finish` is ignored (the record is already in the log).
    - Then the result: `sessions_changed` broadcast (committed), the `answer shown but not saved`
      error (commit failed; turn record has `committed:false`), `null`, or the existing
      deleted/detached/gone errors. So `trace_turn` is queued before `sessions_changed` and before
      the `send_turn` response.
  - `get_trace {id, before?: u64, limit?: usize}`: a `SharedRequests` method (no attach; runs in
    its own task, takes no hub lock). Unknown session: `unknown session: {id}` (code -32603).
    Then `traces.log(id)?.page(before, limit)?.wire()`; an IO error is -32603 with its text.
    Const `RuntimeConnection::M_GET_TRACE = "get_trace"`.
  - `delete_session`: after `sessions.delete` and `hub.detach_session`, `traces.delete(id)`; an
    error is logged to stderr (`trace log not deleted for {id}: {e}`) and the delete still
    succeeds.
  - `reset`: after a successful `sessions.reset`, `traces.log(id)` + `SessionTrace::reset(&log)`;
    an error is logged (`trace reset marker not written for {id}: {e}`) and ignored. Runs under
    the busy flag, no hub/session-store lock held.
  - Module docs: a paragraph on the trace notes, `get_trace`, reset and delete.
- `backend/src/runtime/trace.rs`
  - Removed the temporary `#[allow(dead_code)]` (and its comment) on `SessionTrace::reset`.
  - New `pub fn TraceRecord::line_value(&self) -> Value`: the record as its log line
    (`"v":1` and `"kind"` included), via the existing private `TraceLine`.
  - New `pub fn TracePage::wire(self) -> Value`: the `TracePage` shape with every step and
    summary as `line_value()`. Private serialize-only `WireTurn`/`WirePage` structs (no key
    literals).
  - New `pub(crate) fn SessionTrace::warning(session: &str, message: &str) -> Value` (the
    `trace_warning` params; `warn` uses it, and so does the server's no-log path).
  - Test `wire_page_is_the_page_shape_with_log_line_records`.
- `backend/src/runtime/tool_loop.rs`: `pub fn EndReason::name(self) -> String` (the serde
  snake_case name, so it is `loop_done.reason` verbatim).
- `backend/src/runtime/test_support.rs`: `TestRuntime` gets `traces` (on its sessions dir),
  `TestRuntime::reopen(self, link) -> TestRuntime` (a fresh runtime over the same data dir, as
  after a restart), `TestRuntime::sessions_dir()`; `new` delegates to a private `over(dir, link)`.
- `backend/src/main.rs`: `AgentsStack` has `traces: TraceStore`, built in `open_stores` as
  `TraceStore::new(data_dir/sessions)` (the same dir the `SessionStore` opens), and moved into
  `RuntimeDeps.traces`.
- `docs/specs/2026-11-23-agent-templates-sessions-design.md`: the "not persisted" line now says
  logprobs are not persisted, timing and token counts are, in `{id}.trace.jsonl`, and a reloaded
  session shows its trace via `get_trace`.

## Wire contract for Task 5 (copy field for field)

Notification frames are `{"method": ..., "params": ...}` (no `jsonrpc` field).

One `trace_step` note (live record is the BARE StepRecord, no `v`/`kind`, as Task 3 built it):

```json
{"method":"trace_step","params":{"id":"s-1795474000000-0","record":{"turn":3,"step":1,"at":1795474024627,"prompt_tokens":3102,"n_ctx":8192,"out_tokens":38,"think_tokens":0,"load_ms":null,"ttft_ms":1900,"gen_ms":2600,"end":"tool_calls","tools":[{"name":"read_file","id":"call_1","ok":true,"ms":4,"bytes":26}],"error":null}}}
```

`trace_turn`: `{"method":"trace_turn","params":{"id":"…","record":{"turn":3,"at":1795474046000,"steps":2,"reason":"answered","total_ms":21400,"out_tokens":59,"think_tokens":0,"peak_fill":0.41,"committed":true,"user":"What does notes.txt say?"}}}`

`trace_warning`: `{"method":"trace_warning","params":{"id":"…","message":"<io error text>"}}`

One `get_trace` result (`{"id":"…"}` or `{"id":"…","before":3,"limit":20}`); records are in their
log-line shape, `v` and `kind` included; `summary` is `null` for a running / crash-cut turn:

```json
{"turns":[
  {"turn":3,
   "steps":[{"v":1,"kind":"step","turn":3,"step":1,"at":1795474024627,"prompt_tokens":3102,"n_ctx":8192,"out_tokens":38,"think_tokens":0,"load_ms":null,"ttft_ms":1900,"gen_ms":2600,"end":"tool_calls","tools":[{"name":"read_file","id":"call_1","ok":true,"ms":4,"bytes":26}],"error":null},
            {"v":1,"kind":"step","turn":3,"step":2,"at":1795474046000,"prompt_tokens":3360,"n_ctx":8192,"out_tokens":21,"think_tokens":0,"load_ms":null,"ttft_ms":1200,"gen_ms":1500,"end":"answered","tools":[],"error":null}],
   "summary":{"v":1,"kind":"turn","turn":3,"at":1795474046000,"steps":2,"reason":"answered","total_ms":21400,"out_tokens":59,"think_tokens":0,"peak_fill":0.41,"committed":true,"user":"What does notes.txt say?"},
   "reset_before":false},
  {"turn":2,"steps":[],"summary":null,"reset_before":true}],
 "more":false,
 "reset_pending":false}
```

Field order follows the Rust structs; `end` is one of `tool_calls|answered|budget|rejected|error|aborted`;
`reason` is a `loop_done` reason (`answered|step_limit|window_full|error|no_harness`) or `aborted`.
Errors: `unknown session: {id}` (-32603); bad params -32602.

Notification order on one turn (attached connections): `session_loading` (if loading) …
loop notes with `trace_step` after each step's tools and before the loop's closing
`turn_error`/`loop_done` … `loop_done`, `trace_turn`, then (committed) `sessions_changed`,
`session_busy false`, and the `send_turn` response. A `trace_warning` precedes the record it
concerns.

## Tests (server.rs)

New: `turn_streams_trace_step_and_trace_turn_before_result` (real websocket; trace_step <
trace_turn < sessions_changed, all before the response; `get_trace` has one turn, one step, `v`
/`kind` present, summary fields equal the live record), `failed_turn_is_traced_uncommitted`
(window_full, committed false, step end rejected), `aborted_turn_is_in_the_log` (hang up while
the gated load runs; second, unattached client sees the aborted step and `reason "aborted"`),
`first_message_load_records_load_ms` (needs_load: turn 1 step 1 `load_ms` a number; turn 2
`null`), `unwritable_log_warns_and_turn_still_commits` (`.trace.jsonl` pre-created as a dir),
`get_trace_unknown_session_errors`, `get_trace_needs_no_attach` (also with before/limit),
`delete_removes_trace_and_reset_appends_marker`, `turn_numbers_continue_after_restart`
(`TestRuntime::reopen`). Changed: `create_attach_send_commits` now expects
`[trace_turn, sessions_changed, session_busy]` after `loop_done`. Plus the trace.rs wire test.

## TDD evidence

- RED (mutation, as the tests were written with the wiring): with `run_turn` building no
  `SessionTrace`, the load flag filtered out, `traces.delete` and the reset marker skipped,
  `scripts/hive-test.sh -- server` gave `FAILED. 45 passed; 8 failed` (the 7 new trace-writing
  tests and `create_attach_send_commits`); TEST_EXIT=101. The source was restored and
  cmp-verified.
- GREEN: `scripts/hive-test.sh -- server`: 53 passed, TEST_EXIT=0, no warnings.
- `scripts/hive-test.sh -- --tests`: lib 413 passed, integration 7/10/6/2/5 passed, TEST_EXIT=0,
  no warnings.
- `scripts/hive-test.sh --features llamacpp`: lib 421 passed, TEST_EXIT=0, no warnings.
- `scripts/hive-test.sh --restore`: done.

## Self-review / concerns

- **Live vs paged record shape (for the controller).** As instructed, `get_trace` records carry
  `v` and `kind`; the live `trace_step`/`trace_turn` records stay bare (Task 3's reviewed
  contract). Task 5 must accept both (treat `v`/`kind` as optional, ignored). Making the live
  notes carry them too is a two-line change in `SessionTrace::step`/`finish` (`line_value()`)
  plus one Task 3 test assertion; not done, to keep Task 3's contract.
- The no-log `trace_warning` is sent before `ToolLoop::run`, so it precedes `turn_started`.
- `get_trace` reads the whole log (as Task 1 designed); it runs in its own task, no lock held
  across file IO except the TraceLog's own.
- rustfmt run on the touched files; only my lines changed. LF endings. No new crates;
  Cargo.lock unchanged.

## Fix round 1

Commit: d43c9f16 fix(runtime): live trace records in log-line shape; reset takes its trace handle first.

- **0 (ruling): live records carry `v` and `kind`.** `SessionTrace::deliver` now sends
  `record.line_value()` (the `step`/`finish` callers no longer build a bare `live` value), so
  live, logged and paged records are the same JSON. Task 3's assertion in
  `session_trace_appends_then_notifies_and_finishes` now compares with
  `TraceRecord::Turn(turn.clone()).line_value()`. New server test
  `live_trace_step_is_byte_equal_to_the_paged_step`: `serde_json::to_string` of the live
  `trace_step` record equals that of `get_trace`'s step (and the same for `trace_turn` vs
  `summary`). Module docs updated. **This supersedes the "live record is bare" note above.**
- **1 (Important): reset ordering.** `reset` takes `traces.log(id)` before the record read and
  `sessions.reset`, the same ordering as `start_turn`. A racing delete either makes the
  read/reset fail or retires this handle, dropping the marker. A deterministic test is not
  feasible because there is no hook between `sessions.reset` and the append, so the ordering is
  documented in a comment there.
- **2 (Minor): eviction.** New `SharedRequests::forget_trace_if_gone(deps, id)`. It calls
  `traces.delete(id)` only when `!deps.sessions.contains(id)`, so a live session's log is never
  touched. Ids are never reused, so a session found gone stays gone. An `InvalidInput` (an id
  that is not a plain file name) is silent; any other error is logged. Where it is called:
  - in `start_turn` when the record read fails;
  - in `reset` when the read or the reset fails;
  - in `get_trace` when the session is unknown. `get_trace` now takes the handle before the
    `contains` check, which closes the window between them.

  New test `get_trace_on_a_gone_session_forgets_its_log_only`: a stray `s-0-0.trace.jsonl` is
  removed by `get_trace`, and a live session's log and page are untouched.
- **3 (Minor): test field-name consts.** I added `REASON`, `COMMITTED`, `END`, `KIND`, `TURN`,
  `LOAD_MS`, `V`, `STEP`, `RESET_PENDING` and `WINDOW_FULL`, and the new tests use them. `"id"`
  stays inline, as in the rest of the module's tests.

Tests: `scripts/hive-test.sh -- --tests` gave lib 415 passed and integration 7/10/6/2/5 passed,
TEST_EXIT=0, no warnings. `--restore` is done. After the run, only a module-doc comment changed.

### Updated `trace_step` note (Task 5 copies this)

```json
{"method":"trace_step","params":{"id":"s-1795474000000-0","record":{"v":1,"kind":"step","turn":3,"step":1,"at":1795474024627,"prompt_tokens":3102,"n_ctx":8192,"out_tokens":38,"think_tokens":0,"load_ms":null,"ttft_ms":1900,"gen_ms":2600,"end":"tool_calls","tools":[{"name":"read_file","id":"call_1","ok":true,"ms":4,"bytes":26}],"error":null}}}
```

`trace_turn` follows the same rule: `{"method":"trace_turn","params":{"id":"…","record":{"v":1,"kind":"turn","turn":3,...}}}`.
The record is identical to the `get_trace` step and summary objects shown above. On the wire,
keys come out in alphabetical order, because records pass through `serde_json::Value`, so do not
rely on field order.
