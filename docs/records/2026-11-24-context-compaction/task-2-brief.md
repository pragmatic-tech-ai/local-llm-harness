### Task 2: Worker `measure_prompt`

**Files:**
- Modify: `backend/src/rpc.rs` (the worker method)
- Modify: `backend/src/runtime/worker.rs` (`WorkerLink::measure_prompt` and `WsWorkerLink`)
- Modify: every test `WorkerLink` fake (`runtime/test_support.rs`, `tool_loop.rs` tests, `server.rs` tests)

**Interfaces:**
- Produces:
  - The worker method `measure_prompt {messages, tools?, enable_thinking}`, which returns `{prompt_tokens: usize, n_ctx: usize}`.
    - It takes the same message, tools and thinking params as `generate`.
    - It renders through the same template path `generate` uses, then tokenizes.
    - It does not generate and does not touch the engine's cache.
  - `WorkerLink::measure_prompt(&self, req: &GenerateRequest) -> Result<PromptSize, LinkError>`, with `pub struct PromptSize { pub prompt_tokens: usize, pub n_ctx: usize }`. It reuses `GenerateRequest`, and its sampling is ignored.
  - A worker without the method answers -32601. `WsWorkerLink` surfaces that as an `Err`, and later tasks treat it as "measure unavailable".

- [ ] **Step 1: Write the failing tests:**
  - in `rpc.rs`, `measure_prompt_matches_generate_turn_started_tokens`. Using the `FakeEngine` harness, `measure_prompt` on a set of messages returns the same `prompt_tokens` as the `turn_started` of a `generate` with the same messages, and returns `n_ctx` equal to the harness's;
  - `measure_prompt_bad_params_is_32602`;
  - in `worker.rs`, `ws_link_measure_prompt_round_trips` against `TestServer`.
- [ ] **Step 2:** Run `scripts/hive-test.sh -- measure_prompt`. Expected: FAIL.
- [ ] **Step 3: Implement.** Extract the render-and-count part of `generate` into one shared method, so the two cannot drift apart.
- [ ] **Step 4:** Run `scripts/hive-test.sh -- --tests`, then `scripts/hive-test.sh --features llamacpp`, then `--restore`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(rpc): measure_prompt renders and counts a prompt without generating`.

