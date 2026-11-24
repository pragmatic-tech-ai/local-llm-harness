# Task 7 report: App, the Models rail item

Status: DONE. Commit `2ec6b5ab feat(app): Models rail item for repositories` on `feat/model-providers` (not pushed).

## Implemented

- `agents.ts`: wire types `RepositoryKind` (enum `pragmatic`/`openrouter`), `ProviderCapabilities`, `RepositorySummary`,
  `OpenRouterModelInfo`, `RepositoryModels`, `SaveRepositoryParams`, `SaveRepositoryResult`. Field names match
  `backend/src/providers/registry.rs` (`RepositorySummary`, `RepositoryModels`, `SaveResult`) and
  `providers/openrouter/api.rs` (`OpenRouterModel`: id, name, context_length, prompt_price, completion_price, reasoning).
- `RuntimeClient`: `ListRepositories()`, `ListRepositoryModels(name, refresh)`, `SaveRepository({name, previous_name?, id_prefixes, api_key?})`
  (sends only the optional fields that are set), `DeleteRepository(name)`, and `OnRepositoriesChanged` for `repositories_changed`.
- `RepositoryRowViewModel`: `Text` = `{name}  {Kind}  {n} models`, plus `DetailText` (`{Kind}  {n} models`), `HasError`, updated in place.
- `RepositoryModelRowViewModel`: `Id`, `ContextText` (`200K`, `1M`, `512`), `PriceText` (`3.00/15.00`), `RuntimesText` for Pragmatic models, `IsPriced`.
- `ModelsPanelViewModel`: `Rows`, `Selected` (a user pick is a request, as in the Agents tree), `AddOpenRouterCommand`,
  `RefreshCommand`, `StatusText`; reloads on `repositories_changed`, reconciling rows by name so the selection survives.
- `RepositoryEditorViewModel`: Pragmatic = read-only (`IsEditable=false`, models with runtimes, a capabilities line);
  OpenRouter = `Name`, `KeyHint` (`••••7f2a` / `not set`), `NewKey` (password box), `FilterText` -> `Prefixes`,
  `SaveCommand`, `RevertCommand`, `DeleteCommand`, `RefreshCommand`, `FetchedText` (`fetched HH:MM`), `ErrorText`,
  `HasNoKeyWarning` + `NoKeyWarning` (`no API key: models can be listed but not run`, verbatim), `Models`.
  Save sends `previous_name` for an existing repository (rename support) and `api_key` only when `NewKey` is non-empty.
  A new draft is named `openrouter` (or `openrouter-2`, ... when taken).
- `ModelsService`: mirrors `AgentsService` (ServiceKey, `Panel`, `Editor`, `IActivatable`, shares `SessionsService.Client`,
  follows connection state and runtime error). Unsaved-changes guard on picking another row, on Add, and on leaving
  the rail (Cancel switches back, as Agents does). Delete asks a confirm; a refusal shows the server error in the editor.
- Views: `views/ModelsModule.mu` (Capability "Models", icon `ModelsIcon`), `views/ModelsPanel.mu` (row DataTemplate:
  name over `{Kind}  {n} models` plus a `fetch failed` mark), `views/RepositoryEditor.mu` (model rows have their own
  DataTemplate, `ItemsPanel = @VerticalStackPanel`; key field is `TextBox [ IsPassword = true ]`, mural supports it),
  `views/icons/models.svg` (stacked layers), registered in `app.mu` and `views/RuntimeIcons.mu`.
  `.mu.js` files are not tracked by the repo, so none were committed.

## RED / GREEN

- RED: wrote all tests first; `npx vitest run` on the 6 files: 5 test files failed (modules
  `ModelsService`, `ModelsPanelViewModel`, `RepositoryEditorViewModel`, `RepositoryRowViewModel`,
  `RepositoryModelRowViewModel` not found). The RuntimeClient methods were written just before their table rows, so
  the RuntimeClient rows passed on first run (no RED for those 6 rows).
- GREEN: the 6 files: 47/47 passed on first implementation run. Final: `npm test` 22 files, 249 tests passed.
- Brief-named tests: `panel_lists_repositories_with_counts`, `repositories_changed_reloads_panel` (ModelsPanelViewModel.test.ts);
  `pragmatic_editor_is_read_only`, `openrouter_save_sends_key_only_when_changed`, `filter_text_splits_to_prefixes`,
  `refresh_shows_fetched_time_and_errors`, `no_key_shows_warning` (RepositoryEditorViewModel.test.ts);
  `delete_refused_shows_server_error`, `unsaved_changes_guard_on_leave` (ModelsService.test.ts). Plus extra tests for
  rows, drafts, save errors/revert, late replies, reconnect, refresh-after-change, and the hidden-text rule below.
- `npm run typecheck:web`: clean. `npm run typecheck:node`: clean. `npm run build`: OK.

## Real-window check

Built app launched with Playwright `_electron` (Plexus playwright 1.63, app's electron, ELECTRON_RUN_AS_NODE removed).
Script: `D:/tmp/models-ui/probe.cjs`. The window connected to the hive (supervisor :8778, runtime :8779).

- The hive runtime is NOT deployed with this branch: `list_repositories` answers `unknown method: list_repositories`.
  The panel shows it in red: `repositories unavailable: unknown method: list_repositories` (`02-models-real-runtime.png`).
- To see real rendering with data, a Playwright `routeWebSocket` on :8779 answered `list_repositories`,
  `list_repository_models` and `save_repository` with canned wire-shaped data and forwarded every other frame to the
  real runtime, then the page was reloaded. Screenshots (all looked at):
  - `03-models-list.png`: rows `hive / Pragmatic  2 models`, `openrouter / OpenRouter  3 models`, `or-nokey / OpenRouter  0 models  fetch failed`.
  - `04-openrouter-editor.png`: header, `prompt tokens estimated`, Name, `API key ••••7f2a`, password field,
    filter `anthropic/, google/`, Refresh models + `fetched HH:MM`, `3 models` and the table (id, `200K`/`1M`, `3.00/15.00` etc.).
  - `05-hive-editor.png`: read-only Pragmatic: header, capabilities line, `2 models`, `gemma-4-e2b  llamacpp, candle`, `qwen3-4b  llamacpp`; no form, no Save/Delete.
  - `04b-nokey-direct.png`, `06-nokey-editor.png`: server error in red, the verbatim no-key warning in amber, `API key not set`.
  - `07a-discard-guard.png`: the Unsaved changes dialog when picking hive with a dirty draft; `07b-draft-after-hive.png`: the draft after Discard.
  - `08-draft-key-typed.png`: the typed key renders as bullets; the warning disappears.
  - `09-after-save.png`: the route saw `save_repository {"name":"openrouter-2","id_prefixes":[],"api_key":"sk-or-v1-secret-abcd"}`; editor then shows `API key ••••abcd`.
- Not seen live: real OpenRouter fetches, a real `repositories_changed` broadcast after save/delete, and a real delete
  refusal (covered by unit tests only, since the hive runtime predates Task 5).

## Concerns

1. **mural render bug found (worked around, not fixed in mural).** A TextBlock whose text changes while an ancestor is
   Collapsed, and is then set to that same text once shown, keeps its OLD render (seen: `••••7f2a` stayed on a keyless
   repository after openrouter -> hive -> or-nokey). The editor follows a rule (documented on the class as
   HiddenTextRule, pinned by `hidden_texts_read_empty_and_shown_texts_never_do`): texts in a collapsible part read ""
   while hidden and are never "" while shown (or collapse themselves when ""). Worth a mural issue; other apps'
   templates that collapse parts may hit it.
2. Row text: the panel is narrow, so the single-line `Text` (`{name}  {Kind}  {n} models`) was clipped; the view
   renders name over `DetailText` instead. `Text` stays on the VM as the brief's interface (tested).
3. The template editor still says Host (Task 8 territory); untouched here.

---

# Fix round 1 (commit `0cda5793 fix(app): models editor filter stale text, signal and hint cleanups`)

## FilterText stale-text path (Important)

- Ran exactly openrouter (`anthropic/, google/`) -> hive -> or-nokey (no prefixes), and openrouter -> hive -> Add,
  with the same canned-route setup (`D:/tmp/models-ui/probe2.cjs`). Screenshots, all looked at:
  `p2-1-openrouter.png` (filter `anthropic/, google/`), `p2-2-hive.png`, `p2-3-nokey-after-hive.png` (filter
  box empty, grey placeholder shown), `p2-4-openrouter.png`, `p2-5-hive.png`, `p2-6-draft-after-hive.png` (empty,
  placeholder). Both before and after the fix the Filter box was NOT stale.
- Why it was not stale: field props (Name/NewKey/FilterText) happened to be raised before `IsOpenRouter` flipped,
  so the "" was written while the form was still visible. The reviewer is right that the documented rule did not
  cover this: it held only by list order, and the reverse direction (showing a part from collapsed, e.g. empty ->
  first open) wrote texts while still collapsed.
- Fix (one place, `RepositoryEditorViewModel.Changed`): HiddenTextRule is now an explicit ordering. Changed raises the
  visibility props that are now true first (parts being shown appear), then every field and text, then the
  visibility props that are now false (parts being hidden disappear last). So no bound text or field is ever written
  while its part is collapsed, whatever the value ("" included). Pinned by `texts_change_only_while_their_part_is_shown`
  (asserts the raise order on empty -> OpenRouter, OpenRouter -> Pragmatic, Pragmatic -> draft, and Clear); that
  test fails against the old order. The "" reads of hidden-only texts stay (they are meaningful: a Pragmatic
  repository has no key hint) and keep their test.

## Minors

- `OnSaved` removed (no subscriber; ModelsService follows saves through `repositories_changed` -> Reload).
- Client-side `key.slice(-4)` removed. After a save that sent a key, `KeyHint` shows the neutral `••••` (and the
  no-key warning stays off) until the server's `key_hint` arrives via Reload. Tests updated: `••••` after save,
  `••••9999` after Reload with the server hint.
- `RaiseNamed` takes `readonly (keyof RepositoryEditorViewModel)[]`; the prop lists (FieldProps, VisibilityProps,
  TextProps, StateProps) are typed so, and the reflective cast is gone: a renamed property fails to compile.

## Verification

- `npm test`: 22 files, 250 tests passed. `npm run typecheck:web` and `npm run typecheck:node`: clean. `npm run build`: OK.
- Re-ran `probe.cjs` and `probe2.cjs` on the new build: list, editors, guard dialog and draft render as before;
  after saving a draft with a key the editor shows `API key ••••` (the route does not broadcast
  `repositories_changed`, so the server hint never arrives there, as expected).

## mural follow-up

- **Component:** mural visual engine, a `TextBlock` (seen with `Text = $Binding`, BodySm style, mural ^0.67.1) whose
  ancestor's `Visibility` is bound and toggled to `Collapsed`. TextBox was not observed going stale in the same
  situation, but it was not isolated either.
- **Steps (seen in the Models editor before the workaround):**
  1. A StackPanel `[ Visibility = $IsOpenRouter << ToVisibility ]` contains `TextBlock [ Text = $KeyHint ]`.
     Show it with KeyHint = `••••7f2a` (rendered).
  2. In one change, raise `IsOpenRouter` = false (the panel collapses), then raise `KeyHint` = `not set`. The
     TextBlock's Text DP becomes `not set` while collapsed.
  3. Later, raise `IsOpenRouter` = true and `KeyHint` = `not set` again (same value, so the binding/DP does not
     change).
- **Expected:** the TextBlock shows `not set`.
- **Actual:** it shows the old render `••••7f2a`. `getBoundingClientRect` and the screenshot both show the old
  text. The DP value is `not set`. A text set while collapsed is not re-rendered when the ancestor becomes visible.
  Measure/Arrange in `visual-engine/visual.js` short-circuit for Collapsed, and the render pass for the text
  seems skipped and never redone. Root cause not confirmed.
- **Workaround used (app side, `RepositoryEditorViewModel`, HiddenTextRule):** raise newly visible parts before
  their texts and newly hidden parts after them, so no text changes while its part is collapsed. Hidden-only texts
  also read "".
