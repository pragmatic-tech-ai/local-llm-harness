# Local LLM Harness — Phase A Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A working instrument-panel chat app — type a message, watch tokens stream from a local model — with the low-level mechanics (sampling knobs, the raw prompt sent, token/context-fill meters, per-token logprobs, tokenization) visible and adjustable.

**Architecture:** A standalone **Rust sidecar** owns the engine seam (`Engine` trait), the chat session (hand-built prompt, token accounting, warn-and-stop), and a WebSocket/JSON-RPC surface. Phase A's only `Engine` implementation talks to `llama-server` over its native HTTP endpoints. An **Electron** app spawns the sidecar and hosts a **mural/TypeScript** instrument-panel UI that connects as a thin WS client. The seam is the line phase B later swaps behind; nothing above it changes.

**Tech Stack:** Rust (tokio, reqwest w/ streaming, axum+ws or tokio-tungstenite, serde/serde_json, async-trait, futures); Electron; mural/TypeScript; llama.cpp (`llama-server`); small quantized GGUF on an RTX 3060 (8 GB).

**Spec:** `docs/specs/2026-11-17-local-llm-harness-design.md`

## Global Constraints

- Engine seam is a **Rust trait** with exactly one Phase-A implementation (`HttpEngine`); the chat loop and UI depend only on the trait. No phase-B binding code in this plan.
- Backend is a **standalone process with a configurable bind address** (default `127.0.0.1`, overridable to `0.0.0.0`), spawned by Electron as a sidecar — **not** a native Node addon.
- Phase A applies sampling knobs to the **next turn only**; no mid-generation mutation.
- Context-window-full policy: **warn and stop**. Never auto-truncate or summarize. The raw-prompt view always shows the real, untruncated history.
- Prompt is **built by hand** in the backend (**Gemma turn template** for the default Gemma-4 model), not delegated to `/v1/chat/completions`. Phase A uses llama.cpp's **native** endpoints (`/completion`, `/tokenize`, `/detokenize`, `/props`).
- Context size comes from the server's actual `n_ctx` via `/props` — never hardcoded.
- **Deployment:** `llama-server` runs on the **hive-laptop** (`192.168.1.101`, 6 GB laptop GPU, partial CPU offload for the 12B), bound to the LAN; the Rust sidecar reaches it via `--server-url`. Sidecar + Electron run on the dev machine. (Prerequisite, not a code task: a CUDA `llama-server` present on the hive-laptop, loading the GGUF below — see "Prerequisite" after the tasks.)
- Conventions by layer: **Rust** = `snake_case` + `rustfmt`. **mural/TS** = OOP (no module-level free functions / mutable module state), Allman braces, PascalCase public methods/interfaces, string literals hoisted to named class constants, VMs extend `Observable`.
- Default model: `gemma-4-12B-it-QAT-Q4_0.gguf`, reused in place from the hive-laptop's LM Studio dir (`~/.lmstudio/models/lmstudio-community/gemma-4-12B-it-QAT-GGUF/`). Model path is configuration, not code; a smaller GGUF can be swapped in. LM Studio is not used at runtime — only its downloaded weights.

## Review Focus

- **`llama-server` not running / connection refused** — the backend must surface a clear, typed error to the UI, not panic or hang. (Task 2)
- **UTF-8 split across streamed token pieces** — a multi-byte character divided over two chunks must render whole, never as replacement/garbage. (Task 3)
- **Context size from `/props`, not assumed** — context-fill math and warn-and-stop must use the server's real `n_ctx`. (Task 4)
- **A single first message larger than the whole window** — warn-and-stop must trigger on turn one, before any completion request goes out. (Task 4)
- **SSE stream interrupted mid-generation** — if the server drops the connection, the turn ends cleanly with a reported error and partial text retained, not a hang. (Task 3)

---

### Task 1: Backend scaffold, `Engine` trait, and wire types

**Files:**
- Create: `backend/Cargo.toml`, `backend/src/lib.rs`, `backend/src/engine/mod.rs`, `backend/src/engine/types.rs`
- Test: `backend/src/engine/types.rs` (`#[cfg(test)]` module)

**Interfaces:**
- Produces:
  - `SamplingConfig { temperature: f32, top_p: f32, top_k: u32, repeat_penalty: f32, seed: i64, max_tokens: i32 }`
  - `Role { System, User, Assistant }`, `ChatMessage { role: Role, content: String }`
  - `TokenInfo { token: String, logprob: f32, top: Vec<(String, f32)> }`
  - `CompletionChunk { text: String, tokens: Vec<TokenInfo>, stop: bool }`
  - `TokenizeResult { tokens: Vec<i64>, count: usize }`
  - `ModelProps { n_ctx: usize, model_name: String }`
  - `trait Engine` (async, `async_trait`): `props() -> Result<ModelProps>`, `tokenize(&str) -> Result<TokenizeResult>`, `detokenize(&[i64]) -> Result<String>`, `complete(prompt: &str, cfg: &SamplingConfig) -> Result<BoxStream<'static, Result<CompletionChunk>>>`
  - `EngineError` enum with at least `ConnectionRefused`, `Protocol(String)`, `StreamInterrupted`

- [ ] **Step 1: Write the failing test** — `serde_round_trips_sampling_config_and_props`: serialize a `SamplingConfig` and a `ModelProps` to JSON and back; assert field equality and that `ModelProps` deserializes from a `/props`-shaped JSON fragment (`{"default_generation_settings":{"n_ctx":4096}, "model":"..."}` → `n_ctx == 4096`).
- [ ] **Step 2: Run test to verify it fails** — `cargo test -p backend serde_round_trips` → FAIL (types not defined).
- [ ] **Step 3: Define the types and trait** in `types.rs`/`mod.rs` with serde derives. Implementer writes the bodies; `Engine` methods are signatures only (no impl yet). Map `ModelProps` from the `/props` shape above.
- [ ] **Step 4: Run test to verify it passes** — `cargo test -p backend serde_round_trips` → PASS.
- [ ] **Step 5: Commit** — `feat(backend): engine trait and wire types`.

---

### Task 2: `HttpEngine` — tokenize, detokenize, props

**Files:**
- Create: `backend/src/engine/http.rs`
- Test: `backend/tests/http_engine.rs` (mock HTTP server — `wiremock`)

**Interfaces:**
- Consumes: `Engine`, `EngineError`, types (Task 1).
- Produces: `HttpEngine::new(base_url: String) -> Self` implementing `props`, `tokenize`, `detokenize`.

- [ ] **Step 1: Write the failing tests** —
  - `tokenize_maps_llama_endpoint`: mock `POST /tokenize` returning `{"tokens":[9906,1917]}`; assert `tokenize("Hello world")` yields `count == 2`, `tokens == [9906,1917]`.
  - `props_reads_n_ctx`: mock `GET /props` with the `/props` shape; assert `n_ctx == 4096`.
  - `connection_refused_is_typed`: point `HttpEngine` at a dead port; assert `Err(EngineError::ConnectionRefused)` (Review Focus — server down).
- [ ] **Step 2: Run to verify they fail** — `cargo test -p backend --test http_engine` → FAIL (methods unimplemented).
- [ ] **Step 3: Implement** the three methods with `reqwest`, mapping transport errors to `EngineError` (refused → `ConnectionRefused`).
- [ ] **Step 4: Run to verify they pass** — same command → PASS.
- [ ] **Step 5: Commit** — `feat(backend): http engine tokenize/detokenize/props`.

---

### Task 3: `HttpEngine::complete` — streaming SSE

**Files:**
- Modify: `backend/src/engine/http.rs`
- Test: `backend/tests/http_stream.rs` (mock SSE server)

**Interfaces:**
- Produces: `complete()` returning a `BoxStream` of `CompletionChunk`, requesting `stream: true` and `n_probs` so per-token logprobs arrive.

- [ ] **Step 1: Write the failing tests** —
  - `streams_chunks_in_order`: mock `POST /completion` emitting SSE `data:` lines `{"content":"Hel","stop":false}` … `{"content":"lo","stop":false}` … `{"content":"","stop":true}`; assert concatenated text == `"Hello"` and the final chunk has `stop == true`.
  - `utf8_split_across_chunks_renders_whole`: SSE emits a multi-byte char (e.g. `😀`) split across two `data:` lines as raw bytes; assert the surfaced text contains the whole character, never U+FFFD (Review Focus — UTF-8 split). Approach: buffer incomplete UTF-8 between chunks.
  - `interrupted_stream_ends_with_error`: mock server closes mid-stream; assert the stream yields retained partial text then terminates with `Err(EngineError::StreamInterrupted)`, no hang (Review Focus — mid-stream disconnect).
- [ ] **Step 2: Run to verify they fail** — `cargo test -p backend --test http_stream` → FAIL.
- [ ] **Step 3: Implement** streaming with `reqwest` bytes-stream + an SSE line parser; carry a byte buffer across chunks to keep UTF-8 whole; map `n_probs` payload into `TokenInfo`.
- [ ] **Step 4: Run to verify they pass** — same command → PASS.
- [ ] **Step 5: Commit** — `feat(backend): streaming completion with logprobs`.

---

### Task 4: `ChatSession` — prompt construction, token accounting, warn-and-stop

**Files:**
- Create: `backend/src/session.rs`
- Test: `backend/src/session.rs` (`#[cfg(test)]`, fake `Engine`)

**Interfaces:**
- Consumes: `Engine`, types (Task 1).
- Produces:
  - `ChatSession::new(engine, props: ModelProps, system: Option<String>)`
  - `build_prompt(&self) -> String` (Gemma turn template; no distinct system role —
    system text, if any, is prepended into the first user turn)
  - `context_fill(&self) -> Result<ContextFill>` where `ContextFill { used: usize, total: usize }`
  - `send(&mut self, user: &str, cfg: &SamplingConfig) -> Result<BoxStream<..CompletionChunk..>, TurnRejected>` with `TurnRejected::WindowFull { needed: usize, total: usize }`

- [ ] **Step 1: Write the failing tests** —
  - `build_prompt_is_gemma`: system "S" + user "hi" → exact string `"<start_of_turn>user\nS\n\nhi<end_of_turn>\n<start_of_turn>model\n"` (system folded into the first user turn; confirm the exact separator against the model's tokenizer/template metadata and adjust the assertion to match).
  - `context_fill_uses_props_n_ctx`: fake engine reports `tokenize` count 10, props `n_ctx` 4096 → `ContextFill { used: 10, total: 4096 }` (Review Focus — real n_ctx).
  - `oversized_first_message_is_rejected`: fake engine returns a token count > `n_ctx` for the first user turn → `send` returns `Err(TurnRejected::WindowFull{..})` and makes **no** `complete` call (Review Focus — oversized first message; assert the fake's `complete` counter stayed 0).
- [ ] **Step 2: Run to verify they fail** — `cargo test -p backend session` → FAIL.
- [ ] **Step 3: Implement** `ChatSession`: append message, build ChatML prompt, tokenize to get `used`, compare against `props.n_ctx` (reserve `max_tokens` headroom), reject with `WindowFull` before any `complete`; otherwise return the engine stream and append the assistant reply on completion.
- [ ] **Step 4: Run to verify they pass** — same command → PASS.
- [ ] **Step 5: Commit** — `feat(backend): chat session with warn-and-stop`.

---

### Task 5: WebSocket/JSON-RPC server + runnable binary

**Files:**
- Create: `backend/src/rpc.rs`, `backend/src/main.rs`
- Test: `backend/tests/rpc.rs` (WS client against the server with a fake `Engine`)

**Interfaces:**
- Consumes: `ChatSession`, `Engine`, `HttpEngine`.
- Produces — JSON-RPC methods over WS:
  - `model_props() -> ModelProps`
  - `tokenize({text}) -> TokenizeResult`
  - `set_sampling({config})` (stored, applied next `send_turn`)
  - `send_turn({content})` → emits a stream of notifications `turn_chunk` (`{text, tokens, stop}`), terminating with `turn_done` or `turn_error`; and `turn_rejected` (`{needed,total}`) when the window is full.
  - CLI/env config: `--bind <addr:port>` (default `127.0.0.1:8777`), `--server-url <llama-server url>`, `--model <name>`.

- [ ] **Step 1: Write the failing tests** —
  - `ws_tokenize_roundtrips`: connect, call `tokenize`, assert the result matches the fake engine.
  - `ws_send_turn_streams_then_done`: call `send_turn`, assert ordered `turn_chunk`* then `turn_done`.
  - `ws_window_full_emits_rejected`: fake engine forces overflow; assert a `turn_rejected` notification with `needed`/`total`, and no `turn_chunk`.
- [ ] **Step 2: Run to verify they fail** — `cargo test -p backend --test rpc` → FAIL.
- [ ] **Step 3: Implement** the WS server (axum ws or tokio-tungstenite) dispatching JSON-RPC to `ChatSession`; `main.rs` parses config, builds `HttpEngine`, fetches `/props`, binds the configured address.
- [ ] **Step 4: Run to verify they pass** — same command → PASS. Then manual: start `llama-server`, run the binary, confirm it binds and logs the address.
- [ ] **Step 5: Commit** — `feat(backend): websocket json-rpc surface and binary`.

---

### Task 6: Electron shell — spawn and manage the Rust sidecar

**Files:**
- Create: `app/package.json`, `app/electron/main.ts`, `app/electron/sidecar.ts`
- Test: `app/electron/sidecar.test.ts` (spawns a stub binary)

**Interfaces:**
- Produces: `Sidecar` class — `Start(): Promise<{ url: string }>` (spawns the backend with bind/model config, resolves when it logs ready), `Stop(): Promise<void>` (graceful kill), surfaces exit/crash as an event. Exposes the resolved WS url to the renderer.

- [ ] **Step 1: Write the failing test** — `SpawnsAndReportsReadyUrl`: point `Sidecar` at a stub script that prints a ready line with a url; assert `Start()` resolves with that url, and `Stop()` terminates the process (assert exit).
- [ ] **Step 2: Run to verify it fails** — `npm --prefix app test -- sidecar` → FAIL.
- [ ] **Step 3: Implement** `Sidecar` (child_process spawn, parse ready line, lifecycle events) and minimal `main.ts` that starts it on app ready and stops it on quit. Fold in the Electron/mural project scaffold here (this is the first frontend task).
- [ ] **Step 4: Run to verify it passes** — same command → PASS.
- [ ] **Step 5: Commit** — `feat(app): electron shell spawns rust sidecar`.

---

### Task 7: mural frontend — WS client + streaming transcript

**Files:**
- Create: `app/src/HarnessClient.ts`, `app/src/ChatViewModel.ts`, `app/src/views/Chat.mu`
- Test: `app/src/HarnessClient.test.ts`

**Interfaces:**
- Consumes: the JSON-RPC surface (Task 5), the sidecar url (Task 6).
- Produces: `HarnessClient` — `Connect(url)`, `SendTurn(content)`, events/callbacks for `TurnChunk`, `TurnDone`, `TurnError`, `TurnRejected`; `ChatViewModel extends Observable` holding the transcript and the streaming-reply buffer.

- [ ] **Step 1: Write the failing test** — `MapsTurnChunkNotificationsToText`: feed the client a sequence of `turn_chunk` then `turn_done` frames (fake socket); assert it raises `TurnChunk` per frame with the right text and `TurnDone` last.
- [ ] **Step 2: Run to verify it fails** — `npm --prefix app test -- HarnessClient` → FAIL.
- [ ] **Step 3: Implement** `HarnessClient` (ws, JSON-RPC framing) and `ChatViewModel`; `Chat.mu` binds the transcript and appends streamed tokens live.
- [ ] **Step 4: Run to verify it passes** — same command → PASS. Then manual: with `llama-server` + sidecar up, launch the app, send a message, watch tokens stream into the transcript.
- [ ] **Step 5: Commit** — `feat(app): streaming chat transcript over websocket`.

---

### Task 8: mural frontend — the instrument panel

**Files:**
- Create: `app/src/InstrumentPanelViewModel.ts`, `app/src/views/InstrumentPanel.mu`
- Modify: `app/src/views/Chat.mu` (host the panel)
- Test: `app/src/InstrumentPanelViewModel.test.ts`

**Interfaces:**
- Consumes: `HarnessClient` (`set_sampling`, `tokenize`, `model_props`), `TurnChunk` token/logprob data, `TurnRejected`.
- Produces: `InstrumentPanelViewModel extends Observable` exposing: sampling knobs (temperature/top-p/top-k/repeat-penalty/seed/max-tokens) with an `ApplyNext` semantic; the **raw outgoing prompt** (as sent); token-count + **context-fill** meter (`used`/`total`); per-token **logprob** list for the last reply; a **tokenization** view (text → tokens via `tokenize`).

- [ ] **Step 1: Write the failing tests** —
  - `KnobEditsApplyToNextTurnNotCurrent`: edit temperature mid-stream; assert the value sent via `set_sampling` is applied on the *next* `SendTurn`, not the in-flight one.
  - `WindowFullSurfacesWarning`: feed a `TurnRejected`; assert the VM exposes a visible warning with `needed`/`total` and does not clear the transcript (warn-and-stop).
- [ ] **Step 2: Run to verify they fail** — `npm --prefix app test -- InstrumentPanel` → FAIL.
- [ ] **Step 3: Implement** the VM and `InstrumentPanel.mu` (knob controls, raw-prompt view, meters, logprob list, tokenization view), hosted beside the transcript as first-class UI. Hoist all labels to named constants.
- [ ] **Step 4: Run to verify they pass** — same command → PASS. Then manual: change a knob, send, confirm the next stream reflects it and the prompt/meters/logprobs update.
- [ ] **Step 5: Commit** — `feat(app): instrument panel — knobs, prompt, meters, logprobs, tokenization`.

---

## Prerequisite (environment, not a code task)

Before Task 5's manual check and Tasks 7–8's manual runs, a CUDA `llama-server` must
be serving the GGUF on the hive-laptop:

- Obtain a CUDA build of llama.cpp's `llama-server` on the hive-laptop (prebuilt
  release binary or a local build; the box has the toolchain). LM Studio's embedded
  engine is **not** reused — only its downloaded `.gguf`.
- Run it against the reused weights, bound to the LAN, e.g.:
  `llama-server -m "C:\Users\visor\.lmstudio\models\lmstudio-community\gemma-4-12B-it-QAT-GGUF\gemma-4-12B-it-QAT-Q4_0.gguf" --host 0.0.0.0 --port 8080 -ngl <fit-to-6GB> -c <ctx> --parallel 1`
  Tune `-ngl` (GPU layers) down until it fits 6 GB with the rest on CPU/RAM; raise
  `--parallel` only once single-slot works.
- Confirm reachable from the dev machine: `curl http://192.168.1.101:8080/props`.
- The Rust sidecar then runs on the dev machine with
  `--server-url http://192.168.1.101:8080`.

Firewall/exposure note: binding `0.0.0.0` exposes the server on the LAN; keep it to a
trusted network or add a firewall rule, consistent with the design's security note.
