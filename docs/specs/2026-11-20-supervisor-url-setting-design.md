# Supervisor URL as an In-App Setting — Design

**Date:** 2026-11-20
**Status:** proposed

## Intent & why

The supervisor URL is currently an environment variable (`HARNESS_SUPERVISOR_URL`)
read once in the Electron **main process** and handed to the renderer over IPC. That
means the only way to point the app at a different supervisor is to relaunch it from a
shell with the variable set — invisible, unpersisted, and not something you can change
from inside the running app.

Move it into the app as a first-class **setting**, backed by a real settings service
modelled on Plexus: a persistent store, a declarative setting definition, and the mural
settings gear in the navigation rail that opens an auto-generated settings dialog. This
also lays the groundwork so future app settings are a one-line definition, not new
plumbing each time.

## Goals

- Adopt mural's `ApplicationSettings` service with a persistent store, exactly the
  Plexus host-seam shape: the framework owns the service + launcher; the app supplies
  the store, the definitions, the rail contribution, and the settings page.
- Persist settings to `userData/settings.json` via a preload→main IPC bridge
  (synchronous snapshot on load so settings have real values at construction; async
  write-through on change).
- Declare a **`supervisor.url`** `String` setting on `RuntimeModule`, editable through
  the rail's settings gear.
- The setting is the source of truth for the supervisor connection. Changing it
  **reconnects live**: drop the old supervisor connection, connect the new one, and
  re-attach the Runtime picker — no restart.
- `HARNESS_SUPERVISOR_URL` becomes a **first-run default**: on a profile with no saved
  value, seed the setting from the env var (so the current launch workflow keeps
  working); thereafter the saved value wins.

## Non-goals / anti-goals

- No new settings beyond `supervisor.url` now. (But adding one later = one
  `SettingDefinition` line; the page renders it automatically.)
- Not porting Plexus's full row-template set — only the **String** row is needed now.
  The page is structured so other `Kind`s (Boolean/Number/Choice/…) can be added later
  without reworking it.
- No change to the direct-backend fallback (`HARNESS_BACKEND_URL`). It stays a
  startup-only alternative used when the supervisor URL is empty at launch.
- Not a bespoke settings store — reuse mural's `ISettingsStore`/`ApplicationSettings`
  seam, not a hand-rolled one.

## Constraints

- mural `0.67.1`. Settings engine imported from `@pragmatic-tech-ai/mural/framework`
  (`ApplicationSettings`, `SettingsStoreKey`, `ISettingsStore`, `SettingDefinition`,
  `SettingKind`, `Setting`, `SettingsLauncherService`, `SettingsContributionKey`,
  `ISettingsContribution`); `DialogService` from the same barrel.
- House style: Allman braces, no semicolons, PascalCase public methods/interfaces,
  hoisted `private static readonly` string literals, VMs/services bind by name; `.mu`
  labels stay inline.
- Electron main/preload/renderer split: the renderer never touches the filesystem; all
  persistence goes through the preload bridge to the main process.
- `.mu` compiled by `npm run compile:mu`; `.mu.js` generated.

## Approaches considered

- **A — Mirror Plexus's mural settings service (chosen).** App-side `ISettingsStore`
  over an IPC bridge; `ApplicationSettings` + a `SettingsContribution` registered in
  `app.mu`; `supervisor.url` declared on `RuntimeModule`; the rail gear opens an
  auto-generated settings dialog. Idiomatic, persistent, extensible. Cost: a settings
  page + String-row template + the IPC bridge.
- **B — Settings service + a field in the Runtime pane.** Same store/service but no
  gear/dialog; the URL is a text field in the existing Runtime capability pane.
  Lighter UI, but diverges from the Plexus pattern and doesn't scale to more settings.
  Rejected per the request for the Plexus shape.
- **C — Minimal persist (localStorage, no `ApplicationSettings`).** Quickest, but not a
  settings service and not extensible. Rejected.

## The design (decided)

### Persistence (main + preload + renderer)

- **Main** (`app/src/main/`): a settings module owning `userData/settings.json`. Two
  IPC channels: `settings:get-snapshot` (synchronous `event.returnValue` = parsed file
  or `{}` on missing/corrupt) and `settings:save` (async `writeFile`, pretty JSON).
  Registered during app bootstrap.
- **Shared contract**: a channel enum + `ISettingsBridge { snapshot; Save(values) }`
  shape, mirrored in `main` and `preload`.
- **Preload**: read the snapshot once synchronously at load (`ipcRenderer.sendSync`),
  freeze it, expose `window.harness.settings = { snapshot, Save }` (`Save` →
  `ipcRenderer.invoke(settings:save)`), alongside the existing `window.harness` bridge.
- **Renderer** `HarnessSettingsStore implements ISettingsStore`: `Load()` returns the
  frozen snapshot; `Save(values)` forwards to the bridge (fire-and-forget).

### Composition (`app.mu`)

Register in `.services:`: `HarnessSettingsStore -> SettingsStoreKey`,
`ApplicationSettings`, `HarnessSettingsContribution -> SettingsContributionKey`. Merge a
new `SettingsResources` dictionary (the page + row templates) and a settings gear icon
resource.

### Setting definition (`RuntimeModule.mu`)

Add a `.settings:` block to `RuntimeModule`:

```
SettingDefinition [ Key = "supervisor.url", Label = "Supervisor URL",
    Description = "WebSocket URL of the harness supervisor (e.g. ws://host:8778).",
    Kind = String, Default = "", Category = "Runtime" ]
```

### Settings UI (ported, minimal)

- `HarnessSettingsContribution implements ISettingsContribution`: `Icon` = a settings
  gear glyph resource, `Tooltip = "Settings"`, `CreateView()` →
  `new SettingsPageViewModel(provider.getRequired(ApplicationSettings.Key))`.
- `SettingsPageViewModel`: groups `ApplicationSettings.Settings` by `Definition.Category`
  (blank → "General") and wraps each live `Setting` in a per-`Kind` row VM. Only
  `StringSettingRow` is implemented now; an unknown `Kind` falls back to the string row
  (or is skipped) so the page never throws.
- `SettingsResources.mu`: `DataTemplate [ DataType = SettingsPageViewModel ]` (scroll +
  category list) and `DataTemplate [ DataType = StringSettingRow ]` (label + `TextBox`
  bound **TwoWay** to `$Setting.Value`). Editing writes through the `Setting.Value`
  channel → `ApplicationSettings.persist()` → store.
- `EditorShell` sees the `SettingsContributionKey` and pins the footer rail gear wired
  to `SettingsLauncherService.OpenCommand`, which shows the page via `DialogService` as
  a modal "Done" dialog (settings apply live).

### Supervisor connection driven by the setting (live reconnect)

The **Runtime capability** owns the supervisor connection, driven by the setting:

- `RuntimeManagementService` resolves `ApplicationSettings` (its values are available at
  construction via the synchronous store load). It reads `supervisor.url` and applies
  it, and subscribes to that setting's change channel to re-apply on edit.
- `ApplyConnection(url)`: disconnect/drop the previous `SupervisorClient`; if `url` is
  non-empty, construct + connect a new `SupervisorClient` and `Attach` it to the picker
  (the picker's `Bind` already disposes its prior subscription); if empty, leave the
  picker inert (and unbind any live chat client). Re-entrant and idempotent.
- `main.ts` bootstrap keeps the **first-run seed**: after settings are available, if
  `supervisor.url` is empty and the env var (via the existing `window.harness
  .SupervisorUrl()`) is non-empty, `ApplicationSettings.Set('supervisor.url', env)` —
  which persists it and fires the change the service already listens for. main.ts no
  longer constructs the supervisor client itself; the direct-backend fallback
  (`HARNESS_BACKEND_URL`, when the supervisor URL is empty) is preserved via
  `BindBackend`.

## Resulting behavior

```
First run (no saved value):
  env HARNESS_SUPERVISOR_URL set → seeded into supervisor.url → service connects.
  env unset → supervisor.url empty → picker inert; direct backend used if HARNESS_BACKEND_URL set.
Later runs: saved supervisor.url used directly.
Edit via rail gear → settings dialog → change supervisor.url →
  service drops old connection, connects new, re-attaches picker (live).
```

## What's left to the builder

1. Exact IPC channel names, the preload bridge property, and the main-process file path
   helper (`app.getPath('userData')`).
2. The gear glyph source (reuse an existing Material/Pragmatic settings icon).
3. Minimal `SettingsPageViewModel` + `StringSettingRow` + templates (port/trim from
   Plexus's `settings-page.ts` / `settings.resources.mu`; one `Kind` only).
4. Tests: store `Load`/`Save` round-trip and legacy-empty fallback; `SettingsPageViewModel`
   grouping + string-row write-through; `RuntimeManagementService` `ApplyConnection`
   (empty→no client, non-empty→connect+Attach, change→reconnect drops the old client);
   first-run env seed (empty + env → Set once; non-empty → no seed).
