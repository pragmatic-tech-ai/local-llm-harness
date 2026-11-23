# SDD ledger — plan: D:/local-llm-harness/docs/plans/2026-11-23-tool-calling-runtime.md
Spec: docs/specs/2026-11-23-tool-calling-runtime-design.md
Branch: feat/tool-calling (off main d019bc2a); spec+plan commit is the branch's first commit.

## Pre-flight scan
| Pair / task | Produces vs consumes | Finding |
|---|---|---|
| T1→T3,T7 | ChatMessage::new/template_view, apply_template(..,tools) | consistent |
| T2→T3 | GemmaToolParser push/content/calls/finished, ParseEvent | consistent |
| T2→T7 | ParsedCall is Serialize only; T7 deserializes turn_done.tool_calls | CONFLICT → R1 |
| T3→T6 | FakeEngine reused by T6 tests | T6 says make it pub(crate) cfg(test); T3 should create it that way → R2 |
| T4→T7 | EchoTool test helper reused by T7 registry | needs pub(crate) cfg(test) → R2 |
| T7→T8 | FakeConnector/FakeLink reused by T8 server tests | needs pub(crate) cfg(test) → R2 |
| T6→T8 | ReadyWorkerUrl on HarnessManager, SupervisorConnector | consistent |
| T7→T9 | tool_result{id,name,ok,content}, loop_done{steps,reason} | consistent |
| T3→T9 | model_props.supports_tools proxied verbatim by T8 | consistent |
| T10 vs branch | T10 says "push main"; work is on feat/tool-calling | → R3 |
| T1 self | hive-test.sh writes into the deploy clone; --restore at T10 | consistent |
| T3 self | 5 chunks, marker chunk relayed, "fake" not → 4 chunks | consistent w/ RF#2 |
| T2,T4,T5,T6,T7,T8,T9 self | tests vs interfaces | consistent |

Ruling R1: ParsedCall derives Deserialize as well as Serialize (T2) — T7 must decode turn_done.tool_calls — cost if wrong: none.
Ruling R2: test helpers reused across tasks (rpc FakeEngine, runtime EchoTool, runtime FakeConnector/FakeLink) are pub(crate) under #[cfg(test)] from the task that creates them — later tasks import them — cost: trivial visibility edit.
Ruling R3: T10 "push main" = after final review, merge feat/tool-calling into main locally (finishing skill, owner choice) then push only on owner go — cost: none, push gated anyway.

## Tasks
Task 1: dispatched (BASE b197fa20, implementer a92a3d92bad2fbf48, sonnet)
Task 1: BLOCKED — hive Smart App Control (enforce) blocks rustc loading fresh proc-macro DLLs; code written uncommitted in working tree; asked owner
Task 1: review approved (0 crit/0 imp, 4 minor) → fix round 1 sent to implementer (fixture BOM, rustfmt, 3 tests, script quoting)
Task 1: complete (commits b197fa2..74f7534, review clean after 1 fix round)
Task 2: dispatched (BASE 74f7534d, implementer a0d4ad0a2dfe73606, sonnet)
Task 2: DONE_WITH_CONCERNS 6fb374c5 — added finish() (unterminated call → parse_error); Ruling: accept finish(), Task 3 must call it at stream end — cost: small API addition
Task 2: review approved (0/0, 6 minor) → fix round 1. Ruling: <|tool_response> outside a string in Call mode implicitly ends the call (body parsed normally) — model may skip <tool_call|> — cost: none if never happens
Task 2: complete (commits 74f7534..321a71f, review clean after 1 fix round)
Task 3: dispatched (BASE 321a71f8, implementer a13a762bfaab7a43e, sonnet)
Task 3: DONE b3b30c6a; noted candle supports_tools()=true (from T1) contradicts spec (only llamacpp) → fix in T3 round
Task 3: review approved (0/0, 5 minor) → fix round 1 (+candle supports_tools false). Ruling: generate with tools on non-tool engine → turn_error 'tools are not supported by this engine' — runtime sends [] anyway — cost: none
Task 3: complete (commits 321a71f..acadc8a, review clean after 1 fix round)
Task 4: dispatched (BASE acadc8a9, implementer a414aee415fa30571, sonnet)
Task 4: review Needs fixes (1 Important: tool panic unwinds; 5 minor) → fix round 1
Task 4: complete (commits acadc8a..d1e755e, review clean after 1 fix round)
Task 5: dispatched (BASE d1e755ed, implementer af512e78b600b3752, sonnet)
Task 5: review approved (0/0, 7 minor) → fix round 1. Ruling: lexical .. normalisation before canonicalize so escapes don't reveal host-path existence — cost: none
Task 5: complete (commits d1e755e..7cb3242, review clean after 1 fix round + sibling-junction test)
Task 6: dispatched (BASE 7cb3242a, implementer a5da4a754534d453d, sonnet)
Task 6: review Needs fixes (Important: no timeouts) → fix round 1. Ruling: CONNECT 5s, CALL 30s, STREAM_IDLE 120s (idle per frame, prompt eval can be slow), overridable for tests — cost: tune later if live prompts exceed
Task 6: complete (commits 7cb3242..dcbfc63, review clean after 2 fix rounds)
Task 7: dispatched (BASE dcbfc637, implementer ac47bf62283722c48, sonnet). Rulings: any truncation (clean end / Err item / generate Err) → Error + turn_error; model_props failure → Error; SinkClosed aborts run
Task 7: review approved (0/0, 6 minor + ⚠️ dup call ids). Ruling: dup call_1 ids across steps are a correctness bug (template binds by tool_call_id) → runtime rewrites ids unique per transcript, incl. relayed tool_call note — cost: relay no longer strictly verbatim for tool_call.id. Fix round 1 sent.
Task 7: complete (commits dcbfc63..1236d1d, review clean after 2 fix rounds)
Task 8: dispatched (BASE 1236d1da, implementer ac759599e6b0f1cf9, sonnet). Rulings: no sandbox → only get_time, '(1 tools)'; tokenize w/o harness → 'no model running'
Task 8: review Needs fixes (4 Important: param dup, bind abort, sandbox warn, no ws e2e test) → fix round 1. Ruling: runtime bind failure degrades (eprintln, chat_port None, control plane stays up) — cost: chat silently falls back to... no chat_url → app uses worker url
Task 8: complete (commits 1236d1d..316855e, review clean after 1 fix round)
Task 9: dispatched (BASE 316855ed, implementer a9d0ba54e0f3a9678, sonnet)
Task 9: DONE_WITH_CONCERNS 9854ff6a. Rulings: tests adjusted for busy-until-response rule OK (spec mandates); separate TextBlocks in .mu OK (no interpolation); ToolsText atop chat DockPanel OK; extra model_props per Bind accepted (cheap) — cost: one extra rpc
Task 9: review approved (0/0, 5 minor) → fix round 1 (disconnect note mid-loop, parse_error rendering, shared model_props, rebind test, test helpers)
Task 9: complete (commits 316855e..f39e9f3, review clean after 2 fix rounds)
Final review: dispatched (d019bc2a..f39e9f39, opus, a7f4938277169e0d3)
Final review: With fixes (2 Important: preamble closes turn; thinking leak after tool result; 8 minor). Rulings: (1) split preamble into separate assistant message; (2) worker prefixes turn_done.content with unclosed channel opener from prompt tail; (4) per-step connect, mid-loop missing link → Error 'no model running'; (5) build minimal ToolCallFormat trait per spec; (6) local time via windows-sys feature — costs: see final-findings.md
Final fix wave: dispatched (FIX_BASE f39e9f39, opus, a57e11ec0ed6fd766)
Final fix wave: DONE 19a5f739..11ca7778 (hive lib 220, --tests 246, llamacpp 228, app 118). Scoped re-review dispatched (opus, a67ef6b586f2ef716)
Final re-review: all 10 fixed; residual minors (http template_view regression, loose OpenChannel tail, dup const, thinking+preamble test). Ruling: owner's fix-all rule → one small residual round by same fixer (not a new wave) — cost: one more short run. Ruling: keep separate bubble for raw <|tool_response> marker chunk — owner wants raw mechanics visible — cost: cosmetic noise
Residual round: 84136bf5, hive lib 223 + --tests green; controller spot-checked diff. Final review clean.
