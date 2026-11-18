# Harness Supervisor (always-on control plane) — Design

Date: 2026-11-18
Status: proposed (awaiting approval)

## Intent & why

Today the backend is launched *to be* one thing: `HARNESS_ENGINE` + `HARNESS_MODEL`
are read once at process start, so one process = one engine = one loaded model.
Switching runtime or model means stopping one scheduled task and starting another by
hand on the hive. There is no always-on service and no notion of a harness as a
managed thing.

We want a **harness** to be a first-class entity with a **lifetime**: it can be
started (runtime brought up, model loaded onto the GPU) and stopped (unloaded, VRAM
freed). An always-on **supervisor** owns those lifetimes. In the future, an *agent
definition* will name a `{runtime, model}` (plus, later, tools/configs/skills); when
the agent starts, the supervisor spins its harness up, and when it ends the harness is
stopped and unloaded. This design builds the supervisor and the manual
drive-it-from-the-UI path; agent definitions are a later consumer of the same
lifecycle API.

**Harness (minimal representation)** = a runtime + a loaded model. `runtime` and
`model` are **separate axes**: a runtime is an engine host (candle / llamacpp / http);
a model is a discovered gguf. A *running* harness is the pairing, brought to life.

## Goals

- An always-on supervisor process that manages harness lifecycles over RPC:
  `list_runtimes`, `list_models`, `list_harnesses`, `start{runtime, model}`,
  `stop{id}`, plus a lifecycle-change notification.
- `start` brings a worker through `loading -> ready` and returns the worker's
  `ws://host:port`; the app connects its existing chat client **directly** to that
  worker (handoff; the supervisor does not proxy chat traffic).
- `stop` kills the worker and frees VRAM. The GPU holds one harness at a time, so
  `start` **evicts** the current one first.
- **Models are discovered** by scanning configured directories (read gguf metadata for
  name/architecture/size); **runtimes are declared** in a small catalog config
  (binary path + launch recipe + readiness check).
- A renderer **harness picker** in the app: lists runtimes and discovered models,
  starts/stops a harness, auto-connects chat to the handed-off url, and disables chat
  until a harness is `ready`.

## Non-goals

- Agent definitions (the future consumer). Out of scope here; the lifecycle API is
  designed so they can drive it later without change.
- Multiple concurrent live harnesses / multi-GPU scheduling. One live at a time on the
  6 GB GPU.
- Supervisor proxying chat/inference traffic (we chose handoff).
- In-process load/unload of models or hot-swapping engines inside one process.
- Authentication / multi-tenant control plane (LAN-trusted, as today).
- Changing the worker's own chat RPC surface (`model_props` / `tokenize` /
  `set_sampling` / `send_turn`) — it is reused unchanged.

## Anti-goals (would make this a failure even if it "works")

- The supervisor linking any inference engine into its own process (defeats the
  LNK1319 avoidance and the toolchain-free build).
- Leaking absolute filesystem paths of models/binaries to the renderer client.
- A `start` that returns before the model can actually serve a turn (client connects
  to a not-yet-ready worker and the first turn fails).
- Two harnesses holding VRAM at once because eviction didn't wait for the old process
  to die (OOM on the next load).
- The app silently spawning/killing server-side harnesses the user didn't ask for.

## Constraints

- **Engines cannot co-link**: `backend-llamacpp.exe` and `backend-candle.exe` are
  separate binaries (LNK1319: llama-cpp-sys cmake-CUDA vs candle cudarc). The
  supervisor must therefore *spawn worker processes*, never embed an engine.
- **6 GB GPU**: harnesses are mutually exclusive; ~4.7 GB (llamacpp 12B) / ~5.8 GB
  (candle 4B) / ~5.1 GB (llama-server). One live at a time.
- **Windows / OpenSSH**: OpenSSH puts session children in a job object killed on
  disconnect. The supervisor itself must run as the one long-lived scheduled task; its
  spawned workers are *its* children and survive as long as it does.
- **All Rust builds + unit-test runs happen on the hive-laptop** via ssh, not the dev
  machine (project rule). The supervisor itself is the crate's default (no-feature,
  toolchain-free) build, so it compiles anywhere, but authoritative build/test is on
  the hive.
- Rust: snake_case, `cargo fmt` clean, `cargo clippy` clean. TS house style: Allman
  braces, no semicolons, PascalCase public methods/interfaces, string literals hoisted
  to `private static readonly` class constants, view models extend `Observable`.
- Commit author `Eugene Napryaglo <napryaglo@users.noreply.github.com>`; messages end
  with the `Co-Authored-By: Claude` line. Never leak the real email.

## Approaches considered

**Connection topology (how a chat turn reaches the live harness):**

- *Supervisor proxies* chat RPC + relays token streams. Single stable client endpoint,
  reconnect-free — but the supervisor must relay streams.
- *Supervisor hands off* — **CHOSEN**. `start` returns the worker's address; the app
  connects straight to the worker for chat, to the supervisor only for lifecycle. The
  supervisor stays lean (no stream relay); workers stream as they do today. Cost: the
  client juggles two connections and reconnects when the harness changes. Chosen
  because it keeps the supervisor simple and reuses the worker chat path verbatim.

**Supervisor packaging:**

- *New separate crate/binary* purely for supervision.
- *A `--supervisor` mode of the existing backend crate's default build* — **CHOSEN**.
  The default (no engine features) build already compiles toolchain-free and already
  owns the ws/JSON-RPC framing (`rpc.rs`). It contains no engine, so it cannot hit the
  co-link wall; it only launches the pre-built worker exes named in its catalog.
  Chosen for maximal reuse of the existing RPC stack and zero new toolchain needs.

**Model availability:**

- *Fixed declared list* in config.
- *Discovered by scanning a models dir* — **CHOSEN** (reads gguf metadata). Runtimes
  stay declared (binaries + launch recipe). Chosen so adding a model is just dropping a
  file.

**Rejected outright:** in-process load/unload across engines (co-link wall);
auto-starting a default harness on app launch (harnesses cost VRAM + load time;
explicit lifecycle is the point).

## The decided "how"

### Process & ports

- **Supervisor**: backend crate default build, new `--supervisor` mode, binds a
  **control port** on the LAN (default `0.0.0.0:8778`, env `HARNESS_SUPERVISOR_BIND`).
  Runs on the hive as the one long-lived scheduled task `harness-supervisor`. The app
  runs on the **dev machine** and reaches the supervisor over the LAN
  (`ws://192.168.1.101:8778`), exactly as Phase A reached llama-server.
- **Advertised host**: the supervisor knows the host clients should dial it by
  (config `HARNESS_ADVERTISE_HOST`, default the hive LAN ip `192.168.1.101`). The
  handed-off worker url it returns from `start` uses this host, never `127.0.0.1` /
  `localhost` — a loopback url would be unreachable from the dev-machine renderer and
  chat would silently fail to connect after a successful `start`.
- **Workers**: spawned by the supervisor as child processes, each an existing worker
  exe (`backend-candle.exe` / `backend-llamacpp.exe` / the http proxy — see the http
  runtime note below), bound to the **LAN** on a single **fixed worker port**
  (`0.0.0.0:8777`), since only one is ever live. The supervisor launches them with the
  env the workers already read (`HARNESS_ENGINE`, `HARNESS_MODEL`, `HARNESS_TOKENIZER`,
  `HARNESS_N_CTX`, `HARNESS_NGL`, `HARNESS_BIND`) and advertises `ws://{advertise
  host}:8777` as the handed-off url.
- Workers are unchanged except where a readiness signal needs confirming (see below).

### The http runtime (two-child harness)

Raw `llama-server.exe` does not speak the worker's ws/JSON-RPC chat surface, so an
`http` harness is **two children** the supervisor owns together:

1. `llama-server.exe -m <model> --host 127.0.0.1 --port <internal> -ngl .. -c ..` —
   holds the VRAM, bound to loopback on the hive (not client-facing).
2. `backend.exe --engine http --server-url http://127.0.0.1:<internal>` bound to
   `0.0.0.0:8777` — the proxy that speaks the ws chat RPC the renderer needs; this is
   the url handed off to the app.

Readiness is the ws `model_props` probe against the proxy (which itself waits on
llama-server). Eviction/`stop` **terminates both children and confirms both exit**;
VRAM is freed when llama-server exits. The harness `id` is `http:{model}`. This is the
only two-process runtime; candle and llamacpp are a single in-process child each.

### Catalog config (hive, declared)

A small TOML file (path via `HARNESS_CATALOG`, default beside the supervisor exe):

- `[[runtime]]` entries: `name` (`candle`/`llamacpp`/`http`), `exe` path, a launch
  recipe (which env vars to set), a `kind` that drives model compatibility, and an
  optional `requires_tokenizer` flag.
- `[models]`: one or more directories to scan; optional explicit `tokenizer` mapping or
  a convention (a sibling `*tokenizer*.json`).

Absolute paths live only in this server-side file and are never sent to the client.

### Model discovery

- Scan each configured dir for `*.gguf`. For each, read gguf metadata
  (`general.architecture`, parameter/size hints) and derive a **display name** from the
  file stem.
- Compatibility is a per-runtime predicate from the catalog `kind`:
  candle -> architecture is a candle-supported family (e.g. `gemma3`) **and** a
  tokenizer.json is resolvable; llamacpp/http -> any gguf. `list_models` reports each
  model with the set of runtimes that can load it; `start` re-validates the pair and
  rejects an incompatible `{runtime, model}` with a clear error.

### Lifecycle

States: `stopped -> loading -> ready`, and `ready|loading -> stopping -> stopped`, with
`failed` reachable from `loading` (spawn/load error) or `ready` (worker crashed).

- `start{runtime, model}`: validate pair; if a harness is live, **stop it first and
  wait for the child process to fully exit** (VRAM free) before spawning the new one;
  spawn the worker; **wait for readiness** by opening a ws to the worker and calling
  `model_props` successfully (bounded timeout, e.g. up to ~3 min for a 12B load); on
  success return `{id, url, props}`; on timeout/exit kill and return `failed`.
- A running harness `id` is `"{runtime}:{model-display-name}"` (e.g.
  `candle:gemma-3-4b`), surfaced by `list_harnesses` with its state + url.
- `stop{id}`: terminate the worker child, confirm exit, mark `stopped`.
- **Crash detection**: the supervisor holds each child handle; an unexpected exit marks
  the harness `failed` and emits a `harness_changed` notification so the UI can react.
- App lifetime is independent of harness lifetime: closing the app does **not** stop a
  running harness (it is a server-side resource); the user stops it explicitly. No
  harness is auto-started on app launch.
- **Fresh context on switch (decided):** each worker keeps conversation history
  server-side in its own `ChatSession`, so a newly started harness begins with empty
  context — the new model does not see prior turns. The renderer transcript stays
  visible across a switch, but the conversation does not carry into the new model.
  `send_turn` is unchanged; no transcript replay. (A different model can't meaningfully
  inherit another's context, so switching = clean slate.)

### Control RPC (new, over the same ws/JSON-RPC framing as rpc.rs)

- `list_runtimes` -> `[{ name, kind, available }]` (available = exe present).
- `list_models` -> `[{ name, runtimes: [..], size_hint }]` (no absolute paths).
- `list_harnesses` -> `[{ id, runtime, model, state, url? }]`.
- `start{ runtime, model }` -> `{ id, url, props }` | error.
- `stop{ id }` -> `{}`.
- notification `harness_changed` -> `{ id, state, url? }`.

### App / renderer changes (handoff)

- Main process resolves a **supervisor url** (new env `HARNESS_SUPERVISOR_URL`) and
  hands it to the renderer, alongside the existing single-backend path (kept as a
  fallback for direct-worker use).
- New `SupervisorClient` (renderer): lifecycle RPC over a ws, mirroring `HarnessClient`
  framing; exposes typed methods + a `harness_changed` signal.
- New `HarnessPickerViewModel` (extends `Observable`): lists runtimes + models, current
  harness + state; `Start(runtime, model)` / `Stop()`; on `ready`, produces the worker
  url.
- Chat rebind: on a new worker url, (re)connect a `HarnessClient` to it and point the
  `ChatViewModel` at it; disconnect/disable chat when no harness is `ready`. This
  requires `ChatViewModel`/`HarnessClient` to support rebinding or disposal (the
  previously-deferred HarnessClient dispose / TurnSignal disposables land here).
- UI placement: the picker sits in the instrument-panel area; chat input is disabled
  with a "no harness loaded" affordance until `ready`.

### Deploy (hive)

- Scheduled task `harness-supervisor` running the default-build exe in `--supervisor`
  mode with the catalog; firewall allow on the control port `8778` (worker `8777`
  already allowed). The existing per-engine tasks (`harness-candle`, `harness-backend`,
  `llama-server`) are superseded by supervisor-spawned workers, but left in place as
  manual fallbacks. Document the new flow in `docs/specs/hive-llama-server-setup.md`.

## What's left to the builder

- Exact Rust module layout for the supervisor (likely `supervisor/` with a catalog
  loader, a discovery scanner, a process-manager, and a control-RPC handler reusing the
  ws framing), and whether to factor the shared ws framing out of `rpc.rs`.
- Precise readiness probe (connect + `model_props`) timeout/backoff values and how to
  distinguish "still loading" from "crashed".
- Exact gguf metadata read path for discovery in the default build (a minimal header
  parser vs reusing candle's gguf reader) — pick the lighter one that avoids pulling an
  engine feature into the default build.
- Windows child-process semantics: spawning a child the supervisor tracks and can kill
  (and that dies if the supervisor dies — a job object), and confirming VRAM is freed
  before the next spawn.
- mural view markup for the picker and the chat-disabled state; how `ChatViewModel`
  rebinds cleanly (dispose old signals/socket).
- Test strategy: supervisor logic (catalog parse, discovery, compatibility, state
  machine, eviction ordering) unit-tested with a fake process-spawner/clock; renderer
  VMs unit-tested with fake sockets; live GPU proof on the hive (start candle ->
  chat -> stop -> start llamacpp -> chat, confirming eviction + VRAM).

## Accepted limitations (post-implementation, 2026-11-18)

- **Control plane serializes behind an in-flight load; a load is not
  cancellable.** `start` holds the single-harness lock for the whole readiness
  wait (up to ~180 s for a 12B), and the control connection handles one request
  at a time, so a Stop or harness switch issued mid-load is buffered and cannot
  pre-empt the load — the GPU finishes loading a model the user may have
  abandoned before the switch takes effect. This is a UX degradation, not a
  correctness bug: the one-live-harness invariant and eviction-before-spawn hold
  throughout, and the picker state self-heals (generation + ignored-id logic)
  once the stale reply lands. Deferred as a fast-follow: make the control loop
  handle requests concurrently (outbound-channel ws writer) and thread a
  cancellation token through the readiness wait so a newer intent pre-empts the
  load. Until then, prefer letting a load finish before switching.
