# Task 3 report: worker `generate` and tool capability

Status: DONE

## Built
- `session.rs`: `ContextFill::check(used, total, max_tokens) -> Result<ContextFill, (ContextFill, TurnRejected)>`; `begin_turn` now calls it.
- `rpc.rs`: `generate` method (`GenerateParams {messages, tools?, enable_thinking?, sampling?}`; sampling falls back to the connection's `set_sampling`). Flow: apply_template (tools) -> tokenize -> ContextFill::check -> complete. Each chunk is relayed raw as `turn_chunk` (including the chunk carrying the response marker); parser events send `tool_call` notes; on ResponseMarker the stream is dropped and nothing further is relayed. If the stream ends without a marker, `parser.finish()` is called and any Call is relayed as `tool_call` before `turn_done {content, tool_calls}`. Errors -> `turn_error`; window full -> `turn_started` + `turn_rejected`. `model_props` gains `supports_tools` (engine.supports_tools()). Helpers are associated fns (`started`, `turn_error`, `notify_call`); `send_turn` behaviour unchanged.
- `llamacpp.rs`: `supports_tools` field computed once in `load` via `GemmaToolFormat::template_supports` (false if no template metadata); `supports_tools()` returns it (was hard-coded true).
- `tests/rpc.rs`: existing model_props expectation updated with `supports_tools: false`.

## Reusable FakeEngine
Path: `crate::rpc::test_support::FakeEngine` (`#[cfg(test)] pub(crate)`).
Constructor: `FakeEngine::new(chunks: &[&str]) -> FakeEngine` (last chunk has stop=true; token count 10; no tool support), builders `.with_token_count(n)`, `.with_tools_support(bool)`, getter `.recorded_tools() -> Vec<Vec<Value>>`. Also `test_support::TestClient`: `TestClient::connect(Arc<dyn Engine>, n_ctx).await` (runs real `serve` on 127.0.0.1:0), `.request(method, params).await -> (notes, response)`, `TestClient::methods(&notes)`, `TestClient::params_of(&notes, method)`.

## Tests (rpc::tests, 7)
The 6 from the brief plus `generate_reports_unterminated_call_at_end_of_stream` (covers finish()).

## TDD evidence
- RED: `scripts/hive-test.sh -- rpc` -> `test result: FAILED. 0 passed; 7 failed` (generate/model_props unimplemented), TEST_EXIT=101.
- GREEN: same command -> `7 passed; 0 failed`, TEST_EXIT=0.
- Whole lib: `scripts/hive-test.sh` -> 114 passed. Integration `-- --test rpc` -> 5 passed.
- `scripts/hive-test.sh --features llamacpp -- engine` -> 63 passed (first attempt hit the 30 min background limit; rerun with 2h timeout passed).
- `--restore` run at the end.

## Self-review / concerns
- Candle engine still returns `supports_tools() == true` unconditionally (out of scope for this brief).
- When stream errors mid-way, `turn_error` is sent and no finish()/turn_done (matches send_turn).
- The tool_call/turn_chunk ordering: chunk is relayed before its tool_call notes.

## Review follow-ups (commit "fix(worker): task 3 review follow-ups")
1. Candle `supports_tools` override removed (trait default false applies). No candle engine can be built without a model, so the guard is the trait-default test plus the new unsupported-engine rpc test; `--features candle` lib run: 125 passed.
2. `FakeEngine` now streams lazily (`LazyChunks`), counting pulls and flagging its drop; new test `generate_drops_the_stream_at_response_marker` asserts 4 chunks pulled and stream dropped.
3. `generate` doc comment documents the wire contract (marker chunk keeps engine `stop`; `turn_done` ends the turn).
4. `ContextFill::check` unit tests: needed == total ok, +1 rejected, max_tokens -1 as 0.
5. rustfmt run on tests/rpc.rs.
6. `generate` with non-empty tools on an engine without `supports_tools()` sends only `turn_error {message: "tools are not supported by this engine"}` before any render/turn_started/complete; test added. `generate_passes_tools_to_template` now uses `with_tools_support(true)`.
Evidence: whole lib 119 passed; `--features candle` 125 passed; `--test rpc` 5 passed; `--restore` run. llamacpp not re-run (llamacpp.rs untouched).
