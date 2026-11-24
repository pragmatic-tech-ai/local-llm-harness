# Final review: feat/context-compaction (1d30774b..0c7a53fb, 12 commits)

Reviewed against `docs/plans/2026-11-24-context-compaction.md` and its spec
`docs/specs/2026-11-24-context-compaction-design.md`, with the ledger rulings in `progress.md`.
The 240 KB package was read in passes: agents, then rpc/worker, then compaction.rs (read in
full from the tree at HEAD), then loop/server/trace, then app. Cargo was not run (hive only).
`cd app && npx vitest run` gives 17 files and 202 tests, all passing.

## Strengths

- **One render path.** `generate` and `measure_prompt` share `Connection::render_and_count`
  (rpc.rs). `PromptParams` is flattened into `GenerateParams`, so the two param shapes cannot
  drift. The equality test runs four tools×thinking cases with char-count tokens and proves
  that each input reaches the render.
- **Only the model's copy changes.** `ToolLoop::run` sends `compacted.messages` but keeps
  `scratch`, `calls_in(&scratch)` and the committed transcript intact. The tests assert the
  stored transcript is whole after trim and after summarize.
- **Trim works on whole units.** It works over the original transcript (`TrimPlan`), so calls and
  results stay paired: stubs keep `tool_call_id`, and drops remove whole user-bounded turns. The
  system message, the current turn and summary notes are fenced off by `touchable`. Review
  Focus 2 (the never-grow stub check) compares serialized lengths, the same measure the stub's
  `{bytes}` uses (`ToolTiming::result_bytes`, the one home).
- **Summary persistence is sound.**
  - The save goes through a store-only lock (`SessionCompactionSink`). A session deleted
    between the check and the write is handled.
  - `reset` clears the summary under the busy flag that also excludes turns. The transcript only
    ever grows (`commit`/`reset` are the only writers), so a saved `covers` stays a valid turn
    boundary.
  - `KeepingSink` keeps the newest state for later steps, even when a later measure fails.
- **The summary call stays hidden.** Its notes are consumed inside `Summarizer::read` and never
  reach the `LoopSink`. A server test asserts there is exactly one `turn_started` per step.
- **Wire match.**
  - Backend `AgentTemplate` (always serialized) matches TS `AgentTemplate` (optional, defaulted
    on read).
  - `CompactionRecord` matches TS `CompactionRecord` field for field, with the same nullability.
  - `StepRecord.compaction` (`skip_serializing_if`) matches TS `compaction?: … | null`.
  - The four VM formats match the spec copy, U+2009 thousands included.
- **Budget arithmetic matches `ContextFill::check`.** `-1` and `0` both reserve 0. The budget is
  floored, and `fits` is `<=`. The `max_tokens_reservation_pushes_a_fitting_prompt_over_budget`
  test covers the reservation.
- **Review Focus 1, 3, 4 and 5 each have a dedicated test** at the level the plan named
  (compaction.rs, tool_loop.rs, server.rs).

## Issues

### Critical

None.

### Important

**I1. The summarize prompt breaks user/assistant alternation, so summarize silently does nothing on Gemma 3 models.**
- File: `backend/src/runtime/compaction.rs:462-467` (`SummarizeStep::prompt`). It is reached
  from `:343` (the reuse measure) and `:482` (`on_top`). Both use `?`, and `prepare` swallows the
  error at `:1001-1006`.
- What: the prompt is `[system, note(user), messages[covers..]]`, and `messages[covers]` is
  always a user message (covers is a turn start). That gives two consecutive user messages. The
  GGUF chat templates are rendered as they ship (`engine/template.rs` registers
  `raise_exception`), and Gemma 3's official template raises "Conversation roles must
  alternate". The candle runtime loads only `gemma3` (`supervisor/discovery.rs:17`), and
  `gemma-3-12b-it` is the 12B the spec times.
- What happens on Gemma 3:
  1. The summary call itself succeeds, and the summary is saved.
  2. The measure of the summarised prompt then fails.
  3. `prepare` logs one stderr line, sends the step uncompacted, and puts no trace record on it.
  4. Every later step fails the same way in its reuse measure.
  5. The turn ends `window_full`, which is exactly what the feature exists to prevent.
  It is also invisible in the trace. The Gemma 4 fixture template doesn't enforce alternation,
  so no test can catch this.
- Fix: keep alternation. Either append the note to the system message (creating one when the
  session has none), or prefix it onto the first kept user message's content (header, summary,
  a blank line, then the original text). The second keeps `SummaryNote::is_note` working
  unchanged. Either way this departs from the spec's "one user message", so it needs an owner
  ruling. Separately, a render failure of a *compacted* prompt is not "measure unavailable":
  - In `Summarize::compact`, route the `on_top` and reuse measure errors to
    `step.fallback(...)` with `summary_error` set.
  - Add a test that uses a template raising on non-alternating roles.

**I2. Summary calls are sized with the agent step's tools and thinking flag, not the call's own.**
- File: `backend/src/runtime/compaction.rs:379` (`Summarizer::new(link, measure, …)`). The
  wrong measure is then used at `:732-738` (`Summarizer::tokens`) and `:390` (the note
  measure).
- What: `measure` is the step's `LinkMeasure`, carrying the step's `tools` and
  `enable_thinking` (`:943-948`). But the summary call sends `tools: []` and
  `enable_thinking: false` (`:756-761`). This breaks the render-agreement contract the branch is
  built on. Every planning measure (`whole`, `overhead`, the per-line `cost`) includes the
  tool-declaration block and the thinking switch.
- Effect:
  - `input` is understated by the size of the tool declarations, so spans chunk earlier than
    needed.
  - With a large tool set in an 8K window, it can reach the spurious `ERR_TOO_MANY` or
    `ERR_LINE_TOO_BIG` fallback.
  - `CompactionState.tokens` records "note + tool declarations + template overhead", not the
    note.
- Fix: pass `Summarize::compact` a second `PromptMeasure` for the summary call, a `LinkMeasure`
  with `tools: &[]` and `enable_thinking: false`. Use it in `Summarizer` and for the note
  measure. Then record `tokens` as the note measure minus an empty-prompt measure, or document
  what it is. In tests, `FakeMeasure` can serve both.

**I3. Span lines feed raw thinking channels and control-token text into the summary call's user message.**
- File: `backend/src/runtime/compaction.rs:547-567` (`SpanLine::render`, the `_ =>
  message.content.clone()` arm).
- What: in a thinking session, assistant content in the transcript keeps the
  `<|channel>thought\n…<channel|>` block. `rpc.rs` prefixes the opener so the chat template can
  strip it, and the Gemma 4 template does strip it (`strip_thinking`). `SpanLine` bypasses the
  template, so the reasoning text lands verbatim inside a user message.
- Effect:
  1. It spends the summary input budget on reasoning the model never sees in a normal prompt.
     That is often most of the tokens, since a trace in the spec shows 176 of 193 tokens were
     thinking.
  2. The worker tokenizes the rendered prompt with special-token parsing, so `<|channel>` and
     `<channel|>` become real control tokens in the middle of the user turn. This can derail the
     12B's summary.
- Fix: in `SpanLine::render`, strip leading thought channels from assistant content and keep
  the visible answer text. Put the helper next to `OpenChannel` in `engine/template.rs` so it
  has one home. The stored transcript stays raw, which respects the owner's "keep raw
  mechanics visible" rule, since this changes only model-facing text. Add a test with a
  thinking-prefixed assistant message.

### Minor

**M1. When `max_tokens` reaches the budget, every step drops all history.**
- File: `backend/src/runtime/compaction.rs:956-961`.
- What: when `reservation >= budget` (for example `max_tokens` 6500, `n_ctx` 8192, at 75%), no
  prompt can ever "fit". Every step therefore stubs every result and drops every earlier turn,
  or makes a summary. This happens even when the prompt plus the reservation would fit `n_ctx`
  easily.
- Fix: when `reservation >= budget`, compare against `n_ctx`, so compaction runs only when the
  worker would reject. Or skip compaction and log it. Add a test for each choice.

**M2. The summary call's sampling isn't fully fixed.**
- File: `backend/src/runtime/compaction.rs:631-636`.
- What: `..session.clone()` also inherits the session's `top_k` and `repeat_penalty`. The spec
  says the sampling is fixed whatever the session's sampling is, with only the seed taken from
  the session.
- Fix: build it from `SamplingConfig::default()` and take only `seed` from the session. Add
  `TOP_K` and `REPEAT_PENALTY` consts if the defaults shouldn't be implicit.

**M3. The summarize trace line hides trim-on-top counts.**
- File: `app/src/renderer/src/TraceViewModel.ts:174-181`, with the backend merge at
  `compaction.rs:498-502`.
- What: when trim runs on top of a summary, `elided_results` and `dropped_turns` are recorded,
  but the summarize and reused lines never show them. A trace reader then can't tell that the
  kept tail was also cut.
- Fix: append `· {e} results elided · {d} turns dropped` to both formats when either count is
  non-zero, as a hoisted format. Add a VM test case. This is fixed copy, so confirm it with the
  owner.

**M4. Summary chunk planning costs one worker round trip per span line.**
- File: `backend/src/runtime/compaction.rs:689-704` (`Summarizer::plan`).
- What: once a span exceeds one call, every line costs one `measure_prompt`, each over a fresh
  websocket with a full render and tokenize. A long span means dozens of round trips before
  the first summary token.
- Fix: measure cumulative prefixes with a binary search per chunk end (about log₂ n
  measures per chunk). Alternatively, estimate from one tokenize per line batch and confirm
  each chunk with a single measure.

**M5. A dead pair of fields on `TurnConfig`.**
- File: `backend/src/runtime/tool_loop.rs:89-95`.
- What: `context_strategy` and `compact_at_pct` are documented as "the loop does not read it".
  They are a second copy of the snapshot values, used only by `SessionView::compactor`.
- Fix: drop them from `TurnConfig` and read `rec.snapshot` in `SessionView::compactor`. This is
  a plan-level choice (the plan's Task 5 Interfaces asked for them), so it needs a ruling.

**M6. The compact-at error text duplicates the range constants.**
- File: `app/src/renderer/src/TemplateEditorViewModel.ts:39`.
- What: `CompactAtInvalid` hardcodes "10 to 95" next to `MinCompactAtPct` and `MaxCompactAtPct`.
- Fix: make it a format (`'… from {0} to {1}.'`) filled by `TextFormat.Format` with the two
  consts.

**M7. Test code that is likely not rustfmt-clean.**
- Files and fixes:
  - `backend/src/agents/template.rs:236-238`: the `format!` string holds a literal line break
    mid-literal. Use `"{BASE}compact_at_pct = {bad}\n"`.
  - `backend/src/agents/template_store.rs:601-604`: the long `matches!` arm. Move the
    predicate into a `let`, or a small associated helper on `TempDir`.
  - Then run `cargo fmt --check` on the hive.

**M8. A repeated method literal in worker.rs.**
- File: `backend/src/runtime/worker.rs:240-245`.
- What: `"measure_prompt"` appears both as the method name and inside the error format.
- Fix: add a `const M_MEASURE_PROMPT: &str` on `WsWorkerLink` and use it in both places.

**M9. Stale docs.**
- `backend/src/rpc.rs:1-13`: the module doc lists `send_turn` and `generate` but not
  `measure_prompt`. Add a sentence: same params as `generate`, returns `{prompt_tokens, n_ctx}`,
  no notes, no generation.
- `docs/specs/2026-11-24-context-compaction-design.md:3`: the status still reads "draft,
  awaiting owner approval", but the ledger records the owner's go.
- Same spec, `:122-123`: it defines `covers` as a count from after the system message. The
  code and the Task 4 ruling use a transcript index, with `summarized_messages` as the count.
  Update the text to match the ruling.
- `docs/specs/2026-11-23-agent-templates-sessions-design.md:433` lists "compaction settings" as
  future, and `docs/specs/2026-11-24-agent-trace-design.md` (the step record shape, around
  `:120`) lacks the optional `compaction`. Add a one-line forward pointer to the compaction
  spec in each.
- The roadmap step 3 status is left to Task 7 (after the live proof). That is correct, and not
  a finding.

## Declined to judge

- **The trim policy.** Stubbing all old results before dropping any turn, and the step being
  sent as is when trim can't reach the budget, are spec and ledger rulings (Task 3), so I
  didn't challenge them.
- **What the client sees of the compacted prompt.** `turn_started.prompt` relayed to the client
  shows the compacted prompt, including the summary text. That is the worker's existing note,
  not the hidden summary call. The spec says nothing about it, and it serves the trace's
  transparency goal.
- **Summary time outside the step's timing.** The summary call's time is not in the step's
  `ttft_ms` or `gen_ms`, only in `summary_ms` and the turn total. The spec defines
  `summary_ms` as the place for it.
- **A summary exactly `max_tokens` long.** One that ended naturally at exactly `max_tokens` is
  treated as truncated, because the ledger's Task 4 ruling detects truncation by token count.
- **A client disconnect during a summary call.** It does not abort the 10 to 30 s call. The
  step is then reported `Aborted` at its next notify, as for any step. This is pre-existing
  loop behaviour.
- **Concurrent sessions on one in-process engine.** A blocked tokenize, for example, would be
  pre-existing behaviour of `generate`. It is not introduced here.

## Assessment

**Ready to merge: With fixes.**

**Reasoning:**
- The seams are solid: one render path, transcripts never altered, the summary call hidden,
  the wire matching field for field, and persistence, reset and delete all correct. App tests
  pass.
- I1 makes summarize silently inert on every Gemma 3 model. That covers the whole candle
  runtime and the 12B the spec targets. I2 and I3 skew the summary call's sizing and input.
- All three are small, local fixes in compaction.rs. I1 needs an owner ruling on the note's
  shape.

## Re-review of fix wave (0c7a53fb..540ccb12, plus 2495fd91 spec status)

Scoped re-review against the controller's rulings in `progress.md`. Cargo not run (hive only);
the fixer's hive results were lib 477, llamacpp 485, app 203.

### Findings

- **I1: fixed.** `SummaryNote::system` appends `\n\n{text}` to the system message, makes the
  note the whole system message when the session has none or its content is empty, and keeps
  the system message's other fields. `SummarizeStep::prompt` always gives exactly one system
  message followed by `messages[covers..]`, so the turns after it keep the transcript's
  alternation.
  - **Detection and protection.** `is_note` now matches only `Role::System`. `touchable`
    already fences index 0 when it is a system message, and in the compacted prompt index 0 is
    always the note's system message, so trim can never stub or drop the note. The `is_note`
    clause is a harmless belt. The trim-on-top `current = 1 + (current - covers)` is correct
    with or without a session system message, and the old `system_len + 1` would have been
    off by one here.
  - **Re-summary.** The old text comes from `saved.summary` (`CompactionState`), never from
    a system message. `prompt` always rebuilds from the committed transcript's own
    `messages[0]`, which never carries a note, because `ToolLoop` keeps `scratch` and the
    transcript unmodified. So the old note can't be duplicated or nested, and the
    `resummary_folds_old_summary_and_new_messages` expectation (one system message with only
    the new summary) proves it.
  - **Fallback.** A failed reuse measure, a failed `on_top` measure (including the measure
    inside trim on top) and the empty-span `on_top` all go to `step.fallback` with
    `summary_error = "measuring the summarized prompt failed: …"`. The test covers both the
    fresh and the reuse case.
  - **Rendering.** The test renders through Gemma 4 and `STRICT_ALTERNATION`, with and without
    a system message, and it asserts the old shape is rejected. `STRICT_ALTERNATION` mirrors
    Gemma 3's alternation check: fold the system message into the first user turn, then
    `raise_exception`.
- **I2: fixed.** `summary_measure` is a `LinkMeasure` with `tools: &[]` and
  `enable_thinking: false`, which matches what `Summarizer::call` sends. It is used for
  planning and for `note_tokens`, and `note_tokens` subtracts the empty-call measure. The
  step's `measure` is used only for the step's prompts.
- **I3: fixed.** `OpenChannel::strip_blocks` removes closed blocks and an unclosed trailing
  opener, and only `SpanLine`'s non-tool content uses it. Tool results are left alone, and the
  test asserts the transcript stays raw. `GemmaThinkCounter` reuses `OpenChannel::CLOSE`, so
  the marker has one home.
- **M1: fixed.** `Compactor::target` returns `n_ctx` when `reservation >= budget`. With
  `fits` as `prompt + reservation <= target`, that is exactly the `n_ctx − reservation`
  window. `None` when `reservation >= n_ctx` is a sensible extension the ruling didn't cover,
  and it is documented. The tests cover the boundary values and four loop rows.
- **M2: fixed.** The struct literal is now exhaustive (it compiles without `..`): 0.2, 0.9,
  `TOP_K` 40, `REPEAT_PENALTY` 1.0, the session's seed and the summary `max_tokens`.
- **M3: fixed.** `TrimOnTopFormat` is `' · {0} results elided · {1} turns dropped'`, as the
  ruling words it. It is appended to both the summarize and the reused line only when either
  count is above 0, and a VM test covers it.
- **M4: fixed, and the search is correct.**
  - The invariant holds: `fit` is a known-fitting end, starting at `start` (empty), and
    `over` is a known-failing end. For the first chunk that is `len`, because the whole span
    was just measured over budget with the same predicate, `content(old, …) <= window`. For
    later chunks it is the sentinel `len + 1`.
  - `mid` always lies in `(fit, over)`, so `mid <= len` and the slice is always in range. The
    loop ends with `fit` as the longest fitting prefix, assuming measured size is monotone in
    prefix length, which holds for a tokenized join.
  - `fit == start` means a single line can't fit, giving `ERR_LINE_TOO_BIG`.
  - The `MAX_CALLS` check runs at the top of the loop, so it fires before any call once four
    chunks leave lines over.
  - A later chunk reserves `max_tokens + SUMMARY_BREAK_TOKENS`, the same room as before.
  - The 10-measure count in the test checks out: 1 + ⌈log₂40⌉ + ⌈log₂8⌉.
- **M5: fixed.** Both fields are gone from `TurnConfig`. `SessionView::compactor` reads
  `rec.snapshot`, and its `config` parameter is still used for `sampling`. The tool_loop test
  imports are still used.
- **M6: fixed.** `CompactAtInvalidFormat` is filled from `MinCompactAtPct`/`MaxCompactAtPct`,
  and the test asserts the full text.
- **M7: fixed.** The `format!` literal now uses `\n`, and the `matches!` arm is now a `bad`
  closure.
- **M8: fixed.** `WsWorkerLink::M_MEASURE_PROMPT` is used in both places.
- **M9: fixed.** The `rpc.rs` module doc now covers `measure_prompt`. The spec status line
  was changed in 2495fd91, `covers` is defined as an index, and both forward pointers were
  added.

### Fixer concerns

- **No RED run: accepted.** The new tests demonstrably fail on the old code: the old-shape
  rejection, `(7, 1.3)` → `(40, 1.0)`, the 10-measure count, the note-in-system expectations
  and the `target` rows. That is adequate given the hive-only cargo.
- **The saved summary is kept on a rejected summarized prompt: behaviour correct, doc gap.**
  The summary is valid and covers committed messages. A later step's reuse measure fails and
  falls back at once, with no new summary call, so the cost is one extra measure per step and
  never repeated 10 to 30 s summary calls. However, the spec's Fallback bullet still says
  "The saved summary is left as it was", and a reader applies that to the new
  measure-failure fallback too (see N1).
- **Gemma 3 rejects `tool` messages: agreed, pre-existing and out of scope.** On such a
  prompt the original measure already fails, so `prepare` logs it and sends the step
  unchanged, as before this branch.

### New issues in the fix diff

- **N1 (Minor, doc).** `docs/specs/2026-11-24-context-compaction-design.md`, the Summarize
  "Fallback" bullet, says "The saved summary is left as it was", while the new "What the
  model sees" text adds a fallback after the summary was already saved. Say that when
  measuring the summarised prompt fails, the fresh summary stays saved, and later steps that
  reuse it fall back the same way without a new summary call.

No regressions found in the touched code. No out-of-scope issues noted.

**Verdict: Needs fixes.** One doc-only Minor (N1). All of I1–I3 and M1–M9 are fixed
correctly under the rulings.
