# UI Context-Size Control — Design

**Date:** 2026-11-21
**Status:** implemented (frontend + supervisor tests green; stopgap `n_ctx = 8192`
applied to the live catalog as an interim unblock)

## Intent & why

With the "trained max, no clamp" default, launching a large model (gemma-4-12B,
>200K trained window) fails at generation time with
`context creation failed: null reference from llama.cpp` — the per-request KV
cache for the full window won't fit the 6GB card. The user needs to choose a
context size **at launch**, in the same picker block where runtime and model are
selected, rather than editing the catalog or an env var.

## Goals

- A context-size input in the harness picker block (`HarnessPanel.mu`), beside
  the Runtime and Model ComboBoxes.
- A free **numeric box prefilled with a safe default of 8192**; the user raises
  or lowers it. The engine already caps whatever is sent at the model's trained
  max (`resolve_n_ctx`), so over-large values are safe; a never-sent/invalid
  value falls back to the default rather than to "trained max".
- The chosen size reaches the worker: threaded as an optional `n_ctx` through
  the start RPC into `SpawnPlan::build`, where it becomes `HARNESS_N_CTX`
  (in-process) / `-c` (llama-server).

## Non-goals

- No preset dropdown, no "Auto" option (owner chose the numeric-box form).
- No persistence of the chosen size across launches/sessions (per-launch only).
  The picker is not wired to the settings service; a remembered last-value is a
  later nicety, not v1.
- No change to the resolution policy itself — the override is just another
  explicit request, still capped at the trained max.

## Decided (owner)

- **Numeric box, safe default 8192.** Capped at trained max by the engine.

## The change (UI → start RPC → supervisor)

1. **Picker VM** (`HarnessPickerViewModel.ts`): add a string-backed
   `ContextSizeText` property (TextBox binds it TwoWay), default `"8192"`, with a
   private `ParsedContextSize(): number` that parses it to a positive integer and
   falls back to `DefaultContextSize = 8192` on blank/≤0/NaN. `Start()` passes
   that number to `client.Start(...)`.
2. **View** (`views/HarnessPanel.mu`): a new DockPanel row between Model and the
   Start/Stop row — `TextBlock "Context"` + `TextBox [ Text = $ContextSizeText ]`.
3. **Client** (`SupervisorClient.ts`): `Start(runtime, model, nCtx?: number)` →
   params `{ runtime, model, n_ctx }` (omit when undefined, for back-compat).
4. **Control** (`supervisor/control.rs`): `StartParams` gains
   `n_ctx: Option<u32>` (serde default `None`); `dispatch` passes it to
   `manager.start(runtime, model, n_ctx)`.
5. **Manager** (`supervisor/manager.rs`): `start` takes `n_ctx_override:
   Option<u32>`; `SpawnPlan::build(def, model, catalog, n_ctx_override)` uses
   `n_ctx_override.or(def.n_ctx)` — a UI value wins over the catalog, absence
   falls back to the catalog, and absence of both means trained max.

## Tests

- **manager**: `build` with `n_ctx_override = Some(8192)` → `HARNESS_N_CTX=8192`
  even when the catalog is unset; override beats a catalog value; `None` override
  falls back to the catalog.
- **control**: `start` with an `n_ctx` param threads through to the manager.
- **picker VM** (vitest): `Start` sends the parsed `ContextSizeText`; blank/≤0
  falls back to 8192; `SupervisorClient.Start` includes `n_ctx` in params and
  omits it when undefined.

## Immediate unblock (optional, separate)

Set `n_ctx = 8192` on the llamacpp/http runtimes in the live hive `catalog.toml`
so the 12B runs now, before the UI control ships. Once the UI always sends a
value, the catalog default is moot.

## What's left to the builder

- Exact mural TwoWay TextBox binding syntax (mirror an existing TextBox view).
- Whether to right-size the TextBox width in the block (cosmetic).
