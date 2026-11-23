# Task 3 report: model hosts and the local supervisor host

## Built
- `backend/src/agents/host.rs`: `ModelRequirement`, `HostState`, `HostError`, `trait ModelHost`, `trait LocalHarness` (the slice of HarnessManager the host needs; blanket-implemented for every `HarnessManager<L,P>`), `LocalSupervisorHost` (non-generic; holds `Arc<dyn LocalHarness>`; `new<H: LocalHarness>(name, Arc<H>, &Catalog)` so `Arc<HarnessManager<L,P>>` works), and test-only `FakeHost`.
- Choice: trait object rather than generics over L,P, so the host is one plain type and tests use a recording `FakeHarness` instead of fake processes (the manager's own tests cover the real manager).
- `supervisor/manager.rs`: `HarnessInfo.n_ctx: Option<u32>` (skip if None, recorded by `start` from the override), `HarnessManager::current()`.
- `supervisor/catalog.rs`: `AgentsConfig { data_dir: Option<PathBuf> }` with `data_dir()` defaulting to `C:\llm-harness\agent-data`; `HostConfig { name, kind, local }`; `Catalog.agents`, `Catalog.hosts` (`[[host]]`); `HostConfigs::build(&Catalog) -> Result<Vec<HostConfig>, String>` (implicit hive, else `host kind not supported yet: {kind}`; a supervisor with `local=false` is also rejected, with its kind in the message).
- Test literals in control.rs, discovery.rs, manager.rs updated.

## Behaviour notes
- `serves`: cached discovery (`refresh()` re-runs it); model display_name + runtime match; if `req.host` is Some and differs from the host name, false.
- `state()`: `loaded` only when Ready; `loading` when Loading.
- `current_link`: Some(WsWorkerLink(ready_worker_url)) only when Ready and runtime/model/`n_ctx == Some(req.n_ctx)`.
- `ensure_ready`: serialised by an internal mutex; reuses a matching harness; otherwise `start(runtime, model, Some(n_ctx))`; errors map to `HostError(e.to_string())`.
- `subscribe`: spawns one forwarder task per call (needs a tokio runtime); maps `HarnessInfo` events to `()`; the task ends when the manager closes or on the first event after the receiver is dropped.

## FakeHost API (`pub(crate)`, `#[cfg(test)]`, `crate::agents::host::FakeHost`; all constructors return `Arc<FakeHost>`)
- `FakeHost::new(name)` serves nothing; `FakeHost::serving(name, &[(runtime, model)])`; `FakeHost::with_link(name, Arc<FakeLink>)` serves nothing, custom link.
- `.link() -> Arc<FakeLink>`: the link returned by `current_link`/`ensure_ready` (default `FakeLink::new(false, vec![])`).
- `.set_state(HostState)` sets and fires the signal; `.fire()` fires only; `.fail_ensure(Option<&str>)`; `.ensure_ready_calls() -> usize`; `.unload_calls() -> usize`.
- `ModelHost` impl: `serves` = listed (runtime, model) and host pin matches; `state` = scripted; `current_link` = Some only if `state.loaded == (runtime, model, Some(n_ctx))`; `ensure_ready` counts, errors if scripted, otherwise marks req loaded (fires if it changed) and returns the link; `unload` counts, resets state, fires; `subscribe` = broadcast of the fire signal.
- Needs a `with_link` + serving combination? Not provided; construct via `serving` and use `.link()` (or add a builder when Task 4 needs it).

## Tests (hive, `scripts/hive-test.sh`)
New: manager `start_records_n_ctx_in_info`; host `current_link_requires_exact_match`, `harness_without_n_ctx_never_matches_and_loading_is_not_a_link`, `ensure_ready_starts_when_mismatched`, `ensure_ready_reuses_matching_harness`, `serves_uses_discovery_and_refresh_picks_up_new_models`, `unload_stops_and_subscribe_forwards_changes`, `fake_host_scripts_serves_state_and_counts`; catalog `host_config_defaults_to_local_hive`, `host_config_accepts_local_supervisor`, `rejects_unsupported_host_kind`, `rejects_non_local_supervisor`, `agents_data_dir_parses_and_defaults`.
Evidence: whole lib `279 passed; 0 failed`, `-- --tests` all suites ok, TEST_EXIT=0. Caveat: tests and implementation were written together; no separate RED run was recorded. After the green run, only an `#[allow(dead_code)]` on `FakeHost` (silences the unused-warning until Task 4) was added, and a rustfmt-induced reflow of two unrelated manager test lines was reverted. `--restore` run.

## Concerns
- `LocalSupervisorHost::subscribe` requires a tokio runtime context.

## Fix report (review follow-ups)
1. Forwarder: `broadcast::Sender` has no `closed()`, so the suggested select is not possible. Instead there is now ONE forwarder task for all subscribers (started by the first `subscribe`), aborted in `Drop for LocalSupervisorHost`. Test `forwarder_ends_when_host_is_dropped` asserts two subscribers share one harness subscription and it is released after drop.
2. `unload` takes the `loading` lock. Test `unload_waits_for_an_in_flight_load` (gated fake `start`) shows stop is not called until the load completes.
3. Test `every_served_model_resolves_for_start`: both discovery and `resolve_model` key on the gguf file stem (incl. a mixed-case `.GGUF` ext); every discovered display_name resolves. No alignment needed.
4. `FakeHarness::stop` now broadcasts a Stopped info; `unload_stops_and_subscribe_forwards_changes` asserts the signal.
5. Final tree on hive: lib `282 passed; 0 failed`, `-- --tests` ok, no warnings; rustfmt run; LF verified (0 CR); `--restore` run.

Fix round 2: forwarder mutex now recovers from poisoning (subscribe and Drop); `subscribe` takes the receiver before spawning the forwarder. `agents::host` tests pass on the hive; rustfmt, LF, --restore done.
