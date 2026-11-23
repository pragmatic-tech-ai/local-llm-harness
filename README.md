# local-llm-harness

A from-scratch learning harness for local LLMs — an **instrument-panel chat app**:
type a message, watch tokens stream, and see/own the low-level mechanics (the exact
prompt sent, sampling knobs, token/context-fill, per-token logprobs, tokenization,
a thinking on/off toggle). Built to learn LLM engineering from the bottom up.

- **Design:** `docs/specs/2026-11-17-local-llm-harness-design.md`
- **Plan:** `docs/plans/2026-11-17-local-llm-harness-phase-a.md`

## Status

**Backend: complete and live-proven.** A standalone Rust backend (`backend/`) that talks to
a llama.cpp `llama-server` (or runs llama.cpp / candle in-process), plus the Electron/mural
app (`app/`) that reaches it through the supervisor:

- `Engine` trait + `HttpEngine` over llama.cpp native endpoints (`/props`,
  `/tokenize`, `/detokenize`, `/completion` streaming with logprobs, `/apply-template`).
- `ChatSession`: prompt rendered via `/apply-template` (exact bytes shown), token
  accounting, warn-and-stop on a full context window, atomic turn commit.
- Worker WebSocket/JSON-RPC server: `model_props`, `tokenize`, `set_sampling`, `send_turn`
  (emits `turn_started{prompt,prompt_tokens,n_ctx}` → `turn_chunk*` →
  `turn_done`/`turn_error`, or `turn_rejected`); a thinking on/off toggle.
- Supervisor (`--supervisor`, control plane `:8778`): starts and stops worker harnesses
  from a catalog, and hosts the agent runtime (`:8779`). Its `hello` advertises the
  runtime url (or why the runtime is off); the app finds the runtime through it.
- Agent runtime: agent **templates** (TOML files in groups) and persisted **sessions**
  started from them, each with its own transcript, tool subset, step budget and model;
  the tool loop runs a session's turns and the dispatcher loads its model on demand.
- Backend tests run on the hive (`scripts/hive-test.sh`, never on the dev machine);
  validated end-to-end against the real Gemma model (both thinking modes).

**App: live.** Two capabilities on the rail: **Sessions** (the session list, a New session
dialog, and the chat + instrument panel bound to the open session, with Load / Unload model)
and **Agents** (the template tree and the template editor).
See `docs/specs/2026-11-23-agent-templates-sessions-design.md`.

## Engine (inference server)

A CUDA `llama-server` runs on the hive-laptop serving Gemma-4-12B-QAT, reachable at
`http://192.168.1.101:8080`. Setup/restart details: `docs/specs/hive-llama-server-setup.md`.
To (re)start it: `ssh pragmatic-hive 'Stop-Process -Name llama-server -Force; schtasks /Run /TN llama-server'` (1–2 min to load).

## Run the backend + smoke-test it

```sh
# 1. build + run the harness backend (points at the hive engine)
cd backend
cargo run --bin backend -- --bind 127.0.0.1:8777 --server-url http://192.168.1.101:8080 --model gemma-4-12b-qat
# it logs: listening on ws://127.0.0.1:8777 (...)

# 2. in another shell, drive it over WebSocket (needs `npm i ws` once in tools/ or reuse an install)
node tools/ws-smoke-client.js "ws://127.0.0.1:8777" "Reply with exactly: pong" false   # thinking off
node tools/ws-smoke-client.js "ws://127.0.0.1:8777" "What is 17 + 26?" true             # thinking on
```

The client prints the `turn_started` prompt (the real Gemma template bytes), the
streamed reply, and `turn_done`.

## Conventions
- Backend (Rust): `snake_case` + `rustfmt` + clippy-clean.
- Frontend (mural/TS): OOP, Allman braces, PascalCase public methods, hoisted string
  constants, VMs extend `Observable`.
