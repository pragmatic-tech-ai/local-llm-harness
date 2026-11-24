# Model Providers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move today's local model path behind a `Provider` seam as the Pragmatic provider, add OpenRouter as a second provider running inside our runtime over HTTPS, give templates a repository, and add a Models rail item that configures repositories.

**Architecture:**
- A `providers` module holds three things:
  - the `Provider` trait, which is today's `ModelHost` plus `capabilities()`;
  - `PragmaticProvider`, which is today's `LocalSupervisorHost` renamed;
  - the OpenRouter parts: an API client, a model list with a cache, `OpenRouterLink` (a `WorkerLink` over SSE) and `OpenRouterProvider` (always ready).
- A `RepositoryRegistry` owns every repository, each with its provider. It holds the Pragmatic repositories from the catalog hosts and the OpenRouter repositories from `repositories.toml`.
- The dispatcher looks providers up in the registry by the requirement's `repository`, with no fallback.
- The loop prefers `turn_done.usage` when it is present.
- The app gains a Models capability and adapts to each session's `capabilities`.

**Tech Stack:**
- Rust: tokio, serde, async-trait, and reqwest gaining `native-tls` (Windows SChannel), all vendored.
- App: Electron and mural TypeScript, tested with vitest.

**Spec:** `docs/specs/2026-11-24-model-providers-design.md`. It builds on the templates, sessions, trace and compaction specs it lists.

## Global Constraints

- **Dependencies:**
  - The only dependency change is reqwest's `native-tls` feature and the crates it pulls in.
  - They are vendored once with `cargo vendor vendor` on the dev machine. That command only fetches; nothing else runs cargo locally.
  - The vendored tree is committed.
  - There is no npm install.
- **Where Rust runs:** only on the hive, through `scripts/hive-test.sh [--features F] [-- <one filter or --tests>]`, ending with `scripts/hive-test.sh --restore`.
- **Rust style:**
  - snake_case and rustfmt.
  - Helpers are associated functions, with no new free functions (tests included).
  - Every error or user-visible string is a named const.
- **TypeScript style:**
  - OOP only.
  - Allman braces and no semicolons.
  - PascalCase for public members.
  - Wire fields keep their snake_case.
  - Literals are `private static readonly` PascalCase constants.
  - VMs extend Observable.
  - `.mu` labels stay inline.
  - A list of plain values needs a row VM plus a DataTemplate (mural renders nothing for a plain string item).
- **Names:**
  - The Pragmatic repository is named `hive`, or each catalog `[[host]]` name.
  - The OpenRouter runtime value is `openrouter`.
  - Repository names are 1 to 64 characters of `A-Z a-z 0-9 _ -` and are unique across kinds.
- **Errors, verbatim:**
  - `unknown repository: {v}`
  - `repository not found: {name}`
  - `repository {name} has no API key`
  - `model not in repository {name}: {model}`
  - `model list unavailable for repository {name}`
  - `n_ctx exceeds the model's context length ({n})`
  - `repository is used by templates: {names}`
  - `not supported by this provider`
  - `openrouter: {status} {message}`
  - `openrouter: {error}`
  - the app text `not available for this provider`
  - the editor text `no API key: models can be listed but not run`
- **OpenRouter:**
  - `base_url` defaults to `https://openrouter.ai/api/v1`.
  - Headers are `Authorization: Bearer {key}` and `X-Title: Pragmatic LLM Harness`, and no `HTTP-Referer`.
  - The body has `stream: true` and `usage: {include: true}`.
  - `reasoning: {enabled: true}` is sent only when thinking is on and the model lists `reasoning`.
  - Sampling sets `temperature`, `top_p`, `top_k`, `repetition_penalty`, `seed` (unless -1) and `max_tokens` (unless -1).
  - Models come from `GET {base_url}/models`. A model is kept when `supported_parameters` contains `tools`, and when it matches `id_prefixes` if any are set.
  - The cache file is `{agent data dir}/cache/openrouter-{name}.json`.
  - The default `n_ctx` for an OpenRouter model is `min(context_length, 32768)`.
  - The estimate is `chars(messages+tools JSON) / 4`, scaled by `last usage.prompt_tokens / its estimate` per repository and model.
- **The key:**
  - It lives only in `{agent data dir}/repositories.toml`.
  - `Debug` prints `***`.
  - It is never in an RPC result or notification, the trace, a session, or a log line. Results carry `key_hint`, the last 4 characters.
  - reqwest errors are reported `without_url()`.
- **Schemas:**
  - Session and trace schema versions are unchanged. New fields are serde default and skipped when empty.
  - On `StepRecord`: `cost: Option<f64>` and `estimated: bool`.
  - On `CompactionRecord`: `summary_cost: Option<f64>`.
  - On `ProviderCapabilities` on the wire: `{measure_prompt: "exact"|"estimated", tokenize, raw_prompt, logprobs, load_unload}`.
- **Compatibility:**
  - Templates still read `host` as an alias of `repository`, and a missing value means `hive`.
  - `list_catalog` keeps `hosts`.
- **Commits:** use `git -c user.name="Eugene Napryaglo" -c user.email="napryaglo@users.noreply.github.com" commit`, and end each message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Work on the branch `feat/model-providers`, and never push, except in Task 9 after the owner's go.

## Review Focus

1. **A tool call whose arguments arrive split across many SSE deltas, two calls interleaved by index.** Both calls are emitted complete and in index order in `turn_done`. Covered in Task 4: `tool_call_deltas_accumulate_by_index`.
2. **A `data:` line split across two network chunks, plus `: OPENROUTER PROCESSING` comment lines and a final `[DONE]`.** The parser yields exactly the events, and comments are ignored. Covered in Task 4: `sse_lines_split_across_chunks_and_comments`.
3. **An API key in any error path.** A 401 response, a connect failure and a mid-stream error object all produce a `turn_error` that never contains the key. Covered in Task 4: `errors_never_echo_the_key`.
4. **An old template with `host = "hive"`, an old session snapshot with `host`, and a template whose model has since left the OpenRouter list.** All of them load. The first two run on `hive`, and the third is marked in the tree, not dropped. Covered in Task 3: `legacy_host_field_routes_to_hive`, and Task 5: `template_with_vanished_model_still_loads`.
5. **A repository renamed or deleted while a template uses it, and a session whose repository was deleted.** Both changes are refused with `repository is used by templates: {names}`. The session placement reads `repository not found: {name}`. Covered in Task 5: `rename_and_delete_refused_while_used`, `deleted_repository_session_unavailable`.

---

### Task 1: HTTPS for the backend

**Files:**
- Modify: `backend/Cargo.toml` (reqwest features gain `"native-tls"`)
- Modify: `backend/Cargo.lock`
- Modify: `backend/vendor/` (regenerated)
- Test: `backend/tests/https_smoke.rs` (new, `#[ignore]` by default)

**Interfaces:**
- Produces: `reqwest::Client` can reach `https://` URLs on the hive.

- [ ] **Step 1: Turn on the feature.** Add `"native-tls"` to reqwest's features. Then, on the dev machine, in `backend/`, run `cargo vendor vendor` (fetch only). Check that `vendor/` now contains `native-tls`, `schannel` and `tokio-native-tls`.
- [ ] **Step 2: Write the smoke test.** It is an ignored integration test, `https_get_openrouter_models_list`. It sends `GET https://openrouter.ai/api/v1/models` and asserts status 200 and a JSON body with a `data` array.
- [ ] **Step 3: Run it.**
  1. `scripts/hive-test.sh -- --tests`. Expected: PASS, the build is offline with the new crates, and nothing else changes.
  2. `scripts/hive-test.sh -- --tests -- --ignored https_get_openrouter_models_list`. Expected: PASS, which proves TLS from the hive.
  3. `--features llamacpp`, then `--restore`.
- [ ] **Step 4: Commit** with the message `build: reqwest native-tls (Windows SChannel), vendored`.

### Task 2: Provider seam, capabilities and the Pragmatic provider

**Files:**
- Create: `backend/src/providers/mod.rs`
- Create: `backend/src/providers/pragmatic.rs` (moved from `agents/host.rs`)
- Modify: `backend/src/agents/host.rs` (keeps only what is not provider-specific, or re-exports)
- Modify: `backend/src/agents/dispatcher.rs`
- Modify: `backend/src/lib.rs`
- Modify: all users of `ModelHost` and `LocalSupervisorHost`

**Interfaces:**
- Produces:
  - `pub trait Provider`, which is today's `ModelHost` methods plus `fn capabilities(&self) -> ProviderCapabilities`.
  - The capabilities type:

    ```rust
    pub struct ProviderCapabilities { pub measure_prompt: Measure, pub tokenize: bool, pub raw_prompt: bool, pub logprobs: bool, pub load_unload: bool }
    ```

    It derives Serialize/Deserialize and Clone/Copy/PartialEq/Debug, plus `ProviderCapabilities::FULL` (every flag true, `Exact`).
  - `pub enum Measure { Exact, Estimated }`, with wire names `exact` and `estimated`.
  - `PragmaticProvider`, which is `LocalSupervisorHost` renamed with unchanged behaviour. Its capabilities are `FULL`.
  - `ModelRequirement.repository: String`, which replaces `host: Option<String>`. `ModelRequirement::of` fills it from `template.repository` (Task 3), defaulting to `hive`.
  - `pub trait ProviderSource: Send + Sync { fn provider(&self, repository: &str) -> Option<Arc<dyn Provider>>; fn providers(&self) -> Vec<Arc<dyn Provider>>; }`.
  - `Dispatcher::new(source: Arc<dyn ProviderSource>)`. `PlacementPolicy` and `ExplicitLoadPolicy` are removed: routing is exact.
    - A repository that is missing gives `Unavailable{reason: "repository not found: {name}"}`.
    - A provider that does not serve the requirement gives `model not in repository {name}: {model}`.
  - `Dispatcher::capabilities(&self, req) -> Option<ProviderCapabilities>`.
  - A temporary `StaticProviders(Vec<(String, Arc<dyn Provider>)>)` implements `ProviderSource` for main.rs and the tests until Task 5.

- [ ] **Step 1: Write the failing tests** in dispatcher.rs:
  - `routes_only_to_named_repository`: two providers both serve the model, and the requirement names the second one. The second is used and the first is never asked.
  - `missing_repository_is_unavailable_with_reason`
  - `unserved_model_is_unavailable_with_reason`
  - `capabilities_come_from_the_provider`
  - The existing dispatcher and host tests are kept and renamed.
- [ ] **Step 2: Watch them fail.** Run `scripts/hive-test.sh -- agents::`. Expected: FAIL.
- [ ] **Step 3: Implement** the items above.
- [ ] **Step 4: Watch them pass.** Run `scripts/hive-test.sh -- --tests`, then `--features llamacpp`, then `--restore`. Expected: PASS, with every existing test green.
- [ ] **Step 5: Commit** with the message `refactor(agents): Provider seam with capabilities; the local host becomes the Pragmatic provider; exact routing`.

### Task 3: Templates name a repository

**Files:**
- Modify: `backend/src/agents/template.rs`
- Modify: `backend/src/agents/template_store.rs`
- Modify: `backend/src/runtime/catalog_source.rs`
- Modify: `backend/src/agents/host.rs` (`ModelRequirement::of`)

**Interfaces:**
- Consumes: Task 2.
- Produces:
  - `AgentTemplate.repository: Option<String>`, with `#[serde(alias = "host")]`, skipped when `None`. `host` no longer exists as a field.
  - `TemplateRules`:
    - `hosts` is replaced by `repositories: Vec<RepositoryRules>`.
    - `pub struct RepositoryRules { pub name: String, pub kind: RepositoryKind, pub models: Option<Vec<RuleModel>> }`, where `None` means the list is unknown.
    - `pub struct RuleModel { pub id: String, pub runtimes: Vec<String>, pub context_length: Option<u32> }`.
    - `pub enum RepositoryKind { Pragmatic, OpenRouter }`, with wire names `pragmatic` and `openrouter`.
  - `TemplateRules.validate` resolves the repository (missing means `hive`). An unknown one gives `unknown repository: {v}`.
    - **Pragmatic:** today's runtime and model checks, against that repository's models.
    - **OpenRouter:** the runtime must be `openrouter`, and an unknown list gives `model list unavailable for repository {name}`. An unknown model gives `unknown model: {v}`, and `n_ctx` over the context length gives `n_ctx exceeds the model's context length ({n})`.
  - `DiscoveredCatalog` builds the Pragmatic `RepositoryRules` from discovery for each host name. Task 5 adds the OpenRouter ones from the registry.

- [ ] **Step 1: Write the failing tests:**
  - `legacy_host_field_routes_to_hive` (Review Focus 4): TOML with `host = "hive"` parses to `repository: Some("hive")`, and `ModelRequirement::of` gives `"hive"`.
  - `missing_repository_means_hive`
  - `unknown_repository_is_rejected`
  - `openrouter_model_validates_runtime_model_and_ctx`
  - `openrouter_unknown_list_refuses_save`
  - `pragmatic_validation_unchanged`: the existing runtime and model tests still pass.
- [ ] **Step 2: Watch them fail.** Run `scripts/hive-test.sh -- agents::`. Expected: FAIL.
- [ ] **Step 3: Implement** the items above. `list_catalog`'s `hosts` keeps listing the Pragmatic repository names.
- [ ] **Step 4: Watch them pass.** Run `scripts/hive-test.sh -- --tests`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(agents): templates name a repository (host stays an alias)`.

### Task 4: The OpenRouter client, link and provider

**Files:**
- Create: `backend/src/providers/openrouter/mod.rs`
- Create: `backend/src/providers/openrouter/api.rs` (HTTP client, model list, SSE reader)
- Create: `backend/src/providers/openrouter/link.rs` (`OpenRouterLink: WorkerLink`)
- Create: `backend/src/providers/openrouter/provider.rs` (`OpenRouterProvider: Provider`)
- Test: in-module tests using a tokio `TcpListener` fake HTTP server serving canned SSE bodies

**Interfaces:**
- Consumes: `Provider`, `ProviderCapabilities` (Task 2); `WorkerLink`, `GenerateRequest`, `Note` (runtime/worker.rs).
- Produces:
  - `OpenRouterApi::new(base_url: String, key: Option<String>) -> Self`.
  - `async fn models(&self) -> Result<Vec<OpenRouterModel>, String>`. The result is filtered to tools only; the id-prefix filtering is done by the caller.
  - `async fn stream_chat(&self, body: Value) -> Result<SseStream, String>`.
  - `pub struct OpenRouterModel { pub id: String, pub name: String, pub context_length: u32, pub prompt_price: f64, pub completion_price: f64, pub reasoning: bool }`. Prices are in USD per million tokens.
  - `SseReader`: it splits bytes into events on blank lines, takes `data:` lines, and ignores `:` comment lines. `[DONE]` ends the stream.
  - `OpenRouterLink::new(api: Arc<OpenRouterApi>, model: OpenRouterModel, n_ctx: u32, calibration: Arc<Calibration>)`.
    - `generate` emits the spec's notes in order: `turn_started {prompt: null, prompt_tokens, n_ctx, estimated: true}`, then `turn_chunk` for each content or reasoning delta (`reasoning: true` on reasoning), then `turn_done {content, tool_calls, usage}`, `turn_rejected` or `turn_error`.
    - `usage` is `{prompt_tokens, completion_tokens, reasoning_tokens, cost}`.
    - `measure_prompt` returns the calibrated estimate.
    - `model_props` returns `{n_ctx, model_name: id, supports_tools: true}`.
    - `tokenize` gives `Err("not supported by this provider")`.
  - `Calibration`: a per-model ratio holder, `fn estimate(&self, raw_chars: usize) -> u32` and `fn observe(&self, estimate: u32, actual: u32)`.
  - `OpenRouterProvider::new(repository: String, api: Arc<OpenRouterApi>, models: Arc<RwLock<Option<Vec<OpenRouterModel>>>>, has_key: bool)`.
    - It is always ready, as the spec describes.
    - Its capabilities are `{Estimated, false, false, false, false}`.
    - `serves` checks the list when known, and is always true when it is unknown.
    - With no key, `ensure_ready` gives `Err(repository {name} has no API key)` and the dispatcher reports `Unavailable` with that reason.
  - **Message mapping:**
    - The assistant's `tool_calls` arguments become a JSON string.
    - A tool result becomes `{role: "tool", tool_call_id, content: <JSON string>}`.
    - The parsed-call shape in `turn_done.tool_calls` matches what the worker emits today (read `engine/tool_format.rs` `ParsedCall`). Invalid JSON arguments set `parse_error` and keep `raw`.

- [ ] **Step 1: Write the failing tests:**
  - `models_filtered_to_tools_and_prices_per_million`: the canned `/models` JSON has a non-tools model and string prices `"0.000003"`, which become `3.0`.
  - `request_body_maps_messages_tools_sampling_and_headers`: the fake server captures the request. It asserts the Bearer header, `X-Title`, no `HTTP-Referer`, `stream: true`, `usage.include`, the tool-message mapping, `repetition_penalty`, and that `seed` and `max_tokens` are omitted when -1.
  - `reasoning_sent_only_when_thinking_and_model_supports_it`
  - `content_stream_becomes_started_chunks_done_with_usage`
  - `reasoning_deltas_are_flagged_chunks_not_content`
  - `tool_call_deltas_accumulate_by_index` (Review Focus 1)
  - `invalid_tool_arguments_set_parse_error`
  - `sse_lines_split_across_chunks_and_comments` (Review Focus 2)
  - `context_length_400_becomes_turn_rejected`
  - `other_http_error_becomes_turn_error_with_status_and_message`
  - `errors_never_echo_the_key` (Review Focus 3)
  - `estimate_calibrates_after_usage`: the first estimate is chars/4. After observing actual = 2× the estimate, the next estimate doubles.
  - `tokenize_is_not_supported`
  - `provider_without_key_is_unavailable`
  - `provider_serves_unknown_list_but_checks_known_list`
- [ ] **Step 2: Watch them fail.** Run `scripts/hive-test.sh -- providers::`. Expected: FAIL.
- [ ] **Step 3: Implement** the items above, with no SSE crate.
- [ ] **Step 4: Watch them pass.** Run `scripts/hive-test.sh -- --tests`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(providers): OpenRouter client, streaming link and always-ready provider`.

### Task 5: Repository registry, the repositories file, and the RPCs

**Files:**
- Create: `backend/src/providers/registry.rs`
- Modify: `backend/src/runtime/server.rs`
- Modify: `backend/src/runtime/catalog_source.rs`
- Modify: `backend/src/main.rs`

**Interfaces:**
- Consumes: Tasks 2 to 4.
- Produces:
  - `RepositoryRegistry::open(data_dir: &Path, pragmatic: Vec<(String, Arc<dyn Provider>)>, discovery: Arc<dyn CatalogSource>, base_url: String) -> io::Result<Self>`.
    - It implements `ProviderSource`.
    - It reads `repositories.toml` (`[[openrouter]] name, api_key, id_prefixes`) and the cache files.
  - It has these methods:
    - `fn list(&self) -> Vec<RepositorySummary>`
    - `async fn models(&self, name: &str, refresh: bool) -> Result<RepositoryModels, String>`
    - `async fn save(&self, name, previous_name: Option<&str>, id_prefixes: Vec<String>, api_key: Option<String>, used_by: &dyn Fn(&str) -> Vec<String>) -> Result<SaveResult, String>`
    - `fn delete(&self, name, used_by) -> Result<(), String>`
    - `fn rules(&self) -> Vec<RepositoryRules>`
  - `save` writes the file atomically, then fetches the model list. The save succeeds when the fetch fails, and the result then carries `fetch_error`. A rename carries the key.
  - `RepositorySummary { name, kind, editable, capabilities, model_count, has_key, key_hint, id_prefixes, fetched_at, error }`. A manual `Debug` on the stored config prints the key as `***`.
  - **Server RPCs:**
    - `list_repositories`, `list_repository_models {name, refresh?}`, `save_repository {name, previous_name?, id_prefixes, api_key?}` and `delete_repository {name}`, with the shapes in the spec's RPC table.
    - `list_catalog` gains `repositories`.
    - `repositories_changed` is broadcast after save, delete and refresh.
    - `used_by` scans the template tree for `repository` (default `hive`).
  - `attach_session` and `list_sessions` rows gain `capabilities` (from `Dispatcher::capabilities`, or `null` when unavailable).
  - `main.rs` builds the registry and passes it as the dispatcher's `ProviderSource`. `StaticProviders` from Task 2 is deleted.

- [ ] **Step 1: Write the failing tests:**
  - In registry.rs:
    - `file_round_trips_and_key_never_in_summary`
    - `save_fetches_and_reports_fetch_error_but_saves`
    - `rename_carries_key`
    - `id_prefix_filter_applies`
    - `cache_survives_restart_without_network`
    - `debug_redacts_key`
  - In server.rs:
    - `list_repositories_has_hive_and_openrouter`
    - `rename_and_delete_refused_while_used` (Review Focus 5)
    - `deleted_repository_session_unavailable` (Review Focus 5)
    - `template_with_vanished_model_still_loads` (Review Focus 4)
    - `session_rows_carry_capabilities`
    - `repositories_changed_broadcast_on_save`
    - `openrouter_session_turn_runs_against_fake_server`: an end-to-end turn with one `read_file` tool call through the fake server. It commits, traces, and the transcript has the tool result.
- [ ] **Step 2: Watch them fail.** Run `scripts/hive-test.sh -- runtime::server`. Expected: FAIL.
- [ ] **Step 3: Implement** the items above, and update the server module docs.
- [ ] **Step 4: Watch them pass.** Run `scripts/hive-test.sh -- --tests`, then `--features llamacpp`, then `--restore`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(runtime): repository registry, repositories file and RPCs`.

### Task 6: Loop and trace use provider usage, cost and estimates

**Files:**
- Modify: `backend/src/runtime/tool_loop.rs` (`StepMeter`)
- Modify: `backend/src/runtime/trace.rs`
- Modify: `backend/src/runtime/compaction.rs`

**Interfaces:**
- Consumes: the `turn_done.usage` and `turn_started.estimated` notes from Task 4.
- Produces:
  - **`StepMeter`.**
    - When `turn_done.usage` is present, it sets `prompt_tokens`, `out_tokens` and `think_tokens` from `usage.prompt_tokens`, `completion_tokens` and `reasoning_tokens`, and `cost` from `usage.cost`.
    - `estimated` is true when `turn_started.estimated` was true and no usage arrived.
    - Chunks flagged `reasoning: true` do not feed the Gemma think counter.
  - **`StepRecord`.** It gains `cost: Option<f64>` and `estimated: bool`, serde default and skipped when empty or false.
  - **`CompactionRecord`.** It gains `summary_cost: Option<f64>`, taken from the summary call's usage.
  - **Calibration.** After an answered or tool-calls step with usage, the loop calls nothing extra: the link observes its own usage. This is how calibration is fed.

- [ ] **Step 1: Write the failing tests:**
  - `usage_overrides_counted_tokens_and_sets_cost`
  - `estimated_flag_when_no_usage`
  - `reasoning_chunks_do_not_count_as_gemma_thinking`
  - `pragmatic_steps_unchanged`: there is no usage, so the record is identical to today's.
  - `summary_cost_recorded`
  - `new_fields_skipped_when_empty`
- [ ] **Step 2: Watch them fail.** Run `scripts/hive-test.sh -- runtime::`. Expected: FAIL.
- [ ] **Step 3: Implement** the items above.
- [ ] **Step 4: Watch them pass.** Run `scripts/hive-test.sh -- --tests`, then `--restore`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(runtime): trace provider usage, cost and estimates`.

### Task 7: App: the Models rail item

**Files:**
- Create: `app/src/renderer/src/ModelsService.ts`
- Create: `ModelsPanelViewModel.ts`
- Create: `RepositoryEditorViewModel.ts`
- Create: `RepositoryRowViewModel.ts`
- Create: `RepositoryModelRowViewModel.ts`
- Create: tests for each of those
- Create: `views/ModelsModule.mu`
- Create: `views/ModelsPanel.mu`
- Create: `views/RepositoryEditor.mu`
- Create: an icon svg in `views/icons/`
- Modify: `app.mu`, `views/RuntimeIcons.mu`, `RuntimeClient.ts`, `agents.ts`

**Interfaces:**
- Consumes: the Task 5 RPCs.
- Produces:
  - `RuntimeClient` gains:
    - `ListRepositories()`
    - `ListRepositoryModels(name, refresh)`
    - `SaveRepository({name, previous_name?, id_prefixes, api_key?})`
    - `DeleteRepository(name)`
    - an `OnRepositoriesChanged` signal
  - `ModelsService` mirrors `AgentsService`: `ServiceKey`, `Panel`, `Editor`, `IActivatable`, and an unsaved-changes guard. It shares `SessionsService.Client`.
  - `ModelsPanelViewModel.Rows: ObservableCollection<RepositoryRowViewModel>`, with Text `{name}  {Kind}  {n} models`, `Selected`, and an `AddOpenRouterCommand`.
  - **`RepositoryEditorViewModel`.**
    - For a Pragmatic repository, `IsEditable=false` and it lists models with their runtimes.
    - For an OpenRouter repository it has `Name`, `KeyHint`, `NewKey` (password), `FilterText` (comma separated id prefixes), `SaveCommand`, `DeleteCommand`, `RefreshCommand`, `FetchedText`, `ErrorText`, `HasNoKeyWarning` (with the verbatim text), and `Models: ObservableCollection<RepositoryModelRowViewModel>` (id, context as `200K`, price `3.00/15.00`).
    - Save sends `api_key` only when `NewKey` is non-empty.

- [ ] **Step 1: Write the failing tests:**
  - `panel_lists_repositories_with_counts`
  - `pragmatic_editor_is_read_only`
  - `openrouter_save_sends_key_only_when_changed`
  - `filter_text_splits_to_prefixes`
  - `refresh_shows_fetched_time_and_errors`
  - `no_key_shows_warning`
  - `delete_refused_shows_server_error`
  - `unsaved_changes_guard_on_leave`
  - `repositories_changed_reloads_panel`
- [ ] **Step 2: Watch them fail.** Run `cd app && npx vitest run <files>`. Expected: FAIL.
- [ ] **Step 3: Implement** the items above, registering the capability like Agents.
- [ ] **Step 4: Watch them pass.** Run `cd app && npm test && npm run typecheck:web && npm run typecheck:node`.
  - Then launch the built app with Playwright (`D:/architecture-agent/Plexus/node_modules/playwright` `_electron`; strip `ELECTRON_RUN_AS_NODE`).
  - Open the Models rail and screenshot the list and the editor into `D:/tmp`. **Look at it.** The rows and models must render.
- [ ] **Step 5: Commit** with the message `feat(app): Models rail item for repositories`.

### Task 8: App: repository in templates, and capability-aware sessions

**Files:**
- Modify: `TemplateEditorViewModel.ts`, `views/TemplateEditor.mu`, `NewSessionDialogViewModel.ts`
- Modify: `SessionViewModel.ts`, `views/SessionView.mu`
- Modify: `InstrumentPanelViewModel.ts`, `views/InstrumentPanel.mu`
- Modify: `ChatViewModel.ts`, `messages.ts`, `views/Chat.mu`
- Modify: `TraceViewModel.ts`, `agents.ts`
- Plus the tests for each

**Interfaces:**
- Consumes: `list_catalog.repositories`, row `capabilities`, the trace's `cost` and `estimated`, `turn_chunk.reasoning`.
- Produces:
  - **The template editor.**
    - The Host combo becomes a `Repository` combo (its choices come from `catalog.repositories`).
    - `Model` choices come from that repository.
    - The `Runtime` row is visible only for Pragmatic repositories.
    - Picking an OpenRouter model sets `runtime: "openrouter"` and a default `n_ctx` of `min(context_length, 32768)`.
    - Save sends `repository`, never `host`.
  - **`SessionViewModel.Capabilities`.** `IsLoadable` (`load_unload`) hides Load and Unload.
  - **The instrument panel.**
    - Tokenize, Raw prompt and Logprobs show `not available for this provider` when the capability is false.
    - Context fill text gains a leading `≈` when it is estimated.
  - **The chat.** A chunk with `reasoning: true` appends to the assistant message's reasoning text, shown dimmed above the answer. It is not in the history reload.
  - **The trace.** A step line shows `≈` before ctx when `estimated`, and ` · $0.0012` (4 decimals) when `cost` is set.
    - A compaction line with `summary_cost` appends ` · $x`.

- [ ] **Step 1: Write the failing tests:**
  - `editor_repository_combo_filters_models`
  - `openrouter_model_hides_runtime_and_defaults_ctx`
  - `legacy_host_template_loads_as_hive`
  - `save_sends_repository_not_host`
  - `load_buttons_hidden_without_load_unload`
  - `instrument_features_marked_not_available`
  - `estimated_context_shows_approx`
  - `reasoning_chunks_render_dimmed_not_in_content`
  - `trace_line_shows_cost_and_estimate`
- [ ] **Step 2: Watch them fail.** Run `cd app && npx vitest run <files>`. Expected: FAIL.
- [ ] **Step 3: Implement** the items above.
- [ ] **Step 4: Watch them pass.** Run `cd app && npm test` and both typechecks. Then use Playwright to screenshot the template editor with an OpenRouter repository selected (which needs the Task 9 deploy, or a fake), and look at it.
- [ ] **Step 5: Commit** with the message `feat(app): templates pick a repository; sessions adapt to provider capabilities`.

### Task 9: Deploy and live proof (owner go and key required)

- [ ] **Step 1: Stop for the go.** Run `scripts/hive-test.sh --restore`, then stop and ask the owner for the go to merge and push.
- [ ] **Step 2: After the go:**
  1. Merge to main and push.
  2. On the hive, run `git pull --ff-only`, then `stop-sup.ps1`, then `build-both.cmd`. Expect `CANDLE_EXIT=0 LLAMA_EXIT=0 SUP_EXIT=0`.
  3. Run `schtasks /Run /TN harness-supervisor`. Expect `supervisor.out` to list repositories.
- [ ] **Step 3: Live proof.** The owner pastes the key in the Models rail. Run the spec's six live-proof checks, covering both protocol and UI.
  - Check 6 greps `C:\llm-harness` (excluding `repositories.toml`) and the supervisor output for the key. Expected: no match.
- [ ] **Step 4: Record the result** in the spec status, the roadmap (an inserted "model providers" item) and the memory note.
