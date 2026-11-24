# Task 2 report: Provider seam, capabilities, Pragmatic provider

Status: DONE. Commit 1b074256 on feat/model-providers (not pushed).

## Implemented
- `backend/src/providers/mod.rs`: `Provider` trait (ModelHost methods + `capabilities()`), `Measure` (wire `exact`/`estimated`), `ProviderCapabilities` (+ `FULL`), `ProviderSource`, temporary `StaticProviders`, a wire-format test. Re-exports `LocalHarness`, `PragmaticProvider`.
- `backend/src/providers/pragmatic.rs`: `LocalSupervisorHost` renamed `PragmaticProvider` (behaviour unchanged, capabilities FULL), `LocalHarness` moved here, with its tests. `serves` no longer checks the repository; routing is the dispatcher's job.
- `agents/host.rs` keeps `ModelRequirement` (`repository: String`, `DEFAULT_REPOSITORY = "hive"`, `of` fills it from `template.host`, None -> hive), `HostState`, `HarnessIdentity`, `HostError`, and the test double `FakeHost` (now a `Provider`, with `set_capabilities`).
- `agents/dispatcher.rs`: `Dispatcher::new(Arc<dyn ProviderSource>)`; `PlacementPolicy`/`ExplicitLoadPolicy` removed; exact routing with `repository not found: {name}` and `model not in repository {name}: {model}`; `capabilities(req)`.
- `lib.rs` registers `providers`; `main.rs` and `runtime/test_support.rs` use `StaticProviders`; module docs updated.

## Tests
New: `routes_only_to_named_repository`, `missing_repository_is_unavailable_with_reason`, `unserved_model_is_unavailable_with_reason` (renamed), `capabilities_come_from_the_provider`, `capabilities_are_full`, `capabilities_serialize_with_wire_names`, plus an `of` check for host -> repository in `requirement_comes_from_the_snapshot`.
Removed: `link_for_turn_refuses_when_policy_forbids_loading` and `NoTurnLoadPolicy` (the policy no longer exists; a turn always may load), and the pinned-host assertions in `serves_uses_discovery...` (superseded by routing tests). `preferred_host_wins` became `routes_only_to_named_repository`.

RED: `scripts/hive-test.sh -- agents::` with the new tests and the old dispatcher failed to compile (`E0432 unresolved import crate::agents::host::ModelHost` in dispatcher.rs and test_support.rs; before that, an unclosed delimiter I had introduced in host.rs, fixed).
GREEN:
- `scripts/hive-test.sh -- agents::`: 92 passed, 0 failed.
- `scripts/hive-test.sh -- --tests`: lib 566 passed, plus integration suites 7/10/6/2/5 passed, 0 failed.
- `scripts/hive-test.sh --features llamacpp`: 574 passed, 0 failed. No compiler warnings.
- `scripts/hive-test.sh --restore`: RESTORED.

## Concerns
- `FakeHost` keeps its name and lives in host.rs (cfg(test)), not renamed to FakeProvider; Task 3/5 can move it if wanted.
- lib.rs/agents/mod.rs are CRLF in the working copy (git warns); unchanged line-ending state.
- No rustfmt was run (Rust never runs locally); formatting is by hand.
