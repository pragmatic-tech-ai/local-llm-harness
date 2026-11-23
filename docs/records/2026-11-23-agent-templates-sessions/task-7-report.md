# Task 7 report: supervisor wiring

Status: DONE_WITH_CONCERNS (no separate RED run; see concerns)

## Built
- `backend/src/main.rs`: `AgentsStack` (associated fns only).
  - `host_names(&Catalog) -> Result<Vec<String>, String>` wraps `HostConfigs::build`. `run_supervisor` uses `?` on it, so a bad host list prints and exits non-zero.
  - `open(&Arc<Catalog>, host_names, &ToolRegistry) -> Result<AgentsStack, String>` builds `TemplateRules`, the `CatalogView` and the two stores under `data_dir/{templates,sessions}`.
  - `into_deps(self, manager, registry, tool_loop) -> RuntimeDeps` builds the `LocalSupervisorHost`s, the `Dispatcher` with `ExplicitLoadPolicy` and the `Hub`.
  - `template_count(&[TemplateNode])` counts template leaves.
- The stack is opened BEFORE the runtime port is bound. If it fails, stderr gets `agents data unavailable: {e}`, nothing is bound, no `chat_port` is set and no runtime line is printed. The control plane keeps running.
- On success, it prints `runtime listening on ws://{addr} ({n} tools, {t} templates, {s} sessions)`, then runs `PlacementWatcher::spawn` and serves.
- `CatalogView` comes from the real catalog. The new `supervisor::control::CatalogListing` has `runtimes()` ([{name, kind}]), `models()` ([{name, runtimes, size_hint}], the same code `list_models` now uses) and `kind_name`.
- `runtime/worker.rs`: removed `WorkerConnector`, `ReadyWorkerUrl`, `SupervisorConnector`, their test and the unused imports. `HarnessManager::ready_worker_url` stays, because `LocalHarness` uses it.
- `runtime/test_support.rs`: `FakeConnector::connect` is now an inherent method, because the trait is gone. `tool_loop` tests still use it through `TurnLinks`.

## Tests
- `main.rs::tests::startup_builds_stores_and_seeds`: a temp data dir gives 1 seeded template, 0 sessions, hosts `["hive"]` and 1 runtime.
- `scripts/hive-test.sh -- --tests`: lib 341, bin 5 (including the new test), http_engine 10, http_stream 6, http_warmup 2, rpc 5. 0 failed, no warnings, TEST_EXIT=0.
- `scripts/hive-test.sh --features llamacpp`: lib 349 passed, 0 failed, TEST_EXIT=0.
- `--restore` printed RESTORED. rustfmt was run on the touched files, and there are no CRs.

## Concerns
- No separate RED run. The test and the builder were written together; the GREEN runs above are the proof.
- `CatalogView.runtimes` now carries `kind`. Task 6's `TestRuntime::catalog()` fixture still has `{name}` only, which is harmless.
- The `HostConfigs` error goes through `Box<dyn Error>` from a `String`, so it prints the bare message and exits 1.

# Fix round 1

1. `runtime/worker.rs` tests: `use std::sync::Arc;` moved after the `crate::` imports (rustfmt order; `rustfmt --check` is clean).
2. `main.rs` tests:
   - `startup_builds_stores_and_seeds` now asserts the tree is group `Examples` holding template `Default`.
   - A `DataDir` drop guard removes the temp dir even when an assertion fails. It also holds the catalog and registry helpers.
   - New `unusable_data_dir_fails_to_open`: the data dir is under an existing file, and `AgentsStack::open` returns Err.
   - New `unsupported_host_is_a_host_names_error`: a non-supervisor host makes `host_names` return Err.
3. `supervisor/control.rs`: the unrelated rustfmt reflow in `start_accepts_n_ctx_override` is reverted. The diff against the previous commit no longer touches that test.

Evidence: `scripts/hive-test.sh -- --tests` gave lib 341, bin 7, http_engine 10, http_stream 6, http_warmup 2, rpc 5. 0 failed, no warnings, TEST_EXIT=0. `--restore` printed RESTORED.
