# Final whole-branch review findings (base e220d89c, head ecdb5f1e) — fix ALL

I1 (Important) builtin.rs FileWork::run (~237) + workspace.rs Workspace::open create_dir_all (~45): read tools (list_dir/read_file) on a deleted session recreate sessions/{id} (only run_write checks session_deleted). Also run_write checks before spawn_blocking (race window). Fix: apply session-deleted refusal (`session deleted`) in FileWork::run too; move the check inside the blocking closure immediately before Workspace::open; test: scope with exists()==false → list_dir → assert sessions/ not created.

M1 template.rs:391,404 tests workspace_named_round_trips / unknown_workspace_mode_is_rejected: format! strings with raw newlines → use \n like neighbours.
M2 template_store.rs:292 save normalizes workspace but untested → test: save shared+name, re-read, name None.
M3 main.rs:160-166 registry.specs() called twice → collect once, derive tools and writing_tools.
M4 Sandbox::normalise / Workspace::confine accept Windows-hostile components: reserved device names (CON, PRN, AUX, NUL, COM1-9, LPT1-9, case-insensitive, with or without extension), ADS (`:` in a component), trailing dot/space. Refuse in normalise with a named const error (e.g. `invalid file name: {name}`) + tests.
M5 builtin.rs:733 DeletePathTool::delete: resolve_write canonicalises the leaf, so deleting an inner link deletes its target. Fix: canonicalise parent only, symlink_metadata on the leaf, remove the link itself (remove_dir for a dir junction/symlink, remove_file for file symlink); root check still applies; test.
M6 append_file writes through host-planted hard link: add code comment in AppendFileTool::append and one sentence in docs/specs/2026-11-24-longer-tasks-design.md Anti-goals (accepted limitation: only a host user can plant one).
M7 builtin.rs:539 WriteFileTool::replace temp name `.{name}.{pid}-{nanos}.tmp` breaks long names → short fixed-width temp name e.g. `.wf-{counter:08x}.tmp` (atomic counter, keep create_new) + test with a ~240-char name.
M8 duplicate consts: "." in Sandbox::CURRENT, FileWork::CURRENT, Workspace::CURRENT; "/" in Sandbox::SEPARATOR and Workspace::SLASH; path_schema/path_and_content_schema write "path"/"content" inline → one home each; schemas keyed by FileWork::PATH/CONTENT.
M9 notes.rs:77,112,142 tool names, descriptions, "text" schema key inline → consts; schema uses NoteRules::TEXT_ARG.
M10 notes.rs:18-19 TOO_LONG/LIMIT texts hard-code 300/20 apart from MAX_CHARS/MAX_NOTES → tie them (test asserting texts contain the constants' values, or const assertion).
M11 no test for set_notes()==false branch (notes.rs:66,131) → fake whose notes() succeeds but set_notes() returns false → `session deleted`.
M13 compaction.rs (~2588, notes_block_follows_summary_note): summary-measure filter .all() vacuous → assert filtered set non-empty first.
M14 session fake duplicated: FakeSession in builtin.rs tests, notes.rs tests, tool_loop.rs tests; Alive in tool.rs tests → one configurable fake in runtime::test_support.
M15 tool_loop.rs:~2424 Retry::HINT restates RepeatGuard::HINT → assert verbatim text against prod const once, then use the const.
M16 server.rs:~305 SessionNotes::notes clones whole record → add SessionStore::notes(id) -> Option<Vec<String>>.
M17 server.rs:~309 set_notes maps any SessionError (incl. disk) to false → tool says `session deleted`. Fix: log the underlying error with a named const before returning false when the session still exists (or extend the trait to a Result — your call, keep it small).
M18 server.rs tests literals "SharedAgent", "NamedAgent", "nope" → consts on the Workspaces test helper.
M19 app TemplateEditorViewModel.ts:~1027 Fingerprint includes workspaceName even when mode≠Named → fingerprint name only when Named.
M20 TemplateEditorViewModel.ts:~899 Named + empty/invalid name → add client-side field error (same rule: 1-64 of A-Z a-z 0-9 _ -; messages `workspace name is required for named mode` / `workspace name must be 1-64 letters, digits, _ or -`) included in HasFieldErrors like other fields; tests.
M21 tests missing: IsNotesExpanded toggle; Reset clears notes on session switch (SessionViewModel.ts:~344).
M22 RuntimeClient.ts:62-63 import order of NotesChanged/NotesResult → alphabetical.
M23 tool_loop.rs:~112 RepeatGuard::observe keys parse-error calls by name+arguments (usually null) → include the raw call text in the key when parse_error is set (look at ToolCallRequest / the parsed call for the raw text available; if none, key parse-error calls by name + parse_error message).
M24 notes.rs:~169 NotesBlock::text renders a note containing "\n" verbatim, breaking numbering → note_add collapses any run of \r/\n into a single space before storing (and validate length after collapsing); test.
M25 workspace.rs:~45 Workspace::open called by every file tool incl. reads; if {sandbox}/sessions (or shared/workspaces) is an ordinary file, even `/sandbox/notes.txt` reads fail → `/sandbox/` reads resolve against the sandbox root without opening/creating the write folder; test.
M26 app TraceViewModel.ts:~58 and agents.ts:~199 doc wording "repeated one that already failed twice" → "repeated a call that just failed the same way".
M27 app SessionViewModel.ts:~304 ShowNotes uses this.Update(() => { }) to refresh HasNotes → raise HasNotes (and what actually changed) directly.

(M12 = Task 3 RED step not run — process only, nothing to fix.)
