# Phase A frontend — deferred

The Phase A **backend** (Rust sidecar: engine trait, HTTP engine, streaming
completion, chat session, WebSocket/JSON-RPC server) and the **Electron shell**
that spawns it (the `Sidecar` class) are complete and live-proven against the
real hive Gemma engine.

The **mural UI** (plan Tasks 7–8: WS client + streaming transcript, and the
instrument panel) is deferred to a focused follow-up session.

To resume:
- Plan: `docs/plans/2026-11-17-local-llm-harness-phase-a.md` (Tasks 7 and 8).
- Design: `docs/specs/2026-11-17-local-llm-harness-design.md`.
- **mural integration how-to** (electron-vite setup, `.mu` AOT compile, vite
  gotchas, bootstrap, VM/binding patterns, with file:line references into the
  Mural demo and Plexus): `docs/frontend-later/mural-integration-notes.md`.
- Known carry-forwards for Task 8: separate reasoning (`<|channel>thought … <channel|>`)
  from the answer and persist only the answer; basename `model_name` for display;
  render the `f32::MIN` logprob sentinel as "n/a".
- NOTE: the Task-6 `app/` scaffold uses tsc/CommonJS + vitest (fine for the
  Sidecar). The **renderer** must move to electron-vite/ESM (see the how-to).
