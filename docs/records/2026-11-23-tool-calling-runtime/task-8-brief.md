### Task 8: Runtime server and supervisor wiring

**Files:**
- Create: `backend/src/runtime/server.rs`
- Modify: `backend/src/main.rs`, `backend/src/supervisor/catalog.rs`, `backend/src/supervisor/manager.rs`, `backend/src/supervisor/control.rs` (test literals)

**Interfaces:**
- Consumes: Tasks 4–7.
- Produces:
  - `RuntimeDeps { connector: Arc<dyn WorkerConnector>, registry: Arc<ToolRegistry>, tool_loop: Arc<ToolLoop> }`.
  - `RuntimeServer::serve(listener: TcpListener, deps: RuntimeDeps)` — gives each connection its own `RuntimeSession`, using the same JSON-RPC envelope as `rpc.rs`. Make `rpc.rs`'s `RpcError`, error codes and frame helpers `pub(crate)` and reuse them rather than duplicating.
  - The unit-tested seam `RuntimeConnection::dispatch(&mut self, method: &str, params: Value, sink: &mut dyn LoopSink) -> Result<Value, RpcError>`.
  - Methods:
    - `model_props`: the worker's props verbatim; with no harness, the error `no model running`.
    - `tokenize`: proxied.
    - `set_sampling {config}`.
    - `send_turn {content, enable_thinking}`: runs the loop; the result `null` is sent after `loop_done`.
    - `reset`.
    - `list_tools`: returns `[ToolSpec]`.
- Produces (supervisor):
  - `ToolsConfig { sandbox: PathBuf }` and `Catalog.tools: Option<ToolsConfig>` (`#[serde(default)]`).
  - `ManagerConfig.chat_port: Option<u16>` (default `None`).
  - `HarnessInfo.chat_url: Option<String>`, set to `ws://{advertise_host}:{chat_port}` alongside `url` when the harness is Ready (`skip_serializing_if = "Option::is_none"`).
  - `Config.runtime_bind: String` (`--runtime-bind`, env `HARNESS_RUNTIME_BIND`, default `0.0.0.0:8779`).

In `run_supervisor`:
1. Build `BuiltinSource` with `Sandbox::new(catalog.tools.sandbox)`. If that fails, print `tools sandbox unavailable: {e}` and continue with `None`.
2. Build the registry and `ToolExecutor(ReadOnlyPolicy, 10 s)`, then `ToolLoop(step_cap 8)`.
3. Keep one `Arc` of the manager for both `control::serve` and `SupervisorConnector`.
4. Set `chat_port` from the runtime bind's port.
5. Bind the runtime listener, print `runtime listening on ws://{addr} ({n} tools)`, and `tokio::spawn` the runtime server before `control::serve`.

- [ ] **Step 1: Write failing tests:**

```rust
// runtime::server (FakeConnector/FakeLink from Task 7)
list_tools_returns_specs
set_sampling_is_used_by_send_turn          // the GenerateRequest.sampling equals the config set
reset_clears_transcript
model_props_without_harness_errors          // Err message "no model running"
send_turn_result_comes_after_loop_done      // sink's last note is loop_done before dispatch returns
// supervisor
catalog_parses_tools_sandbox                // `[tools]\nsandbox = "C:/llm-harness/sandbox"` → Some(ToolsConfig{sandbox})
catalog_without_tools_section_is_none
ready_info_carries_chat_url                 // chat_port Some(8779) → info.chat_url == Some("ws://192.168.1.101:8779")
```

- [ ] **Step 2: Run, expecting FAIL.** **Step 3: Implement.** Update every `ManagerConfig {..}`, `HarnessInfo {..}` and `Catalog {..}` literal in tests.
- [ ] **Step 4: Run, expecting PASS:** `scripts/hive-test.sh` (whole lib), then `scripts/hive-test.sh --features llamacpp` (whole lib with the engine).
- [ ] **Step 5: Commit:** `feat(supervisor): host the agent runtime and publish chat_url`.

