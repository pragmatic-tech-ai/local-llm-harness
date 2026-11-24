# SDD ledger — plan: docs/plans/2026-11-24-longer-tasks.md
Spec: docs/specs/2026-11-24-longer-tasks-design.md. Branch feat/longer-tasks (in place, from main @e220d89c).

## Pre-flight
| Pair / task | Produces vs consumes | Finding |
|---|---|---|
| T1↔T5 | WorkspaceConfig.folder/mode, TemplateRules.writing_tools, SessionStore::set_notes → server+seeds | consistent |
| T2↔T3 | SessionHandle/ToolScope (T2) → note tools (T3); both edit builtin.rs | sequential, ok |
| T2↔T4 | execute(.., scope) default in T2, real in T4 | ok |
| T2↔T5 | Workspace::remove_folder → delete_session | ok |
| T3↔T4 | NotesBlock::text → loop/compactor | ok |
| T4↔T6 | ToolTiming.repeat_failure → app tag | ok |
| T5↔T6 | get_notes / notes_changed → app | ok |
| T1 self | DiscoveredCatalog holds `tools: Vec<String>` given at construction; writing_tools must be passed the same way | Ruling: DiscoveredCatalog gains `writing_tools` filled in main.rs from registry specs with side_effects — plan says "fills it from the registry specs" — cheap to redo |
| T2 self | tests vs code consistent; read tests unchanged via scope None | ok |
| T3 self | "trims nothing" + reject whitespace-only consistent | ok |
| T4 self | ok | ok |
| T5 self | ok | ok |
| T6 self | ok | ok |
| T7 self | push needs owner go | stop point |

## Tasks
Task 1: implemented ad481ea7 (base e220d89c), implementer aa011e5cca05ce0ef
Task 1: minor (deferred): template_store.rs test fixture `workspace:` line mis-indented (rustfmt)
Task 1: minor (deferred): template.rs tests format! strings with raw newlines — use \n (rustfmt)
Task 1: minor (deferred): no test for TemplateStore::save normalizing (shared+name → name dropped on re-read)
Task 1: minor (deferred): main.rs registry.specs() called twice — collect once
Task 1: complete (commits e220d89..ad481ea, review clean; 4 minors deferred to final wave)
Task 2: implemented b5dafebf (base ad481ea7), implementer affd6bd5e847c93f1
Task 2: minor (deferred): Windows reserved device names (CON/NUL/COM1...) and ADS `name:stream` components not refused in confine — refuse + test
Task 2: minor (deferred): delete_path follows an inner link to its target; should delete the link itself (canonical parent + symlink_metadata leaf)
Task 2: minor (deferred): append_file writes through a host-planted hard link — document as accepted limitation (spec/code comment)
Task 2: minor (deferred): write_file temp name adds ~30 chars → long names fail only in write_file; use short fixed-width temp name
Task 2: minor (deferred): three "." CURRENT consts (Sandbox/FileWork/Workspace) + inline "path"/"content" in schema builders — one const each
Task 2: ⚠️ production passes ToolScope::default() until Task 4 — Task 4 must thread the real scope (carried into T4 dispatch); no seed enables write tools before Task 5
Task 2: complete (commits ad481ea..b5dafeb, review clean; 5 minors deferred)
Task 3: implemented 8f64ff3d (base b5dafebf), implementer a2f0017104d0bbb59; TDD RED step skipped (noted)
Task 3: minor (deferred): notes.rs tool names/descriptions/"text" schema key inline — hoist consts, schema uses TEXT_ARG
Task 3: minor (deferred): TOO_LONG/LIMIT texts hard-code 300/20 apart from MAX_CHARS/MAX_NOTES — tie them (const assert or comment)
Task 3: minor (deferred): no test for set_notes()==false → `session deleted` path
Task 3: minor (deferred): no RED run evidence (disclosed)
Task 3: complete (commits b5dafeb..8f64ff3, review clean; 4 minors deferred)
Task 4: implemented 2b03f590 (base 8f64ff3d), implementer a61d7179a92d1ce81
Task 4: minor (deferred): compaction.rs:294-298 summary-measure assertion can pass vacuously — assert filtered set non-empty / classify by tools.is_empty()&&!thinking
Task 4: minor (deferred): FakeSession duplicated (tool_loop.rs tests + notes.rs tests) — one configurable fake in runtime::test_support
Task 4: minor (deferred): test Retry::HINT restates RepeatGuard::HINT — assert equality once, use prod const
Task 4: ⚠️ server.rs:302 turn_config still ToolScope::default() — Task 5 must replace + add server-level test that a turn's calls see the session scope (carried into T5)
Task 4: complete (commits 8f64ff3..2b03f59, review clean; 3 minors deferred)
Task 5: implemented dc8b98c3 (base 2b03f590), implementer a0e940e0f0a3f41a2
Task 5: ⚠️ concurrent note calls → checked: tool_loop runs a step's calls sequentially (`for call in &calls { execute().await }`), so no lost update. Resolved.
Task 5: minor (deferred): SessionNotes::notes() clones whole record — add SessionStore::notes(id)
Task 5: minor (deferred): set_notes bool → disk error reported as `session deleted`; log the underlying error (or Result)
Task 5: minor (deferred): inline test literals "SharedAgent"/"NamedAgent"/"nope" — consts
Task 5: complete (commits 2b03f59..dc8b98c, review clean; 3 minors deferred)
Task 6: implemented ecdb5f1e (base dc8b98c3), implementer acc12638f0e2a4e77
Task 6: ⚠️ wire names checked: server get_notes/notes_changed {id, notes} == RuntimeClient constants. Resolved.
Task 6: minor (deferred): WorkspaceName in fingerprint when mode≠Named → spurious dirty; fingerprint name only when Named
Task 6: minor (deferred): no client-side field error for empty/invalid Named name (server error shows via ErrorText)
Task 6: minor (deferred): no tests for IsNotesExpanded toggle / Reset clearing notes on session switch
Task 6: minor (deferred): RuntimeClient.ts import order (NotesChanged/NotesResult)
Task 6: complete (commits dc8b98c..ecdb5f1, review clean; 4 minors deferred)
Final review: 0 C, 1 I (read tools recreate deleted session folder), 27 M; fix wave dispatched (base ecdb5f1e)
Final fix wave: commits ecdb5f1..eb15e67 (implementer abdf566a7a1228416); fixer calls: M5 delete_path on outward junction deletes the link; M17 logging only; M4 device-name check in normalise
Final re-review: all 27 findings ADDRESSED; fixer calls M5/M17 accepted. Residual minors (R1-R5) → second small fix round per owner's fix-all-minors rule (overrides skill's single-wave rule):
R1 Named workspace `con`/`nul`/`com1` passes WorkspaceConfig::validate + client check but fails at tool time — add reserved-name check to both
R2 Sandbox::RESERVED misses CONIN$, CONOUT$, COM¹-³, LPT¹-³
R3 SessionViewModel Props spells 'HasNotes' inline beside PropHasNotes
R4 tool result keys inline in builtin.rs/notes.rs (path, bytes, deleted, truncated, content, notes, number, count, cleared)
R5 tool_loop tests Retry::failing passes "c" inline despite Retry::ID
Residual round R1-R5: commit c20ac4bd, re-review all ADDRESSED, no new breakage. Branch clean.
