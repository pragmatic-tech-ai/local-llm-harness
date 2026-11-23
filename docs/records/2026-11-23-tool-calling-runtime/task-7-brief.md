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

