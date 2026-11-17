# Minimal mural app (Electron + TypeScript) — integration notes

(Recon captured 2026-11-17 for the local-llm-harness frontend. Reference for Task 7/8 implementers.)

## Traps (read first)
1. mural is ESM-only and needs a bundler in the renderer. The Task-6 scaffold (tsc + CommonJS + plain electron) CANNOT render mural. Use **electron-vite** for main/preload/renderer; keep tsc for typecheck only.
2. Never resolve the `development` export condition in the renderer (it points at mural's uncompiled src). Pin `resolve.conditions = ['import','module','browser','default']` so Vite uses `dist/`. Two copies of mural → "PragmaticDark2" theme crash → add `resolve.dedupe: ['@pragmatic-tech-ai/mural']`.
3. Exclude mural (and each imported subpath: `/runtime`,`/basic`,`/framework`,`/visual-engine`,`/resources/pragmatic`) from Vite's dep pre-bundler (`optimizeDeps.exclude`), else theme inits in wrong order and the shell renders empty. Add `optimizeDeps.include: ['highlight.js']` (CJS dep).
4. `.mu` files are NOT loaded at runtime — compile them AOT to sibling `.mu.js` via the mural CLI (`node node_modules/@pragmatic-tech-ai/mural/dist/tooling/cli.js compile src/**/*.mu`) BEFORE vite. Import the `.mu.js`. Add a `compile:mu` script run in `dev`/`build`.
5. Theme fonts 404 unless copied: Pragmatic theme requests `assets/fonts/*.woff2` from `node_modules/@pragmatic-tech-ai/mural/build/resources/pragmatic/fonts/`. Copy via a small Vite plugin (see Plexus `ThemeFontCopy`). Non-fatal (fallback fonts) if skipped.
6. A VM extending `Observable` must call `RaisePropertyChanged('Name', old, new)` manually in setters, or the UI won't update.
7. Importing `.mu.js` from TS needs `allowJs: true` (or a `declare module '*.mu.js'` shim).
8. Every VM/template class used as a `DataType` in a `.mu` must be imported there by class name (no string type proxies).
9. mural is mid-migration: project rule is VMs extend `Observable` (not `MuralBase`); copy `Plexus .../account-dialog.ts` (Observable), not the MuralBase demos.

## Bootstrap/mount
- No window-attach API. Mount into a DOM element via `HtmlTarget`; it renders SVG. Electron just hosts the page.
- Renderer `main.ts`:
  `import { app } from './app.mu.js'` (an `Application[...]{}` block exports `const app`);
  `app.initialize({ theme: Pragmatic, autoScheme: { light: PragmaticLight, dark: PragmaticDark } })`;
  `app.initialize(new HtmlTarget(document.getElementById('app')!))`. Set root DataContext via `app.initialize(target,{dataContext: vm})` or `app.DataContext = vm`.
- `index.html`: full-size `<div id="app"></div>` + `<script type="module" src="./main.ts">`.
- Imports: `HtmlTarget` from `@pragmatic-tech-ai/mural/visual-engine`; `ThemeManager/Density` from `/runtime`; `Pragmatic/PragmaticLight/PragmaticDark` from `/resources/pragmatic`. Theme/scheme are CLASS refs, never strings.
- KNOWN-GOOD root is `EditorShell x:root {}` (themed chrome for free). A bare `DockPanel x:root` with a plain Observable DataContext is UNVERIFIED — if the minimal root misbehaves, fall back to `EditorShell x:root {}` or a `DataTemplate[DataType=ChatVM]` + `ContentControl x:root`.

## View-model
- `import { Observable, ObservableCollection, RelayCommand, type ICommand } from '@pragmatic-tech-ai/mural/runtime'`.
- Setter calls `this.RaisePropertyChanged('Draft', old, v)` (protected; name must equal the binding path). Subscribe via `vm.PropertyChanged('Draft').subscribe(e=>…)` → has `dispose()`.
- `RelayCommand(execute, canExecute?)`; call `RaiseCanExecuteChanged()` when canExecute inputs change.
- Item VMs can be plain `Observable`s. `ObservableCollection<T>` — verify method names in `Mural/src/runtime/observable-collection.ts` (likely PascalCase `Add`).

## .mu markup + binding
- Three parts: TS-style imports, optional `resources Name {}`, and an `Application [Theme=…, Scheme=…] { resources: { … x:root … } }` block. Comments `//`.
- Bind one-way: `Text = $Draft`; interpolate `{{ String($Count) }}`; list `ItemsSource = $Messages`.
- `TextBox.Text` is two-way by default: `TextBox [ Text = $Draft, AcceptsReturn=true, SubmitsOnEnter=true, Placeholder="Ask…" ]`.
- Command (only invoke path — no `Click=$method`): `Button [ Command = $SendCommand ] { TextBlock [ Text = "Send" ] }` with a `RelayCommand` on the VM.
- Enter-to-send: a `Style [TargetType=DockPanel] { on KeyDown { InvokeCommand [ Command=$SubmitCommand ] } }`; the command filters for Return itself.
- Converter/function: `Visibility = $IsBusy << ToVisibility`.
- `$Path` → DataContextBinding against inherited DataContext (set at root).

## Lists (transcript)
- `ScrollViewer [ AutoScrollToEnd=true ] { ItemsControl [ ItemsSource=$Messages, ItemsPanel=@VerticalStackPanel ] }` + per-type `DataTemplate [DataType=UserMessage] { Border {…} TextBlock [ Text=$Text, TextWrapping=Wrap ] }`.
- Streaming assistant text: make `AssistantMessage.Text` an Observable property raising PropertyChanged; bound TextBlock updates live.

## Build/tooling
- Deps: `electron-vite ^5`, `vite ^7.3`, `@electron-toolkit/{utils,preload,tsconfig}`, `@pragmatic-tech-ai/mural ^0.66.2` (transitive: todl-runtime, highlight.js, marked, opentype.js, yaml).
- Scripts: `compile:mu` → `node scripts/compile-mu.mjs` (probe `node_modules/@pragmatic-tech-ai/mural/dist/tooling/cli.js`); `dev`=`compile:mu && electron-vite dev`; `build`=`compile:mu && electron-vite build`.
- Renderer tsconfig: extend `@electron-toolkit/tsconfig/tsconfig.web.json`, `allowJs:true`.
- Best references: Mural demo `D:/architecture-agent/Mural/demo/` (electron.vite.config.ts, src/main|renderer); Plexus `D:/architecture-agent/Plexus/apps/plexus/` (electron.vite.config.ts ThemeFontCopy; agent-chat module .mu + VMs); `Plexus/packages/plexus-core/src/vite/mural-renderer.ts`.

## Unverified / fallbacks
- Minimal `Application{resources:{DockPanel x:root}}` with a plain Observable DataContext — not seen in any app. Fallback: `EditorShell x:root {}` (known-good) or DataTemplate+ContentControl.
- `ObservableCollection` exact method names.
