# Final whole-branch review findings (192cd623..52b0c7de) — fix ALL listed below

## Important
F1. Compaction "≈" mark can never appear for OpenRouter. TraceViewModel.ts:201 CompactionLine marks only when record.estimated; StepMeter::apply_usage (tool_loop.rs:332) clears estimated whenever usage arrives, but compaction before/after counts come from measure_prompt (still an estimate). TS test TraceViewModel.test.ts:445-455 uses an impossible combination.
  RULING: no schema change — pass the session's capabilities.measure_prompt into TraceViewModel.ShowSession (or equivalent) and mark compaction counts when it is "estimated". Replace the test with a usage-cleared step (estimated false) carrying a compaction on an estimated-measure session.
F2. Template editor and Agents tree never follow repositories_changed. TemplateEditorViewModel.EnsureCatalog (TemplateEditorViewModel.ts:654) reloads only when catalog===null; AgentsService.OnTreeRefreshed only calls EnsureCatalog; only ModelsPanelViewModel subscribes to OnRepositoriesChanged. Also the server sends no templates_changed after a repository refresh (server.rs:1239), so a template whose model left the list is not re-marked in a shown tree.
  Fix: editor subscribes to client.OnRepositoriesChanged → LoadCatalog() (keeps dirty edits via its pristine check); server's repositories_changed path also broadcasts TEMPLATES_CHANGED (or AgentsService refreshes the panel on OnRepositoriesChanged — pick one, prefer server broadcast). One test per side.
F3. Spec not amended for the identity ruling: docs/specs/2026-11-24-model-providers-design.md lines ~124 and ~236 say identity is "per repository and model" — amend to "per repository; within one requirement it is stable".

## Minor (final review)
F4. link.rs:468 take_usage writes prompt/completion/reasoning tokens as 0 when absent → StepMeter trusts Some(0) and clears estimated (invented numbers). Emit null for absent counts; StepMeter clears `estimated` only when usage.prompt_tokens is present (T6 minor).
F5. registry.rs:621/636 (store) and 700/705 (delete): used_by (templates_using: dir walk + TOML parse) runs while the registry std Mutex is held. Compute used_by(target) before self.lock().
F6. registry.rs:86: unknown keys in repositories.toml silently dropped on next save (typo `apikey =` loses the key). Add #[serde(deny_unknown_fields)] to RepositoriesFile and OpenRouterConfig (surfaces as `does not parse at line N`); test.
F7. api.rs:387 inline `.expect("reqwest client construction")` → named const.
F8. link.rs:105+ inline body/wire keys ("model","stream","usage","temperature","top_p","top_k","repetition_penalty","role","content","tool_call_id","text","tokens","stop","needed","total","message","prompt_tokens","estimated") → one const home each / reuse existing consts.
F9. Add TemplateEditorViewModel.test.ts late_catalog_hides_runtime_for_openrouter_template: open an OpenRouter template before the catalog arrives; assert IsRuntimeVisible goes true→false and is raised after RuntimeChoice.

## Deferred minors from task reviews — all Fix
T1a backend/tests/https_smoke.rs:74,76 inline .expect messages → consts.
T1b https_smoke.rs:75 assert_eq!(status,200) → add a context message.
T1c scripts/hive-test.sh vendor diff vs possibly-stale local origin/main → `git fetch -q origin main` before the diff (tolerate offline failure).
T2a dispatcher.rs:151/170/182 DispatchError mapping of HostError duplicated 3× → impl From<HostError> for DispatchError.
T2b agents/host.rs five separate #[cfg(test)] use lines → group; move FakeHost to runtime/test_support (or providers test support).
T2c HostState/HostError/HarnessIdentity live in agents::host but are provider types → move to providers (agents::host may re-export ModelRequirement-related bits only; fix imports).
T3a session_store: add a test loading an old session snapshot whose template has `host = "hive"` → snapshot.repository == Some("hive") and it routes to hive (Review Focus 4).
T3b template.rs validate_model `runtime {} cannot load model {}` inline format → const.
T3c template_store.rs:179 seed_base: when the catalog's Pragmatic names lack `hive`, seed templates must set repository to the first Pragmatic name (else they fail validation). Test.
T3d catalog_source rules clone model list per host → share one list (Arc or build once); RepositoryKind: use #[serde(rename_all = "lowercase")] at template.rs:247 instead of per-variant renames.
T4a link.rs:298 parse-error arguments Value::String(raw) → Value::Null (worker parity; keep raw in `raw`).
T4b errors_never_echo_the_key: mid-stream error case must carry the key in the error object's message and code, asserting it is scrubbed.
T4c provider.rs:52 OpenRouterProvider::new has_key param duplicates api.has_key() → derive it, drop the param.
T4d link.rs ~1040 lines → move Turn/CallDraft/Phase to providers/openrouter/turn.rs.
T5a template_store collect_users skips unparsable files → parse leniently as toml::Table and read `repository`/`host` so a broken template still counts as a user (default hive when absent only if it parses as a table).
T6a compaction.rs:906 summary max_tokens guard counts chunk tokens (empty on OpenRouter) → use usage.completion_tokens when present, else chunk count as today.
T7a TemplateEditor/RepositoryEditor test texts_change_only_while_their_part_is_shown: vacuous indexOf ordering asserts → assert each index >= 0 first.
T8a TemplateEditorViewModel.ts ~1060 FitToRepository: OpenRouter→Pragmatic must restore the last Pragmatic runtime and n_ctx (remember them before switching to OpenRouter; fall back to the loaded template's values) instead of first runtime + OpenRouter n_ctx. Test.
T8b TracePanel.mu / TraceViewModel.ts:1709: cost suffix squeezes the step fill bar to a dot at 360px → give the bar its own fixed column (e.g. bar before text, or cost/metrics wrap to the second line). Verify with a Playwright screenshot.
T8c InstrumentPanelViewModel.ts:185: `≈0 / n` before any turn → mark only when context used > 0 (or after a turn_started).
T8d SessionViewModel.ts:382 Reset → also Panel.ApplyCapabilities(null).

## Dropped (no action)
T2 removed policy test (covered elsewhere); T3 wrong-runtime text on OpenRouter (matches Pragmatic verbatim); T4 template_store unused imports (moot); T5 no RED run (process); T5 keep_fetched cache write under lock (needed for overtaken-fetch correctness).

## Docs
Document the benign save_template/delete_repository race in server module docs (template may name a deleted repo → tree marks `unknown repository`).
