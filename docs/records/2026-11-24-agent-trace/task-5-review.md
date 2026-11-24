### Spec Compliance
- ✅ StepRecord, ToolTiming, TurnRecord, TraceTurn and TracePage match the Task 4 "Fix round 1" wire JSON field by field (agents.ts:158-231). Nullability is right: load_ms, ttft_ms, gen_ms and error on steps, summary on TraceTurn.
- ✅ StepRecord.end is the six-value union.
- ✅ `v` and `kind` are present on both records, as the Fix round 1 ruling requires.
- ✅ GetTrace (RuntimeClient.ts) omits undefined params and does not require an attach. The wire name is `get_trace`.
- ✅ OnTraceStep, OnTraceTurn and OnTraceWarning dispatch on `trace_step`, `trace_turn` and `trace_warning`. Their payloads are `{id, record}`, `{id, record}` and `{id, message}`.
- ✅ Style: Allman braces, no semicolons, PascalCase members and hoisted `private static readonly` constants all hold in the new code.
- ✅ Tests: two GetTrace table rows (no paging, and before/limit) plus `trace_notifications_raise_signals`. The page fixture covers a null summary and reset_before true.
- ⚠️ The test and typecheck results are the implementer's claims. I did not re-run them.

### Strengths
- Wire fidelity is exact, snake_case is kept, and nullable fields are modelled as `| null` rather than optional.
- The signal and dispatch pattern matches the existing code.
- The test fixtures use the authoritative wire shapes.

### Issues
#### Critical
None.
#### Important
None.
#### Minor
1. agents.ts:171 and :190. `kind` is `'step' | 'turn'` on both StepRecord and TurnRecord. It should be the literal `'step'` on StepRecord and `'turn'` on TurnRecord. As written, the compiler cannot discriminate the two records, and a TurnRecord typed `kind: 'step'` would compile. Fix: use the narrow literal on each interface.
2. agents.ts:166 (doc comment). It says "ignore a record whose kind is unknown", but the type admits only step and turn. The comment is about the log reader and does not apply to this type. Fix: reword it or drop it.
3. RuntimeClient.test.ts, trace_notifications_raise_signals. It only checks that the payloads pass through unchanged, so it cannot catch a signal wired to the wrong note. Each signal is the only one to receive its own payload, and `toEqual` on the ordered list does catch a cross-wiring. The test is adequate, so this is optional. No finding beyond noting it.
4. RuntimeClient.ts:6-9 (import order). TraceStepNote, TracePage and TraceTurnNote are inserted before TemplateNode and SamplingConfig, and the existing list is not alphabetical either. Cosmetic.

### Assessment
**Task quality:** Approved
**Reasoning:** The types and client match the authoritative wire JSON, the style rules are met, and the tests cover the specified rows. The only findings are minor typing and polish.
