### Task 5: Tool loop driven by a session configuration

**Files:**
- Modify:
  - `backend/src/runtime/tool_loop.rs`
  - `backend/src/runtime/registry.rs`
  - `backend/src/runtime/executor.rs`
  - `backend/src/runtime/server.rs`: the minimum to keep it compiling. Task 6 rewrites it.
  - `backend/src/runtime/test_support.rs`
- Delete: `backend/src/runtime/session.rs`. `RuntimeSession` is replaced.

**Interfaces:**
- Produces:
  - `TurnConfig { step_budget: usize, tools: Vec<String>, enable_thinking: bool, sampling: SamplingConfig }`
  - `#[async_trait] trait TurnLinks: Send + Sync { async fn link(&self, step: usize) -> Result<Arc<dyn WorkerLink>, String /*turn_error text*/>; }`. This replaces `WorkerConnector` in `run`. Step 0 is the first step, and an `Err` there ends `NoHarness`, as before. A mid-loop `Err` ends `Error` with that text.
  - `ToolLoop::new(executor, registry)`. The step cap moves to `TurnConfig`.
  - `async fn run(&self, transcript: &[ChatMessage], content: &str, config: &TurnConfig, links: &dyn TurnLinks, sink: &mut dyn LoopSink) -> Result<(LoopEnd, Option<Vec<ChatMessage>>), SinkClosed>`. It returns the new transcript only for `Answered`, after `loop_done` was delivered, and the caller commits it.
  - `ToolRegistry::openai_tools_for(&self, names: &[String]) -> Vec<Value>`, in registry order.
  - `ToolExecutor::execute(&self, call, enabled: &[String])`. A name outside `enabled` fails with `tool not enabled for this agent: {name}`, checked after "unknown tool".
- Everything else in the loop stays the same: call-id rewriting, preamble split, commit rule.

- [ ] **Step 1: Write the failing tests** (update the existing loop tests to the new signature):

```rust
step_budget_comes_from_config            // budget 2, every step calls → 2 requests, StepLimit
only_enabled_tools_are_sent              // enabled ["echo"] of a registry {echo, writer} → request.tools names == ["echo"]
call_outside_subset_is_refused           // model calls "writer" → tool_result content {"error":"tool not enabled for this agent: writer"}
system_prompt_renders_in_system_turn     // transcript [System("You are terse."), ...] → step-1 request rendered with the real GEMMA4 fixture contains the prompt in the system turn, before the tool declarations block end
run_returns_transcript_only_when_answered
```

- [ ] **Step 2: Run, expecting FAIL.** **Step 3: Implement.** **Step 4: Run, expecting PASS** (`-- runtime` and the whole lib).
- [ ] **Step 5: Commit:** `refactor(runtime): tool loop configured per session`.

