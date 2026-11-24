# Task 4 report: loop wiring (scope per call, notes block in every prompt, retry hint)

Status: DONE. Commit `2b03f590` on `feat/longer-tasks` (not pushed).

## What was implemented

- **`TurnConfig.scope: ToolScope`.** Every call's `execute` now receives `&config.scope`; the
  `&ToolScope::default()` placeholder is gone. The existing `TurnConfig` literals (the
  tool_loop.rs fixture, server.rs `turn_config` and the server.rs test) pass
  `ToolScope::default()`. Task 5 fills server's real scope.
- **The notes block is read before every step.** `ToolLoop::notes_block(&config.scope)`
  reads `session?.notes()?` fresh and turns it into `NotesBlock::text`. Its `as_deref()` goes
  to `Compactor::prepare`.
- **`Compactor::prepare(.., max_tokens, notes: Option<&str>, link)`.** I split it in two:
  - `prepare` calls a private `compact` (the old body), then applies
    `NotesBlock::attach(&out.messages, notes)` to the returned messages. That covers every
    path: off, under budget, measure failure, trim, summarize, reuse, on-top trim and fallback.
  - Inside `compact`, the step measure is a new `NotedMeasure` wrapper over `LinkMeasure` that
    attaches the block before measuring. Every step measure therefore includes it: the first
    measure, every trim re-measure, the reused and summarized prompts.
  - Summary calls keep the plain `LinkMeasure`, so they never see the block.
  - All index logic (Trim, Summarize, `covers`, `system_len`) still runs on the un-noted
    messages. The scratch and committed transcripts never contain the block.
- **`NotesBlock::attach(messages, block)`** (notes.rs, associated fn):
  - With a leading system message, the block goes after its content and `"\n\n"` (named
    const `SEPARATOR`). An empty system content becomes just the block.
  - With no system message, a system message holding only the block is inserted first.
  - With `None`, the messages are returned unchanged.
  - Because the summary note is already inside the system content, the block lands after it.
- **`RepeatGuard { last_failed: Option<(String, Value)> }`** (tool_loop.rs, private, fresh
  per `run`):
  - `observe(name, args, ok)` returns true when the call failed and the remembered call has
    the same name and equal `serde_json::Value` arguments. It remembers the call on failure
    and forgets it otherwise.
  - On true, `RepeatGuard::hint` inserts `"hint": "This call already failed twice; try
    something different."` (named consts) into the error object. That one `result` value
    goes to `ToolTiming.bytes`, the `tool_result` note and the transcript.
- **`ToolTiming.repeat_failure: bool`** has
  `#[serde(default, skip_serializing_if = "std::ops::Not::not")]`. The existing literal in
  the trace.rs test is updated.
- **Module docs.** tool_loop.rs (module doc and `run` doc) and compaction.rs (module doc and
  `prepare` doc) are updated.

## Tests

New tests:
- tool_loop.rs:
  - `scope_reaches_the_tool_context`
  - `notes_block_is_sent_on_every_step_and_never_committed`: uses the real `NoteAddTool`
    over a fake `SessionHandle`, and a system-prompt transcript.
  - `second_identical_failure_carries_hint`: hint pattern [no, yes, no, no] across two calls
    in one step plus later steps. A different call in between resets the guard. The committed
    tool results equal the notified ones.
  - `third_identical_failure_carries_hint_again`
  - `success_between_resets_guard`: also checks that a fresh run remembers nothing.
  - `repeat_failure_is_traced`: the flag is set only on the hinted call, there is no JSON key
    when it is false, and `bytes` counts the hinted result.
  - `repeat_guard_compares_name_and_arguments`: unit test.
- compaction.rs:
  - `notes_block_follows_summary_note`: also checks that the last step measure equals the
    sent prompt and `after_tokens`, and that the summary calls and their measures never carry
    `[Your notes]`.
  - `notes_block_is_measured_under_trim`
  - `notes_block_without_system_prompt_keeps_summary_reuse` (Review Focus 3):
    - Turn 1 saves `covers == 4`, a stored-transcript index. The sent system message is
      summary then notes.
    - Turn 2 is a new Compactor with the saved state. It reuses the summary with no generate
      call, `summarized_messages == 4`, and `messages[1..] == transcript[4..]`.
  - `off_strategy_still_carries_notes_block`: covers no system message, with a system
    message, and `None` leaves the messages unchanged.
  - `under_budget_and_failed_measure_still_carry_notes_block` (extra).
- notes.rs:
  - `block_attaches_to_the_system_message_or_comes_first` (extra).

### TDD evidence

RED: `scripts/hive-test.sh -- runtime::`, with the tests added and before the implementation:
```
error[E0061]: this method takes 6 arguments but 7 arguments were supplied   (x2)
error[E0433]: cannot find type `NotesBlock` in this scope                   (x3)
error[E0433]: cannot find type `RepeatGuard` in this scope
error[E0560]: struct `tool_loop::TurnConfig` has no field named `scope`     (x3)
error[E0560]: struct `trace::ToolTiming` has no field named `repeat_failure` (x2)
error[E0609]: no field `repeat_failure` on type `trace::ToolTiming`         (x2)
error[E0609]: no field `scope` on type `tool_loop::TurnConfig`              (x2)
error: could not compile `backend` (lib test) due to 15 previous errors
TEST_EXIT=101
```
(The script takes one test filter only, so I used `runtime::` instead of the brief's two
filters.)

GREEN:
- `scripts/hive-test.sh -- runtime::`: `281 passed; 0 failed`, TEST_EXIT=0.
- `scripts/hive-test.sh -- --tests`: lib `540 passed; 0 failed`, plus the 7/10/6/2/5
  suites all ok, TEST_EXIT=0. No warnings.
- `scripts/hive-test.sh --features llamacpp -- --tests`: lib `548 passed; 0 failed`, all
  suites ok, TEST_EXIT=0. All 13 new tests are listed as ok.
- `scripts/hive-test.sh --restore`: RESTORED.
- rustfmt: `rustfmt --edition 2021 --check` on the hive over compaction.rs, tool_loop.rs,
  notes.rs, trace.rs and server.rs gives FMT_OK, after fixing one import-order diff.

## Files changed
- backend/src/runtime/tool_loop.rs
- backend/src/runtime/compaction.rs
- backend/src/runtime/notes.rs (`NotesBlock::attach`, `SEPARATOR`)
- backend/src/runtime/trace.rs
- backend/src/runtime/server.rs (`scope: ToolScope::default()` in two literals, plus import)

## Self-review
- There are no free functions. The helpers are associated fns (`ToolLoop::notes_block`,
  `RepeatGuard::hint`, `NotesBlock::attach`) and the test helpers live on `Nx`/`Retry`
  structs. All new non-test strings are named consts.
- `prepare` and `compact` take 8 parameters, so they carry
  `#[allow(clippy::too_many_arguments)]` like `Summarize::compact` does. The brief fixed the
  signature (`notes` before `link`).
- The block is applied as a final transform and inside the measure wrapper only, so no
  strategy index can shift. Trim never touches index 0 when it is the system message, and the
  summarize prompt always starts with the note's system message. So the block always lands
  on the system message the strategies left in place.
- `NotedMeasure` clones the messages once more per measure. That is negligible next to
  `LinkMeasure`'s own clone.
- A call with a parse error is compared with its raw `arguments` Value, as the guard rules
  say.

## Concerns
- None blocking.
- `SessionHandle::notes()` is called once per step on the loop task. Task 5's
  implementation must take the store lock only briefly, which the constraints already
  require.
- The brief's command `scripts/hive-test.sh -- runtime::tool_loop runtime::compaction` does
  not work: cargo accepts only one filter.
