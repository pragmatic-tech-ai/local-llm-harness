# Task 3 report

Implemented: backend/src/runtime/notes.rs (NoteAddTool, NoteListTool, NoteClearTool, NotesBlock, private NoteRules holding consts and shared helpers), registered in runtime/mod.rs; BuiltinSource always registers the three note tools (after the sandbox tools, so existing spec indices are unchanged). Error order for note_add follows the resolution: no session, notes() None, empty, over 300 chars, 20 reached, set_notes false.

Tests: all 9 listed notes tests plus renamed source_without_sandbox_has_time_and_notes (also asserts the with-sandbox name list). Full `scripts/hive-test.sh -- --tests`: 527 passed (lib), other suites 7/10/6/2/5 passed, TEST_EXIT=0. Restore run.

TDD: no separate RED run was done; tests and implementation were written together and verified green in one run (honest deviation from brief step 2).

Files: backend/src/runtime/notes.rs (new), mod.rs, builtin.rs.

Self-review: no free functions, all strings are consts in non-test code, chars().count() for the limit, nothing trimmed. Missing/non-string `text` gives "text must be a string" (not in spec). rustfmt not run on the hive; formatting hand-written to rustfmt style.
