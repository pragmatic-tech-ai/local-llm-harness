# Final whole-branch review: fix report

Branch `feat/tool-calling`, starting at f39e9f39. All 10 findings are fixed. Nothing was pushed.

## Commits
- 19a5f739 fix(app): text after a tool call renders below the call block (finding 7)
- 9d178664 refactor(app): rename TurnSignal to TypedSignal (finding 8, signal type)
- 279b980a fix(runtime): keep preamble inside the model turn, resolve the harness per step (findings 1, 3, 4, 10, 8 session)
- 69d25afb fix(worker): restore an open thinking channel; ToolCallFormat seam; shared frame loop (findings 2, 5, 8 rest)
- 2242f488 feat(runtime): get_time reports local time and UTC offset (finding 6)
- 11ca7778 docs(runtime): note that a timeout abort cannot stop spawn_blocking work (finding 9)

## Findings

### 1. Preamble text closed the model turn (Important)
- **Change:** `ToolLoop::assistant_messages` (backend/src/runtime/tool_loop.rs:289) handles a step whose calls are non-empty and whose `text.trim()` is non-empty. It pushes `assistant{content:text}` and then `assistant{content:"", tool_calls}`. `run` now does `scratch.extend(...)`.
- **Test:** `preamble_text_stays_inside_an_open_model_turn` (tool_loop.rs:844). It renders the step-2 request through the real GEMMA4 fixture via `ChatTemplate`, with `add_generation_prompt`. The output was inspected on the hive with `--nocapture`. The step-2 prompt ends with:
  `<|turn>user\nq<turn|>\n<|turn>model\nLet me check.<|tool_call>call:echo{text:<|"|>x<|"|>}<tool_call|><|tool_response>response:echo{text:<|"|>x<|"|>}<tool_response|>`
  The test asserts that the prompt does not end with `<turn|>\n` and does not contain `Let me check.<turn|>`. It then sends a second user message on the same session. That later-turn prompt ends with `...<tool_response|>done<turn|>\n<|turn>user\nq<turn|>\n<|turn>model\n<|channel>thought\n<channel|>`, and it contains exactly two `<|turn>model\n` openers.
- **Deviation:** none.

### 2. Reasoning after a tool result leaked into history (Important)
- **Change:** this is a worker-side fix. `OpenChannel::at_tail` (backend/src/engine/template.rs:73) returns the text from the last `<|channel>` to the end of the prompt, but only when no `<channel|>` follows it and the tail is a single line (the marker plus a channel name and newline). This guard stops a stray marker in user text from matching. In `Connection::generate` (backend/src/rpc.rs:397), `turn_done.content` is that opener followed by the parser content. Relayed `turn_chunk` notes stay raw.
- **Tests:**
  - `rpc::tests::generate_restores_the_open_thinking_channel_in_turn_done` (rpc.rs:867). `FakeEngine::with_template` renders through the real fixture. The test runs two generate steps with `enable_thinking=true`:
    - Step 1 prompt ends with `<tool_response|><|channel>thought\n`.
    - The first relayed chunk is raw: `The clock reads 14:05.`.
    - `turn_done.content` is `<|channel>thought\nThe clock reads 14:05.<channel|>It's 14:05.`.
    - Step 2 feeds that content back as history. Its prompt does not contain `The clock reads` and ends with `<tool_response|>It's 14:05.<turn|>\n<|turn>user\nthanks<turn|>\n<|turn>model\n`. This was inspected on the hive.
  - Unit tests on the real fixture: `open_channel_found_after_tool_response_with_thinking`, `no_open_channel_in_closed_or_absent_cases` and `open_channel_ignores_an_unclosed_marker_followed_by_more_prompt` (template.rs:227-258).
- **Deviation:** none in behaviour. The detection lives on a small `OpenChannel` type in `engine/template.rs` rather than inline in rpc.rs, so the format knowledge stays in the worker's engine layer.

### 3. turn_done.tool_calls ids not rewritten
- **Change:** `ToolLoop::rename_done_calls` (tool_loop.rs:240) rewrites each `turn_done.tool_calls[i].id` before relay, by position from the same base as the `tool_call` notes. The old per-call rename after `parse_done` was removed because the ids now arrive already rewritten.
- **Test:** `call_ids_are_unique_across_steps_and_consistent` now also asserts that the `turn_done` ids equal the relayed `tool_call` ids, which equal the `tool_result` ids (tool_loop.rs:818).
- **Deviation:** none.

### 4. Link chosen once per user message instead of per step
- **Change:** each step now calls `connector.connect()` and `model_props`, and picks its tools with `tools_for` (tool_loop.rs:133, :226).
  - No link at step 1 ends `NoHarness` with steps 0.
  - A link missing later ends with `turn_error {"message":"no model running"}` and reason `Error`.
  - `steps` counts the generate calls made. A mid-loop `model_props` failure also ends `Error`.
- `FakeConnector::sequence` was added (backend/src/runtime/test_support.rs:158), with a connect counter.
- **Tests:** `link_and_props_are_resolved_per_step` (the second link has its own tool support, so its request carries no tools), `harness_gone_mid_loop_is_error` and `model_props_failure_mid_loop_is_error` (tool_loop.rs:893-935). The existing `no_harness` test is unchanged and passes.
- **Deviation:** none.

### 5. ToolCallFormat trait was never built
- **Change:**
  - `ToolCallFormat { template_supports, new_parser }` and `ToolCallParser { push, finish, content, calls }` (backend/src/engine/tool_format.rs:28, :37).
  - `ToolCallFormats::detect(src)`, the single registry, as an associated fn (tool_format.rs:52).
  - `PlainTextParser` for generations without tools (:68).
  - `GemmaToolFormat` implements `ToolCallFormat`, and `GemmaToolParser` implements `ToolCallParser` (:511). The inherent `template_supports` was removed.
  - The `Engine` trait gains `tool_format() -> Option<&'static dyn ToolCallFormat>`, defaulting to `None`. `supports_tools()` now defaults to `tool_format().is_some()` (backend/src/engine/mod.rs:62).
  - `LocalLlamaEngine` stores the detected format (llamacpp.rs:354).
  - `generate` uses `parser_for(tools)` (rpc.rs:404): the engine's parser when tools are non-empty, otherwise plain text.
- **Tests:**
  - `detected_format_parses_through_the_trait` and `plain_text_parser_keeps_everything_as_content` (tool_format.rs:941, :952).
  - `template_supports_detects_gemma4` now also covers `detect`.
  - `rpc::tests::generate_without_tools_does_not_parse_calls` (rpc.rs:853): no `tool_call` notes, no cut at the marker, and all chunks become content.
  - The existing marker and unterminated-call rpc tests now offer tools on a tool-capable fake.
- **Deviation:** none.

### 6. get_time reported UTC only
- **Change:**
  - `LocalOffset::current_minutes()` (backend/src/runtime/builtin.rs:128). On Windows it calls `GetDynamicTimeZoneInformation` and returns `-(Bias + StandardBias|DaylightBias)` according to the returned zone id, so DST is honoured. `TIME_ZONE_ID_INVALID` gives 0. Other targets return 0.
  - `CivilTime::local_iso8601(unix, offset)` (builtin.rs:93) produces `YYYY-MM-DDTHH:MM:SS+HH:MM`. `utc_iso8601` now shares the same signed `civil()` routine.
  - The output gains `"local"` and `"utc_offset_minutes"`, and the spec description was updated.
- **Cargo:** `Win32_System_Time` was added to the existing windows-sys 0.61 features. The vendored windows-sys 0.61.2 has the feature and `GetDynamicTimeZoneInformation`. Cargo.lock is unchanged, and the `--offline` build passed on the hive.
- **Tests:** `local_time_formats_offsets` covers +00:00, +01:00, -05:30 across the epoch, and 2026-11-23T14:05:00+01:00. `get_time_returns_utc_and_unix` now also checks the offset range, `local` against `local_iso8601` and the offset against `current_minutes()`. That test runs the Windows path on the hive.
- **Deviation:** none.

### 7. App: chunks after a call landed above the call block
- **Change:** in `ChatViewModel.OnToolCall` (app/src/renderer/src/ChatViewModel.ts:204), a tool call adds the call block and closes the segment. `OnChunk` (:189) opens a new `AssistantMessage` for the next non-empty chunk while busy.
- **Tests:** `chunks_after_a_call_render_below_the_call_block` and `empty_chunk_after_a_call_opens_no_segment` (ChatViewModel.test.ts:351, :366).
- **Deviation:** an empty chunk does not open a segment. This avoids blank bubbles from `stop` chunks.

### 8. Duplication
- **SendTurnParams:** there is now one `pub(crate)` definition in rpc.rs:59, used by runtime/server.rs.
- **Frame loop:** `rpc::FrameLoop` is generic over a new `RpcDispatch` trait (rpc.rs:139, :147). It handles accept, the text-frame loop, envelope decoding, parse errors and response framing. `Failure` gained a `Closed` variant meaning "end quietly, no reply".
  - The worker's `Connection` implements it (rpc.rs:230).
  - `RuntimeConnection` implements it (server.rs:160) by wrapping its `handle_request` (renamed from `dispatch`; the tests call it directly) and returning `Failure::Closed` when the sink closed.
  - Both `run` and `handle_frame` copies were deleted.
  - The supervisor control server (`supervisor/control.rs`) was left alone. It runs its own `select!` loop over manager events, which does not fit this shape, and it was outside the finding.
- **RuntimeSession:** the field is now private. Only `transcript()` remains, plus `commit(Vec)` for the loop and tests (session.rs:31).
- **GemmaToolParser::finished():** removed, along with its field. Tests now assert on `ParseEvent::ResponseMarker` events. `TestFeed::both` compares the event lists of whole-fed and char-fed input, which is stronger than comparing the old flag.
- **Signal type:** the app's `TurnSignal<T>` was renamed `TypedSignal<T>` everywhere (JsonRpcConnection.ts:27; HarnessClient, InstrumentPanelViewModel, HarnessPickerViewModel, SupervisorClient and their tests). `OnModelProps` and the other non-turn events no longer use a turn-named type.
- **Deviation:** instead of a props-specific signal, the one generic class was renamed. It was already used for non-turn events (`OnReady`, `OnCleared`, `OnHarnessChanged`), so a single correctly named type is cleaner than two identical classes. Mural's own `Signal` was not adopted because it uses camelCase `subscribe`/`emit` and would cascade through the app.

### 9. Executor abort cannot stop spawn_blocking work
- **Change:** a comment at the timeout `abort()` (backend/src/runtime/executor.rs:103).
- **Deviation:** none.

### 10. Tool message carried the result twice
- **Change:** `ToolLoop::tool_message` (tool_loop.rs:339) now leaves `content` empty. The result lives only in `tool_result`, which is what `template_view` hands the template.
- **Test:** `executes_call_then_answers` asserts `t[2].content == ""`.
- **Deviation:** none.

## Test evidence
- **`scripts/hive-test.sh`** (lib, release, offline): `test result: ok. 220 passed; 0 failed`, TEST_EXIT=0, with no compiler warnings.
- **`scripts/hive-test.sh -- --tests`**: lib 220 passed, main 4, http_engine 9, http_stream 6, http_warmup 2, rpc 5. All ok, TEST_EXIT=0.
- **`scripts/hive-test.sh --features llamacpp`**: `Compiling backend`, then `test result: ok. 228 passed; 0 failed`, TEST_EXIT=0, no warnings. The llama-cpp-2 dependency build was cached, so this took about 20 seconds.
- **Prompt inspection:** `scripts/hive-test.sh -- preamble_text -- --nocapture` and `-- open_thinking -- --nocapture`. Temporary `eprintln!`s printed the real rendered prompts, quoted above under findings 1 and 2, and were removed before committing.
- **`scripts/hive-test.sh --restore`**: RESTORED.
- **App:** `cd app && npm test` passed 13 files / 118 tests. `npm run typecheck:web` was clean.
- **Format and line endings:** `rustfmt --edition 2021` was run on the touched files. All were rustfmt-clean at HEAD except engine/mod.rs, where rustfmt cannot resolve the module standalone; that file was left unformatted and its small change was written in rustfmt style by hand. All files are LF-only.

## Concerns
- **Response-marker cut without tools (finding 5):** when no tools are offered, `generate` no longer stops at a hallucinated `<|tool_response>`. This follows the ruling (parse only when tools are offered), and with no tool declarations Gemma has no reason to emit the marker. It is still a behaviour change for tool-less requests.
- **Opener heuristic (finding 2):** `OpenChannel` is matched on the Gemma marker strings. Formats that use other channel markers would need their own entry. It is not part of `ToolCallFormat`, because thinking applies without tools too.
- **Transcript shape (finding 1):** transcripts now hold two consecutive assistant messages for a preamble plus calls. That is correct for the Gemma 4 template. A different model's template that dislikes consecutive assistant messages would need checking when that format is added.
- **No live check yet:** none of this was proven against a live model on the hive. The tests render the real template but replay scripted model output.

## Re-review residuals (commit 84136bf5 "fix: final re-review residuals")

1. **HTTP harness dropped tool results.** `HttpEngine::apply_template` now posts `messages.iter().map(ChatMessage::template_view)` (backend/src/engine/http.rs:149). Finding 10 left the tool message `content` empty, and this restores the tool result on HTTP harnesses. Test: `apply_template_posts_tool_results_as_content` (backend/tests/http_engine.rs:138). Its wiremock `body_json` checks that the tool message is posted as `{"role":"tool","content":{"utc":...},"tool_call_id":"call_1"}`.
2. **`OpenChannel::at_tail` was too loose.** It now matches only when the prompt ends with exactly `<|channel>` + `[A-Za-z0-9_]+` + `\n` (backend/src/engine/template.rs:82). The unused `CLOSE` constant was removed.
   - Tests: `tool_result_ending_in_a_channel_marker_is_no_opener` (template.rs:292) renders a tool result whose text ends `notes <|channel>x`. With thinking off there is no opener. With thinking on, the template's own `<|channel>thought\n` is the one found.
   - `thinking_off_prompts_have_no_opener` (template.rs:318) covers three conversations with thinking off.
   - String cases: no newline, empty name, a name with a space, a trailing blank line, an already-closed channel, and `my_chan2`.
   - The test helpers moved onto a `Conversations` test type, so the tests add no free functions.
3. **One home for "no model running".** It is now `pub(crate) const ToolLoop::NO_MODEL` (backend/src/runtime/tool_loop.rs:89). `RuntimeConnection::no_model` uses it (server.rs:154), and the server's duplicate was removed.
4. **Thinking-on preamble.** New sibling test `thinking_preamble_reasoning_stays_hidden_and_turn_open` (tool_loop.rs:901). It runs with `enable_thinking=true`, and step 1's content is `<|channel>thought\nR-secret reasoning<channel|>Let me check.` plus a call. The step-2 prompt, rendered through the real fixture with thinking on, does not contain `R-secret`. It does not contain `Let me check.<turn|>`. It ends with `<|turn>user\nq<turn|>\n<|turn>model\nLet me check.<|tool_call>...<tool_call|><|tool_response>...<tool_response|><|channel>thought\n`, so the model turn stays open.

The separate bubble for the raw `<|tool_response>` chunk was left as is, per the coordinator's ruling.

### Evidence
- **`scripts/hive-test.sh`** (lib): 223 passed, 0 failed, TEST_EXIT=0, no warnings.
- **`scripts/hive-test.sh -- --tests`**: lib 223, main 4, http_engine 10, http_stream 6, http_warmup 2, rpc 5. All ok, TEST_EXIT=0.
- **llamacpp:** not run, because no llamacpp-gated code was touched.
- **App:** not touched.
- **Format, line endings and restore:** rustfmt was run on the touched files (all were clean at HEAD, so only these lines changed). All files are LF-only. `--restore` reported RESTORED.
