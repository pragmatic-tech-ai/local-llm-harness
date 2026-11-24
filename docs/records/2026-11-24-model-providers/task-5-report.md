# Task 5 report: repository registry, repositories file and RPCs

Status: DONE. Commit `1df786ce feat(runtime): repository registry, repositories file and RPCs` on `feat/model-providers` (not pushed).

## Implemented

- **`backend/src/providers/registry.rs` (new).** `RepositoryRegistry::open(data_dir, pragmatic, discovery, base_url) -> io::Result<Self>` reads `repositories.toml` (`[[openrouter]] name, api_key?, id_prefixes`) and each `cache/openrouter-{name}.json`. A file that does not parse, or has an invalid or duplicate name (a clash with a Pragmatic name counts), is an `InvalidData` error.
  - It implements `ProviderSource`, Pragmatic repositories first.
  - `list()` returns `RepositorySummary { name, kind, editable, capabilities, model_count, has_key?, key_hint?, id_prefixes?, fetched_at?, error? }`.
  - `models(name, refresh)` returns `RepositoryModels { models, fetched_at, error? }`. Pragmatic gives discovery's models. OpenRouter gives the served list, and fetches when `refresh` is set or the list is unknown.
  - `save(...)` is a synchronous `store` step (checks, atomic file write via `AtomicFile`, swap in memory, cache-file rename on a rename) followed by a fetch. A failed fetch keeps the old list and sets `fetch_error`. A rename carries the key, the list and the cache file.
  - The other methods are `delete(name, used_by)` (also removes the cache file), `rules()` (OpenRouter `RepositoryRules`: runtime `openrouter`, `context_length` set, `models: None` while unknown) and `catalog()` (for `list_catalog`).
  - `OpenRouterConfig` has a manual `Debug` that prints `api_key: Some("***")`. `key_hint` is the last 4 characters.
- **`providers/mod.rs`.** `StaticProviders` is deleted. A `#[cfg(test)] FixedProviders` with the same shape replaces it for the dispatcher and provider unit tests.
- **`runtime/catalog_source.rs`.** Adds `RulesWithRepositories<R>`, which wraps a rules source and adds `registry.rules()` once `attach(registry)` is called. It has to be attached later because the registry needs the providers, which exist only after the template store opens.
- **`agents/template_store.rs`.**
  - `templates_using(repository) -> Vec<String>` returns template names, sorted; no repository means `hive`.
  - The unused imports at line 13 (`RepositoryKind`, `RepositoryRules`) moved into the tests module, and the build is now warning-free.
- **`runtime/server.rs`.**
  - `RuntimeDeps.repositories` is added.
  - New RPCs: `list_repositories`, `list_repository_models {name, refresh?}`, `save_repository {name, previous_name?, id_prefixes, api_key?}` (returns `{name, fetch_error?}`) and `delete_repository {name}` (returns `{}`).
  - `used_by` is a closure over `templates.templates_using`.
  - `repositories_changed` is broadcast after a save, a delete, or a listing with `refresh: true`. Each of these also calls `dispatcher.refresh()`, so `PlacementWatcher` announces changed placements.
  - `list_catalog` adds `repositories: [{name, kind, capabilities, models}]` and keeps `hosts`.
  - `SessionRow.capabilities` is `Option<ProviderCapabilities>` (`null` when the repository is gone), so it appears in `list_sessions`, `create_session` and `attach_session.summary`. `attach_session` also has it at the top level, like `placement`.
  - The module docs are updated.
- **`runtime/test_support.rs`.** `TestRuntime` builds a real registry over its temp dir (`hive` = the `FakeHost`, OpenRouter at `OFFLINE` = `http://127.0.0.1:1`), and its template rules are `RulesWithRepositories`. `TestRuntime::openrouter(tag, link, base_url)` is sandboxed and uses the given base url.
- **`main.rs`.** `into_deps` builds the registry (base url `OpenRouterApi::DEFAULT_BASE_URL`), attaches it to the template rules and passes it to `Dispatcher::new`. It now returns `Result`. A bad repositories file aborts startup with `repositories unavailable: ...`. New test: `malformed_repositories_file_is_an_error`.

## Tests

- Registry: `file_round_trips_and_key_never_in_summary`, `save_fetches_and_reports_fetch_error_but_saves`, `rename_carries_key`, `id_prefix_filter_applies`, `cache_survives_restart_without_network`, `debug_redacts_key`, plus the extra `unparsable_file_error_never_quotes_the_key`.
- Server: `list_repositories_has_hive_and_openrouter`, `rename_and_delete_refused_while_used`, `deleted_repository_session_unavailable`, `template_with_vanished_model_still_loads`, `session_rows_carry_capabilities`, `repositories_changed_broadcast_on_save` and `openrouter_session_turn_runs_against_fake_server`.
  - The last one is end to end through `FakeHttp`: list, then a `read_file` tool call on `/sandbox/notes.txt`, then the answer. It checks 2 trace steps, `trace_turn` answered/committed, the tool result in the transcript, the `Bearer` header, that the second request carries the tool result, and that the key is in no session or trace file and no frame.
- Template store: `templates_using_names_the_repository_users`.
- Main: `malformed_repositories_file_is_an_error`.
- `list_all_tools_and_catalog` is updated for `repositories`.

## RED/GREEN evidence

- **RED.** The tests and the implementation were written in one pass, so a RED run before the implementation does not exist. To prove the tests bite, I used a mutation run instead: `check_unused` was forced to `Ok` and `SessionRow.capabilities` was forced to `None`. Then `scripts/hive-test.sh -- runtime::server` gave `test result: FAILED. 71 passed; 3 failed`. The 3 failures were `deleted_repository_session_unavailable`, `session_rows_carry_capabilities` and `rename_and_delete_refused_while_used`. The files were restored afterwards (`cmp`-verified).
- **GREEN, default features.** `scripts/hive-test.sh -- --tests`: lib `test result: ok. 608 passed; 0 failed`; main 8, http_engine 10, http_stream 6, http_warmup 2, https_smoke 0 (1 ignored), rpc 5; `TEST_EXIT=0`. There were no compiler warnings (`Compiling backend`, then straight to `Finished`).
- **GREEN, `--features llamacpp`.** `scripts/hive-test.sh --features llamacpp -- --tests`: lib `test result: ok. 616 passed; 0 failed`; the same integration counts; `TEST_EXIT=0`; no warnings. This run came after the final change.
- **Restore.** `scripts/hive-test.sh --restore` printed `RESTORED`.
- **Formatting.** `rustfmt --edition 2021 --check` is clean on every changed file, and all files are LF.

## Deviations

1. `save`'s `used_by` is `&(dyn Fn(&str) -> Vec<String> + Sync)`, not `&dyn Fn`. The future is awaited inside a spawned task, so it must be `Send`. `delete` keeps `&dyn Fn`.
2. `StaticProviders` is gone from production code. A test-only `FixedProviders` keeps the dispatcher and provider unit tests independent of the registry.
3. The registry is attached to the template rules through the new `RulesWithRepositories` wrapper, not a field on `DiscoveredCatalog`. This lets `TestRuntime`'s fixed rules use it too.
4. Vanished-model marking needed no new field: the tree's existing `error` marker carries `unknown model: {id}` from save-time validation (spec: "using today's validation-error marker").
5. My own choices where the spec says nothing:
   - **Picking the repository.** `previous_name` (or `name`) picks the repository to update; none by that name creates one. A `previous_name` that does not exist reads `repository not found: {name}`.
   - **Blank fields.** A blank `api_key` keeps the stored key, like an omitted one, so there is no way to clear a key except deleting the repository. Blank prefixes are dropped.
   - **Short keys.** A key of 4 characters or fewer gets no `key_hint`, since the hint would be the whole key.
   - **New error texts** (named consts): `repository name must be 1-64 letters, digits, _ or -`, `repository already exists: {name}`, `repository is not editable: {name}`, `repositories file: ...` and `repositories unavailable: ...`.
   - **Listing an unknown list.** `list_repository_models` without `refresh` fetches when the list is unknown, and does not broadcast.
   - **Times.** `fetched_at` is Unix seconds.
6. A malformed repositories file aborts supervisor startup rather than disabling only the runtime, because the registry is built after the runtime port is bound.

## Concerns

- A toml parse error is reported by line only (`does not parse at line N`), because toml's own message quotes the offending line, which may hold the key. This was found in self-review and is covered by a test.
- `Dispatcher::subscribe` merges the provider signals it saw on first subscribe, so OpenRouter providers added later are not merged. This is harmless today because they never signal; registry changes go through `dispatcher.refresh()`.
- Plaintext key in `repositories.toml` with default permissions, as ruled.

## Fix round 1

Commit: `fix(runtime): bad repositories file disables the runtime, not the supervisor` (see `git log`).

- **Important 1: a bad repositories file disables only the runtime.**
  - `RepositoryRegistry::open` is split into two steps. `load(data_dir, pragmatic_names) -> io::Result<LoadedRepositories>` parses the file and checks names: invalid, duplicate, or clashing with a host. `build(data_dir, loaded, pragmatic, discovery, base_url) -> Self` is infallible. `open` is now load followed by build.
  - `AgentsStack::open_stores` calls `load` with `host_names`. Its error becomes `agents data unavailable: repositories file: ...`, which goes to `RuntimeStatus::disabled` through the existing path. The control plane still binds.
  - `into_deps` is infallible again and builds from the loaded repositories. The `ERR_REPOSITORIES` const is removed, and the run_supervisor policy comment is updated.
  - The test is replaced by `malformed_repositories_file_disables_the_runtime`. It covers a name clashing with a host and an unparsable line holding a key. It asserts the exact disabled reason, that the key is absent, and the `RuntimeStatus::disabled` fields.
  - New registry test: `load_checks_the_file_against_the_pragmatic_names`.
- **Minor: every fetch now counts as a refresh.** `RepositoryModels` gains `#[serde(skip)] fetched: bool`. `list_repository_models` broadcasts `repositories_changed` and calls `dispatcher.refresh()` whenever a fetch ran: a refresh, or the first listing of an unknown list. `repositories_changed_broadcast_on_save` is extended to check three cases: a fetching plain listing announces once, a plain listing of a known list announces nothing, and a refresh announces once. It also checks that `fetched` is not on the wire.
- **Minor: a fetch overtaken by a rename or delete writes nothing back.**
  - The fetch itself still runs outside the lock (`RepositoryRegistry::fetch`). Its outcome (the in-memory list, the error and the cache file) is kept only if the entry is still the live one for its name (`Arc::ptr_eq`), checked under the lock.
  - The rename's cache move and the delete's cache removal now happen under the lock too.
  - A failed cache move is logged as `model cache not moved for repository {from} -> {to}: {e}`, with no key. NotFound (no cache) is not logged.
  - New test: `a_fetch_overtaken_by_a_delete_writes_no_cache`, where the list is streamed in slow parts and the delete lands mid-fetch.
- **Kept as ruled:** a blank `api_key` keeps the stored key.
- **Evidence:**
  - RED (mutation): with the live check forced to true, `scripts/hive-test.sh -- providers::registry` gave `FAILED. 8 passed; 1 failed` (`a_fetch_overtaken_by_a_delete_writes_no_cache`). The file was restored and `cmp`-verified.
  - GREEN: `-- --tests` gave lib `ok. 610 passed`, and `--features llamacpp -- --tests` gave lib `ok. 618 passed`. All integration suites passed, `TEST_EXIT=0` both times, no warnings.
  - `--restore` printed RESTORED, and `rustfmt --check` is clean.
