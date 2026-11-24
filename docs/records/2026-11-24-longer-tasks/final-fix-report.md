# Final-review fix report (feat/longer-tasks, base ecdb5f1e)

Commits:
- `b6769ff0` fix(runtime): file-tool lifecycle and confinement (I1, M4, M5, M6, M7, M8, M14, M25)
- `81baa36d` fix(runtime): notes, retry guard and store polish (M1, M2, M3, M9, M10, M11, M13, M15, M16, M17, M18, M23, M24)
- `eb15e679` fix(app): workspace name validation, notes refresh and wording (M19, M20, M21, M22, M26, M27)

## Per finding

- **I1.** `FileWork::run` (backend/src/runtime/builtin.rs:274) now covers every file tool, reads included. It clones the scope and checks `session_deleted()` inside the `spawn_blocking` closure, immediately before `work` (which opens the workspace first). `run_write` is removed, so the check no longer runs before the spawn. Test: `reads_after_session_deleted_are_refused_and_create_nothing` (builtin.rs:1602). It runs list_dir, list_dir `/sandbox` and read_file on a deleted session, and asserts `session deleted` and that `sessions/` does not exist. Writes are still covered by `write_after_session_deleted_is_refused` (:1527).
- **M1.** template.rs:390 and :400: the format strings use `\n` escapes. Covered by the same two tests.
- **M2.** Test `save_drops_name_on_a_non_named_workspace` (template_store.rs:1286). It saves `shared` with a name, re-reads it, gets `name == None`, and the file does not contain the name.
- **M3.** main.rs:158: `registry.specs()` is called once, and both `tools` and `writing_tools` come from it.
- **M4.** `Sandbox::check_name` (builtin.rs:109) is called from `normalise` for every Normal component. It refuses reserved device names (CON/PRN/AUX/NUL/COM1-9/LPT1-9, case-insensitive, with or without an extension, and with a trailing space before the dot), any `:`, and a trailing `.` or space. The error is `invalid file name: {name}` (const `INVALID_NAME`). All values are named consts. Tests: `windows_hostile_names_are_refused` (:1648, with bad and good tables) and `write_tools_refuse_hostile_names` (:1685).
- **M5.** New `Workspace::resolve_leaf` (workspace.rs:99) confines only the parent and joins the leaf name. `DeletePathTool::delete` uses it. A leaf whose `symlink_metadata` reports a symlink or junction goes to `DeletePathTool::remove_link` (builtin.rs:792): `remove_dir` for a directory link on Windows (`is_symlink_dir`), `remove_file` otherwise. The root check is kept. Tests: `delete_inner_junction_removes_the_link_not_its_target` (:1715) and `delete_junction_to_outside_removes_only_the_link` (:1734, which also covers `out2/keep.txt` → `path escapes the sandbox`). `write_through_inner_junction_is_refused` (workspace.rs) now uses `delete_path link/new.txt` for the refusal case, and asserts that deleting `link` itself removes only the junction.
- **M6.** There is a code comment in `AppendFileTool::append` (builtin.rs:663). One sentence was added to the spec's Anti-goals (docs/specs/2026-11-24-longer-tasks-design.md, first bullet): an accepted hard-link limitation that only a host user can create. Documentation only, no test.
- **M7.** `WriteFileTool::replace` (builtin.rs:575) names the temp file `.wf-{pid:08x}{counter:08x}.tmp`: a function-local `static AtomicU32` counter, `create_new` kept, and the `TEMP_PREFIX` and `TEMP_SUFFIX` consts. Test: `write_file_takes_a_name_near_the_length_limit` (:1701) writes a 240-char name and checks that no temp file is left.
- **M8.** `Sandbox::CURRENT` and `Sandbox::SEPARATOR` are now `pub(crate)` and are the one home for these values. `FileWork::CURRENT`, `Workspace::CURRENT` and `Workspace::SLASH` were removed. `path_schema`, `path_and_content_schema` (builtin.rs:308) and the list_dir and read_file schemas use `FileWork::PATH`, `FileWork::CONTENT` and `ReadFileTool::MAX_BYTES`. The max_bytes description is a const. Covered by the existing schema and tool tests.
- **M9.** notes.rs:73/128/162: each note tool has `NAME` and `DESCRIPTION` consts. The schema key, the `required` entry and the note_list output key use `NoteRules::TEXT_ARG`. The description is `TEXT_DESCRIPTION`. A shared `NoteRules::no_arguments()` builds the empty schema. Covered by `source_without_sandbox_has_time_and_notes` and the notes tests.
- **M10.** Test `rule_texts_match_the_limits` (notes.rs:398) asserts that `TOO_LONG`, `LIMIT` and the note_add description contain `MAX_CHARS` and `MAX_NOTES`.
- **M11.** Test `failed_save_reports_session_deleted` (notes.rs:364) uses `FakeSession::refusing_saves()`, whose `notes()` succeeds while `set_notes` returns false. note_add and note_clear both answer `session deleted`.
- **M13.** compaction.rs:2588: the summary-measure filter is collected first, the test asserts it is non-empty, then checks `.all(...)`.
- **M14.** There is now one configurable `FakeSession` in runtime::test_support (test_support.rs:305), with `alive`, `deleted`, `refusing_saves`, `delete` and `revive`. The fakes in builtin.rs, notes.rs and tool_loop.rs tests and `Alive` in tool.rs tests are removed, and those tests use the shared fake.
- **M15.** tool_loop.rs: `Retry::HINT` is removed. Test `retry_hint_text_is_verbatim` (:2643) asserts `RepeatGuard::HINT` and `FIELD_HINT` verbatim, and the fixtures use `RepeatGuard::HINT` (:2548).
- **M16.** `SessionStore::notes(id)` (session_store.rs:187) is used by `SessionNotes::notes` (server.rs:308). Covered by `set_notes_persists_and_reloads`, which now also asserts `notes(id)` and `notes("nope") == None`.
- **M17.** `SessionNotes::set_notes` (server.rs:~313) logs `notes not saved for {id}: {err}` (const `ERR_NOTES_NOT_SAVED`) when the save fails while the session still exists, then returns false. I kept the trait as bool to keep the change small. Not unit-tested: it is a log line on a disk error.
- **M18.** The server tests use `Workspaces::SHARED_AGENT`, `NAMED_AGENT` and `MISSING` consts (server.rs:2898).
- **M23.** `RepeatGuard` is keyed by a new `CallKey` (tool_loop.rs:109): name, arguments, and for a parse error `(raw, parse_error)`. `observe` takes the `ParsedCall` (:134). Tests: `repeat_guard_compares_name_and_arguments` was ported, and `repeat_guard_tells_unparsed_calls_apart_by_raw_text` (:2631) is new.
- **M24.** `NoteRules::one_line` (notes.rs:39) collapses each run of `\r`/`\n` into one space before the empty and length checks. Test: `line_breaks_collapse_to_one_space` (:374), covering the stored text, the rendered block, 300 chars after collapsing, and breaks-only → `note text is empty`.
- **M25.** New `Workspace::open_read` (workspace.rs:57). A `/sandbox` read only locates the folder (via `confine`, so existing links are still checked) and does not create or require it. Other reads still open (and create) the folder. list_dir and read_file use it. Test: `sandbox_reads_work_when_the_write_folder_cannot_be_made` (builtin.rs:1621). With `sessions` as a file, the `/sandbox/notes.txt` read and the `/sandbox` list both work, and a plain list gives `cannot create workspace folder: ...`.
- **M19.** `TemplateEditorViewModel.Fingerprint` includes the workspace name only when the mode is Named. Test: `workspace_name_counts_toward_dirty_only_when_named`.
- **M20.** New `WorkspaceNameError` and `HasWorkspaceNameError` use the backend rule `/^[A-Za-z0-9_-]{1,64}$/` on the trimmed name, with the verbatim messages as `private static readonly` consts. They are raised via DerivedProps and included in `HasFieldErrors`, so Save is blocked. TemplateEditor.mu shows the error under the name box. Test: `named_workspace_needs_a_valid_name`, covering blank, invalid, 64 vs 65 chars, padded input, and another mode ignoring the name.
- **M21.** Tests `notes_expander_toggles_and_notifies` and `switching_sessions_clears_notes_at_once` in SessionViewModel.test.ts.
- **M22.** RuntimeClient.ts: `NotesChanged` and `NotesResult` moved into alphabetical position in the type import.
- **M26.** agents.ts (`repeat_failure` doc) and TraceViewModel.ts (`RepeatText` doc) now read "repeated a call that just failed the same way".
- **M27.** `SessionViewModel.ShowNotes` raises only `HasNotes` (with its old and new value) through a new `PropHasNotes` const. Test: `notes_change_raises_has_notes_only` asserts that no header properties are raised.

## Test runs

- `scripts/hive-test.sh -- runtime::`: 300 passed, 0 failed (while iterating).
- `scripts/hive-test.sh -- agents::`: 102 passed, 0 failed (while iterating).
- `scripts/hive-test.sh -- --tests`: lib 562 passed, plus the integration binaries with 7, 10, 6, 2 and 5 passed. 0 failed, TEST_EXIT=0, no compiler warnings.
- `scripts/hive-test.sh --features llamacpp`: 570 passed, 0 failed, TEST_EXIT=0, no warnings.
- `scripts/hive-test.sh --restore`: RESTORED.
- `rustfmt --edition 2021 --check` on every touched .rs file: clean. This is the standalone formatter, not cargo.
- `cd app && npm test`: 17 files, 217 tests passed.
- `npm run typecheck:web` and `npm run typecheck:node`: clean.

## R1-R5 (re-review residual minors), commit `c20ac4bd`

- **R1.** The reserved-name rule now has one home, `Sandbox::is_reserved_device_name` (backend/src/runtime/builtin.rs:153). `Sandbox::check_name` uses it. `WorkspaceConfig::validate` (backend/src/agents/template.rs:171) calls it after the character rule, because agents already depends on `runtime::worker`, so this adds no new layering direction. It refuses with `ERR_WORKSPACE_NAME_RESERVED` = `workspace name is a reserved Windows device name`.
  - Client side: `TemplateEditorViewModel.WorkspaceNameError` returns the same message, a `private static readonly WorkspaceNameReserved`. It checks against `ReservedDeviceNames`, a `private static readonly` Set (TemplateEditorViewModel.ts:71) that mirrors `Sandbox::RESERVED`.
  - Tests: `reserved_device_workspace_name_is_rejected` (template.rs:445) refuses con/NUL/Aux/prn/com1/LPT9 and accepts console/com10/lpt/nul_x. The TS test `named_workspace_needs_a_valid_name` was extended with the same two lists.
- **R2.** `Sandbox::RESERVED` (builtin.rs:38) now has 30 entries. It adds `CONIN$`, `CONOUT$`, `COM¹`, `COM²`, `COM³`, `LPT¹`, `LPT²` and `LPT³`, written as `\u{b9}`, `\u{b2}` and `\u{b3}`.
  - Test: `windows_hostile_names_are_refused` (builtin.rs:1707) adds these names, with and without extensions and in mixed case. It also adds the near-misses `com\u{b9}0` and `conin`, which must still be accepted.
- **R3.** `SessionViewModel.Props` now starts with `SessionViewModel.PropHasNotes`. Covered by the existing notes tests.
- **R4.** File tool result keys and values live on `FileWork` (builtin.rs:~306): `BYTES`, `TRUNCATED`, `DELETED`, `ENTRIES`, `ERRORS`, `NAME`, `KIND`, `SIZE` and `KIND_DIR`/`KIND_FILE`/`KIND_OTHER`. `PATH` and `CONTENT` are reused for the result keys.
  - `DeletePathTool`'s own `FILE`/`DIR` were removed in favour of `FileWork::KIND_*`.
  - `GetTimeTool` has `UTC`, `LOCAL`, `UTC_OFFSET_MINUTES` and `UNIX`, plus `CLOCK_BEFORE_EPOCH` for its error.
  - The note tools use `NoteRules::NOTES`, `NUMBER`, `COUNT` and `CLEARED` (notes.rs:24).
  - Every result `json!` uses the const-key form. Covered by the existing exact-match result tests: list_dir, read_file, write, append, make_dir, delete and notes.
- **R5.** `Retry::failing` calls `Notes::call(Self::ID, Self::NOPE, args)`, and the new `Retry::NOPE` const is at tool_loop.rs:2427.

Test runs:
- `scripts/hive-test.sh -- --tests`: lib 563 passed, plus the integration binaries with 7, 10, 6, 2 and 5 passed. 0 failed, TEST_EXIT=0, no warnings.
  - The first attempt did not compile: the `NOPE` const had been inserted into the wrong fixture, `E0599`. I moved it to `Retry` and the rerun was green.
- `scripts/hive-test.sh --features llamacpp`: 571 passed, 0 failed, TEST_EXIT=0.
- `scripts/hive-test.sh --restore`: RESTORED.
- `rustfmt --check` on the touched .rs files: clean.
- `npm test`: 17 files, 217 passed.
- `npm run typecheck:web` and `npm run typecheck:node`: clean.
