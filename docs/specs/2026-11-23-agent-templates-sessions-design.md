# Agent Templates, Sessions and the Dispatcher — Design

**Date:** 2026-11-23
**Status:** approved 2026-11-23; shipped and live-proven on the hive 2026-11-24 (main @af5e1b17). Protocol proof: a Researcher template with only read_file answered from notes.txt with a load on the first message, and a candle + gemma-3 session evicted gemma-4 with the placement changes announced. The owner confirmed the Agents and Sessions UI and the restart persistence.
**Roadmap:** step 1 of `docs/roadmap.md` (agent definitions)
**Builds on:** `2026-11-23-tool-calling-runtime-design.md` (Agent Runtime, tool loop),
`2026-11-18-harness-supervisor-design.md` (harness lifecycle),
`2026-11-20-navigation-rail-capabilities-design.md` (capability rail)

## Intent and why

The harness today has one flat way to work. You pick runtime, model and context size in the
Runtime panel, start a harness, and chat. Every conversation gets the same default system
prompt (none), every built-in tool and a fixed 8-step cap, and it disappears when the app
reconnects.

The owner wants the harness to be about **agents**: named, reusable configurations that
decide how a conversation behaves. They also want conversations to be **sessions** you can
keep, list, return to and resume. This is step 1 of the agentic roadmap. Every later step
hangs off templates and sessions: trace, memory and context, longer tasks, planning,
multiple agents, MCP and the TODL graphs.

The owner also wants model hosting decoupled from the agent. The tool loop and harness are
always ours. The model may be served by the local hive or by remote hosts, for example one
local and three remote, and a remote host may be just a model API. A **dispatcher**
component decides where a session's model runs. Today it follows a simple rule, and it is
built to be replaced by a more sophisticated one.

## Goals

- **Agent templates**, each a named bundle of:
  - system prompt
  - tool set (a subset of the registry)
  - step budget
  - runtime
  - model
  - context size
  - thinking default
  - optional sampling defaults
  - optional preferred host

  Templates are organised in **groups**, shown as a tree.
- **An "Agents" capability on the rail.** The panel holds the group/template tree. Selecting
  a template opens its editor in the host area. You can create, rename, move and delete
  templates and groups.
- **The existing "Runtime" capability renamed to "Sessions".** The panel lists sessions.
  Selecting one loads that session's UI (chat + instrument panel) in the host area. A
  **New session** button opens a dialog where you pick a template.
- **Runtime, model and context size move out of the side panel into templates.** The
  runtime/model/context picker in the panel is removed.
- **Sessions are backend objects with persisted transcripts on the hive.** They survive app
  restarts, reconnects and supervisor restarts.
- **A dispatcher component with a replaceable placement policy.** The first policy is "(b)":
  opening a session is read-only, and its model is loaded only on an explicit **Load model**
  or on the first message.
- **A host abstraction.** Today's hive supervisor is the first host, and the model-host list
  is configuration.

## Non-goals (this slice)

- **Remote hosts.** No remote supervisor host and no model-API host are built. The `ModelHost`
  seam, its config shape and the API-host design below are defined now; only the local hive
  host ships.
- **Smarter placement policies.** Load-balancing, cost or latency routing, and fallback
  chains are out. Only policy (b) ships.
- **Agent trace, compaction, longer-task tools, planning, multi-agent, MCP.** These are
  roadmap steps 2-8.
- **Tool approval.** Postponed by the owner.
- **Sharing or importing templates between machines; template versioning beyond a snapshot.**
- **Multi-user anything.** Single user, home lab.

## Anti-goals

- **A second agent loop.** Only our tool loop runs agents, whatever host serves the model.
- **An automatic model swap triggered just by clicking a session.** Browsing history must
  never evict the running model.
- **A template edit silently changing an existing session's behaviour.** Sessions snapshot
  their template.
- **Templates hard-coding the hive.** The model host is resolved by the dispatcher, not
  baked into the session.

## Constraints

- **No new crates** (offline vendored build). `reqwest`, `tokio`, `serde`, `toml` and
  `tokio-tungstenite` are already present.
- **Rust builds and tests run on the hive only.** App tests run on the dev machine.
- **House styles.** Rust is idiomatic. TypeScript follows the owner's OOP, Allman,
  no-semicolon and hoisted-literal rules. The rail uses mural's `EditorShell`/capability
  modules, the same as the Runtime capability today.
- **Hive GPU limits.** The 6 GB GPU holds one model at a time, and swapping models takes tens
  of seconds.
- **The layering rule.** Lower layers only signal upward.

## Approaches considered

- **A. Sessions, templates and the dispatcher live in the backend, in the supervisor process
  next to the runtime (chosen).** One place owns state on the hive. External hosts are
  reached from the backend. Later, coordinator agents can create sessions themselves.
- **B. The app owns sessions and the dispatcher.** Rejected. Every host would have to be
  reached from Electron, and agents could never start sessions.
- **C. Same as A, but as a separate agent-service process.** It isolates the code, but adds
  another process to run on the hive. A can be split into C later along the same seams.

On placement, the owner chose **(b)**: read-only open, and the model loads on explicit
action or on the first message. They asked for it to be wrapped in a dispatcher that can be
swapped out later.

## The design

### Concepts

- **Template:** a stored agent definition (fields above), with an id, a name and a group.
- **Group:** a folder in the template tree. Groups nest.
- **Session:** one conversation started from a template. It holds a **snapshot** of the
  template taken at creation, a transcript, a title, created/updated times and a live state.
- **Model host:** something that can serve a model to our worker protocol. It has a name, a
  kind and capacity rules.
- **Dispatcher:** maps a session's model requirement (runtime, model, n_ctx, preferred host)
  to a host, asks that host to make the model ready, and hands the tool loop a `WorkerLink`.
  The rule it follows is a `PlacementPolicy`.

### Storage on the hive

The data root is configurable. It defaults to `C:\llm-harness\agent-data\` and is set in
`catalog.toml` as `[agents] data_dir = "..."`.

- `templates/` is a directory tree. **Directories are groups and each `*.toml` file is a
  template.** The tree on disk is the tree in the UI, and hand edits stay possible. The file
  stem is the template id, its `name` field is the display name, and groups use the
  directory name.
- `sessions/<session-id>.json` holds the snapshot, title, times and transcript (the
  `ChatMessage` list). It is written atomically (temp file, then rename) after every
  committed turn.

Template file shape:

```toml
name = "Researcher"
system_prompt = "You are a careful research assistant..."
tools = ["get_time", "list_dir", "read_file"]   # registry names; unknown names rejected on save
step_budget = 8
runtime = "llamacpp"
model = "gemma-4-12B-it-QAT-Q4_0"
n_ctx = 8192
enable_thinking = false
host = "hive"                                   # optional preferred host

[sampling]                                      # optional; omitted = worker defaults
temperature = 0.7
top_p = 0.95
```

The **system prompt** becomes the transcript's first message (`Role::System`) when the
session is created, so the Gemma template renders it in its system turn alongside the tool
declarations.

### Model hosts and configuration

The `ModelHost` trait:

- `name()`
- `kind()`
- `available_models()`: runtime/model pairs it can serve
- `state()`: what is loaded right now
- `ensure_ready(requirement) -> Result<Arc<dyn WorkerLink>, HostError>`: loads if needed
- `current_link(requirement) -> Option<Arc<dyn WorkerLink>>`: returns a link only if that
  exact model is already loaded, without side effects

Host kinds:

- **`supervisor`.** A machine running our supervisor. The **local hive host** wraps the
  in-process `HarnessManager` directly, using `start` and `ready_worker_url`, which is the
  path the runtime already uses. A future *remote supervisor* host would speak the existing
  control protocol (`start`, `list_harnesses`, `harness_changed`) to another machine's
  `:8778` and build a `WsWorkerLink` to its worker. It is designed here, not built.
- **`api`** (designed, not built). A remote model API, for example an OpenAI-compatible
  `/v1/chat/completions` endpoint. The harness is still ours: an `ApiWorkerLink` implements
  `WorkerLink`. It translates a `GenerateRequest` (messages + tools) into the API's native
  tool-calling request and maps the streamed response back into our notes (`turn_started`,
  `turn_chunk`, `tool_call`, `turn_done`), so the tool loop does not change. API keys come
  from environment variables that the host config names, never from files.

Config, in `catalog.toml`:

```toml
[[host]]            # implicit if absent: one supervisor host named "hive" = this process
name = "hive"
kind = "supervisor"
local = true
```

Only `local = true` supervisor hosts are accepted in this slice. Any other entry is rejected
at startup with `host kind not supported yet: <kind>`.

### Dispatcher and placement policy

`Dispatcher` sits between sessions and hosts:

- `requirement(session) -> ModelRequirement` (runtime, model, n_ctx, preferred host)
- `status(session) -> Placement`, one of:
  - `Ready { host }`: the model is loaded and matches
  - `NeedsLoad { host, evicts: Option<String> }`: it can be loaded, possibly replacing
    what is loaded
  - `Unavailable { reason }`: no host can serve it
- `load(session) -> Result<Arc<dyn WorkerLink>, DispatchError>`: makes it Ready
- `link_for_turn(session) -> Result<Arc<dyn WorkerLink>, DispatchError>`: used at
  `send_turn`

`PlacementPolicy` is the replaceable part. It picks the host for a requirement and decides
whether a turn may trigger a load. The shipped `ExplicitLoadPolicy`, which is **(b)**:

- **Host choice:** the preferred host if it serves the model; otherwise the first configured
  host that lists the model.
- **Opening a session or calling `status`:** never loads.
- **`send_turn` on a `NeedsLoad` session:** loads the model, which is "first message loads",
  then runs the turn. The client is told first through a `session_loading` notification.
- **Other sessions on the same host:** a load that evicts the model they use makes their
  status `NeedsLoad`. Their transcripts are untouched.
- **A turn running on a model that gets evicted:** it ends the way any truncated turn does
  today: `turn_error`, then `loop_done{reason:"error"}`, with the transcript not committed.

The dispatcher raises `placement_changed {session_id, placement}` for every session whose
status changes, so the Sessions list and session view stay live. It reuses the supervisor
manager's `harness_changed` events.

### Sessions

`SessionStore` provides:

- `create(template_id) -> Session`: takes the snapshot and writes the file
- `list() -> [SessionSummary]`
- `get(id)`
- `rename(id, title)`
- `delete(id)`
- `commit(id, transcript)`

A session's live state combines its placement (above) with `busy` while a turn runs. One
turn at a time per session: a second `send_turn` while busy is rejected with
`session is busy`.

The default title is `<template name> #<n>`, numbered per template.

The existing `ToolLoop` runs unchanged with three exceptions:

- The step cap comes from the snapshot's `step_budget`.
- The tools sent are the snapshot's subset (`ToolRegistry::openai_tools_for(names)`). The
  executor also refuses a call to a tool outside the subset, with
  `tool not enabled for this agent: <name>`.
- The link comes from `Dispatcher::link_for_turn`, so a model swap between steps is still
  detected.

### Client protocol (runtime, `:8779`)

The connection becomes session-aware. Existing methods take a `session_id`, or act on the
connection's **attached** session:

- **Templates:**
  - `list_templates` returns the tree, `[{kind:"group", name, path, children} | {kind:"template", id, path, name, model, runtime, error}]`
  - `get_template {id}` returns `{id, path, template, error}`
  - `save_template {id?, path, fields}` validates tools, runtime and model against the
    catalog and registry, and returns the id
  - `delete_template {id}`
  - `create_group {path}`
  - `rename_group {path, new_name}`
  - `delete_group {path}` (only when empty)
  - `move_template {id, path}`
- **Sessions:**
  - `list_sessions`
  - `create_session {template_id}` returns a summary
  - `attach_session {id}` returns the session's transcript, snapshot and placement, and from
    then on routes that session's notifications to this connection
  - `rename_session`
  - `delete_session`
  - `load_session_model {id}`
- **Chat**, on the attached session:
  - `send_turn {content}`: thinking comes from the snapshot, and the panel can override it
  - `set_sampling`
  - `model_props`
  - `tokenize`
  - `list_tools` returns the session's tools
  - `reset` clears the transcript back to the system prompt and persists it
- **Notifications:**
  - everything from today (`turn_*`, `tool_call`, `tool_result`, `loop_done`)
  - `placement_changed`
  - `session_loading`
  - `sessions_changed` (list-level add/rename/delete)
  - `templates_changed`

Several app windows attached to the same session all receive its notifications.

### App

- **Rail.** Two capability modules: **Sessions** (renamed from Runtime, keeping its icon) and
  **Agents** (new icon). The `supervisor.url` setting stays under the Sessions module. The
  app still discovers the runtime through the supervisor (`hello`'s `runtime_url`), so the
  runtime endpoint keeps one source of truth.
- **Sessions panel.** A **New session** button, then the session list. Each row shows the
  title, template name and a state dot: live = model loaded, idle = needs load,
  unavailable = no host. Right-click gives rename and delete. Selecting a row opens the
  session view in the host area.
- **Session view.** The existing chat and instrument panel, bound to the attached session. A
  header line shows:
  - the template name
  - the model and its host
  - the placement state
  - a **Load model** button when `NeedsLoad`, labelled with what it will evict
  - the tools indicator from the last slice

  Sending while `NeedsLoad` shows a loading note in the chat, then proceeds.
- **New session dialog.** The template tree with a short summary of the selected template
  (model, tools, budget). Choosing **Create** creates the session and opens it.
- **Agents panel.** A TreeView of groups and templates, with toolbar actions: new template,
  new group, rename, delete. Templates can be moved by drag or a "Move to…" command.
  Selecting a template opens the **template editor** in the host area:
  - name
  - system prompt (multi-line)
  - tools (checkboxes from `list_tools`)
  - step budget
  - runtime and model (combos from the catalog)
  - context size
  - thinking
  - sampling defaults
  - preferred host

  Save and Revert. Validation errors from `save_template` show inline.
- **Removed.** The runtime/model/context picker and Start/Stop in the panel. Loading happens
  through sessions. Stopping a model gets a "Unload model" command in the session view header
  (calls the host's stop).

### Migration

- On first start, with no `templates/` directory, the backend seeds one group `Examples`
  with a template `Default`. It uses llamacpp, the first discovered gemma-4 model (or the
  first model llamacpp lists), `n_ctx 8192`, all tools, step budget 8 and no system prompt.
  This matches today's behaviour.
- In-memory, connection-bound conversations from the old protocol are not migrated.

### Settled details (designer's calls, from the builder check)

- **Context size is part of "the same model".** A loaded model whose `n_ctx` differs from the
  requirement counts as `NeedsLoad`, which reloads it. `HarnessInfo` and the local host record
  the `n_ctx` the harness was started with. So two templates that differ only in context size
  do evict each other. That is the honest cost of different KV-cache sizes.
- **Restarts and crashes.**
  - A supervisor restart, worker crash or mid-turn eviction loses the running turn. It was
    never committed, so nothing is replayed.
  - The session's `busy` flag clears. The app reconnects with backoff and re-attaches the
    session it was showing.
  - Sessions whose host has nothing loaded come back as `NeedsLoad`.
- **Notification routing.**
  - List-level notifications go to **every** connection: `sessions_changed`,
    `templates_changed` and `placement_changed` for any session.
  - Turn notifications go only to the connections attached to that session: `turn_*`,
    `tool_call`, `tool_result`, `loop_done` and `session_loading`.
  - Each connection attaches to at most one session at a time. The app uses one connection,
    shows one session view at a time, and re-attaches when you select another session.
- **The old flow is removed.**
  - The unattached chat flow goes away: chat methods without an attached session fail with
    `no session attached`.
  - The per-connection `RuntimeSession` is replaced by the persisted session.
  - `set_sampling` and the thinking override are **per session** and persisted in the
    session file. The panel shows the session's values.
  - `reset` restores the transcript to just the system message, or to an empty transcript
    when the template has no system prompt, and persists it.
- **What is persisted.**
  - The session file holds `schema: 1`, the snapshot, title, times, sampling/thinking
    overrides and the `ChatMessage` transcript as committed, including any thinking text in
    assistant content.
  - Logprobs are **not** persisted. Timing and token counts **are**: since the agent trace
    (`docs/specs/2026-11-24-agent-trace-design.md`) they live in the session's trace log,
    `{id}.trace.jsonl`, next to the session file. A reloaded session shows its transcript and
    its trace (`get_trace`); the rest of its instrument panel starts empty.
  - `ChatMessage` round-trips through serde. The plan adds `Deserialize` where it is missing
    and a round-trip test.
- **Files and drift.**
  - The backend process is the only writer, and each store serialises its writes behind a
    lock.
  - Hand edits are picked up on the Agents panel's Refresh or on restart.
  - A template file that fails to parse or validate is listed in the tree as a broken entry,
    with its error shown in the editor, which can still fix and save it.
  - A session whose snapshot names a model no host serves shows `Unavailable` with the
    reason.
- **Editor data.**
  - `list_all_tools` returns the whole registry for the tool checkboxes, and `list_tools`
    stays session-scoped.
  - `list_catalog` returns runtimes, models (with which runtimes serve them) and hosts for
    the combos.
  - Both are served on the runtime port, from the in-process catalog and host list. The app
    edits templates without touching the control port.
- **Catalog freshness.** Model discovery is re-run on `list_catalog`, on `list_templates` and
  `save_template` validation, and on `create_session` (which also refreshes the hosts' model
  lists), so a model file dropped into a model directory is offered, accepted and servable
  without a supervisor restart. Runtimes, tools and hosts stay fixed at startup: changing
  them in the catalog needs a supervisor restart.
- **Validation in the tree.** `list_templates` applies the same validation as `save_template`
  (tools, step budget, runtime, model, host), so a template that parses but fails it is listed
  with `error` set to the validation text. `get_template` still returns such a template, with
  `error: string | null` in its result, and fails only when the file does not parse (or its
  id is a duplicate). `create_session` refuses a template that fails validation, with that
  text as the RPC error. The editor shows the real fields and the error; it falls back to the
  defaults (plus the tree node's fields) only when the file does not parse. The New session
  dialog disables Create for a flagged template.
- **Hosts.** Duplicate host names and more than one local host are rejected at startup. A
  template's `host` must name a configured host (`unknown host: {h}`).
- **Concurrent requests.** Requests on one runtime connection run concurrently and responses
  are matched by id: a running turn or model load never holds up the template, session-list
  or catalog methods. Attaching and the chat methods on the attached session (`set_sampling`,
  `set_thinking`, `reset`, `send_turn`, `model_props`, `tokenize`, `list_tools`) are started
  in arrival order, so a `set_sampling` sent before `send_turn` applies to it and an
  `attach_session` sent after `send_turn` does not retarget it. Attaching elsewhere while a
  turn runs leaves the turn to the session's other connections (it aborts if none remain).
- **A turn is bound to its harness.** The harness identity (model, context size and the
  manager's start instance) is recorded with the first step's link. If a later step finds the
  host serving another harness, a reload of the same model included, the turn ends with
  `turn_error {message: "model changed"}` then `loop_done {reason: "error"}`.
- **`updated` means activity.** Only a committed turn, `create_session` and
  `rename_session` move `updated` (and so the list order); sampling and thinking overrides
  and `reset` do not.
- **`send_turn` errors.** When the sender's own session is deleted mid-turn the reply is
  `session deleted`; when the answer was shown but committing it failed, it is
  `answer shown but not saved: {e}`.
- **Runtime discovery.** The supervisor's control plane answers `hello` with
  `{runtime_url, runtime_error}`. `runtime_url` is the bound runtime's websocket whether or not
  a model is loaded; `runtime_error` says why the runtime is off (unusable agents data, a
  failed bind). The app prefers `runtime_url`, then a Ready harness's `chat_url` (an older
  supervisor), then the supervisor host on :8779; while `runtime_error` is set the Sessions
  and Agents panels show it and the runtime socket is not tried.

## Extension points (designed now, built later)

- Remote `supervisor` hosts and `api` hosts behind `ModelHost`.
- Other `PlacementPolicy` implementations: auto-load, load-balancing, fallback chains.
- Template fields for later roadmap steps: compaction settings, planning mode, delegate
  agents. Compaction is now specified in
  `docs/specs/2026-11-24-context-compaction-design.md`: what the model sees may be compacted.
- A coordinator agent calling `create_session` / `send_turn` through tools (step 6).

## Tests (what proves it)

- **Template store:**
  - round-trips TOML
  - the directory tree maps to groups
  - rejects unknown tools, runtime or model
  - seeds the default
- **Session store:**
  - creates with a snapshot, and an edited template does not change it
  - the atomic write survives a reload
  - default titles are numbered
- **Dispatcher with fake hosts:**
  - `status` never loads
  - `NeedsLoad` reports the eviction
  - `send_turn` on `NeedsLoad` loads and then runs
  - the eviction flips other sessions to `NeedsLoad` and raises `placement_changed`
  - an eviction mid-turn ends with `error` and nothing committed
  - the preferred host wins
  - an unserved model gives `Unavailable`
- **Tool loop:**
  - the step budget comes from the snapshot
  - only the subset of tools is sent
  - a call outside the subset is refused
  - the system prompt is rendered in the Gemma system turn (real fixture)
- **Server:**
  - attach/send/notification routing
  - two connections attached to one session both receive notes
  - a busy session is rejected
- **App:**
  - Sessions list states
  - New session dialog creates and opens
  - session view Load button and the first-message load
  - Agents tree CRUD
  - editor validation display
- **Live on the hive:**
  - create a Researcher template with only `read_file` and a system prompt
  - a session from it reads `notes.txt`
  - a second template on gemma-3 shows `NeedsLoad` with "evicts gemma-4"
  - Load swaps the model
  - the first session flips to idle
  - its history is still readable after an app restart

## What's left to the builder

- Template id generation: slug of the name plus a numeric suffix on collision.
- Exact icon for Agents. Use a placeholder SVG in the same style as `RuntimePlay.svg`.
- How the TreeView drag/move is wired, if mural lacks drag-and-drop. "Move to…" is enough.
- Whether `list_templates` returns the full tree or is lazy. The full tree is fine at home-lab
  scale.
- File-watching for hand edits. Not required; a Refresh action in the Agents panel is enough.
