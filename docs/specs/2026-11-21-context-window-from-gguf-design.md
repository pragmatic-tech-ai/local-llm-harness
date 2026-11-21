# Context Window from the GGUF — Design

**Date:** 2026-11-21
**Status:** implemented (pending build/test verification + commit)

## Intent & why

The effective context window (`n_ctx`) was a flat `4096` pinned in four places —
the engine factory, the supervisor's spawn planner, and three `n_ctx = 4096`
lines in the live catalog — so every model got the same window regardless of
what it was actually trained for. A model trained to a far larger window was
silently capped at 4096; the harness never asked the GGUF what the model could
do.

Make each model's **trained context length** the default. llama.cpp exposes it
directly (`llama_n_ctx_train`, wrapped as `LlamaModel::n_ctx_train()` in
`llama-cpp-2`), sourced from the GGUF's `*.context_length` metadata — the window
the model's RoPE was trained for, and therefore the honest ceiling.

## Goals

- In-process llamacpp engine defaults `n_ctx` to the model's trained length when
  no explicit size is requested.
- An explicit request (`--n-ctx` / `HARNESS_N_CTX`, or a catalog `n_ctx`) is
  honored, but capped at the trained length (exceeding it only degrades quality
  via RoPE extrapolation), with a stderr warning when reduced.
- The default reaches supervisor-spawned workers (how the hive runs), not just
  direct launches — the supervisor must stop force-injecting `4096`.
- Uniform meaning across all three engine arms: an **unset** catalog `n_ctx`
  means "use the model's trained context length".

## Non-goals / anti-goals

- **No clamp to VRAM.** The default is the trained max, full stop; the user
  lowers `--n-ctx` if a large model's KV cache won't fit 6GB. (Chosen over a
  KV-byte budget or a flat token ceiling — most faithful to the model, least
  magic.)
- No change to candle's behavior. `candle`'s crate has no `n_ctx_train`
  accessor; when the supervisor omits the env it falls back to its own 4096
  default, exactly as before.
- No new config surface. `n_ctx` stays `Option<u32>` end to end; `None` is the
  new, meaningful default.

## Decisions (both confirmed with the owner)

1. **Clamp strategy: trained max, no clamp.** Default = `n_ctx_train`; explicit
   request capped at it; no VRAM-aware reduction.
2. **http / llama-server arm:** when the catalog omits `n_ctx`, pass `-c 0`
   (llama.cpp's "use trained context" convention) to the external
   `llama-server`; an explicit catalog value passes through as `-c <n>`.

## The change (four parts, all required for the default to take effect)

1. **Engine** (`backend/src/engine/llamacpp.rs`): `load` takes
   `requested_n_ctx: Option<u32>`. A new pure helper
   `resolve_n_ctx(trained, requested) -> u32` implements the policy:
   `None | Some(0)` → `trained.max(1)`; `Some(req) > trained` → `trained` (warn);
   else `req`. `load` calls it with `model.n_ctx_train()`.
2. **Factory** (`backend/src/engine/factory.rs`): pass `cfg.n_ctx` straight to
   `load` instead of `unwrap_or(DEFAULT_N_CTX)`; the now-dead `DEFAULT_N_CTX`
   constant is removed.
3. **Supervisor** (`backend/src/supervisor/manager.rs`): `SpawnPlan::build` no
   longer does `def.n_ctx.unwrap_or(4096)`. For candle/llamacpp it pushes
   `HARNESS_N_CTX` only when the catalog set `n_ctx`; for the llama-server child
   it passes `-c 0` when unset. The `DEFAULT_N_CTX` constant is removed.
4. **Live catalog** (hive `C:\llm-harness\harness-build\catalog.toml`): drop the
   three `n_ctx = 4096` lines so `None` flows through. (The in-repo `SAMPLE` in
   `catalog.rs` is a test fixture and keeps its explicit values.)

## Tests

- `engine::llamacpp::tests::resolve_n_ctx_*` — the policy helper: default to
  trained, honor within trained, cap above trained, floor trained at 1.
- `supervisor::manager::tests::in_process_n_ctx_omitted_when_unset_present_when_set`
  — unset catalog → no `HARNESS_N_CTX` env; explicit → env carries it.
- `supervisor::manager::tests::http_server_ctx_arg_is_zero_when_unset` — unset →
  `-c 0`; explicit → `-c <n>`.

## What's left to the builder

- Nothing structural. The `resolve_n_ctx` policy, the supervisor passthrough,
  and the catalog edit fully specify behavior.
