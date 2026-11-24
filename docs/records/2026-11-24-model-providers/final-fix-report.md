# Final fix report: feat/model-providers (final whole-branch review)

Commits (on top of 52b0c7de, not pushed):
- `da0e5870` fix(runtime): final review fixes for model providers (backend + scripts)
- `8ca2736b` fix(app): final review fixes for models UI
- `8f3e73ed` docs(spec): provider identity is per repository

## Per finding

### Important
- **F1** `app/.../TraceViewModel.ts`: `ShowSession(client, id, busy, capabilities)` takes the session's capabilities; turns and steps get `estimatedMeasure` (`measure_prompt === 'estimated'`), and compaction before/after counts are marked `≈` when it is set (or the step itself is estimated). `SessionViewModel.ts` passes `this.capabilities`. Test: the impossible-combination case in `TraceViewModel.test.ts` became an exact-measure check; new `compaction_counts_marked_on_an_estimated_measure_session` (a usage-cleared step, `estimated: false`, with a compaction, paged and live, then an exact session unmarked).
- **F2** Server (`runtime/server.rs`): `repositories_changed()` now broadcasts `repositories_changed` then `templates_changed` (preferred server broadcast). Test `repository_changes_also_announce_templates_changed`. App (`TemplateEditorViewModel.ts`): subscribes to `client.OnRepositoriesChanged` → `LoadCatalog()` (pristine check keeps edits); `dispose()` added, called from `AgentsService.dispose`. Test `repositories_changed_reloads_the_catalog_and_keeps_edits` (shrunk list → model stays as marked choice, edit and dirty kept, no reload after dispose).
- **F3** `docs/specs/2026-11-24-model-providers-design.md` (both places): "per repository; within one requirement it is stable".

### Minor
- **F4** `providers/openrouter/turn.rs` `take_usage`: absent counts are `null`; calibration only observes a present prompt count. `runtime/tool_loop.rs` `StepMeter::apply_usage` clears `estimated` only when `prompt_tokens` is present. Tests `absent_usage_counts_are_null_and_do_not_calibrate` (link), `usage_without_prompt_count_keeps_the_estimate` (tool loop).
- **F5** `providers/registry.rs`: `store` computes `used_by(target)` (only for a rename) and `delete` computes `used_by(name)` before `self.lock()`; `check_unused` takes the list. Test `used_by_runs_outside_the_registry_lock` (the closure `try_lock`s the registry mutex during rename and delete).
- **F6** `#[serde(deny_unknown_fields)]` on `RepositoriesFile` and `OpenRouterConfig`. Test `unknown_keys_are_refused_by_line` (`apikey =` → `does not parse at line 3`; top-level typo → line 2; the key never quoted since the message is by line only).
- **F7** `api.rs`: `OpenRouterApi::CLIENT_BUILD` const for the `.expect`.
- **F8** `link.rs`: `Wire` struct holds every shared body/note key (model, stream, usage, include, temperature, top_p, top_k, repetition_penalty, seed, max_tokens, reasoning, enabled, role, content, tool_calls, tool_call_id, id, type, function, name, arguments, n_ctx, model_name, supports_tools, prompt_tokens, completion_tokens, reasoning_tokens, cost, estimated); note-only keys (prompt, text, tokens, stop, needed, total, message) are `Turn` consts. All `json!` keys use `(Const)` keys.
- **F9** Test `late_catalog_hides_runtime_for_openrouter_template` (open before catalog: visible true → false, raised after `RuntimeChoice`, both indices asserted ≥ 0). Passed without code change.

### Deferred minors
- **T1a/T1b** `backend/tests/https_smoke.rs`: `.expect` messages and the status context are consts; `assert_eq!` has a message.
- **T1c** `scripts/hive-test.sh`: `git fetch -q origin main` before the vendor diff; offline prints a note and continues.
- **T2a** `agents/dispatcher.rs`: `impl From<HostError> for DispatchError`; the three call sites use `DispatchError::from`.
- **T2b/T2c** `HostState`, `HarnessIdentity`, `HostError` moved to `providers/mod.rs`; `agents/host.rs` keeps only `ModelRequirement` (no re-exports). `FakeHost` (+ its tests) moved to new `providers/test_support.rs` (`#[cfg(test)]`), its imports grouped. Imports fixed in dispatcher, server, pragmatic, provider, registry, runtime/test_support.
- **T3a** Test `old_snapshot_with_host_routes_to_that_repository` (`session_store.rs`): snapshot with `host = "hive"` (and `"lab"`) loads with `repository == Some(host)` and `list_with_requirements` routes there.
- **T3b** `template.rs`: `ERR_RUNTIME_PREFIX` / `ERR_CANNOT_LOAD_MODEL` consts.
- **T3c** `TemplateRules::seed_repository()` (first Pragmatic, preferring hive); `seed_base` sets `repository` to it when not `hive`. Test `seeds_name_the_first_pragmatic_repository_when_there_is_no_hive` (seeds validate; with hive, none set).
- **T3d** `RepositoryRules.models: Option<Arc<[RuleModel]>>`; `DiscoveredCatalog::rules` builds the list once and shares it per host. `RepositoryKind` uses `#[serde(rename_all = "lowercase")]`.
- **T4a** `turn.rs` `CallDraft::into_call`: unparsable arguments are `Value::Null` (raw kept). `invalid_tool_arguments_set_parse_error` asserts `Null`.
- **T4b** `errors_never_echo_the_key`: the mid-stream error object carries the key in `code` and `message`; asserts `openrouter: bad_key_*** upstream died for Bearer ***`.
- **T4c** `OpenRouterProvider::new` drops `has_key`; derives `api.has_key()`.
- **T4d** `Phase`, `CallDraft`, `Turn` moved to `providers/openrouter/turn.rs` (link.rs 1037 → ~800 lines incl. tests).
- **T5a** `template_store.rs` `collect_users`: parses as `toml::Table`; reads `repository` then `host` (default hive); name from `name` else file stem; non-TOML skipped. Test `broken_templates_still_count_as_users`.
- **T6a** `compaction.rs`: the summary max_tokens guard uses `usage.completion_tokens` when present, else the chunk count. New row "max_tokens hit by the usage count" in `summary_failure_falls_back_to_trim_with_reason`.
- **T7a** `RepositoryEditorViewModel.test.ts`: the `at()` helper asserts each entry index ≥ 0; `TemplateEditorViewModel.test.ts` ordering asserts guarded likewise.
- **T8a** `TemplateEditorViewModel.ts`: Repository setter remembers runtime + context size while on a Pragmatic repository; `FitToRepository(fromOpenRouter)` → `RestorePragmatic()` (remembered, else loaded template's when not OpenRouter, else default 8192 and the catalog's first runtime). Test `back_to_pragmatic_restores_its_runtime_and_ctx`.
- **T8b** `views/TracePanel.mu`: the step fill bar now sits on its own row under the fill text (fixed 80 px), so the cost no longer squeezes it. Bar-before-text was tried first: in the real window the text then overflowed and the `· $cost` was clipped, so the own-row layout was chosen (the cost stays on the trace line, per spec).
- **T8c** `InstrumentPanelViewModel.IsContextEstimated`: only when context used > 0. Test `estimating_provider_marks_nothing_before_the_first_turn`.
- **T8d** `SessionViewModel.Reset` → `Panel.ApplyCapabilities(null)`. Test `clear_resets_the_panel_capabilities`.

### Docs
- Server module doc (`runtime/server.rs`) describes the `templates_changed` follow-up and the benign `save_template`/`delete_repository` race (template may name a deleted repository; tree marks `unknown repository: {name}`).

## Test evidence
- `scripts/hive-test.sh -- --tests` (after one compile fix for the `Arc<[RuleModel]>` change in a template_store test): lib `624 passed; 0 failed`; integration targets 8, 10, 6, 2, 5 passed, https smoke 1 ignored; `TEST_EXIT=0`; no warnings.
- `scripts/hive-test.sh --features llamacpp`: lib `632 passed; 0 failed`, `TEST_EXIT=0`, no warnings.
- `scripts/hive-test.sh --restore`: `RESTORED` (hive clone cleaned).
- `rustfmt --edition 2021 --check` on all changed .rs files: clean.
- App: `npm test` → 22 files, 266 tests passed; `npm run typecheck:web` and `typecheck:node` clean; `npm run build` ✓.

## Real window (T8b)
Script `D:/tmp/models-fix/probe.cjs` (the Task 8 probe, session part only; OpenRouter session "Default #1" faked over the WebSocket route, second step usage-cleared `estimated: false` with a compaction). Screenshots in `D:/tmp/models-fix/`:
- `t8b-session-openrouter.png`: step line `1  ≈ctx 1 234/32 768 (4%) · $0.0012` fully visible, the 80 px bar on the row below with its 4% fill, compaction `⟲ trim: ≈7 420 → ≈5 980 …`; Context reads `0 / 32768 (0%)` with no `≈` before a turn (T8c).
- `t8b-after-turn-crop.png`: new step `1  ctx 2 301/32 768 (7%) · $0.0004` (no ≈ on the usage-cleared step) with its bar, compaction still `≈7 420 → ≈5 980` (F1); Context `≈2345 / 32768 (7%)`.
- `t8b-session-hive.png`: Pragmatic session unchanged apart from the bar row: no `≈`, no cost, Load/Unload shown.
The app was closed by the script; no electron process remained.

## Not fixed
None.
