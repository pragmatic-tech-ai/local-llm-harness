# Local LLM Harness — Learning Lab (Design)

_2026-11-17_

## Intent & why

Learn LLM engineering from the bottom up by building a harness I own completely —
no high-level framework hiding the loop. The vehicle is the canonical chat app, but
its real purpose is to be an **instrument panel**: a tight feedback loop where I turn
a low-level knob (a sampling parameter, the prompt, the context) and immediately see
the effect in the token stream. I'm doing this to become comfortable working with
LLMs at a low level, which I don't yet feel from an engineering perspective.

Success = I understand every byte that crosses the wire between my code and the model,
can change any knob and predict/observe the effect, and have a harness I can extend in
any direction later. Learning is the goal; a product is not.

## Goals

- A chat application that streams a model's reply **token by token**.
- The app **surfaces the low-level state from day one**, rather than hiding it behind
  clean chat bubbles. At minimum, live and visible:
  - sampling knobs I can change between (or during) turns: temperature, top-p, top-k,
    repeat penalty, seed, max tokens;
  - the **exact prompt / message sequence actually sent** to the model (not a
    pretty-printed guess — the real bytes);
  - token accounting: prompt tokens, completion tokens, and **context-window fill**
    (how full the window is);
  - per-token detail where the engine exposes it (e.g. token strings and logprobs);
  - a tokenization view (see how text splits into tokens).
- Changing a knob and seeing the effect is **immediate** — that loop is the product.
- Written in **TypeScript, from scratch**, with no agent/LLM framework.
- **Staged** engine connection (see Approaches): start on llama.cpp over HTTP, then
  drop to in-process bindings for the mechanics HTTP can't reach — without rewriting
  the UI or the chat loop.

## Non-goals (for now)

These are deliberately out of scope for the build, but the architecture must not
preclude them:

- Multi-agent orchestration / routing many sessions across slots.
- Tool-calling / function-calling.
- Connecting to the TODL graphs (the eventual aspiration — explicitly later).
- Authentication, multi-user, or production-grade serving.
- A polished chat UX.

## Anti-goals (would make this a failure even if it "worked")

- Reaching for LangChain or any framework that hides the loop — that defeats the
  entire purpose of learning.
- Prioritizing a slick chat experience over **visibility** of the mechanics.
- Wiring this learning lab *into* Plexus or the server. It uses mural as its UI
  framework (a deliberate dogfooding choice) but stays a standalone app — not a feature
  of the suite.

## Constraints

- Hardware / deployment: inference runs on the **hive-laptop** (NVIDIA RTX 3060
  **Laptop**, **6 GB VRAM**, Windows, LAN `192.168.1.101`), where `llama-server` loads
  the model and binds to the LAN. The **harness (Rust sidecar) and Electron/mural UI
  run on the dev machine** during development and reach the hive over the LAN via the
  sidecar's `--server-url`. (The sidecar can be relocated onto the hive later for an
  always-on service.) The dev machine also has an 8 GB RTX 3060, usable as an alternate
  local inference host by pointing `--server-url` at a local `llama-server`.
- Models: quantized GGUF. **Default: reuse the model LM Studio already downloaded on
  the hive-laptop** — `gemma-4-12B-it-QAT-Q4_0.gguf` under
  `~/.lmstudio/models/lmstudio-community/gemma-4-12B-it-QAT-GGUF/`. A 12B Q4 on 6 GB
  VRAM runs with **partial CPU/RAM offload** (as LM Studio was doing) — functional,
  not fast; fine for a learning lab. The model is configuration, not code, so smaller
  GGUFs can be swapped in for speed. (Gemma 4 ships vision `mmproj` files; ignored —
  text chat only.) The 6 GB ceiling keeps us on llama.cpp (GGUF + offload), as decided.
- Engine: **llama.cpp** (`llama-server` for phase A — it handles the math; the harness
  is everything above it). LM Studio is **not** in the loop at runtime; we only reuse
  the `.gguf` weights it downloaded.
- Language/runtime: **Rust backend** (the sidecar that owns the engine seam and chat
  loop) + **Electron shell with a mural / TypeScript frontend**.
- House conventions: this project lives in the user's workspace, so code follows the
  workspace conventions, per layer. **Frontend (mural/TS):** OOP (classes/methods, no
  module-level free functions or mutable module state), Allman braces, PascalCase
  public methods/interfaces, string literals hoisted to named class constants, VMs
  extend `Observable`. **Backend (Rust):** idiomatic Rust — `snake_case` and `rustfmt`
  (which override the PascalCase/Allman rules for the Rust side).

## Approaches

The decisive axis is **how the harness connects to the engine**, because that gates
how low I can actually poke.

- **A — HTTP to `llama-server`.** Talk to llama.cpp's HTTP endpoints over JSON.
  Gives streaming, every sampling knob, logprobs (`n_probs`), `/tokenize` +
  `/detokenize`, and slot/KV/timing info — a large slice of the low-level surface,
  cheaply and visibly, language-agnostic. **Ceiling:** I set the knobs the server
  exposes but can't change the sampling *algorithm* or touch KV-cache internals.
- **B — In-process bindings** (`node-llama-cpp`). The engine runs inside my process.
  I can own the sampling loop, read raw logits, manage the KV cache — the true "very
  bottom." **Cost:** native build + CUDA setup, and I'm now also managing model
  loading.
- **C — Staged (A then B).** Build the chat app + instrument panel on **A** to learn
  the harness layer and get the visible loop working; then drop to **B** for the
  mechanics A won't expose. Nothing from phase A is wasted — the UI, streaming, and
  loop stay; only the engine seam swaps.

**Chosen: C (staged).** It matches the stated plan — start simple and visible, go
lower once effects are observable — and it's the only option that lets the learning
happen in the order the user wants.

**Stack chosen (user decision): Rust backend + Electron/mural-TS frontend.** The
backend — the engine seam, chat loop, and low-level mechanics — is **Rust**, run as a
standalone sidecar process. The frontend is an **Electron** shell hosting a **mural /
TypeScript** instrument-panel UI, talking to the backend over local WebSocket/JSON-RPC.

Why this over the earlier TS-end-to-end lean: the user is already porting the platform
to Rust, so putting the low-level work in Rust is on-trajectory and makes "the very
bottom" (phase B) a first-class Rust exercise rather than an FFI afterthought; mural is
the user's own UI framework and this is a deliberate place to dogfood it. The web
(TypeScript) alternative was simpler and avoided a Rust↔TS boundary, and Python was the
LLM ecosystem's native tongue — both were set aside in favor of matching the user's
real stack.

**Noted tension (accepted):** mural is itself mid-port to Rust, so the frontend builds
on a framework that is changing underneath. Accepted for the dogfooding value. The lab
remains a *standalone app that uses* mural — it is not wired into Plexus or the server.

## The "how", as decided

- **Engine seam is a Rust trait with two implementations.** Define one engine
  abstraction (streaming completion, tokenize/detokenize, model/context info) in the
  Rust backend. Phase A implements it over llama.cpp's HTTP endpoints; phase B
  implements the same trait with **in-process Rust bindings to llama.cpp**
  (`llama-cpp-2` or equivalent). The chat loop and the frontend depend only on this
  seam, so the phase-A → phase-B graduation is a swap behind the trait, not a rewrite.
  _(Phase-B fork, deferred: llama.cpp Rust bindings keep continuity with phase A's
  engine; a pure-Rust path like Candle would be the deeper "read/modify the forward
  pass" bottom but is a larger undertaking with tighter 8 GB quantization limits. Decide
  at phase B; default to llama.cpp bindings.)_
- **Prefer llama.cpp's _native_ HTTP endpoints over the OpenAI-compat `/v1` ones** in
  phase A. The native `/completion` (with `stream`, `n_probs`, `logit_bias`, full
  sampler settings), `/tokenize`, `/detokenize`, and `/props` expose more of the
  low-level data the instrument panel needs than the `/v1/chat/completions` shim does.
  Owning prompt construction by hand (rather than letting `/v1` build it) is itself
  part of the learning.
- **Instrument panel is first-class, not an afterthought.** The layout treats the
  knobs, the raw outgoing prompt, and the token/context meters as primary UI, with the
  chat transcript alongside — not a chat window with settings buried in a menu.
- **Sampling knobs apply to the _next_ turn in phase A, not mid-generation (user
  decision).** Change a knob, re-send, compare the new stream against the last — the
  immediate turn-knob-see-effect loop, over a clean single-request generation path
  (`llama-server`'s `/completion` carries one fixed sampler config per request).
  **True mid-generation mutation** — changing temperature/top-p between individual
  token samples — is deferred to **phase B**, where owning the sampling loop (in Rust)
  makes it both feasible and far more instructive than faking it over HTTP.
- **A persistent Rust sidecar owns the engine seam and chat loop from day one; the
  Electron/mural frontend is a thin client over it** (local WebSocket/JSON-RPC), not
  the renderer talking straight to `llama-server`. This is forced by phase B: the
  llama.cpp Rust bindings run the engine in-process in the Rust backend, so the seam
  and loop must live there for the phase-A→B swap to stay behind the seam as promised.
  Building it this way in phase A avoids a re-architecture later.
- **The backend is a standalone process with a configurable bind address.** Electron
  spawns it as a sidecar and connects as a client; it is not a native Node addon. A
  side effect is that the backend is reachable from the network when bound beyond
  localhost — the original pain that started this whole discussion.

### My calls (builder-level, stated as proposals — correct any)

- **UI stack:** Electron shell hosting a **mural / TypeScript** instrument-panel UI
  (user decision — supersedes the earlier framework-light / not-mural call). The UI is
  a thin client over the Rust backend.
- **Backend↔frontend transport:** local **WebSocket / JSON-RPC**, Electron spawning the
  Rust backend as a sidecar (not a native Node addon).
- **First model:** `gemma-4-12B-it-QAT-Q4_0.gguf`, reused in place from the
  hive-laptop's LM Studio model directory (no re-download). A smaller GGUF can be
  dropped in later via config for faster, packed-context experiments.
- **Prompt rendering (revised after live discovery, user decision):** Gemma 4 is a
  *thinking* model whose real chat template is an 18 KB Jinja with an unusual token
  scheme (`<|turn>{role}`, `<turn|>`, `<|channel>thought`) and a thinking on/off
  switch — far too complex and model-specific to hand-reimplement. So the harness owns
  the **message list, sampling, and a thinking on/off toggle**, and renders the prompt
  via llama-server's **`/apply-template`**, which returns the exact prompt string. That
  string is what the instrument panel DISPLAYS and what we send to native
  `/completion`. The "own the prompt" goal is served by showing the real bytes, not by
  re-deriving Google's template. (`/v1/chat/completions` was rejected — it hides the
  prompt.)
- **Project location:** standalone at `D:\local-llm-harness` (this doc lives under
  `docs/specs/`).
- **Context-window-full policy (phase A): warn and stop, don't silently manage.** When
  the conversation would overflow the window, the harness refuses the turn with a clear
  warning rather than auto-truncating or summarizing. This fits the learning intent —
  the learner *sees* the wall and then experiments with strategies (sliding window,
  truncation, summarization) deliberately, as features they add and observe, not magic
  that hides the mechanic. The "exact prompt actually sent" view therefore always shows
  the real, untruncated history until the user introduces a strategy themselves.
- **Phase-B capability order: raw logits + own sampling loop first, KV-cache
  manipulation later.** The through-line of the whole lab is "change a sampling knob,
  see the effect," so owning the sampling loop and reading raw logits is the natural
  first phase-B target; KV-cache experiments come after. The `IEngine` seam is scoped
  so both are reachable without widening it later.

## What's left to the builder

- Exact instrument-panel widget layout and how logprobs / context-window fill are
  visualized.
- Model acquisition/download mechanics and where GGUF files live.
- Phase-A milestone cut: the smallest first version (send a prompt, stream tokens,
  show token counts + the raw prompt + the sampling knobs) before adding the richer
  instrumentation.
- How the engine server is launched/managed (manual `llama-server` vs. harness-managed
  process) in phase A.

## Future directions (not now, not precluded)

Tool-calling, multi-agent routing across llama.cpp slots, and connecting to the TODL
graphs (via the Cypher API / server surface) are the intended growth path. The engine
seam and hand-owned prompt construction are chosen partly so these remain reachable.
