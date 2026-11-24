# SDD ledger — plan: docs/plans/2026-11-24-agent-trace.md
Spec: docs/specs/2026-11-24-agent-trace-design.md (owner approved "go" 2026-11-24). Branch feat/agent-trace from main @49b4d340. Execution: subagent-driven (owner "go").

## Pre-flight scan
| pair / task | produces vs consumes | finding |
|---|---|---|
| T1→T3 | T1 records + TraceLog in trace.rs; T3 adds TurnTrace/SessionTrace to the same file | ok; T1 owns the file, T3 extends it |
| T1→T4 | TracePage JSON for get_trace; TraceStore in deps | ok |
| T2→T3 | T2 `think_counter(prompt)` on "the existing format type"; T3 loop in runtime has no handle on the worker's ToolCallFormat | CONFLICT → ruling R1 |
| T3→T4 | StepLink, SessionTrace::new/finish/reset; T3 keeps server compiling with NoTrace | ok |
| T4→T5 | get_trace JSON + trace_* notes vs agents.ts types | ok (field-for-field mandated) |
| T5→T6 | GetTrace + signals | ok |
| T1 self | test `session_store_ignores_trace_files` lives in agents/session_store.rs, not in Files list | minor → ruling R2 |
| T2 self | spec says test per engine; plan defers in-process engines to live proof | accepted in plan; owner told |
| T3 self | record enum renamed StepOutcome to avoid private StepEnd clash | ok |
| T6 self | one-file vs per-class | implementer follows app pattern |
| T7 self | push needs owner go | stop point |

Ruling R1: T2 also produces `ThinkCounters::for_prompt(prompt: &str) -> Box<dyn ThinkCounter>` (associated fn in engine/tool_format.rs) that picks GemmaThinkCounter when the prompt carries Gemma 4 turn/channel syntax, else NoThinkCounter; the loop (T3) uses it on turn_started.prompt — cost if wrong: a non-Gemma model with channels counts 0 think tokens
Ruling R2: T1 may touch agents/session_store.rs tests (and the loader if it does pick up .trace.jsonl) — cost: none

Task 1: dispatched (BASE 49b4d340, implementer a196cfd93917a0aff, sonnet)
Task 1: DONE 362a64b0 (no RED run; extra test for cut-tail append)
Task 1: review Needs fixes (Important: read errors swallowed → turn-number reuse; 6 minor) → round 1. Ruling: reset before turn 1 keeps reset_before true (divider under oldest turn) — cost: none; Ruling: TraceStore::log validates id, delete marks log deleted (appends no-op) — cost: none
Task 1: fix round 1 c906b3cc (next_turn/log now io::Result) — controller spot-check OK
Task 1: complete (commits 49b4d34..c906b3c, review clean after 1 fix round)
Task 2: dispatched (BASE c906b3cc, implementer adb39dd45e5c65196, sonnet) with ruling R1
Task 2: DONE_WITH_CONCERNS 60ca41d8 (no RED; brief's think-3 explanation mismatched its rule, rule followed; http completion_body extracted)
Task 2: review APPROVED (0/0, 4 minor) → round 1. Ruling: for_prompt anchors on the Gemma 4 generation-prompt tail, not anywhere-match — cost: a Gemma prompt rendered without the model opener counts 0 think
Task 2: fix round 1 859cfa87. Ruling: for_prompt also matches a bare open channel tail (post-tool-response thinking) — needed, cost: none; Ruling: keep the carry closer guard (defensive if CARRY_LEN changes) — cost: none
Task 2: complete (commits c906b3c..859cfa8, review clean after 1 fix round; controller spot-check)
Task 3: dispatched (BASE 859cfa87, implementer a0d7d7bbdff417130, opus). Ruling: SessionTrace::new infallible; next_turn error → turn 0 + pending trace_warning — cost: turn 0 rows if the log is unreadable
Task 3: DONE_WITH_CONCERNS 209a57f1 (mutation-RED; reset allow(dead_code) until T4; trace_step precedes loop's turn_error/loop_done; failed-before-worker steps traced)
Task 3: review APPROVED (0/0, 3 minor: test gaps + blank lines) → round 1 (test-only)
Task 3: fix round 1 82b2e4dd (test-only; controller spot-check)
Task 3: complete (commits 859cfa8..82b2e4d, review clean after 1 fix round)
Task 4: dispatched (BASE 82b2e4dd, implementer ae880f79e8a669f5d, opus). Ruling: TraceStore::log Err → turn runs with NoTrace + one trace_warning — cost: that turn untraced
Task 4: DONE_WITH_CONCERNS d9d70a3e (live trace_* records bare vs get_trace records with v/kind). Ruling: live records must carry v/kind too (spec: same records) — fix in round 1 — cost: none
Task 4: review Needs fixes (Important: reset/delete race leaves stray log; 2 minor) → round 1 incl. live v/kind ruling
Task 4: fix round 1 d43c9f16 (live v/kind byte-equal test; reset handle first; forget_trace_if_gone) — controller spot-check
Task 4: complete (commits 82b2e4d..d43c9f1, review clean after 1 fix round)
Task 5: dispatched (BASE d43c9f16, implementer a7f67168e69109bba, sonnet)
Task 5: DONE 18ca41fa
Task 5: review APPROVED (0/0, 4 minor) → round 1
Task 5: fix round 1 555e52e9 (mechanical; controller accepts)
Task 5: complete (commits d43c9f1..555e52e, review clean after 1 fix round)
Task 6: dispatched (BASE 555e52e9, implementer afad42d123e46f7d0, opus). Ruling: live turn = trace_step seen while shown and no trace_turn yet; page-only summary-less turn = incomplete
Task 6: DONE_WITH_CONCERNS cc8a4f81 (unrendered .mu; prompt_tokens/n_ctx widened to null; auto-collapse overrides manual expand)
Task 6: review Needs fixes (Important: tool rows on one line; stale warning after same-session reload; 4 minor) → round 1. Ruling: auto-collapse only untoggled turns — cost: none
Task 6: fix round 1 38107249; scoped re-review dispatched
Task 6: re-review Needs fixes (trace_warning note survives same-session reload; my ruling was ambiguous) → round 2. Ruling: successful page load clears both warnings — cost: none
Task 6: fix round 2 fb35dd4e. Ruling: a successful Load older page also clears a trace_warning — cost: a write-failure warning can vanish on paging
Task 6: complete (commits 555e52e..fb35dd4, review clean after 2 fix rounds; round 2 controller-accepted)
FINAL REVIEW: dispatched on 49b4d3408b36b0d960e7e092bf6726126c11a162..fb35dd4e
FINAL REVIEW: with fixes (I1 turn numbers not reserved → collisions on unwritable log; 7 minor). Fix wave a22ff63754595d1a6 (opus) for I1,M1-M5; controller does M6 (spec status) + M7 (records archive).
Ruling: turn numbers reserved at allocation; unreadable log → turn untraced + one trace_warning (supersedes T3 "turn 0" ruling) — cost: none
Ruling: busy session at attach makes newest summary-less turn live ("running…") — cost: none
FINAL FIX WAVE: 9478d56e, 4be5f662; scoped re-review dispatched
FINAL RE-REVIEW: APPROVED, nothing remaining. Task 7 awaits owner go.
