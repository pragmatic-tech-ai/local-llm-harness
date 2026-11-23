# Task 9 review: Sessions capability (9282ce53..86425a3d)

### Spec Compliance (✅ / ❌ with file:line; ⚠️ cannot verify)

- ✅ Rail: `views/SessionsModule.mu` declares module and capability `Name = "Sessions"`, the same `@RuntimeIcon`, and keeps the `supervisor.url` setting. `app.mu` registers `SessionsModule` and has `DataTemplate[SessionsService] { ContentPresenter Content=$Panel }`.
- ✅ `SessionsService` owns the single `RuntimeClient` (`public readonly Client`) for Task 10. Backoff is `min(1000·2^n, 10000)`, so 1/2/4/8/10/10 s, which `backoff_caps_at_ten_seconds` tests. `OnConnected` re-attaches `View.ShownId` (SessionsService.ts:228-238).
- ✅ Runtime URL: the supervisor URL keeps its scheme and host and takes the port from the first `chat_url`, else 8779 (SupervisorClient.ts `RuntimeUrl`). I checked that `chat_url` really is the runtime port (backend/src/supervisor/manager.rs:287, "Port of the agent runtime server, advertised as `chat_url`"). No `runtime_url` field was added, as the brief allowed.
- ✅ Panel: `Sessions`, row `Title/TemplateName/StateText/StateKind/Busy`, `SelectedSession` raises `OnOpen`, and `NewSessionCommand/RenameCommand/DeleteCommand` are all present. The panel refreshes on `sessions_changed` and updates rows on `placement_changed` and `session_busy` (SessionsPanelViewModel.ts:51-71). StateKind maps ready→live, needs_load→idle, unavailable→unavailable, and busy wins (SessionRowViewModel.ts:84-98).
- ✅ Controller decisions:
  - `OnSessionBusy` drives row Busy and StateKind.
  - A refresh reuses rows by id and does not re-attach. This holds at VM level; see Important 1 for the view level.
  - The panel is seeded from attach's effective `sampling`/`enable_thinking` (SessionViewModel.ts Attach → `Panel.ApplySession`).
  - The old flow is deleted.
- ✅ SessionViewModel:
  - It wraps the existing `ChatViewModel` and `InstrumentPanelViewModel`.
  - `HeaderText` reads `template · model @ host`. `PlacementText`, `CanLoad`, `LoadLabel` (`Load model` / `Load model (evicts X)`), `LoadCommand` and `UnloadCommand` (`unload_session_model`, the "Unload model" header command) are all present, plus a tools indicator built from the snapshot.
  - History maps to `UserMessage`, `AssistantMessage`, `ToolCallMessage` and `ToolResultMessage`, with system hidden (ChatViewModel.ts `HistoryItems`). The `tool_result` field matches backend `ChatMessage` (engine/types.rs:52-61).
- ✅ NewSessionDialogViewModel:
  - It has the TreeView `Roots`, a `Summary` (model, runtime, ctx, tools, budget) and `CreateCommand` (template-only, not broken, not creating).
  - It is shown via `DialogService.Show`, and `OnCreated` closes the dialog, then `Panel.Show` and `View.Attach`.
- ✅ `session_loading` puts `[loading model…]` in the chat, and the reply opens below it (ChatViewModel.ts `OnSessionLoading`).
- ✅ All 7 brief test names exist. `npm test` / typecheck results are taken from the report and were not re-run.
- ⚠️ .mu rendering not run. I read it against mural/Plexus patterns:
  - The ListBox `SelectedItem` is two-way by default (selector.js:17), and TreeView `SelectedDataItem` is two-way by default (tree-view.js:99).
  - `HierarchicalDataTemplate itemsselector=Children` matches the compiler's requirement (compiler.js:1111).
  - The dialog's Create button follows CanExecute. `DialogActionTemplate` binds `Button Command=$Command` (surfaces.template.mu:95-101), CommandSource listens to CanExecuteChanged (command-source.js:36-45), and the VM raises it on selection and on create.
  - The row context menu (`ContextMenu x:key` + `$RenameCommand` against the row DataContext) is the same pattern as Plexus `LibraryContextMenu` (library.resources.mu:14-19).
  - `Border Fill`, `ToVisibility`, `Variant = Tonal` and `TextTrimming` all exist in mural or in sibling app views.

### Strengths

- Clean separation of roles:
  - The panel and rows only signal.
  - The service owns the dialogs and the connection.
  - The view is one stable instance (it suits the view-reusing ContentControl).
  - Stale-reply tokens are used everywhere they are needed (attach, refresh, template read, settings generation).
- `JsonRpcConnection.OnClosed` fires only on a spontaneous close of the live socket, never on `Disconnect`. That keeps the settings-change path from spawning reconnects.
- `TakeSamplingChange`/`TakeThinkingChange` replace Task 8's per-send `set_sampling`/`set_thinking`. They respect per-session persisted overrides and reset to unknown after a failed set, and both behaviours are tested.
- Good test breadth beyond the brief: backoff sequence, stale attach, settings reconnect, broken template, create failure, history mapping incl. error results.
- Style is clean in all added TS. There are no semicolons, the braces are Allman, literals are hoisted, and I found no module-level functions/let in production code and no CRs.

### Issues

#### Critical

None.

#### Important

1. **SessionsPanelViewModel.ts:173-199 (`Reconcile`): a reorder wipes the ListBox's visual selection, and the VM never restores it.**
   - **What happens:**
     - Any change in order triggers `Sessions.Batch(Clear + Add…)`, and `Batch` emits `reset` (observable-collection.js:69-80).
     - On reset, `ItemsControl.rebuildContainers` calls `ClearContainerForItemOverride` for every container (items-control.js:562-575). In `Selector` that drops the item from `_selectedData` and pushes `SelectedItem = undefined` (selector.js:306-321, 333-340).
     - The two-way binding hands `undefined` to `SelectedSession`. The setter ignores null and undefined (line 86-94), so `_selected` stays, but no PropertyChanged is raised. The rebuilt containers are therefore never re-selected (selector.js `syncContainerSelectionFromData`).
   - **Why it matters:** the list is ordered newest `updated` first, and `sessions_changed` fires after every committed turn. Chatting in any session that is not already at the top moves it to index 0 and un-highlights the open row. That is exactly the "refresh must keep the selection" decision, and the VM-level test (`sessions_changed_refresh_keeps_selection_without_reopening`) cannot see it.
   - **Fix:** reconcile with `RemoveAt`/`Insert`/`Move` (ObservableCollection has `Move`, observable-collection.d.ts:47; ItemsControl's `moved` path keeps the container) instead of Clear+Add. Or, at minimum, re-raise `SelectedSession` PropertyChanged after the batch so the binding re-pushes the row. Add a test asserting that the collection emits no `reset` when only the order changes.

2. **SessionsService.ts:63 / SessionViewModel.ts:165 (`Attach`): selecting another row while a turn runs silently kills that turn.**
   - **What happens:**
     - The app has one connection, and `attach_session` replaces the earlier attachment.
     - Per Task 6's ruling, a turn with no attached connection left fails its next note with `SinkClosed` and aborts uncommitted ("client connection closed").
     - `Chat.Bind(null)` adds `[disconnected]` to the old transcript, then `Reset` clears it. The user gets no warning, and the work is lost on the server.
   - **Why it matters:** clicking around the list during a long tool loop is ordinary browsing, and the result is silent data loss.
   - **Fix:** while `View.Chat` has a turn in flight, either keep the selection (refuse the switch and show a status line) or ask via `DialogService.Confirm` ("Switching stops the running turn"). The controller may prefer a different ruling, such as per-session connections, but the app must not drop the turn silently. Add a test either way.

3. **SessionsService.ts:132, 151: rename/delete failures are only `console.error`ed.**
   - **What happens:** a failed `rename_session`/`delete_session` (e.g. `unknown session`, a dropped socket) shows nothing in the UI, and the list simply does not change.
   - **Why it matters:** these are swallowed errors in a GUI. The implementer flags this as concern 4.
   - **Fix:** surface them on the panel (e.g. a `Panel.ReportError(text)` that sets `StatusText`, which the panel view already shows). Add a test.

#### Minor

1. **Duplicated placement logic:**
   - `SessionRowViewModel.ts:23` and `SessionViewModel.ts:15` each hoist `PlacementReady`/`PlacementNeedsLoad` string constants.
   - Each class also builds its own "unavailable: {reason}" text.
   - Fix: add one `PlacementState` enum next to `Placement` in agents.ts, per the enums-over-string-literals rule.
2. **Repeated private static `Format`/`Message` helpers** in NewSessionDialogViewModel, SessionViewModel and ChatViewModel, plus single-token `.replace` in SessionRowViewModel/SessionsPanelViewModel/SessionsService. The pattern already existed, but it now has five copies. Consider one shared static text-format class.
3. **SessionsPanelViewModel.ts:128 uses `(e as Error).message`**, while the other new VMs use a safe `Message(e)`. A non-Error rejection would show `undefined`.
4. **SessionsPanelViewModel.test.ts:57-59: module-level `const Ready/NeedsLoad/Unavailable` objects.** Move them to `static readonly` on `Rig`, as NewSessionDialogViewModel.test.ts does with `Rig.Tree`.
5. **InstrumentPanelViewModel.test.ts:241-253: a stray blank line was added before the closing `})`, plus a trailing blank line at EOF.**
6. **No connection-state feedback:**
   - While the runtime is down (or `supervisor.url` is empty), the panel shows "No sessions yet." and the header keeps its last placement ("live on local").
   - Fix: show a reconnecting/disconnected `StatusText` from `OnClosed`/`OnOpened`.
7. **SessionsService.ts:185 (`DiscoverRuntimeUrl`) has no timeout.** A supervisor that accepts but never answers `list_harnesses` leaves the app inert. Discovery also runs only once per settings change (implementer concern 3). Add a short timeout that falls back to :8779.
8. **During a first-message load, `CanLoad` stays true.** `session_loading` is chat-only, and the header still offers "Load model", so the user can issue a concurrent `load_session_model`. Have `SessionViewModel` follow `OnSessionLoading` for the shown id and treat it as `loading`.
9. **README.md:23 still lists "Electron `Sidecar` that spawns/stops the backend binary"** in the status section. Mark it as removed in this slice.
10. **Process (implementer concern 1): the implementation was written before the tests.** The RED is credible: the new tests were run against BASE, with 25 failing for the right reasons, and the tests are specific and assertive. This does not lower trust in the result, but it is noted as a deviation from test-first.

**Assessment of the implementer's concerns:**

- **(1) Tests after implementation:** a process deviation. RED was shown against BASE, so this is Minor 10.
- **(2) Deleting the sidecar, backend-url, the IPC handler and the preload `BackendUrl`:** correct. The sidecar spawned a plain worker that speaks only the removed unattached-chat protocol (spec "The old flow is removed"; chat without attach fails `no session attached`). Keeping it would be dead code. Only the README line is stale (Minor 9).
- **(3) Runtime URL resolved app-side:** this is within the brief's ruling, and the `chat_url` semantics were verified. The fallback is the common path because no harness exists while nothing is loaded. Acceptable, and the timeout gap is Minor 7.
- **(4) .mu views never rendered:** I read the bindings, command wiring, two-way selection, `itemsselector`, the dialog Create CanExecute path and the context-menu DataContext pattern against mural and Plexus sources, and they hold up (see ⚠️ above). Rendering cannot verify the one real problem found: the ListBox selection loss, Important 1. That is a VM/collection issue.

### Assessment

**Task quality:** Needs fixes

**Reasoning:** The task meets the spec, and the structure, signalling and tests are solid. Three behaviours can't be trusted yet. A reorder after every committed turn drops the visual selection. Switching rows mid-turn silently aborts the running turn. Rename/delete errors never reach the UI.

## Re-review round 1 (86425a3d..8758a56e)

Scope: fix verification only. Tests and typecheck not re-run.

### Fixed correctly
- Important 1 (reorder drops selection): `Reconcile` now uses Insert/Move/RemoveAt with no Batch or reset. The invariant (first i+1 rows equal rows[0..i], tail removed) holds. Null/undefined writes are ignored and the selection is re-announced on a microtask. No feedback loop (the row === _selected guard stops it).
- Important 3 (swallowed rename/delete errors): `Panel.ReportError` shows `rename failed:` / `delete failed:` / `could not open the dialog:` on StatusText. The prompt, confirm and RPC are all inside the try.
- Minors 1-9 as ruled: `PlacementState` enum plus `PlacementText`; `TextFormat` as the single helper; `Message(e)` in the panel; test constants moved; blank lines; `ConnectionState` with an empty-text hint and stale header (Load/Unload off); discovery timeout with :8779 fallback, run on every attempt, same backoff; Load disabled during first-message load (`turnLoading`); README. Minor 10 needed no change.
- Lock is released on any abandon (`AbandonTurn` goes through `SetBusy(false)` via `Chat.Bind(null)`), so it cannot stick after a disconnect.

### Remaining findings

#### Important
1. **The switch lock covers only the row-selection path; "New session" still silently kills the running turn.**
   - SessionsService.ts: `OpenNewSessionDialog` -> `OnCreated` -> `OpenCreated` -> `Panel.Show` + `View.Attach(newId)`.
   - `SessionViewModel.Attach` (SessionViewModel.ts:189-197) calls `Chat.Bind(null)` -> `AbandonTurn`. That is the same abort the ruling set out to prevent, reached through a different entry.
   - `NewSessionCommand.CanExecute` is only `client !== null`; it is not gated on the lock.
   - Fix: while `switchLocked`, either disable `NewSessionCommand` (raise CanExecuteChanged from `SetSwitchLocked`) or refuse in `OpenCreated`, and set the same notice. Add a test: a turn in flight, New session, no second attach.

### Out of scope
- None found.

**Verdict: Needs fixes** (1 Important: New session bypasses the switch lock).
