### Task 4: Dispatcher and the explicit-load policy

**Files:**
- Create: `backend/src/agents/dispatcher.rs`

**Interfaces:**
- Consumes: Task 3 `ModelHost`, `ModelRequirement`. Task 2 `SessionRecord`.
- Produces:
  - `Placement`, serialized as `{"state":"ready","host":..}`, `{"state":"needs_load","host":..,"evicts":..}` or `{"state":"unavailable","reason":..}`. The variants are `Ready { host: String }`, `NeedsLoad { host: String, evicts: Option<String> /*loaded model name*/ }` and `Unavailable { reason: String }`.
  - `DispatchError(String)`
  - `trait PlacementPolicy: Send + Sync`:
    - `fn choose_host<'a>(&self, req: &ModelRequirement, hosts: &'a [Arc<dyn ModelHost>]) -> Option<&'a Arc<dyn ModelHost>>`
    - `fn turn_may_load(&self) -> bool`
  - `ExplicitLoadPolicy`:
    - `choose_host` picks the preferred host if it serves the requirement, otherwise the first host that does.
    - `turn_may_load` is true.
  - `Dispatcher::new(hosts: Vec<Arc<dyn ModelHost>>, policy: Box<dyn PlacementPolicy>)`, with:
    - `requirement(record: &SessionRecord) -> ModelRequirement`, built from the snapshot.
    - `status(&self, req) -> Placement`, with no side effects.
    - `async load(&self, req) -> Result<Arc<dyn WorkerLink>, DispatchError>`
    - `async link_for_turn(&self, req, notify_loading: impl FnOnce()) -> Result<Arc<dyn WorkerLink>, DispatchError>`. It calls `notify_loading` exactly once before a load, and refuses with `model not loaded` when the policy disallows turn loads.
    - `async unload(&self, req)`
    - `subscribe(&self) -> broadcast::Receiver<()>`, which merges the hosts' signals.
  - An unavailable requirement fails `load` and `link_for_turn` with the `Unavailable` reason text: `no host serves {runtime}/{model}`.

- [ ] **Step 1: Write the failing tests** (`FakeHost` with scripted serves/state, counting `ensure_ready` calls, `pub(crate)` under `#[cfg(test)]` for Task 6):

```rust
status_never_loads                        // status() any number of times → 0 ensure_ready calls
needs_load_reports_eviction               // loaded gemma-4, req gemma-3 → NeedsLoad{host:"hive", evicts:Some("gemma-4…")}
n_ctx_mismatch_needs_load                 // loaded gemma-4@8192, req gemma-4@4096 → NeedsLoad{evicts:Some("gemma-4…")}
ready_when_matching
link_for_turn_loads_and_notifies_once     // NeedsLoad → notify called once → ensure_ready once → link
preferred_host_wins                       // two hosts serve it; req.host = "b" → b chosen
unserved_model_is_unavailable             // Unavailable{reason:"no host serves llamacpp/x"}
host_signal_reaches_subscribers
```

- [ ] **Step 2: Run, expecting FAIL.** **Step 3: Implement.** **Step 4: Run, expecting PASS.**
- [ ] **Step 5: Commit:** `feat(agents): dispatcher with an explicit-load placement policy`.

