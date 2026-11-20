# Navigation Rail Capabilities — Design

**Date:** 2026-11-20
**Status:** proposed

## Intent & why

Reorganize the harness UI around a left **navigation rail of capabilities**, adopting
mural's native capability/module shell rather than a bespoke rail. The first capability
is **Runtime Management**: the runtime + model pickers and Start/Stop, moved off the
top bar into the capability's side pane. The point is not just to relocate two combos —
it's to establish the rail so future facets of a harness (tools, skills, configs,
agents, …) each become *one more capability* without reworking the shell. This matches
the harness-as-extensible-entity vision (a harness is a runtime + model today,
extensible with tools/configs/skills later).

## Goals

- A left navigation rail driven by mural's `EditorShell` + `NavigationService`,
  populated from `shell module` declarations — the same pattern as mural's own demo
  platform (`demo/platform/platform.mu`).
- A **Runtime** capability whose **icon is the provided `RuntimePlay.svg`** and whose
  side pane presents the runtime/model combos, Start/Stop, and status line (the current
  `HarnessPanel` content, relocated verbatim).
- Chat stays as the **main content**; the instrument panel stays docked to its right
  (unchanged).
- The picker↔chat coordination is preserved: a harness reaching *ready* binds a fresh
  chat client to the worker url; stop/clear unbinds it.

## Non-goals / anti-goals

- No capabilities other than Runtime are built now. (But adding one later = a new
  `shell module` + `Capability` + its service — no shell surgery.)
- No change to supervisor, chat, instrument-panel, backend, or protocol behavior.
- **Not** a hand-rolled rail. A bespoke rail would reinvent `NavigationService` /
  `RailAction` / `ShellSideContentPane` and drift from the framework's own model — a
  failure even if it looked right.

## Constraints

- mural `0.67.1` (just published). The harness's `compile:mu` is the mural CLI, which
  supports `shell module`, `Capability`, and `include "icons/x.svg" as Key` — the CLI
  wires the SVG→geometry include resolver (verified in `dist/tooling/include-resolver.js`).
- House style: Allman braces, no semicolons, PascalCase public methods/interfaces,
  hoisted string literals, VMs extend `Observable`; `.mu` labels stay inline.

## Approaches considered

- **A — Adopt mural's EditorShell capability/module shell (chosen).** `EditorShell`
  (already the app root, currently empty) auto-registers a `NavigationService` and
  populates the rail from the composed modules' capabilities; each capability's
  `ServiceKey` resolves a service rendered in the side pane via a `DataTemplate`. Mirrors
  `platform.mu` exactly. Idiomatic and extensible. Cost: a composition-root rework.
- **B — Hand-rolled rail.** A custom rail view-model + content switcher. Smaller today
  but reinvents the framework's navigation system and won't compose with real
  capabilities/modules later. Rejected.

## The design (decided)

**Composition root (`app.mu`)**
- Register `RuntimeManagementService` under its class token (the capability's backing
  service).
- `shell module RuntimeModule` declaring
  `Capability [ Name = "Runtime", Icon = @RuntimeIcon, ServiceKey = RuntimeManagementService ]`.
- An icon dictionary merged app-global: `include "icons/runtime-play.svg" as RuntimeIcon`.
- A `DataTemplate [ DataType = RuntimeManagementService ]` = the picker pane — the
  current `HarnessPanel` markup (Runtime/Model `ComboBox`es, Start/Stop, status).
- Keep `EditorShell x:root { }` (now populated from the module).
- Main content host = the chat, via `ContentHostService` (mirroring `platform.mu`'s
  `$service(ContentHostService).Content`).

**Coordination (preserve picker↔chat)**
- The existing `ShellViewModel` is repurposed as the top-level coordinator: it owns the
  `SupervisorClient`, the picker, and the `Chat`, and wires `picker.OnReady → Chat.Bind`
  / `OnCleared → unbind`. `RuntimeManagementService` surfaces that one picker instance
  into the capability pane (so the DI-constructed pane service and the content-host chat
  share a single coordinator and a single supervisor connection).

**Startup**
- The Runtime capability is selected and its side pane visible by default, so the picker
  is on screen at launch. `Picker.Refresh()` kicks off discovery as today.

## Resulting layout

```
┌────┬───────────────┬──────────────────────────────┬────────────┐
│ ▣  │ Runtime       │  chat transcript             │            │
│    │ Runtime  [▾]  │  ──────────────────          │ instrument │
│    │ Model    [▾]  │                              │   panel    │
│    │ [Start][Stop] │  composer…            [Send] │            │
│    │ status        │                              │            │
└────┴───────────────┴──────────────────────────────┴────────────┘
 rail  capability pane        main content             (right)
```

## Left to the plan (builder)

1. **DI wiring** of `RuntimeManagementService` to the coordinator's picker + supervisor
   (register the coordinator/supervisor as services, or hold a reference the service
   reads). Resolve against how `platform.mu`'s group services obtain their state.
2. **Content host under EditorShell** — confirm the harness sets
   `ContentHostService.Content = Chat` and that `EditorShell` renders it, mirroring
   `platform.mu`.
3. **Rail population** — confirm `EditorShell` auto-populates `NavigationService` from
   `Application.Modules`, else call `PopulateFromModules()` in `main.ts`.
4. **Icon asset** at `src/renderer/src/views/icons/runtime-play.svg`; dictionary
   `include`.
5. **Retire the top dock** — remove the `HarnessPanel`-on-top from `Shell.mu`; fold
   what remains of `Shell`/`ShellViewModel` into the module-driven composition.
