# Tool Calling and the Agent Runtime Component — Design

**Date:** 2026-11-23
**Status:** implemented and live-verified on the hive 2026-11-23 (main @3304a8c6). Live proof: "What time is it?" → get_time → local-time answer; "What does notes.txt in the sandbox say?" → list_dir → read_file → quoted "The harness sandbox works."; candle + gemma-3 reports supports_tools false and plain chat works.

## Intent and why

The harness hosts a model, but the model can only talk. Tool calling lets it act: it emits
a structured request, the host runs it, and the result goes back into the conversation so
the model can continue. This is the first capability that turns the harness from a chat
host into something agents can be built on.

The owner chose the standard architecture (inference layer does format only; an
orchestration layer above it owns the loop and the tools) and asked for the orchestration
layer to be a real **Agent Runtime component** — minimal now, extended later into the full
agentic runtime (agent definitions, approval policy, MCP tool sources, persistence). This
design builds the tool capability and the runtime's skeleton; it does not design agents.

## Goals

- The model in the harness can call tools and use their results, in the existing chat.
- A durable tool layer: one tool contract, a registry, an executor. Built-in tools now;
  other tool sources (MCP) plug into the same contract later.
- The worker stays an inference host: it renders tools into the prompt and parses calls
  out of the stream, and nothing more.
- A new Agent Runtime component owns the transcript and the tool loop, with clear seams
  for the extensions listed below.
- Mechanics stay visible: the raw token stream (including Gemma's tool-call markers) is
  still shown, with structured tool-call and tool-result blocks alongside it.

## Non-goals (this slice)

- Agent definitions (named system prompt + tool set + model), multi-agent, planning.
- Approval UI / human-in-the-loop. The policy seam exists; only read-only tools ship.
- MCP client. The tool-source seam exists; only built-ins ship.
- Conversation persistence across reconnects or supervisor restarts, and conversations
  surviving a model stop/swap (designed for, not enabled — see "Conversation lifetime").
- Tool calling on the candle engine (Gemma 3 has no tool format) or the llama-server
  proxy engine. They keep working as plain chat through the runtime.
- Prompt/KV prefix caching for the now-stateless worker generate path.

## Anti-goals

- Hiding the raw stream or "cleaning up" Gemma's markers. The lab shows mechanics.
- Tools that can touch anything outside a configured sandbox folder.
- Tool or orchestration code inside the engines.
- A bespoke tool format: the contract is the OpenAI `function` shape, which is exactly
  what Gemma 4's template consumes.

## Constraints

- Backend is Rust, all dependencies **vendored** (`backend/vendor/`, offline builds). Any
  new crate must be vendored and committed; prefer no new crates (see Decisions).
- Rust builds and tests run on the hive laptop (`ssh pragmatic-hive`), never the dev box.
- Frontend: mural/Electron TS, house style (Allman braces, no semicolons, PascalCase public
  members, hoisted `private static readonly` literals, VMs extend Observable, OOP, `.mu`
  labels inline). Rust follows idiomatic snake_case + rustfmt.
- Model in use: gemma-4-12B (Q4_0) on a 6GB RTX 3060 via the in-process llamacpp engine.
  Its GGUF chat template natively supports tools (verified, see "Gemma 4 tool format").

## Approaches considered

1. **Loop in the app (TypeScript).** Fastest; approval is just a dialog. But tools run on
   the dev machine and the agent only lives while the window is open — a dead end for the
   headless agents that come next. Rejected.
2. **Loop inside the worker's chat session.** One hop, current chat gets tools at once.
   But it mixes orchestration into the inference host, every engine binary carries tool
   code, and stopping the model kills the conversation. Rejected.
3. **Separate Agent Runtime above the worker (chosen).** Matches the standard split and is
   the natural home for agent definitions later. Costs a new component, a new stateless
   worker method, and an extra hop. Chosen by the owner, built as an extensible component.

## Gemma 4 tool format (verified from the model's own template)

Read from `tokenizer.chat_template` in `gemma-4-12B-it-QAT-Q4_0.gguf`:

- **Declaration (in):** the template takes a `tools` variable — a list of OpenAI-shaped
  `{type: "function", function: {name, description, parameters}}` — and declares them in
  the system turn. It is emitted when `tools` is non-empty, even with no system message.
- **Call (out):** `<|tool_call>call:NAME{key:value,...}<tool_call|>`. **Not JSON**: keys are
  unquoted; strings are delimited by `<|"|>…<|"|>`; numbers, `true`, `false`, `null` are
  literal; objects `{}` and arrays `[]` nest. Several calls may follow one another.
- **Calls back into the prompt:** an assistant message with `tool_calls` (OpenAI shape,
  `function.arguments` must be a **mapping**, not a JSON string — the template raises
  otherwise).
- **Results back into the prompt:** `tool`-role messages with `tool_call_id` (resolved to the
  function name via the preceding assistant `tool_calls`), rendered as
  `<|tool_response>response:NAME{...}<tool_response|>`.
- **Hazard:** right after a tool call, the template's generation prompt opens a
  `<|tool_response>` block. Left alone, the model will start writing a fake result. The
  worker must end generation at the `<|tool_response>` marker.
- Thinking (`<|channel>…<channel|>`) in prior assistant messages is stripped by the template
  itself, so the runtime appends assistant content raw.

## The design

```
App (chat) ──ws :8779──▶ Agent Runtime (new; hosted in the supervisor process on the hive)
                          ├─ Session (one per connection): transcript incl. tool calls/results
                          ├─ ToolLoop: generate → calls? → policy → execute → append → repeat
                          └─ Tool layer: Tool contract · ToolSource(s) · Registry · Executor
                               │ ws :8777  generate {messages, tools, sampling, enable_thinking}
                               ▼
                          Worker (inference only): template render + Gemma call parser
App (instrument panel) ──ws :8777──▶ Worker (unchanged: tokenize, model_props, …)
```

### 1. Worker: inference format layer

- **Message model.** `ChatMessage` grows to the OpenAI-like shape: role
  `system | user | assistant | tool`; `content`; optional `tool_calls`
  (`[{id, type: "function", function: {name, arguments: object}}]`); optional
  `tool_call_id`. Serde-compatible with what the template expects. `ChatTemplate::render`
  passes whole messages (not just role/content) plus a `tools` variable (empty list = no
  tools, which renders exactly as today).
- **Capability.** `model_props` gains `supports_tools: bool`. The llamacpp engine sets it
  when the model's template references `tools` *and* the Gemma call markers; candle and the
  http engine report `false` this slice.
- **New stateless method `generate`** alongside the unchanged `send_turn`:
  params `{messages, tools?, enable_thinking?, sampling?}` (sampling falls back to the
  connection's `set_sampling` value). It renders, applies the same context-window check as
  `send_turn` (emitting `turn_rejected` on overflow), and streams the same notifications:
  `turn_started`, `turn_chunk` (raw text/tokens, markers included), then `turn_done`, which
  now carries `{content, tool_calls}` — the assistant text (without the stop marker) and the
  parsed calls. It also emits one `tool_call {id, name, arguments, raw}` notification as each
  call completes in the stream, so the UI can show it the moment it appears.
- **Gemma call parser** (`GemmaToolFormat`), incremental over the text stream: recognises
  `<|tool_call>call:NAME{…}<tool_call|>`, parses the value grammar above into
  `serde_json::Value`, assigns ids `call_1, call_2, …` per generate. On `<|tool_response>`
  it ends generation by dropping the completion stream (the llamacpp worker thread already
  exits when its receiver is dropped). A call that fails to parse is reported as a call
  with `parse_error` set and its raw text, so the runtime can return the error to the model.
  The format sits behind a `ToolCallFormat` trait so other model families can be added.

### 2. Tool layer (inside the runtime component)

- **Contract.** `trait Tool { fn spec(&self) -> ToolSpec; async fn call(&self, args: Value,
  ctx: &ToolContext) -> ToolOutcome }`. `ToolSpec { name, description, parameters (JSON
  Schema), side_effects: bool }`. `ToolOutcome` is success (a JSON value) or failure (a
  message) — failures are data for the model, never panics or transport errors.
- **Sources.** `trait ToolSource { fn tools(&self) -> Vec<Arc<dyn Tool>> }`.
  `BuiltinSource` now; an `McpSource` later implements the same trait.
- **Registry.** Aggregates sources; lookup by name; rejects duplicate names at build time;
  exports the OpenAI-shaped `tools` list sent to the worker.
- **Executor.** Validates arguments against the schema (required keys + primitive types,
  hand-rolled — no new crate), runs with a per-call timeout (10s), converts every failure
  (unknown tool, bad args, timeout, tool error) into a failure outcome.
- **Policy seam.** `trait ToolPolicy { fn decide(&self, call, spec) -> Decision }` with
  `Allow | Deny(reason) | Ask`. This slice ships `ReadOnlyPolicy`: allow tools without side
  effects, deny the rest. `Ask` is reserved for the approval feature.
- **Built-in tools** (all read-only, run on the hive):
  - `get_time {}` → local date-time with timezone.
  - `list_dir {path}` → entries (name, kind, size) of a folder under the sandbox.
  - `read_file {path, max_bytes?}` → UTF-8 text of a file under the sandbox, capped
    (default and maximum 64 KiB), with a truncation flag.
  Paths are relative to the sandbox root; resolution canonicalises and refuses anything
  that escapes it (`..`, absolute paths, symlinks/junctions pointing outside).

### 3. Agent Runtime component

- **Its own module** (`backend/src/runtime/`), with one public entry point, hosted by the
  supervisor process (always on, already knows the live harness). Not part of any engine
  binary's code path.
- **Session** — one per client connection (same lifetime as today's worker chat session):
  the transcript, sampling config, `enable_thinking`. Lost on disconnect or restart.
- **Worker access.** For each generate step the runtime asks the harness manager for the
  current Ready harness and talks to its worker locally (`ws://127.0.0.1:<worker_port>`).
  No Ready harness → `loop_done{reason: no_harness}`. The session never holds a worker
  connection or any worker identity between steps.
- **Conversation lifetime (owner decision: B now, swap later).** This slice keeps today's
  behaviour: the app opens a chat connection to the runtime when a harness becomes Ready and
  closes it when the harness is cleared, so **every model start is a fresh conversation**.
  The runtime is nevertheless built so that a session already survives a model swap: the
  session is independent of any worker (previous bullet), and `no_harness` is a first-class
  end reason. Enabling "conversation survives swap" later is an app-side change only —
  connect once to the always-on runtime (its url published by the supervisor rather than per
  `HarnessInfo`), keep the connection across stop/start, and add a "New chat" action that
  calls `reset`. Caveat to handle then: a transcript containing tool messages may not render
  on a model without tool support; the runtime will need to drop or flatten them for such
  models.
- **ToolLoop** for one user message:
  1. Append the user message. Send tools only if the worker's `supports_tools` is true.
  2. `generate`; relay every worker notification to the client unchanged.
  3. Append the assistant message (`content` + `tool_calls`).
  4. No calls → done. Otherwise, for each call in order: policy → executor; emit
     `tool_result {id, name, ok, content}`; append a `tool` message.
  5. Repeat from 2, up to a **step cap of 8** generate calls.
  Ends with `loop_done {steps, reason}`, reason ∈ `answered | step_limit | window_full |
  error | no_harness`. A window-full rejection ends the loop (the worker's `turn_rejected`
  is relayed).
- **Transcript commit rule (builder-check call).** A user message's loop is committed
  **all-or-nothing**, mirroring today's worker session: the loop works on a scratch copy and
  the user message plus every step's assistant/tool messages are committed only when the
  loop ends `answered`. Every other end reason (`step_limit`, `window_full`, `error`,
  `no_harness`, dropped connection) leaves the transcript exactly as it was before the user
  message, so the history is always a sequence the template accepts. The UI still showed
  the whole attempt live; `loop_done` tells it the attempt was not kept.
- **Message shapes back into the prompt (builder-check call).** A `tool` message's
  `content` is the outcome as a JSON **value**, not a string: success → the tool's value
  (the template wraps non-objects as `{value:…}`); failure → `{error: "<message>"}`. An
  assistant message with `tool_calls` carries `content` with the call spans removed (text
  before/between calls and any thinking channel kept; the template strips thinking), so the
  calls are never rendered twice. The worker's `turn_done.content` is produced that way; the
  raw markers remain only in the `turn_chunk` stream.
- **Client protocol** — deliberately the worker's chat protocol, so the app's chat client
  barely changes: `model_props` (proxied from the live worker, plus `supports_tools`),
  `tokenize` (proxied), `set_sampling`, `send_turn {content, enable_thinking}` (now runs the
  whole loop and resolves after `loop_done`), `reset` (clear transcript), `list_tools`.
  Notifications: the relayed `turn_*` events per step — **verbatim**, including
  `turn_started.prompt`/fill and `turn_chunk.tokens`/logprobs — plus `tool_call`,
  `tool_result`, `loop_done`.
- **One client, one connection (builder-check call).** The instrument panel today reads the
  chat client's notifications (prompt view, logprobs, context fill) and sends sampling over
  it, so it stays bound to the same client, which now points at the runtime. That is why the
  runtime relays verbatim and proxies `tokenize`/`model_props`: the panel keeps working
  unchanged. The worker `url` in `HarnessInfo` stays for other consumers (tests, debugging)
  but the app no longer uses it.
- **Hosting/config.** Binds `0.0.0.0:8779` by default (`--runtime-bind` /
  `HARNESS_RUNTIME_BIND`). The catalog gains a `[tools]` section with `sandbox = "<path>"`;
  without it, `list_dir`/`read_file` are not registered (`get_time` always is).
  `HarnessInfo` gains `chat_url` (`ws://<advertise_host>:8779`), so the app learns where to
  chat; `url` (the worker) stays for the instrument panel.

### 4. App

- When a harness becomes Ready, the chat connects to `chat_url` (the runtime) instead of
  the worker `url`; the instrument panel keeps using `url`.
- The chat client handles the new notifications. The transcript shows, per loop step: the
  raw stream as today, then a tool-call block (name + arguments), then a tool-result block
  (ok/failed + content), then the next step's stream. Multiple `turn_started…turn_done`
  cycles per user message are expected.
- A small "tools" indicator in the chat shows whether the live model supports tools and
  which tools are available (`list_tools`).

## Resulting behaviour

```
User: what time is it?
  turn_started → turn_chunk… "<|tool_call>call:get_time{}<tool_call|>" → tool_call{get_time}
  → turn_done{tool_calls:[get_time]}            (generation ended at <|tool_response>)
  tool_result{get_time, ok, "2026-11-23T14:05:00+01:00"}
  turn_started → turn_chunk… "It's 14:05." → turn_done{tool_calls:[]}
  loop_done{steps:2, reason:answered}
Model without tool support (candle): plain chat through the runtime, no tools sent.
Sandbox escape ("../.."): read_file returns a failure result; the model sees the error.
Model calls tools forever: stops after 8 generate steps, loop_done{reason:step_limit}.
```

## Extension points (designed now, built later)

- `Ask` policy decision + an approval request/response in the client protocol.
- `McpSource` as another `ToolSource`.
- Agent definitions: a named bundle (system prompt, tool set, policy, preferred model)
  that configures a Session.
- Session persistence (store behind the Session).
- Conversation survives model swap: app-side change only (see "Conversation lifetime").
- Other model tool formats via `ToolCallFormat`.

## Tests

- **Parser:** every value kind, nesting, multiple calls, a call split across chunks,
  malformed call → `parse_error`, stop at `<|tool_response>`. Golden strings produced by
  rendering the real Gemma 4 template.
- **Template:** rendering with `tools`, assistant `tool_calls` and `tool` messages through
  the real template matches the expected Gemma markers; empty tools renders as before. The
  real Gemma 4 template (18.7 KB, extracted from the GGUF header) is committed as a test
  fixture so these tests run without the model.
- **Worker `generate`:** with a fake engine — notifications order, `turn_done` content and
  calls, window-full rejection, stream cut at the response marker.
- **Tool layer:** registry duplicate names, schema validation, timeout, failure-as-result,
  sandbox escapes (`..`, absolute, junction), read cap and truncation flag.
- **Runtime loop:** with a fake worker — answered, multi-call, step limit, window full,
  no harness, unsupported-tools model sends no tools.
- **App (vitest):** chat binds to `chat_url`; tool-call/result notifications render as
  blocks; multi-step turns; `send_turn` resolves after `loop_done`.
- **Live proof on the hive:** gemma-4-12B asked for the time calls `get_time` and answers;
  asked about a sandbox file, it lists/reads it.

## What's left to the builder

- Exact module/file layout inside `backend/src/runtime/` and the worker format code.
- Rendering of tool blocks in `.mu` (reuse existing chat bubble styles).
- Whether `get_time` reports the hive's local zone or UTC alongside (either is fine).
- Hive firewall rule for port 8779 if the dev machine can't reach it.
