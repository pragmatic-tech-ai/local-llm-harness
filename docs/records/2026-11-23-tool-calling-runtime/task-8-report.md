# Task 8 report: runtime server and supervisor wiring

## Built
- `backend/src/runtime/server.rs`: `RuntimeDeps`, `RuntimeServer::serve`, `RuntimeConnection` (`dispatch` is the unit-tested seam), `WsSink` (writes notifications to the socket as the loop runs; a write failure maps to `SinkClosed`). Methods: `model_props`, `tokenize` (both error `no model running` without a harness), `set_sampling`, `send_turn` (result `null` sent after `loop_done`), `reset`, `list_tools`. Unknown method uses `ERR_METHOD_NOT_FOUND`, bad params `ERR_INVALID_PARAMS`. If `ToolLoop::run` returns `Err(SinkClosed)` the connection is marked closed, no response is written, and the loop ends quietly.
- `rpc.rs`: envelope pieces made `pub(crate)` (`Request`, `RpcError`, `ERR_*`, `Ws`, `BoxError`) and a new `Frames` helper (result/error/notification frames and `send`); the existing `Connection` now delegates to it (no duplication).
- Supervisor: `ToolsConfig` + `Catalog.tools: Option<ToolsConfig>`; `ManagerConfig.chat_port`; `HarnessInfo.chat_url` (Ready only, `ws://{advertise_host}:{chat_port}`, skipped when None); `Config.runtime_bind` (`--runtime-bind`/`HARNESS_RUNTIME_BIND`, default `0.0.0.0:8779`).
- `run_supervisor`: builds sandbox (prints `tools sandbox unavailable: {e}` and continues with none), registry, `ToolExecutor(ReadOnlyPolicy, 10 s)`, `ToolLoop(cap 8)`; binds the runtime listener first so `chat_port` is its real port; one `Arc` manager shared by `SupervisorConnector` and `control::serve`; prints `runtime listening on ws://{addr} ({n} tools)`; spawns the runtime server before `control::serve`. Helper `Config::tool_registry` is an associated fn.

## Tests
New: server (`list_tools_returns_specs`, `set_sampling_is_used_by_send_turn`, `reset_clears_transcript`, `model_props_without_harness_errors`, `model_props_and_tokenize_are_proxied`, `send_turn_result_comes_after_loop_done`, `unknown_method_and_bad_params_use_rpc_codes`, `closed_sink_ends_the_connection`); catalog (`catalog_parses_tools_sandbox`, `catalog_without_tools_section_is_none`); manager (`ready_info_carries_chat_url`, also asserts None without chat_port); main (`runtime_bind_defaults_to_8779`). Existing Catalog/ManagerConfig literals updated in manager, control and discovery tests.

## TDD evidence
RED: supervisor tests written first; hive run failed to compile (E0422/E0560/E0609: no `ToolsConfig`, no `chat_url`/`chat_port`), exit 101. GREEN after implementation: whole lib 207 passed, 0 failed. See the final-run results in the reply (`--tests`, `--features llamacpp`).

## Self-review / concerns
- The server module's own tests were written together with the implementation (the module did not exist, so its RED was the compile failure above).
- Socket-level path (`RuntimeConnection::run`, `WsSink`) is not covered by an end-to-end test; only `dispatch` is. It mirrors `rpc.rs` `Connection::run`.
- Binding the runtime port happens before the control port; a busy 8779 aborts supervisor startup.
- A tool-less run (no `[tools]`) prints `(1 tools)` as specified.

## Fix report: review follow-ups
1. `TokenizeParams`/`SetSamplingParams` are now `pub(crate)` in rpc.rs and imported by server.rs; the duplicate param structs and decode helper are gone, replaced by one `Frames::params(Value)` used by both servers.
2. Runtime bind failure degrades: `Config::bind_runtime` prints `runtime bind {addr} failed: {e}; chat through the runtime is unavailable` to stderr and returns None; the runtime is bound before the manager so `chat_port` is None on failure (no chat_url) and the control plane continues. Tested by `runtime_bind_failure_degrades_to_none` (binds an occupied loopback port).
3. Unusable sandbox with `[tools]` configured is reported with `eprintln!`; absent `[tools]` stays quiet.
4. New `serve_streams_notifications_before_the_response`: real `RuntimeServer::serve` on 127.0.0.1:0 with FakeConnector/FakeLink; asserts notifications (last is `loop_done`, includes `turn_done`) precede a `null` response, then an unparseable frame yields id null and `ERR_PARSE`. rpc's `TestClient` gained `connect_to(addr)` and `send_raw` (factored, not duplicated).
5. Removed rpc.rs `error_frame` and `send_json` wrappers; callers use `Frames` directly.
6. Reverted the unrelated rustfmt churn in manager.rs tests.
7. Module header added to server.rs (sequential requests; disconnect noticed at next notification write; turn cancellation is in neither the spec's Non-goals nor its Extension points, so it needs a spec addition first).

Evidence (hive): whole lib 208 passed; `-- --tests` all green (lib 208, main 4, http_engine 9, http_stream 6, http_warmup 2, rpc 5), no warnings. llamacpp not re-run (no gated code touched). LF verified, rustfmt applied to rpc.rs, server.rs, main.rs only.
