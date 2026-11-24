# Task 8 report: templates pick a repository; sessions adapt to provider capabilities

Status: DONE. Commit on `feat/model-providers` (not pushed).

## Implemented

- `agents.ts`
  - `AgentTemplate.repository?`. `host?` stays only so old templates can still be read.
  - `TemplateRepository.Of(template)` returns `repository ?? host ?? 'hive'`, with `TemplateRepository.Default = 'hive'`.
  - `CatalogInfo.repositories?: CatalogRepository[]`, where a `CatalogRepository` is `{name, kind, capabilities, models}`.
  - `capabilities?: ProviderCapabilities | null` on `SessionSummary` and `AttachedSession`.
  - `StepRecord.cost?` and `estimated?`, and `CompactionRecord.summary_cost?`.
  - Task 7's `RepositoryKind`, `ProviderCapabilities` and `OpenRouterModelInfo` are reused.
- `RuntimeClient.ts`
  - `TurnStarted.prompt` is now `string | null`, and it gains `estimated?`.
  - `TurnChunk` gains `reasoning?`.
- `TemplateEditorViewModel` and `TemplateEditor.mu`
  - The Host combo is replaced by a `Repository` combo, placed above Runtime. Its choices come from `catalog.repositories`.
  - A runtime older than repositories offers its `hosts` as Pragmatic repositories over `catalog.models`.
  - `Model` choices come from the selected repository. For a Pragmatic one they are filtered by runtime, with the existing missing / not-served markers kept.
  - `IsRuntimeVisible` is true unless the repository is OpenRouter. The Runtime label and combo are wrapped in a StackPanel bound to it.
  - Changing the repository:
    - OpenRouter sets `runtime = 'openrouter'`.
    - Pragmatic falls back to the catalog's first runtime when the current one is not a catalog runtime.
    - A model the repository lacks is replaced by its first model.
  - Picking an OpenRouter model, or switching to OpenRouter, sets the runtime to `openrouter` and `n_ctx = min(context_length, 32768)`.
  - Opening an OpenRouter template keeps its saved `n_ctx`. Only a pick sets the default.
  - Fill reads `TemplateRepository.Of`, so legacy `host = "hive"`, or no field at all, opens as `hive` and is not dirty. Build always sends `repository` and never `host`.
  - A fresh or seeded template defaults to `hive`. A seeded template with an `openrouter` runtime gets the OpenRouter repository that lists its model.
  - HiddenTextRule: `Changed` raises the visibility props that are now true first and those now false last. A field whose assignment changes other fields (Repository, Model) raises everything.
- `NewSessionDialogViewModel`: the summary gains a `Repository: {name}` line.
- `SessionViewModel` and `SessionView.mu`
  - New `Capabilities` property, taken from `attached.capabilities` and reset on clear.
  - `IsLoadable` (`load_unload !== false`) hides Load and Unload. Null or absent capabilities count as loadable.
  - In `Update`, IsLoadable is raised before the header texts when true and after them when false.
  - Attach passes the capabilities to the panel.
  - The header falls back to `snapshot.repository` when there is no placement host.
- `InstrumentPanelViewModel` and `InstrumentPanel.mu`
  - `ApplyCapabilities`, plus `IsTokenizeAvailable/Unavailable`, `IsRawPromptAvailable/Unavailable`, `IsLogprobsAvailable/Unavailable`, and `NotAvailableText = 'not available for this provider'`.
  - Each feature's area is hidden when unsupported, and the text is shown in its place. Null capabilities (a gone repository) show every feature.
  - `ContextLabel` gains a leading `≈` when the last `turn_started` had `estimated: true`, or when the provider's `measure_prompt` is `estimated`.
  - A `prompt: null` turn shows an empty raw prompt.
  - Features are raised in HiddenTextRule order.
- `messages.ts`, `ChatViewModel` and `Chat.mu`
  - `AssistantMessage` gains `Reasoning`, `HasReasoning` and `AppendReasoning`. HasReasoning is raised before the first reasoning text.
  - A chunk with `reasoning: true` goes to the live segment's reasoning. The template shows it dimmed (`BodySm`, `Fg2`) above the answer.
  - History reload builds messages from content only, so it has no reasoning.
- `TraceViewModel`
  - The step line reads `{step}  ≈ctx …` when `estimated`, then ` · $0.0012` (`toFixed(4)`) when `cost` is set. This also applies to a no-fill step (`1  — · $0.0100`).
  - The compaction line marks its counts `≈` when the step is estimated, which matches the spec's live proof ("A trim compaction shows `≈` counts"). It appends ` · $x` when `summary_cost` is set.

## TDD evidence

- RED: `npx vitest run` on the 6 test files gave 13 failed and 113 passed. All 9 required tests failed:
  - `editor_repository_combo_filters_models`
  - `openrouter_model_hides_runtime_and_defaults_ctx`
  - `legacy_host_template_loads_as_hive`
  - `save_sends_repository_not_host`
  - `load_buttons_hidden_without_load_unload`
  - `instrument_features_marked_not_available`
  - `estimated_context_shows_approx`
  - `reasoning_chunks_render_dimmed_not_in_content`
  - `trace_line_shows_cost_and_estimate`

  Also failing were the extra `summary_names_the_repository` and 3 existing tests that I updated: two Host→Repository tests in the template editor and the dialog summary text.
- GREEN: `npm test` gives 22 files and 260 tests, all passing. `npm run typecheck:web` and `npm run typecheck:node` are clean. `npm run build` is OK.
- Raise-order assertions (HiddenTextRule) are in these tests: the OpenRouter template test (runtime row), the Load buttons test, and the reasoning test.

## Real window (Step 4)

Script: `D:/tmp/models-ui-t8/probe.cjs`.

- It runs the built app against the hive (runtime :8779) through a Playwright `routeWebSocket`, after `page.reload()`, so the first connection also passes through the route.
- Every frame is forwarded. Only the new wire is added:
  - The `list_catalog` reply gains `repositories`: `hive` with its real models, plus a canned `openrouter` with `claude-sonnet-4.5` (200K) and `gemini-2.5-flash-lite` (16K).
  - The `attach_session` reply for "Default #1" gains OpenRouter `capabilities` and an OpenRouter snapshot.
  - For that session, `get_trace`, `model_props` and `send_turn` are answered locally. The turn's notifications are an estimated `turn_started`, a reasoning chunk, an answer chunk, `turn_done`, `trace_step` (estimated, cost) and `loop_done`.
- Nothing was saved to the hive. The app was closed at the end, and no electron process remained.

Screenshots, all looked at:

- `01-template-hive.png`: Repository `hive`, then Runtime `llamacpp`, then Model `gemma-4-12B…`.
- `02-repository-popup.png`: the popup lists `hive` and `openrouter`.
- `03-template-openrouter.png`: Repository `openrouter`. The Runtime row is gone and Model follows Repository directly. Model is `anthropic/claude-sonnet-4.5`, and Context size is `32768` (min of 200000 and 32768).
- `04-model-popup.png`: the model list contains only the repository's two OpenRouter ids.
- `05-template-or-model.png`: picking `google/gemini-2.5-flash-lite` sets Context size to `16384`.
- `06-template-back-to-hive.png`: the Runtime row reappears with a rendered value (`candle`, the real catalog's first runtime) and a hive model. The texts are not stale.
- `07-session-openrouter.png`:
  - The header reads `Default · anthropic/claude-sonnet-4.5 @ openrouter`, with no Load/Unload buttons.
  - Context reads `≈0 / 32768 (0%)`.
  - The trace step reads `1  ≈ctx 1 234/32 768 (4%) · $0.0012`, and the compaction line `⟲ trim: ≈7 420 → ≈5 980 tokens …`.
  - Raw prompt, Logprobs and Tokenize each read `not available for this provider`.
- `08-session-reasoning.png`: after sending "hi", the reply shows the reasoning dimmed and smaller above `Hello! How can I help today?`. Context reads `≈2345 / 32768 (7%)`, and the new step line ends `· $0.0004`.
- `09-session-hive.png`: switching to a Pragmatic session brings back Load model and Unload model, the Raw prompt area, the Logprobs area and the Tokenize box. Context reads `0 / 8192 (0%)` with no `≈`, and trace lines have no `≈` or cost.

## Files

- `app/src/renderer/src/agents.ts` and `RuntimeClient.ts`
- `app/src/renderer/src/TemplateEditorViewModel.ts`, `views/TemplateEditor.mu`, and `NewSessionDialogViewModel.ts`
- `app/src/renderer/src/SessionViewModel.ts` and `views/SessionView.mu`
- `app/src/renderer/src/InstrumentPanelViewModel.ts` and `views/InstrumentPanel.mu`
- `app/src/renderer/src/ChatViewModel.ts`, `messages.ts`, and `views/Chat.mu`
- `app/src/renderer/src/TraceViewModel.ts`
- Tests: `TemplateEditorViewModel.test.ts`, `SessionViewModel.test.ts`, `InstrumentPanelViewModel.test.ts`, `ChatViewModel.test.ts`, `TraceViewModel.test.ts`, and `NewSessionDialogViewModel.test.ts`

## Concerns and judgement calls

- **Switching back from OpenRouter to a Pragmatic repository.** The runtime becomes the catalog's first runtime (`candle` on the hive), and the context size keeps the OpenRouter default (for example 16384) rather than restoring 8192. This is by rule. The context size is user-owned, and only an OpenRouter pick sets it.
- **≈ placement in the trace.** I chose `≈ctx` (directly before `ctx`). The compaction counts also carry `≈` for estimated steps; the brief lists only the step line, but the spec's live proof expects it.
- **Null capabilities** (the session's repository was deleted, or an older runtime) are treated as "show everything". The session is unavailable anyway, so it cannot run a feature.
- **The step line is longer.** On narrow panels the trace step line (`… · $0.0012`) pushes the mini bar to the right edge. In the 360-px panel the bar is mostly clipped, as seen in `08`. The text itself is fully visible.
- **Not seen live.** I have not seen a real OpenRouter turn, a real `list_catalog.repositories`, or real session `capabilities`; Task 9's deploy covers those. Everything new was exercised through the route with wire-shaped data taken from server.rs, trace.rs and link.rs.
