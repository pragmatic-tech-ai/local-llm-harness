# Final fix wave: feat/agent-templates (findings 1-14 of final-review.md)

All 14 findings are fixed, following the controller rulings where they go beyond the review.
Rust was built and tested only on the hive (`scripts/hive-test.sh`), with `--restore` at the
end. The app suite and both typechecks were run locally. No npm install, no new crates,
Cargo.lock unchanged.

## Per finding

### 1 (Important): head-of-line blocking on one connection

- **Fix** (`backend/src/runtime/server.rs`):
  - `RuntimeConnection::run` decodes each frame, *starts* the request (`start`), and finishes
    it in its own `tokio::spawn` task. That task queues the reply through the hub, so a reply
    still follows that request's own notes. Replies are matched by id.
  - `start` runs the synchronous part of each method at once, in arrival order. That covers
    `attach_session` and the chat methods: `set_sampling`, `set_thinking`, `list_tools`,
    `reset`, `send_turn` (busy flag + record read + config), `model_props` and `tokenize`
    (link resolution). The returned `'static` future does the rest: the turn, or the worker
    call.
  - So a `set_sampling` sent before `send_turn` applies to it. An `attach_session` sent after
    `send_turn` does not retarget it, because the turn captured its session id when it
    started.
  - Template, catalog, session-list and model-load methods live in a new `SharedRequests`
    struct (shared state only). They run entirely in their task and never wait behind a turn
    or a load.
  - Attaching while a turn runs on the same connection keeps today's semantics. The turn
    keeps running for the session's other connections, and aborts with `client connection
    closed` if none remain.
- **Tests:**
  - `requests_on_one_connection_answer_while_its_turn_runs`. This runs over the real socket.
    The turn is held mid-step in a gated load, and `list_templates`, `list_sessions` (with
    `busy: true`) and `save_template` all answer on the same connection before `loop_done`.
  - `chat_methods_take_effect_in_arrival_order`: attach, then `set_sampling`, then
    `send_turn`, started back to back. The turn sees the sampling.
  - `rpc.rs` `TestClient` gained `send_request` and `response_to`.

### 2 (Important): validation in the tree

- **Backend:**
  - `template_store.rs`: `tree()` validates every parsed template with the same rules as
    `save`. A failure is listed with `error` set to the validation text, plus the real name,
    runtime and model.
  - New `TemplateStore::check(&t)`.
  - `get_template` returns `{id, path, template, error}`, and fails only on a parse failure or
    a duplicate id.
  - `create_session` refuses a template that fails validation, with the text as the RPC error.
- **App:**
  - `agents.ts`: `TemplateRecord.error: string | null`.
  - `RuntimeClient.GetTemplate` normalises a missing `error` to `null`, for an older server.
  - `TemplateEditorViewModel.Load` always calls `get_template` first.
    - On success it fills the real fields. `broken` and `ErrorText` come from `record.error`.
    - On failure (a parse error or a duplicate) it falls back to `Seed`: the defaults plus the
      tree node's fields.
    - A new `seeded` flag keeps a late catalog from re-filling the defaults over real fields.
      Only a `seeded` form is completed from the catalog.
  - The New session dialog already disables Create for any node with `HasError`. Validation
    errors now reach the tree, so flagged templates are disabled too. The existing test
    `broken_template_cannot_be_created` uses a validation-style error.
- **Tests:**
  - `template_failing_validation_is_listed_with_its_error` covers an unknown tool,
    `step_budget = 0`, a runtime that cannot load the model, and an unknown host.
  - `template_failing_validation_is_flagged_and_cannot_start_a_session` is the server test.
    It covers the tree error, `get_template.error` (and `null` on a good template), and the
    `create_session` refusal.
  - App: `template_failing_validation_opens_with_its_real_fields_and_the_error`, and the
    updated `broken_template_opens_with_error`, which now goes through a failing
    `get_template`.
  - RuntimeClient mapping rows cover `error` and its absence.

### 3 (Minor): "loading model…" sticks after a reconnect

- **Fix:** `SessionViewModel.Detach` clears `turnLoading`, `loading` and the loading status.
- **Test:** `a_reconnect_during_a_first_message_load_ends_loading_model` (`session_loading`,
  then detach, then re-attach; `CanLoad` follows the placement).

### 4 (Minor): snapshot replies older than notifications

- **Fix:** done app-side, per the ruling.
  - `SessionsPanelViewModel` counts placement and busy notes per session and per field.
  - `Refresh` records the counter when it asks. In the reply, a field whose note is newer
    keeps the notified value (`KeepNewerNotes`).
  - `SessionViewModel` counts `placement_changed` for the shown session. An attach reply, or
    a Load reply, computed before a newer note does not overwrite the placement.
- **Tests:** `a_list_reply_older_than_a_notification_keeps_the_notified_state` and
  `an_attach_reply_older_than_a_placement_change_keeps_the_change`.

### 5 (Minor): runtime URL

- **Fix:**
  - `supervisor/control.rs` has a new `hello` method returning `RuntimeStatus
    {runtime_url, runtime_error}`.
  - `runtime_url` is `ws://{advertise_host}:{bound port}` whenever the runtime port is bound,
    whether or not a model is loaded. `main.rs` sets it from the bound listener.
  - App: `SupervisorClient.Hello()`. `RuntimeUrl(supervisorUrl, hello, harnesses)` prefers
    `runtime_url`'s port, then a harness's `chat_url`, then :8779.
  - `SessionsService` calls `hello` first. A supervisor that rejects `hello` (an older one) is
    asked `list_harnesses`.
  - I put the field on the new `hello`, not on `list_runtimes`. That method returns an array,
    so a top-level field cannot be added without breaking its shape. Discovery reads `hello`.
- **Tests:**
  - Rust: `hello_reports_the_runtime_url_without_a_loaded_model`.
  - App: `hello_parses_and_its_runtime_url_wins_over_chat_url` and
    `hello_runtime_url_is_preferred_and_an_old_supervisor_falls_back_to_chat_url`.

### 6 (Minor): catalog freshness

- **Fix:**
  - New `runtime/catalog_source.rs`:
    - `CatalogView`, moved here and re-exported from `server`.
    - A `CatalogSource` trait. A fixed `CatalogView` is its own source.
    - `DiscoveredCatalog`, which implements `CatalogSource` and `RulesSource` and re-runs
      `discover` on every read.
  - `agents/template.rs`: a `RulesSource` trait. `TemplateRules` and `Arc<T>` implement it.
  - `TemplateStore` holds a `Box<dyn RulesSource>` and reads the rules on every tree, check
    and save.
  - `RuntimeDeps.catalog` is `Arc<dyn CatalogSource>`.
  - `list_catalog` and `create_session` call the new `Dispatcher::refresh()`. It calls the new
    `ModelHost::refresh()` on every host, which re-runs `LocalSupervisorHost` discovery, then
    signals the placement watcher.
  - So a dropped model is offered, accepted by save, and servable without a restart.
    Runtimes, tools and hosts stay fixed; the spec's Settled details now say so.
- **Tests:** `a_model_dropped_on_disk_is_seen_without_a_restart` (`DiscoveredCatalog`),
  `rules_are_read_on_every_use` (template store) and
  `identity_follows_reloads_and_refresh_signals` (dispatcher).

### 7 (Minor): model swaps

- **Fix:**
  - `HarnessInfo.instance` (`#[serde(skip)]`) is set to the manager's start generation when a
    harness becomes Ready.
  - New `HarnessIdentity {runtime, model, n_ctx, instance}`, `ModelHost::identity()` and
    `Dispatcher::identity(req)`.
  - `SessionLinks` records the identity with step 0's link. Before each later step it compares
    the host's current identity. If a harness is loaded and it differs (a reload of the same
    model included), the step returns `model changed`.
  - The tool loop turns that into `turn_error {message:"model changed"}` and then
    `loop_done {reason:"error"}`.
  - Nothing loaded still gives `no model running`, which keeps the unload semantics.
  - `FakeHost` bumps an instance on every loaded `set_state`.
- **Tests:**
  - `a_reloaded_model_ends_the_turn_with_model_changed` (`SessionLinks`).
  - `a_model_swap_between_steps_reports_model_changed`: a full `ToolLoop` run with a tool
    call at step 0 and a reload before step 1. It asserts the last two notes and
    `EndReason::Error`.
  - `identity_names_the_ready_harness_and_each_start` (local host).

### 8 (Minor): dead code

- **Fix:**
  - Deleted `Hub::try_busy` and `Hub::clear_busy`. The tests use `hold_busy`.
  - Deleted `ModelHost::unload` and its implementations. Its tests now use `unload_if`; the
    duplicate test `unload_waits_for_an_in_flight_load` is gone, because
    `unload_if_waits_for_an_in_flight_load` already covers it.
  - The inherent `LocalSupervisorHost::refresh` is now the trait's `refresh`. It has a
    production caller: `Dispatcher::refresh`.
  - Deleted `RuntimeClient.Reset` and `RuntimeClient.ListTools`, their method constants and
    their tests. `ChatViewModel` and the rest of the app never called them; `reset` and
    `list_tools` stay on the server.
  - `RuntimeConnection::dispatch` is now only a test entry point, so it is `#[cfg(test)]`.

### 9 (Minor): stale text

- **Fix:**
  - `README.md` Status: the supervisor and runtime, the Sessions and Agents capabilities, and
    no struck-through sidecar line.
  - `SettingsKeys.ts` now names SessionsModule.
  - `JsonRpcConnection.ts`: the `SidecarExitSignal` mention is gone. The messages are now
    `Runtime not connected` and `Runtime socket error`, and the three test expectations follow.
  - `host.rs` module doc.
  - `test_support.rs` import list.
  - `docs/roadmap.md` step 1 says it shipped (2026-11-23) and describes what shipped.
  - Spec: the App bullet (`hello`'s `runtime_url`), the protocol lines for `list_templates`
    and `get_template`, and new Settled details covering catalog freshness, validation in the
    tree, hosts, concurrent requests, the turn bound to its harness, `updated`, the
    `send_turn` errors and runtime discovery.

### 10 (Minor): the `updated` stamp

- **Fix:** `SessionStore::mutate` takes a `Bump`. `commit` and `rename` bump it, and `create`
  sets it. `set_overrides` and `reset` keep it.
- **Test:** `list_orders_by_updated_then_id_descending_and_updated_bumps` now asserts that
  overrides and reset keep `updated`.

### 11 (Minor): `send_turn` errors

- **Fix:**
  - When the sink closes and the session no longer exists, the reply is `session deleted`
    (`ERR_DELETED`).
  - A commit failure after Answered gives `answer shown but not saved: {e}` (`ERR_NOT_SAVED`).
- **Tests:**
  - `own_session_deleted_mid_turn_reports_session_deleted`: a gated load, a delete from
    another client, then release.
  - `failed_commit_says_the_answer_was_not_saved`: the sessions dir is removed before the turn.

### 12 (Minor): hosts

- **Fix:**
  - `HostConfigs::build` rejects `duplicate host name: {n}` and `more than one local host: {n}`.
  - `TemplateRules.hosts` lets validation fail with `unknown host: {h}`. The `DiscoveredCatalog`
    rules carry the configured host names.
- **Tests:** `rejects_duplicate_names_and_a_second_local_host`, plus the `host` case in the
  tree validation test.

### 13 (Minor): the Review Focus 4 test gap

- **Fix:** `deleted_session_detaches_connections` now has a third client attached to another
  session. After the delete, its `send_turn` runs to `loop_done` and commits.

### 14 (Minor): data-folder failure

- **Fix:**
  - `main.rs`: a failed `AgentsStack::open` message (`agents data unavailable: {e}`) or a
    failed runtime bind becomes `hello.runtime_error`.
  - `Config::bind_runtime` now returns `Result<TcpListener, String>`.
  - App: `SessionsService.RuntimeError` and `OnRuntimeErrorChanged`. While an error is set,
    discovery does not try the runtime socket; it retries with backoff, and a clear `hello`
    connects.
  - `SessionsPanelViewModel.EmptyText` shows `The runtime is off on the supervisor: {e}` in
    place of "Connecting to the runtime…".
  - `AgentsService` forwards the error to `AgentsTreeViewModel.SetRuntimeError`, which shows
    it on the tree's status line and clears it when the error clears.
- **Tests:**
  - Rust: `unusable_data_dir_fails_to_open` asserts the message prefix, and the `hello` test
    covers the disabled case.
  - App: `runtime_error_replaces_connecting_until_connected`,
    `a_runtime_off_on_the_supervisor_shows_why_and_is_asked_again_on_retry` and
    `a_runtime_turned_off_on_the_supervisor_shows_why_in_the_tree`.

## Wire changes

- **Runtime (`:8779`):**
  - `get_template` result: a new `error: string | null` field (the validation error).
  - `list_templates`: a template that parses but fails validation now has `error` set; before,
    only parse and duplicate errors set it.
  - `create_session`: refuses a template that fails validation, with the validation text as
    the RPC error.
  - `send_turn` errors: `session deleted` (your own session was deleted mid-turn) and
    `answer shown but not saved: {e}` (the commit failed after the answer was shown). Before,
    these were `client connection closed` and the raw store error.
  - New turn outcome: `turn_error {message: "model changed"}` then `loop_done {reason:
    "error"}` when the harness changes between steps.
  - Requests on one connection run concurrently. Responses can arrive out of request order,
    matched by id. The app's `JsonRpcConnection` already matched by id.
  - `save_template`: also validates `host` against the configured hosts (`unknown host: {h}`).
  - `list_catalog`: models re-discovered on every call.
- **Supervisor control (`:8778`):**
  - New `hello` method returning `{runtime_url: string|null, runtime_error: string|null}`.
  - `chat_url` on Ready harnesses is unchanged and stays as the fallback.
  - `HarnessInfo.instance` is not serialised.

## Evidence

- **`scripts/hive-test.sh -- --tests`** (default features):
  - First run: RED. It was a compile error from a spec edit that dropped `ensure_ready` from
    the `ModelHost` trait. I restored it.
  - Second run: GREEN. The lib had 354 passed and 0 failed, and every integration-test binary
    passed (7, 10, 6, 2 and 5). There were two warnings: `dispatch` unused outside tests, and
    an unused import in the `main.rs` tests. Both are fixed.
  - Final run: see the addendum below.
- **`scripts/hive-test.sh --features llamacpp`:** the lib had 362 passed and 0 failed, with no
  warnings. The final rerun is in the addendum.
- **`cd app && npm test`:** 16 files and 176 tests passed. There were 168 before.
- **`npm run typecheck:web` and `npm run typecheck:node`:** both clean.
- **Formatting:** `rustfmt --edition 2021` was applied to the lines I changed. The
  rustfmt-check diffs that remain in `manager.rs` (lines ~1160-1207) and `control.rs` (~484)
  were there before and are left untouched.

## Concerns

- I added the `runtime_url` field to a new `hello` method, not to `list_runtimes`, because
  `list_runtimes` returns a bare array. The ruling allowed "or wherever chat_url discovery
  reads", and discovery reads `hello`.
- Per ruling 10, `reset` keeps `updated`. The review's own suggestion had `reset` bump it.
- Two connection-level behaviours changed with concurrency:
  - A gone client's in-flight reply is dropped silently.
  - The connection unregisters as soon as its socket read ends. Before, a running turn kept
    the read loop blocked until the turn finished. A turn with no other attached connection
    therefore now aborts at its next note. That is the documented "no attached connection
    remains" rule, applied sooner.

## Addendum: final runs (after the two warning fixes)

- `scripts/hive-test.sh -- --tests`: GREEN, with no warnings.
  - lib: 354 passed, 0 failed.
  - Integration binaries: 7, 10, 6, 2 and 5 passed, 0 failed.
  - `TEST_EXIT=0`.
- `scripts/hive-test.sh --features llamacpp`: GREEN, with no warnings. lib: 362 passed,
  0 failed. `TEST_EXIT=0`.
- `scripts/hive-test.sh --restore`: RESTORED.

## Follow-up: re-review Minors N1 and N2, and the rustfmt drift

### N1: a panic in a request task left the request unanswered

- **Fix:** a new `RuntimeConnection::guarded(pending)` wraps every started request in
  `AssertUnwindSafe(..).catch_unwind()` (`futures::FutureExt`, already a dependency). A panic
  is answered with `-32603 request failed` (`ERR_REQUEST_FAILED`).
- **Where it applies:** `run` uses it for every request. So does the test-only `dispatch`.
- **Busy guard:** unwinding drops the future's state, so a busy guard it held is released.
- **Test:** `a_panicking_request_is_answered_and_releases_busy`. It sends a turn on a link with
  no generate script, so `FakeLink::generate` panics. It asserts the error code and message,
  that `is_busy` is false, and that busy can be taken again.

### N2: a turn left by attaching elsewhere answered "client connection closed"

- **Fix:** `start_turn` passes its `conn` into `run_turn`. On `SinkClosed`, the error is
  chosen in this order:
  - The session is gone: `session deleted`.
  - The sender is still registered and attached to another session: `session detached`
    (`ERR_DETACHED`).
  - Otherwise: `client connection closed`.
- **Test:** `a_turn_left_by_attaching_elsewhere_reports_session_detached`. The turn is held in
  a gated load, and the same connection attaches to a second session. When the gate is
  released, the turn answers `session detached` and nothing is committed.

### rustfmt drift

`rustfmt --edition 2021` was run on `supervisor/manager.rs` (the asserts and the `manager(..)`
call around lines 1160-1207) and `supervisor/control.rs` (the `dispatch` call around line 484).
It changed formatting only. `rustfmt --check` is now clean for `server.rs`, `manager.rs` and
`control.rs`.

### Wire change

`send_turn` can answer `session detached`. Any request can answer `request failed` (-32603) if
its handling panics.

### Evidence

- `scripts/hive-test.sh -- --tests`: GREEN, with no warnings.
  - lib: 356 passed, 0 failed. That is 354 plus the 2 new tests.
  - Integration binaries: 7, 10, 6, 2 and 5 passed.
  - `TEST_EXIT=0`.
- `scripts/hive-test.sh --restore`: RESTORED.
- No llamacpp run: no code gated by `cfg(feature = "llamacpp")` was touched.
