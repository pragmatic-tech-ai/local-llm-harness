### Spec Compliance (✅ / ❌ with file:line; ⚠️ cannot verify)

Line numbers are lines of the new files, as given by the diff hunks.

**Overall: ✅ compliant.** Every brief interface and every controller ruling is implemented. The defects below are quality findings, not missing requirements.

**Rail and wiring**
- ✅ Agents rail module, icon and wiring. `views/AgentsModule.mu:3-8` declares `Name = "Agents"`, `Icon = @AgentsIcon` and `ServiceKey = AgentsService`. `RuntimeIcons.mu:4` includes the icon. `app.mu` adds the module to `.modules`, merges the two resource dictionaries and adds `DataTemplate[AgentsService] { ContentPresenter Content=$Panel }`.
- ✅ The connection is shared, with no second socket. `AgentsService.ts:69-72` uses `provider.getRequired(SessionsService.Key).Client`. Lines 75 and 99-111 follow `OnConnectionChanged`.

**Template tree**
- ✅ The tree has `Roots`, `Selected` and the commands NewTemplate, NewGroup, Rename, Delete, MoveTo and Refresh (`AgentsTreeViewModel.ts:9-37, 56-61`).
- ✅ The recursive node exposes `Name`, `IsGroup`, `Children` and `Error` (`TemplateTreeNodeViewModel.ts`).
- ✅ The tree refreshes on `templates_changed` (`AgentsTreeViewModel.ts:76`).
- ✅ Ruling (refresh): a refresh keeps the selection and the expansion. `TemplateTreeNodeViewModel.Reconcile` reuses nodes by key across the whole tree, and the test `templates_changed_refreshes_tree` asserts both.
- ✅ Selecting a template opens the editor in the content host (`AgentsService.ts:112-127`).

**Template editor**
- ✅ The editor has every brief field: `Name`, `SystemPrompt`, `StepBudgetText`, `RuntimeChoices`/`Runtime`, `ModelChoices`/`Model`, `ContextSizeText`, `EnableThinking`, `TemperatureText`/`TopPText`, `HostChoices`/`Host` and `Tools`.
- ✅ It has `SaveCommand`/`RevertCommand`, `IsDirty`, and an `ErrorText` set from the `save_template` message (`TemplateEditorViewModel.ts:473-505`).
- ✅ The tool checkboxes come from `list_all_tools`, as the "Editor data" settled detail requires (`TemplateEditorViewModel.ts:394-411, 687-708`).
- ✅ Ruling (broken template): it opens from the defaults plus the node's name, runtime and model, with `ErrorText` set to the node's error. Save is allowed while clean and repairs the file (`TemplateEditorViewModel.ts:155, 539-583`).
- ✅ Ruling (dirty editor): switching away asks Discard / Cancel. This covers another template (`AgentsService.ts:119-123`), New template (line 152) and the rail (lines 340-370).
- ✅ Ruling (number fields): a field that fails to parse gets a field error and disables Save, and `Build()` returns null, so NaN is never sent (`TemplateEditorViewModel.ts:626-668, 739-760`).
- ✅ Ruling (move to group): Move to opens a group-picker dialog with "(root)" first (`AgentsService.ts:266-301`, `MoveToDialogViewModel.ts`, `AgentsTreeViewModel.ts:195-200`).
- ✅ Ruling (catalog): model choices are filtered by runtime, and a value the catalog lacks is kept and marked (`TemplateEditorViewModel.ts:676-685, 710-714`). The label wording has a nuance, see Minor 2.
- ✅ A new template is `Untitled` in the selected group, and a new group is `New group`.

**Tests**
- ✅ All 8 test names from the brief are present: in AgentsTreeViewModel.test, TemplateEditorViewModel.test and AgentsService.test.
- ✅ There are extra tests for each ruling.

**Spec scope**
- ✅ Spec "moved by drag or a Move to… command" is satisfied by the Move to command alone.

**Cannot verify**
- ⚠️ Nothing was rendered: the TreeView keeping `SelectedDataItem` when a node moves to another parent collection, the snap-back, and how the toolbar wraps. This is deferred to the Task 11 live check, as the report says.

### Strengths

**Tree and state handling**
- In-place tree reconciliation keyed across the whole tree is the right design. A template moved between groups keeps its VM object, so its selection survives. There is no reset event.
- The editor follows a move of the open template (`SyncEditor` → `SetPath`). Without that, the next save of a dirty editor would silently move the template back. The test `move_to_lists_the_groups_and_moves` pins this.
- Dirty tracking uses a fingerprint taken at fill or save time. A save that is in flight records the fingerprint that was sent, so edits typed during the save stay dirty.

**Layering and dialogs**
- The signal-only seam is clean. The tree only raises requests, and the service owns every dialog and RPC. There are no upward calls.
- Command failures all reach the status line with a prefix such as `delete failed: …`, so nothing is swallowed.

**Verified against the framework**
- `ComboBox` displays `ChoiceItem.Label` without a template: `combo-box.js` `displayString` prefers `.Label`.
- `NavigationService.syncActiveService` raises `ActiveService` and then calls `OnActivated()`. So the shared ContentHost really needs `SessionsService.OnActivated`, and the rail guard subscribes to the right property.

**Style**
- No semicolons, no non-Allman braces and no module-level functions in the new TS files.
- Literals are hoisted to constants.
- The VMs extend Observable.

### Issues

#### Critical

None.

#### Important

**1. Clearing Temperature and Top P deletes the template's other sampling knobs**

Where: `TemplateEditorViewModel.ts:648-663` (`Build`) and the test `blank_sampling_sends_no_sampling_and_keeps_other_knobs` (`TemplateEditorViewModel.test.ts` ~232-244).

What is wrong:
- When both fields are blank, `sampling` is dropped altogether. That throws away `top_k`, `repeat_penalty`, `seed` and `max_tokens`, which the editor does not show.
- This contradicts the code's own comment ("The knobs this editor does not show ... are kept as loaded") and the report's claim.
- The test codifies the loss while its name says "keeps_other_knobs".

Why it matters: the spec keeps hand edits of template files possible. A user who blanks two fields silently loses the hand-set seed and top_k on Save. That is data loss with no signal.

Fix:
1. Always start from `{ ...this.sampling }` and delete `temperature` and `top_p`.
2. Add each back only when it is set.
3. Omit `sampling` only when the resulting object has no keys.
4. Fix the test so that clearing both fields sends `{top_k: 20, repeat_penalty: 1.1, seed: 3, max_tokens: -1}`, and add a separate case where only temperature and top_p were loaded and `sampling` is omitted.

**2. A refresh during an open can show the wrong template**

Where: `AgentsService.ts:130-148` (`SyncEditor`) and `TemplateEditorViewModel.ts:358-371, 514-537` (`Reload` / `Load`).

What is wrong:
- `Open(B)` changes no state until `get_template` replies. In particular, `Editor.TemplateId` stays A.
- Suppose a `templates_changed` refresh is in flight when the user clicks B. Its `ListTemplates` reply lands before B's `get_template` reply, and `SyncEditor` calls `Reload(A)`.
- `Reload(A)` bumps `openToken`, so B's reply is dropped as stale.
- The tree then shows B selected while the editor holds A. Saving writes A, and the user believes they are editing B.
- The same gap causes report concern 3: the previous template's fields stay editable until the reply, then are overwritten.

Why it matters: the tree selection and the edited template diverge silently. Hand edits plus Refresh, or a second window, make this window reachable.

Fix: make the open intent synchronous. Any one of these works:
- `Open` immediately sets `templateId`, `path` and a "loading" flag, disabling the form and Save until the reply, so `SyncEditor` targets B.
- Or `SyncEditor` skips while an open is pending.
- Or `Reload` uses its own token, so it never cancels a pending `Open` of a different id.

Add a test: refresh in flight → select B → answer `list_templates` → answer `get_template(B)` → the editor shows B.

#### Minor

**1. After a rail Cancel, the next Sessions click also hides the side pane**

Where: `AgentsService.ts:356-370` (`ConfirmLeave`).

What is wrong:
- Cancel writes `navigation.SelectedItem = back`, but `NavigationService._lastActivated` stays on the Sessions destination, because the click's `ActivateCommand` already ran (`navigation-item.js:52-57`).
- The next click on Sessions switches the rail and then hits `dest === _lastActivated`, which toggles the side pane hidden.

Fix: after restoring, also execute `back.ActivateCommand` when `back` is a `NavigationDestination`. That sets `_lastActivated` and shows the side pane, and needs no private access. Add it to the Task 11 live check either way.

**2. "(not in catalog)" also labels a model that is in the catalog**

Where: `TemplateEditorViewModel.ts:676-685, 710-714` and `ChoiceItem.ts`.

What is wrong: after a runtime switch, a model that is in the catalog but not served by the new runtime is labelled `gemma-4 (not in catalog)`, and the test `model_choices_filter_by_runtime` asserts this.

Why it matters: the label is false, and Save will then fail backend validation.

Fix: use a second marker for "in the catalog, but not served by this runtime", for example `gemma-4 (not served by candle)`. Keep "(not in catalog)" for values that really are absent.

**3. Deleting the open template drops its unsaved edits silently**

Where: `AgentsService.ts:138-141` (implementer concern 6).

What is wrong:
- When the delete comes from this panel, the confirm text does not mention the unsaved edits.
- When it comes from elsewhere (another window, or a hand delete then Refresh), the edits vanish with no notice.

Fix: if the editor is dirty when its template disappears, keep the form as a draft (`IsNew`, same path) with `ErrorText` = "This template was deleted; Save recreates it." Otherwise clear it. At a minimum, add "Unsaved changes will be lost" to the delete confirm when the target is the dirty open template.

**4. A catalog failure is effectively invisible and never retried**

Where: `TemplateEditorViewModel.ts:332-355` (`LoadCatalog`).

What is wrong:
- The error goes to `ErrorText`, which is hidden while no template is open and is cleared by the next `Open`.
- `catalog` then stays null until a reconnect. Every runtime, model and host then shows as "(not in catalog)", and new drafts get an empty runtime and model.

Fix: keep a separate `CatalogError` that `Open` and `Clear` do not reset, or retry `LoadCatalog` on Refresh or on Open while `catalog === null`.

**5. Property-name lists repeat the literals they should reference**

Where: `TemplateEditorViewModel.ts:49-50` and `TemplateTreeNodeViewModel.ts` (`NodeProps`).

What is wrong: `FieldProps` re-types `'Name'`, `'SystemPrompt'` and the others, which already exist as the `Prop*` constants. `NodeProps` has the same pattern.

Why it matters: the global "one home, repeated keys can't drift" rule.

Fix: build `FieldProps` from the `Prop*` constants (add `PropTemplateId`), and the same for `DerivedProps`/`NodeProps` where constants exist.

**6. `OnSelectionLost` on the tree VM has no production subscriber**

Where: `AgentsTreeViewModel.ts:27, 241`.

What is wrong: `AgentsService` handles a vanished template through `OnRefreshed` → `SyncEditor`, so only tests consume this signal.

Fix: drop it, or have the service use it in place of the `Find` check.

**Implementer concerns, assessed**
- **`SessionsService.OnActivated` outside the brief's file list.** Accepted. NavigationService calls `OnActivated` on every switch, and the shared ContentHost needs it. It is minimal and tested by `activation_re_presents_the_session_view`.
- **A new template is an unsaved draft with no tools.** Accepted. The spec is silent on new-template defaults, and opt-in tools is the safer choice. The owner may prefer parity with the seeded `Default` (all tools), so mention it in the hand-off.
- **Deleting the open template clears the editor even when it is dirty.** See Minor 3.
- **The icon file is lower-case `agents.svg`.** Accepted. It matches the repo's `runtime-play.svg` and `settings.svg`, and the spec only asks for a placeholder in that style.
- **The views were not rendered.** I read the `.mu` files against the existing patterns:
  - The TreeView, HierarchicalDataTemplate and ItemContainerStyle follow `NewSessionDialog.mu`, with distinct `x:key`s.
  - The ListBox uses an ItemTemplate.
  - The ComboBox label works through `displayString`.
  - `Checkbox`, `Placeholder`, `AcceptsReturn` and `HorizontalScrollEnabled` are already used in `Chat.mu`, `InstrumentPanel.mu` and `SettingsResources.mu`.
  - I found no structural problems. The remaining risk is live-only (Task 11).

### Assessment

**Task quality:** Needs fixes

**Reasoning:** The structure, the rulings and the test coverage are solid. However, Save drops hand-set sampling knobs when both visible fields are blank, and a refresh racing an open can leave the editor on a different template than the one selected. Both must be fixed, along with the six Minors, before this task is trusted.

## Re-review round 1

Scope: fix diff 1e6f55ce..24dfce03, checked against the controller rulings. Tests and typechecks were not re-run.

- Sampling: `Build` now starts from the loaded knobs and sends the default for a blank field. `sampling` is omitted only when none was loaded and both fields are blank. This follows the ruling. Fixed.
- Open/refresh race: `Open` records `templateId` and `path` synchronously and sets `IsLoading`. The form is read-only and Save is off until the reply. A reply is applied only if its token is the latest and its id matches the requested id. `Reload` ignores other ids. The latest request wins. Fixed.
- Rail Cancel: after restoring the selection, `ActivateCommand` is executed. The `leaving` guard prevents re-entry. Fixed.
- Two labels: `ChoiceItem.NotServed` gives "(not served by {runtime})". "(not in catalog)" stays for values that are really absent. Fixed.
- Delete of a dirty template: a local delete asks Discard / Cancel first. A remote delete calls `Orphan()`, which keeps the edits as a draft with a notice. Fixed.
- Catalog failure: a persistent `CatalogError` banner, retried through `EnsureCatalog` on Open, OpenNew and every refresh. Missing-value marks are suppressed while the catalog is unknown. Fixed.
- `Prop*` constants: used for `FieldProps` and `NodeProps`. Fixed.
- `OnSelectionLost`: removed. Fixed.
- New templates start with all tools enabled, and a pristine draft is completed when the registry arrives later. Fixed.

No regressions found.

Out of scope, minor: `IsEditable` is false while saving, so the whole form is disabled during a save. This is acceptable but worth a glance in the Task 11 live check.

**Task quality:** Approved
