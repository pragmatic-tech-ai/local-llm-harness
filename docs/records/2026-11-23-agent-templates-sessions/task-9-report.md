# Task 9 report: Sessions capability (replaces Runtime)

Status: DONE_WITH_CONCERNS
Commit: `86425a3d feat(app): Sessions capability with session list, view and new-session dialog` (BASE 9282ce53)

## Built

- **Rail.** `views/SessionsModule.mu` (renamed from RuntimeModule.mu): module and capability `Name = "Sessions"`, the same `@RuntimeIcon`, the `supervisor.url` setting kept (only the description now says the runtime is found through it). In `app.mu`, `DataTemplate[SessionsService] { ContentPresenter Content=$Panel }` and the new resources are merged.
- **`SessionsService`** (`SessionsService.ts`, ServiceBase, Key `SessionsService`). It owns the single `RuntimeClient` (`public readonly Client`), `Panel: SessionsPanelViewModel` and `View: SessionViewModel`. The view is shown in the ContentHost from construction.
  - **Runtime URL.** Ruling: app-side, no backend change. It connects a `SupervisorClient` to `supervisor.url`, calls `list_harnesses`, and disconnects it. `SupervisorClient.RuntimeUrl(supervisorUrl, harnesses)` keeps the supervisor's scheme and host, takes the port from the first `chat_url`, and falls back to `8779`. If the supervisor is unreachable it still uses `:8779`. I did not add `runtime_url` to the supervisor. While nothing is loaded no harness exists, so `chat_url` is usually absent and the fallback is the normal path.
  - **Reconnect.** `JsonRpcConnection` gained `OnOpened` and `OnClosed`. `OnClosed` is raised only when the live socket closes on its own, never on `Disconnect`. On close the view detaches (the chat goes inert, the list stays) and a reconnect is scheduled at `min(1000·2^n, 10000)` ms. On open the backoff resets, the panel rebinds and refreshes, and `View.Attach(ShownId)` re-attaches the shown session. A settings change cancels the timer, disconnects, and clears the panel and view.
  - **Dialogs.** These are resolved lazily via `Provider.get(DialogService.Key)`.
    - New session uses `DialogService.Show` with Content `NewSessionDialogViewModel` and the Cancel / Create `DialogAction`s. Create's command is the VM's `CreateCommand`. Its `OnCreated` closes the dialog, calls `Panel.Show(summary)` (selects the row without raising `OnOpen`) and calls `View.Attach(id)`.
    - Rename uses `TextPromptViewModel.Ask` and then `RenameSession`.
    - Delete uses `DialogService.Confirm` and then `DeleteSession`.
    - The list follows each of these through `sessions_changed`.
- **`SessionsPanelViewModel`** has these members:
  - `Sessions: ObservableCollection<SessionRowViewModel>` and `SelectedSession`. A user set of a different, non-null row raises `OnOpen: TypedSignal<string>`; null or the same row is ignored.
  - `NewSessionCommand`, which can execute only while bound. `RenameCommand` and `DeleteCommand` take a row as the parameter, or else act on the selected row.
  - `StatusText`, `IsEmpty`, `Refresh()`, `Show(summary)`, `ClearSelection()` and `Find(id)`.

  Its signals are `OnSelectionLost`, `OnNewSessionRequested`, `OnRenameRequested` and `OnDeleteRequested`; the panel only signals and the service owns the dialogs. On `sessions_changed` it refreshes and reconciles rows by id in place. Order follows the server and row objects are reused, so the selection is kept and nothing is re-opened or re-attached. When the selected row disappears it raises `OnSelectionLost`, and the service clears the view if that row was the shown session. `placement_changed` maps to `row.SetPlacement` and `session_busy` maps to `row.SetBusy`.
- **`SessionRowViewModel`** (new file `SessionRowViewModel.ts`) has `Id`, `Title`, `TemplateName`, `StateText`, `StateKind`, `Busy`, `IsLive`/`IsIdle`/`IsUnavailable`/`IsBusy` (these drive the dots), and `RenameCommand`/`DeleteCommand` (for the context menu, which reports back through `ISessionRowOwner`). `StateKind` is the enum `SessionStateKind`: `ready`→`live`, `needs_load`→`idle`, `unavailable`→`unavailable`, and busy wins as `busy`. `StateText` is `live`/`idle`/`busy`/`unavailable: {reason}`.
- **`SessionViewModel`** is one stable instance. Its members:
  - `Chat` (the existing `ChatViewModel`) and `Panel` (`Chat.Panel`).
  - `ShownId`, `HasSession`/`IsEmpty` and `EmptyText`.
  - `HeaderText` (`{template} · {model} @ {host}`, or without the host when unavailable).
  - `PlacementText` (`live on {host}` / `model not loaded` / `unavailable: {reason}`, or a transient line: opening, loading, or an error).
  - `CanLoad`/`CanUnload`, `LoadLabel` (`Load model` / `Load model (evicts {evicts})`), `LoadCommand`/`UnloadCommand`, and `ToolsText` (the snapshot's tool subset or `tools: off`).
  - `Attach(id)`, `Detach()` and `Clear()`.

  Attach guards against stale replies with a token. When attached it binds the chat, loads the history, and seeds the panel from the effective `sampling`/`enable_thinking`. It follows `placement_changed` for the shown id. On a transition to ready it calls `Panel.RefreshModelProps()`.
- **`NewSessionDialogViewModel`** has `Roots` (TemplateTreeNodeViewModel), `SelectedNode` (two-way with the TreeView's `SelectedDataItem`), `Summary` (`Model: {model} ({runtime}, context {n_ctx})\nTools: …\nStep budget: N`, using `get_template`), `StatusText`, `CreateCommand` (enabled only for a template that has no error), `Load()` and `OnCreated`. It has a static `Describe(template)`. A broken template shows `This template cannot be used: {error}`.
- **Chat rework (`ChatViewModel`):**
  - Added `LoadHistory(messages)` and `Clear()`. In history, user messages become `UserMessage` and assistant content becomes `AssistantMessage`. Each `tool_calls[]` entry becomes a `ToolCallMessage` (with JSON arguments), and a `tool` message becomes a `ToolResultMessage` (its name is looked up by `tool_call_id`; `ok` is false only for the runtime's `{error}` object). System messages are hidden.
  - On `session_loading` it shows `[loading model…]`, and the reply opens below it.
  - Removed `ToolsText` and the model_props/list_tools refresh; the session header shows the tools instead.
  - Placeholder: `Select a session to chat`.
- **`Send` / sampling rework (`InstrumentPanelViewModel`).** It gained `ApplySession(sampling, thinking)`, `TakeSamplingChange()`, `TakeThinkingChange()` and `RefreshModelProps()`; `OnModelProps` was removed. `Send` now sends `set_sampling` / `set_thinking` only when the knobs differ from what the session runs with. Bind and a failed set reset that to unknown, so the next send re-applies them. The values persist per session server-side, so this replaces the per-send calls from Task 8.
- **Views.**
  - `views/SessionsPanel.mu`: a New session button, status and empty text, and a `ListBox` (`ItemTemplate=@SessionRow`, `SelectedItem=$SelectedSession`). Each row has `ContextMenuService.ContextMenu=@SessionContextMenu` (Rename / Delete) and four state dots (`@StateSuccess`/`@Fg3`/`@StateDanger`/`@StateWarning`).
  - `views/SessionView.mu`: the header over `ContentControl Content=$Chat`, plus the empty state.
  - `views/NewSessionDialog.mu`: a `TreeView` with `HierarchicalDataTemplate itemsselector=Children` and an IsExpanded item style, beside the summary.
  - `views/TextPrompt.mu`.
  - `Chat.mu`: the ToolsText line was dropped.
- **Removed (old flow).**
  - `HarnessPickerViewModel(.test).ts`, `views/HarnessPanel.mu` and `RuntimeManagementService(.test).ts` (their replacements are new files).
  - Everything in `SupervisorClient` except `ListHarnesses`/`HarnessInfo`: list_runtimes/list_models/start/stop/harness_changed.
  - In the renderer `main.ts`, the direct-backend fallback (`BackendUrl` → `BindBackend`). It now force-constructs `SessionsService` and seeds the setting.
  - The main-process `Sidecar` (`sidecar.ts`/test) and `backend-url.ts`/test, the `backend-url` IPC handler, and the preload `BackendUrl`. These only fed that direct-chat path: the sidecar spawned a plain worker, which now has no app consumer. `main/index.ts` now only owns the window, the settings IPC and `supervisor-url`.

## Tests

The brief's tests:
- `sessions_panel_lists_sessions_with_state` and `placement_changed_updates_row` are in SessionsPanelViewModel.test.ts.
- `selecting_row_attaches_and_shows_history`, `new_session_dialog_creates_and_opens`, `reconnect_reattaches_shown_session` and `deleted_shown_session_clears_view` are in SessionsService.test.ts.
- `load_button_label_names_eviction` is in SessionViewModel.test.ts.

Additional tests:
- **Panel:** busy row from `session_busy`, OnOpen raised once, refresh keeps the selection without reopening, `OnSelectionLost`, row commands signal, unbound panel.
- **Service:** discovery through the supervisor at `:8779`, empty url stays inert, sessions_changed after a turn does not re-attach, backoff sequence 1/2/4/8/10/10 s, rename prompt, a settings change reconnects and clears.
- **View:** header/tools, unavailable reason, load → ready (+model_props), load failure, unload + placement_changed, panel seeded from attach, stale attach dropped.
- **Dialog:** summary text, broken template, create failure.
- **Chat:** session_loading note, history mapping (calls/results/error result/system hidden), history replaces the transcript.
- **Panel knobs:** session values seed the knobs and are not re-sent, a failed set_sampling re-applies.
- **SupervisorClient:** RuntimeUrl port from chat_url and the fallback.

Updated: `KnobEditsApplyToNextTurnNotCurrent` now expects an unchanged thinking flag not to be re-sent, and the placeholder test has the new text. `tools_text_reflects_runtime` was removed along with ChatViewModel.ToolsText.

## TDD evidence

- **RED.** I wrote the implementation before these tests, which is a process deviation. I stand by the RED below, but it is not a strict test-first sequence. I ran the new tests against the BASE `ChatViewModel.ts` / `InstrumentPanelViewModel.ts` / `JsonRpcConnection.ts`, with the new modules missing: `npx vitest run ChatViewModel.test.ts InstrumentPanelViewModel.test.ts SessionsService.test.ts SessionViewModel.test.ts SessionsPanelViewModel.test.ts` gave **25 failed**, covering every SessionViewModel and SessionsService test, the chat session_loading/history tests, the panel seed/re-apply tests, `KnobEditsApplyToNextTurnNotCurrent` and the placeholder. Then I restored the files.
- **GREEN.** `npm test` gives 13 files / 110 tests passed. `npm run typecheck:web` and `npm run typecheck:node` are clean. A throwaway tsconfig that also includes the `*.test.ts` files (deleted afterwards) is clean too. I found no CR in any staged file. All 10 `.mu` files compile.

## Self-review / concerns

1. **Nothing was rendered.** As instructed, the Electron app was not run, so the `.mu` layout is unverified: the ListBox row template with its context menu, the four-dot state, the TreeView in the dialog, and the header buttons. All of it compiles, and it follows the Plexus patterns (library tree, conversations panel, target dialogs). The owner's Task 11 live check should look at the row context menu (whether `$RenameCommand` resolves against the row) and at whether the dialog's Create button follows `CanExecute`.
2. **Main-process deletion.** Deleting the sidecar and `HARNESS_BACKEND_URL` is wider than the renderer `main.ts`. I judged them old-flow-only, because the renderer no longer has a consumer for them. Without a supervisor url the app now opens inert, with no local fallback. The README's Phase-A status text still mentions the Sidecar, as history; I did not touch it.
3. **Discovery happens once per settings change.** The runtime url is resolved when the setting changes, and reconnects reuse it. A supervisor restart onto a different chat port would need a settings re-apply. The port is fixed at 8779 today.
4. **One-way refresh.** The panel's `IsEmpty` / `StatusText` are refreshed only on list events. Rename and delete errors are only logged with `console.error`, with no UI surface.

## For Task 10 (reuse, exact paths/signatures)

- **Shared connection.** Use `app/src/renderer/src/SessionsService.ts`: `SessionsService.Key`, and `public readonly Client: RuntimeClient`. Resolve it with `provider.getRequired(SessionsService.Key).Client`. It is already connected and reconnects on its own. Subscribe to `Client.OnTemplatesChanged`, and to `Client.OnOpened` (new on `JsonRpcConnection`, `TypedSignal<void>`) to re-list after a reconnect; `Client.OnClosed` also exists. Template methods (from Task 8): `ListTemplates`, `GetTemplate`, `SaveTemplate(id?, path?, template)`, `DeleteTemplate`, `CreateGroup`, `RenameGroup(path, newName)`, `DeleteGroup`, `MoveTemplate(id, path)`, `ListCatalog`, `ListAllTools`.
- **Tree nodes.** `app/src/renderer/src/TemplateTreeNodeViewModel.ts`: `new TemplateTreeNodeViewModel(node: TemplateNode)`, `static FromNodes(nodes): TemplateTreeNodeViewModel[]`, `static FindTemplate(roots: Iterable<…>, id): … | undefined`, and the getters `Node`, `Name`, `Path`, `IsGroup`, `IsTemplate`, `TemplateId: string | null`, `Error: string | null`, `HasError`, `Children: ObservableCollection<…> | undefined` (undefined for templates, i.e. a leaf), and `IsExpanded` (get/set, default true).
- **Tree binding pattern.** `app/src/renderer/src/views/NewSessionDialog.mu`: `HierarchicalDataTemplate x:key=… [ DataType = TemplateTreeNodeViewModel, itemsselector = Children ]`, an `ItemContainerStyle` with `IsExpanded = $IsExpanded`, and `TreeView [ ItemsSource=$Roots, ItemTemplate=@…, ItemContainerStyle=@…, SelectedDataItem=$SelectedNode, SelectionMode=Single ]`. Use distinct `x:key` names, because resources are merged app-global.
- **Text prompt.** `app/src/renderer/src/TextPromptViewModel.ts`: `static Ask(dialogs: DialogService, prompt: ITextPrompt): Promise<string | undefined>` with `ITextPrompt { Title, Label, Text, ConfirmLabel }`. It resolves to the trimmed text, or undefined on cancel or blank. Its view is `views/TextPrompt.mu` (already merged in app.mu). Use it for new group and rename group.
- **Dialog pattern.** See `SessionsService.OpenNewSessionDialog`. It uses `Provider.get(DialogService.Key)` and `dialogs.Show({ Title, Content: vm, Width, Actions: [new DialogAction(label, cmd), new DialogAction(label, vm.Cmd, ButtonVariant.Filled)] })`, and closes from a VM signal with `dialogs.Close(result)`. Use `dialogs.Confirm({ Title, Message, ConfirmLabel })` for deletes.
- **Template summary.** Use `NewSessionDialogViewModel.Describe(template: AgentTemplate): string` if the editor wants the same summary line.
- **Test rig.** `SessionsService.test.ts` has `FakeDialogs` (with `Show`, `Close` and `Confirm`) and a `StubProvider` that implements both `get` and `getRequired`. Copy them; FakeSocket already serves the RuntimeClient.
- **Module wiring.** Add a second `shell module` `.mu` file the same way as `views/SessionsModule.mu`. Register it in `app.mu`'s `.modules`, and add `DataTemplate[AgentsService] { ContentPresenter Content=$Panel }` to app.mu's resources. It needs a new icon in `views/RuntimeIcons.mu` (`include "icons/….svg" as …`).

---

# Fix round 1, commit `8758a56e fix(app): task 9 review follow-ups`

## Important findings

1. **Reorder no longer drops the selection.**
   - `SessionsPanelViewModel.Reconcile` now works in place. Existing rows are updated, then moved with `Move` or inserted with `Insert` into the server's order, and leftover tail rows are removed with `RemoveAt`. There is no `Batch`, `Clear` or `reset` any more.
   - A null or undefined write from the list (for example from a Selector clearing on a container rebuild) is ignored, and `SelectedSession` is re-announced on a microtask. A refused switch is handled the same way, so a two-way list shows the kept row again.
   - Tests:
     - `reorder_after_sessions_changed_keeps_selection_without_reattach` (service): the change events are exactly `['moved']`, the selection stays the same object, and there is one `attach_session`.
     - `reorder_moves_rows_in_place_without_reset` (panel): no reset or cleared event, and row identity is kept.
     - `cleared_or_refused_selection_is_reannounced`.
2. **No switching away from a running turn.**
   - `ChatViewModel` now has `IsBusy` and `OnBusyChanged: TypedSignal<boolean>`, driven by a single `SetBusy`. The service feeds this into `Panel.SetSwitchLocked(busy)`.
   - While the panel is locked, a click on another row keeps the shown row (re-announced so the list snaps back), shows `Finish the running turn before switching sessions.`, and sends no attach. Unlocking clears that notice.
   - Test: `switch_is_refused_while_the_shown_session_runs_a_turn`. The selection is kept, the notice shows and there is one attach; once the turn settles the switch to s2 attaches.
3. **Command failures reach the panel.**
   - `Panel.ReportError(text)` writes to `StatusText`.
   - Rename, delete and dialog-open failures report `rename failed: …`, `delete failed: …` and `could not open the dialog: …`. This covers the prompt, the confirm and the RPC.
   - Refresh failures already went to `StatusText`. Load, unload and attach failures show in the session header.
   - Tests: `rename_and_delete_failures_show_on_the_panel` (an RPC error, and a socket drop during delete) and `report_error_shows_on_status_line`.

## Minor findings

1. **Placement state is shared.** `PlacementState` is a new enum in `agents.ts`, and `Placement.state` is now typed with it. `PlacementText.ts` provides `Unavailable(p)`, `IsReady(p)` and `NeedsLoad(p)`, and both the row and the view use it; the per-class `PlacementReady`/`PlacementNeedsLoad` constants are gone.
2. **One home for the text helpers.** `TextFormat.ts` provides `TextFormat.Format(template, ...values)` and `TextFormat.Message(e)`. ChatViewModel, InstrumentPanelViewModel, NewSessionDialogViewModel, SessionViewModel, SessionRowViewModel (through PlacementText), SessionsPanelViewModel and SessionsService all use it. No private `Format`/`Message` copies and no single-token `.replace` calls remain, and the `(e: Error)` catch sites now use `Message`.
3. **Safe rejection text in the panel.** `SessionsPanelViewModel` now uses `TextFormat.Message(e)`. Test: `refresh_failure_shows_on_status_line`.
4. **Test placements are class constants.** In the panel test they are `Rig.Ready/NeedsLoad/Unavailable` (`static readonly`); the service test gained `Rig.Ready/NeedsLoad`.
5. **Blank lines.** The stray blank lines in InstrumentPanelViewModel.test.ts are removed.
6. **Connection state is visible.**
   - `ConnectionState.ts` defines the enum `Connected | Reconnecting | Disconnected`. `SessionsService` exposes `Connection` and `OnConnectionChanged`. Reconnecting covers discovering, connecting and waiting to retry; Disconnected means the url is empty.
   - The panel has `SetConnection`, `EmptyText` (`No sessions yet.` / `Connecting to the runtime…` / `Not connected: set the supervisor URL in Settings.`) and `ShowsEmptyText`; SessionsPanel.mu binds both.
   - In the view, `SetStale(bool)` makes `PlacementText` read `{text} (stale: not connected)` and turns Load and Unload off; `IsStale` is exposed.
   - Tests: `connection_state_shows_on_panel_and_marks_placement_stale`, `empty_text_follows_connection_state` and `stale_marks_placement_and_disables_load`.
7. **Discovery has a timeout.** `DiscoverRuntimeUrl` races `list_harnesses` against a 3 s timer. On a timeout or an error it falls back to `:8779`. Discovery now runs on every connection attempt, which also resolves my concern 3, and a failed attempt keeps the same backoff. Test: `silent_supervisor_times_out_to_the_default_port_and_retries_discover_again`. The service test rig's supervisor socket answers by default and can be made silent.
8. **Load is disabled during a first-message load.** `SessionViewModel` follows `session_loading` for the shown id and sets `turnLoading` until `turn_started` or `loop_done`. While it is set, `CanLoad`/`CanUnload` are false and `PlacementText` reads `loading model…`. Test: `first_message_load_disables_load_until_the_turn_starts`, which also checks that another session's `session_loading` is ignored.
9. **README.** The Sidecar line is marked as removed in this slice.

Minor 10 is a process note only and needed no change.

## Evidence

- **RED.** With the production files reverted to 86425a3d and the new tests kept, `npx vitest run` gave 3 test files failed and 10 tests failed. SessionsPanelViewModel.test and SessionsService.test failed to load because `PlacementState` and `ConnectionState` were missing, and SessionViewModel had 10 failures. Then I restored the files.
- **GREEN.** `npm test` gives 13 files / 122 tests passed. `npm run typecheck:web` and `npm run typecheck:node` are clean. A throwaway tsconfig that also includes the tests is clean too. I found no CR in any staged file, and all `.mu` files compile.

## For Task 10, in addition to the earlier list

- `SessionsService.Connection`, plus the `OnConnectionChanged: TypedSignal<ConnectionState>` signal from `ConnectionState.ts`.
- `TextFormat.Format` / `TextFormat.Message` from `TextFormat.ts`.
- `PlacementState` from `agents.ts`.
- `PlacementText` from `PlacementText.ts`.

---

# Fix round 2, commit `a50060ed fix(app): no new session opens over a running turn`

- **New session is disabled during a turn.** `NewSessionCommand` can execute only while the panel is bound and not switch-locked (`client !== null && !switchLocked`). `SetSwitchLocked` raises `CanExecuteChanged`, so the button turns off while the shown session runs a turn and back on when the turn settles.
- **A session created during a turn is not opened.** This covers a dialog that was already open when the turn started. `SessionsService.OpenCreated` first checks `View.Chat.IsBusy`. If a turn is running, it lists the new session with the new `Panel.Add(summary)`, which adds or updates the row without selecting it. It then calls `Panel.ReportSwitchRefused()`, which shows the same `Finish the running turn before switching sessions.` notice, and returns without `Show` or `Attach`. The dialog still closes, because the create succeeded.
- **Panel changes.** I added `IsSwitchLocked`, `ReportSwitchRefused()` and `Add(summary)`. `Show(summary)` is now `Add` plus selecting the row.
- **Tests.**
  - `new_session_is_disabled_while_the_shown_session_runs_a_turn`: CanExecute is true, then false during the turn, then true after `loop_done` and the `send_turn` response.
  - `session_created_during_a_turn_is_listed_but_not_opened`: the turn starts while the dialog is open, then Create runs. The dialog closes, the new row is listed, the selection and the view stay on s1, there is one attach, the notice shows, and the turn is still busy.
- **RED.** With `SessionsPanelViewModel.ts` and `SessionsService.ts` reverted to 8758a56e, `npx vitest run src/renderer/src/SessionsService.test.ts` gave 2 failed (the two tests above) and 15 passed.
- **GREEN.** `npm test` gives 13 files / 124 tests passed. `npm run typecheck:web` and `npm run typecheck:node` are clean, and I found no CR in the staged files.
