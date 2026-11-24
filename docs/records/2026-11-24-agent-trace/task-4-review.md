### Spec Compliance

- ✅ `RuntimeDeps.traces: Arc<TraceStore>`. main.rs builds it as `TraceStore::new(sessions_dir)`, on the same directory `SessionStore::open` uses (main.rs, `open_stores`). `TestRuntime` does the same.
- ✅ `run_turn` builds a `SessionTrace` and passes it to `ToolLoop::run` (server.rs `run_turn`, the `session_trace`/`trace` match). On a `TraceStore::log` Err, it sends one `trace_warning` through the sink and runs with `NoTrace`, per the controller ruling.
- ✅ **Ordering.** The commit is decided first, then `finish(name, committed, &mut sink)` runs, and only after that come `sessions_changed` (the broadcast), the `answer shown but not saved` error, or `null`. Every frame goes through the one per-connection hub channel, so `trace_turn` is queued ahead of both `sessions_changed` and the `send_turn` response. The busy guard (`_busy` in the future) is held across `finish`, so `trace_turn` also comes before `session_busy false`. Two tests pin this: `create_attach_send_commits` (`[trace_turn, sessions_changed, session_busy]` after `loop_done`) and the real-websocket test (step < turn < sessions_changed, all before the response).
- ✅ **committed.** It is `matches!(saved, Some(Ok(())))`, so it is true only when `sessions.commit` returned Ok. A `window_full` or other non-answered end, a commit error, and `SinkClosed` all give false. The window_full case is tested.
- ✅ **Aborted path.** `Err(SinkClosed)` maps to `REASON_ABORTED`, and `finish` still runs. Its `SinkClosed` is ignored after the append, which Task 3 guarantees happens first. `aborted_turn_is_in_the_log` reads the turn back from a second, unattached client: the last step has `end "aborted"`, the summary has `reason "aborted"` and `committed false`. After the trace, the existing deleted, detached and client-gone error mapping is unchanged.
- ✅ `reason` is `EndReason::name()`, derived from the enum's own serde `snake_case`, so it matches `loop_done.reason` exactly.
- ✅ **load_ms.** At step 0 it is `Some(elapsed)` only when the `notify_loading` callback set `loaded`, otherwise `None`. Later steps are unchanged. `first_message_load_records_load_ms` checks a number on turn 1 and `null` on turn 2, and that only one load happened.
- ✅ **get_trace.** It goes through `SharedRequests`, so it needs no attach and runs in its own task. An unknown session gives `unknown session: {id}` with code -32603 (`RpcError::internal(SessionError::unknown)`), and the test asserts both the code and the message. Bad params give -32602 via `Frames::params`. The result is `TracePage::wire()` in the spec's shape. The const is `M_GET_TRACE = "get_trace"`.
- ✅ **Delete.** `delete_session` calls `traces.delete(id)` after `sessions.delete` and `detach_session`. An error goes to stderr and the delete still succeeds. `TraceStore::delete` retires a handle that is still held (trace.rs:410), so a running turn cannot recreate the file.
- ❌ **Reset.** It appends a `ResetRecord` after a successful reset and logs a failure, as required. But it can recreate a deleted session's log (Important 1).
- ✅ **Restart.** `turn_numbers_continue_after_restart` reopens a `TestRuntime` over the same data directory, and the next turn is 2. `get_trace` returns `[2, 1]`.
- ✅ **Concurrency.** `TraceLog` takes its own lock inside `append`, and the guard drops before `notify`. The session-store commit finishes before `finish`. The hub lock is taken per frame only. No hub or session-store lock is held across a trace append.
  - The busy flag serialises a session's turns and resets, so two turns never interleave appends on one log.
  - A `get_trace` that runs during a turn reads a consistent prefix, and Task 1 makes the reader tolerate a partial last line.
- ✅ The module docs' protocol paragraph and the "Change to the shipped spec" line in the sessions spec are both updated.
- ⚠️ I could not verify the hive test results (53 server tests and the full `--tests` and llamacpp runs). These are the implementer's claims, and I ran no cargo.

### Strengths

- `run_turn` is restructured cleanly into three stages: decide the commit, write the turn record, then map the result. This makes the "trace_turn before result and sessions_changed" ordering structural instead of incidental.
- The `start_turn` race reasoning is correct for turns, given that `TraceStore::delete` retires the removed handle.
- The tests are thorough: a real websocket for ordering, a second unattached client for the aborted turn, a restart over the same directory, and a directory planted as the log file for the append-failure path.

### Judging the implementer's concerns

- **`start_turn` takes the log before reading the record.** Accepted for turns.
  - If the record read succeeds and a delete follows, `TraceStore::delete` removes and retires this exact handle, so the turn's appends are dropped.
  - If the delete came first, the record read fails.
  - The same protection is missing from `reset` (Important 1), and the early take leaves a cache entry behind (Minor 1).
- **The store-Err `trace_warning` is sent before `turn_started`.** Accepted. The spec only requires a warning to precede the record it concerns, and there is no record here. `TraceStore::log` fails only on an invalid id (`check_id`), which no stored session can have, so this path is a defensive one.
- **Helpers outside server.rs.** Accepted. Each helper sits on the type it describes, and each is an associated fn or method:
  - `TraceRecord::line_value` and `TracePage::wire` are on the trace types, so the log-line shape keeps one home (`TraceLine`).
  - `EndReason::name` reuses the enum's serde naming rather than a second string table.
  - `SessionTrace::warning` removes a duplicated params literal.
  - `TestRuntime::reopen` and `sessions_dir` are test rig additions the restart test needs.

### Issues

#### Critical

None.

#### Important

1. **server.rs `reset` (the `traces.log(&rec.id).and_then(SessionTrace::reset)` block): a concurrent delete can leave an orphan trace log.**
   - What's wrong: `delete_session` does not take the busy flag. The race runs like this:
     1. `sessions.reset` succeeds.
     2. Another connection's `delete_session` runs in full. Its `traces.delete` evicts the cache entry and retires the old handle.
     3. `reset` then calls `traces.log(id)`. Because `TraceStore` keeps no tombstone, it gets a fresh, non-retired handle, and the append creates `{id}.trace.jsonl` for a session that no longer exists.
   - Why it matters: the result is a stray file that nothing ever deletes, plus a stale cache entry. It breaks the spec's "Deleting a session deletes its log". This is the exact race the implementer guarded against in `start_turn`, missed here.
   - Fix: take `let log = self.deps.traces.log(&rec.id)` before `sessions.reset`, as `start_turn` does. A delete after the reset then retires this handle and the marker is dropped. A delete before the reset makes the reset fail. Add a comment pointing at `start_turn`'s reasoning.

#### Minor

1. **server.rs `start_turn` and `get_trace`: `traces.log(id)` caches a handle for a session that is already gone.**
   - What's wrong: in `start_turn`, the log is taken before the record read. If the session was deleted before `log()`, the record read fails, but `TraceStore` keeps the new entry. `get_trace` has the same window between `contains` and `log`. No file is created, because the open is lazy, but the map grows by one entry for each such race and never shrinks.
   - Fix: in `start_turn`, on a failed record read, call `traces.delete(&id)` before returning `no_session`. The handle is empty, so this only evicts it. Alternatively, have `TraceStore` drop entries for ids it has deleted.
2. **server.rs tests: field-name literals are repeated inline.** `"reason"`, `"committed"`, `"end"`, `"kind"`, `"v"`, `"turn"` and `"load_ms"` each appear several times across the new tests. `RECORD`, `TURNS`, `STEPS` and `SUMMARY` were hoisted, but these were not.
   - Why it matters: the rule says reused strings become consts, and the module already has a const block for test keys.
   - Fix: add `REASON`, `COMMITTED`, `END`, `KIND`, `TURN`, `LOAD_MS` and `V` next to `RECORD`, and use them.

### Assessment

**Spec compliance:** ❌ (reset can recreate a deleted session's log)
**Task quality:** Needs fixes
**Reasoning:** The turn wiring meets the spec and the controller's rulings. Ordering, `committed`, the aborted record, `load_ms`, `get_trace` errors and restart numbering are all correct and tested. The one real defect is `reset` taking its trace handle after the session reset, which lets a concurrent delete leave an orphan log. The fix is one line, using the same ordering `start_turn` already uses.
