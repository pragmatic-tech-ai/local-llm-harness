### Spec Compliance

- ✅ `TraceViewModel` is owned by the session view and hosted by the instrument panel. `Panel.HostTrace(this.Trace)` is in the `SessionViewModel` constructor, and the panel binds `ContentControl [ Content = $Trace ]` right below the context bar (InstrumentPanel.mu:58).
- ✅ The header strings match the brief exactly: `Turn {n} · {reason} · {steps} steps · {s} s · peak {pct}%`, `Turn {n} · running…` and `Turn {n} · incomplete` (TraceViewModel.ts TraceTurnViewModel SummaryFormat/RunningFormat/IncompleteFormat). `{s}` is `toFixed(1)` of ms/1000 and `{pct}` is `Math.round`. The test asserts `Turn 3 · answered · 2 steps · 21.4 s · peak 41%`.
- ✅ The step `Text` is `{step}  ctx {used}/{total} ({pct}%)`, with `{step}  —` when the fill is null. The thousands separator is U+2009; I checked the bytes, which are `e2 80 89`.
- ✅ `StatsText` follows the brief: `out n`, ` (think k)` only when k > 0, `  load`, `  ttft`, `  gen`, then the rate only when gen_ms > 0. Absent parts are omitted. The `Tools` strings are `└ {name}  ok|failed  {ms} ms  {bytes} B`. `ErrorText` covers rejected, error and aborted.
- ✅ Live and paged records are deduplicated by `(turn, step)`. `ApplyStep(record, replace)` never lets a paged record overwrite, and a live record always replaces through `SetAt`. `ApplySummary` behaves the same way. The test covers both orders.
- ✅ The live turn and incomplete rulings are implemented. `MarkLive` runs only on a live `trace_step`, and a page-only turn with no summary reads `incomplete`.
- ✅ Session filtering: all three subscriptions check `n.id === this.id`, and they are re-made on every `ShowSession`.
- ✅ Async: a token drops a late first page or older page after a re-show, a switch or `ShowSession(null, null)`. A page landing after live records cannot overwrite them, because of the replace flags.
- ✅ Load older sends `before = oldest paged turn` and updates `more`. `CanExecute` is false while the request is in flight and at the end.
- ✅ Reset dividers: `reset_before` is drawn below its row, which matches the sketch. `reset_pending` shows a top divider. The first live step of a new newest turn moves that divider onto the new turn. A page that says `reset_pending` while a newer live turn is already shown assigns `ResetBefore` to that live turn.
- ✅ The trace has its own scroll region (`ScrollViewer MaxHeight 320`), below the context bar.
- ❌ The spec requires "one sub-row per tool". TracePanel.mu:62 binds `ToolsText`, which is `Tools.join('\n')` (TraceViewModel.ts:139-141), to a single `TextBlock`. Mural's `TextBlock` does not break lines on `\n`. See Important 1.

### Strengths

- The replace-flag design (`replace` true for live, false for paged) handles both arrival orders in one rule. It needs no ordering assumptions, which is what the spec's "no ordering guarantee is needed" asks for.
- The token guard covers the first page, older pages, `ShowSession(null, null)` and `dispose`. `loadingOlder` resets on re-show, so a stale older request cannot leave the button disabled forever.
- `LiveTurnAfter` handles the hard edge correctly: `reset_pending` arrives after a live turn is already shown, so the divider goes under the live turn and not at the top.
- The tests are concrete and exact-string. They cover null fill, `gen 0.0 s` with no rate, failed tools, thin-space thousands, a repeated `trace_turn`, and a late page after `ShowSession(null, null)`.
- The `.mu` follows existing app patterns. The bar is the context-bar Border-in-Border. The nested `ScrollViewer` with `MaxHeight` is the same as Raw prompt and Logprobs. A per-file `ItemsPanelTemplate` is how InstrumentPanel and TemplateEditor do it. `ToggleButton.IsChecked = $Expanded` compiles to the same `DataContextBinding` as `Checkbox.IsChecked = $EnableThinking` (I checked the compiled TracePanel.mu.js:82 against InstrumentPanel.mu.js:156). The three imports from one module compile to a named import, and `../messages.js` already sets that precedent.
- The house style holds throughout: classes only, Allman braces, no semicolons, PascalCase public members, hoisted literals, and Observable VMs.

### Issues

#### Critical

None.

#### Important

1. **TracePanel.mu:62 and TraceViewModel.ts:139-141: tool rows render on one line.**
   - What's wrong: `ToolsText` joins the rows with `\n` into one `TextBlock` that has no `TextWrapping`. In mural's `TextBlock.MeasureOverride` (node_modules/@pragmatic-tech-ai/mural/dist/basic/text-block.js:144-156), the NoWrap path measures and renders the whole string as one `FormattedText` line. The Wrap path splits on `/\s+/` and rejoins with `' '`. Neither path breaks on `\n`, so two or more tools come out as one long run-on line.
   - Why it matters: the spec says "one sub-row per tool" (sketch `└ read_file …` per row). The implementer flagged this risk but did not resolve it. The live check would fail.
   - Fix: bind the `Tools: string[]` the brief already specifies to an `ItemsControl [ ItemsSource = $Tools, ItemsPanel = @TraceStackPanel ]` whose items are TextBlocks. A plain string item needs a template, or wrap each string in a tiny row VM if the app has no string template. Then drop `ToolsText` and `ToolsSeparator`.
2. **TraceViewModel.ts:377 together with 425/476: a page-failure warning sticks after a successful reload of the same session.**
   - What's wrong: `ShowSession` clears the warning only when the id changes, which is meant to keep a `trace_warning` across a reconnect. But the `trace unavailable: {msg}` warning goes into the same slot. The realistic case: the connection drops while `get_trace` is in flight, so `Detach` does not bump the trace token and the rejection sets `trace unavailable: …`. The re-attach then calls `ShowSession(client, sameId)`, the page loads fine, and the stale "trace unavailable" line stays until the user switches session. A failed `LoadOlder` followed by a later successful page has the same effect.
   - Why it matters: the UI says the trace is unavailable while it is showing it. That is incorrect state on exactly the reconnect path this task drives.
   - Fix: keep the page error separate from the `trace_warning` text, for example a `pageError` field combined into `WarningText`. Clear `pageError` when a page succeeds or on every `ShowSession`. Alternatively, clear the warning on a successful page when it was a page failure. Add a test: page fails, same-id re-show succeeds, and the warning is cleared.

#### Minor

1. **TraceViewModel.ts:557: auto-collapse overrides the user's choice (implementer concern 3).**
   - What's wrong: when a new newest row is created, the previous top row is forced to `Expanded = false`, even if the user had expanded it by hand.
   - Why it matters: the spec's "default" wording describes the initial state, not something to enforce. Collapsing the old newest turn is a reasonable reading. Overriding an explicit user toggle is mildly surprising, because the user may be reading that turn as the next one starts.
   - Fix: collapse it only if its expansion is still the default, using a `userToggled` flag set by the setter path from the binding. Losing the expand state on reload is fine, because the spec says a reload clears and reloads. That part is not an issue.
2. **TracePanel.mu:61: StatsText's `TextWrapping = Wrap` collapses the double-space separators.**
   - What's wrong: inside the width-bounded ScrollViewer, the Wrap path splits on `\s+` and rejoins with a single space, so `out 38  ttft 1.9 s  gen 2.6 s` renders with single spaces. The VM string is right, but the rendered text does not match the sketch's spacing. The same goes for the thin space inside `out` thousands.
   - Fix: drop `Wrap` on `StatsText`, since the line is short. Otherwise accept the trade-off and note it in the live check.
3. **TraceViewModel.test.ts:101: leftover casts.**
   - What's wrong: `prompt_tokens: null as unknown as number` and `n_ctx: null as unknown as number` are no longer needed now that `StepRecord` is widened to `number | null`.
   - Why it matters: the casts hide whether the widened type is actually exercised.
   - Fix: pass `null` directly.
4. **Test coverage gaps.**
   - What's missing: no test for a failed `LoadOlder`, which should re-enable the command and show the warning. There is also no test for a live `trace_turn` that lands before a page carrying a different summary for the same turn. The live summary must win, and the code does handle it.
   - Fix: add both, alongside the Important 2 regression test.

#### Implementer concerns, judged

- **The `.mu` was never rendered.**
  - ToggleButton header: fine. It binds the same way as the Checkbox, and `ToggleButton extends Button` takes content as the Load older Button does.
  - The 320 px nested scroll: fine. It is the existing Raw prompt and Logprobs pattern.
  - Newlines in a TextBlock: confirmed broken. That is Important 1.
- **Widening `prompt_tokens`/`n_ctx` to `number | null`:** correct. The spec's Records section says a step that never reached the worker has them null, so the widening fixes Task 5's typing. The only leftover is the test casts in Minor 3.
- **Auto-collapse and losing expand state on reload:** the auto-collapse is a Minor, see Minor 1. Losing the state on reload is by design.

### Assessment

**Task quality:** Needs fixes

**Reasoning:** The VM logic, deduplication, session filtering, race guards, paging and reset handling are correct and well tested. However, tool rows cannot render one per line in mural's TextBlock. In addition, a page-failure warning goes stale on the reconnect path this task wires.

## Re-review round 1

Scope: fix commit 38107249 against the findings and rulings.

- Important 1 (tool rows): fixed. `TraceToolViewModel` rows go through an `ItemsControl` with a `DataTemplate`. `ToolsText` and `ToolsSeparator` are gone. `HasTools` still gates the visibility. The test asserts the row texts and an empty list.
- Important 2 (stale warning): the "trace unavailable" part is fixed. `pageError` is cleared on every `ShowSession` and in `ApplyPage`, so a successful reload clears it. The `trace_warning` part does not match the ruling. The ruling says a `trace_warning` note stays until the next switch or the next successful reload. `ShowSession` clears `noteWarning` only when the id changes (TraceViewModel.ts, in the `UpdateWarning` lambda), and `ApplyPage` never touches it. A note therefore survives a successful same-session reload. The test `successful_reload_clears_a_page_failure_but_keeps_a_trace_warning` locks that in: the final `WarningText` is still `trace write failed: disk full` after a successful reload. Fix: clear `noteWarning` in `ApplyPage` (or on a successful first page) and update the test to expect `''`. If the controller meant "stays across a successful reload", the ruling wording should be corrected instead.
- Minor 1 (auto-collapse): fixed. `UserToggled` is set by the public setter, `CollapseByDefault` skips toggled rows, and the test covers it.
- Minor 2 (stats separators): fixed. `Wrap` is removed from `StatsText`.
- Minor 3 (casts): fixed.
- Minor 4 (tests): fixed. Both new tests are present and assert real behavior.

Regressions: none found.

**Verdict: Needs fixes**
