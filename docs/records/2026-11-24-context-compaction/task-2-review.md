### Spec Compliance
- ✅ Prompt identity: `measure_prompt` and `generate` both call `Connection::render_and_count` (rpc.rs, new fn after `generate`). It passes `messages`, `tools` (`unwrap_or_default`) and `enable_thinking` straight to `apply_template(.., true, ..)`, then `tokenize`. `generate` keeps its own `OpenChannel::at_tail(&prompt)` tail handling on the same `text`. The rendered text is identical by construction.
- ✅ `generate` behaviour: the tools-unsupported check still comes first, then template, then tokenize. Errors still go out as `turn_error` with the same strings (`e.to_string()` equals the previous Display). `ContextFill::check`, `turn_started` and the order of the notifications are untouched.
- ✅ No cache or state: `render_and_count` takes `&self` and calls only `apply_template` and `tokenize`. In llamacpp these are stateless: `apply_template` reads metadata and renders, and `tokenize` calls `vocab.tokenize`. Neither calls `complete`. `measure_prompt_does_not_generate` asserts `pulled_chunks() == 0`. ⚠️ I did not read the candle and http engine bodies.
- ✅ Error mapping: `Frames::params` maps bad params to ERR_INVALID_PARAMS (-32602). Unknown methods hit the `other` arm (-32601). `WsWorkerLink::classify` turns any error frame for the request id into `Err(LinkError(message))`, so both codes surface as `Err`. The -32601 case is tested in `ws_link_measure_prompt_unknown_method_is_err`. Engine failures and tools on a non-tool engine become an internal error, which is acceptable.
- ✅ Interfaces match the brief: `PromptSize`, `WorkerLink::measure_prompt`, the `{prompt_tokens, n_ctx}` result, and sampling ignored (`MeasureParams` has no sampling field, and serde ignores the extra field from `GenerateRequest`).
- ⚠️ Cannot verify the commit author or the Co-Authored-By trailer from the diff. Nothing in the diff changes the crates or Cargo.lock.
- ⚠️ Cargo was not run, as instructed. The implementer's pass counts are unverified.

### Strengths
- One shared render+count path, `generate`'s control flow otherwise unchanged, and `render_and_count` is an associated method on `Connection` with no free functions.
- `PromptSize` derives `Deserialize` and a bad result shape maps to a `LinkError`. The `FakeLink` default for `measure_prompt` is an error, so a task that forgets to script it fails loudly.
- The tests cover the happy path, no generation, bad params, a real-socket round trip and the error frame.

### Issues
#### Critical
None.

#### Important
None.

#### Minor
1. rpc.rs `measure_prompt_matches_generate_turn_started_tokens`: `FakeEngine::tokenize` returns a constant count, so the test cannot detect a renderer difference in tools or `enable_thinking`. It only proves both paths reach `tokenize`. Identity is guaranteed structurally by the shared path, not by this test. Fix: also compare the `recorded_tools` and the rendered prompt that `generate` reports in `turn_started`.
2. rpc.rs tests: no test for `measure_prompt` with tools on a non-tool engine (it should return an internal error). No test with `enable_thinking` false versus true. Fix: add a small test for the tools-unsupported error.
3. worker.rs module doc (line 1-2) still lists only `generate`, `model_props` and `tokenize`. Fix: add `measure_prompt`.
4. rpc.rs `MeasureParams` duplicates three of the four fields of `GenerateParams`. Tolerable, but it can drift. Optional fix: flatten a shared struct.
5. test_support.rs `set_measure` has `#[allow(dead_code)]`, which is speculative code with no user in this task. See verdict 2.
6. worker.rs has no link-level test for the -32602 case. The path is the same as -32601, so this is low risk.

### Concern verdicts
- **No separate RED run: accept, non-blocking process deviation.** A pure "method missing" RED would only show -32601 or a compile error. `measure_prompt_bad_params_is_32602` and the equality test would fail meaningfully against an unknown method. The one real loss is that no test was shown to fail for the right reason. Compensate by confirming that the tests pass on the hive. Do not rewrite history.
- **Temporary `#[allow(dead_code)]` on `set_measure`: accept, with a condition.** It is narrowly scoped and commented. Task 3, the first caller, must remove the attribute in the same commit that uses it. Record that in the Task 3 brief. Moving the helper into Task 3 would be cleaner.

### Assessment
**Task quality:** Approved
**Reasoning:** The render path is shared, `generate` is behaviourally unchanged, nothing touches engine state, and the error mapping is correct for both -32602 and -32601. Only minor test-depth and doc polish remain.
