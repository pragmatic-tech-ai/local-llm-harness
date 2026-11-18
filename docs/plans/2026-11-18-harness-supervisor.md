# Harness Supervisor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an always-on supervisor that manages harnesses (runtime + loaded model) as lifecycle objects — discover models, start/stop worker processes, hand off the live worker's ws url — plus a renderer harness picker that drives it and rebinds chat to the live harness.

**Architecture:** The supervisor is the backend crate's **default (no-feature, toolchain-free) build** running in a new `--supervisor` mode; it embeds no engine (dodging the LNK1319 co-link wall) and only spawns the pre-built worker exes named in a catalog. It exposes lifecycle RPC over the existing ws/JSON-RPC wire contract and **hands off** each live worker's url to the app, which connects its existing `HarnessClient` straight to it. Candle/llamacpp are single in-process children; `http` is a two-child harness (llama-server + the http-proxy). The renderer gains a `SupervisorClient`, a `HarnessPickerViewModel`, and a `ShellViewModel` coordinator that rebinds the chat client when the live harness changes.

**Tech Stack:** Rust (tokio, tokio-tungstenite, serde, toml, thiserror, clap — all already in the default build), Electron + mural/TypeScript (vitest).

**Spec:** `docs/specs/2026-11-18-harness-supervisor-design.md`

## Global Constraints

- The supervisor is the crate's **default build**: a plain `cargo build`/`test`/`clippy`/`fmt` with **no features** must compile and pass with no CUDA/clang/cmake toolchain. The supervisor and its model-discovery must **not** pull in the `candle`/`llamacpp` features — gguf metadata is read by a minimal self-contained parser, not an engine crate.
- Existing feature builds (`--features llamacpp`, `--features candle`/`candle-cuda`) and the worker chat RPC surface (`model_props`/`tokenize`/`set_sampling`/`send_turn`) stay unchanged.
- **All Rust builds and unit-test runs happen on the hive-laptop** via ssh (project rule), including the default no-feature build. Implementers write code; the controller syncs to the hive and runs `cargo build`/`clippy`/`test`/`fmt` there as authoritative verification. TS unit tests run via `vitest` (not a heavy compile); the live end-to-end app run is against the hive supervisor.
- Rust: snake_case, `cargo fmt` clean, `cargo clippy` clean (no blanket `-A`).
- TS house style: Allman braces, no semicolons, PascalCase public methods/interfaces, string literals hoisted to `private static readonly` class constants, view models extend `Observable`.
- **No absolute filesystem paths** (model files, exe paths) are ever sent to the renderer client.
- The handed-off worker url uses the configured **advertise host** (default `192.168.1.101`), never `127.0.0.1`/`localhost`.
- One live harness at a time: `start` evicts the current harness and **confirms the prior child has exited** (VRAM freed) before spawning the new one.
- Switching harness = **fresh context** (new worker, empty `ChatSession`); `send_turn` is unchanged, no transcript replay.
- Commits authored `Eugene Napryaglo <napryaglo@users.noreply.github.com>`; messages end with the `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` line.

## Review Focus

- **Eviction ordering (dual-VRAM OOM):** a `start` while a harness is live must fully stop and confirm the prior child's exit before spawning the next — never two VRAM-holding children at once. Pinned by Task R3's `start_evicts_then_spawns` test (fake launcher records `stop(before) < spawn(after)`).
- **Readiness vs crash:** `start` must distinguish "still loading" from "worker exited", return `failed` on timeout or early exit, and never return `ready` (or a url) for a dead worker, and never hang. Pinned by Task R3's `start_times_out_to_failed` and `start_fails_when_child_exits_early` tests.
- **Incompatible {runtime, model}:** `start` with a model the runtime cannot load is rejected up front with a clean error, not a spawned-then-crashing worker. Pinned by Task R2's compatibility-predicate tests and Task R4's `start_rejects_incompatible_pair`.
- **Handoff url host:** the url returned from `start`/`list_harnesses` is `ws://{advertise-host}:8777`, never loopback. Pinned by Task R3's `ready_url_uses_advertise_host` test.
- **Chat rebind leak:** switching harness unsubscribes the old client's turn signals and closes the old socket, so a stale worker's late frames cannot leak into the new transcript. Pinned by Task T1's `disconnect_rejects_pending_and_closes` and Task T4's `rebind_ignores_old_client_signals`.

---

### Task R1: Catalog config loader (backend, default build)

**Files:**
- Create: `backend/src/supervisor/mod.rs` (declare submodules)
- Create: `backend/src/supervisor/catalog.rs`
- Modify: `backend/src/lib.rs` (add `pub mod supervisor;`)
- Modify: `backend/Cargo.toml` (add `toml` to default deps)
- Test: `catalog.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `RuntimeKind { Candle, Llamacpp, Http }`; `RuntimeDef { name: String, kind: RuntimeKind, exe: PathBuf, requires_tokenizer: bool, n_ctx: Option<u32>, n_gpu_layers: Option<i32>, internal_port: Option<u16> }`; `Catalog { runtimes: Vec<RuntimeDef>, model_dirs: Vec<PathBuf>, advertise_host: String, worker_port: u16 }`; `Catalog::from_toml(&str) -> Result<Catalog, CatalogError>` and `Catalog::load(path: &Path)`.
- Consumed by: R2 (model_dirs, kinds), R3 (runtime defs, advertise_host, worker_port).

- [ ] **Step 1: Write failing tests** — `parses_runtimes_and_dirs` (a sample TOML with candle/llamacpp/http runtimes + two model dirs + advertise_host parses to the expected struct); `missing_advertise_host_defaults_to_hive` (`192.168.1.101`); `unknown_runtime_kind_errors`.
- [ ] **Step 2: Run on hive, verify fail** — `cargo test -p backend supervisor::catalog`.
- [ ] **Step 3: Implement** the types + `serde`/`toml` deserialization with defaults; `RuntimeKind` via `#[serde(rename_all = "lowercase")]`; `CatalogError` via `thiserror`.
- [ ] **Step 4: Verify on hive** — tests pass; `cargo build`/`clippy`/`fmt` no-feature green. Commit.

---

### Task R2: Model discovery + compatibility (backend, default build)

**Files:**
- Create: `backend/src/supervisor/discovery.rs`
- Create: `backend/src/supervisor/gguf_meta.rs` (minimal gguf header reader — architecture + name/param hint only)
- Modify: `backend/src/supervisor/mod.rs`
- Test: both files `#[cfg(test)]`

**Interfaces:**
- Produces: `GgufMeta { architecture: String, name: Option<String>, size_bytes: u64 }`; `read_gguf_meta(path: &Path) -> Result<GgufMeta, DiscoveryError>`; `DiscoveredModel { display_name: String, runtimes: Vec<String>, size_hint: String }`; `discover(catalog: &Catalog) -> Vec<DiscoveredModel>`; `runtime_can_load(kind: RuntimeKind, meta: &GgufMeta, tokenizer_present: bool) -> bool`.
- Consumes: `Catalog`, `RuntimeKind` (R1).

- [ ] **Step 1: Write failing tests** — `reads_architecture_from_header` (craft a minimal valid gguf byte buffer in a temp file with `general.architecture = "gemma3"` and assert it is read); `candle_requires_gemma_arch_and_tokenizer` (`runtime_can_load(Candle, gemma3, true) == true`; `(Candle, gemma3, false) == false`; `(Candle, llama, true) == false`); `llamacpp_and_http_accept_any_arch`; `discover_lists_models_with_capable_runtimes` (temp dir with two ggufs → each reports the runtimes that can load it, no absolute paths in the output); `display_name_is_file_stem`.
- [ ] **Step 2: Run on hive, verify fail** — `cargo test -p backend supervisor::discovery supervisor::gguf_meta`.
- [ ] **Step 3: Implement** the minimal gguf reader (magic `GGUF`, version, tensor/kv counts, walk the metadata KV array reading only the `general.architecture`/`general.name` string values and taking file size from fs metadata — do not decode tensors); the compatibility predicate from `RuntimeKind`; `discover` scanning `model_dirs` for `*.gguf`, resolving a sibling/`*tokenizer*.json` for candle, building `DiscoveredModel` (display_name = file stem; size_hint = human-readable bytes).
- [ ] **Step 4: Verify on hive** — tests pass; no-feature build pulls no candle/llamacpp. Commit.

---

### Task R3: Harness process-manager + state machine (backend, default build)

**Files:**
- Create: `backend/src/supervisor/manager.rs`
- Modify: `backend/src/supervisor/mod.rs`
- Test: `manager.rs` `#[cfg(test)]` (fakes for launcher, probe, clock)

**Interfaces:**
- Produces: `HarnessState { Stopped, Loading, Ready, Stopping, Failed }`; `HarnessInfo { id: String, runtime: String, model: String, state: HarnessState, url: Option<String> }`; traits `WorkerLauncher { fn spawn(&self, plan: &SpawnPlan) -> io::Result<Box<dyn WorkerHandle>> }`, `WorkerHandle { fn kill(&mut self); fn try_wait(&mut self) -> Option<ExitStatus>; fn wait(&mut self) }`, `ReadinessProbe { async fn await_ready(&self, url: &str, timeout: Duration) -> bool }`; `HarnessManager<L, P>` with `async fn start(&self, runtime: &str, model: &str) -> Result<HarnessInfo, ManagerError>`, `async fn stop(&self) -> Result<(), ManagerError>`, `fn list(&self) -> Vec<HarnessInfo>`, and a `harness_changed` broadcast (`tokio::sync::broadcast<HarnessInfo>`).
- Consumes: `Catalog`, `DiscoveredModel`/compatibility (R1/R2). A `SpawnPlan` is built from a `RuntimeDef` + resolved model (incl. the two-child plan for `http`).

- [ ] **Step 1: Write failing tests** (all with a fake launcher recording spawn/kill order, a fake probe with programmable readiness, a fake clock):
  - `start_evicts_then_spawns` — with a Ready harness, starting another records `kill(old)` and the old handle's `wait` **before** `spawn(new)`.
  - `start_times_out_to_failed` — probe never ready within timeout → state `Failed`, no url, child killed.
  - `start_fails_when_child_exits_early` — handle `try_wait` returns an exit before ready → `Failed`, clear error.
  - `ready_url_uses_advertise_host` — on success `url == "ws://192.168.1.101:8777"`.
  - `http_runtime_spawns_two_children` — an `http` start produces a `SpawnPlan` with both llama-server and the proxy, and `stop` kills **both**.
  - `start_rejects_incompatible_pair` — candle + non-gemma model → `ManagerError::Incompatible`, nothing spawned.
  - `id_is_runtime_colon_model`.
  - `crash_marks_failed_and_broadcasts` — a Ready harness whose handle later reports exit → state `Failed` + a `harness_changed` event.
- [ ] **Step 2: Run on hive, verify fail** — `cargo test -p backend supervisor::manager`.
- [ ] **Step 3: Implement** the state machine and `start`/`stop`/`list`; eviction (stop + confirm exit, then spawn); readiness via the probe with the catalog timeout; the `SpawnPlan` builder (single-child candle/llamacpp with their env; two-child http: llama-server on `internal_port` + `backend.exe --engine http --server-url http://127.0.0.1:<internal>` bound to the worker port); a background crash watch that marks `Failed` + broadcasts. Hold state behind a `tokio::sync::Mutex`.
- [ ] **Step 4: Implement the real adapters** `ProcessLauncher` (tokio `Command`, each child in a Windows **job object** so children die if the supervisor dies — note the mechanic for the reviewer) and `WsProbe` (`tokio_tungstenite::connect_async` + a `model_props` request with a short per-attempt timeout, retried until the overall timeout). These are exercised live in Task H1, not unit-tested.
- [ ] **Step 5: Verify on hive** — tests pass; `clippy`/`fmt` green. Commit.

---

### Task R4: Control RPC server (backend, default build)

**Files:**
- Create: `backend/src/supervisor/control.rs`
- Modify: `backend/src/supervisor/mod.rs`
- Test: `control.rs` `#[cfg(test)]` (dispatch over an in-memory request → json, with a manager built on R3 fakes)

**Interfaces:**
- Produces: `async fn serve(listener: TcpListener, manager: Arc<HarnessManager<..>>, catalog: Arc<Catalog>)`; an internal `dispatch(method, params) -> Result<Value, RpcError>` that is the unit-tested seam. Wire contract identical to `rpc.rs` (`{id,method,params}` → `{id,result}`/`{id,error}`; server→client notification `{method,params}`). Implemented independently of `rpc.rs` to avoid refactoring the proven chat path (documented in a module comment).
- Methods: `list_runtimes` → `[{name, kind, available}]` (available = exe exists); `list_models` → `discover(..)` output; `list_harnesses` → `manager.list()`; `start{runtime, model}` → `HarnessInfo` | error; `stop{id}` → `{}`; notification `harness_changed` forwarded from the manager broadcast to every connected client.

- [ ] **Step 1: Write failing tests** — `list_runtimes_reports_availability`; `start_returns_info_with_url` (manager fake → ready); `start_rejects_incompatible_pair` (error response, not a crash); `stop_calls_manager`; `unknown_method_errors` (`-32601`); `list_models_omits_absolute_paths`.
- [ ] **Step 2: Run on hive, verify fail** — `cargo test -p backend supervisor::control`.
- [ ] **Step 3: Implement** the dispatch + a ws accept loop that subscribes each connection to the manager's `harness_changed` broadcast and relays notifications. Reuse the same error codes as `rpc.rs`.
- [ ] **Step 4: Verify on hive** — tests pass; `clippy`/`fmt` green. Commit.

---

### Task R5: `--supervisor` mode entry wiring (backend, default build)

**Files:**
- Modify: `backend/src/main.rs`
- Test: `main.rs` `#[cfg(test)]` for arg/env parsing only.

**Interfaces:**
- Consumes: `Catalog::load`, `discover`, `HarnessManager`, `control::serve`.
- Adds flags/env: `--supervisor` (bool, env `HARNESS_SUPERVISOR`), `--supervisor-bind` (env `HARNESS_SUPERVISOR_BIND`, default `0.0.0.0:8778`), `--catalog` (env `HARNESS_CATALOG`), `--advertise-host` (env `HARNESS_ADVERTISE_HOST`, overrides catalog).

- [ ] **Step 1: Write failing test** — `supervisor_flag_parses_bind_and_catalog` (env/args → the supervisor `Config` fields).
- [ ] **Step 2: Run on hive, verify fail**.
- [ ] **Step 3: Implement** — when `--supervisor` is set, load the catalog (advertise-host override), build `ProcessLauncher` + `WsProbe` + `HarnessManager`, bind `--supervisor-bind`, and run `control::serve` (instead of the existing engine path). The existing engine/`rpc::serve` path is untouched when the flag is absent.
- [ ] **Step 4: Verify on hive** — tests pass; the supervisor binary starts, binds `:8778`, and `list_runtimes`/`list_models` answer over a ws client (smoke, no GPU needed). `clippy`/`fmt` green. Commit.

---

### Task T1: HarnessClient disconnect + disposable TurnSignal (renderer)

**Files:**
- Modify: `app/src/renderer/src/HarnessClient.ts`
- Test: `app/src/renderer/src/HarnessClient.test.ts`

**Interfaces:**
- Produces: `TurnSignal.Subscribe(handler) -> IDisposable` (`{ Dispose(): void }`); `HarnessClient.Disconnect(): void` (close socket, clear handlers, reject pending). `Connect` on an already-connected client first disconnects the old socket.

- [ ] **Step 1: Write failing tests** — `subscribe_returns_disposable_that_unsubscribes` (after `Dispose()`, `Raise` does not call the handler); `disconnect_rejects_pending_and_closes` (a pending request rejects with "Harness not connected" and the socket is closed); `reconnect_closes_previous_socket`.
- [ ] **Step 2: Run, verify fail** — `npm test -- HarnessClient`.
- [ ] **Step 3: Implement** — `TurnSignal` returns a disposable (splice on dispose); `Disconnect` closes the socket, nulls it, clears handlers, rejects pending; `Connect` calls `Disconnect` on the prior socket first.
- [ ] **Step 4: Verify** — tests pass. Commit.

---

### Task T2: SupervisorClient (renderer)

**Files:**
- Create: `app/src/renderer/src/SupervisorClient.ts`
- Test: `app/src/renderer/src/SupervisorClient.test.ts` (reuse `FakeSocket`)

**Interfaces:**
- Produces: `interface RuntimeInfo { name, kind, available }`, `ModelInfo { name, runtimes, size_hint }`, `HarnessInfo { id, runtime, model, state, url?: string }`; `class SupervisorClient` with `Connect(url)`, `ListRuntimes()`, `ListModels()`, `ListHarnesses()`, `Start(runtime, model): Promise<HarnessInfo>`, `Stop(id): Promise<void>`, and `OnHarnessChanged: TurnSignal<HarnessInfo>`; `IsConnected`. Same JSON-RPC framing as `HarnessClient` (queue-before-open, id-matched responses, notification dispatch).

- [ ] **Step 1: Write failing tests** — `start_resolves_with_harness_info`; `list_models_parses`; `harness_changed_notification_raises_signal`; `request_before_open_is_queued_then_flushed`; `rpc_error_rejects`.
- [ ] **Step 2: Run, verify fail** — `npm test -- SupervisorClient`.
- [ ] **Step 3: Implement** mirroring `HarnessClient`'s framing (factor shared framing only if trivial; otherwise mirror and note it). Hoist method-name constants.
- [ ] **Step 4: Verify** — tests pass. Commit.

---

### Task T3: HarnessPickerViewModel (renderer)

**Files:**
- Create: `app/src/renderer/src/HarnessPickerViewModel.ts`
- Test: `app/src/renderer/src/HarnessPickerViewModel.test.ts`

**Interfaces:**
- Produces: `class HarnessPickerViewModel extends Observable` with observable `Runtimes: ObservableCollection<RuntimeInfo>`, `Models: ObservableCollection<ModelInfo>`, `SelectedRuntime`/`SelectedModel`, `State: HarnessState` (string), `StatusText`, `CanStart`/`CanStop`; `StartCommand`/`StopCommand`; `Refresh()`; and `OnReady: TurnSignal<string>` raising the worker url when a harness reaches `ready` (and an `OnCleared` when stopped/failed). Consumes a `SupervisorClient`.

- [ ] **Step 1: Write failing tests** (fake `SupervisorClient`) — `refresh_populates_runtimes_and_models`; `start_sets_loading_then_ready_and_raises_url`; `stop_clears_and_raises_cleared`; `start_disabled_without_selection`; `harness_changed_to_failed_updates_state`; `incompatible_start_surfaces_error_text`.
- [ ] **Step 2: Run, verify fail**.
- [ ] **Step 3: Implement** — extends `Observable`; wire commands (`RelayCommand`) + `RaisePropertyChanged`; subscribe to `OnHarnessChanged`; hoist all label/status string literals to `private static readonly` constants.
- [ ] **Step 4: Verify** — tests pass. Commit.

---

### Task T4: ChatViewModel rebind + chat-disabled (renderer)

**Files:**
- Modify: `app/src/renderer/src/ChatViewModel.ts`
- Modify: `app/src/renderer/src/InstrumentPanelViewModel.ts` (accept a nullable/rebindable client if it holds one)
- Test: `app/src/renderer/src/ChatViewModel.test.ts`

**Interfaces:**
- Produces: `ChatViewModel.Bind(client: HarnessClient | null): void` (dispose prior subscriptions, subscribe the new client, update `CanChat`); observable `CanChat: boolean` (false when unbound); `SendCommand` is a no-op/disabled while `CanChat` is false. Constructor no longer requires a live client (starts unbound).

- [ ] **Step 1: Write failing tests** — `starts_unbound_cannot_chat`; `bind_enables_chat_and_routes_chunks`; `rebind_ignores_old_client_signals` (chunks from a disposed prior client do not touch the transcript); `unbind_disables_send`.
- [ ] **Step 2: Run, verify fail**.
- [ ] **Step 3: Implement** — move the turn-signal subscriptions out of the constructor into `Bind`, storing the `IDisposable`s and disposing them on the next `Bind`/unbind; gate `Send` on `CanChat`; raise `CanChat` property change.
- [ ] **Step 4: Verify** — tests pass. Commit.

---

### Task T5: ShellViewModel coordinator (renderer)

**Files:**
- Create: `app/src/renderer/src/ShellViewModel.ts`
- Test: `app/src/renderer/src/ShellViewModel.test.ts`

**Interfaces:**
- Produces: `class ShellViewModel extends Observable` owning a `HarnessPickerViewModel` (`Picker`) and a `ChatViewModel` (`Chat`); on `Picker.OnReady(url)` it constructs/`Connect`s a fresh `HarnessClient` and calls `Chat.Bind(client)`; on `Picker.OnCleared` it calls `Chat.Bind(null)` and disconnects the old client. Takes a `SupervisorClient` and a `HarnessClient` factory (default `() => new HarnessClient()`).

- [ ] **Step 1: Write failing tests** (fake supervisor + injected client factory) — `ready_binds_fresh_client_to_chat`; `switch_disconnects_previous_client_then_binds_new` (old client `Disconnect` called before new `Bind`); `cleared_unbinds_chat`.
- [ ] **Step 2: Run, verify fail**.
- [ ] **Step 3: Implement** the coordinator.
- [ ] **Step 4: Verify** — tests pass. Commit.

---

### Task T6: Main-process supervisor url + preload + bootstrap + mural views (renderer/main)

**Files:**
- Create: `app/src/main/supervisor-url.ts` + `app/src/main/supervisor-url.test.ts`
- Modify: `app/src/main/index.ts` (IPC channel `supervisor-url`)
- Modify: `app/src/preload/index.ts`, `app/src/preload/index.d.ts` (`SupervisorUrl()`)
- Modify: `app/src/renderer/src/main.ts` (supervisor-driven bootstrap; fall back to the existing single-backend path when `HARNESS_SUPERVISOR_URL` is unset)
- Create: `app/src/renderer/src/views/HarnessPanel.mu` + `.mu.js` (picker: runtime + model selectors, Start/Stop, status)
- Create: `app/src/renderer/src/views/Shell.mu` + `.mu.js` (picker docked above the chat)
- Modify: `app/src/renderer/src/app.mu` (merge Shell + HarnessPanel resources; host `ShellViewModel`)
- Modify: `app/src/renderer/src/views/Chat.mu` (disable composer + show a "no harness loaded" affordance when `CanChat` is false)

**Interfaces:**
- Produces: `BackendUrl`-style `SupervisorUrl.Resolve(env): string | null` (env `HARNESS_SUPERVISOR_URL`); `window.harness.SupervisorUrl(): Promise<string | null>`.

- [ ] **Step 1: Write failing test** — `supervisor-url.test.ts`: `Resolve` returns the trimmed url or null (mirror `backend-url.test.ts`).
- [ ] **Step 2: Run, verify fail** — `npm test -- supervisor-url`.
- [ ] **Step 3: Implement** the pure resolver, the IPC handler, the preload bridge method + d.ts, and the renderer bootstrap: when a supervisor url is present, create `SupervisorClient` + `ShellViewModel` and host it, then `Refresh()`; otherwise keep today's direct `ChatViewModel` path. Author the mural `.mu` views (Allman/house style; the `.mu.js` are generated siblings — build the markup and regenerate).
- [ ] **Step 4: Verify** — `supervisor-url` test passes; full `npm test` green; `npm run build` (electron-vite) compiles. Commit. (Live UI proof is Task H1.)

---

### Task H1: Hive deploy + live GPU proof (controller/hive)

**Files:** Modify `docs/specs/hive-llama-server-setup.md`. Hive-side: catalog TOML, `start-supervisor.cmd`, scheduled task, firewall.

- [ ] **Step 1:** On the hive, build the default (supervisor) binary and the two worker binaries (`build-both.cmd` already produces `backend-candle.exe`/`backend-llamacpp.exe`; add the default `backend.exe` for the supervisor + http proxy). Write the catalog TOML (`C:\llm-harness\harness-build\catalog.toml`): candle/llamacpp/http runtimes with exe paths + launch params, model dirs (`C:\llm-harness\models` + the LM Studio dir), `advertise_host = "192.168.1.101"`, `worker_port = 8777`.
- [ ] **Step 2:** `start-supervisor.cmd` (sets `HARNESS_SUPERVISOR=1`, `HARNESS_SUPERVISOR_BIND=0.0.0.0:8778`, `HARNESS_CATALOG=...`; logs). Scheduled task **`harness-supervisor`** (Run As visor, interactive, one-time — it must be a task so its spawned workers survive ssh disconnect). Firewall inbound allow **"harness-supervisor 8778"** (TCP 8778).
- [ ] **Step 3 (live proof):** From the dev machine set `HARNESS_SUPERVISOR_URL=ws://192.168.1.101:8778`, launch the app, and via the picker: `list_models` shows discovered ggufs; **start candle (4B)** → reaches `ready`, chat streams a turn; **start llamacpp (12B)** → candle is evicted (confirm VRAM dropped then rose, no OOM), chat streams; **start http (12B)** → two-child harness serves chat; **stop** frees VRAM; a second turn after a switch confirms fresh context. Confirm no absolute paths crossed the wire.
- [ ] **Step 4:** Document the supervisor task, catalog, firewall, and the picker-driven flow in `docs/specs/hive-llama-server-setup.md`; note the per-engine tasks remain as manual fallbacks. Restore the hive to a sensible default (supervisor running, no harness loaded). Commit.

---

## Notes on execution order & machines

- R1 → R2 → R3 → R4 → R5 are the backend spine (each on the hive for build/test). R3 is the riskiest (eviction/readiness/crash) and carries most of the Review Focus.
- T1 → T2/T3 (independent) → T4 → T5 → T6 are the renderer; TS unit tests run via vitest locally.
- H1 (hive, controller) is last: wires the real adapters to the real GPU and proves the whole loop end to end.
