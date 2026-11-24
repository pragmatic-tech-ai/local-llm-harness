# Task 2 report: worker measure_prompt

Built: Connection::render_and_count (rpc.rs) is the single render+tokenize path used by both
`generate` and the new `measure_prompt` RPC, so counts equal turn_started.prompt_tokens.
Works for all engines (lives above the Engine trait).

Public signatures:
- RPC `measure_prompt {messages, tools?, enable_thinking}` -> `{prompt_tokens, n_ctx}`; bad params -32602;
  tools on a non-tool engine -> internal error; unknown method on old worker -> -32601.
- `pub struct PromptSize { pub prompt_tokens: usize, pub n_ctx: usize }` (Debug, Clone, Copy, PartialEq, Eq, Deserialize) in runtime/worker.rs
- `WorkerLink::measure_prompt(&self, req: &GenerateRequest) -> Result<PromptSize, LinkError>` (sampling ignored)
- Test fake: `FakeLink::set_measure(&self, PromptSize)`; default measure_prompt returns Err("measure_prompt unavailable"). Only one WorkerLink fake exists (runtime/test_support.rs).

Tests: measure_prompt_matches_generate_turn_started_tokens, measure_prompt_does_not_generate,
measure_prompt_bad_params_is_32602 (rpc.rs); ws_link_measure_prompt_round_trips,
ws_link_measure_prompt_unknown_method_is_err (worker.rs).
GREEN: `-- measure_prompt` 5 passed; `-- --tests` ok; `--features llamacpp` lib 442 passed; --restore done.
RED run was not performed separately (implementation written alongside tests); noted as a process deviation.
Files: backend/src/rpc.rs, backend/src/runtime/worker.rs, backend/src/runtime/test_support.rs.

## Fix round 1
1. Equality test now uses `FakeEngine::with_char_tokens()` (one token per char). Four cases (tools x thinking) each assert measure == turn_started.prompt_tokens == prompt char count, and all four counts are distinct.
2. Added `measure_prompt_with_tools_on_unsupporting_engine_matches_generate` (internal error, same message as generate's turn_error).
3. worker.rs module doc lists `measure_prompt`.
4. `PromptParams {messages, tools, enable_thinking}` is shared; `GenerateParams` flattens it and adds `sampling`; `MeasureParams` removed.
5. `set_measure` kept with `#[allow(dead_code)]`; comment now says Task 5 uses it and removes the attribute.
6. Added `ws_link_measure_prompt_invalid_params_is_err` (-32602 frame -> Err).
Evidence: `scripts/hive-test.sh -- --tests` all pass (lib 436), `--restore` done.
