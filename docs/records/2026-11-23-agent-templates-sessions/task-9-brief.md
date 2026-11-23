### Task 9: Sessions capability (replaces Runtime)

**Files:**
- Rename/modify:
  - `views/RuntimeModule.mu` → `views/SessionsModule.mu`, with module and capability `Name = "Sessions"`, the same icon, and the `supervisor.url` setting kept.
  - `RuntimeManagementService.ts` → `SessionsService.ts`, with its test.
  - `app.mu`
- Create: `SessionsPanelViewModel.ts`, `SessionViewModel.ts`, `NewSessionDialogViewModel.ts`, `views/SessionsPanel.mu`, `views/SessionView.mu`, `views/NewSessionDialog.mu`, and their tests
- Delete:
  - `HarnessPickerViewModel.ts` and its test
  - `views/HarnessPanel.mu`
  - The picker-only parts of `SupervisorClient.ts`. Keep the code that discovers `chat_url`: `SessionsService` lists harnesses, or reads the supervisor's runtime endpoint, so it can find the runtime URL. Ruling: the runtime URL is the supervisor URL with the port replaced by the `chat_url` port from `list_harnesses`, or falls back to `:8779`. Add a `runtime_url` field to the supervisor's `list_runtimes`/`hello` if that is cleaner, and say which in the report.

**Interfaces:**
- Produces:
  - `SessionsService` owns one `RuntimeClient`. It reconnects with backoff (1 s, 2 s, 4 s, capped at 10 s) and re-attaches the session that was shown.
  - `SessionsPanelViewModel`:
    - `Sessions: ObservableCollection<SessionRowViewModel>`, where each row has `Title`, `TemplateName`, `StateText`, `StateKind` and `Busy`.
    - `SelectedSession`, which on set raises `OnOpen: TypedSignal<string>`.
    - `NewSessionCommand`, `RenameCommand`, `DeleteCommand`.
    - It refreshes on `sessions_changed` and updates the row state on `placement_changed`.
  - `StateKind`:
    - `ready` → live
    - `needs_load` → idle
    - `unavailable` → unavailable
    - busy → busy
  - `SessionViewModel` wraps the existing `ChatViewModel` and `InstrumentPanelViewModel`, bound to the attached session. It exposes `HeaderText` (template · model @ host), `PlacementText`, `CanLoad`, `LoadLabel` (`Load model` or `Load model (evicts {evicts})`), `LoadCommand`, `UnloadCommand`, and the tools indicator. It shows the attached transcript on attach: history becomes `UserMessage`, `AssistantMessage`, `ToolCallMessage` and `ToolResultMessage` items, with system messages hidden.
  - `NewSessionDialogViewModel` holds the template tree (`TreeView`), a `Summary` of the selected template (model, tools, budget), and `CreateCommand`. It is shown via `DialogService.Show`, creates the session, then opens it.
  - The chat side: `send_turn` on `needs_load` gets a `session_loading` note, which shows `[loading model…]` in the chat.

- [ ] **Step 1: Write the failing tests:**

```ts
'sessions_panel_lists_sessions_with_state'        // placements ready/needs_load/unavailable map to live/idle/unavailable
'placement_changed_updates_row'
'selecting_row_attaches_and_shows_history'        // AttachSession sent; transcript rendered; system message hidden
'load_button_label_names_eviction'                // 'Load model (evicts gemma-4-12B-it-QAT-Q4_0)'
'new_session_dialog_creates_and_opens'
'reconnect_reattaches_shown_session'
'deleted_shown_session_clears_view'
```

- [ ] **Step 2–4: Run FAIL, implement, run PASS** (`npm test && npm run typecheck:web`).
- [ ] **Step 5: Commit:** `feat(app): Sessions capability with session list, view and new-session dialog`.

