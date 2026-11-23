# Tool Calling and the Agent Runtime Component — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The model in the harness can call tools in the existing chat: the worker renders tools and parses Gemma 4 tool calls, and a new Agent Runtime component (hosted by the supervisor) owns the transcript, the tool layer and the tool loop.

**Architecture:** Worker = inference format layer only (new stateless `generate` method + Gemma call-syntax parser). `backend/src/runtime/` = tool layer (contract, sources, registry, policy, executor, built-ins) + session + tool loop + its own JSON-RPC server on `:8779`, speaking the worker's chat protocol plus tool notifications. The app's chat connects to the runtime's `chat_url` instead of the worker.

**Tech Stack:** Rust (tokio, tokio-tungstenite, serde_json, minijinja, async-trait — all already vendored; **no new crates**), Electron + mural TypeScript, vitest.

**Spec:** `docs/specs/2026-11-23-tool-calling-runtime-design.md` — read it first; this plan argues from it.

## Global Constraints

- **No new crates.** Everything must build with `cargo build --offline` from `backend/vendor/`. Do not run `cargo vendor` or touch `Cargo.lock`.
- **Rust builds and tests run on the hive only** (`ssh pragmatic-hive`), via `scripts/hive-test.sh` (Task 1). Never `cargo` on the dev machine. The app (Node/vitest) runs on the dev machine: `cd app && npm test` and `npm run typecheck:web`.
- Rust: idiomatic snake_case + rustfmt; helper logic as associated functions on types (no free-standing module functions in new code).
- TypeScript: Allman braces, no semicolons, PascalCase public members and interfaces, every literal hoisted to a `private static readonly` constant, view models extend `Observable`, OOP only. `.mu` labels stay inline.
- Gemma 4 call format (exact): `<|tool_call>call:NAME{key:value,...}<tool_call|>`; strings delimited by `<|"|>`; tool-response opener `<|tool_response>`; response block `<|tool_response>response:NAME{...}<tool_response|>`.
- Runtime values: step cap **8** generate calls; per-call tool timeout **10 s**; `read_file` cap **65536** bytes (default and maximum); runtime bind default **`0.0.0.0:8779`** (`--runtime-bind` / `HARNESS_RUNTIME_BIND`); `loop_done.reason` ∈ `answered | step_limit | window_full | error | no_harness`.
- Commits: author `Eugene Napryaglo <napryaglo@users.noreply.github.com>`; message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. **Never push** without the owner's explicit go (Task 10).
- Hive ssh pitfalls: `$` and nested quotes are mangled over ssh — write `.cmd`/`.ps1` files locally (CRLF), `scp` them, run with `powershell -NoProfile -ExecutionPolicy Bypass -File …`. Never wrap `git` in `$ErrorActionPreference='Stop'` (git writes progress to stderr).

## Review Focus

1. A call marker or a `<|"|>` delimiter split across stream chunks at any byte boundary → parsed exactly as if unsplit (Task 2: `parses_when_fed_one_char_at_a_time`).
2. The model writes `<|tool_response>` itself (with or without a preceding call) → generation ends there and nothing after it is relayed or kept (Task 3: `generate_stops_at_response_marker_without_call`).
3. String arguments containing `{ } , : [ ]` or `"` inside `<|"|>…<|"|>` → kept literally (Task 2: `string_values_keep_structural_characters`).
4. The harness stops mid-generate (worker socket closes without `turn_done`) → `turn_error` relayed, `loop_done{reason:"error"}`, transcript untouched, the app's send resolves (Task 7: `stream_ending_without_done_is_error`; Task 9: `send_resolves_after_loop_done_not_turn_done`).
5. `read_file` truncation lands inside a multi-byte UTF-8 character → content is valid UTF-8, shorter than the cap, `truncated: true` (Task 5: `read_file_truncates_on_char_boundary`).

---

### Task 1: Message model, tools in the template, hive test script

**Files:**
- Create: `scripts/hive-test.sh`
- Create: `backend/tests/fixtures/gemma4-chat-template.jinja` (copied, not written)
- Modify: `backend/src/engine/types.rs`, `backend/src/engine/template.rs`, `backend/src/engine/mod.rs`, `backend/src/engine/{llamacpp,candle,http}.rs`, `backend/src/session.rs` (FakeEngine + literals)

**Interfaces:**
- Produces (types.rs): `Role::Tool` (serde `"tool"`); `ToolCall { id: String, #[serde(rename = "type")] kind: String, function: FunctionCall }`; `FunctionCall { name: String, arguments: serde_json::Value }`; `ChatMessage { role, content: String, tool_calls: Option<Vec<ToolCall>>, tool_call_id: Option<String>, tool_result: Option<Value> }` (Option fields `#[serde(default, skip_serializing_if = "Option::is_none")]`); `ChatMessage::new(role: Role, content: impl Into<String>) -> Self`; `ChatMessage::template_view(&self) -> serde_json::Value` (`{role, content, tool_calls?, tool_call_id?}` where `content` is `tool_result` for `Role::Tool`).
- Produces (template.rs): `ChatTemplate::render(&self, messages: &[ChatMessage], add_generation_prompt: bool, enable_thinking: bool, tools: &[Value]) -> Result<String, TemplateError>`.
- Produces (mod.rs): `Engine::apply_template(&self, messages, add_generation_prompt, enable_thinking, tools: &[Value]) -> Result<String>`; `Engine::supports_tools(&self) -> bool` with default `false`.

- [ ] **Step 1: Create `scripts/hive-test.sh`.** Usage: `scripts/hive-test.sh [--features F] [--restore] [-- <cargo test args>]`. It tars `backend/src backend/tests backend/Cargo.toml` from the working tree, pipes it over `ssh pragmatic-hive "tar -xf - -C C:/llm-harness/local-llm-harness"` (if stdin piping through ssh misbehaves on the Windows side, scp the tar file and extract it there instead), then scp's and runs a generated `.cmd` that calls `vcvars64.bat`, adds the CUDA env lines from the hive's `C:\llm-harness\harness-build\build-llamacpp.cmd` when `F` contains `llamacpp` or `candle-cuda`, and runs `cargo test --release --offline --lib [--features F] <args>` in `backend`, printing the result summary. `--restore` runs `git checkout -- backend` plus `git clean -fd backend/src backend/tests` in the hive clone, so the clone matches origin again.

- [ ] **Step 2: Copy the real template fixture.** `scp pragmatic-hive:C:/llm-harness/harness-build/gemma4-template.jinja backend/tests/fixtures/gemma4-chat-template.jinja` (extracted from `gemma-4-12B-it-QAT-Q4_0.gguf`'s `tokenizer.chat_template`, 18 682 bytes; verify the size after copying). Add `backend/tests/fixtures/*.jinja -text` to `backend/.gitattributes` so line endings are never rewritten.

- [ ] **Step 3: Write failing tests in `template.rs`** (load the fixture with `include_str!("../../tests/fixtures/gemma4-chat-template.jinja")`):

```rust
#[test] fn gemma4_declares_tools_in_system_turn() {
    // tools = [{"type":"function","function":{"name":"get_time","description":"Current time","parameters":{"type":"object","properties":{}}}}]
    // messages = [user "what time is it?"]
    assert!(prompt.contains("declaration:get_time{description:<|\"|>Current time<|\"|>"));
}
#[test] fn gemma4_renders_assistant_tool_calls_and_tool_results() {
    // messages = [user "q",
    //   assistant{content:"", tool_calls:[{id:"call_1",type:"function",function:{name:"get_time",arguments:{}}}]},
    //   tool{tool_call_id:"call_1", tool_result:{"utc":"2026-11-23T13:05:00Z"}}]
    assert!(prompt.contains("<|tool_call>call:get_time{}<tool_call|>"));
    assert!(prompt.contains("<|tool_response>response:get_time{utc:<|\"|>2026-11-23T13:05:00Z<|\"|>}<tool_response|>"));
}
#[test] fn gemma4_without_tools_has_no_declarations() {
    assert!(!prompt.contains("declaration:"));
}
```

- [ ] **Step 4: Run them on the hive, expect a compile failure** (`render` has no `tools` parameter): `scripts/hive-test.sh -- engine::template`.

- [ ] **Step 5: Implement the types, `render`, and the trait change.** `render` maps every message through `template_view()` into `minijinja::Value::from_serialize` and adds `tools => tools` to the context. Every `ChatMessage { role, content }` literal becomes `ChatMessage::new(..)`. `apply_template` forwards `tools`: llamacpp and candle forward them to `ChatTemplate::render`. The http engine returns `EngineError::Protocol("tools are not supported by the http engine")` when `tools` is non-empty, and otherwise behaves as before. `ChatSession` passes `&[]`.

- [ ] **Step 6: Run the template tests, then the whole lib with no features, expecting PASS:** `scripts/hive-test.sh -- engine::template` and `scripts/hive-test.sh`.

- [ ] **Step 7: Commit:** `feat(worker): tool-aware chat messages and tools in the chat template`.

### Task 2: Gemma 4 tool-call parser

**Files:**
- Create: `backend/src/engine/tool_format.rs` (registered in `engine/mod.rs`)

**Interfaces:**
- Produces: `ParsedCall { id: String, name: String, arguments: Value, raw: String, parse_error: Option<String> }` (Serialize, `parse_error` skipped when `None`). `ParseEvent::{Call(ParsedCall), ResponseMarker}`. `GemmaToolFormat::template_supports(src: &str) -> bool` (true iff `src` contains both `tools` and `<|tool_call>`). `GemmaToolFormat::parse_value(text: &str) -> Result<Value, String>`. `GemmaToolFormat::parse_call(body: &str) -> Result<(String, Value), String>`, where `body` is the text between the markers (`call:NAME{…}`). `GemmaToolParser::new() -> Self`, `push(&mut self, text: &str) -> Vec<ParseEvent>`, `content(&self) -> String` (text outside call spans; nothing at or after `<|tool_response>`), `calls(&self) -> Vec<ParsedCall>`, `finished(&self) -> bool` (true once the response marker has been seen).

Value grammar (whitespace allowed between tokens): string = `<|"|>`…`<|"|>` (non-greedy); `true` / `false` / `null`; number = JSON number text (an `i64` when integral, otherwise `f64`); object = `{` [key `:` value (`,` key `:` value)*] `}`, where a key is either `<|"|>…<|"|>` or bare text up to `:` (trimmed); array = `[` [value (`,` value)*] `]`. Anything else is an error. Call ids are `call_1`, `call_2`, … per parser instance.

- [ ] **Step 1: Write failing tests:**

```rust
parse_value_covers_every_kind      // `{a:1,b:2.5,c:true,d:null,e:<|"|>x<|"|>,f:[1,<|"|>y<|"|>],g:{h:false}}` == json!({"a":1,"b":2.5,"c":true,"d":null,"e":"x","f":[1,"y"],"g":{"h":false}})
string_values_keep_structural_characters // `{q:<|"|>a{b},c:d[e]"f<|"|>}` == json!({"q":"a{b},c:d[e]\"f"})
parse_call_reads_name_and_args     // "call:read_file{max_bytes:100,path:<|\"|>notes.txt<|\"|>}" == ("read_file", json!({"max_bytes":100,"path":"notes.txt"}))
parse_call_with_no_args            // "call:get_time{}" == ("get_time", json!({}))
parser_strips_call_from_content    // push("Let me check.<|tool_call>call:get_time{}<tool_call|>") → one Call{id:"call_1",name:"get_time"}; content()=="Let me check."
parser_two_calls_get_sequential_ids
parses_when_fed_one_char_at_a_time // the read_file call string above, pushed 1 char per push → same ParsedCall as pushing it whole
malformed_call_reports_parse_error // "<|tool_call>call:x{a:}<tool_call|>" → Call with parse_error Some(_), raw == "call:x{a:}"
response_marker_ends_parsing       // push("<|tool_call>call:get_time{}<tool_call|><|tool_response>response:get_time{}") → [Call, ResponseMarker]; finished(); further push returns []; content()==""
template_supports_detects_gemma4   // true for the fixture; false for "{% for m in messages %}{{m.content}}{% endfor %}"
```

- [ ] **Step 2: Run, expecting FAIL (missing module):** `scripts/hive-test.sh -- engine::tool_format`.
- [ ] **Step 3: Implement** `GemmaToolFormat` and `GemmaToolParser`. The parser buffers any trailing text that could be the start of `<|tool_call>`, `<tool_call|>` or `<|tool_response>`, and holds it back until it can decide.
- [ ] **Step 4: Run, expecting PASS.**
- [ ] **Step 5: Commit:** `feat(worker): Gemma 4 tool-call syntax parser`.

### Task 3: Worker `generate` method and tool capability

**Files:**
- Modify: `backend/src/rpc.rs`, `backend/src/session.rs`, `backend/src/engine/llamacpp.rs`

**Interfaces:**
- Consumes: Task 1 `apply_template(.., tools)` and `supports_tools()`; Task 2 `GemmaToolParser`, `GemmaToolFormat::template_supports`.
- Produces (session.rs): `ContextFill::check(used: usize, total: usize, max_tokens: i32) -> Result<ContextFill, (ContextFill, TurnRejected)>`, extracted from `begin_turn`'s window rule (`needed = used + max(max_tokens, 0)`; reject when `needed > total`), which `begin_turn` now calls.
- Produces (wire): request `generate {messages: [ChatMessage], tools?: [Value], enable_thinking?: bool, sampling?: SamplingConfig}`. Notifications: `turn_started {prompt, prompt_tokens, n_ctx}`, then `turn_chunk {text, tokens, stop}`*, with `tool_call {id, name, arguments, raw, parse_error?}` sent as soon as each call completes. The stream ends with `turn_done {content: String, tool_calls: [ParsedCall]}` | `turn_rejected {needed, total}` | `turn_error {message}`. The response `null` comes last. `model_props` gains `supports_tools: bool`.
- Produces (llamacpp): `LocalLlamaEngine` computes `supports_tools` once in `load` via `GemmaToolFormat::template_supports` on the model's chat template.

- [ ] **Step 1: Write failing tests in `rpc.rs`.** Add a test module with a scripted `FakeEngine`: `apply_template` records the `tools` it receives and returns `"PROMPT"`; `tokenize` returns a configurable count; `complete` yields scripted chunks; `supports_tools` is configurable. Run `serve` on `127.0.0.1:0` and talk to it with `tokio_tungstenite::connect_async`.

```rust
generate_streams_chunks_then_done_with_content // chunks ["Hello"," world"] → methods [turn_started, turn_chunk, turn_chunk, turn_done]; turn_done.content=="Hello world"; tool_calls==[]
generate_emits_tool_call_and_stops_at_response_marker // chunks ["Checking.","<|tool_call>call:get_time{}","<tool_call|>","<|tool_response>","fake"] → a tool_call note {id:"call_1",name:"get_time",arguments:{}}; exactly 4 turn_chunk notes; turn_done.content=="Checking."; turn_done.tool_calls[0].name=="get_time"
generate_stops_at_response_marker_without_call // chunks ["Hi","<|tool_response>","response:x{}"] → 2 turn_chunk notes; turn_done.content=="Hi"; tool_calls==[]
generate_rejects_when_window_full              // tokenize count 9000, n_ctx 8192 → [turn_started, turn_rejected{needed:9000,total:8192}]
generate_passes_tools_to_template              // the tools sent == the tools the FakeEngine recorded
model_props_reports_supports_tools             // supports_tools true → result.supports_tools == true
```

- [ ] **Step 2: Run, expecting FAIL:** `scripts/hive-test.sh -- rpc`.
- [ ] **Step 3: Implement** `generate` in `Connection::dispatch`. Use `sampling` from the params, falling back to the connection's `set_sampling` value. Render, tokenize, run `ContextFill::check`, then `complete`. Relay each chunk as `turn_chunk`, feed the parser, and send `tool_call` for each `Call`. On `ResponseMarker`, drop the stream (this stops the engine). Finish with `turn_done {content, tool_calls}`. Leave `send_turn` behaviour unchanged.
- [ ] **Step 4: Run, expecting PASS** (both `scripts/hive-test.sh -- rpc` and the whole lib). Also run `scripts/hive-test.sh --features llamacpp -- engine` to compile and test the engine with the feature on.
- [ ] **Step 5: Commit:** `feat(worker): stateless generate with tool-call parsing`.

### Task 4: Tool layer — contract, registry, policy, executor

**Files:**
- Create: `backend/src/runtime/mod.rs`, `backend/src/runtime/tool.rs`, `backend/src/runtime/registry.rs`, `backend/src/runtime/policy.rs`, `backend/src/runtime/executor.rs`; add `pub mod runtime;` to `backend/src/lib.rs`

**Interfaces:**
- Produces (tool.rs):
  - `#[async_trait] trait Tool: Send + Sync { fn spec(&self) -> ToolSpec; async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutcome; }`
  - `ToolSpec { name, description, parameters: Value, side_effects: bool }` (Serialize, Clone)
  - `ToolContext { call_id: String }`
  - `enum ToolOutcome { Ok(Value), Err(String) }`, with `ToolOutcome::result_value(&self) -> Value` (returns `v` for `Ok`, `{"error": msg}` for `Err`) and `is_ok()`
  - `trait ToolSource: Send + Sync { fn tools(&self) -> Vec<Arc<dyn Tool>>; }`
  - `ToolCallRequest { id: String, name: String, arguments: Value, parse_error: Option<String> }`
- Produces (registry.rs): `ToolRegistry::new(sources: Vec<Box<dyn ToolSource>>) -> Result<Self, RegistryError>` (`RegistryError::Duplicate(String)`); `get(&self, name) -> Option<Arc<dyn Tool>>`; `specs(&self) -> Vec<ToolSpec>` (in source order); `openai_tools(&self) -> Vec<Value>`, where each entry is `{"type":"function","function":{"name","description","parameters"}}`.
- Produces (policy.rs): `enum Decision { Allow, Deny(String), Ask }`; `trait ToolPolicy: Send + Sync { fn decide(&self, spec: &ToolSpec, args: &Value) -> Decision; }`; `ReadOnlyPolicy`.
- Produces (executor.rs): `SchemaCheck::validate(schema: &Value, args: &Value) -> Result<(), String>`; `ToolExecutor::new(registry: Arc<ToolRegistry>, policy: Box<dyn ToolPolicy>, timeout: Duration)`; `async fn execute(&self, call: &ToolCallRequest) -> ToolOutcome`.

Executor order and exact failure messages:
1. `parse_error` set → `could not parse tool call: {e}`
2. Unknown tool → `unknown tool: {name}`
3. Policy `Deny(r)` → `r`; `Ask` → `approval required (not supported yet)`
4. Validation fails → `invalid arguments: {e}`
5. Timeout → `tool timed out after 10s` (rendered from the configured timeout)
6. The tool's own `Err` is passed through unchanged.

`ReadOnlyPolicy` denies side-effecting tools with `tool has side effects; not allowed by the read-only policy`.

`SchemaCheck` rules: `args` must be an object. Every `required` key must be present. Every present key that has a `properties.<key>.type` must match it (`string`, `integer` means i64/u64, `number`, `boolean`, `object`, `array`). Extra keys are allowed.

- [ ] **Step 1: Write failing tests** (test-only `EchoTool` returns `Ok(args)`; `SlowTool` sleeps 60 s; `WriterTool` has `side_effects: true`):

```rust
registry_rejects_duplicate_names           // two sources both providing "echo" → Err(Duplicate("echo"))
registry_exports_openai_tool_shape         // openai_tools()[0] == json!({"type":"function","function":{"name":"echo","description":..,"parameters":..}})
schema_requires_required_keys              // schema {required:["text"]}, args {} → Err containing "text"
schema_checks_primitive_types              // {properties:{n:{type:"integer"}}}, args {n:"1"} → Err; {n:1} → Ok
executor_reports_unknown_tool              // name "nope" → Err("unknown tool: nope")
executor_reports_parse_error               // parse_error "bad" → Err("could not parse tool call: bad")
executor_denies_side_effects_under_read_only // WriterTool → Err("tool has side effects; not allowed by the read-only policy")
executor_times_out                         // SlowTool with timeout 50ms → Err("tool timed out after 0.05s"); the text is format!("tool timed out after {}s", timeout.as_secs_f64()), so 10 s reads "tool timed out after 10s"
executor_runs_valid_call                   // EchoTool {text:"x"} → Ok({"text":"x"})
outcome_result_value_wraps_errors          // Err("boom").result_value() == json!({"error":"boom"})
```

- [ ] **Step 2: Run, expecting FAIL:** `scripts/hive-test.sh -- runtime`.
- [ ] **Step 3: Implement** the four files.
- [ ] **Step 4: Run, expecting PASS.**
- [ ] **Step 5: Commit:** `feat(runtime): tool contract, registry, read-only policy, executor`.

### Task 5: Built-in tools and the sandbox

**Files:**
- Create: `backend/src/runtime/builtin.rs`

**Interfaces:**
- Consumes: Task 4 `Tool`, `ToolSource`, `ToolSpec`, `ToolOutcome`.
- Produces: `Sandbox::new(root: &Path) -> std::io::Result<Self>` (canonicalises the root); `Sandbox::resolve(&self, rel: &str) -> Result<PathBuf, String>`; `CivilTime::utc_iso8601(unix_secs: u64) -> String` (`YYYY-MM-DDTHH:MM:SSZ`, using the days-to-civil algorithm, no crates); `BuiltinSource::new(sandbox: Option<Sandbox>)`, which always provides `get_time` and adds `list_dir` and `read_file` only when it has a sandbox.

Tool shapes (all `side_effects: false`):
- `get_time {}` returns `{"utc": "<iso>", "unix": <secs>}`. UTC only — the spec leaves the zone to the builder.
- `list_dir {path?: string = "."}` returns `{"path", "entries": [{"name", "kind": "file"|"dir"|"other", "size"}]}`, sorted by name, at most 500 entries, plus `"truncated": bool`.
- `read_file {path: string (required), max_bytes?: integer}` returns `{"path", "content", "bytes", "truncated"}`. The cap is `min(max_bytes, 65536)`, default 65536. A truncation that lands mid-character backs off to the last full character. Non-UTF-8 content fails with `file is not valid UTF-8 text`.

`resolve` rules:
- An absolute path, or one with a drive or root prefix, fails with `path must be relative to the sandbox`.
- The path is joined to the root and canonicalised; a missing path fails with `no such path: {rel}`.
- A result that does not start with the canonical root fails with `path escapes the sandbox`. This catches `..` and also symlinks or junctions that point outside.

- [ ] **Step 1: Write failing tests** (use a fresh temp dir per test as the sandbox):

```rust
civil_time_formats_known_instants   // 0 → "1970-01-01T00:00:00Z"; 951782400 → "2000-02-29T00:00:00Z"; 1700000000 → "2023-11-14T22:13:20Z"
get_time_returns_utc_and_unix
list_dir_lists_sorted_entries       // files b.txt, a.txt, dir sub → names ["a.txt","b.txt","sub"], kinds file,file,dir
read_file_returns_text
read_file_truncates_on_char_boundary // file "aé" (3 bytes), max_bytes 2 → content "a", bytes 1, truncated true
read_file_rejects_non_utf8          // bytes [0xff,0xfe] → Err("file is not valid UTF-8 text")
resolve_rejects_parent_escape       // "../x" → Err("path escapes the sandbox") (create the target outside so canonicalize succeeds)
resolve_rejects_absolute            // "C:\\Windows" → Err("path must be relative to the sandbox")
#[cfg(windows)] resolve_rejects_junction_escape // `cmd /c mklink /J <sandbox>\link <outside>` then "link" → Err("path escapes the sandbox")
source_without_sandbox_has_only_get_time
```

- [ ] **Step 2: Run, expecting FAIL:** `scripts/hive-test.sh -- runtime::builtin`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run, expecting PASS.**
- [ ] **Step 5: Commit:** `feat(runtime): sandboxed read-only built-in tools`.

### Task 6: Worker link and connector

**Files:**
- Create: `backend/src/runtime/worker.rs`
- Modify: `backend/src/supervisor/manager.rs` (add `ready_worker_url`)

**Interfaces:**
- Produces:
  - `Note { method: String, params: Value }`
  - `GenerateRequest { messages: Vec<ChatMessage>, tools: Vec<Value>, enable_thinking: bool, sampling: SamplingConfig }` (Serialize, matching Task 3's wire params)
  - `LinkError(String)`
  - `#[async_trait] trait WorkerLink: Send + Sync`, with methods:
    - `async fn model_props(&self) -> Result<Value, LinkError>`
    - `async fn tokenize(&self, text: &str) -> Result<Value, LinkError>`
    - `async fn generate(&self, req: &GenerateRequest) -> Result<BoxStream<'static, Result<Note, LinkError>>, LinkError>` — the stream yields every notification and ends when the response frame arrives. An error frame becomes `Err(LinkError)`. A socket closed before the response simply ends the stream.
  - `WsWorkerLink::new(url: String)`: one websocket connection per call, request id `1`.
  - `trait WorkerConnector: Send + Sync { fn connect(&self) -> Option<Arc<dyn WorkerLink>>; }`
  - `trait ReadyWorkerUrl: Send + Sync { fn ready_worker_url(&self) -> Option<String>; }`
  - `SupervisorConnector::new(source: Arc<dyn ReadyWorkerUrl>)`, implementing `WorkerConnector` with `WsWorkerLink`.
  - `HarnessManager::ready_worker_url(&self) -> Option<String>`: `ws://127.0.0.1:{catalog.worker_port}` when the current harness is `Ready`, otherwise `None`. Also implement `ReadyWorkerUrl` for `HarnessManager<L, P>`.

- [ ] **Step 1: Write failing tests.** Run the real worker `rpc::serve` with Task 3's scripted `FakeEngine` (make that fake `pub(crate)` under `#[cfg(test)]` so it can be reused) on `127.0.0.1:0`:

```rust
ws_link_generate_yields_notes_in_order // methods == ["turn_started","turn_chunk","turn_done"]
ws_link_model_props_round_trips        // contains "supports_tools"
ready_worker_url_only_when_ready       // manager test: before start → None; after a Ready start → Some("ws://127.0.0.1:8777")
connector_returns_none_without_ready_worker
```

- [ ] **Step 2: Run, expecting FAIL.** **Step 3: Implement.** **Step 4: Run, expecting PASS:** `scripts/hive-test.sh -- runtime::worker supervisor::manager`.
- [ ] **Step 5: Commit:** `feat(runtime): worker link and supervisor connector`.

### Task 7: Runtime session and tool loop

**Files:**
- Create: `backend/src/runtime/session.rs`, `backend/src/runtime/tool_loop.rs`

**Interfaces:**
- Consumes: Task 4 `ToolExecutor`, `ToolRegistry`, `ToolCallRequest`, `ToolOutcome::result_value`; Task 6 `WorkerConnector`, `WorkerLink`, `GenerateRequest`, `Note`; Task 2 `ParsedCall` (deserialized from `turn_done.tool_calls`).
- Produces:
  - `RuntimeSession { transcript: Vec<ChatMessage>, sampling: SamplingConfig }` with `new()`, `reset()` and `transcript()`.
  - `#[async_trait] trait LoopSink: Send { async fn notify(&mut self, method: &str, params: Value) -> Result<(), SinkClosed>; }`
  - `EndReason::{Answered, StepLimit, WindowFull, Error, NoHarness}`, serialized as snake_case.
  - `LoopEnd { steps: usize, reason: EndReason }`
  - `ToolLoop::new(executor: Arc<ToolExecutor>, registry: Arc<ToolRegistry>, step_cap: usize)`
  - `async fn run(&self, session: &mut RuntimeSession, content: &str, enable_thinking: bool, connector: &dyn WorkerConnector, sink: &mut dyn LoopSink) -> Result<LoopEnd, SinkClosed>`

`run` follows the spec ("ToolLoop", "Transcript commit rule", "Message shapes"):
1. Work on a scratch copy: `session.transcript` plus the user message. Connect; with no link the reason is `NoHarness`. Read `model_props().supports_tools`; when it's false, send `tools = []`.
2. Each step calls `generate` and relays every `Note` verbatim. `turn_done` ends the step; `turn_rejected` gives `WindowFull`; `turn_error` gives `Error`. A link error, or a stream that ends without one of those three, gives `Error` — first send `turn_error {message: "worker stream ended unexpectedly"}`, or the link error's text.
3. Append the assistant message: `content`, plus `tool_calls` when non-empty. The arguments come from each `ParsedCall`; use `{}` when the arguments are not an object or there was a parse error.
4. With no calls the reason is `Answered`. When this was the `step_cap`-th step, the reason is `StepLimit` and the calls are not executed.
5. Otherwise execute each call in order, send `tool_result {id, name, ok, content: result_value}`, and append `ChatMessage { role: Tool, tool_call_id, tool_result: result_value }`.

Always finish with `loop_done {steps, reason}`. Write the scratch transcript back to `session.transcript` **only** for `Answered`.

- [ ] **Step 1: Write failing tests.** A `FakeConnector` returns an optional `FakeLink`; the `FakeLink` returns scripted note lists per `generate` call and records the requests. `RecordingSink` captures notes. The registry holds Task 4's `EchoTool`.

```rust
answered_without_tools          // [turn_started, turn_chunk, turn_done{content:"hi",tool_calls:[]}] → LoopEnd{1,Answered}; transcript [user "q", assistant "hi"]; last note loop_done{steps:1,reason:"answered"}
executes_call_then_answers      // step1 done{content:"",tool_calls:[{id:"call_1",name:"echo",arguments:{"text":"x"}}]}, step2 done{content:"done"} → tool_result{id:"call_1",name:"echo",ok:true,content:{"text":"x"}} between the steps; transcript [user, assistant{tool_calls:[call_1]}, tool{tool_call_id:"call_1",tool_result:{"text":"x"}}, assistant "done"]; 2nd request has 3 messages
unknown_tool_result_is_error    // tool_result ok:false content {"error":"unknown tool: nope"}
parse_error_result_is_error     // parse_error "bad" → content {"error":"could not parse tool call: bad"}
stops_at_step_limit             // every step returns a call → 8 requests; LoopEnd{8,StepLimit}; no tool_result after step 8; transcript unchanged
window_full_rolls_back          // [turn_started, turn_rejected{needed:9,total:8}] → WindowFull; transcript unchanged; loop_done reason "window_full"
no_harness                      // connector None → LoopEnd{0,NoHarness}; only note: loop_done{steps:0,reason:"no_harness"}
stream_ending_without_done_is_error // [turn_started, turn_chunk] then end → a turn_error note; Error; transcript unchanged
tools_sent_only_when_supported  // supports_tools false → request.tools empty; true → == registry.openai_tools()
sampling_and_thinking_forwarded
```

- [ ] **Step 2: Run, expecting FAIL.** **Step 3: Implement.** **Step 4: Run, expecting PASS:** `scripts/hive-test.sh -- runtime`.
- [ ] **Step 5: Commit:** `feat(runtime): session and tool loop`.

### Task 8: Runtime server and supervisor wiring

**Files:**
- Create: `backend/src/runtime/server.rs`
- Modify: `backend/src/main.rs`, `backend/src/supervisor/catalog.rs`, `backend/src/supervisor/manager.rs`, `backend/src/supervisor/control.rs` (test literals)

**Interfaces:**
- Consumes: Tasks 4–7.
- Produces:
  - `RuntimeDeps { connector: Arc<dyn WorkerConnector>, registry: Arc<ToolRegistry>, tool_loop: Arc<ToolLoop> }`.
  - `RuntimeServer::serve(listener: TcpListener, deps: RuntimeDeps)` — gives each connection its own `RuntimeSession`, using the same JSON-RPC envelope as `rpc.rs`. Make `rpc.rs`'s `RpcError`, error codes and frame helpers `pub(crate)` and reuse them rather than duplicating.
  - The unit-tested seam `RuntimeConnection::dispatch(&mut self, method: &str, params: Value, sink: &mut dyn LoopSink) -> Result<Value, RpcError>`.
  - Methods:
    - `model_props`: the worker's props verbatim; with no harness, the error `no model running`.
    - `tokenize`: proxied.
    - `set_sampling {config}`.
    - `send_turn {content, enable_thinking}`: runs the loop; the result `null` is sent after `loop_done`.
    - `reset`.
    - `list_tools`: returns `[ToolSpec]`.
- Produces (supervisor):
  - `ToolsConfig { sandbox: PathBuf }` and `Catalog.tools: Option<ToolsConfig>` (`#[serde(default)]`).
  - `ManagerConfig.chat_port: Option<u16>` (default `None`).
  - `HarnessInfo.chat_url: Option<String>`, set to `ws://{advertise_host}:{chat_port}` alongside `url` when the harness is Ready (`skip_serializing_if = "Option::is_none"`).
  - `Config.runtime_bind: String` (`--runtime-bind`, env `HARNESS_RUNTIME_BIND`, default `0.0.0.0:8779`).

In `run_supervisor`:
1. Build `BuiltinSource` with `Sandbox::new(catalog.tools.sandbox)`. If that fails, print `tools sandbox unavailable: {e}` and continue with `None`.
2. Build the registry and `ToolExecutor(ReadOnlyPolicy, 10 s)`, then `ToolLoop(step_cap 8)`.
3. Keep one `Arc` of the manager for both `control::serve` and `SupervisorConnector`.
4. Set `chat_port` from the runtime bind's port.
5. Bind the runtime listener, print `runtime listening on ws://{addr} ({n} tools)`, and `tokio::spawn` the runtime server before `control::serve`.

- [ ] **Step 1: Write failing tests:**

```rust
// runtime::server (FakeConnector/FakeLink from Task 7)
list_tools_returns_specs
set_sampling_is_used_by_send_turn          // the GenerateRequest.sampling equals the config set
reset_clears_transcript
model_props_without_harness_errors          // Err message "no model running"
send_turn_result_comes_after_loop_done      // sink's last note is loop_done before dispatch returns
// supervisor
catalog_parses_tools_sandbox                // `[tools]\nsandbox = "C:/llm-harness/sandbox"` → Some(ToolsConfig{sandbox})
catalog_without_tools_section_is_none
ready_info_carries_chat_url                 // chat_port Some(8779) → info.chat_url == Some("ws://192.168.1.101:8779")
```

- [ ] **Step 2: Run, expecting FAIL.** **Step 3: Implement.** Update every `ManagerConfig {..}`, `HarnessInfo {..}` and `Catalog {..}` literal in tests.
- [ ] **Step 4: Run, expecting PASS:** `scripts/hive-test.sh` (whole lib), then `scripts/hive-test.sh --features llamacpp` (whole lib with the engine).
- [ ] **Step 5: Commit:** `feat(supervisor): host the agent runtime and publish chat_url`.

### Task 9: App — chat through the runtime, tool blocks, tools indicator

**Files:**
- Modify: `app/src/renderer/src/SupervisorClient.ts`, `HarnessPickerViewModel.ts`, `HarnessClient.ts`, `messages.ts`, `ChatViewModel.ts`, `views/Chat.mu`
- Test: `SupervisorClient.test.ts`, `HarnessPickerViewModel.test.ts`, `HarnessClient.test.ts`, `ChatViewModel.test.ts`

**Interfaces:**
- Produces:
  - `HarnessInfo.chat_url?: string`. The picker's `OnReady` raises `info.chat_url ?? info.url`.
  - `HarnessClient`:
    - signals `OnToolCall: TurnSignal<ToolCallNote>`, `OnToolResult: TurnSignal<ToolResultNote>`, `OnLoopDone: TurnSignal<LoopDone>`
    - `ListTools(): Promise<ToolInfo[]>` (method `list_tools`)
    - `ModelProps` type gains `supports_tools?: boolean`
    - types:
      - `ToolCallNote { id: string, name: string, arguments: unknown, raw: string, parse_error?: string }`
      - `ToolResultNote { id: string, name: string, ok: boolean, content: unknown }`
      - `LoopDone { steps: number, reason: string }`
      - `ToolInfo { name: string, description: string, side_effects: boolean }`
  - `messages.ts`:
    - `ToolCallMessage(name: string, argumentsText: string)`, with `Name` and `Arguments`
    - `ToolResultMessage(name: string, ok: boolean, contentText: string)`, with `Name`, `Ok`, `Content` and `StatusText` (`'ok'` / `'failed'`)
  - `ChatViewModel.ToolsText: string`:
    - `'tools: get_time, list_dir, read_file'` when `supports_tools` is true and `ListTools` is non-empty
    - otherwise, and on any failure, `'tools: off'`

`ChatViewModel` turn handling replaces the "first `turn_done` ends the turn" rule:
- A private `busy` flag is set in `Send` and cleared only when the `SendTurn` promise settles. It is the guard for `Send`.
- `segment` is the live `AssistantMessage`:
  - `Send` creates the first one, as today.
  - On `turn_started` while busy with no `segment`, a new `AssistantMessage` is added.
  - `turn_chunk` appends to `segment`.
  - `turn_done` sets `segment = null`.
- `tool_call` adds a `ToolCallMessage`, with arguments as `JSON.stringify`.
- `tool_result` adds a `ToolResultMessage`.
- `turn_error` and `turn_rejected` append their existing notes to `segment`, then set `segment = null`.
- `loop_done` with a reason other than `answered` adds `new AssistantMessage('[not kept: <reason>]')`.

This still works against a plain worker, which never sends `loop_done`.

- [ ] **Step 1: Write failing tests:**

```ts
// HarnessPickerViewModel
'ready_raises_chat_url_when_present'           // info has url ws://w and chat_url ws://r → ready == ['ws://r']
'ready_falls_back_to_url'
// HarnessClient
'tool_notifications_raise_signals'             // tool_call/tool_result/loop_done frames → each signal raised with params
'list_tools_sends_method'                      // sent frame method == 'list_tools'
// ChatViewModel (fake client over FakeSocket, as the existing tests do)
'multi_step_turn_renders_call_result_and_answer' // turn_started, chunk "", tool_call get_time, turn_done, tool_result ok, turn_started, chunk "It's 14:05.", turn_done, loop_done answered, response → Transcript types [User, Assistant, ToolCall, ToolResult, Assistant]; last Text "It's 14:05."
'send_resolves_after_loop_done_not_turn_done'  // after the first turn_done, Send is still blocked (a second Send adds nothing); unblocked after the response
'not_kept_loop_adds_note'                      // loop_done step_limit → last item Text "[not kept: step_limit]"
'tools_text_reflects_runtime'                  // supports_tools true + list [get_time] → 'tools: get_time'; supports_tools false → 'tools: off'
```

- [ ] **Step 2: Run, expecting FAIL:** `cd app && npx vitest run`.
- [ ] **Step 3: Implement.** In `Chat.mu`, add `DataTemplate`s for `ToolCallMessage` (`→ {Name} {Arguments}`, mono font) and `ToolResultMessage` (`← {Name} {StatusText}: {Content}`, `@Fg2`), reusing the assistant bubble style. Add a `TextBlock` bound to `$ToolsText` (`Style = @BodySm, Foreground = @Fg2`) in the chat header.
- [ ] **Step 4: Run, expecting PASS:** `cd app && npm test && npm run typecheck:web`.
- [ ] **Step 5: Commit:** `feat(app): chat via the agent runtime with tool call/result blocks`.

### Task 10: Deploy and live proof (owner go required)

**Files:**
- Modify (hive, not repo): `C:\llm-harness\harness-build\catalog.toml`; new folder `C:\llm-harness\sandbox\`

- [ ] **Step 1: Restore the hive clone:** `scripts/hive-test.sh --restore`. Then **stop and ask the owner for the go to push**.
- [ ] **Step 2: After the go:**
  1. Push main.
  2. On the hive, `git pull --ff-only` in `C:\llm-harness\local-llm-harness`.
  3. Stop the supervisor and kill its workers.
  4. Run `C:\llm-harness\harness-build\build-both.cmd`, expecting `CANDLE_EXIT=0`, `LLAMA_EXIT=0` and `SUP_EXIT=0`.
  5. Create `C:\llm-harness\sandbox\notes.txt` with the content `The harness sandbox works.`
  6. Add to `catalog.toml`:
     ```
     [tools]
     sandbox = "C:/llm-harness/sandbox"
     ```
  7. Add a firewall rule for TCP 8779 if the dev machine can't reach it (`netsh advfirewall firewall add rule name="harness-runtime" dir=in action=allow protocol=TCP localport=8779`).
  8. `schtasks /Run /TN harness-supervisor`.
- [ ] **Step 3: Verify the startup lines.** `supervisor.out` shows both `supervisor listening on ws://0.0.0.0:8778` and `runtime listening on ws://0.0.0.0:8779 (3 tools)`.
- [ ] **Step 4: Live proof** (launch the app with `HARNESS_SUPERVISOR_URL=ws://192.168.1.101:8778`, start llamacpp + gemma-4-12B at context 8192):
  - "What time is it?" → a `get_time` call block, a result block, and a final answer with the time.
  - "What does notes.txt in the sandbox say?" → `list_dir` and/or `read_file` blocks, and an answer quoting `The harness sandbox works.`
  - The tools indicator reads `tools: get_time, list_dir, read_file`.
  - Starting candle + gemma-3 shows `tools: off` and plain chat still works.
- [ ] **Step 5: Record the result** in the memory note and in `docs/specs/2026-11-23-tool-calling-runtime-design.md` (status: implemented).
