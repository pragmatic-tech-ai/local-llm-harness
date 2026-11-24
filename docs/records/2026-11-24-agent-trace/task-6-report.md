# Task 6 report: Trace section in the instrument panel

Commit: `cc8a4f81 feat(app): agent trace in the instrument panel` (branch feat/agent-trace, not pushed).

## What was built

`app/src/renderer/src/TraceViewModel.ts` holds four classes:
- `TraceNumbers` (static helpers): `Thousands` (U+2009 thin space as `ThousandsSeparator`), `Seconds` (one decimal), `OneDecimal`, `Percent` (rounded int), `PerSecond`.
- `TraceStepViewModel(record: StepRecord)`: `Step`, `Text` (`{step}  ctx {used}/{total} ({pct}%)`, or `{step}  —` when `prompt_tokens`/`n_ctx` is null or `n_ctx` is 0), `FillFraction` (clamped 0..1), `BarTrackWidth` (80 px) and `BarWidth` for the bar, `StatsText` (`out n`, ` (think k)` if k>0, `  load s`, `  ttft s`, `  gen s`, `  r t/s` only when gen_ms>0, absent parts omitted), `Tools: string[]` (`└ name  ok|failed  ms ms  bytes B`, thin-space thousands), `ToolsText` (newline-joined, for one TextBlock), `HasTools`, `ErrorText` (error text, or the end name when the text is null, for rejected/error; `aborted` for aborted), `HasError`. Immutable: a replaced record means a new step VM, swapped in via `SetAt`.
- `TraceTurnViewModel(turn, expanded)`: `Turn`, `Steps`, `Expanded` (get/set, INPC), `Glyph` (▾/▸), `HeaderText` (summary form / `running…` while live / `incomplete`), `ResetBefore`, plus `MarkLive()`, `ApplySummary(summary, replace)`, `ApplyStep(record, replace)` (step-ordered insert; paged records never overwrite, live records always replace), `SetResetBefore`.
- `TraceViewModel`: `Turns`, `LoadOlderCommand` (CanExecute while `more` and no older page is in flight), `HasMore`, `ResetPending`, `WarningText`/`HasWarning`, `ShowSession(client, id): Promise<void>`, `dispose()`.
  - `ShowSession` takes a token, clears the warning only when the id changes, re-subscribes the three trace signals for that id, clears the list, then loads `GetTrace(id)`. A late reply for an earlier call is dropped. A failed page shows `trace unavailable: {msg}` as the warning.
  - Live step: the row is created at the first `trace_step` (in newest-first position), marked live, and the step is upserted with replacement. Live turn: the summary replaces whatever was there and ends live. Rows created only from a page with no summary read `incomplete`.
  - Expansion: a row created at index 0 is expanded and the previous top row collapses. Rows inserted below are collapsed.
  - Load older: `GetTrace(id, oldestPagedTurn)`. Turns are inserted in order, `more` updates, and `reset_pending` is honoured only on the first page.
  - Reset: `ResetBefore` draws the divider below that turn's row, because the list is newest first and the reset sits between this turn and the older one (this matches the sketch). `ResetPending` draws a divider at the top. When the first live step of a new newest turn arrives while a reset is pending, that turn gets `ResetBefore` and the pending flag clears. If the first page says `reset_pending` but a newer live turn is already shown, that live turn takes `ResetBefore`.

`SessionViewModel` owns `public readonly Trace = new TraceViewModel()` and hands it to the panel in its constructor (`this.Panel.HostTrace(this.Trace)`). A successful `Attach` calls `void this.Trace.ShowSession(this.client, id)`. That covers attach, switch, and the re-attach `SessionsService.OnConnected` does after a reconnect. `Reset()` (switch to another id, and `Clear`) calls `ShowSession(null, null)`. `Detach` leaves the rows visible until the re-attach reloads them. `dispose` disposes the trace.

`InstrumentPanelViewModel` gained `Trace: TraceViewModel | null` and `HostTrace(trace)`. The session view drives the trace; the panel only hosts it for the template.

`agents.ts`: `StepRecord.prompt_tokens` and `n_ctx` are widened to `number | null`, as the spec says ("a step that never reached the worker ... prompt_tokens/n_ctx null"). Task 5 had typed them as `number`.

Views:
- `views/TracePanel.mu` holds `TracePanelResources`, with DataTemplates for the three VMs. The trace sits in a bordered box with its own `ScrollViewer` (MaxHeight 320). The turn header is a `ToggleButton [ Variant = Text, IsChecked = $Expanded ]`, because the app has no expander control; its only expand pattern is TreeView `IsExpanded`. The toggle holds the glyph and the header text, and steps are visible while `$Expanded`. The step bar is the context-bar Border-in-Border style at 6 px high and 80 px wide. Error lines use `@StateDanger`, the panel's existing warning colour. The "Load older…" button is visible while `HasMore`.
- `views/InstrumentPanel.mu`: `ContentControl [ Content = $Trace ]` sits right after the context bar and its warning line, above "Raw prompt".
- `app.mu` merges `TracePanelResources`.

## Tests

- `TraceViewModel.test.ts` has 10 tests, all from the brief. The step test also covers null fill, `gen 0.0 s` with no rate, and load. The tools test also covers `failed`, thousands, and `aborted`. The live-replace test covers both orders (live before the page, and page before live) plus a repeated `trace_turn`. The incomplete test covers a live step reviving a paged turn, and a re-show going back to incomplete. The warning test covers a failed page. The other-sessions test covers a late page dropped after `ShowSession(null, null)`.
- `SessionViewModel.test.ts` adds `attach_loads_trace_and_reconnect_reloads`. It checks that the panel hosts the trace, that attach sends `get_trace {id}`, that Detach plus re-attach sends a second `get_trace` and reloads, and that switching or Clear empties the trace.

TDD evidence:
- RED: `npx vitest run src/renderer/src/TraceViewModel.test.ts src/renderer/src/SessionViewModel.test.ts` gave 2 files failed: `Failed to load url ./TraceViewModel ... Does the file exist?`, and `attach_loads_trace_and_reconnect_reloads` failed at `expect(rig.Frame('get_trace')?.params)` (undefined).
- GREEN: same command, 23 passed (10 + 13).
- Gate: `npm test` gave 17 files and 188 tests passed (the compile:mu pretest compiled 14 .mu files, including TracePanel). `npm run typecheck:web` and `npm run typecheck:node` were both clean.
- All touched files use LF endings.

## Self-review

The house style holds: classes only, Allman braces, no semicolons, PascalCase public members, and literals as `private static readonly`. The step-end cases are `StepEnd`-typed constants. `.mu` labels stay inline. The VMs extend Observable. Nothing in a lower layer calls upward.

## Concerns / notes

- The `.mu` was compiled and type-checked but never rendered, because the Electron app was not run. Whether ToggleButton (Variant=Text) accepts StackPanel content, the 320 px trace scroll nested inside the panel's scroll, and whether TextBlock honours `\n` in `ToolsText` are only confirmed by the live check.
- When a new turn becomes the newest, the previous newest collapses even if the user had expanded it by hand.
- Expansion state is lost on a reload (reconnect or re-show), because the list is rebuilt.

## Owner's live UI check (Task 7)

1. Open a session that has history. The Trace section appears under the Context bar, newest turn first. The newest turn is expanded with ▾, older turns are collapsed with ▸, and the headers read `Turn n · reason · k steps · s.s s · peak p%`.
2. Click a header: it toggles its steps, and the glyph flips.
3. Step rows show `1  ctx 3 102/8 192 (38%)` with thin-space thousands, and a small fill bar to the right whose width matches the percentage. The `out 38  ttft 1.9 s  gen 2.6 s  14.6 t/s` line keeps its double-space gaps and does not wrap. Each tool the step ran gets its own `└ tool  ok  n ms  n B` line (check a step with 2 or more tools).
4. A step that errored, was rejected or was aborted shows its text in the danger colour.
5. Send a message. A `Turn n · running…` row appears at the top at the first step, expanded. The previous top row collapses unless you had toggled it by hand, in which case it keeps its state. Steps fill in live, and the header changes to the summary when the turn ends.
6. Reset the session. A `── reset ──` divider appears at the top. After the next turn's first step, the divider sits below that new turn.
7. On a session with more than 20 turns, "Load older…" appears at the bottom and appends older turns. It disappears at the end of the history.
8. The trace box scrolls on its own, capped at 320 px, without breaking the panel's outer scroll.
9. Switch sessions: the trace clears and reloads, and any trace warning clears.
10. Stop and restart the runtime (reconnect): the rows stay while reconnecting, then reload after the re-attach.
11. With the log made unwritable, a `trace_warning` shows in the danger colour above the box. It stays visible until the next successful trace page load (for example, after a reconnect re-attach) or a session switch.
12. Drop the connection while a trace page is loading. Any `trace unavailable: …` line disappears once the re-attach loads the page.

## Fix round 1

Commit: `38107249 fix(app): trace tool sub-rows, page-failure warning clears on reload, keep user expansion`.

- **Important 1:** I added `TraceToolViewModel` (Observable, `Text`). `TraceStepViewModel.Tools` is now `TraceToolViewModel[]`, built once in the constructor, and `ToolsText` and `ToolsSeparator` are gone. TracePanel.mu renders the tools through `ItemsControl [ ItemsSource = $Tools, ItemsPanel = @TraceStackPanel ]`, using a `DataTemplate [ DataType = TraceToolViewModel ]` with one TextBlock per tool. `tool_rows_format` now asserts `Tools.map(t => t.Text)` and checks that a step with no tools has an empty list.
- **Important 2:** the warning now has two sources:
  - `noteWarning` holds the `trace_warning` text. It is cleared only when `ShowSession` switches to another id.
  - `pageError` holds `trace unavailable: …`. It is cleared on every `ShowSession` and by every successful page (first or older, in `ApplyPage`).

  `WarningText` combines them as `note · pageError`, and `UpdateWarning` raises `WarningText` and `HasWarning` when they change. The new test `successful_reload_clears_a_page_failure_but_keeps_a_trace_warning` covers this.
- **Minor 1:** `TraceTurnViewModel` gained `UserToggled`. The public `Expanded` setter, which the header binding uses, sets it. A new newest row calls `CollapseByDefault()` on the previous top, which collapses it only when it has not been toggled. Losing the state on reload is unchanged, by design. The new test is `new_newest_turn_collapses_the_previous_only_when_not_toggled`, and `first_page_…` also asserts `UserToggled`.
- **Minor 2:** I removed `TextWrapping = Wrap` from the StatsText TextBlock and noted in the `.mu` why.
- **Minor 3:** the `null as unknown as number` casts are gone, and the test passes `null` directly.
- **Minor 4:** I added two tests:
  - `failed_load_older_warns_keeps_more_and_can_retry` checks that the warning shows, `HasMore` stays true, `CanExecute` is true again, and the retry sends `before: 9`, appends the turn and clears the warning.
  - `live_summary_before_page_wins_over_paged_summary`.

TDD evidence:
- RED: I ran the new test file against the previous `TraceViewModel.ts`, restored from HEAD and then put back. 5 tests failed: `first_page_…` (UserToggled), `new_newest_turn_…`, `tool_rows_format`, `successful_reload_…` and `failed_load_older_…`. `live_summary_before_page_…` passed even on the old code, because that behaviour was already right; the test pins it.
- GREEN: `npx vitest run src/renderer/src/TraceViewModel.test.ts` passed 14 tests.
- Gate: `npm test` passed 192 tests in 17 files, and compile:mu compiled 14 files. `npm run typecheck:web` and `npm run typecheck:node` were both clean. All touched files use LF endings.

The owner live-check list above is updated: items 3, 5 and 11 changed, and item 12 is new.

## Fix round 2

Commit: `fb35dd4e fix(app): a successful trace page also clears an earlier trace_warning`.

- `ApplyPage` now clears both `pageError` and `noteWarning` in a single `UpdateWarning`. It is shared by the first page and Load older, so a successful older page also clears both. A switch to another id still clears the note in `ShowSession`. A failed same-session reload keeps the note, shown beside the page error. I updated the field and `ShowSession` doc comments to match.
- I renamed the test to `successful_reload_clears_a_page_failure_and_a_trace_warning`:
  - A page fails, and a same-id reload succeeds and clears it.
  - A `trace_warning` arrives after the page loaded, followed by a live step. It stays visible.
  - A failed same-id reload shows `note · trace unavailable: busy`.
  - The next successful reload clears both (`WarningText` is `''` and `HasWarning` is false).
- RED: I ran the updated test against the round-1 `TraceViewModel.ts`, restored from HEAD and then put back. It gave 1 failure (the final expectation got the old note text).
- GREEN: the file passes 14 tests. `npm test` passes 192 tests in 17 files. `npm run typecheck:web` and `npm run typecheck:node` are clean, and the touched files use LF endings.
- Live-check item 11 is updated: the warning stays visible until the next successful trace page load or a session switch.
