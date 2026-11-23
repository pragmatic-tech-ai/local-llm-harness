### Task 8: App runtime client for sessions and templates

**Files:**
- Modify: `app/src/renderer/src/HarnessClient.ts` (rename the class to `RuntimeClient` in `RuntimeClient.ts`; keep the chat API), `HarnessClient.test.ts` → `RuntimeClient.test.ts`, and all imports
- Create: `app/src/renderer/src/agents.ts` (types)

**Interfaces:**
- Produces:
  - Types:
    - `TemplateNode` (group or template, mirroring Task 1's JSON)
    - `AgentTemplate` (snake_case fields as on the wire)
    - `SessionSummary` (+ `placement: Placement`, `busy: boolean`)
    - `Placement { state: 'ready' | 'needs_load' | 'unavailable', host?: string, evicts?: string | null, reason?: string }`
    - `AttachedSession`
    - `CatalogInfo`
  - `RuntimeClient` methods, one per Task 6 method, in PascalCase: `ListTemplates()`, `GetTemplate(id)`, `SaveTemplate(id, path, template)`, `DeleteTemplate(id)`, `CreateGroup(path)`, `RenameGroup(path, newName)`, `DeleteGroup(path)`, `MoveTemplate(id, path)`, `ListCatalog()`, `ListAllTools()`, `ListSessions()`, `CreateSession(templateId)`, `AttachSession(id)`, `RenameSession(id, title)`, `DeleteSession(id)`, `LoadSessionModel(id)`, `UnloadSessionModel(id)`, `SetThinking(enable)`.
  - Existing methods stay: `SendTurn`, `SetSampling`, `ModelProps`, `Tokenize`, `ListTools`, `Reset`.
  - Signals: `OnSessionsChanged`, `OnTemplatesChanged`, `OnPlacementChanged: TypedSignal<{id, placement}>`, `OnSessionLoading: TypedSignal<{id}>`.

- [ ] **Step 1: Write the failing tests:** `each_method_sends_its_wire_name_and_params` (table-driven over all the methods above), `list_level_notifications_raise_signals`.
- [ ] **Step 2–4: Run** `cd app && npx vitest run src/renderer/src/RuntimeClient.test.ts`: FAIL, implement, then PASS. Then run `npm test && npm run typecheck:web`.
- [ ] **Step 5: Commit:** `feat(app): runtime client for sessions and templates`.

