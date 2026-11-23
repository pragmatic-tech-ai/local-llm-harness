# Task 10 report: Agents capability (template tree and editor)

Status: DONE_WITH_CONCERNS
Commit: `1e6f55ce feat(app): Agents capability with template tree and editor` (BASE a50060ed)

## Built

- **Rail.** `views/AgentsModule.mu`: module and capability `Name = "Agents"`, `Icon = @AgentsIcon`, `ServiceKey = AgentsService`. The new icon is `views/icons/agents.svg` (lower-case, to match the existing `runtime-play.svg` / `settings.svg`; the brief's `Agents.svg` / `RuntimePlay.svg` casing does not match the repo). It is a 512 viewBox, single `currentColor` path in the runtime-play style: a robot head with antenna, ears and base. It is included in `RuntimeIcons.mu`. In `app.mu` I added `AgentsModule` to `.modules`, merged `AgentsPanelResources` and `TemplateEditorResources`, and added `DataTemplate [ DataType = AgentsService ] { ContentPresenter Content=$Panel }`.
- **`AgentsService`** (`AgentsService.ts`, ServiceBase plus `IActivatable`, Key `AgentsService`).
  - It uses the Sessions client through `provider.getRequired(SessionsService.Key).Client`, so there is no second socket. It follows `SessionsService.Connection` / `OnConnectionChanged`. On Connected it binds the tree, refreshes it, and calls `Editor.LoadCatalog()` (list_catalog plus list_all_tools). On any other state the tree is unbound: commands go inert and the tree stays as last seen.
  - `OnActivated()` presents the editor in the content host. I also added `OnActivated()` to `SessionsService`, which re-presents the session view. Without it, the shared ContentHost would keep showing the editor after a switch back to Sessions. That is the NavigationService `IActivatable` hook.
  - Selection goes through `Panel.OnSelectRequested`.
    - A group, or the template already open, is simply selected.
    - Any other template is selected and opened only if the editor is clean, or if the user picks Discard in `DialogService.Confirm` (Discard / Cancel). Cancel calls `Panel.KeepSelection()` (re-announced on a microtask, so the TreeView snaps back).
  - **Rail guard.** It subscribes to `NavigationService.PropertyChanged('ActiveService')`. Arriving records the rail item. Leaving with unsaved edits asks Discard / Cancel. Cancel writes the recorded item back to `NavigationService.SelectedItem`. Discard reverts the edits, or clears them for a new draft.
  - **After every refresh**, `SyncEditor`:
    - If the open template is gone, it clears the editor.
    - Otherwise `Editor.SetPath(node.Path)`. A Move while the editor is dirty therefore cannot be undone by the next save, because `save_template` with an id plus a path moves the file.
    - If the editor is clean, `Editor.Reload(node)` picks up hand edits and renames.
  - **Commands**, all through the Task 9 dialog patterns:
    - New template opens an unsaved `Untitled` draft in `Panel.SelectedGroupPath`. It goes through the dirty guard and deselects a selected template.
    - New group uses `TextPromptViewModel.Ask` (text `New group`), then `CreateGroup(parent/name)`.
    - Rename on a group uses `RenameGroup`. On a template it does `GetTemplate`, then `SaveTemplate(id, path, {...template, name})`, because the name is a field and the id (the file stem) stays. It is refused with `Save or revert the open template first.` when that template is open and dirty.
    - Delete confirms, then calls `DeleteGroup` / `DeleteTemplate`. Its errors show as `delete failed: group is not empty`.
    - Move to shows `MoveToDialogViewModel` in `DialogService.Show` with Cancel / Move, then calls `MoveTemplate(id, path)`.
    - Failures go to `Panel.ReportError` as `could not create the group:` / `rename failed:` / `delete failed:` / `move failed: …`.
  - After a draft is saved, `Editor.OnSaved(id)` → `Panel.SelectWhenListed(id)` selects the new node once `templates_changed` lists it.
- **`AgentsTreeViewModel`** has these members:
  - `Roots` and `Selected`. A user set of `Selected` only raises `OnSelectRequested`; null is re-announced.
  - `Select(node)`, `KeepSelection()`, `SelectWhenListed(id)`, `SelectedGroupPath`, `StatusText`, `IsEmpty`, `ReportError`, `Find(id)`, `Groups(): ChoiceItem[]` (root `""` labelled `(root)` first, then depth-first full paths), `Refresh()` and `Bind(client|null)`.
  - The commands `NewTemplateCommand`, `NewGroupCommand`, `RenameCommand`, `DeleteCommand`, `MoveToCommand` (templates only) and `RefreshCommand`.
  - The signals `OnSelectRequested`, `OnSelectionLost`, `OnRefreshed`, `OnNewTemplateRequested(path)`, `OnNewGroupRequested(path)`, `OnRenameRequested`, `OnDeleteRequested` and `OnMoveRequested`.
  - It refreshes on `templates_changed` and reconciles in place. It keeps the same node objects, so selection and expansion survive.
- **`TemplateTreeNodeViewModel` (extended).**
  - `Node` is now a getter.
  - Added `Key` (`group:{path}` / `template:{id}`), `static KeyOf(node)`, `static Pool(roots)`, `static Reconcile(target, nodes, pool)` and `Update(node, pool)`.
  - Reconcile reuses nodes by key across the whole tree, so a template moved between groups keeps its object (and selection). Rows are updated, moved or inserted in server order; leftover tail rows are removed. There is no reset.
  - `NewSessionDialogViewModel` still uses `FromNodes` and is unchanged.
- **`TemplateEditorViewModel`** is one stable instance in the content host.
  - **Fields:** `Name`, `SystemPrompt`, `StepBudgetText`, `RuntimeChoices`/`Runtime`/`RuntimeChoice`, `ModelChoices`/`Model`/`ModelChoice`, `ContextSizeText`, `EnableThinking`, `TemperatureText`/`TopPText` (blank = none), `HostChoices`/`Host`/`HostChoice` (`""` = `(any host)`), and `Tools: ObservableCollection<ToolChoice{Name, Description, Enabled}>`.
  - **State:** `SaveCommand`, `RevertCommand`, `IsDirty`, `CanSave`, `CanRevert`, `ErrorText`/`HasErrorText`, `TemplateId`, `Path`, `IsNew`, `HasTemplate`/`IsEmpty`/`EmptyText`, `HeaderText`, and `OnSaved: TypedSignal<string>`.
  - **Methods:** `LoadCatalog()`, `Open(node)`, `Reload(node)` (dropped if edits start before the reply), `OpenNew(path)`, `Clear()`, `SetPath(path)`, `Save(): Promise<boolean>` and `Revert()`.
  - **Field errors:**
    - The fields: `NameError` (blank), `StepBudgetError` (a whole number ≥ 1), `ContextSizeError` (a whole number ≥ 1), `TemperatureError` (a decimal ≥ 0 or blank) and `TopPError` (a decimal in 0..1 or blank).
    - Each has a `Has…Error` flag. Any of them disables Save and `Build()` returns null, so NaN is never sent.
  - **Choices.**
    - Models are filtered by `runtimes.includes(Runtime)`.
    - A runtime, model or host the catalog lacks is kept as a `ChoiceItem.Missing` entry, labelled `X (not in catalog)`. That covers a runtime change too: the model stays and is marked rather than being silently swapped.
  - **Tools.** There is one checkbox per `list_all_tools` entry, in registry order. An enabled tool the registry lacks is appended, marked `not in the tool registry`. `tools[]` is sent in checkbox order.
  - **Sampling.** Knobs the editor does not show (top_k, seed, …) are kept from the loaded template. When both fields are blank, `sampling` is omitted. `AgentTemplate.sampling` is now `Partial<SamplingConfig>`, which matches the backend's `#[serde(default)]`.
  - **Defaults.**
    - The values: name `Untitled`, no system prompt, no tools, step budget 8, `n_ctx` 8192, thinking off, the catalog's first runtime and the first model that runtime serves.
    - If the catalog arrives after a pristine draft or broken seed, the runtime and model are completed then.
  - **Broken templates (ruling).**
    - `get_template` is not called. ErrorText is the node's `error`, and the fields are the defaults overlaid with the node's name, runtime and model when present.
    - Save is allowed while clean, and overwrites the file to repair it.
    - A good node whose `get_template` fails is seeded the same way, with the RPC message.
  - **Dirty tracking** compares a fingerprint of the form with the one taken at fill or save time. The tool set is compared order-free. A draft is always dirty.
- **`ChoiceItem`** (`Value`, `Label`, `static Missing(value)`) is shared by the editor combos and the Move to group list. `ToolChoice` reports toggles to an `IToolChoiceOwner`.
- **`MoveToDialogViewModel`** has `Groups`, `SelectedGroup` (the current group preselected; null writes are ignored), `MoveCommand` (enabled for a different group) and `OnChosen(path)`.
- **Views.**
  - `views/AgentsPanel.mu`: a WrapPanel toolbar (New template / New group / Rename / Delete / Move to… / Refresh), a status line, and a TreeView. It uses `HierarchicalDataTemplate AgentTreeNode` with a red `broken` marker, and `AgentTreeItemStyle` with `IsExpanded=$IsExpanded`, `SelectedDataItem=$Selected`. The same file holds the Move to body (ListBox of `ChoiceItem`).
  - `views/TemplateEditor.mu`: the header with Revert / Save (Filled), the ErrorText line, and a scrolling form (TextBoxes, ComboBoxes bound to `SelectedItem=$…Choice`, Checkbox, and the tool ItemsControl). The field errors show under their fields, and there is an empty state.

## Tests

The brief's tests:
- `tree_renders_groups_and_templates` and `templates_changed_refreshes_tree` are in AgentsTreeViewModel.test.ts. The second checks that Researcher is renamed and moved to the root while keeping its object and selection, that the collapsed group stays collapsed, and that Broken is removed.
- `editor_save_sends_fields_and_clears_dirty`, `editor_shows_validation_error`, `model_choices_filter_by_runtime` and `broken_template_opens_with_error` are in TemplateEditorViewModel.test.ts.
- `selecting_template_opens_editor` and `delete_group_error_surfaces` are in AgentsService.test.ts.

Added for the rulings:
- `dirty_editor_asks_before_switching_template`, which covers Cancel keeping both the selection and the edit, then Discard, then a clean switch without asking.
- `dirty_editor_asks_before_leaving_the_rail`, which covers Cancel restoring the Agents rail item, then Discard reverting, then a clean leave.
- `unparsable_numbers_show_field_errors_and_disable_save`.
- `model_outside_the_catalog_stays_a_marked_choice`.
- `move_to_lists_the_groups_and_moves`, which checks `(root)` first, the current group preselected with Move disabled, and that the editor path follows the move.

Other new tests:
- **Tree:** selection lost, a user selection is only a request, select-when-listed, commands carry the selected group, group list order, refresh failure.
- **Editor:** opens into its fields, sampling kept or omitted, revert, the new draft and its first save, a blank name disables Save, a stale reload is dropped.
- **Service:**
  - shared-connection listing
  - activation shows the editor
  - deleting the open template clears the editor
  - a new draft saves into the group and is selected
  - New group prompt defaults
  - group and template rename
  - rename of the dirty open template is refused
  - `templates_changed` reloads a clean editor
  - reconnect re-lists
  - no NavigationService registered
- **SessionsService:** `activation_re_presents_the_session_view`.

## TDD evidence

- **RED.** I wrote the three test files first. `npx vitest run src/renderer/src/AgentsTreeViewModel.test.ts src/renderer/src/TemplateEditorViewModel.test.ts src/renderer/src/AgentsService.test.ts` gave `Test Files 3 failed (3)`: `Failed to load url ./AgentsService` / `./AgentsTreeViewModel` / `./TemplateEditorViewModel`. Then I implemented them in that order. The editor went green at 12/12, the tree at 8/8 and the service at 15/15, with no test edits beyond the NavigationService import path.
- **GREEN.** `npm test` gives 16 files / 160 tests passed. `npm run typecheck:web` and `npm run typecheck:node` are clean. A throwaway tsconfig that also includes the `*.test.ts` files (deleted afterwards) is clean too. All 13 `.mu` files compile, and `AgentsIcon` is embedded in RuntimeIcons.mu.js. I found no CR in any changed file. The untracked docs/plans and docs/specs files from before this task were not staged.

## Self-review / concerns

1. **Nothing was rendered.** As instructed, I did not run the Electron app. For Task 11's live check:
   - Whether the TreeView keeps `SelectedDataItem` when a selected template node moves to another parent collection, and whether the KeepSelection snap-back shows.
   - That the ComboBoxes show the `ChoiceItem.Label` and follow `SelectedItem` after a choices rebuild.
   - That the multi-line system prompt TextBox (`AcceptsReturn`, `MinHeight = 120`) looks right.
   - That the toolbar wraps sensibly in the side pane.
   - The rail Cancel, which writes `NavigationService.SelectedItem` back to the Agents item: the rail's Selector should follow it.
   - That the robot icon reads well at rail size.
2. **SessionsService gained `OnActivated`.** This is outside the brief's file list. It is needed because both capabilities share the one ContentHost, and without it the editor would stay on screen under the Sessions rail.
3. **Open shows the previous fields until `get_template` replies.** The window is short, the token guard drops stale replies, and edits typed in that window are overwritten by the reply. A draft or rename while the editor is dirty is guarded; a plain open is not.
4. **New template creates nothing on the server until Save.** The draft is not in the tree, and it counts as dirty, so switching away asks. This avoids writing an `untitled.toml` that might fail validation.
5. **Default tools for a new template are none.** The spec seeds `Default` with all tools; I chose none for a new template, so the user opts in.
6. **Delete clears the editor even when it is dirty.** If the open template is deleted (here or elsewhere), its unsaved edits are dropped when `templates_changed` arrives.

## For Task 11 (reuse)

- `AgentsService.Key`, `Panel: AgentsTreeViewModel` and `Editor: TemplateEditorViewModel`.
- `TemplateTreeNodeViewModel.Reconcile(target, nodes, TemplateTreeNodeViewModel.Pool(roots))` gives an in-place tree refresh.
- `ChoiceItem` (`app/src/renderer/src/ChoiceItem.ts`).
- Both `SessionsService` and `AgentsService` implement `IActivatable.OnActivated()` and re-present their host content on a rail switch.

---

# Fix round 1, commit `24dfce03 fix(app): task 10 review follow-ups`

## Important findings

1. **Sampling knobs are preserved.**
   - The Rust `AgentTemplate.sampling` is `Option<SamplingConfig>`, and its fields are NOT optional (`f32`/`u32`/`i64`; `#[serde(default)]` on the struct).
   - So `Build` now always starts from the loaded `sampling` (keeping top_k, repeat_penalty, seed, max_tokens and so on). Temperature and Top P set only their own knobs, and a blank field sends the worker default (`DefaultTemperature = 0.8`, `DefaultTopP = 0.95`, from `SamplingConfig::default`).
   - `sampling` is omitted only when the loaded template had none and both fields are blank.
   - Tests:
     - `blank_sampling_fields_keep_the_other_knobs` (renamed; with both fields blank it sends `{temperature: 0.8, top_p: 0.95, top_k: 20, repeat_penalty: 1.1, seed: 3, max_tokens: -1}`)
     - `no_sampling_is_sent_when_none_was_loaded_and_both_fields_are_blank`
   - The save and number-field tests now expect the default for the blank knob.
2. **A refresh can no longer swap the open template.**
   - `Open` records `templateId` and `path` synchronously. The new `IsLoading` flag makes the form read-only: `IsEditable` is false, the view binds `IsEnabled = $IsEditable`, and Save and Revert are off until the reply.
   - A reply is applied only if its token is the latest AND `record.id` equals the requested id.
   - `Reload(node)` ignores any id other than the one last requested, so the refresh path follows the latest request.
   - Tests:
     - `refresh_racing_an_open_shows_the_selected_template` (service): a refresh is in flight, the user selects Loose, `list_templates` is answered, every refresh-path read is for t3, and the editor ends on t3 / Loose, editable.
     - `a_later_open_wins_over_a_late_reply_for_an_earlier_one` (editor): the stale t1 reply is dropped, a Reload for t1 is ignored, and t3 is shown.

## Minor findings

1. **Rail Cancel.** After restoring `navigation.SelectedItem`, the service now also executes the restored `NavigationDestination`'s `ActivateCommand`. That points NavigationService's click tracking back at Agents and shows the side pane, so the next Sessions click is a plain switch. Test: `rail_cancel_reactivates_the_agents_destination`, using a real `NavigationDestination(new Capability())` with a counting command.
2. **Two labels.** `ChoiceItem.NotServed(value, runtime)` gives `X (not served by {runtime})` for a catalog model that the selected runtime cannot load. `X (not in catalog)` stays for values that are really absent. Test: `model_choices_filter_by_runtime` now expects `gemma-4 (not served by candle)`, and `model_outside_the_catalog_stays_a_marked_choice` still expects `(not in catalog)`.
3. **Deleting a dirty template.**
   - **From this app.** Deleting the open template while it is dirty asks Discard / Cancel first, through the same `MayDiscardEdits`, and only then shows the Delete confirm.
   - **Remotely.** When the open template disappears while dirty, `Editor.Orphan()` keeps the form as an unsaved draft: `IsNew`, `TemplateId` null, the same group. ErrorText reads `This template was deleted; Save recreates it.`, and Save creates it again with no id. A clean editor still clears.
   - Tests: `deleting_the_dirty_open_template_asks_discard_first` and `a_dirty_template_deleted_elsewhere_becomes_an_unsaved_draft`.
4. **Catalog failure.**
   - `CatalogError`/`HasCatalogError` is a persistent banner at the top of the editor view, also shown in the empty state. Open, OpenNew and Clear do not reset it.
   - `EnsureCatalog()` retries while the catalog is unknown and not already loading. It runs on Open, on OpenNew, and after every tree refresh (Refresh or `templates_changed`).
   - While the catalog is unknown, a missing value is listed plainly, with no "(not in catalog)" or "(not served…)" mark.
   - Tests: `catalog_failure_stays_visible_and_is_retried_on_the_next_open` (the error survives an Open, labels are unmarked, the retry succeeds and clears it, no retry once known) and `a_tree_refresh_retries_an_unknown_catalog`.
5. **Prop constants.** `FieldProps` is built from the `Prop*` constants, with `PropTemplateId` added. `TemplateTreeNodeViewModel.NodeProps` is built from the new `PropNode/PropName/PropPath/PropError/PropHasError`. `DerivedProps` has no constants of its own (that list is their only home).
6. **`OnSelectionLost` removed.** The tree still deselects a vanished node, and the service handles the editor in `OnTreeRefreshed` (renamed from `SyncEditor`). The tree test is now `selection_is_cleared_when_the_selected_template_disappears`.

**Owner-visible default.** A new template now starts with every `list_all_tools` tool enabled, as in the seeded Default. If the registry arrives after a pristine draft opened, the draft is completed then. Test: `new_template_is_an_unsaved_untitled_draft` checks that all checkboxes are on and the save sends `['read_file', 'get_time']`.

## Evidence

- **RED.** With the new and updated tests, and the five production files (`TemplateEditorViewModel.ts`, `AgentsService.ts`, `AgentsTreeViewModel.ts`, `ChoiceItem.ts`, `TemplateTreeNodeViewModel.ts`) reverted to 1e6f55ce, `npx vitest run` on the three test files gave **12 failed | 31 passed**. The failures were the save, model_choices, unparsable, blank_sampling, later_open, catalog_failure and new_template tests in the editor, and rail_cancel, deleting_dirty, deleted_elsewhere, refresh_racing and tree_refresh_retries in the service. Then I restored the files.
- **GREEN.** `npm test` gives 16 files / 168 tests passed. `npm run typecheck:web` and `npm run typecheck:node` are clean. A throwaway tsconfig that also includes the tests is clean too. All 13 `.mu` files compile, and I found no CR in any changed file.
- **Task 11 live check, additions.** The rail Cancel then a click on Sessions should switch normally, with the side pane staying visible. The catalog banner should show. The form should look disabled while a template loads.
