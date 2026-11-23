# Task 1 report: agent templates and the template store

Commit 6164ad53 `feat(agents): agent templates and the template store` on feat/agent-templates.

## Built
- `backend/src/agents/mod.rs` (declares `template`, `template_store`), `template.rs`, `template_store.rs`; `pub mod agents;` added first in `backend/src/lib.rs`.
- `AgentTemplate` (TOML serde, defaults per brief; `sampling`/`host` skipped when None), `TemplateRules::validate` (order: step_budget, tool, runtime, model, runtime-can-load), `#[cfg(test)] TemplateRules::for_tests()` (runtimes llamacpp/candle; models `gemma-4-e4b`->[llamacpp], `tiny`->[candle,llamacpp]; tools read_file, web_search).
- `TemplateStore` with open/tree/get/save/delete/create_group/rename_group/delete_group/move_template, `TemplateNode`, `TemplateError(pub String)` (Display, Error, From<io::Error>, PartialEq).
- `SamplingConfig` already derived Serialize/Deserialize with `#[serde(default)]`; no change needed.

## Behaviour notes
- Ids are the file stem; `locate` only accepts ids that are already slugs, so ids cannot traverse paths. A missing id gives `template not found: {id}`.
- `save(Some(id), path, t)` keeps the id and moves the file to `path` if it differs; `save(None, ..)` generates a unique slug (`agent` if the name slugs to empty).
- Atomic write: `.{id}.toml.tmp` then rename, under a `Mutex`. Tree ignores non-`.toml` files.
- Path validation: each `/` segment must be non-empty, not `.`/`..`, no `\`, `:`, trailing `.`. So `/x`, `../x`, `C:/x` fail with `invalid path`. Root group (`""`) cannot be renamed or deleted.
- Broken TOML shows in `tree()` as a Template with `error: Some(..)`, name = stem, empty runtime/model. Only parse errors are flagged there, not rule violations.
- Extra error strings I chose: `template not found: {id}`, `group not found`, `group already exists`.

## Tests and TDD evidence
10 tests in `template_store::tests` (the 9 from the brief plus `rename_group_and_resave_in_place`). A temp-dir helper is `TempDir` (struct with `new`/`store`/`sample`, removed on drop); no free functions.
- RED: not run separately. I wrote tests and implementation together. The first hive run failed to compile (a test type mismatch on `bad` reused across `Result` types, fixed), which is the only failing run.
- GREEN: `scripts/hive-test.sh -- agents::` -> `10 passed; 0 failed`. Whole lib: `233 passed; 0 failed`. `scripts/hive-test.sh --restore` run afterwards.

## Reuse by later tasks
- `crate::agents::template::{AgentTemplate, TemplateRules}`, `TemplateRules::for_tests()` (cfg(test), `pub`).
- `crate::agents::template_store::{TemplateStore, TemplateNode, TemplateError}`; `TemplateStore::open(&Path, TemplateRules)`.
- In-module test helper `TempDir` is private to `template_store::tests`; other tasks need their own.

## Concerns
- rustfmt was run on the three new files only. LF endings verified. Cargo.lock untouched.
- RED step skipped as noted above.

## Fix report: review follow-ups (commit "fix(agents): task 1 review follow-ups")

All in `backend/src/agents/template_store.rs`.
1. `locate` accepts any id that is a valid segment with no `.`; hand-named `My Agent.toml` works for get/save(Some)/move/delete (test `hand_named_file_is_fully_usable`). New ids remain slugs. File-name match is case-insensitive.
2. Duplicates: chose the simpler option, no extra `path` param. `tree()` flags every colliding entry with `duplicate template id: {id}` (case-insensitive); `locate` returns that error for every operation, delete and move included; the user renames/deletes one file by hand and refreshes. Documented on `locate`. Test `duplicate_ids_are_flagged_and_refused`.
3. `rename_group`: same name is a no-op; case-only rename goes through a temp name. Test `group_rename_same_name_and_case_only`.
4. Reserved Windows names (and trailing space/dot) rejected in every path segment; a slug that is reserved gets `-agent` (`con-agent`). `unique_id` collision check is case-insensitive. Test `reserved_names_are_rejected_or_suffixed`.
5. `write_atomic` removes the tmp file on write/rename failure (`failed_write_removes_tmp_file`).
6. Crash-window comment added in `save`; relies on the duplicate rule.
7. Added tests: save_unknown_id_is_not_found, id_is_stable_across_name_change, rename_and_move_reject_bad_paths_and_names, concurrent_saves_get_unique_ids, root_with_only_empty_groups_is_not_seeded, seed_falls_back_to_first_llamacpp_model.
8. Sort key now (kind, lowercase name, path, id); test sort_ties_break_on_path.
9. Trailing-slash paths still rejected; documented on `dir_for` (test includes `Examples/`).

Evidence: `scripts/hive-test.sh` whole lib: 245 passed; 0 failed. rustfmt run, LF verified (0 CR), `--restore` run.

## Fix report 2: case-safe save and rollbacks (commit "fix(agents): case-safe save and rollbacks")

All in `backend/src/agents/template_store.rs`.
1. `save(Some(id))` now uses the on-disk stem found by `locate` as id and file name (returned id is that stem); old/new compared with `same_file` (ASCII case-insensitive) before removal, so no more self-deletion. `move_template` likewise keeps the on-disk stem. Test `save_with_differently_cased_id_keeps_file` (X.toml, save Some("x") same group: file remains with new content; cross-group too).
2. If removing the old file fails in a cross-group save, the new copy is removed and the error returned. Windows-only test `failed_old_removal_rolls_back_new_copy` holds the old file open without FILE_SHARE_DELETE (read-only attribute did not work: std removes read-only files on current Windows).
3. Test helpers `invalid()` and `hand_file` are now associated fns on `TempDir`.
4. One folding rule everywhere: ASCII-insensitive (`find`, `count_ids`, `flag_duplicates`, `collect_ids`, sort key, case-only group rename). Test `non_ascii_ids_tree_and_locate_agree`: `\u{c9}x` and `\u{e9}x` are distinct ids in both tree and locate (no false duplicate flag).
5. `flag_duplicates` joins messages: `"{parse}; duplicate template id: {id}"` (test `duplicate_flag_keeps_parse_error`).
6. `rename_group` case-only two-step rolls back (renames the temp name back) if the second rename fails.

Evidence: hive whole lib 249 passed; 0 failed. rustfmt, LF (0 CR), `--restore` done.

Fix 3 (same-file by identity): `same_file` now compares `fs::canonicalize` of old and new (ASCII-insensitive name fallback if canonicalize fails); Windows test `save_into_non_ascii_case_variant_group_keeps_file`. Hive `agents::` 27 passed; rustfmt, LF, --restore done.
