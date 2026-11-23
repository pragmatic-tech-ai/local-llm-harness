# Agent Templates, Sessions and the Dispatcher — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Agents become stored templates arranged in groups, and conversations become persisted sessions started from a template. A dispatcher, with a replaceable placement policy, decides when and where a session's model loads. The app gets an Agents capability (a tree and a template editor) and a Sessions capability (a list, a new-session dialog and a session view), replacing the Runtime picker.

**Architecture:** A new backend module, `backend/src/agents/`, holds the template store, the session store, the `ModelHost` trait with the local supervisor host, and the `Dispatcher` with `ExplicitLoadPolicy`. These run in the supervisor process. The runtime server on `:8779` becomes session-aware: it serves template, session and catalog methods, plus chat on the attached session. The existing `ToolLoop` keeps running every turn, now configured per session.

**Tech Stack:** Rust (tokio, tokio-tungstenite, serde, serde_json, toml, async-trait, all already vendored, so no new crates). Electron + mural TypeScript (`EditorShell` capabilities, `TreeView`, `DialogService`) and vitest.

**Spec:** `docs/specs/2026-11-23-agent-templates-sessions-design.md`. Read it first, including "Settled details". This plan argues from it.

## Global Constraints

- **No new crates.** The build must pass `cargo build --offline` from `backend/vendor/`, with `Cargo.lock` unchanged.
- **Rust builds and tests run on the hive only,** through `scripts/hive-test.sh [--features F] [-- <cargo test args>]`. Finish each task with `scripts/hive-test.sh --restore`. Never run `cargo` on the dev machine. `rustfmt --edition 2021` may run locally, on changed lines only, and files keep LF line endings. The app runs on the dev machine with `cd app && npm test && npm run typecheck:web`. Never read `C:\Users\visor\.npmrc`.
- **Rust style:** idiomatic snake_case and rustfmt. Helper logic goes in associated functions on types; add no new free-standing module functions, tests included. Reused strings live as named consts.
- **TypeScript style:** OOP only, with no module-level functions or mutable state. Allman braces, no semicolons, PascalCase public members and interfaces. Every literal is hoisted to a `private static readonly` PascalCase constant, except labels in `.mu` files, which stay inline. VMs extend `Observable`. A lower layer only signals upward.
- **Exact values:**
  - Data root: `[agents] data_dir` in `catalog.toml`, defaulting to `C:\llm-harness\agent-data\`, with `templates/` and `sessions/` under it.
  - The session file carries `schema: 1`.
  - Default title: `<template name> #<n>`.
  - Seed template: group `Examples`, template `Default`.
  - Errors: `host kind not supported yet: <kind>`, `session is busy`, `tool not enabled for this agent: <name>`, `no session attached`.
  - Placement variants: `ready | needs_load | unavailable`.
- **Commits:** author `Eugene Napryaglo <napryaglo@users.noreply.github.com>`, and every message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Never push; Task 11 asks for the owner's go first.

## Review Focus

1. **A hand-edited template that fails to parse or validate** must be listed as a broken entry, with its error, not crash the store and not hide the rest of the tree (Task 1: `broken_template_is_listed_with_error`).
2. **Two templates that share a model but differ in `n_ctx`.** The second must report `needs_load`, evicting the first (Task 4: `n_ctx_mismatch_needs_load`).
3. **A supervisor restart in the middle of a turn.** The reloaded session must not be busy, and its transcript must be the last committed one (Task 2: `busy_is_never_persisted`).
4. **Deleting a session while a connection is attached to it.** That connection gets `sessions_changed`, its next chat call fails `no session attached`, and other connections are unaffected (Task 6: `deleted_session_detaches_connections`).
5. **Deleting the template that live sessions came from.** Those sessions keep working from their snapshot (Task 2: `session_survives_template_delete`).

---

### Task 1: Agent templates and the template store

**Files:**
- Create: `backend/src/agents/mod.rs`, `backend/src/agents/template.rs`, `backend/src/agents/template_store.rs`. Add `pub mod agents;` to `backend/src/lib.rs`, keeping module order alphabetical.

**Interfaces:**
- Produces (`template.rs`):
  - `AgentTemplate { name: String, system_prompt: String, tools: Vec<String>, step_budget: usize, runtime: String, model: String, n_ctx: u32, enable_thinking: bool, sampling: Option<SamplingConfig>, host: Option<String> }`. It is TOML-serde, with defaults for `system_prompt` (`""`), `step_budget` (8), `enable_thinking` (false), and `sampling`/`host` (`None`).
  - `TemplateRules { runtimes: Vec<String>, models: Vec<(String, Vec<String>)>, tools: Vec<String> }`, where `models` pairs each model name with its runtimes.
  - `TemplateRules::validate(&self, t: &AgentTemplate) -> Result<(), String>`. Failure messages: `unknown tool: {name}`, `unknown runtime: {rt}`, `unknown model: {m}`, `runtime {rt} cannot load model {m}`, and `step_budget must be at least 1`.
- Produces (`template_store.rs`):
  - `TemplateNode::Group { name, path: String, children: Vec<TemplateNode> } | Template { id, path, name, runtime, model, error: Option<String> }`. It is Serialize, tagged `kind` with the values `group` and `template`. `path` is `/`-separated relative to `templates/`, and `""` is the root.
  - `TemplateStore::open(root: &Path, rules: TemplateRules) -> io::Result<Self>`. It creates `root` and seeds `Examples/Default.toml` when `root` is missing or empty.
  - `tree(&self) -> Vec<TemplateNode>`, sorted with groups first, then by name.
  - `get(&self, id) -> Result<(AgentTemplate, String /*path*/), TemplateError>`
  - `save(&self, id: Option<&str>, path: &str, t: &AgentTemplate) -> Result<String /*id*/, TemplateError>`, which validates first.
  - `delete(&self, id)`
  - `create_group(&self, path)`
  - `rename_group(&self, path, new_name)`
  - `delete_group(&self, path)`. Fails with `group is not empty` unless the group is empty.
  - `move_template(&self, id, path)`
  - `TemplateError(String)`
- **Ids:** an id is the file stem: a slug of `name` (lowercase ASCII alphanumerics and `-`, other runs collapse to `-`), with `-2`, `-3`, … appended on a collision anywhere in the tree. Ids are unique across the whole tree, so `id` alone locates a file.
- **Writes:** writes are atomic (write a temp file, then rename) and serialised behind a `Mutex`.
- **Path safety:** paths are validated. A `..` component, an absolute path or a drive prefix fails with `invalid path`.
- **Seed `Default`:** runtime `llamacpp`. The model is the first entry in `rules.models` whose runtimes include `llamacpp` and whose name contains `gemma-4`, otherwise the first one llamacpp lists. It uses `n_ctx` 8192, every tool in `rules.tools`, `step_budget` 8 and an empty prompt.

- [ ] **Step 1: Write the failing tests** (temp dir per test):

```rust
seeds_examples_default_when_empty         // tree == [Group{name:"Examples", children:[Template{id:"default", name:"Default"}]}]
round_trips_template_toml                 // save then get returns an equal AgentTemplate
directory_tree_maps_to_groups             // a/b/x.toml → Group a → Group b → Template x
rejects_unknown_tool_runtime_model        // the three exact messages above
broken_template_is_listed_with_error      // invalid TOML file → Template{error: Some(_)} with the rest of the tree intact; get() returns Err
slug_ids_are_unique                       // two "My Agent" saves → ids "my-agent" and "my-agent-2"
delete_group_requires_empty               // Err("group is not empty")
move_template_changes_path_keeps_id
rejects_escaping_paths                    // "../x", "C:/x" → Err("invalid path")
```

- [ ] **Step 2: Run** `scripts/hive-test.sh -- agents::` and expect it to FAIL (missing module).
- [ ] **Step 3: Implement** the three files.
- [ ] **Step 4: Run, expecting PASS.** Also run the whole lib.
- [ ] **Step 5: Commit:** `feat(agents): agent templates and the template store`.

### Task 2: Sessions and the session store

**Files:**
- Create: `backend/src/agents/session.rs`, `backend/src/agents/session_store.rs`
- Modify: `backend/src/engine/types.rs` (round-trip test only, if `Deserialize` is already derived)

**Interfaces:**
- Consumes: Task 1 `AgentTemplate`, `TemplateStore::get`.
- Produces (`session.rs`):
  - `SessionRecord { schema: u32 /*1*/, id: String, title: String, template_id: String, snapshot: AgentTemplate, created: u64, updated: u64, sampling: Option<SamplingConfig>, enable_thinking: Option<bool>, transcript: Vec<ChatMessage> }`. It is JSON-serde, and times are unix seconds.
  - `SessionSummary { id, title, template_id, template_name, runtime, model, updated }`.
- Produces (`session_store.rs`):
  - `SessionStore::open(root: &Path) -> io::Result<Self>`, which loads every `*.json`. A file that fails to parse is skipped and the error is printed to stderr.
  - `create(&self, template_id: &str, template: &AgentTemplate) -> SessionRecord`. The transcript is `[System(system_prompt)]` when the prompt is non-empty, otherwise `[]`. The title is `"{name} #{n}"`, where n is 1 + the count of existing sessions with that `template_id`. Ids are `s-{unix_millis}-{counter}`.
  - `list(&self) -> Vec<SessionSummary>`, newest `updated` first.
  - `get(&self, id) -> Option<SessionRecord>`
  - `rename(&self, id, title)`
  - `delete(&self, id)`
  - `commit(&self, id, transcript: Vec<ChatMessage>)`
  - `set_overrides(&self, id, sampling: Option<SamplingConfig>, enable_thinking: Option<bool>)`
  - `reset(&self, id)`, which goes back to the initial transcript from the snapshot.
  - Each of these persists atomically and returns `Result<_, SessionError>`, where `SessionError(String)` uses `unknown session: {id}`.
- **Busy state is not stored here.** It is runtime state (Task 6) and never touches the record.

- [ ] **Step 1: Write the failing tests:**

```rust
create_snapshots_template_and_titles      // titles "Researcher #1", "Researcher #2"; transcript[0] is System(prompt)
empty_prompt_starts_empty_transcript
snapshot_is_independent_of_template_edits // edit + save the template after create → record.snapshot is unchanged
session_survives_template_delete          // delete template → get(id) is still Some with the same snapshot
commit_persists_across_reopen             // commit, reopen the store → same transcript (ChatMessage round-trip, tool_calls included)
busy_is_never_persisted                   // the JSON has no busy field; reopen → nothing is busy
reset_restores_initial_transcript
overrides_round_trip
corrupt_session_file_is_skipped
```

- [ ] **Step 2: Run, expecting FAIL.** **Step 3: Implement.** **Step 4: Run, expecting PASS**, then run the whole lib.
- [ ] **Step 5: Commit:** `feat(agents): persisted sessions with template snapshots`.

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

### Task 5: Tool loop driven by a session configuration

**Files:**
- Modify:
  - `backend/src/runtime/tool_loop.rs`
  - `backend/src/runtime/registry.rs`
  - `backend/src/runtime/executor.rs`
  - `backend/src/runtime/server.rs`: the minimum to keep it compiling. Task 6 rewrites it.
  - `backend/src/runtime/test_support.rs`
- Delete: `backend/src/runtime/session.rs`. `RuntimeSession` is replaced.

**Interfaces:**
- Produces:
  - `TurnConfig { step_budget: usize, tools: Vec<String>, enable_thinking: bool, sampling: SamplingConfig }`
  - `#[async_trait] trait TurnLinks: Send + Sync { async fn link(&self, step: usize) -> Result<Arc<dyn WorkerLink>, String /*turn_error text*/>; }`. This replaces `WorkerConnector` in `run`. Step 0 is the first step, and an `Err` there ends `NoHarness`, as before. A mid-loop `Err` ends `Error` with that text.
  - `ToolLoop::new(executor, registry)`. The step cap moves to `TurnConfig`.
  - `async fn run(&self, transcript: &[ChatMessage], content: &str, config: &TurnConfig, links: &dyn TurnLinks, sink: &mut dyn LoopSink) -> Result<(LoopEnd, Option<Vec<ChatMessage>>), SinkClosed>`. It returns the new transcript only for `Answered`, after `loop_done` was delivered, and the caller commits it.
  - `ToolRegistry::openai_tools_for(&self, names: &[String]) -> Vec<Value>`, in registry order.
  - `ToolExecutor::execute(&self, call, enabled: &[String])`. A name outside `enabled` fails with `tool not enabled for this agent: {name}`, checked after "unknown tool".
- Everything else in the loop stays the same: call-id rewriting, preamble split, commit rule.

- [ ] **Step 1: Write the failing tests** (update the existing loop tests to the new signature):

```rust
step_budget_comes_from_config            // budget 2, every step calls → 2 requests, StepLimit
only_enabled_tools_are_sent              // enabled ["echo"] of a registry {echo, writer} → request.tools names == ["echo"]
call_outside_subset_is_refused           // model calls "writer" → tool_result content {"error":"tool not enabled for this agent: writer"}
system_prompt_renders_in_system_turn     // transcript [System("You are terse."), ...] → step-1 request rendered with the real GEMMA4 fixture contains the prompt in the system turn, before the tool declarations block end
run_returns_transcript_only_when_answered
```

- [ ] **Step 2: Run, expecting FAIL.** **Step 3: Implement.** **Step 4: Run, expecting PASS** (`-- runtime` and the whole lib).
- [ ] **Step 5: Commit:** `refactor(runtime): tool loop configured per session`.

### Task 6: Session-aware runtime server

**Files:**
- Modify: `backend/src/runtime/server.rs` (rewrite of the dispatch and connection state)
- Create: `backend/src/runtime/hub.rs`

**Interfaces:**
- Consumes: Tasks 1–5.
- Produces:
  - `RuntimeDeps { templates: Arc<TemplateStore>, sessions: Arc<SessionStore>, dispatcher: Arc<Dispatcher>, registry: Arc<ToolRegistry>, tool_loop: Arc<ToolLoop>, catalog: CatalogView, hub: Arc<Hub> }`
  - `CatalogView { runtimes: Vec<Value>, models: Vec<Value>, hosts: Vec<String> }`, which is the `list_catalog` result.
  - `Hub` keeps track of connections and holds the runtime-only `busy: HashSet<String>`. It provides:
    - `register() -> (ConnId, mpsc::UnboundedReceiver<Value /*frame*/>)`
    - `attach(conn, Option<session_id>)`
    - `broadcast(method, params)`, to all connections
    - `to_session(session_id, method, params)`, to the attached connections
    - `try_busy(session_id) -> bool` and `clear_busy(session_id)`
  - A connection's writer drains its channel, so notifications from other connections' actions reach it without blocking.
  - Methods, with names verbatim from the spec:
    - Templates: `list_templates`, `get_template {id}` → `{id, path, template}`, `save_template {id?, path, template}` → `{id}`, `delete_template`, `create_group {path}`, `rename_group {path, new_name}`, `delete_group {path}`, `move_template {id, path}`.
    - Catalog and tools: `list_catalog`, `list_all_tools`.
    - Sessions:
      - `list_sessions` returns each summary plus `placement` and `busy`.
      - `create_session {template_id}` returns a summary.
      - `attach_session {id}` returns `{summary, snapshot, transcript, sampling, enable_thinking, placement}`.
      - `rename_session {id, title}`
      - `delete_session {id}`
      - `load_session_model {id}`
      - `unload_session_model {id}`
    - Chat on the attached session:
      - `send_turn {content}`: a busy session fails `session is busy`. The effective thinking is the override, else the snapshot. The sampling is the override, else the snapshot, else the default. It sends `session_loading {id}` when a load happens, runs the loop, commits on Answered, and the result `null` comes after `loop_done`.
      - `set_sampling {config}` and `set_thinking {enable}` persist the overrides.
      - `model_props` comes from the current link, else fails `no model running`.
      - `tokenize`
      - `list_tools` returns the specs of the session's tools.
      - `reset`
      - With nothing attached, every chat method fails `no session attached`.
  - Notifications:
    - To all connections: `sessions_changed {}`, `templates_changed {}`, and `placement_changed {id, placement}` for each session whose placement changes. A task listens to `dispatcher.subscribe()` and diffs the placements.
    - To attached connections only: turn notes and `session_loading`.
  - `SessionLinks` (in `server.rs`) implements Task 5's `TurnLinks` for one turn: step 0 → `dispatcher.link_for_turn(req, || send session_loading)`; later steps → `dispatcher.current_link(req)` or `Err("no model running")`, so an eviction mid-turn ends `Error`.
  - `delete_session` detaches every connection attached to that session, then broadcasts `sessions_changed`.

- [ ] **Step 1: Write the failing tests** (unit tests via `dispatch` on a `RuntimeConnection` with in-memory stores in temp dirs, `FakeHost` and `FakeLink`, plus one real-socket test):

```rust
chat_without_attach_fails                     // send_turn → Err "no session attached"
create_attach_send_commits                    // transcript persisted after loop_done; result after loop_done
busy_session_rejects_second_turn              // try_busy held → Err "session is busy"
send_turn_on_needs_load_notifies_loading      // notes contain session_loading before turn_started
two_connections_attached_both_receive_notes   // real socket test with two clients
list_level_changes_broadcast_to_all           // create_session on conn A → conn B receives sessions_changed
deleted_session_detaches_connections          // B attached, A deletes → B gets sessions_changed; B send_turn → Err "no session attached"
placement_changed_on_eviction                 // host signal flips session X ready → needs_load → broadcast
list_all_tools_and_catalog
save_template_validation_error_is_rpc_error   // message "unknown tool: nope"
```

- [ ] **Step 2: Run, expecting FAIL.** **Step 3: Implement.** **Step 4: Run, expecting PASS** (whole lib and `-- --tests`).
- [ ] **Step 5: Commit:** `feat(runtime): session-aware runtime server with template and session methods`.

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

### Task 8: App runtime client for sessions and templates

**Files:**
- Modify: `app/src/renderer/src/HarnessClient.ts` (rename the class to `RuntimeClient` in `RuntimeClient.ts`; keep the chat API), `HarnessClient.test.ts` → `RuntimeClient.test.ts`, and all imports
- Create: `app/src/renderer/src/agents.ts` (types)

**Interfaces:**
- Produces:
  - Types:
    - `TemplateNode` (group or template, mirroring Task 1's JSON)
    - `AgentTemplate` (snake_case fields as on the wire)
    - `SessionSummary` (+ `placement: Placement`, `busy: boolean`)
    - `Placement { state: 'ready' | 'needs_load' | 'unavailable', host?: string, evicts?: string | null, reason?: string }`
    - `AttachedSession`
    - `CatalogInfo`
  - `RuntimeClient` methods, one per Task 6 method, in PascalCase: `ListTemplates()`, `GetTemplate(id)`, `SaveTemplate(id, path, template)`, `DeleteTemplate(id)`, `CreateGroup(path)`, `RenameGroup(path, newName)`, `DeleteGroup(path)`, `MoveTemplate(id, path)`, `ListCatalog()`, `ListAllTools()`, `ListSessions()`, `CreateSession(templateId)`, `AttachSession(id)`, `RenameSession(id, title)`, `DeleteSession(id)`, `LoadSessionModel(id)`, `UnloadSessionModel(id)`, `SetThinking(enable)`.
  - Existing methods stay: `SendTurn`, `SetSampling`, `ModelProps`, `Tokenize`, `ListTools`, `Reset`.
  - Signals: `OnSessionsChanged`, `OnTemplatesChanged`, `OnPlacementChanged: TypedSignal<{id, placement}>`, `OnSessionLoading: TypedSignal<{id}>`.

- [ ] **Step 1: Write the failing tests:** `each_method_sends_its_wire_name_and_params` (table-driven over all the methods above), `list_level_notifications_raise_signals`.
- [ ] **Step 2–4: Run** `cd app && npx vitest run src/renderer/src/RuntimeClient.test.ts`: FAIL, implement, then PASS. Then run `npm test && npm run typecheck:web`.
- [ ] **Step 5: Commit:** `feat(app): runtime client for sessions and templates`.

### Task 9: Sessions capability (replaces Runtime)

**Files:**
- Rename/modify:
  - `views/RuntimeModule.mu` → `views/SessionsModule.mu`, with module and capability `Name = "Sessions"`, the same icon, and the `supervisor.url` setting kept.
  - `RuntimeManagementService.ts` → `SessionsService.ts`, with its test.
  - `app.mu`
- Create: `SessionsPanelViewModel.ts`, `SessionViewModel.ts`, `NewSessionDialogViewModel.ts`, `views/SessionsPanel.mu`, `views/SessionView.mu`, `views/NewSessionDialog.mu`, and their tests
- Delete:
  - `HarnessPickerViewModel.ts` and its test
  - `views/HarnessPanel.mu`
  - The picker-only parts of `SupervisorClient.ts`. Keep the code that discovers `chat_url`: `SessionsService` lists harnesses, or reads the supervisor's runtime endpoint, so it can find the runtime URL. Ruling: the runtime URL is the supervisor URL with the port replaced by the `chat_url` port from `list_harnesses`, or falls back to `:8779`. Add a `runtime_url` field to the supervisor's `list_runtimes`/`hello` if that is cleaner, and say which in the report.

**Interfaces:**
- Produces:
  - `SessionsService` owns one `RuntimeClient`. It reconnects with backoff (1 s, 2 s, 4 s, capped at 10 s) and re-attaches the session that was shown.
  - `SessionsPanelViewModel`:
    - `Sessions: ObservableCollection<SessionRowViewModel>`, where each row has `Title`, `TemplateName`, `StateText`, `StateKind` and `Busy`.
    - `SelectedSession`, which on set raises `OnOpen: TypedSignal<string>`.
    - `NewSessionCommand`, `RenameCommand`, `DeleteCommand`.
    - It refreshes on `sessions_changed` and updates the row state on `placement_changed`.
  - `StateKind`:
    - `ready` → live
    - `needs_load` → idle
    - `unavailable` → unavailable
    - busy → busy
  - `SessionViewModel` wraps the existing `ChatViewModel` and `InstrumentPanelViewModel`, bound to the attached session. It exposes `HeaderText` (template · model @ host), `PlacementText`, `CanLoad`, `LoadLabel` (`Load model` or `Load model (evicts {evicts})`), `LoadCommand`, `UnloadCommand`, and the tools indicator. It shows the attached transcript on attach: history becomes `UserMessage`, `AssistantMessage`, `ToolCallMessage` and `ToolResultMessage` items, with system messages hidden.
  - `NewSessionDialogViewModel` holds the template tree (`TreeView`), a `Summary` of the selected template (model, tools, budget), and `CreateCommand`. It is shown via `DialogService.Show`, creates the session, then opens it.
  - The chat side: `send_turn` on `needs_load` gets a `session_loading` note, which shows `[loading model…]` in the chat.

- [ ] **Step 1: Write the failing tests:**

```ts
'sessions_panel_lists_sessions_with_state'        // placements ready/needs_load/unavailable map to live/idle/unavailable
'placement_changed_updates_row'
'selecting_row_attaches_and_shows_history'        // AttachSession sent; transcript rendered; system message hidden
'load_button_label_names_eviction'                // 'Load model (evicts gemma-4-12B-it-QAT-Q4_0)'
'new_session_dialog_creates_and_opens'
'reconnect_reattaches_shown_session'
'deleted_shown_session_clears_view'
```

- [ ] **Step 2–4: Run FAIL, implement, run PASS** (`npm test && npm run typecheck:web`).
- [ ] **Step 5: Commit:** `feat(app): Sessions capability with session list, view and new-session dialog`.

### Task 10: Agents capability (template tree and editor)

**Files:**
- Create: `views/AgentsModule.mu` (capability `Name = "Agents"`, with a new icon `views/icons/Agents.svg` in the `RuntimePlay.svg` style), `AgentsService.ts`, `AgentsTreeViewModel.ts`, `TemplateEditorViewModel.ts`, `views/AgentsPanel.mu`, `views/TemplateEditor.mu`, and their tests. Register the module in `app.mu`.

**Interfaces:**
- Consumes: Task 8 `RuntimeClient`. Use the same client instance as `SessionsService`, through a shared service; do not open a second socket.
- Produces:
  - `AgentsTreeViewModel` has `Roots` (`TemplateTreeNodeViewModel`, recursive: `Name`, `IsGroup`, `Children`, `Error`), `Selected`, and the commands `NewTemplate`, `NewGroup`, `Rename`, `Delete`, `MoveTo` and `Refresh`. It refreshes on `templates_changed`.
  - Selecting a template opens the editor in the content host.
  - `TemplateEditorViewModel` edits:
    - `Name`, `SystemPrompt`, `StepBudgetText`, `RuntimeChoices`/`Runtime`, `ModelChoices`/`Model` (filtered by runtime), `ContextSizeText`, `EnableThinking`, `TemperatureText`/`TopPText` (blank = none), `HostChoices`/`Host`
    - `Tools: ObservableCollection<ToolChoice {Name, Description, Enabled}>`
  - It has `SaveCommand` and `RevertCommand`, `IsDirty`, and an `ErrorText` set from the `save_template` error message. A broken template opens with `ErrorText` = its load error and the raw fields it could recover. Otherwise the fields start from the defaults.
  - New template: `Untitled` in the selected group. New group: `New group`.

- [ ] **Step 1: Write the failing tests:**

```ts
'tree_renders_groups_and_templates'
'selecting_template_opens_editor'
'editor_save_sends_fields_and_clears_dirty'
'editor_shows_validation_error'            // SaveTemplate rejects "unknown tool: x" → ErrorText
'model_choices_filter_by_runtime'
'delete_group_error_surfaces'              // "group is not empty"
'broken_template_opens_with_error'
'templates_changed_refreshes_tree'
```

- [ ] **Step 2–4: Run FAIL, implement, run PASS** (`npm test && npm run typecheck:web`).
- [ ] **Step 5: Commit:** `feat(app): Agents capability with template tree and editor`.

### Task 11: Deploy and live proof (owner go required)

- [ ] **Step 1:** Run `scripts/hive-test.sh --restore`, then stop and ask the owner for the go to push.
- [ ] **Step 2:** After the go:
  1. Push main.
  2. On the hive: `git pull --ff-only`, stop the supervisor and its workers, run `build-both.cmd` (expect `CANDLE_EXIT=0 LLAMA_EXIT=0 SUP_EXIT=0`), then `schtasks /Run /TN harness-supervisor`.
  3. Check that `supervisor.out` shows `runtime listening on ws://0.0.0.0:8779 (3 tools, 1 templates, 0 sessions)` and that `C:\llm-harness\agent-data\templates\Examples\Default.toml` exists.
- [ ] **Step 3: Live proof** (launch the app with `HARNESS_SUPERVISOR_URL=ws://192.168.1.101:8778`):
  - Create a `Research` group and a `Researcher` template on gemma-4 with only `read_file` and the system prompt `Answer in one sentence.`.
  - A session from it answers "What does notes.txt say?" with a `read_file` call and one sentence.
  - Create a template on candle + gemma-3 and a session from it. It shows `Load model (evicts gemma-4-12B-it-QAT-Q4_0)`. Load it: the first session's dot goes idle.
  - Restart the app. Both sessions are listed, and the first one's history is readable without loading anything.
- [ ] **Step 4:** Record the result in the spec status and the memory note.
