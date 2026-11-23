# Task 6 report: session-aware runtime server

Status: DONE_WITH_CONCERNS
Commit: `9d041eba feat(runtime): session-aware runtime server with template and session methods` (base b7916efc)

## What was built

- `backend/src/runtime/hub.rs` (new): `Hub`, `ConnId`, `BusyGuard`.
  - `register() -> (ConnId, mpsc::UnboundedReceiver<Value>)`, `unregister(conn)`,
    `attach(conn, Option<String>)`, `attached(conn) -> Option<String>`,
    `detach_session(id)`, `send(conn, frame) -> bool` (a response, or any frame for one connection),
    `broadcast(method, params)`, `to_session(id, method, params) -> usize` (how many connections it
    reached; connections whose channel closed are dropped from the hub),
    `try_busy(id) -> bool`, `clear_busy(id)`, `is_busy(id)`,
    `hold_busy(self: &Arc<Self>, id) -> Option<BusyGuard>` (the guard clears busy on drop, panics included).
- `backend/src/runtime/server.rs` (rewritten; Task 5's `ConnectorLinks` scaffolding and the per-connection
  transcript/sampling are deleted):
  - `CatalogView { runtimes: Vec<Value>, models: Vec<Value>, hosts: Vec<String> }` (Serialize, Clone, Default).
  - `RuntimeDeps { templates, sessions, dispatcher, registry, tool_loop, catalog, hub }` (Clone), exactly as the brief.
  - `RuntimeServer::serve(listener, deps)`, plus the wire-name consts `RuntimeServer::{SESSIONS_CHANGED, TEMPLATES_CHANGED, PLACEMENT_CHANGED, SESSION_LOADING}`.
  - `RuntimeConnection::new(deps) -> (Self, UnboundedReceiver<Value>)` (it registers with the hub, and `Drop` unregisters),
    `run(self, stream, frames)` (reader loop plus a spawned writer task that drains the channel), and
    `pub(crate) dispatch(&mut self, method, params) -> Result<Value, RpcError>` (the unit-test seam).
  - `SessionLinks` implements `TurnLinks`. Step 0 calls `dispatcher.link_for_turn(req, || hub.to_session(id, "session_loading", {id}))`.
    Later steps call `dispatcher.current_link(req)`, or return `Err("no model running")`.
  - `SessionSink` implements `LoopSink` through `hub.to_session`. Reaching 0 connections means `SinkClosed`.
  - `PlacementWatcher::spawn(&deps) -> JoinHandle<()>`. It subscribes and snapshots the placements synchronously
    before returning, then on every dispatcher signal (or Lagged) recomputes `status` for every session in
    `SessionStore::list()` order and broadcasts `placement_changed` for each changed or newly seen session.
    It holds the dispatcher weakly, so it stops when the dispatcher drops.
- `backend/src/rpc.rs`: removed `Failure::Closed`, which is now unused because the runtime was its only producer.
  `FrameLoop::handle_frame` now returns `Result<(), BoxError>`. Stale "shared with the agent runtime" docs were fixed.
  `TestClient::notes_until(method)` was added (test-only; it reads frames until that notification, with a 5 s timeout).
- `backend/src/runtime/test_support.rs`: `TempDir` and `TestRuntime`, both `pub(crate)` and `#[cfg(test)]`, for Task 7 to reuse (see below).
- `backend/src/main.rs`: **interim wiring** so the binary keeps building. Changing `RuntimeDeps` broke it, and `-- --tests` compiles the bin.
  It builds the hosts from `HostConfigs::build` (each one a `LocalSupervisorHost` over the manager), the rules from the
  catalog, `discover` and the registry, a simple `CatalogView` (`runtimes: [{name}]`, `models: [{name, runtimes}]`, host
  names), and the stores at `agents.data_dir()/{templates,sessions}`. On a store IO error it prints
  `agents data unavailable: {e}` and skips the runtime. Then come the dispatcher (`ExplicitLoadPolicy`), the Hub, the watcher and serve.
  Task 7 still owns the extracted builder, its test, the final startup line, the final CatalogView shape and the removal of
  `SupervisorConnector`/`WorkerConnector`, which main.rs no longer uses.

## Final wire protocol (runtime :8779), as implemented

Errors are JSON-RPC errors. Bad params give code -32602, an unknown method gives -32601 (`unknown method: X`), and everything else gives -32603 with the message shown.
"Row" below means a **SessionRow**: every `SessionSummary` field flattened
(`id, title, template_id, template_name, runtime, model, updated`), plus `placement` and `busy: bool`.
`Placement` is `{"state":"ready","host"}`, `{"state":"needs_load","host","evicts":string|null}` or
`{"state":"unavailable","reason"}`.

### Templates (no attach needed). Every successful mutation broadcasts `templates_changed {}`.

| method | params | result |
|---|---|---|
| `list_templates` | none | `TemplateNode[]` (`{kind:"group",name,path,children}` or `{kind:"template",id,path,name,runtime,model,error:string\|null}`) |
| `get_template` | `{id}` | `{id, path, template: AgentTemplate}` |
| `save_template` | `{id?: string, path?: string (default ""), template: AgentTemplate}` | `{id}`. A validation error is an RPC error with the validator text, e.g. `unknown tool: nope` |
| `delete_template` | `{id}` | `null` (sessions are unaffected; they run from their snapshot) |
| `create_group` | `{path}` | `null` |
| `rename_group` | `{path, new_name}` | `null` |
| `delete_group` | `{path}` | `null` (`group is not empty` otherwise) |
| `move_template` | `{id, path}` | `null` |

`AgentTemplate` wire fields: `name, system_prompt, tools[], step_budget, runtime, model, n_ctx, enable_thinking, sampling?, host?`.

### Catalog and tools

| method | params | result |
|---|---|---|
| `list_catalog` | none | `CatalogView {runtimes: object[], models: object[], hosts: string[]}` |
| `list_all_tools` | none | `ToolSpec[]` (`{name, description, parameters, side_effects}`) for the whole registry |

### Sessions (no attach needed)

| method | params | result / effect |
|---|---|---|
| `list_sessions` | none | `Row[]`, newest `updated` first |
| `create_session` | `{template_id}` | `Row` of the new session; broadcasts `sessions_changed {}` |
| `attach_session` | `{id}` | `{summary: Row, snapshot: AgentTemplate, transcript: ChatMessage[], sampling: SamplingConfig, enable_thinking: bool, placement}`. `sampling`/`enable_thinking` are the **effective** values (override, else snapshot, else default). Attaches this connection (replacing any earlier attachment). Unknown id: `unknown session: {id}` |
| `rename_session` | `{id, title}` | `null`; broadcasts `sessions_changed {}` |
| `delete_session` | `{id}` | `null`; detaches every connection attached to it, then broadcasts `sessions_changed {}` |
| `load_session_model` | `{id}` | the `Placement` after the load (normally `ready`) |
| `unload_session_model` | `{id}` | `null` (does nothing if the host holds another model) |

### Chat, on the attached session. Every method fails `no session attached` without one (checked before params).

| method | params | result |
|---|---|---|
| `send_turn` | `{content}` | `null`, sent after `loop_done`. Fails `session is busy` if a turn is running. Sends `session_loading {id}` before `turn_started` when the model has to load. Commits the transcript only on Answered |
| `set_sampling` | `{config: SamplingConfig}` | `null`; persists the override (the thinking override is kept) |
| `set_thinking` | `{enable: bool}` | `null`; persists the override (the sampling override is kept) |
| `model_props` | none | worker `model_props` from the current link; else `no model running` |
| `tokenize` | `{text}` | worker tokenize result from the current link; else `no model running` |
| `list_tools` | none | `ToolSpec[]` of the session's snapshot tools, in registry order |
| `reset` | none | `null`; transcript back to `[system]` or `[]`, persisted; fails `session is busy` during a turn |

### Notifications

- To every connection: `sessions_changed {}`, `templates_changed {}`, `placement_changed {id, placement}`.
- To the connections attached to the session only: `session_loading {id}`, `turn_started`, `turn_chunk`,
  `turn_done`, `turn_rejected`, `turn_error`, `tool_call`, `tool_result`, `loop_done` (payloads unchanged from Task 5).

## Rulings applied / documented (module docs of server.rs and hub.rs)

- There is one outbound channel per connection. Responses go through it too, so a result is always queued after the notes that preceded it.
- A turn keeps running while at least one connection attached to its session remains. With none left (including after
  `delete_session` of a busy session), the next note fails `SinkClosed`, the turn aborts, nothing is committed, and the
  response is `client connection closed`.
- A gone client is noticed only when a write to it fails, as before.
- `busy` lives only in the Hub, held through `BusyGuard` (Drop), so it is cleared on every exit path, panics included.
- `send_turn` takes `busy` first and then reads the record, so a concurrent reset or turn cannot change it underneath. `reset` refuses while busy.

## Tests (26 in server.rs, 5 in hub.rs)

All of the brief's tests:
- `chat_without_attach_fails` (covers all 7 chat methods)
- `create_attach_send_commits`
- `busy_session_rejects_second_turn` (also checks reset, and that list_sessions shows `busy: true`)
- `send_turn_on_needs_load_notifies_loading`
- `two_connections_attached_both_receive_notes` (real socket)
- `list_level_changes_broadcast_to_all`
- `deleted_session_detaches_connections`
- `placement_changed_on_eviction`
- `list_all_tools_and_catalog`
- `save_template_validation_error_is_rpc_error`

Plus:
- `send_turn_result_comes_after_loop_done_on_the_wire` (real socket, plus a parse error)
- `busy_clears_after_a_turn`
- `send_turn_on_loaded_model_does_not_notify_loading`
- `turn_finishes_for_others_when_its_connection_is_gone`
- `turn_aborts_when_no_attached_connection_remains`
- `placement_watcher_reports_only_changes`
- `list_tools_is_the_sessions_tools`
- `template_methods_round_trip`
- `deleting_a_template_leaves_its_sessions_running`
- `overrides_persist_and_drive_the_turn`
- `turn_uses_snapshot_thinking_and_sampling_without_overrides`
- `reset_restores_the_initial_transcript`
- `model_props_and_tokenize_need_the_sessions_model`
- `load_and_unload_session_model`
- `later_steps_without_a_model_end_with_no_model_running` (SessionLinks)
- `unknown_method_and_bad_params_use_rpc_codes`

Hub tests:
- `broadcast_reaches_every_connection`
- `to_session_reaches_only_attached_connections`
- `closed_receivers_are_not_counted_and_are_dropped`
- `detach_session_detaches_all_its_connections`
- `busy_guard_clears_on_drop_and_on_panic`

### TDD evidence

- RED: `scripts/hive-test.sh -- runtime::server`, with the new tests over the old implementation, failed to compile with 31 errors:
  `cannot find type PlacementWatcher`, `struct RuntimeDeps has no field named templates/sessions/hub/...`,
  `cannot find struct SessionLinks`, `CatalogView not found in super::server`.
- GREEN: `scripts/hive-test.sh` gave `test result: ok. 332 passed; 0 failed`, TEST_EXIT=0, with no warnings after removing `Failure::Closed`.
- GREEN `-- --tests`: lib 332, bin 4, http_engine 10, http_stream 6, http_warmup 2 and rpc 5 passed; TEST_EXIT=0.
- `scripts/hive-test.sh --restore` printed RESTORED.

## For Task 7 (reuse)

- `crate::runtime::test_support::TestRuntime::new(tag, link: Arc<FakeLink>) -> TestRuntime { deps: RuntimeDeps, host: Arc<FakeHost>, dir: TempDir }`.
  It has temp-dir stores (`dir.0/templates`, `dir.0/sessions`), one `FakeHost` named `hive` serving `llamacpp`/`tiny`
  with `link`, an echo-only registry, the `ExplicitLoadPolicy` dispatcher, a fresh Hub and `TestRuntime::catalog()`.
  Consts: `HOST`, `RUNTIME`, `MODEL`, `TEMPLATE_ID` (`default`, the seeded template).
  Helpers: `rules(&registry)`, `template()`, and `load()` (marks the seeded model loaded and fires the signal).
- `crate::runtime::test_support::TempDir::new(tag)` (removed on drop; the path is `.0`).
- `PlacementWatcher::spawn(&deps)` must be called inside the tokio runtime, after the deps are built.
- The interim block in `main.rs::run_supervisor` is the thing to replace with the extracted builder.

## Self-review / concerns

1. **main.rs interim wiring overlaps Task 7.** It was necessary to keep the bin compiling. Task 7 should
   replace it (builder fn plus test, the final startup line with template/session counts, the real CatalogView shape, and failing
   non-zero on a `HostConfigs` error, which `?` already does here) and drop `SupervisorConnector`/`WorkerConnector` (now unused).
2. **Load failures at step 0 are silent.** `link_for_turn` errors (e.g. `no host serves llamacpp/x` or a failed load) end
   the loop `NoHarness` (Task 5's contract: step-0 link errors produce `loop_done {reason: no_harness}` with no
   `turn_error`), so the client never sees the dispatcher's message. The app can show the placement's `reason`
   for Unavailable. A failed *load* has no text on the wire, though. That could be a follow-up: a step-0 `turn_error` or a
   `session_load_failed` note. It needs a spec call.
3. `create_session` and `attach_session.summary` return the full Row (summary plus placement and busy), a superset of "a summary",
   so the app's `SessionSummary` type (Task 8: summary + placement + busy) fits every place.
4. `attach_session` returns the **effective** `sampling`/`enable_thinking`, not the raw overrides (the panel shows the session's values).
5. `busy` changes do not broadcast anything. A client sees them only through `list_sessions` (the spec has no busy notification).
6. A turn commit bumps `updated` (list order) without a `sessions_changed`, per "list-level add/rename/delete".

---

# Fix round 1 (review: Needs fixes), commit `bd196204 fix(runtime): task 6 review follow-ups`

## Findings fixed

1. **Reset race.** `reset` now takes `hub.hold_busy(id)` (busy error otherwise) and reads the record after taking it, so a reset and a turn exclude each other. Test: `reset_and_turn_exclude_each_other`. A held turn makes reset fail `session is busy`; the reset itself broadcasts `session_busy` true and then false.
2. **Envelope duplication.** Added `Frames::decode(text) -> Result<Request, Value /*parse-error frame, null id*/>` and `Frames::reply(id, Result<Value, RpcError>) -> Value` in `rpc.rs`. Both `rpc::FrameLoop::handle_frame` (worker) and `RuntimeConnection::respond` use them. The worker's behaviour is unchanged: all of the worker's rpc unit tests and `tests/rpc.rs` pass.
3. **Watcher cost.** Added `SessionStore::list_with_requirements() -> Vec<(SessionSummary, ModelRequirement)>`, in `list` order (the shared `list_order`), built under the lock without cloning transcripts. Building it inside the store does not break layering, because `ModelRequirement` and the store are both in `agents`. `ModelRequirement::of(&AgentTemplate)` (in host.rs) is the one snapshot-to-requirement rule, and `Dispatcher::requirement` delegates to it. The watcher and `list_sessions` both use it. Test: `list_with_requirements_matches_list_order_and_snapshots`.
4. **New sessions are recorded silently.** `publish` broadcasts only when the session was already in `last` and its placement differs. `PlacementWatcher::new(&deps)` (private) and `publish()` are now driven synchronously in tests: `placement_watcher_records_new_sessions_silently` and `placement_watcher_reports_only_changes` (now deterministic). `placement_changed_on_eviction` still goes through the real signal path via `spawn`.
5. **One home for "unknown session: ".** `SessionError::unknown` is now `pub`, and the server uses `SessionError::unknown(id)`. Server-side `ERR_UNKNOWN_SESSION` is deleted.
6. **Test literals.** The server tests use `C::M_*`, `C::ERR_*`, `RuntimeServer::*`, `Hub::SESSION_BUSY` and `ToolLoop::NO_MODEL`, plus local consts (`GROUP`, `CONTENT`, `ANSWER`, `TURN_*`, `LOOP_DONE`, `METHOD`, `PARAMS`, `RESULT`). The hub tests use local consts. Helpers live on the test `Client` (`send`, `create`, `attach`, `hang_up`, `position`, `serve`, `links`); there are no free functions.
7. **unload_session_model on a busy session.** This is documented in the module doc and at the match arm: it is allowed, and the turn's next step finds no model and ends `Error` with `no model running`. Test: `unload_during_a_turn_ends_its_next_step`. A held turn takes link(0); another client unloads (OK, `unload_calls == 1`); then link(1) returns `no model running`.

## Rulings implemented

- **Step-0 link failure.** `ToolLoop` now sends `turn_error {message: <err>}` and then `loop_done {steps: 0, reason: "no_harness"}` (new `ToolLoop::no_harness`; docs on `TurnLinks::link` and `run` updated). Task 5's `no_harness` test now expects `["turn_error", "loop_done"]` with message `no model running`. New server test: `failed_load_reports_turn_error_then_no_harness`. With `fail_ensure("boom")`, the notes are `session_loading`, then `turn_error {message:"boom"}`, then `loop_done {steps:0, reason:"no_harness"}`, and nothing is committed.
- **Busy and commit notifications.** `Hub::hold_busy` broadcasts `session_busy {id, busy: true}` to all connections. `BusyGuard::drop` clears the flag and broadcasts `{id, busy: false}`. The raw `try_busy`/`clear_busy` do not broadcast. A successful commit broadcasts `sessions_changed {}`. Tests:
  - `busy_and_commit_are_broadcast` (an unattached connection sees `session_busy` true, then `sessions_changed`, then `session_busy` false)
  - `create_attach_send_commits` (after `loop_done`: `sessions_changed`, then `session_busy`)
  - hub `busy_guard_broadcasts_taking_and_releasing`
- main.rs is untouched (Task 7 replaces it).

## Updated protocol for Task 8 (only the differences from the tables above)

- **Notifications to every connection** are now `sessions_changed {}`, `templates_changed {}`, `placement_changed {id, placement}` and **`session_busy {id, busy: bool}`**.
- **`sessions_changed {}`** is now also sent after every committed turn, besides create, rename and delete.
- **`placement_changed`** is sent only for sessions the watcher has already seen. A session created since the last signal is recorded silently; its placement comes from the `create_session` row or `list_sessions`.
- **`reset`** fails `session is busy` while a turn runs. While it runs it holds busy itself, so it also emits `session_busy` true and then false.
- **`send_turn`**, in order:
  - `session_busy {id, busy:true}` (to all)
  - then, to the attached connections: `session_loading {id}` (only if a load is needed), then `turn_*`, `tool_call` and `tool_result`, then `loop_done`
  - on Answered, `sessions_changed {}` (to all)
  - `session_busy {id, busy:false}` (to all)
  - finally the `null` result
  - If the step-0 link or load fails: `turn_error {message}`, then `loop_done {steps:0, reason:"no_harness"}`, then the `null` result.
- **`unload_session_model {id}`** is allowed during a turn; that turn then ends with `turn_error {message:"no model running"}` and `loop_done {reason:"error"}`.

## Evidence

- `scripts/hive-test.sh -- --tests` gave lib `338 passed; 0 failed`, bin 4, http_engine 10, http_stream 6, http_warmup 2, rpc 5. TEST_EXIT=0, with no warnings.
- `rustfmt --edition 2021` was run on the touched files; the diff shows only changed lines. The LF check found no CR in any changed file. `scripts/hive-test.sh --restore` printed RESTORED.
- There was no separate RED run in this round: the new and changed tests were written together with the fixes, and the whole suite run above is the proof.

---

# Fix round 2 (re-review), commit `fb0b9443 fix(runtime): seeded placements, atomic busy release`

1. **Placement seeding.** The last announced placement per session now lives in the Hub (`placements: Mutex<HashMap<String, Placement>>`, a separate mutex from the connection state).
   - `Hub::seed_placement(id, placement)` uses `or_insert`, so an update that already ran wins. `create_session` seeds it with the returned row's placement.
   - `Hub::update_placements(compute) -> Vec<(id, Placement)>` holds the lock while it runs `compute()` (list plus statuses), replaces the map, and returns every session whose placement differs from the recorded one; nothing recorded counts as a difference. So a concurrent seed is never wiped.
   - `PlacementWatcher` no longer has `last`. `new` records silently; `publish` broadcasts the returned changes. The silent-first-sight rule is gone.
   - Tests:
     - `eviction_right_after_create_is_announced`: loaded, create (row ready), evict before any signal, then `publish` gives `placement_changed` needs_load.
     - `placement_watcher_compares_new_sessions_with_their_seed` (replaces `..._records_new_sessions_silently`): the seed matches, so an unchanged signal announces nothing; a load then announces ready.
     - `session_unknown_to_the_hub_is_announced`: a session created behind the server's back is announced at the first signal.
     - hub `seeded_placement_survives_and_only_changes_are_returned`.
2. **Atomic busy.** `hold_busy` inserts the flag and broadcasts `busy:true` under one state lock. The new private `Hub::release_busy`, used by `BusyGuard::drop`, removes the flag and broadcasts `busy:false` under one lock, through `broadcast_in(&mut HubState, ..)`. Every connection therefore sees `session_busy` frames in the order the flag actually changed. There is no dedicated test for this. The race needs another thread to take the flag between the old clear and the old broadcast, and the lock now makes that window unobservable. A test could only show the sequential order, which the existing `busy_guard_broadcasts_taking_and_releasing` and `busy_and_commit_are_broadcast` already show.
3. **`"boom"`.** Hoisted to the local test const `LOAD_ERROR`.
4. **Deleted session under an attachment.** `reset` and `send_turn` call the new `attached_session()`, which checks `SessionStore::contains(id)` (also new, and cheap: no clone) before taking busy. Both still re-read the record after taking busy. Test: `chat_on_a_session_deleted_underneath_broadcasts_no_busy`. The session is deleted in the store only; `reset` and `send_turn` fail `no session attached`, the other connection receives nothing, and busy is not set.
5. **Extra fix.** `supervisor::manager` tests `in_process_n_ctx_omitted_when_unset_present_when_set` and the `manager("nctx", ..)` test shared the temp dir `mgr-nctx-{pid}` (`model_dir` wipes it first), so they raced. One hive run failed `gem.gguf resolves`. The second test's tag is now `nctxreq`. This was pre-existing and is a one-word fix.

**Protocol changes for Task 8.** `placement_changed {id, placement}` is now sent for **every** session whose placement differs from the last one announced. That includes a session created moments ago: its baseline is the placement in its `create_session` row. Nothing else changed.

**Evidence.**
- `scripts/hive-test.sh` passed three times in a row (lib 342 passed, 0 failed).
- `scripts/hive-test.sh -- --tests` passed: lib 342, bin 4, http_engine 10, http_stream 6, http_warmup 2, rpc 5. TEST_EXIT=0, with no warnings.
- rustfmt `--check` was clean on the touched files, there is no CR in any changed file, and `--restore` printed RESTORED.
