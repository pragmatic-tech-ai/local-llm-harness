# Task 5 report: server wiring, session folder lifecycle, seeds

Status: DONE. Commit `dc8b98c3` on `feat/longer-tasks` (not pushed).

## Implemented

- `RuntimeDeps.sandbox: Option<Sandbox>`. main.rs builds the sandbox once (`Config::sandbox`; the
  "tools sandbox unavailable:" message is now the const `Config::ERR_SANDBOX`) and passes a clone
  to `Config::tool_registry(sandbox)` and to `AgentsStack::into_deps(.., sandbox)`.
- `SessionNotes { sessions, hub, session }` implements `SessionHandle`. `notes()` reads through
  `SessionStore::get`, `exists()` through `contains`, and `set_notes` calls
  `SessionStore::set_notes`. Only once that returns, with the store lock released, does it send
  `notes_changed {id, notes}` through `hub.to_session` (`SessionNotes::announce`). The store and
  hub locks are never held together.
- `SessionView::turn_config(deps, rec)` now sets
  `scope: ToolScope { workspace: Some(rec.snapshot.workspace.folder(&rec.id)), session: Some(Arc::new(SessionNotes{..})) }`.
  This replaces the `ToolScope::default()` Task 4 left in place.
- `RuntimeServer::NOTES_CHANGED = "notes_changed"`.
- `M_GET_NOTES = "get_notes"` runs in `SharedRequests` and needs no attachment. It takes `{id}`
  and returns `{notes}`. An unknown id gives `SessionError::unknown` through `SharedRequests::record`.
- `reset` sends `notes_changed {id, notes: []}` after a successful reset.
- `delete_session` reads the record before it deletes. After the store delete, the detach and
  the trace delete, `SharedRequests::remove_workspace` runs `Workspace::remove_folder` on the
  blocking pool. It does this only when the mode is `WorkspaceMode::Session` and the sandbox is
  `Some`. A failure is logged with `ERR_WORKSPACE_DELETE = "workspace folder not deleted for"`,
  the id and the error, and the delete still succeeds. Shared and named folders are never touched.
- The server module docs have a new paragraph on workspaces, notes, `get_notes`,
  `notes_changed`, the reset/delete folder rules, and the snapshot not moving the folder.
- Seeds (template_store.rs):
  - The fields both seeds share live in `seed_base`.
  - `seed_template` (Default) is `seed_base` with `rules.writing_tools` removed from the tools.
    Its workspace is explicit `WorkspaceConfig::default()`, written to the file as
    `[workspace] mode = "session"`.
  - `worker_template` is `Examples/worker.toml`, named `Worker`, with all `rules.tools`,
    step_budget 16, `ContextStrategy::Trim`, the default workspace, and the verbatim system
    prompt. Its runtime, model and n_ctx follow the same rules as Default.
  - Both are written only on a fresh root.
- TestRuntime:
  - `TestRuntime::sandboxed(tag, link)` adds a `sandbox` dir and `BuiltinSource` over it,
    alongside echo.
  - New items: `WORKER_ID` and `sandbox_root()`.
  - `reopen` keeps the sandboxed flag.
  - Plain `TestRuntime::new` has `sandbox: None`, so the existing tests are unaffected.
- Fixed the misindented `workspace:` line that Task 1 left in the template_store test sample. At
  HEAD this file was the only one that was not rustfmt-clean.

## Tests

New in server.rs (helper struct `Workspaces`, associated fns only):
- `note_add_in_a_turn_persists_and_notifies`: a Worker turn calls the real `note_add`. The
  attached client gets exactly one `notes_changed {id, notes:[NOTE]}`, `get_notes` returns it,
  and a reopened store has it on disk.
- `get_notes_needs_no_attachment`: an unattached client gets the empty list, then the note list.
  An unknown id gets `ERR_INTERNAL` with the `SessionError::unknown` text.
- `reset_clears_notes_and_notifies_empty`: the attached client gets `notes_changed {id, notes:[]}`,
  an unattached one gets nothing, and the notes are cleared.
- `delete_session_removes_session_folder`: the folder is gone and a sibling session's folder is
  kept. A session that never had a folder still deletes cleanly.
- `delete_session_keeps_shared_and_named_folders`: covers the shared and the named (`team`)
  templates. Their folders survive, and so does a stray `sessions/{id}` folder.
- `delete_session_does_not_follow_junctions` (Review Focus 2, `#[cfg(windows)]`): a junction
  inside `sessions/{id}` points to an outside dir. The folder is removed and the outside file
  survives.
- `turn_scope_uses_snapshot_workspace`: this is the server-level proof that tool calls see the
  scope. The Worker template is switched to `shared` after create, then a turn calls the real
  `write_file`. The file lands in `sessions/{id}/out.txt` and no `shared` folder exists.
  `turn_config(deps, rec)` carries the snapshot folder and a live session handle (`exists`, and
  `notes() == Some([])`).

New in template_store.rs:
- `seeds_default_without_writing_tools_and_worker_with_all`
- `worker_seed_has_budget_16_and_trim`: covers the verbatim prompt, the same runtime/model/n_ctx
  as Default, the path `Examples`, and no reseed on a non-fresh root.

Updated:
- `seeds_examples_default_when_empty`: the tree now holds Default and Worker.
- `delete_group_requires_empty`: Examples stays non-empty until Worker is deleted too.
- main.rs `startup_builds_stores_and_seeds`: 2 templates, Default then Worker; `into_deps(.., None)`.

## TDD evidence

RED: `scripts/hive-test.sh -- runtime::server` exited 1 with a compile failure (11 errors) on the
missing items:
```
error[E0599]: no associated function or constant named `NOTES_CHANGED` found for struct `runtime::server::RuntimeServer`
error[E0599]: no associated function or constant named `M_GET_NOTES` found for struct `runtime::server::RuntimeConnection`
error[E0061]: this function takes 1 argument but 2 arguments were supplied
error[E0609]: no field `sandbox` on type `RuntimeDeps`
error[E0560]: struct `RuntimeDeps` has no field named `sandbox`
error: could not compile `backend` (lib test) due to 11 previous errors
```
GREEN:
- `scripts/hive-test.sh -- runtime::server`: `test result: ok. 67 passed; 0 failed`. All 7 new
  tests are listed as ok.
- `scripts/hive-test.sh -- agents::template_store`: the first run gave 31 passed and 1 failed.
  The failure was `delete_group_requires_empty`, a stale assumption that Examples holds only
  Default. After updating that test it passed in the full run.
- `scripts/hive-test.sh -- --tests`: lib `549 passed`, bin (main.rs) `7 passed` (incl.
  `startup_builds_stores_and_seeds`), integration `10/6/2/5 passed`; 0 failed, no compiler warnings.
- `scripts/hive-test.sh --features llamacpp`: `557 passed; 0 failed`.
- `scripts/hive-test.sh --restore`: RESTORED.
- `rustfmt --edition 2021 --check` on the 4 touched files: clean. rustfmt only, no cargo locally.

## Files changed
- D:/local-llm-harness/backend/src/runtime/server.rs
- D:/local-llm-harness/backend/src/runtime/test_support.rs
- D:/local-llm-harness/backend/src/agents/template_store.rs
- D:/local-llm-harness/backend/src/main.rs

## Self-review
- Lock discipline:
  - `SessionNotes` takes the store lock inside `get`/`set_notes`/`contains`, and the hub lock
    only in `to_session` after the store call has returned.
  - `reset` announces after the store reset. It still holds the busy flag, which is a hub-side
    flag guard and not the hub mutex.
  - Folder removal runs on the blocking pool with no lock held.
- Delete ordering: the store delete comes before the folder removal. A running turn's write
  tools therefore see `session_deleted` and cannot recreate the folder afterwards.
- The record is read with `get`, not `record`. That keeps the store's own unknown-id error from
  `delete` as the reply.
- String literals are named consts: `NOTES_CHANGED`, `M_GET_NOTES`, `ERR_WORKSPACE_DELETE`,
  `ERR_SANDBOX`, and the worker seed consts. JSON keys stay inline, as elsewhere in server.rs.

## Concerns
- `SessionHandle::set_notes` returns a bool (the Task 3 contract). A store write failure that is
  not a deletion, such as a disk error, therefore reaches the model as `session deleted`. To fix
  this, the trait would need to return an error.
- `notes()` clones the whole record through `get`, transcript included, once per note-tool call
  and per step. This follows the resolution. It is cheap at today's sizes, but a
  `SessionStore::notes(id)` accessor would avoid it.
- A write that is already inside its file work when `delete_session` runs can race the removal
  and leave leftovers. The spec accepts this: writes check the handle first, and removal
  failures are only logged.
