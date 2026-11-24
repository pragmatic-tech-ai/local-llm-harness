### Task 6: Trace section in the instrument panel

**Files:**
- Create:
  - `app/src/renderer/src/TraceViewModel.ts`, which holds `TraceViewModel`, `TraceTurnViewModel` and `TraceStepViewModel`, or one file each if the app's pattern is one class per file
  - their test file
  - `app/src/renderer/src/views/TracePanel.mu`
- Modify:
  - `app/src/renderer/src/SessionViewModel.ts` (owns a `TraceViewModel` and drives it on attach, session switch and reconnect)
  - `app/src/renderer/src/views/InstrumentPanel.mu`, or the session view that hosts it: place `TracePanel` below the context bar, in its own scroll region

**Interfaces:**
- Consumes: Task 5.
- Produces:
  - `TraceViewModel`:
    - `Turns: ObservableCollection<TraceTurnViewModel>`, newest first
    - `LoadOlderCommand`, which `CanExecute` while `more`
    - `WarningText` and `HasWarning`
    - `ShowSession(client: RuntimeClient | null, id: string | null): Promise<void>`. This clears the list and loads the first page. It is called on attach, on switch and after reconnect.
    - It applies live signals only for its current id.
    - It deduplicates by `(turn, step)`, and a live record replaces the paged one.
    - A turn row appears at its first `trace_step`, and its summary fills in on `trace_turn`.
    - `reset_pending` shows a divider above the newest turn.
  - `TraceTurnViewModel`:
    - `Expanded`, which defaults to true for the newest turn and false for the others
    - `HeaderText`, `Steps` and `ResetBefore`.
    - `HeaderText` takes one of these forms:
      - `Turn {n} · {reason} · {steps} steps · {s} s · peak {pct}%`, where `{s}` has one decimal and `{pct}` is an integer;
      - `Turn {n} · running…` while it is the live turn with no summary;
      - `Turn {n} · incomplete` when there is no summary and it is not live.
  - `TraceStepViewModel`:
    - `Text`: `{step}  ctx {used}/{total} ({pct}%)`. Numbers use a thin-space thousands separator, as in the sketch (`3 102/8 192`). If fill is null, it is `{step}  —`.
    - `FillFraction`, for the bar.
    - `StatsText`: `out {n}`, then ` (think {k})` only when k > 0, then `  load {s} s` when present, then `  ttft {s} s  gen {s} s  {r} t/s` with one decimal each, omitting any absent part.
    - `Tools`: one `└ {name}  ok|failed  {ms} ms  {bytes} B` string each.
    - `ErrorText`, set when `end` is rejected, error or aborted. `aborted` shows as `aborted`.

- [ ] **Step 1: Write the failing tests:**
  - `first_page_lists_turns_newest_first_with_headers`: header text exactly as specified, for an answered turn of 2 steps, 21400 ms and peak 0.41: `Turn 3 · answered · 2 steps · 21.4 s · peak 41%`.
  - `step_text_formats_fill_tokens_and_timing`: `1  ctx 3 102/8 192 (38%)`, and stats `out 38  ttft 1.9 s  gen 2.6 s  14.6 t/s`. A think of 5 shows `out 38 (think 5)`.
  - `tool_rows_format`: `└ read_file  ok  4 ms  26 B`.
  - `live_step_then_turn_updates_in_place`: the header goes from `running…` to the final header.
  - `live_record_replaces_paged_duplicate`, which covers Review Focus 3.
  - `turn_without_summary_and_not_live_is_incomplete`, which covers Review Focus 4.
  - `load_older_appends_and_disables_at_end`
  - `reset_dividers_and_pending_reset`
  - `warning_shows_and_clears_on_switch`
  - `signals_for_other_sessions_are_ignored`
  - In the `SessionViewModel` tests: `attach_loads_trace_and_reconnect_reloads`.
- [ ] **Step 2–4:** Run vitest on the new tests: they fail first, then implement, then they pass. Then run `npm test && npm run typecheck:web && npm run typecheck:node`.
- [ ] **Step 5: Commit** `feat(app): agent trace in the instrument panel`.

