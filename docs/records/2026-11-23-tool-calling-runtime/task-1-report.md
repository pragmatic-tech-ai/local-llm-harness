# Task 1 report: DONE

SAC was turned off by the owner; the blocker in the earlier version of this report is resolved.

## Implemented
types.rs (Role::Tool, ToolCall, FunctionCall, ChatMessage + new/template_view, 2 unit tests); template.rs (render takes tools,
messages via template_view, `tools` in context, 3 gemma4 tests); mod.rs (apply_template tools param, supports_tools default false;
candle/llamacpp true); http.rs (Protocol error when tools non-empty); session.rs, candle.rs, llamacpp.rs, tests/http_engine.rs,
tests/rpc.rs updated. Fixture copied (18687 bytes = hive source; brief said 18682), backend/.gitattributes line added.
The gemma4 expected substrings from the brief matched the real fixture rendering unchanged.

## TDD evidence
RED was not reachable as written: the code was already in place before the hive could compile anything (the first compile after SAC was
lifted failed only on 8 stale render() call sites in template.rs tests, fixed). GREEN runs:
- `scripts/hive-test.sh -- engine::template` -> 10 passed, TEST_EXIT=0
- `scripts/hive-test.sh` -> 77 passed, TEST_EXIT=0
- `scripts/hive-test.sh --features candle` -> 83 passed, TEST_EXIT=0
- `scripts/hive-test.sh -- --tests` (lib + integration incl. rpc, http_engine) -> all ok, TEST_EXIT=0

## Self-review / concerns
- llamacpp feature not compiled (CUDA build); its change is the same mechanical edit as candle (tools param, supports_tools, render arg).
- rustfmt not run on the hive; some long test lines may need `cargo fmt`.
- template_view for Role::Tool with no tool_result falls back to plain content (small addition beyond the brief).
- hive-test.sh uses CARGO_TARGET_DIR=backend	arget\hive-test inside the clone (separate from live targetelease).

## scripts/hive-test.sh usage
`scripts/hive-test.sh [--features F] [--restore] [-- <cargo test args>]`
Tars backend/src, tests, Cargo.toml; scp to hive harness-build\src.tar; generated run-tests.cmd extracts into the clone and runs
`cargo test --release --offline --lib [--features F] <args>` (vcvars64; CUDA env when F contains llamacpp or candle-cuda).
Extra args are appended after --lib, so `-- --tests` also runs integration tests. Exit 0 only when TEST_EXIT=0.
--restore: git checkout -- backend and git clean -fd backend/src backend/tests in the clone (run after finishing).

## Review follow-up fixes
1. Fixture: was UTF-8 BOM (3) + trailing CRLF (2) = 5 extra bytes. Stripped both; now 18682 bytes, starts `{#`, ends `-%}`. Hive source
   C:\llm-harness\harness-build\gemma4-template.jinja replaced with the same bytes (18682).
2. rustfmt --edition 2021 run locally on changed .rs files (rustfmt only touched my files; unrelated factory.rs churn reverted).
3. New tests: http engine Protocol error with tools; HttpEngine supports_tools false; trait default false (session FakeEngine);
   template_view Tool role without tool_result falls back to content.
4. hive-test.sh: cargo args re-quoted into the generated .cmd; comment that locally deleted files stay in the hive clone until --restore.
Verification on hive: lib 79 passed; `-- --tests` all ok (TEST_EXIT=0); engine::template 10 passed. Hive clone restored afterwards.
