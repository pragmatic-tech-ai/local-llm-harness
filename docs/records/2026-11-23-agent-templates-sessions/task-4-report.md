# Task 4 report: dispatcher and explicit-load policy

## Built
- `backend/src/agents/dispatcher.rs` (new; registered in `agents/mod.rs`): `Placement` (serde tag `state`, snake_case), `DispatchError(String)`, `trait PlacementPolicy`, `ExplicitLoadPolicy`, `Dispatcher`.
- `Dispatcher` API: `new(hosts, policy)`, `requirement(&SessionRecord) -> ModelRequirement` (assoc fn), `status(&req) -> Placement`, `current_link(&req) -> Option<Arc<dyn WorkerLink>>` (no side effects), `async load(&req)`, `async link_for_turn(&req, notify_loading: impl FnOnce())`, `async unload(&req) -> Result<(), DispatchError>`, `subscribe() -> broadcast::Receiver<()>`.
- `host.rs`: removed `#[allow(dead_code)]` from FakeHost; added `FakeHost::serving_with_link(name, &[(rt, model)], link)`.

## Behaviour notes
- `status`: no host -> `Unavailable{reason:"no host serves {runtime}/{model}"}`; `current_link` present -> `Ready`; else `NeedsLoad` with `evicts` = loaded model name on that host, or None if nothing loaded. An n_ctx mismatch of the same model reports `evicts: Some(model)` (as the brief requires).
- `link_for_turn`: Unavailable error; existing link returned with no notify; policy forbids -> `model not loaded`; else notify once, then `ensure_ready`. No lock is held anywhere in the dispatcher during a load, so `status()` is never blocked (the host serialises loads internally).
- `unload` (my call): only unloads when the host's loaded runtime+model equal the requirement's, so one session cannot evict another's model; otherwise it is a no-op `Ok`.
- `subscribe`: one merger task per Dispatcher, started lazily by the first `subscribe` (needs a tokio runtime), merging all host receivers via `futures::stream::select_all`; aborted in `Drop`. The caller's receiver is taken before spawning. Poisoned lock recovered with `unwrap_or_else(|e| e.into_inner())`.

## Tests (hive)
New in `agents::dispatcher::tests` (14): status_never_loads, needs_load_reports_eviction, needs_load_with_nothing_loaded_evicts_nothing, n_ctx_mismatch_needs_load, ready_when_matching, link_for_turn_loads_and_notifies_once, link_for_turn_refuses_when_policy_forbids_loading, explicit_load_and_load_failure, preferred_host_wins, unserved_model_is_unavailable, unload_only_evicts_the_requirements_own_model, host_signal_reaches_subscribers, requirement_comes_from_the_snapshot, placement_serializes_with_a_state_tag.
Evidence: `scripts/hive-test.sh` whole lib: `296 passed; 0 failed`, TEST_EXIT=0, no warnings (an earlier run had a dead-code warning for `FakeHost::new`/`link`; fixed by using them in tests). `--restore` run. rustfmt run, LF verified. No separate RED run recorded (tests and implementation written together).

## Files
- D:/local-llm-harness/backend/src/agents/dispatcher.rs (new)
- D:/local-llm-harness/backend/src/agents/mod.rs, host.rs (edited)

## Concerns
- The workspace is not a git repository, so no commit was made.
- `unload` returns `Result` (brief left the type open).

## Fix report (review follow-ups)
1. `status` doc now states mid-load gives `NeedsLoad{evicts:None}`; test `status_while_loading_reports_needs_load_without_eviction`.
2. FakeHost now serialises loads/unloads with a loading lock, has `set_gate`, `loads()` and `load_started()`. Test `concurrent_turns_for_different_models_serialise_and_evict`: second load waits for the first, evicts it, and each load matched its own requirement at return.
3. `ModelHost::unload_if(&req) -> Result<bool, HostError>` (check + stop under the loading lock) for LocalSupervisorHost and FakeHost; `Dispatcher::unload` uses it. Tests: `unload_if_only_stops_the_matching_model`, `unload_if_waits_for_an_in_flight_load`, and the dispatcher unload test now covers the non-matching no-op and a repeat no-op.
4. Test `subscribe_with_no_hosts_returns_a_receiver`.
5. Removed `yield_now` from `host_signal_reaches_subscribers`.
6. One-line comment in `ExplicitLoadPolicy::choose_host`.
Evidence: whole lib on hive `301 passed; 0 failed`, no warnings, TEST_EXIT=0; rustfmt run; LF verified; `--restore` run.

Re-review fixes: shared `same_model` used by `matches` and `unload_if` (a Loading harness of the same model is also stopped; documented; FakeHost mirrors it via `set_loading`; tests added); spins and gate/turn awaits bounded with 5 s timeouts. Lib on hive 303 passed, rustfmt, LF, --restore done.
