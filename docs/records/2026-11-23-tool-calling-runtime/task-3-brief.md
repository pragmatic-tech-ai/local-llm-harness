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

