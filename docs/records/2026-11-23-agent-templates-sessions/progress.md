# SDD ledger — plan: D:/local-llm-harness/docs/plans/2026-11-23-agent-templates-sessions.md
Spec: docs/specs/2026-11-23-agent-templates-sessions-design.md
Branch: feat/agent-templates (off main 0ae0d512..a49e069c + plan commit)

## Pre-flight scan
| Pair / task | Produces vs consumes | Finding |
|---|---|---|
| T1→T2 | AgentTemplate, TemplateStore::get | consistent |
| T1→T6/T7 | TemplateRules, TemplateNode JSON | consistent |
| T2→T4 | SessionRecord.snapshot → requirement | consistent |
| T3→T4 | ModelHost, ModelRequirement, current_link | consistent (Dispatcher::current_link added) |
| T4→T6 | Dispatcher, FakeHost pub(crate) | consistent |
| T5→T6 | TurnConfig, TurnLinks, run returns transcript | consistent; SessionLinks adapter in T6 |
| T5 vs T7 | ToolLoop::new(executor, registry) (no cap) | consistent |
| T6→T8 | wire method names | consistent |
| T8→T9/T10 | RuntimeClient shared instance | T10 says shared service; T9 owns client → R1 |
| T3 self | HarnessInfo.n_ctx None never matches | consistent w/ spec settled detail |
| T9 self | runtime URL discovery ruling inline | → R2 |
| others self | tests vs interfaces | consistent |

Ruling R1: SessionsService owns the single RuntimeClient and exposes it (signal OnClientChanged / property) for AgentsService; AgentsService resolves SessionsService via the provider — one socket — cost: small coupling between two app services.
Ruling R2: runtime URL from supervisor chat_url (list_harnesses) with :8779 fallback; implementer may add explicit field, must report — cost: none.

## Tasks
Task 1: dispatched (BASE b3607103, implementer ad867772664702a78, sonnet)
Task 1: DONE_WITH_CONCERNS 6164ad53 (extra error strings template not found/group not found/group already exists — accepted)
Task 1: review Needs fixes (2 Important: non-slug stems unlocatable; duplicate stems) → fix round 1. Ruling: locate accepts any valid stem; duplicates flagged 'duplicate template id: {id}' — cost: hand-made dupes need manual rename
Task 1: re-review Needs fixes (Critical: case-insensitive locate + case-sensitive PathBuf compare deletes the written file) → fix round 2. Ruling: keep on-disk stem casing as id on save
Task 1: complete (commits b360710..c3f289e, review clean after 3 fix rounds; last round identity check, test green, controller spot-check)
Task 2: dispatched (BASE c3f289e6, implementer a25ffa5ca1f14c2c6, sonnet)
Task 2: DONE_WITH_CONCERNS 19cdf5fa. Ruling: create returns Result (writes to disk) — cost: none
Task 2: review approved (0/0, 8 minor) → fix round 1. Ruling: titles use max #n+1 (no repeats after deletes) — deviates from 'count+1' in brief, spec only says numbered per template — cost: none
Task 2: complete (commits c3f289e..f4589b9, review clean after 2 fix rounds)
Task 3: dispatched (BASE f4589b93, implementer aaca90175efafaeb6, sonnet)
Task 3: DONE_WITH_CONCERNS d4362ce9 (LocalHarness trait seam, FakeHarness tests; allow(dead_code) on FakeHost until T4)
Task 3: review approved (0/0, 5 minor) → fix round 1
Task 3: complete (commits f4589b9..f006d38, review clean after 2 fix rounds)
Task 4: dispatched (BASE f006d38f, implementer af5d75eaef9c2d2e1, sonnet)
Task 4: DONE (implementer's git failed; controller committed). Ruling: unload only when host's loaded model matches req — prevents cross-session eviction — cost: none
Task 4: review approved (0/0, 6 minor) → fix round 1. Ruling: mid-load status stays NeedsLoad{evicts:None} (three spec states), documented; atomic ModelHost::unload_if closes unload race
Task 4: re-review Needs fixes (minor: duplicated match rule, unbounded test waits) → round 2. Ruling: unload_if also stops a Loading harness of the same model (cancel) — cost: none
Task 4: complete (commits f006d38..4dacd59, review clean after 2 fix rounds)
Task 5: dispatched (BASE 4dacd592, implementer abfd0b90c81eb48de, sonnet). Ruling: temporary WorkerConnector→TurnLinks adapter in server.rs until T6
Task 5: review Needs fixes (Important: lost reset-keeps-sampling coverage; 4 minor) → round 1
Task 5: complete (commits 4dacd59..b7916ef, review fixes test/doc-only; Ruling: controller spot-check instead of re-review for test/doc-only round — cost: small)
Task 6: dispatched (BASE b7916efc, implementer a5409ae70107e052f, opus). Rulings: per-connection outbound channel+writer; turn continues while ≥1 attached connection remains; busy guard with Drop; create/delete broadcast sessions_changed
Task 6: DONE_WITH_CONCERNS 9d041eba. Rulings: create/attach return list rows (summary+placement+busy) OK; attach returns effective sampling/thinking OK; main.rs temp wiring replaced in T7
Task 6: review Needs fixes (Important: reset race, envelope dup; 5 minor) → round 1. Rulings: step-0 link Err → turn_error{text} then loop_done no_harness (changes T5 test); broadcast session_busy{id,busy} on guard start/drop and sessions_changed after commit — cost: two new notifications for the app
Task 6: re-review Needs fixes (seed placements on create; atomic busy release) → round 2. Ruling: placement map moves into Hub, seeded by create_session
Task 6: complete (commits b7916ef..fb0b944, review clean after 2 fix rounds)
Task 7: dispatched (BASE fb0b9443, implementer ab0f268e513c8d3e0, sonnet). Ruling: store open failure → no runtime bind, control plane runs
Task 7: DONE_WITH_CONCERNS b2afffd0. Ruling: AgentsStack::open + into_deps split (bind ordering) OK
PAUSE for compaction (owner request): Task 7 review running (reviewer ab680338088c96646) on fb0b9443..b2afffd0; implementer ab0f268e513c8d3e0 resumable for fixes. Next: handle T7 review → Task 8 (app RuntimeClient; protocol list in task-6-report.md) → 9 → 10 → final review → T11 deploy (push needs go).
Task 7: review APPROVED (0/0, 3 minor: worker.rs import order; startup test should assert Examples/Default + unwritable path + cleanup; revert unrelated control.rs reflow) → fix round pending after compaction (resume implementer ab0f268e513c8d3e0)
Task 7: fix round 1 sent to ab0f268e513c8d3e0 (resumed after compaction)
Task 7: fix round 1 43def89e (test/import/reflow only; controller spot-check per T5 ruling)
Task 7: complete (commits fb0b944..43def89, review clean after 1 fix round)
Task 8: dispatched (BASE 43def89e, implementer ae525af85a2735a70, sonnet). Ruling: add OnSessionBusy signal for session_busy (post-plan notification) — cost: none
Task 8: DONE_WITH_CONCERNS 264ceca7 (SendTurn still sends enable_thinking; Reset added; ListTools old ToolInfo type). Review dispatched a8d3a10e65a4cc329 (sonnet)
Task 8: review APPROVED (0/0, 5 minor) → fix round 1. Ruling: drop SendTurn enableThinking now (server ignores it; SetThinking is the path) — cost: tiny VM touch before T9
Task 8: fix round 1 9282ce53 (controller spot-check: small, mechanical; SetThinking per send)
Task 8: complete (commits 43def89..9282ce5, review clean after 1 fix round)
Task 9: dispatched (BASE 9282ce53, implementer a633e65d0026d03ba, opus). Rulings: OnSessionBusy drives row busy; panel refresh keeps selection without re-attach; old flow deleted
Task 9: DONE_WITH_CONCERNS 86425a3d (impl before tests; sidecar/backend-url/IPC removed; runtime url resolved app-side, no runtime_url field)
Task 9: review Needs fixes (3 Important: selection lost on reorder, switch during turn aborts it silently, rename/delete errors console-only; 10 minor) → round 1. Rulings: switch refused while shown turn in flight (snap back + notice) — cost: user waits for turn; connection-state indicator; discovery timeout → :8779 fallback
Task 9: fix round 1 8758a56e (all fixed); scoped re-review dispatched
Task 9: re-review Needs fixes (Important: New session aborts running turn) → round 2. Ruling: NewSessionCommand disabled during turn; OpenCreated refuses (session created, not opened) — cost: none
Task 9: fix round 2 a50060ed (controller spot-check of a 2-file targeted fix)
Task 9: complete (commits 9282ce5..a50060e, review clean after 2 fix rounds)
Task 10: dispatched (BASE a50060ed, implementer a2922b56e5246c5af, opus). Rulings: broken template opens from defaults+tree-node fields with ErrorText, save repairs; dirty-switch confirm; refresh keeps selection; unparsable numbers block Save; model not in catalog kept as '(not in catalog)'
Task 10: DONE_WITH_CONCERNS 1e6f55ce (SessionsService.OnActivated added; new template = unsaved draft; delete clears dirty editor; icon lowercase agents.svg)
Task 10: review Needs fixes (2 Important: sampling knobs dropped on save, open/refresh race; 6 minor) → round 1. Rulings: preserve loaded sampling knobs; latest-request-wins open; remote delete of dirty template keeps edits as draft; new template starts with all tools (matches Default) — cost: small
Task 10: fix round 1 24dfce03 (blank temp/top_p send worker defaults since Rust fields non-optional); scoped re-review dispatched
Task 10: re-review APPROVED (out of scope: form disabled during save — live-check note)
Task 10: complete (commits a50060e..24dfce0, review clean after 1 fix round)
FINAL REVIEW: dispatched ae34858baa098910c (opus) on a49e069c..24dfce03 (28 commits). Tasks 1-10 complete; Task 11 deploy needs owner go to push
FINAL REVIEW: Needs fixes (2 Important: per-connection serial dispatch freezes app during turn; validation-failed templates show healthy; 12 minor). Fix wave dispatched afc265f195f1c1005 (opus, BASE 24dfce03).
Ruling: concurrent per-request dispatch, chat methods on attached session started in arrival order — cost: concurrency complexity in server.rs
Ruling: tree applies save validation; create_session refuses flagged templates; get_template returns parsed template + error — cost: wire change
Ruling: supervisor hello/list_runtimes always advertise runtime_url; hello reports runtime_error — cost: wire change
Ruling: model discovery re-run on list_catalog/save/create; runtimes+hosts fixed at startup — cost: restart for new runtime
Ruling: worker link bound to harness identity; swap mid-turn → turn_error "model changed" — cost: none
Ruling: only commit/create/rename bump updated — cost: none
Ruling: duplicate/multi-local hosts rejected at startup; template host validated — cost: none
FINAL FIX WAVE: 43b99d25 (hello carries runtime_url/runtime_error; discovery also on list_templates). Scoped re-review dispatched
FINAL RE-REVIEW: 14/14 fixed, deviations accepted; 2 new minor (N1 panic leaves request unanswered; N2 misleading 'client connection closed' on detach). Ruling: owner's fix-all-minors rule overrides 'no second wave' — small follow-up to afc265f195f1c1005 + rustfmt drift; controller spot-check — cost: small
FINAL FOLLOW-UP: e522991a (N1 catch_unwind→request failed; N2 session detached; rustfmt drift). Controller spot-check OK. Final review closed. Task 11 (deploy+live proof) awaits owner go to push.
