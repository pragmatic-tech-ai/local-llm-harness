### Task 3: Model hosts and the local supervisor host

**Files:**
- Create: `backend/src/agents/host.rs`
- Modify:
  - `backend/src/supervisor/manager.rs`: add `HarnessInfo.n_ctx: Option<u32>` (`skip_serializing_if` None), which `start` records, and `HarnessManager::current() -> Option<HarnessInfo>`.
  - `backend/src/supervisor/catalog.rs`: add `[agents] data_dir` (`AgentsConfig`) and `[[host]]` (`HostConfig { name, kind, local: bool }`).
  - Update the test literals.

**Interfaces:**
- Produces:
  - `ModelRequirement { runtime: String, model: String, n_ctx: u32, host: Option<String> }`
  - `HostState { loaded: Option<(String /*runtime*/, String /*model*/, Option<u32> /*n_ctx*/)>, loading: bool }`
  - `HostError(String)`
  - `#[async_trait] trait ModelHost: Send + Sync` with:
    - `fn name(&self) -> &str`
    - `fn serves(&self, req: &ModelRequirement) -> bool`
    - `fn state(&self) -> HostState`
    - `fn current_link(&self, req) -> Option<Arc<dyn WorkerLink>>`
    - `async fn ensure_ready(&self, req) -> Result<Arc<dyn WorkerLink>, HostError>`
    - `async fn unload(&self) -> Result<(), HostError>`
    - `fn subscribe(&self) -> broadcast::Receiver<()>`, which fires on any state change
  - `LocalSupervisorHost::new(name: String, manager: Arc<HarnessManager<L,P>>, catalog: &Catalog)`:
    - `serves` checks discovery for the model being listed for the runtime.
    - `current_link` is `Some(WsWorkerLink(ready_worker_url))` only when the current harness is Ready and matches runtime, model and `n_ctx`.
    - `ensure_ready` calls `manager.start(runtime, model, Some(n_ctx))` when the current harness doesn't match.
    - `subscribe` maps `manager.subscribe()`.
  - `HostConfigs::build(catalog) -> Result<Vec<HostConfig>, String>`. It returns the implicit `[{name:"hive", kind:"supervisor", local:true}]` when there is no `[[host]]`. Any entry that isn't `kind = "supervisor"` with `local = true` fails with `host kind not supported yet: {kind}`.
  - `AgentsConfig::data_dir()` defaults to `C:\llm-harness\agent-data`.
- `n_ctx` matching: a harness started without an explicit `n_ctx` has `HarnessInfo.n_ctx = None` and never matches a requirement.

- [ ] **Step 1: Write the failing tests:**

```rust
// supervisor::manager
start_records_n_ctx_in_info              // start(.., Some(4096)) → info.n_ctx == Some(4096)
// agents::host (fake launcher/probe from the manager tests)
current_link_requires_exact_match        // ready (llamacpp,gemma,8192): same req → Some; n_ctx 4096 → None; other model → None
ensure_ready_starts_when_mismatched      // records start(runtime, model, Some(n_ctx))
ensure_ready_reuses_matching_harness     // no second start
// supervisor::catalog
host_config_defaults_to_local_hive
rejects_unsupported_host_kind            // kind="api" → Err("host kind not supported yet: api")
agents_data_dir_parses_and_defaults
```

- [ ] **Step 2: Run, expecting FAIL.** **Step 3: Implement.** **Step 4: Run, expecting PASS**, then run the whole lib and `-- --tests`.
- [ ] **Step 5: Commit:** `feat(agents): model hosts and the local supervisor host`.

