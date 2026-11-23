### Task 7: Supervisor wiring

**Files:**
- Modify: `backend/src/main.rs`, `backend/src/runtime/worker.rs` (remove `SupervisorConnector`/`WorkerConnector` if they are now unused; keep `ReadyWorkerUrl` only if it is still used)

**Interfaces:**
- Consumes: everything above.
- `run_supervisor` does the following:
  1. Builds the hosts from `HostConfigs::build`. On error it prints the message and exits non-zero, because a misconfigured host list is an operator error.
  2. Builds `TemplateRules` from the catalog (runtimes, `discover` models and their runtimes) and the registry names.
  3. Opens `TemplateStore` at `data_dir/templates` and `SessionStore` at `data_dir/sessions`. On an IO failure it prints `agents data unavailable: {e}` to stderr and still runs the control plane, but without the runtime, like a runtime bind failure.
  4. Builds the dispatcher with `ExplicitLoadPolicy`.
  5. Builds the `Hub` and spawns the placement watcher.
  6. Serves the runtime.
- The startup line becomes `runtime listening on ws://{addr} ({n} tools, {t} templates, {s} sessions)`.

- [ ] **Step 1: Write the failing test:** `startup_builds_stores_and_seeds` (exercise the extracted builder fn with a temp data dir: it seeds Default, and has 0 sessions).
- [ ] **Step 2–4: Run FAIL, implement, run PASS**: the whole lib, `-- --tests`, then `--features llamacpp` (long; use background or long timeouts).
- [ ] **Step 5: Commit:** `feat(supervisor): host templates, sessions and the dispatcher`.

