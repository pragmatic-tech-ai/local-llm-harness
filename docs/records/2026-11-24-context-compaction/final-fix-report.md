# Final fix wave: feat/context-compaction

**Status:** DONE_WITH_CONCERNS (concerns at the bottom)

**Commits:**
- `5c0d3720` fix: summary note in the system message, own summary measure, final review fixes
- `540ccb12` fix: trace shows trim on top of a summary; compact-at error from the range consts

## Findings and fixes

- **I1. The note goes in the system message.** File: `backend/src/runtime/compaction.rs`.
  - `SummaryNote` methods:
    - `text(summary)` gives `[Summary of the earlier conversation]\n{summary}`.
    - `system(Option<&ChatMessage>, summary)` appends `\n\n{text}` to the system message. With
      no system message, or an empty one, it gives a system message that holds only the note.
    - `message(summary)` is the no-system case.
    - `is_note` now matches a **system** message that starts with the header or contains
      `\n\n` + header.
  - `SummarizeStep::prompt` is always one system message (with the note), then
    `messages[covers..]`. The trim-on-top current index is `1 + (current - covers)`.
  - Trim still never touches index 0 when it is the system message. That guard now also covers
    the note, and the `is_note` guard stays as a belt.
  - A failed reuse measure, a failed `on_top` measure (including the trim on top) and a failed
    empty-span `on_top` measure now all go to `step.fallback`. They carry `summary_error =
    "measuring the summarized prompt failed: {e}"` (`ERR_PROMPT_MEASURE`).
  - The spec's "What the model sees" text is rewritten.
- **I2. Summary calls get their own measure.**
  - `Summarize::compact` gains `summary_measure`. `Compactor::prepare` builds a second
    `LinkMeasure` for it (`tools: &[]`, `enable_thinking: false`).
  - `Summarizer` uses it for planning and for the saved tokens. `Summarizer::note_tokens` is
    measure(note text) minus measure(""), so `tokens` is the note text's own size. That is
    documented on `CompactionState.tokens` and in the spec.
- **I3. Thinking blocks are stripped from span text.**
  - `OpenChannel::CLOSE` and `OpenChannel::strip_blocks` are in `backend/src/engine/template.rs`.
    `strip_blocks` removes closed blocks and an unclosed trailing opener.
  - `GemmaThinkCounter::CHANNEL_CLOSE` now reuses `OpenChannel::CLOSE`.
  - The non-tool content in `SpanLine::render` is now `strip_blocks(..).trim()`.
- **M1. A reservation at or above the budget targets the window.** `Compactor::target(n_ctx,
  pct, reservation)` returns:
  - the budget when `reservation < budget`;
  - `n_ctx` when `reservation < n_ctx`;
  - otherwise `None`, which leaves the prompt unchanged with no record.
  It is documented in the spec's settled details.
- **M2. Summary sampling is fixed.** temperature 0.2, top_p 0.9, `TOP_K` 40, `REPEAT_PENALTY`
  1.0, the session's seed and the summary max_tokens. Nothing else is inherited. The spec is
  updated.
- **M3. The summarize and reused trace lines show trim on top.** `TraceViewModel.ts` adds
  `TrimOnTopFormat = ' · {0} results elided · {1} turns dropped'` through a private static
  `TrimOnTopText`, and only when either count is > 0. A new VM test covers it, and the existing
  summarize cases set `elided_results: 0`. The spec prose notes the suffix.
- **M4. Chunk planning binary-searches each chunk end.** It measures the assembled call through
  `Summarizer::fits`. The 4-chunk stop runs before each new chunk's search. `JOIN_TOKENS` and
  `cost` are removed.
- **M5. Two fields leave `TurnConfig`.** `context_strategy` and `compact_at_pct` are removed.
  `SessionView::compactor` reads `rec.snapshot`, and the tests are updated.
- **M6.** `CompactAtInvalidFormat` is filled with `MinCompactAtPct`/`MaxCompactAtPct`. The test
  asserts the full text.
- **M7.** The `format!` literal in `agents/template.rs` is fixed. The `matches!` arm in
  `template_store.rs` is now a `bad` closure. `rustfmt --edition 2021 --check` is clean on all
  touched Rust files.
- **M8.** `WsWorkerLink::M_MEASURE_PROMPT` is used for both the method name and the error.
- **M9. Docs.**
  - `rpc.rs` module doc now covers `measure_prompt`.
  - The spec defines `covers` as a transcript index, with `summarized_messages` as the count.
  - Forward pointers are added in the templates-sessions spec (extension points) and the trace
    spec (records notes).
  - The spec status line is left to the controller, as instructed.

## New tests

**compaction.rs:**
- `summary_note_rides_in_the_system_message`
- `trim_never_touches_system_note_or_current_turn` (reworked)
- `rejected_summarized_prompt_falls_back_to_trim_with_reason` (covers the fresh and the reuse
  case)
- `summarized_prompt_renders_through_gemma4_and_strict_alternation`:
  - There was no Gemma 3 fixture, so this adds `STRICT_ALTERNATION`, a minimal template in the
    shape of Gemma 3's: it folds the system message into the first user turn and calls
    `raise_exception` on broken alternation.
  - It renders both with a system message and without one, through Gemma 4 and the strict
    template.
  - It also asserts that the old shape (a separate user note) is rejected.
- `span_lines_drop_thinking_channels_but_the_transcript_keeps_them`
- `chunk_planning_binary_searches_each_chunk_end`: 40 lines give chunks `0..33, 33..40` in
  exactly 10 measures, against 42 per-line.
- `target_is_the_window_when_the_reservation_reaches_the_budget`

**Elsewhere:**
- `template.rs`: `strip_blocks_removes_closed_and_trailing_open_channels`
- `tool_loop.rs`: `reservation_past_the_budget_targets_the_window` (4 rows)
- `TraceViewModel.test.ts`: `compaction_text_shows_trim_on_top_of_a_summary`

## Verification (final tree)

- `scripts/hive-test.sh -- --tests`: lib 477 passed, plus integration 7/10/6/2/5. `TEST_EXIT=0`.
- `scripts/hive-test.sh --features llamacpp`: lib 485 passed. `TEST_EXIT=0`.
- `scripts/hive-test.sh --restore`: RESTORED.
- `cd app && npm test`: 17 files and 203 tests passed. `typecheck:web` and `typecheck:node` are
  clean.
- `rustfmt --edition 2021 --check` on every touched `.rs` file is clean. Line endings are LF,
  checked byte-wise.

## Concerns

- **No separate RED run.** The fixes and tests were written together, then run green on the
  first hive pass. Before the fix, several new tests could not compile or would fail, but I did
  not record a RED run.
- **What a fresh summary leaves behind on fallback.** When a fresh summary is saved and the
  measure of its summarised prompt then fails, the step falls back to trim. The saved summary
  is kept, and a later step tries to reuse it, which falls back the same way if the template
  still rejects it. I judged this correct (the summary itself is valid), but it means repeated
  fallbacks on a template that rejects every summarised prompt.
- **Gemma 3 and tools.** Gemma 3's real template also rejects `tool` role messages at
  alternating positions. That is a pre-existing limit of Gemma 3 with tools, not a compaction
  issue.
- **Reuse-measure failure on a dead link.** It now runs trim, whose measure then fails too, so
  `prepare` logs it and sends the prompt unchanged, as before.
