# Task 2 report: workspace paths, write tools, enabled-tools policy

Status: DONE. Commit `b5dafebf` on `feat/longer-tasks` (not pushed).

## What was built

- **`runtime/workspace.rs` (new).** `Workspace { sandbox, folder }`. Its public surface:
  - `open(sandbox, Option<&Path>)`: `None` means the sandbox root. Before `create_dir_all` it walks the existing components and requires the nearest existing ancestor to canonicalise inside the sandbox, so a junction at the root (for example `sessions` pointing outside) can never make it build folders outside. After creating the folder it canonicalises it and checks it again.
  - `resolve_read`, `resolve_write` and `prepare_write`. `prepare_write` creates missing parents, then calls `resolve_write` again.
  - `display`, `is_root`, `confined()` (an extra method: true when the folder is not the sandbox root, used for the list note), `remove_folder` and `SANDBOX_PREFIX`. There is also `WRITES_CONFINED`.
- **Write confinement (`Workspace::confine`).**
  - Normalise lexically, using the same routine `Sandbox::resolve` uses.
  - Walk the components with `symlink_metadata` to find the deepest one that exists.
  - Canonicalise it and require it to be inside the canonical write folder.
  - Append the missing components, which must be `Normal`. A dangling link fails to canonicalise, so it is refused and never written through.
- **The `/sandbox` prefix.** It must match exactly `/sandbox` or `/sandbox/...`, so `/sandboxes` falls through to "path must be relative to the sandbox". On Windows, backslashes count as slashes.
- **`Sandbox` refactor (builtin.rs).**
  - It now has `root()`, `resolve_in(base, rel, shown)`, `normalise(&Path)`, `no_such_path`, `slashed` and the consts `NOT_RELATIVE` and `ESCAPES`.
  - `resolve(rel)` is now `resolve_in(root, rel, rel)`, with the messages unchanged.
- **Tools (builtin.rs).**
  - `list_dir` and `read_file` go through `Workspace` and `resolve_read`. Both descriptions end with the spec sentence. `list_dir` adds the `note` key when the confined write folder itself lists empty.
  - New tools, all with `side_effects: true`:
    - `write_file`: a temp file `.{name}.{pid}-{nanos}.tmp` is written with `create_new`, synced, then renamed over the target. The temp file is removed on failure.
    - `append_file`: the existing size plus the content length is checked before the file is opened, so a refused append leaves the file unchanged.
    - `make_dir`: resolves the path again after `create_dir_all`.
    - `delete_path`: refuses the root, a missing path, and a non-empty folder.
  - Shared helpers live in `FileWork`: argument reading, `spawn_blocking`, and the session-deleted gate in `run_write`. Limits and refusals live in `WriteLimits`. All strings are named consts.
  - `BuiltinSource` registers all four write tools whenever there is a sandbox.
- **tool.rs.** `SessionHandle`, and `ToolScope` with a hand-written `Debug` and a `PartialEq` that compares sessions with `Arc::ptr_eq`. Also `ToolScope::session_deleted()`, and `ToolContext.scope`.
- **executor.rs.** `execute(call, enabled, scope)` puts a clone of the scope into the context. The order of checks is unchanged.
- **policy.rs.** `ReadOnlyPolicy` is deleted. `EnabledPolicy` always returns Allow. It replaces the old policy in main.rs (both sites), test_support.rs and the tool_loop tests. tool_loop passes `&ToolScope::default()`.
- **test_tools.rs.** A `ScopeTool` (`scope_boxed`) proves the executor passes the scope through.

## Tests

Every name in the brief is implemented:
- **workspace:** `reads_resolve_against_the_write_folder`, `sandbox_prefix_reads_the_root`, `write_to_sandbox_prefix_is_refused`, `dotdot_out_of_folder_is_refused`, `write_through_inner_junction_is_refused`, `display_is_relative_or_sandbox_prefixed`, `no_scope_folder_is_the_sandbox_root`
- **builtin:** the 13 write tests and `empty_workspace_list_carries_note`
- **executor:** `executor_runs_side_effect_tool_when_enabled` and `executor_refuses_side_effect_tool_when_not_enabled`. `executor_denies_side_effects_under_read_only` is removed.

What `write_through_inner_junction_is_refused` checks:
- `resolve_write` refuses a junction to outside the sandbox, for both new and deep targets and for the link itself.
- It also refuses a junction to elsewhere in the sandbox.
- `write_file`, `append_file`, `make_dir` and `delete_path` through the junction all return `path escapes the sandbox`.
- The outside folder and the shared folder stay empty.
- A read through the in-sandbox junction is still allowed.

Extra tests:
- `open_refuses_folder_outside_the_sandbox`
- `open_through_root_junction_is_refused`
- `remove_folder_removes_and_tolerates_missing`
- `remove_folder_does_not_follow_junctions`
- `read_file_reads_workspace_and_sandbox_prefix`
- `executor_passes_scope_to_tool`
- `scope_compares_sessions_by_identity`
- `scope_reports_deleted_session`
- `enabled_policy_allows_side_effects`

`source_without_sandbox_has_only_get_time` was extended to cover the new tool list, side effects and descriptions.

The hive is Windows, so the `#[cfg(windows)]` junction tests ran.

### RED

`scripts/hive-test.sh -- runtime::` with the tests written before any implementation:

```
error[E0433]: cannot find type `Workspace` in this scope   (x8)
error[E0432]: unresolved import `crate::runtime::policy::EnabledPolicy`
error[E0432]: unresolved imports `crate::runtime::tool::SessionHandle`, `crate::runtime::tool::ToolScope`
error: could not compile `backend` (lib test) due to 22 previous errors
TEST_EXIT=101
```

### GREEN

`scripts/hive-test.sh -- runtime::`:

```
test result: ok. 258 passed; 0 failed; ... 259 filtered out
TEST_EXIT=0
```

`scripts/hive-test.sh -- --tests` is the full suite, including the bin's test target, so main.rs compiles too:

```
test result: ok. 518 passed; 0 failed   (lib)
test result: ok. 7 / 10 / 6 / 2 / 5 passed; 0 failed   (other targets)
TEST_EXIT=0
```

There were no compiler warnings. I ran `rustfmt --edition 2021` on the hive over the runtime files I touched and copied the results back; line endings are still LF. I finished with `scripts/hive-test.sh --restore`, which printed RESTORED.

## Files changed

All under `backend/src/`:
- main.rs
- runtime/builtin.rs
- runtime/executor.rs
- runtime/mod.rs
- runtime/policy.rs
- runtime/test_support.rs
- runtime/test_tools.rs
- runtime/tool.rs
- runtime/tool_loop.rs
- runtime/workspace.rs (new)

## Self-review

- **Error strings.** Every verbatim string matches constraints.md: content/file caps, `path is a directory`, `path is a file`, `directory is not empty`, `cannot delete the workspace root`, `session deleted`, `writes are confined to your workspace`, `no such path: {path}`, and the escape and relative-path messages.
- **Unchanged read tests.** Existing read tests pass unchanged when the scope is `None`, because `display` relative to the sandbox root is today's form.
- **Nothing created after a deleted session.** The session-deleted gate runs before `Workspace::open`, so a deleted session creates no folder (the test asserts that `sessions/` does not appear).
- **Locks.** Tools take no locks. `SessionHandle::exists()` is called once, before the blocking work.

## Concerns (none blocking)

1. **`delete_path` on a link.** Called on a junction or symlink inside the folder that points elsewhere inside the folder, it acts on the canonical target, not on the link. A link that leads out of the folder is refused. Writes are never outside the folder in either case. A model cannot create links, so this only matters for links that someone else planted.
2. **Hard links and races.**
   - A hard link that already exists inside the folder and points to an outside file cannot be detected through canonicalisation. `append_file` would write through it; `write_file` would only replace the link, because of the rename. Hard links can only be planted with host access, so this is out of scope for the lab.
   - The gap between resolving a path and writing it is closed only by resolving again after parents are created, as the brief accepted.
3. **Pre-existing rustfmt drift.** `cargo fmt --check` on the hive shows drift in files I did not touch: `agents/template.rs` and `agents/template_store.rs` from Task 1, and `engine/factory.rs`. I left them alone.
4. **The write folder is created on every call.** `list_dir` and `read_file` now create the write folder when a scope folder is set, as the spec requires. Until Task 4 threads the real scope through, the loop passes `ToolScope::default()`, so behaviour in production is unchanged.
