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

