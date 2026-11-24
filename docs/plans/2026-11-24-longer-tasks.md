# Longer Tasks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give agents a template-chosen workspace they can write files into, a notes scratchpad that survives compaction, and a hint out of repeated failing calls.

**Architecture:**
- Templates gain a `workspace` table. The server maps it to a folder relative to the sandbox and hands it to each tool call, through a new `ToolScope` on `ToolContext`.
- `runtime/workspace.rs` resolves reads and writes against that folder, with the `/sandbox/` prefix for reads.
- Four write tools and three note tools join the built-ins.
- Notes live on the session record and are reached through a `SessionHandle` trait that `runtime` declares and the server implements over the store and the hub.
- The `Compactor` appends the `[Your notes]` block to every outgoing system message and measures it.
- The loop's `RepeatGuard` adds the retry hint.
- `ReadOnlyPolicy` gives way to `EnabledPolicy`.
- The app gains a Workspace row, a "writes" tag, a Notes panel and a "repeated failure" tag.

**Tech Stack:**
- Rust: tokio, serde, async-trait, all vendored.
- App: Electron and mural TypeScript, tested with vitest.

**Spec:** `docs/specs/2026-11-24-longer-tasks-design.md`. It builds on:
- `docs/specs/2026-11-23-agent-templates-sessions-design.md`
- `docs/specs/2026-11-24-agent-trace-design.md`
- `docs/specs/2026-11-24-context-compaction-design.md`

## Global Constraints

- **Dependencies:** no new crates (the build is vendored and offline), Cargo.lock unchanged, and no npm install.
- **Where Rust runs:** only on the hive, through `scripts/hive-test.sh [--features F] [-- <cargo args>]`, ending with `scripts/hive-test.sh --restore`. Never run cargo on the dev machine.
- **Rust style:**
  - Idiomatic snake_case and rustfmt.
  - Helpers are associated functions, with no new free functions (tests included).
  - Every error or user-visible string is a named const.
- **TypeScript style:**
  - OOP only.
  - Allman braces and no semicolons.
  - PascalCase for public members.
  - Wire fields keep their snake_case.
  - Literals are `private static readonly` PascalCase constants.
  - VMs extend Observable.
  - `.mu` labels stay inline.
- **Workspace modes:**

  | Mode | Folder, relative to the sandbox |
  |---|---|
  | `session` | `sessions/{session id}` |
  | `shared` | `shared` |
  | `named` | `workspaces/{name}` |

  A missing `[workspace]` table, in templates and in old session snapshots, means `session`. A name on a mode other than `named` is dropped.
- **Workspace errors, verbatim:**
  - `unknown workspace mode: {v}`
  - `workspace name is required for named mode`
  - `workspace name must be 1-64 letters, digits, _ or -`
- **Paths:**
  - Tool paths are relative to the write folder.
  - `/sandbox` or `/sandbox/...` reads from the sandbox root.
  - Writes there fail with `writes are confined to your workspace`.
  - Everything else follows today's `Sandbox::resolve` rules and messages (`path must be relative to the sandbox`, `path escapes the sandbox`, `no such path: {path}`).
  - Displayed paths are relative to the write folder, or `/sandbox/...` outside it. Host paths are never shown.
- **Write limits and errors, verbatim:**

  | Condition | Error |
  |---|---|
  | `content` over 262144 bytes per call | `content exceeds 262144 bytes` |
  | a file over 1048576 bytes after the write | `file would exceed 1048576 bytes` |
  | writing over a folder | `path is a directory` |
  | `make_dir` over a file | `path is a file` |
  | deleting a non-empty folder | `directory is not empty` |
  | deleting `.` or an empty path | `cannot delete the workspace root` |
  | any write when the session is gone | `session deleted` |

- **Notes, verbatim:**
  - Errors:
    - `note text is empty`
    - `note exceeds 300 characters`, counted in chars
    - `note limit reached (20); use note_clear to start over`
    - `notes need a session`, when there is no session scope
  - The block is `[Your notes]\n1. {first}\n2. {second}`.
  - It follows the system content, and any summary note, after a blank line.
- **Retry hint:** the key is `"hint"`, and the text is exactly `This call already failed twice; try something different.`
- **Locks:** never hold the session store's lock or the hub's lock across a worker call or a tool's file work.
- **Schemas:**
  - Session JSON stays at schema 1. The new `notes` field is serde default empty and skipped when empty.
  - Trace records stay `v: 1`. `repeat_failure` is skipped when false.
- **Commits:** use `git -c user.name="Eugene Napryaglo" -c user.email="napryaglo@users.noreply.github.com" commit`, and end each message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Work on the branch `feat/longer-tasks`, and never push, except in Task 7 after the owner's go.

## Review Focus

1. **A write through a junction or symlink that already exists inside the workspace and points outside.** It is refused with `path escapes the sandbox`, and nothing is written outside. This holds even when the target file does not exist yet. Covered in Task 2: `write_through_inner_junction_is_refused`.
2. **Deleting a `session` workspace that contains a junction to a folder outside.** The junction is removed and the outside folder's contents survive. Covered in Task 5: `delete_session_does_not_follow_junctions`.
3. **Notes on a session with no system prompt, under summarize.** The block arrives as a system message of its own. The saved summary's `covers` index still refers to the stored transcript, so reuse on the next turn works. Covered in Task 4: `notes_block_without_system_prompt_keeps_summary_reuse`.
4. **A note with non-ASCII text.** 300 Cyrillic or emoji characters are accepted, and 301 are refused, because the limit counts chars, not bytes. Covered in Task 3: `note_limit_counts_chars_not_bytes`.
5. **An append that would make a file exceed 1 MiB.** It is refused, and the file is left unchanged on disk. Covered in Task 2: `append_over_file_cap_leaves_file_unchanged`.

---

### Task 1: The template `workspace` field, session notes, and the rules for writing tools

**Files:**
- Modify: `backend/src/agents/template.rs`
- Modify: `backend/src/agents/session.rs`
- Modify: `backend/src/agents/session_store.rs`
- Modify: `backend/src/runtime/catalog_source.rs`, plus every `TemplateRules { .. }` literal in tests

**Interfaces:**
- Produces:
  - **`WorkspaceMode`.** `pub enum WorkspaceMode { Session, Shared, Named }`, with `Default = Session`. It serializes through the same `ALL` table pattern as `ContextStrategy`, with the wire names `session`, `shared` and `named`. An unknown value gives `unknown workspace mode: {v}`.
  - **`WorkspaceConfig`.** `pub struct WorkspaceConfig { pub mode: WorkspaceMode, pub name: Option<String> }`. It derives `Default`, and its `name` is serde default and skipped when `None`.
    - `pub fn folder(&self, session_id: &str) -> PathBuf` returns the relative folder from the Global Constraints table, built from components rather than a joined string.
    - `pub fn normalized(self) -> Self` drops the name unless the mode is `Named`.
  - **`AgentTemplate.workspace`.** `pub workspace: WorkspaceConfig`, with `#[serde(default)]`. In TOML it is the `[workspace]` table.
  - **`TemplateRules.validate`.** It checks the name rules for `named`. The store's save path saves `workspace.normalized()`.
  - **`TemplateRules.writing_tools`.** A new field, `pub writing_tools: Vec<String>`: the names of the registry tools with `side_effects`. `catalog_source.rs` fills it from the registry specs.
  - **`SessionRecord.notes`.** A new field, `pub notes: Vec<String>`. It is serde default and skipped when empty.
  - **`SessionStore::set_notes`.** `pub fn set_notes(&self, id: &str, notes: Vec<String>) -> Result<(), SessionError>` works through `mutate`. An unknown id gives `SessionError::unknown`.
  - **`SessionStore::reset`.** It also clears `notes`.

- [ ] **Step 1: Write the failing tests.**
  - In template.rs:
    - `workspace_defaults_to_session_when_missing`: TOML with no table gives `Session` and `None`.
    - `workspace_named_round_trips`
    - `unknown_workspace_mode_is_rejected`: the error contains `unknown workspace mode: bogus`.
    - `named_without_name_is_rejected`: missing and `""` both give `workspace name is required for named mode`.
    - `bad_workspace_name_is_rejected`: `"a b"`, `"x/y"` and a 65-character name all give `workspace name must be 1-64 letters, digits, _ or -`. 64 characters of `[A-Za-z0-9_-]` are accepted.
    - `name_on_other_mode_is_dropped_by_normalized`
    - `folder_maps_each_mode`: `sessions/s-1-0`, `shared`, `workspaces/research`.
  - In session_store.rs:
    - `set_notes_persists_and_reloads`
    - `reset_clears_notes`
    - `set_notes_on_unknown_session_errors`
    - `old_record_without_notes_loads_empty`
- [ ] **Step 2: Watch them fail.** Run `scripts/hive-test.sh -- agents::`. Expected: FAIL, as the items don't exist yet.
- [ ] **Step 3: Implement** the items above. Existing `AgentTemplate { .. }` literals gain `workspace: WorkspaceConfig::default()`. Existing `TemplateRules { .. }` literals gain `writing_tools: vec![]`.
- [ ] **Step 4: Watch them pass.** Run `scripts/hive-test.sh -- --tests`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(agents): template workspace field and session notes`.

### Task 2: Workspace paths, write tools, and the enabled-tools policy

**Files:**
- Create: `backend/src/runtime/workspace.rs`, registered in `runtime/mod.rs`
- Modify: `backend/src/runtime/tool.rs`
- Modify: `backend/src/runtime/builtin.rs`
- Modify: `backend/src/runtime/executor.rs`
- Modify: `backend/src/runtime/policy.rs`
- Modify: `backend/src/main.rs` (`EnabledPolicy`)

**Interfaces:**
- Produces:
  - **The session handle** (tool.rs):

    ```rust
    pub trait SessionHandle: Send + Sync {
        /// The session's notes; None when the session no longer exists.
        fn notes(&self) -> Option<Vec<String>>;
        /// Replaces the notes; false when the session no longer exists.
        fn set_notes(&self, notes: Vec<String>) -> bool;
        fn exists(&self) -> bool;
    }
    #[derive(Clone, Default)]
    pub struct ToolScope {
        /// The write folder, relative to the sandbox; None means the sandbox root.
        pub workspace: Option<PathBuf>,
        pub session: Option<Arc<dyn SessionHandle>>,
    }
    ```

    - `ToolScope` gets a hand-written `Debug`, and a `PartialEq` that compares `workspace` and the sessions by `Arc::ptr_eq`, both `None` being equal.
    - `ToolContext` gains `pub scope: ToolScope`.
  - **The executor.** `ToolExecutor::execute(&self, call: &ToolCallRequest, enabled: &[String], scope: &ToolScope) -> ToolOutcome`. It puts a clone of the scope into the `ToolContext`.
  - **The policy.** `pub struct EnabledPolicy;` returns `Decision::Allow` for every call. `ReadOnlyPolicy` is deleted.
  - **`Workspace`** (workspace.rs):
    - `pub fn open(sandbox: &Sandbox, folder: Option<&Path>) -> Result<Workspace, String>` creates the folder (`create_dir_all`), canonicalizes it, and checks that it lies inside the sandbox.
    - `pub fn resolve_read(&self, path: &str) -> Result<PathBuf, String>`
    - `pub fn resolve_write(&self, path: &str) -> Result<PathBuf, String>`. The target may not exist; its nearest existing ancestor is canonicalized and must lie inside the folder.
    - `pub fn display(&self, resolved: &Path) -> String`
    - `pub fn is_root(&self, resolved: &Path) -> bool`
    - `pub const SANDBOX_PREFIX: &'static str = "/sandbox"`
    - `pub fn remove_folder(sandbox: &Sandbox, folder: &Path) -> io::Result<()>` uses `fs::remove_dir_all`, which does not follow links on current Rust. A `NotFound` error is `Ok`. Task 5 uses it.
    - The base-relative resolution is shared with `Sandbox::resolve` (one lexical-normalise-then-canonicalise routine, taking a base).
  - **The tools** (builtin.rs):
    - `list_dir` and `read_file` open `Workspace::open(&self.sandbox, ctx.scope.workspace.as_deref())` and resolve with `resolve_read`. Their descriptions end with the spec's sentence: `Paths are relative to your workspace; prefix /sandbox/ to read elsewhere in the sandbox.`
    - `list_dir` of an empty write folder adds `"note": "your workspace is empty; prefix /sandbox/ to list the shared sandbox"` beside `entries`.
    - `WriteFileTool`, `AppendFileTool`, `MakeDirTool` and `DeletePathTool` are named `write_file`, `append_file`, `make_dir` and `delete_path`. All have `side_effects: true`, and their arguments and results are as in the spec's table.
    - Every write tool first returns `session deleted` when `ctx.scope.session` is `Some` and `!exists()`.
    - `write_file` writes a temp file in the target folder and then renames it over the target.
    - All file work runs in `spawn_blocking`.
  - **Registration.** `BuiltinSource` registers the four write tools whenever a sandbox is configured.

- [ ] **Step 1: Write the failing tests** in workspace.rs and builtin.rs, using the existing `TestDir` helper and junction helper.
  - **Paths:**
    - `reads_resolve_against_the_write_folder`
    - `sandbox_prefix_reads_the_root`: covers `/sandbox/notes.txt` and `/sandbox`.
    - `write_to_sandbox_prefix_is_refused`: the error is `writes are confined to your workspace`.
    - `dotdot_out_of_folder_is_refused`: `../x.txt` from `sessions/s1` is refused.
    - `write_through_inner_junction_is_refused` (Review Focus 1)
    - `display_is_relative_or_sandbox_prefixed`
    - `no_scope_folder_is_the_sandbox_root`: today's read tests keep passing unchanged.
  - **Writes:**
    - `write_file_creates_parents_and_reports_bytes`
    - `write_file_overwrites`
    - `content_over_cap_is_refused`: 262145 bytes are refused and 262144 are accepted.
    - `append_creates_then_appends_and_reports_size`
    - `append_over_file_cap_leaves_file_unchanged` (Review Focus 5)
    - `write_over_directory_is_refused`
    - `make_dir_is_idempotent_and_refuses_file`
    - `delete_path_file_and_empty_dir`
    - `delete_non_empty_dir_is_refused`
    - `delete_root_is_refused`: both `.` and `""`.
    - `delete_missing_reports_no_such_path`
    - `write_after_session_deleted_is_refused`: uses a fake `SessionHandle` with `exists() == false`.
  - **Listing:** `empty_workspace_list_carries_note`.
  - **Executor:**
    - `executor_runs_side_effect_tool_when_enabled`
    - `executor_refuses_side_effect_tool_when_not_enabled`: the error is `tool not enabled for this agent: X`. It replaces `executor_denies_side_effects_under_read_only`.
- [ ] **Step 2: Watch them fail.** Run `scripts/hive-test.sh -- runtime::workspace runtime::builtin runtime::executor`. Expected: FAIL.
- [ ] **Step 3: Implement** the items above.
  - Existing `execute` callers pass `&ToolScope::default()` until Task 4 threads the real one.
  - Existing `ToolContext { .. }` literals gain `scope: ToolScope::default()`.
- [ ] **Step 4: Watch them pass.** Run `scripts/hive-test.sh -- --tests`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(runtime): workspace-confined write tools; the tool list is the permission list`.

### Task 3: Note tools and the notes block

**Files:**
- Create: `backend/src/runtime/notes.rs`, registered in `runtime/mod.rs`
- Modify: `backend/src/runtime/builtin.rs` (register the note tools)

**Interfaces:**
- Consumes: `SessionHandle` and `ToolScope` from Task 2.
- Produces:
  - **The tools.** `NoteAddTool`, `NoteListTool` and `NoteClearTool` are named `note_add`, `note_list` and `note_clear`, all with `side_effects: false`.
    - Results: `{number, count}`, `{notes: [{number, text}]}` and `{cleared: n}`.
    - `note_clear` on an empty list returns `{cleared: 0}`.
    - Every tool fails `notes need a session` without `ctx.scope.session`.
    - Every tool fails `session deleted` when `notes()` is `None`, or when `set_notes` returns false.
    - `note_add` trims nothing. It rejects text that is empty or whitespace only.
  - **The block.** `pub struct NotesBlock;` with:
    - `pub const HEADER: &'static str = "[Your notes]"`
    - `pub fn text(notes: &[String]) -> Option<String>`, which is `None` when empty, otherwise the header followed by `\n{n}. {note}` per note
  - **Registration.** `BuiltinSource` always registers the three note tools, with or without a sandbox.

- [ ] **Step 1: Write the failing tests** in notes.rs, using a fake `SessionHandle` over a `Mutex<Option<Vec<String>>>`:
  - `note_add_numbers_from_one`
  - `note_list_returns_numbered_notes`
  - `note_clear_reports_count_and_empties`
  - `empty_note_is_refused`, with `""` and `"  \n"`.
  - `note_limit_counts_chars_not_bytes` (Review Focus 4): 300 `"я"` are accepted, 301 are refused.
  - `twenty_first_note_is_refused`
  - `notes_without_session_scope_fail`
  - `notes_on_deleted_session_fail`
  - `notes_block_text_format`: `NotesBlock::text(&["a".into(), "b".into()]) == Some("[Your notes]\n1. a\n2. b".into())`, and empty gives `None`.
  - Update `source_without_sandbox_has_only_get_time`: it becomes `source_without_sandbox_has_time_and_notes`.
- [ ] **Step 2: Watch them fail.** Run `scripts/hive-test.sh -- runtime::notes runtime::builtin`. Expected: FAIL.
- [ ] **Step 3: Implement** the items above.
- [ ] **Step 4: Watch them pass.** Run `scripts/hive-test.sh -- --tests`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(runtime): note_add, note_list and note_clear`.

### Task 4: Loop wiring: scope per call, the notes block in every prompt, and the retry hint

**Files:**
- Modify: `backend/src/runtime/tool_loop.rs`
- Modify: `backend/src/runtime/compaction.rs`
- Modify: `backend/src/runtime/trace.rs` (`ToolTiming.repeat_failure`)

**Interfaces:**
- Consumes: Tasks 2 and 3.
- Produces:
  - **`TurnConfig.scope`.** `TurnConfig` gains `pub scope: ToolScope`, which every call's `execute` receives.
  - **Notes read every step.** Before each step's `prepare`, the loop reads `config.scope.session.and_then(notes)` and passes `NotesBlock::text(..)` on.
  - **`Compactor::prepare`.** It gains `notes: Option<&str>` before `link`.
    - Every prompt it returns carries the block on the system message, after any summary note and separated by a blank line. That covers off, under budget, trim, summarize and fallback alike.
    - When there is no system message, a system message holding only the block is put first.
    - Every measure includes the block.
    - Saved `CompactionState.covers` keeps indexing the stored transcript.
    - The scratch transcript and the committed transcript never contain the block.
  - **The retry guard.** `struct RepeatGuard { last_failed: Option<(String, Value)> }` lives in tool_loop.rs.
    - `fn observe(&mut self, name: &str, args: &Value, ok: bool) -> bool` returns `true` when this call failed and the remembered call was the same name and equal arguments.
    - After every call it remembers the call when it failed and forgets otherwise.
    - It is created fresh per `run`.
    - On `true`, the loop inserts `"hint": "This call already failed twice; try something different."` into the error object. The same value goes to the `tool_result` note and to the transcript.
  - **The trace.** `ToolTiming` gains `pub repeat_failure: bool`, `#[serde(default, skip_serializing_if = "std::ops::Not::not")]`.

- [ ] **Step 1: Write the failing tests:**
  - In tool_loop.rs:
    - `scope_reaches_the_tool_context`: a test tool echoes `ctx.scope.workspace`.
    - `notes_block_is_sent_on_every_step_and_never_committed`: the fake handle gains a note between steps 1 and 2. Step 2's request shows it, and the committed transcript has no `[Your notes]`.
    - `second_identical_failure_carries_hint`: in a third case, a different call in between resets the guard.
    - `third_identical_failure_carries_hint_again`
    - `success_between_resets_guard`
    - `repeat_failure_is_traced`: `ToolTiming.repeat_failure` is true only on the hinted call, and serialized JSON has no key when it is false.
  - In compaction.rs:
    - `notes_block_follows_summary_note`
    - `notes_block_is_measured_under_trim`: the fake measure sees the block's text.
    - `notes_block_without_system_prompt_keeps_summary_reuse` (Review Focus 3)
    - `off_strategy_still_carries_notes_block`
- [ ] **Step 2: Watch them fail.** Run `scripts/hive-test.sh -- runtime::tool_loop runtime::compaction`. Expected: FAIL.
- [ ] **Step 3: Implement** the items above. Existing callers pass `ToolScope::default()` and `None` for notes. Update the module docs of `tool_loop.rs` and `compaction.rs`.
- [ ] **Step 4: Watch them pass.** Run `scripts/hive-test.sh -- --tests`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(runtime): notes block in every prompt, tool scope per call, retry hint`.

### Task 5: Server wiring, the session folder lifecycle, and the seeds

**Files:**
- Modify: `backend/src/runtime/server.rs`
- Modify: `backend/src/main.rs`
- Modify: `backend/src/agents/template_store.rs` (seeds)

**Interfaces:**
- Consumes: Tasks 1 to 4.
- Produces:
  - **`RuntimeDeps.sandbox`.** `RuntimeDeps` gains `pub sandbox: Option<Sandbox>`. `main.rs` builds the sandbox once and passes it to both the registry and the deps.
  - **The session handle.** `struct SessionNotes { sessions: Arc<SessionStore>, hub: Arc<Hub>, session: String }` implements `SessionHandle`.
    - `set_notes` calls `SessionStore::set_notes`.
    - On success it sends `notes_changed {id, notes}` through `hub.to_session`.
    - It never holds either lock across the other.
  - **`turn_config`.** It sets `scope: ToolScope { workspace: Some(rec.snapshot.workspace.folder(&rec.id)), session: Some(Arc::new(SessionNotes { .. })) }`.
  - **`get_notes`.** A new method, `M_GET_NOTES = "get_notes"`, taking `{id}` and returning `{notes: [string]}`. It needs no attachment, and an unknown id gives the existing unknown-session error.
  - **`reset`.** It sends `notes_changed {id, notes: []}` after a successful reset.
  - **`delete_session`.**
    - When the deleted record's `snapshot.workspace.mode == WorkspaceMode::Session` and the sandbox is `Some`, it calls `Workspace::remove_folder(sandbox, &folder)`.
    - On failure it logs `ERR_WORKSPACE_DELETE = "workspace folder not deleted for"` with the id and the error, and the delete still succeeds.
    - Shared and named folders are never touched.
  - **The server docs.** The module docs gain a paragraph on workspaces, notes and `notes_changed`.
  - **Seeds** (template_store.rs):
    - The Default seed gets `workspace: WorkspaceConfig::default()`, written explicitly, and `tools` = `rules.tools` minus `rules.writing_tools`.
    - A new `Examples/Worker` seed, with id `worker` and name `Worker`, is written only on a fresh root together with Default.
      - `tools` is all of `rules.tools`.
      - `step_budget` is 16, `context_strategy` is `Trim`, and the workspace is the session default.
      - The model, runtime and `n_ctx` come from the same rules as Default.
      - Its `system_prompt` is exactly:

        ```
        You work in a private workspace folder. Save your results there as files with write_file and append_file. Keep a running plan with note_add, and check your notes before each step. Paths are relative to your workspace; prefix /sandbox/ to read the shared sandbox.
        ```

- [ ] **Step 1: Write the failing tests:**
  - In server.rs:
    - `note_add_in_a_turn_persists_and_notifies`: the attached client gets `notes_changed`, and `get_notes` returns the note.
    - `get_notes_needs_no_attachment`
    - `reset_clears_notes_and_notifies_empty`
    - `delete_session_removes_session_folder`
    - `delete_session_keeps_shared_and_named_folders`
    - `delete_session_does_not_follow_junctions` (Review Focus 2)
    - `turn_scope_uses_snapshot_workspace`: a template edit after create does not move the folder.
  - In template_store.rs:
    - `seeds_default_without_writing_tools_and_worker_with_all`
    - `worker_seed_has_budget_16_and_trim`
- [ ] **Step 2: Watch them fail.** Run `scripts/hive-test.sh -- runtime::server agents::template_store`. Expected: FAIL.
- [ ] **Step 3: Implement** the items above.
- [ ] **Step 4: Watch them pass.** Run `scripts/hive-test.sh -- --tests`, then `scripts/hive-test.sh --features llamacpp`, then `scripts/hive-test.sh --restore`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(runtime): session workspaces, notes over the wire, worker seed`.

### Task 6: App: the workspace row, the writes tag, the Notes panel and the repeated-failure tag

**Files:**
- Modify: `app/src/renderer/src/agents.ts`
  - `WorkspaceMode`, and `WorkspaceConfig { mode, name? }`
  - `AgentTemplate.workspace?`
  - `ToolSpec.side_effects`
  - `ToolTiming.repeat_failure?`
- Modify: `app/src/renderer/src/RuntimeClient.ts` (`GetNotes(id)`, and the `notes_changed` notification signal)
- Modify: `TemplateEditorViewModel.ts`, `ToolChoice.ts` and `views/TemplateEditor.mu`, with their tests
- Modify: `SessionViewModel.ts` and `views/SessionView.mu`, with their tests
- Modify: `TraceViewModel.ts` and `views/TracePanel.mu`, with their tests

**Interfaces:**
- Produces:
  - **`TemplateEditorViewModel`.**
    - `WorkspaceChoices`: Session, Shared and Named, mapping to `session`, `shared` and `named`.
    - `Workspace`, plus `WorkspaceName` and `IsWorkspaceNameEnabled`, which is true only for Named.
    - Both fields take part in `IsDirty` and `Revert`.
    - Save sends `workspace: {mode}`, or `{mode: 'named', name}`.
    - A template without the field loads as Session with an empty name.
  - **`ToolChoice.Writes`.** True when the spec has `side_effects`. The view shows a "writes" tag.
  - **`SessionViewModel`.**
    - `Notes: ObservableCollection<string>`, displayed as `{n}. {text}`.
    - `HasNotes`, and `IsNotesExpanded`, which defaults to true.
    - It loads through `GetNotes` on open, and replaces the list on a `notes_changed` whose `id` matches.
    - The panel is collapsible and hidden when `HasNotes` is false.
  - **`TraceToolViewModel.RepeatText`.** `'repeated failure'` when `repeat_failure` is set, otherwise `''`. It comes with `HasRepeat`.

- [ ] **Step 1: Write the failing tests:**
  - `editor_loads_and_saves_workspace`
  - `named_name_enabled_only_for_named`
  - `old_template_without_workspace_shows_session`
  - `switching_from_named_drops_name_on_save`
  - `tool_choice_marks_writing_tools`
  - `session_loads_notes_on_open`
  - `notes_changed_for_other_session_is_ignored`
  - `notes_changed_replaces_list_and_empty_hides_panel`
  - `repeat_failure_tool_line_shows_tag`
- [ ] **Step 2: Watch them fail.** Run `cd app && npx vitest run <touched test files>`. Expected: FAIL.
- [ ] **Step 3: Implement** the items above.
- [ ] **Step 4: Watch them pass.** Run `cd app && npm test && npm run typecheck:web && npm run typecheck:node`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(app): workspace in the template editor, notes panel, repeated-failure tag`.

### Task 7: Deploy and live proof (owner go required)

- [ ] **Step 1: Stop for the go.** Run `scripts/hive-test.sh --restore`, then stop and ask the owner for the go to merge and push.
- [ ] **Step 2: After the go:**
  1. Merge to main and push.
  2. On the hive, run `git pull --ff-only`, then `stop-sup.ps1`, then `build-both.cmd`. Expect `CANDLE_EXIT=0 LLAMA_EXIT=0 SUP_EXIT=0`.
  3. Run `schtasks /Run /TN harness-supervisor` and check the runtime line in `supervisor.out`.
  4. Create a Worker template from the editor. It gets all tools, budget 16, trim, and the Session workspace, with the seed prompt above. The existing install gets no seed.
- [ ] **Step 3: Protocol proof**, with a scratch script in D:\tmp. Run the spec's live-proof checks:
  1. `write_file` and then `append_file` produce `report.md` in `C:\llm-harness\sandbox\sessions\{id}\`, and the trace shows both as ok.
  2. Three notes, then a forced trim (`compact_at_pct` 20): the plan is recalled from the notes, and `get_notes` returns the three.
  3. Reading a missing file twice: the second result carries the hint, and the trace has `repeat_failure: true`.
  4. Writes to `/sandbox/x.txt` and to `../x.txt` are both refused.
  5. After `delete_session`, the folder is gone.
  6. A Default-style session reads `/sandbox/notes.txt`, and its `write_file` is refused as not enabled.
- [ ] **Step 4: Owner UI check:**
  - the Workspace row, with the name field enabled only for Named;
  - the "writes" tags;
  - the Notes panel filling live;
  - the "repeated failure" tag.
- [ ] **Step 5: Record the result** in the spec status, the roadmap (step 4 shipped), and the memory note.
