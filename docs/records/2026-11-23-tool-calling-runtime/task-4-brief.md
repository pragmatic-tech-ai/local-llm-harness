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

