# Final fix wave: feat/agent-trace (I1, M1 to M5)

Status: DONE_WITH_CONCERNS (the concern is TDD evidence for the Rust part; see the end of this report).

## Commits

- `9478d56e` fix: reserve trace turn numbers, keep failed load time, read traces off the async workers (I1, M2, M3, M4, M5)
- `4be5f662` fix: a busy session's unfinished trace turn shows as running after attach (M1)

M6 and M7 were left to the controller. The one spec edit is the "Turn numbers" settled detail
(around line 163), which now describes the I1 ruling. The status line (M6) is not touched.

## What changed

**I1, turn numbers reserved at allocation** (backend/src/runtime/trace.rs, server.rs)
- `TraceLog::next_turn()` now reserves the number it hands out. On the first call it scans the
  log (highest `turn` + 1). Each later call returns the cached value and advances it, all under
  the `next` lock. `append` still bumps the counter with `max`. A read error is returned and
  nothing is cached.
- `SessionTrace::new` (sync) is replaced by
  `pub async fn start(log: Arc<TraceLog>, session: String, user: &str) -> io::Result<SessionTrace>`.
  It starts the clock, then allocates the number in `tokio::task::spawn_blocking`.
- `FALLBACK_TURN` and the deferred `warning` field are gone.
- `run_turn` sends both failures through one path: a missing log handle, or a log that can't be
  read at allocation. Either way it sends exactly one `trace_warning` and runs the turn with
  `NoTrace`.

**M2, failed load keeps its load time** (tool_loop.rs, server.rs, test_support.rs)
- New `pub struct LinkFailure { pub message: String, pub load: Option<Duration> }`, with
  `From<String>` (load `None`).
- `TurnLinks::link` now returns `Result<StepLink, LinkFailure>`.
- `SessionLinks::link` step 0 maps the dispatch error to `load: loaded.then(|| started.elapsed())`.
- The loop sets `meter.load` from the failure before it builds the error record.

**M3, blocking reads** (server.rs, trace.rs)
- `get_trace` is now async and runs `log.page(..)` in `spawn_blocking`.
- The first turn-number scan runs in `spawn_blocking`, through `SessionTrace::start`.
- Appends are unchanged.

**M4:** the public `TraceLog::delete` is removed (nothing used it). `remove_file` is folded into
the private `retire`.

**M5** (backend/src/engine/tool_format.rs)
- `MODEL_TURN` is now `"<|turn>model\n"` and is `pub(crate)`. `EMPTY_THOUGHT_TAIL` is now
  `"<|channel>thought\n<channel|>"`.
- All the consts now come before `ends_with_generation_prompt`.
- The test-only `CounterFeed::MODEL_TURN` is removed, and the tests use
  `GemmaToolFormat::MODEL_TURN`.
- The `open_close` literal in the test is escaped as well.

**M1, busy at attach** (app/src/renderer/src/TraceViewModel.ts, SessionViewModel.ts)
- The signature is now `ShowSession(client, id, busy = false)`, and the VM subscribes to
  `OnSessionBusy` for the shown id.
- If the session is still busy when the first page lands, the newest row is marked live
  (`running…`), provided it has no summary.
- When `session_busy` goes false, every live row without a summary gets `EndLive()` and shows
  `incomplete`.
- New `TraceTurnViewModel.HasSummary` and `EndLive()`.
- `SessionViewModel.Attach` passes `attached.summary.busy`.

## Tests added or changed

Rust:
- **`server::unwritable_log_numbers_each_turn_and_both_commit`** (replaces
  `unwritable_log_warns_and_turn_still_commits`). The log is an empty read-only file and the run
  covers two turns. It asserts that:
  - both turns warn;
  - both commit (transcript length 4);
  - the turns are numbered 1 and 2;
  - each step's turn matches its summary;
  - the file is still empty.
- **`server::unreadable_log_runs_the_turn_untraced_with_one_warning`:** the log path is a
  directory. It asserts exactly one `trace_warning`, no `trace_step` or `trace_turn`, a
  `loop_done`, and a commit.
- **`server::failed_first_message_load_keeps_its_load_ms`:** the host fails `ensure_ready`. The
  step-0 record is `error`, carries the load error, and has a u64 `load_ms`. The turn ends
  `no_harness`.
- **`trace::next_turn_reserves_each_number_without_a_write`:** the numbers are 5, 6 and 7 with no
  writes in between. A later record at turn 9 moves the counter to 10.
- **`trace::session_trace_over_an_unreadable_log_does_not_start`** replaces the turn-0 test.
- **Adjusted:** the `SessionLinks` tests now compare `LinkFailure` values, and the trace unit
  tests use `start(..).await`.

App:
- **`TraceViewModel.test.ts`, 4 new tests:**
  - `busy_session_shows_its_newest_unfinished_turn_as_running`
  - `busy_ending_after_the_summary_keeps_the_summary`
  - `busy_session_with_a_finished_newest_turn_marks_nothing_running`
  - `session_busy_notes_set_and_clear_the_running_turn`, which also covers busy arriving while
    the page is in flight, another session's note, and busy ending before the page lands
- **`SessionViewModel.test.ts`:** new `attach_to_a_busy_session_shows_its_unfinished_turn_running`.

## Evidence

- **RED (app):** I stashed the VM changes and ran `npx vitest run` on TraceViewModel.test.ts and
  SessionViewModel.test.ts. 3 failed (`busy_session_shows_...`, `session_busy_notes_...`,
  `attach_to_a_busy_session_...`) and 29 passed. The other two new tests are guards that already
  hold on the old code.
- **First hive run:** this was effectively a RED for the API change. It failed to compile with 3
  E0308 errors in the existing `SessionLinks` tests that compared `String`. I fixed those
  assertions.
- **`scripts/hive-test.sh -- --tests`:** lib 418 passed, plus integration 7, 10, 6, 2 and 5
  passed. 0 failed, no warnings, TEST_EXIT=0.
- **`scripts/hive-test.sh --features llamacpp`:** 426 passed, 0 failed, TEST_EXIT=0.
- **`scripts/hive-test.sh --restore`:** RESTORED.
- **`cd app && npm test`:** 17 files, 197 tests passed. `npm run typecheck:web` and
  `npm run typecheck:node` are both clean.
- **After the hive runs:** the only change was rewrapping one doc comment in server.rs
  (`run_turn`). No code changed.
- **Format and line endings:** I ran `rustfmt --edition 2021` on the changed files. All the
  changed files are LF.

## Concerns

- **No separate RED run for the Rust fixes.** I wrote the Rust fixes and their tests together,
  then ran them on the hive once, to save a hive cycle. Under the old code:
  - `next_turn_reserves_each_number_without_a_write` would fail, because the old `next_turn`
    returned 5 three times;
  - `unwritable_log_numbers_each_turn_and_both_commit` would fail, with both turns numbered 1;
  - `failed_first_message_load_keeps_its_load_ms` would fail, with `load_ms` null.

  This is reasoned, not observed.
- **The read-only-file test depends on the OS.** The hive runs on Windows, where the read-only
  attribute stops an append even for an admin. On a Unix CI running as root, the append would
  succeed and the test would fail. At the end the test resets the permission, with
  `#[allow(clippy::permissions_set_readonly_false)]`.
- **M1 can mislabel an old turn.** If a busy session's running turn has not written its first
  step yet, and the newest paged turn is an older crash-cut turn with no summary, that older
  turn shows `running…` until busy ends. This follows the ruling, "newest summary-less turn
  counts as live".
