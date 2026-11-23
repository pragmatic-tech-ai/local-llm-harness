### Spec Compliance
- ✅ Rename HarnessClient -> RuntimeClient (git rename, 95% similar), test file renamed, all imports updated (ChatViewModel, InstrumentPanelViewModel, LogprobVM, FakeSocket, RuntimeManagementService, main.ts and tests). No stale `HarnessClient` references in the diff.
- ✅ All 18 brief methods exist in PascalCase (RuntimeClient.ts:1015-1116) with wire names and snake_case params matching task-6-report (`new_name`, `template_id`, `enable`, `id`/`path`/`template`). Existing SendTurn/SetSampling/ModelProps/Tokenize/ListTools kept; Reset added (matches the `reset` wire method).
- ✅ Signals OnSessionsChanged, OnTemplatesChanged, OnPlacementChanged, OnSessionLoading, plus the controller-added OnSessionBusy<{id,busy}> (RuntimeClient.ts:982-986, 1128-1142), dispatching the correct note names.
- ✅ Types in agents.ts match the wire (AgentTemplate fields, TemplateNode union, Placement, SessionSummary+placement+busy, AttachedSession, CatalogInfo, ToolSpec). ChatMessage matches backend/src/engine/types.rs:52.
- ✅ SaveTemplate omits undefined id/path so the server default applies; returns the id.
- ✅ Both required tests present; table covers all new methods plus SetSampling and Reset.
- ⚠️ SendTurn still sends `enable_thinking` (RuntimeClient.ts:1012); the server ignores it. Brief says SendTurn stays, so acceptable here, flagged for Task 9.

### Strengths
- Mechanical rename is clean; style rules met (Allman, no semicolons, literals hoisted to private static readonly, no module-level functions/state).
- Dispatch is a single switch with no logic duplication; unknown notifications are ignored.
- SaveTemplate optional-param handling is tested in both forms.

### Issues
#### Critical
None.
#### Important
None.
#### Minor
1. agents.ts:1 and RuntimeClient.ts:831 - circular type-only import (agents.ts imports SamplingConfig from RuntimeClient, RuntimeClient imports agents). Erased at compile time so harmless, but a layering smell; move SamplingConfig into agents.ts (re-export from RuntimeClient) to make the dependency one-way.
2. RuntimeClient.test.ts:791-803 - the table asserts only the request wire name and params; result mapping is verified only for SaveTemplate (returns `saved.id`). Other result-typed methods are never checked to resolve their payload (e.g. CreateSession, AttachSession, LoadSessionModel), so a wrong-field mapping would pass.
3. RuntimeClient.test.ts - SendTurn, ModelProps and Tokenize wire params are not in the table (only SendTurn's rejection and ModelProps's response id are covered elsewhere). SendTurn's `{content, enable_thinking}` payload is untested, which would be the natural place to pin the Task 9 change.
4. RuntimeClient.ts:1010-1012 - SendTurn still sends a dead `enable_thinking` field the server ignores, and a caller passing the flag expects it to take effect (it does not; SetThinking is the real path). Fix in Task 9 as the implementer noted, or drop the parameter now.
5. RuntimeClient.ts:1071 - comment typo "attached session own subset" (missing apostrophe).

### Assessment
**Task quality:** Approved
**Reasoning:** The client faithfully mirrors the Task 6 wire protocol, including the late session_busy notification, with correct params, types and signals, and follows all style constraints. Only minor test-coverage and polish items remain.
