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

