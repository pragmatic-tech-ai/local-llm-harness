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

