### Task 5: App client for the trace

**Files:**
- Modify: `app/src/renderer/src/agents.ts` (types)
- Modify: `app/src/renderer/src/RuntimeClient.ts`
- Modify: `app/src/renderer/src/RuntimeClient.test.ts`

**Interfaces:**
- Produces:
  - Types: `StepRecord`, `ToolTiming`, `TurnRecord`, `TraceTurn` and `TracePage`. They mirror Task 4's JSON field for field, in snake_case. `StepRecord.end` is `'tool_calls' | 'answered' | 'budget' | 'rejected' | 'error' | 'aborted'`.
  - `RuntimeClient.GetTrace(id: string, before?: number, limit?: number): Promise<TracePage>`. It omits undefined params.
  - Signals: `OnTraceStep: TypedSignal<{id: string, record: StepRecord}>`, `OnTraceTurn: TypedSignal<{id: string, record: TurnRecord}>` and `OnTraceWarning: TypedSignal<{id: string, message: string}>`.

- [ ] **Step 1: Write the failing tests:**
  - Add `GetTrace` rows to the existing table test: one with no paging, which sends `{id}`, and one with `before`/`limit`. Each checks the wire name, the params and the result mapping.
  - `trace_notifications_raise_signals`.
- [ ] **Step 2–4:** Run `cd app && npx vitest run src/renderer/src/RuntimeClient.test.ts`: it fails first, then implement, then it passes. Then run `npm test && npm run typecheck:web`.
- [ ] **Step 5: Commit** `feat(app): runtime client for the agent trace`.

