# Task 8 report: RuntimeClient for sessions and templates

Status: DONE_WITH_CONCERNS

## Built
- `HarnessClient` renamed to `RuntimeClient` (`git mv`, `RuntimeClient.ts` / `RuntimeClient.test.ts`), all imports updated (ChatViewModel, InstrumentPanelViewModel, LogprobVM, FakeSocket, RuntimeManagementService, main.ts and their tests).
- New `app/src/renderer/src/agents.ts`: AgentTemplate, TemplateNode (group|template), TemplateRecord, Placement, SessionSummary (+placement, busy), ChatMessage, AttachedSession, CatalogInfo, ToolSpec, PlacementChanged, SessionLoading, SessionBusy. Data fields keep wire snake_case.
- New methods (PascalCase, wire snake_case params): ListTemplates, GetTemplate, SaveTemplate(id?, path?, template) -> id (omits undefined id/path), DeleteTemplate, CreateGroup, RenameGroup(path, newName) -> {path,new_name}, DeleteGroup, MoveTemplate, ListCatalog, ListAllTools, ListSessions, CreateSession(templateId) -> {template_id}, AttachSession -> AttachedSession, RenameSession, DeleteSession, LoadSessionModel -> Placement, UnloadSessionModel, SetThinking(enable), Reset() (did not exist on the client before; added).
- Signals: OnSessionsChanged, OnTemplatesChanged, OnPlacementChanged, OnSessionLoading, OnSessionBusy; turn/tool/loop signals unchanged.

## TDD
- RED: `npx vitest run src/renderer/src/RuntimeClient.test.ts` -> 2 failed (new tests, methods/signals missing).
- GREEN: same file 19 passed; `npm test` 13 files / 120 tests pass; `npm run typecheck:web` clean. No CR in changed files.
- Tests: `each_method_sends_its_wire_name_and_params` (table, 21 rows incl. SaveTemplate with/without id+path, SetSampling, Reset), `list_level_notifications_raise_signals` (incl. session_busy).

## Concerns / for Task 9
- `SendTurn(content, enableThinking)` still sends `{content, enable_thinking}`; the server's send_turn takes only `{content}` (extra field ignored). Task 9 should drop the argument and use SetThinking.
- `SetSampling` / `ModelProps` / `Tokenize` / `ListTools` act on the attached session now (need AttachSession first).
- Old-flow leftovers for Task 9 to delete: no connect-to-worker method exists on the client; the old connect flow lives in RuntimeManagementService (Connect(url), harnessFactory/BindBackend) and main.ts, untouched here.
- ListTools returns the existing `ToolInfo` (no `parameters`); ListAllTools returns `ToolSpec` (with `parameters`).
- ChatMessage.role typed as a union; tool_calls typed `unknown[]`.

# Fix round 1

1. `SamplingConfig` moved into `agents.ts`; `RuntimeClient` imports from agents and re-exports the type; `InstrumentPanelViewModel` imports it from agents. agents.ts no longer imports RuntimeClient.
2. The table test now replies per row and asserts the resolved value (`row.method` in the failure message), covering ListTemplates, GetTemplate, SaveTemplate (id mapping), ListCatalog, ListAllTools, ListSessions, CreateSession, AttachSession, LoadSessionModel and the void methods.
3. Added rows for SendTurn, ModelProps, Tokenize (and ListTools).
4. `SendTurn(content)` only sends `{content}`. `ChatViewModel.Send` now calls `client.SetThinking(Panel.EnableThinking)` right after `SetSampling` and before `SendTurn` (errors go to `Panel.ReportSamplingError`). Tests updated: wire order is set_sampling, set_thinking, send_turn; `ForwardsThinkingToggleOnSend` checks the set_thinking params and `{content}`.
5. Comment typo fixed.
6. `ListTools` returns `ToolSpec[]`; `ToolInfo` removed (no other users).

Evidence: `npm test` 13 files / 120 tests pass; `npm run typecheck:web` clean; no CR in the changed files.
Note: Task 9 should replace the per-send SetThinking with a call on toggle change when it reworks the chat VM.
