# Task 4 report: OpenRouter client, link and provider

Status: DONE. Commit `7732a964` on `feat/model-providers` (base `034ce42a`), not pushed.

## Implemented

- `backend/src/providers/openrouter/api.rs`
  - `OpenRouterModel { id, name, context_length, prompt_price, completion_price, reasoning }`. Prices are per million tokens, rounded to 1e-9, so `"0.000003"` reads `3.0`. String or number prices are accepted.
  - `OpenRouterApi::new(base_url, key)` and `has_key()`. `Debug` prints the key as `***`. Timeouts: connect 15s, read 180s.
  - `models()` sends `GET {base}/models` with `X-Title` and no key. It keeps only tool-capable models, and is robust per entry (manual JSON extraction). The caller does the id-prefix filtering.
  - `stream_chat(body)` sends `POST {base}/chat/completions` with `Bearer` and `X-Title`, and no `HTTP-Referer`. It returns an `SseStream`.
  - `SseReader` is a byte-buffered line splitter. It handles `\r\n`, blank-line event ends, multi-line `data:`, `:` comments, `[DONE]` (nothing is read after it), and a `finish()` for an unterminated tail.
  - `SseStream::next_event()` yields JSON events. `saw_done()` reports whether `[DONE]` arrived.
  - `ApiError { Status{status, message}, Transport }` displays the verbatim forms `openrouter: {status} {message}` / `openrouter: {error}`. `is_context_length()` is true for a 400 whose message mentions context length or window.
  - The private `ApiKey` masks the key in `Debug`. It scrubs the key out of every error text (HTTP message, stream error object, reqwest chain). reqwest errors go through `without_url()`, and their source chain is appended.
- `backend/src/providers/openrouter/link.rs`
  - `Calibration`:
    - `base(chars)` is chars/4.
    - `estimate(chars)` is base × ratio.
    - `observe(estimate, actual)` sets ratio = actual / estimate. It takes the *uncalibrated* base estimate; the link passes it, so concurrent sessions can't double-scale.
  - `OpenRouterLink: WorkerLink`, message mapping:
    - Assistant `tool_calls` arguments become a JSON string, with content `null` when empty.
    - A tool message becomes `{role:"tool", tool_call_id, content: <tool_result JSON string>}`.
  - Sampling: `temperature`, `top_p`, `top_k` and `repetition_penalty` are sent. `f32` values go through their shortest decimal, so `1.1` is sent as `1.1`. `seed` and `max_tokens` are left out when -1. `reasoning:{enabled:true}` is sent only when thinking is on and the model has `reasoning`. `tools` is left out when empty.
  - `generate` returns an unfold stream:
    - `turn_started {prompt:null, prompt_tokens:<estimate>, n_ctx, estimated:true}` comes first, before the request is sent.
    - Then a `turn_chunk {text, tokens:[], stop:false}` per content delta. Reasoning deltas get `reasoning:true` and are never added to content.
    - Tool-call deltas accumulate by index (BTreeMap). The id and name are taken when first sent; argument fragments are appended.
    - At the end comes `turn_done {content, tool_calls:[ParsedCall], usage?}`. `usage` is `{prompt_tokens, completion_tokens, reasoning_tokens (from completion_tokens_details), cost}`, and the link calls `observe` on it.
    - Invalid argument JSON gives `parse_error`, with `raw` kept and `arguments` set to the raw string. Empty arguments give `{}`.
  - Errors:
    - A 400 context-length error gives `turn_rejected {needed:0, total:n_ctx}`.
    - Other statuses, mid-stream `error` objects and network failures give `turn_error`.
    - A stream that ends with neither `[DONE]` nor a finish reason gives `turn_error "openrouter: stream ended before the answer finished"`.
  - `measure_prompt` returns the calibrated estimate. `model_props` returns `{n_ctx, model_name:id, supports_tools:true}`. `tokenize` returns `Err("not supported by this provider")`.
- `backend/src/providers/openrouter/provider.rs`
  - `OpenRouterProvider::new(repository, api, models: Arc<RwLock<Option<Vec<_>>>>, has_key)`, with `RUNTIME = "openrouter"` and `CAPABILITIES = {Estimated, false, false, false, false}`.
  - `serves`: the runtime must be `openrouter`. When the list is known the model must be in it. When the list is unknown, any model is served, through a bare model definition with `reasoning: false`.
  - `state()` is the default (nothing loading or loaded).
  - `identity()` is a fixed `{openrouter, <repository>, None, 0}`.
  - `current_link` / `ensure_ready` build a link per call. The calibration map is per model id, so it is shared by every link.
  - `unload_if` returns `Ok(false)`. `subscribe` uses a quiet channel.
  - With no key, `current_link` gives `None` and `ensure_ready` gives `Err("repository {name} has no API key")`.
- `backend/src/providers/openrouter/mod.rs` holds the re-exports and the test-only `FakeHttp` / `Canned` / `Captured`. This is a tokio `TcpListener` that serves one canned close-delimited HTTP/1.1 response per connection, written in parts with a flush and a 30ms pause, and captures the request head and body.
- `backend/src/providers/mod.rs` and `backend/src/agents/dispatcher.rs` (see deviations): there is a new default `Provider::unavailable() -> Option<String>`, and `Dispatcher::provider_for` checks it before `serves`.

## RED / GREEN

- RED: `scripts/hive-test.sh -- providers::` with the test modules written and no implementation, so the build failed to compile:
  `error[E0432]: unresolved imports api::ApiError, api::OpenRouterApi, ...`, `cannot find type OpenRouterLink`, `cannot find type Calibration`, and so on.
- GREEN, filtered: `scripts/hive-test.sh -- providers::` gave `test result: ok. 35 passed; 0 failed` (21 new OpenRouter tests plus the existing provider tests) with no warnings from the new code.
  - It covers all 15 brief tests.
  - It adds `sse_reader_splits_events_and_skips_comments`, `api_debug_hides_the_key`, `stream_cut_before_the_end_is_turn_error`, `model_props_report_window_name_and_tools`, `provider_with_key_is_always_ready` and `calibration_is_shared_per_model`.
- GREEN, full: `scripts/hive-test.sh -- --tests` gave lib `593 passed; 0 failed`, and all the integration binaries passed. `scripts/hive-test.sh --restore` printed RESTORED.
- `rustfmt --edition 2021 --check` is clean on all changed files.

## Review Focus coverage

1. `tool_call_deltas_accumulate_by_index` sends two calls with interleaved argument fragments across 5 deltas, and splits one event's data line across two network writes. Both calls come out complete and in index order, in `turn_done` and as `tool_call` notes.
2. There are two tests:
   - `sse_lines_split_across_chunks_and_comments` goes through the real HTTP path. A `data:` line is split mid-JSON across writes, a blank line is split across writes, there are `: OPENROUTER PROCESSING` comments, and the body ends with `[DONE]`. Exactly two events come out.
   - `sse_reader_splits_events_and_skips_comments` is the unit-level version, including `\r\n` and an event after `[DONE]` being ignored.
3. `errors_never_echo_the_key` covers three cases, and asserts that no message contains the key, `sk-or-` or the `127.0.0.1:port` address:
   - a 401 whose message echoes `Bearer <key>` (it is scrubbed to `***`);
   - a mid-stream error object, giving `openrouter: server_error upstream died`;
   - a connect failure to a closed port.

## Deviations (deliberate, please review)

- **`stream_chat` returns `Result<SseStream, ApiError>`, not `Result<_, String>`.** The link must tell a 400 context-length error (which becomes `turn_rejected`) from the other errors. A typed error does that without parsing strings, and `Display` gives the verbatim texts. `models()` still returns `String`, as the brief says.
- **`tool_call` notes are emitted before `turn_done`.** The brief lists started / chunk / done. But the worker relays one `tool_call` note per call, and the app's ChatViewModel renders calls from those notes; the loop renames their ids by position. Emitting them keeps the app's tool-call display working with the loop unchanged. They are only emitted at the end, once the calls are complete.
- **`Provider::unavailable()` (a default method) plus a 3-line check in `Dispatcher::provider_for`.** The brief says that with no key "the dispatcher reports `Unavailable` with that reason". `status()` never calls `ensure_ready`, so without this the placement would read `needs_load`. The order follows the spec: repository not found, then no key, then model not in repository. Other providers inherit `None`.
- **`identity()` takes no requirement**, so it is per repository (`model` = the repository name), not per repository and model. It is stable, so the per-turn identity check never sees a reload. `state().loaded` cannot "report the requirement" for the same reason (no requirement parameter), so it reports nothing. Placement still reads `ready`, because `current_link` returns a link.

## Concerns

- **`link.rs` is about 1040 lines**, of which roughly 520 are tests. I did not restructure it.
- **Pre-existing warning from Task 3:** `unused imports: RepositoryKind and RepositoryRules` in `backend/src/agents/template_store.rs:13`. They are used only in its tests. It is not in my files, so I left it.
- **Reasoning while the list is unknown:** the bare model definition has `reasoning: false`, so thinking is not sent until the list is fetched.
- **`template.rs` has its own private `OPENROUTER_RUNTIME` const** (Task 3). It could use `OpenRouterProvider::RUNTIME` as the single home for the value.
- **`has_key` is passed alongside an `api` that also knows whether it has a key.** I kept both, because the brief's signature has both. Task 5 should pass `api.has_key()`.
