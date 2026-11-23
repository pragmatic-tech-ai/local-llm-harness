# Final whole-branch review: feat/agent-templates (a49e069c..24dfce03, 28 commits)

Reviewer: final reviewer (read-only). Plan: `docs/plans/2026-11-23-agent-templates-sessions.md`. Spec: `docs/specs/2026-11-23-agent-templates-sessions-design.md`, including "Settled details".

**Method.** I did the review in passes and prioritised the seams between tasks over re-reading code the per-task reviews had already cleared. I read in full the current HEAD of `runtime/server.rs` and `runtime/hub.rs`, plus the non-test parts of `agents/*`, `tool_loop.rs`, `main.rs` and the `supervisor/*` diffs. On the app side I read `agents.ts`, `RuntimeClient.ts`, `JsonRpcConnection.ts`, `SessionsService`, `SessionViewModel`, `SessionsPanelViewModel`, `ChatViewModel`, the `InstrumentPanelViewModel` diff, `AgentsService`, `NewSessionDialogViewModel` and the save/load paths of `TemplateEditorViewModel`, along with the `.mu` views, the main process and preload, and the README. I did not run cargo, because Rust tests run on the hive only. I ran `cd app && npx vitest run`: 16 files and 168 tests passed.

## Strengths

- **The wire shapes match field by field.**
  - `agents.ts` mirrors what the server actually emits:
    - `SessionRow` flattens `SessionSummary` and adds `placement` and `busy`.
    - `Placement` is tagged on `state` with the values `ready`, `needs_load` and `unavailable`, and `evicts` may be null.
    - `TemplateNode` is tagged on `kind` (`group` / `template`) and carries `error: string|null`.
    - `AttachedSession` has `{summary, snapshot, transcript, sampling, enable_thinking, placement}`.
    - `CatalogView` has `runtimes {name, kind}`, `models {name, runtimes, size_hint}` and `hosts string[]`.
    - `ToolSpec` has `{name, description, parameters, side_effects}`.
    - `ChatMessage` round-trips through serde: lowercase `Role`, optional `tool_calls` with `type`, `tool_call_id` and `tool_result`.
    - The notes `turn_error {message}`, `tool_result {id,name,ok,content}` and `loop_done {steps,reason}` match the `ToolLoop` helpers.
  - The method names and snake_case params (`new_name`, `template_id`, `enable`, `config`) match `RuntimeConnection`'s constants.
- **Hub design.**
  - Each connection has one outbound channel, so a response is always queued behind the notes that preceded it.
  - The `BusyGuard` flag changes and its `session_busy` broadcast happen under one lock.
  - The placement map is seeded by `create_session` and replaced under a lock by the watcher.
  - There are no nested-lock hazards. The orders are placements → session store → harness `shared`, and hub state on its own. No lock is held across an `.await`.
- **Dispatcher and host.**
  - The `unload_if` check and stop are atomic under the host's loading lock.
  - `link_for_turn` holds no lock across the load.
  - `n_ctx` is part of "the same model", exactly as the settled detail says (`host.rs:173-177`).
  - The two merger/forwarder tasks subscribe before they spawn and are aborted on drop.
- **Template store paths.** Paths from the wire are validated per segment (`dir_for`, `valid_segment`, reserved names), and ids go through `locate`. I found no path traversal from `save_template`, `create_group`, `rename_group`, `delete_group` or `move_template`. Session ids only reach the filesystem after a map lookup.
- **Review Focus 2, 3 and 5** are implemented and tested: `n_ctx_mismatch_needs_load`, `busy_is_never_persisted` and `session_survives_template_delete`. Focus 4 is implemented; its test has a gap (Minor 12).
- **The old flow is cleanly removed from the build.** The sidecar, `backend-url`, the `HarnessPicker`/`RuntimeManagementService`, `HarnessPanel.mu`, `RuntimeModule.mu`, `runtime/session.rs`, `SupervisorConnector`/`WorkerConnector`, `TOOL_STEP_CAP`, the `backend-url` IPC and the preload's `BackendUrl` are all gone. Nothing compiled still references them.
- **App reconnect.** It uses backoff with a generation guard, re-attaches the shown session, and `LoadHistory` replaces the partial turn. The Agents capability follows `OnConnectionChanged` and reloads the tree and the catalog on reconnect.

## Issues

### Critical

None.

### Important

**1. Head-of-line blocking: one shared socket plus strictly sequential requests freezes every other feature while a turn or a model load runs.**

- **Where:**
  - `backend/src/runtime/server.rs:370-378`. `run` awaits `self.respond(..)` before it reads the next frame, and `send_turn` (`server.rs:625-668`) returns only after `loop_done`.
  - `app/src/renderer/src/AgentsService.ts:68-69` and `SessionsService.ts:70`. Ruling R1 shares one `RuntimeClient` between Sessions and Agents.
- **What is wrong:** A turn can take minutes: an 8-step budget plus tools, plus a model load of tens of seconds on the first message. So can `load_session_model`. While either runs, no other request on that window's connection is even read. Every one of these hangs:
  - `get_template`, `save_template` and `list_templates` (the Agents tree and editor)
  - `list_sessions`
  - `rename_session` and `delete_session`
  - `tokenize` and `model_props` (the instrument panel)
  - `attach_session` (selecting another session during a Load)
- **Why it matters:** The spec has the user editing templates in the Agents capability while sessions exist. In practice the editor shows "loading" and Save spins until the turn ends. This also turns the Task 10 out-of-scope note (below) from a blip into a freeze that can last minutes.
- **Fix (preferred, server-side):**
  - In `RuntimeConnection::run`, handle each request in its own spawned task. Responses already go through the hub channel, and JSON-RPC ids correlate them, so order between responses is not required.
  - Capture the attached session id when the request arrives, so a later `attach_session` does not retarget an in-flight chat call.
  - Keep "one turn per session" through `hold_busy`, as today.
  - `RuntimeConnection` then becomes `Arc`-shared (`deps` + `conn`). Nothing else in it is mutable.
  - Add a server test: a turn is held at link(0), and `list_templates` on the same connection answers before `loop_done`.
- **Fix (alternative, app-side):** Give `AgentsService` and `SessionsPanelViewModel` their own `RuntimeClient`. This reverses R1, which needs an owner ruling.

**2. Templates that parse but fail validation are listed as healthy and can start sessions (Review Focus 1, spec "fails to parse or validate").**

- **Where:**
  - `backend/src/agents/template_store.rs:199-225`. `read_level` only runs `toml::from_str`; `TemplateRules::validate` is called only in `save` (`template_store.rs:246`).
  - `backend/src/runtime/server.rs:578-594`. `create_session` snapshots whatever `templates.get` returns, without validating it.
- **What is wrong:** Take a hand-edited `tools = ["nope"]`, `step_budget = 0`, an unknown runtime, or a seeded `Default` whose model is `""` because no model was discovered (`template_store.rs:155`). Each is shown without the "broken" mark, the New session dialog enables Create, and the session then misbehaves:
  - `step_budget = 0`: every turn ends `step_limit` with 0 steps and no generate call.
  - An unknown tool is silently dropped by `openai_tools_for`.
  - An unknown model shows up only later, as `Unavailable`.
- **Why it matters:** Review Focus 1 and the spec's "Files and drift" bullet both require such files to be listed as broken, with the error shown in the editor. The plan test `broken_template_is_listed_with_error` only covers a parse failure.
- **Fix:**
  - Make `read_level` a `&self` method (or pass `&self.rules`), and set `error: Some(validate_err)` for templates that parse but fail validation.
  - Have `create_session` refuse with the validator text.
  - Add a test for each case: unknown tool, `step_budget = 0`, model not served by its runtime.
- **App seam that must change with it:** `TemplateEditorViewModel.ts:628-632` seeds the editor from defaults whenever `node.Error !== null`. With validation errors in the tree, that would throw away the template's real system prompt, tools and sampling. Change `Load` to always try `GetTemplate` first, which still succeeds for a validation-only error. Show `node.Error` as `ErrorText` with `broken = true`, and fall back to `Seed` only when `GetTemplate` fails. That is the parse or duplicate case.

### Minor

**3. "loading model…" sticks after a reconnect during a first-message load.**

- **Where:** `app/src/renderer/src/SessionViewModel.ts:228-232` (`Detach`) and `:189-225` (`Attach` with the same id skips `Reset`).
- **What is wrong:** `turnLoading` is only cleared by `turn_started`, `loop_done` or `Reset`. If the socket drops between `session_loading` and `turn_started`, the re-attach keeps `turnLoading = true`. The header shows "loading model…" indefinitely and Load and Unload stay disabled until the user sends another message.
- **Fix:** Clear `turnLoading` (and `loading`) in `Detach()`, or in `Attach` when the reply arrives. Add a test: `session_loading`, socket close, reopen and re-attach, then `CanLoad` follows the placement.

**4. Snapshot replies can be older than notifications that arrive before them.**

- **Where:**
  - `server.rs:567-576` (`list_sessions`) and `server.rs:596-609` (`attach_session`) compute `placement` and `busy` and only then enqueue the reply.
  - `hub.rs:193-206` (the watcher computes and broadcasts) and `hub.rs:155-172` (busy broadcasts) run concurrently on other tasks.
- **What is wrong:** A `placement_changed` or `session_busy {busy:false}` broadcast between the computation and the enqueue reaches the client first. The older reply then overwrites it:
  - `SessionsPanelViewModel.Reconcile` → `row.Update`
  - `SessionViewModel.Attach` → `placement`
- **Effect:** A stale dot or Load button until the next change. This most likely shows with two windows, or right after a commit.
- **Fix:**
  - Build the rows and call `hub.send(conn, reply)` while holding the hub's placements lock (busy: the state lock).
  - Have `PlacementWatcher::publish` broadcast while it still holds the placements lock (`update_placements` returns while locked; move the broadcast inside).

**5. Runtime discovery is no longer "one source of truth".**

- **Where:** `supervisor/manager.rs:548-551` publishes `chat_url` only on a Ready harness. `app/src/renderer/src/SupervisorClient.ts:32-49` falls back to `:8779`.
- **What is wrong:** With explicit load, nothing is loaded at app start or reconnect, so the fallback is the normal path. If `--runtime-bind` or `HARNESS_RUNTIME_BIND` is not 8779, the app never connects while the hive is idle. Ruling R2 accepted the fallback, but the spec line "keeps one source of truth" holds only for the default port.
- **Fix:** Have the control plane advertise the runtime port independently of harness state, for example a `runtime_url` field in a `hello` or `list_runtimes` result. Prefer it in `RuntimeUrl`.

**6. Catalog, rules and discovery are frozen at startup.**

- **Where:**
  - `main.rs:140-152` builds `TemplateRules`, and `CatalogView` through `CatalogListing::models` (another `discover`).
  - `host.rs:155` caches discovery per host; `LocalSupervisorHost::refresh` (`host.rs:164`) has no production caller.
- **What is wrong:** A model added on disk is offered by the control plane's live `list_models`, but the template editor's combos never show it, `save_template` rejects it as `unknown model`, and `serves` says no, all until a supervisor restart. The spec's "Refresh picks up hand edits" covers templates only, but the user expectation reaches the model list too.
- **Fix:** Either rebuild rules, catalog view and host discovery on a `refresh_catalog` call that the Agents Refresh triggers, or document "restart the supervisor after adding models" in the README.

**7. A `WsWorkerLink` is not pinned to the harness it was checked against.**

- **Where:** `host.rs:179-186`. The link is just the fixed `ws://127.0.0.1:{worker_port}`.
- **What is wrong:** `current_link` checks runtime, model and n_ctx, then hands out a URL. If another session's load swaps the model between that check and `model_props`/`generate` connecting (each step makes fresh connections), the step runs on the new model. The "eviction mid-turn ends in error" guarantee relies on the old worker's socket being killed during an open stream.
- **Fix:** Carry the harness identity (id plus a start generation) in the link and re-check it at connect, or verify `model_props.model_name` against the requirement before `generate`.

**8. Dead code left by the redesign.**

- `Hub::try_busy` and `Hub::clear_busy` (`hub.rs:139-149`) are used only by tests. Make them `#[cfg(test)]`, or drop them and use `hold_busy`.
- `ModelHost::unload` (`host.rs:71`, `:234`) is unused in production, because the dispatcher uses only `unload_if`. Drop it from the trait.
- `LocalSupervisorHost::refresh` (`host.rs:164`) is unused; see Minor 6.
- `RuntimeClient.Reset` and `RuntimeClient.ListTools` (`RuntimeClient.ts`) have no app caller. Either wire a Reset button into the session header (the spec defines `reset`), or mark them as kept-for-protocol with a test only.

**9. Stale docs and comments from the removed flow.**

- `README.md:13-14` still says "the Electron shell that spawns it". `README.md:23` strikes the line through instead of updating it, and the Status section never mentions Sessions or Agents.
- `app/src/renderer/src/SettingsKeys.ts:1` says "declared on RuntimeModule"; it is now `SessionsModule`.
- `JsonRpcConnection.ts:26` says "mirrors SidecarExitSignal", which is deleted.
- `JsonRpcConnection.ts:71-72` still says `'Harness not connected'` / `'Harness socket error'`, now shown for runtime calls (e.g. "rename failed: Harness not connected").
- `backend/src/agents/host.rs:4-5` says "The dispatcher (a later task)".
- `runtime/test_support.rs:3` has a stale import list.
- `docs/roadmap.md` step 1 still describes the old design ("drives the supervisor's start/stop lifecycle").

Fix: update the text. Rename the messages to "Runtime not connected" and "Runtime socket error".

**10. Override and rename writes bump `updated`, which silently reorders the list.**

- **Where:** `session_store.rs:221-233`. `mutate` bumps `updated` for `set_overrides` and `rename` too, and `set_sampling`/`set_thinking` broadcast nothing (`server.rs:473-482`).
- **What is wrong:** A thinking toggle moves a session to the top of the list in the server's order, but other windows are not told. `updated` means "last activity" in the panel.
- **Fix:** Bump `updated` only in `commit` and `reset` (or create), or broadcast `sessions_changed` after `set_overrides`.

**11. Misleading `send_turn` errors at two edges.**

- **Deleted mid-turn:**
  - Where: `server.rs:666`.
  - What is wrong: when the sender's own session is deleted mid-turn, `delete_session` detaches it, so the sink reaches 0 connections. The reply is `client connection closed`, which the app appends as "[send failed: client connection closed]" just before it clears the view.
  - Fix: if `!sessions.contains(id)` after `SinkClosed`, answer `unknown session: {id}`.
- **Commit fails after Answered:**
  - Where: `server.rs:657-661`.
  - What is wrong: the client has already rendered an answer that is not persisted and will not be in the next turn's context.
  - Fix: return a specific message ("answer not saved: {e}") so the app's note is explicit.

**12. Host configuration accepts ambiguous setups.**

- **Where:** `supervisor/catalog.rs:96-110`, and `TemplateRules::validate` (`template.rs:52-72`).
- **What is wrong:**
  - Duplicate host names, and more than one `local = true` supervisor (each wrapping the same manager), are both accepted.
  - A template's `host` is never validated, so a typo gives `Unavailable: no host serves llamacpp/x`, which points at the model rather than the host.
- **Fix:**
  - Reject duplicate names and more than one local host in `HostConfigs::build`.
  - Add `hosts` to `TemplateRules` and fail with `unknown host: {h}`.

**13. Review Focus 4 test gap.**

- **Where:** `server.rs:1138-1156` (`deleted_session_detaches_connections`).
- **What is wrong:** The test checks that the deleted session's connections lose the session. It never checks Focus 4's "other connections are unaffected".
- **Fix:** Add a third client attached to another session and assert that its `send_turn` still runs.

**14. Unusable agents data looks like a network problem.**

- **Where:** `main.rs:234-244` (store open fails → no runtime bind, stderr only) and `SessionsPanelViewModel.ts:26`.
- **What is wrong:** The app shows "Connecting to the runtime…" forever, with no hint that the hive's data dir is the cause.
- **Fix:** Advertise "runtime disabled: {reason}" on the control plane (pairs with Minor 5's field), and show it in the panel's empty text.

## Out-of-scope notes from the per-task reviews, triaged

- **Task 10: the form is disabled while a save is in flight** (`IsEditable` false while `saving`). Accept the behaviour: it keeps edits from racing the baseline the save will record. On its own the window is milliseconds. Today, though, a Save issued during a turn waits behind it (Important 1), so the form can stay disabled for minutes. Fixing Important 1 resolves this; no separate change is needed.
- **Task 9 re-review:** "Out of scope: None found." Nothing to triage.
- **Task 8 review:** No out-of-scope lines. Minors 1-5 were fixed in 9282ce53; I checked that `SamplingConfig` now lives in `agents.ts`.
- **Tasks 1-7:** Only ledger lines; there are no review files with out-of-scope sections. The ruled items (duplicate ids, `max #n+1` titles, mid-load `NeedsLoad{evicts:None}`, step-0 `turn_error`, seeded placements) are all consistent with HEAD.

## Declined to judge

- **Auth and exposure of `:8779`/`:8778` on `0.0.0.0`:** These are pre-existing defaults, and the spec's non-goal is multi-user. It is worth noting, though, that the runtime now also writes template files.
- **Blocking file IO (`sync_all`) under std mutexes inside async handlers:** Session and template files are small at home-lab scale. Worth revisiting if transcripts grow large.
- **Unbounded per-connection channels:** A stalled client accumulates turn notes in memory. Acceptable for one user.
- **Rust test results:** I did not run them (hive-only). I relied on the per-task GREEN evidence in the ledger.
- **How the `.mu` views render (TreeView, ContextMenu, dialogs):** This is live-only and belongs to Task 11's live proof.
- **Re-seeding `Examples/Default` when the templates root is empty, not only when it is missing:** This matches the plan's wording ("missing or empty"), so I treat it as intended.

## Assessment

**Ready to merge: No, with fixes.** The wire contract, the hub/dispatcher concurrency and the removal of the old flow are sound, and the app suite is green (168/168). Two Important issues remain. A turn or load freezes the Agents and Sessions features on the shared socket. Validation-broken templates are listed as healthy and can start sessions, against Review Focus 1, and the fix needs a matching editor change. Fix both, plus the Minors per the fix-minors rule, before Task 11.

## Re-review of fix wave (24dfce03..43b99d25)

Scoped re-review of the 14 findings against the controller's rulings, plus the fix diff for new breakage. I read the whole server.rs diff and the HEAD dispatch path, plus the hub, dispatcher, host, manager, session-store, template, template-store, catalog-source, main, control and app diffs. I did not run cargo (hive-only). I ran `cd app && npx vitest run`: 16 files and 176 tests passed.

**Verdict: Needs fixes (Minor only).** All 14 findings are fixed correctly. The fix diff adds two new Minors in the concurrent dispatch.

### Findings 1-14

- **1 Fixed.**
  - Arrival order holds. `start` runs synchronously in the read loop for `attach_session`, `set_sampling`, `set_thinking`, `list_tools`, `reset` and `send_turn` (busy flag plus record read). `model_props` and `tokenize` resolve their link there too. Only the remainder is spawned.
  - No deadlock. The synchronous parts contain no `.await`. `hold_busy` broadcasts through unbounded channels. The lock orders are unchanged.
  - No response is lost while the connection lives. Each spawned task sends its own reply through the hub, after that request's notes. ConnIds are monotonic, so a late reply cannot reach a reused id.
  - Busy semantics are kept. A second `send_turn` or a `reset` during a turn now fails `session is busy` at once instead of queueing behind the turn. The app never issues either.
  - Attach races are closed. The turn captures its session id, and `model_props`/`tokenize` capture their link at start. The app locks session switching during a turn (`SessionsPanelViewModel` switch lock), so attaching mid-turn from the app is not reachable.
  - Writer and channel lifetime are sound. When the read loop ends, `RuntimeConnection` drops and unregisters, which drops the tx. The writer drains, closes, and late replies are dropped.
  - Commit uses `mutate` (read-modify-write of the transcript only), so a `set_sampling` sent during a turn is not clobbered.
- **2 Fixed.**
  - `tree()` validates with live rules. `get_template` returns `{.., error}` and fails only on a parse failure or a duplicate. `create_session` refuses a flagged template.
  - App mapping matches: `TemplateRecord.error` is normalised to `null`. `Load` always tries `GetTemplate`, and a validation error opens the real fields with `broken = true`. `Seed` is used only when `GetTemplate` fails, and the new `seeded` flag correctly gates the late-catalog completion.
  - `NewSessionDialog` disables Create on `HasError`.
- **3 Fixed.** `Detach` clears `turnLoading`, `loading` and the status.
- **4 Fixed.** It was done app-side; there is no `Ruling:` line for 4, but the approach is sound. Every placement and busy change emits a note on the same ordered channel, so keeping a note that is newer than the request converges, both in the panel (per field, per row) and in `SessionViewModel` (attach and Load replies).
- **5 Fixed.** Deviation accepted: `list_runtimes` is a bare array, and the ruling's intent ("discovery reads it") is met by `hello`. The app falls back from `hello` to `list_harnesses` and then to :8779.
- **6 Fixed.** Deviation accepted: re-running discovery on `list_templates` is required for the tree to validate with the same rules as save. `list_templates` does not refresh the host model lists, but `create_session` and `list_catalog` do, so placements catch up.
- **7 Fixed per ruling.** The step-0 identity is recorded and compared before every later step, and a reload counts as a change. A check-then-connect window between the compare and the worker connect remains. It is too small to matter, because a swap needs a full load inside it.
- **8 Fixed.** I grep-confirmed that none of the removed symbols remain.
- **9 Fixed.** The stale strings are gone (grep).
- **10 Fixed.** Deviation accepted: ruling 10 ("only commit/create/rename bump") excludes `reset`.
- **11 Fixed.** `ERR_DELETED` and `ERR_NOT_SAVED` have tests.
- **12 Fixed.** Duplicate names and a second local host are rejected. `unknown host` is validated.
- **13 Fixed.** A third client on another session runs to `loop_done` and commits.
- **14 Fixed.** `hello.runtime_error` covers both the store-open and bind failures. Both panels show it, and discovery skips the socket while it is set.
- **Deviation 4 accepted.** A turn whose sender's socket closed, with no other attached connection, now aborts at its next note. Before, it aborted at the first failed write after the writer died, so this is the same rule, applied marginally sooner.

### New issues (from the fix)

- **N1 (Minor): a panicking request task now leaves its request unanswered.**
  - Where: `server.rs` `run`, `tokio::spawn(async move { hub.send(conn, Frames::reply(id, pending.await)) })`.
  - Before, a panic killed the connection task. The connection unregistered and the socket closed, so the app reconnected.
  - Now only the per-request task dies, and the connection stays up with that id pending forever. The app has no request timeout, so the editor or panel spins.
  - Fix: wrap `pending` in `FutureExt::catch_unwind` (with `AssertUnwindSafe`) and reply `RpcError::internal("request failed")` on a panic.
- **N2 (Minor): a turn aborted because its sender attached elsewhere replies "client connection closed" to a connected client.**
  - Where: `run_turn`, `Err(SinkClosed) => CLIENT_GONE`.
  - This is reachable only by non-app clients, because the app locks switching during a turn. It is the same class as finding 11.
  - Fix: pass `conn` into `run_turn` and, when `hub.attached(conn)` is still registered but not `id`, answer e.g. `session detached`.

### Out of scope

None found.
