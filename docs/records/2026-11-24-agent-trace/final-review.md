# Final whole-branch review: feat/agent-trace (49b4d340..fb35dd4e, 13 commits)

Reviewed against `docs/plans/2026-11-24-agent-trace.md` and its binding spec
`docs/specs/2026-11-24-agent-trace-design.md`, with the ledger's rulings. I read the head
sources of every changed file (trace.rs, tool_loop.rs, server.rs, tool_format.rs, http.rs,
main.rs, test_support.rs, the agents.ts, RuntimeClient, TraceViewModel, SessionViewModel,
InstrumentPanelViewModel and TracePanel.mu changes) in passes, plus the worker relay in `rpc.rs`
and the candle and llamacpp chunk emission, to check the seams. I ran no cargo. `cd app && npx
vitest run` passed: 17 files, 192 tests.

## Strengths

- **The wire matches the app types field by field.** The live `trace_step`/`trace_turn` records
  and the `get_trace` records both go through `TraceRecord::line_value`, so they carry the same
  `v`/`kind` envelope (trace.rs:107-113, 137-157). A test checks that they are byte-equal
  (server.rs `live_trace_step_is_byte_equal_to_the_paged_step`). `StepRecord`, `TurnRecord`,
  `TraceTurn`, `TracePage` and the three notes in agents.ts mirror the Rust structs exactly,
  including which fields can be null.
- **Ordering is correct and pinned by tests.**
  - On the success path: `trace_step` before `loop_done`, then the commit, then `trace_turn`,
    then `sessions_changed`, then the `send_turn` result
    (`turn_streams_trace_step_and_trace_turn_before_result`; the `committed_turn...` test now
    expects `trace_turn` before `sessions_changed`).
  - The app assumes no order. It subscribes before `GetTrace`, folds records by
    `(turn, step)`, and a live record replaces a paged one.
- **Measurement is sound.**
  - `ttft_ms` starts just before `generate`, and `gen_ms` runs from the first chunk to the note
    that ends generation, or to the break.
  - `load_ms` is set only when the dispatcher's on-loading callback fired on step 0
    (server.rs:179-199). A test pins it as present on turn 1 and null on turn 2.
  - Out tokens are `tokens.len()`. I checked that the worker relays the engine's `tokens` as is
    (rpc.rs:311, 383), and that candle (candle.rs:290) and llamacpp (llamacpp.rs:286) push one
    `TokenInfo` per generated token, with an empty stop chunk.
  - The budget cut gives `Budget` with `tools: []`, and a closed sink gives one `Aborted` step.
- **Concurrency around delete and reset is carefully handled.**
  - `start_turn` and `reset` take the trace handle before they read the record.
  - `TraceStore::delete` retires every outstanding handle, so a delete racing a turn never
    recreates the log.
  - `forget_trace_if_gone` cleans the cache after a lost race.
  - All of this sits under the existing busy guard, and no trace I/O holds the session-store
    lock or the hub lock.
- **The log format is robust.** A cut tail is terminated before the next append. Partial and
  unknown lines are skipped. A read error is returned and not cached (fix round 1 of Task 1).
  Ids are validated as single file-name components. `session_store_ignores_trace_files` pins the
  loader.
- **The app VM is well factored.** It is split into Step, Turn and Trace VMs over `Observable`,
  with a token guard against stale page replies, warnings that combine their sources, expansion
  that keeps a user's toggle, and Review Focus 3 and 4 tests that match the plan's names.
- **The docs are current.** The server module docs list the protocol. The agent-templates spec
  line "instrument data is not persisted" is rewritten correctly
  (2026-11-23-agent-templates-sessions-design.md:369-372).

## Issues

### Critical

None.

### Important

**I1. A turn number is never reserved. When the log can't be written, every turn reuses one
number, and the app merges them into one row.**

- **Where:** backend/src/runtime/trace.rs:226-233 (`append` bumps the cached counter only after
  a successful write), trace.rs:255-267 (`next_turn` returns the cached value without reserving
  it), trace.rs:498-502 (an unreadable log falls back to turn 0). On the app side:
  app/src/renderer/src/TraceViewModel.ts:571-583 and 308-328.
- **What happens, case 1, a read-only file:** this is exactly Review Focus 1. The first scan
  caches N. Every append then fails, so the cache never moves past N, and every later turn is
  also traced as turn N. The records are still sent live, as the spec requires, so the app keys
  them all to the row for turn N. Each live step replaces the step with the same number, and each
  summary replaces the last. A 1-step turn after a 3-step turn shows the new step 1 next to the
  old steps 2 and 3, under a "1 steps" header.
- **What happens, case 2, the log is a directory or can't be read:** every turn is turn 0, with
  the same merging. If a later append succeeds, the log gains turn-0 records that sort below
  turn 1 forever. The Task 3 ruling accepted turn-0 rows but did not consider this merging.
- **Why it matters:** the spec says tracing never fails a turn, but in the one degraded case it
  names, the live panel shows wrong data with no sign that it is wrong.
- **Fix:**
  1. Make allocation reserve the number. For example, add `TraceLog::allocate_turn() ->
     io::Result<u64>`, which returns n and sets the cache to `Some(n + 1)` under the `next` lock.
     `append` keeps its `max` bump, which still covers turns written by others.
  2. For an unreadable log, take the `TraceStore::log` Err route: run the turn with `NoTrace`
     plus one `trace_warning`, and stop writing turn-0 records. Or keep a per-log in-memory
     counter that continues from the last allocated number.
  3. Add a server test that makes the log file read-only, runs two turns, and asserts that the
     two `trace_turn` records have different `turn` values.

### Minor

**M1. A turn that is still running after a re-attach shows as "incomplete".**

- **Where:** TraceViewModel.ts:423-500 and SessionViewModel.ts:236.
- **What happens:** a window that attaches, or re-attaches after a reconnect, while the session
  is mid-turn pages that turn with no summary. It is marked `incomplete` until the next
  `trace_step` arrives, which can take a full generation and its tools.
  `AttachedSession.summary.busy` (agents.ts:84) already says the session is running.
- **Fix:** pass `attached.summary.busy` into `ShowSession`, or subscribe to `OnSessionBusy`.
  While busy, `MarkLive` the newest paged turn that has no summary. When `session_busy` turns
  false without a `trace_turn`, switch it back to incomplete. Add a test next to
  `turn_without_summary_and_not_live_is_incomplete`.

**M2. A failed first-message load loses its load time.**

- **Where:** server.rs:183-194 (`map_err(|e| e.0)?` drops `started.elapsed()`), and
  tool_loop.rs:314-316, where the step-0 error record is built with `load_ms: null`.
- **Why it matters:** a load that times out or fails after tens of seconds is exactly the failed
  step the spec says is most worth studying.
- **Fix:** let `TurnLinks::link` return the load with the error as well, for example
  `Err(LinkFailure { message, load: Option<Duration> })`, and set `meter.load` from it before the
  error record is built. Extend `first_message_load_records_load_ms` with a host that fails its
  load.

**M3. Trace reads block a runtime worker.**

- **Where:** trace.rs:367-377 (`read_records` does a synchronous `fs::read` of the whole log),
  called from `get_trace` (server.rs:882-893) and from `next_turn` on the async request path.
- **Why it matters:** with no retention limit, every page or "Load older" call blocks one tokio
  worker for a time that grows with the log. Reading the whole log is the spec's choice, but
  blocking the runtime is not.
- **Fix:** run the page read inside `tokio::task::spawn_blocking`, which needs the `Arc` that
  `log()` already returns. Or note in the module doc that this is bounded only because there is
  no retention.

**M4. `TraceLog::delete` is public, unused, and unsafe to use.**

- **Where:** trace.rs:305-309.
- **What's wrong:** it removes the file but does not retire the handle, so a later append
  recreates the log. Only `retire`, through `TraceStore::delete`, is used.
- **Fix:** remove it, or make it `retire` and drop the separate private method. If the plan's
  listed signature has to stay, have it delegate to `retire`.

**M5. String constants are split across lines with raw newlines.**

- **Where:** backend/src/engine/tool_format.rs:232-235. `MODEL_TURN` and `EMPTY_THOUGHT_TAIL`
  embed a raw newline in the literal. `ends_with_generation_prompt` sits between two const
  groups (the `QUOTE`, `CALL_PREFIX` and `MAX_DEPTH` consts follow it), and the
  `several_markers_in_one_chunk_apply_in_order` test does the same with `open_close`. The test
  helper `CounterFeed::MODEL_TURN` duplicates the production marker.
- **Fix:** write `"<|turn>model\n"` and `"<|channel>thought\n<channel|>"`, move the fn below all
  the consts, and make the const `pub(crate)` so the test can reuse it.

**M6. The spec's status line is stale.**

- **Where:** docs/specs/2026-11-24-agent-trace-design.md:3 still reads "draft 2026-11-24,
  awaiting owner approval". The owner said "go" on 2026-11-24, as the ledger records.
- **Fix:** change it to "approved 2026-11-24; built on feat/agent-trace; live proof pending
  (Task 7)". Marking roadmap step 2 as shipped is correctly left to Task 7 Step 5.

**M7. The SDD records are not in the repo.**

- **Where:** everything is under `.superpowers/sdd/2026-11-24-agent-trace/`, which `.gitignore`
  line 14 excludes. The previous slice archived its records to
  `docs/records/2026-11-23-agent-templates-sessions/` (commit af5e1b17), and the owner's
  records-in-repo rule asks for the same.
- **Fix:** before merging, copy progress.md, the task briefs, reports and reviews, and this file
  and the final fix report, into `docs/records/2026-11-24-agent-trace/`, and commit them as
  `docs(records): agent trace SDD records`.

## Declined to judge

- **The header shows the raw `loop_done` reason (`step_limit`), while the spec's sketch shows
  `budget`.** The spec's record text says "the loop_done reason, verbatim", so the sketch is the
  inconsistent part.
- **The turn record's `steps` includes a step that never reached the worker,** so for a
  `no_harness` or a link failure it is `loop_done.steps + 1`. This is consistent with the spec's
  rule that such a step is written as a step record.
- **Fill can read over 100% on a rejected step.** That is the worker's real `prompt_tokens`, and
  the bar is clamped, so the data stays honest.
- **Token counts for candle and llamacpp have no unit test** (the spec says "a test per
  engine"). The plan defers this to the Task 7 live proof, and the ledger says the owner was
  told. I checked the emission code by reading it.
- **Expansion state is lost when the list reloads after a reconnect or re-show.** The
  implementer noted it, and the spec is silent.
- **`TracePanel.mu` has never been rendered:** the ToggleButton content, the nested
  ScrollViewer, and the tool rows. This is left to the owner's UI check in Task 7.
- **There is no live reset divider while a session is shown,** because the app has no reset
  command. A reset made from elsewhere shows up on the next page load.
- **The fixed `Height`/`Width` on the step bars** mirrors the existing context-bar pattern.
- **A `trace_warning` is cleared by any successful page, including "Load older".** This was
  ruled in Task 6 round 2, and the cost was accepted.

## Assessment

**Ready to merge?** With fixes.

**Reasoning:** The trace is correct end to end in the normal case, and the seams the brief
named all hold: wire shape, ordering, load measurement, delete and reset races, and paging
edges. I1 is a real bug in the plan's own Review Focus 1 scenario: a log that can't be written
makes the live panel merge distinct turns. Fix it, with its test, before merging. The Minors are
small and local.

## Re-review of fix wave (fb35dd4e..4be5f662)

Scoped to I1 and M1 to M5 under the controller's rulings (progress.md, after "FINAL REVIEW: with
fixes"). I read the diff and the head sources it touches (trace.rs append/next_turn/retire/
tail_is_cut, server.rs start_turn/reset/get_trace/handle, hub.rs busy guard, the ShowSession call
sites). I ran no cargo. The fixer reports lib 418, llamacpp 426 and app 197 passing.

**Verdict: Approved.** All six findings are fixed as ruled, and I found no new breakage.

### Findings

- **I1: fixed.** `next_turn` now sets `next = Some(turn + 1)` under the `next` lock on every call,
  so the counter moves whether or not a write lands. `append` still bumps it with `max`. A read
  error caches nothing and returns `Err`. `SessionTrace::start` passes that `Err` to `run_turn`,
  which sends one `trace_warning` through the same path as a missing handle and runs the turn
  with `NoTrace`. `FALLBACK_TURN` and the deferred `warning` are gone, so there are no more turn-0
  records. The new server tests cover both cases from the review: read-only gives turns 1 and 2;
  a directory gives one warning and no trace records.
- **M1: fixed.** `Attach` passes `attached.summary.busy`. Once the page lands, a busy session
  marks row 0 live if it has no summary. The `OnSessionBusy` subscription is filtered to the shown
  id, and on `busy: false` it calls `EndLive` on every live row that still has no summary. Busy
  ending while the page is still in flight is handled, because `busy` is re-read after the await.
  On the server, `session_busy: false` is sent from the `BusyGuard` drop, which comes after
  `trace_turn`, so a normal turn's summary always lands first.
- **M2: fixed.** `LinkFailure { message, load }` carries `loaded.then(|| started.elapsed())` on the
  step-0 error. The loop sets `meter.load` from it before it builds the error record. The
  `MODEL_CHANGED` and `NO_MODEL` failures carry `None`, which is correct.
- **M3: fixed.** `get_trace` runs `log.page` in `spawn_blocking`, and every allocation does too,
  through `SessionTrace::start`. The only reads left on the async path are the 1-byte
  `tail_is_cut` probe inside `append`, which is part of the write and is acceptable.
- **M4: fixed.** The public `delete` is removed and `remove_file` is folded into the private
  `retire`.
- **M5: fixed.** The literals use `\n`. The consts come before the fn. `MODEL_TURN` is
  `pub(crate)` and the tests reuse it. `open_close` is escaped.

### Concurrency checks

- **Turn-number reservation.** Allocation is atomic under `next`. Turns on one session are also
  serialized by the busy guard, and a reset record has no turn, so nothing else allocates or bumps
  in parallel. In the one interleaving that is possible in principle, the first scan runs while an
  append is between its write and its bump. Either the scan sees the record, or `append`'s `max`
  bump fixes the counter once the scan has set it. Either way the numbers stay unique. `retire`
  takes `write` then `next`, `append` releases `write` before it takes `next`, and `next_turn`
  takes only `next`, so there is no lock-order cycle.
- **`spawn_blocking` and locks.** No std guard is held across an `.await`. The closures move only
  an `Arc<TraceLog>` and plain values. The first scan holds `next` on a blocking thread for the
  whole read. The only async-side code that can wait on `next` at that moment is `retire`, if a
  `delete_session` races a session's first allocation. That wait is rare and bounded, and is not
  worth a change. If the turn future is dropped, the blocking task still finishes and the number
  stays reserved, which the ruling allows.
- **Clock start.** `started` moved to before allocation, so the turn's wall time now includes the
  first scan. That is a fair cost to charge to the turn.

### The fixer's concerns

- **No Rust RED run: accepted.** I checked the reasoning against the old code:
  - the old `next_turn` returned the cached 5 three times;
  - the old read-only run numbered both turns 1;
  - the old step-0 record had `load_ms: null`.

  All three new tests would have failed. The test-only `String` to `LinkFailure` compile errors
  were a real RED for the API change.
- **The read-only test depends on the OS: accepted.** It holds on Windows, which is the hive, and
  on Unix as a non-root user, where the write bits are cleared. It fails only on Unix as root,
  and no such runner exists, since the repo has no CI. If one is ever added, gate the test with
  `#[cfg(windows)]` as builtin.rs and template_store.rs already do.
- **An older crash-cut turn can show `running…` in the M1 edge case: accepted.** It is exactly the
  ruled behaviour, it is transient, and it becomes `incomplete` when busy ends.
- **The edit to the spec's "Turn numbers" detail: accepted, and required.** The I1 ruling replaces
  the Task 3 turn-0 behaviour. The new text matches the code: numbers are reserved at allocation,
  an unreadable log runs the turn untraced with one warning, and a number can be reused after a
  restart only when nothing of that turn was written.

### Out of scope

None found.
