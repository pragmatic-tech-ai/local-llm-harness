# Model providers and repositories

**Status:** draft 2026-11-24.
**Roadmap:** inserted before step 5 (planning) at the owner's request.
**Builds on:**
- `docs/specs/2026-11-23-agent-templates-sessions-design.md`: templates, sessions, the dispatcher and `ModelHost`
- `docs/specs/2026-11-24-agent-trace-design.md`: the per-step trace
- `docs/specs/2026-11-24-context-compaction-design.md`: compaction and `measure_prompt`

## Intent

Today every model the harness can run comes from GGUF files in model folders on the hive. It
runs on the hive's own GPU, through the supervisor and a llamacpp or candle worker. The
owner wants the harness to reach models it does not host itself, starting with OpenRouter.
That makes larger and stronger models available to the same agents, tools, trace and
compaction.

The owner's framing:

- **Two concepts that go together.** A **repository** is where model definitions come from. A
  **provider** is what runs those models.
- **The loop and the harness are always ours.** Our host, the runtime on the hive, runs every
  agent turn. A provider is only the place a step's generation happens.
- **The current local path becomes the Pragmatic provider.** It is a provider we control to
  the bone: prompt measuring, tokenizing, raw prompts, logprobs and thinking-token counts are
  all ours.
- **OpenRouter is the second provider.** It may not support everything, and what it lacks is
  declared, not faked.
- **A new rail item configures repositories.** The owner named local ones and third-party ones
  such as OpenRouter and Hugging Face. OpenRouter is the one to build first.

## Goals

- **A provider seam.** The current local implementation moves behind a `Provider` seam as the
  **Pragmatic** provider, with no loss of behaviour.
- **OpenRouter runs agent turns.**
  - It is the second provider, running agent turns through OpenRouter's chat completions API
    over HTTPS from the hive.
  - Tool calling, streaming, the trace and compaction all work.
  - Compaction runs on estimated token counts.
- **Repositories supply model definitions.**
  - The **Pragmatic repository** is today's model-folder discovery.
  - The **OpenRouter repository** is OpenRouter's model list, narrowed to models that support
    tools and, optionally, to id prefixes.
- **Declared capabilities.** Each provider declares what it supports. The loop, the instrument
  panel and the session view adapt: features a provider lacks are hidden or marked
  "not available", never broken.
- **A "Models" rail item.**
  - It lists repositories and shows each one's models.
  - It lets the owner add, edit and delete OpenRouter repositories (name, API key, filter).
  - The Pragmatic repository is shown read-only.
- **Templates pick a repository and a model from it.** Existing templates keep working
  unchanged.

## Non-goals

- Hugging Face, in any form. It is a later repository kind.
- Editing the Pragmatic repository's model folders from the app. They stay in the hive's
  catalog file.
- Remote Pragmatic hosts, meaning other machines running our worker.
- Spending caps, budgets or billing views beyond the per-step cost in the trace.
- Logprobs, tokenize or raw rendered prompts for OpenRouter models.
- OpenRouter's provider routing preferences, model fallbacks or prompt caching controls.
- Moving the agent loop into the app, or calling OpenRouter from the app.

## Anti-goals

- An API key that reaches the app, the trace, logs, git, or any file other than the hive's
  repositories file.
- An OpenRouter session that silently loses tools, the trace or compaction.
- An instrument the provider cannot feed showing invented numbers. An estimate is labelled as
  one, and a missing value reads "not available".
- An existing template or session that stops loading, or changes which model it runs.
- A runaway spend loop. The template's step budget caps every turn, as it does today.

## Approaches considered

### How an API provider plugs into our host

- **A. The provider runs inside the runtime.** **Chosen by the owner.** The runtime calls
  OpenRouter over HTTPS for each step, behind the same link the loop uses for a local worker.
  - Nothing is loaded or evicted.
  - Many API models can serve sessions at once while the GPU serves a local one.
- **B. A provider worker process.** This is a new worker kind that the supervisor launches,
  which forwards to OpenRouter. It keeps one worker protocol, but it forces always-ready API
  models into the one-model-at-a-time load/evict lifecycle and adds a process per model.
- **C. The app calls OpenRouter.** This needs no new crates, but it makes the app a second
  agent loop, against the rule that the loop and the harness are always ours.

### How the hive speaks HTTPS

- **Windows' built-in TLS.** **Chosen by the owner ("yes").** It turns on reqwest's
  `native-tls` feature, which uses SChannel and the Windows certificate store.
  - It adds a few small crates, vendored once, with no C toolchain.
  - The hive is Windows.
  - It also unblocks the parked web fetch.
- **rustls.** It is portable, but it adds more crates, including `ring`, which needs a C or
  assembly build.
- **`curl.exe`.** It adds no crates, but streaming through a child process is fragile.

## Design

### Concepts and names

| Concept | What it is | Variants now |
|---|---|---|
| Repository | a named source of model definitions, bound to one provider | `pragmatic`, `openrouter` |
| Provider | what runs a model for a step; declares capabilities | Pragmatic (today's `LocalSupervisorHost`), OpenRouter |
| Host | our runtime on the hive; runs the loop, tools, trace and compaction for every session | one, ours |

- **Repository and provider pair up.** Each repository owns exactly one provider instance:
  - the Pragmatic repository owns the local supervisor provider;
  - each OpenRouter repository owns an OpenRouter provider with its own API key.
- **The rename.** The code's `ModelHost` trait becomes `Provider`, and `LocalSupervisorHost`
  becomes `PragmaticProvider`. The dispatcher routes by repository name.
- **Routing is exact, with no fallback.** This is my call. `ModelRequirement.host` becomes
  `repository: String`, filled from the template and defaulting to `hive`. The dispatcher sends
  a session only to the provider of that repository; today's "first host that serves it"
  fallback is removed, so a template can never silently run elsewhere. The placement reads
  `unavailable` in these cases:
  - `repository not found: {name}` when the repository is missing;
  - `repository {name} has no API key` when an OpenRouter repository has no key;
  - `model not in repository {name}: {model}` when the model is not served.
- **Identity.** A provider's `identity()` is per repository and model. Two OpenRouter
  repositories listing the same model id are two identities.
- **Names on the wire.** The words *repository* and *provider* are the ones used in the app and
  on the wire.

### Repositories

**The Pragmatic repository.**

- **It always exists.** It is named `hive`, the implicit host name today, so existing
  templates with `host = "hive"` resolve unchanged.
- **Its models are today's discovery.** That is the GGUF files in the catalog's model folders,
  each with the runtimes that can load it.
- **It is read-only in the app.** Its folders and runtimes stay in the catalog file.
- **It follows the catalog's hosts.** If the catalog configures `[[host]]` entries, each one
  is a Pragmatic repository under its name, as hosts are today. At most one is local, which is
  today's rule.

**OpenRouter repositories** are stored in `{agent data dir}/repositories.toml`, next to the
templates folder:

```toml
[[openrouter]]
name = "openrouter"          # unique across all repositories; 1-64 of A-Z a-z 0-9 _ -
api_key = "sk-or-..."        # never leaves the hive
id_prefixes = ["anthropic/", "google/"]   # optional; empty = all tool-capable models
```

- **The file.** It is written atomically, the same way templates are. It is never committed
  anywhere. It holds the key in plain text on the single-user hive, with default file
  permissions. This is my call.
- **Keeping the key out.**
  - The repository type's `Debug` prints `api_key: "***"`.
  - No RPC result or notification carries the key; `key_hint` is the last 4 characters.
  - reqwest errors are reported with `without_url()`.
  - The key never goes into the trace, the session file or a log line.
- **Keyless repositories.** A repository without a key is allowed: the model list needs no
  key. Its sessions read `unavailable` until a key is saved, and the Models editor shows
  `no API key: models can be listed but not run`.
- **Fetching models.** The model list comes from `GET https://openrouter.ai/api/v1/models`,
  which needs no key.
- **Keeping models.** A model is kept when its `supported_parameters` contains `tools`, and
  when it matches one of `id_prefixes`, if any are set.
- **The cache.** The list is cached in memory per repository and refreshed when the owner asks.
  `save_repository` also fetches it straight away. The save succeeds even when that fetch fails,
  and the result then carries `fetch_error`.
  It is also written to `{agent data dir}/cache/openrouter-{name}.json`, so a restart without
  network still knows the models.
- **Model definition fields:**
  - `id` is OpenRouter's model id, which is also the template's `model`.
  - `name`.
  - `context_length`.
  - `prompt_price` and `completion_price`, in USD per million tokens, from `pricing.prompt`
    and `pricing.completion`.
  - `reasoning`, true when `supported_parameters` contains `reasoning`.

### Template fields

- **The repository field.** `host: Option<String>` becomes `repository: Option<String>`.
  - TOML and JSON still accept `host` as an alias on read.
  - A missing value means `hive`, the Pragmatic repository, as "any host" works today.
  - The validation error changes from `unknown host: {v}` to `unknown repository: {v}`.
- **Pragmatic models.** For them, `runtime` and `model` keep today's meaning and validation.
- **OpenRouter models.**
  - `runtime` is the fixed value `openrouter`.
  - `model` must be a model id in that repository's current list. Otherwise the error is
    `unknown model: {v}`, today's text.
  - `n_ctx` must be at most the model's `context_length`. Otherwise the error is
    `n_ctx exceeds the model's context length ({n})`.
  - The editor's default `n_ctx` for an OpenRouter model is `min(context_length, 32768)`. This
    is my call: compaction then keeps cost bounded by default.
- **Load never fails on a model list.** This is my call. Templates and sessions always load.
  The model-list check runs only when a template is saved; the tree marks a template whose
  model has since left the list, using today's validation-error marker. At turn time an
  OpenRouter provider serves any model when its list is unknown, and lets OpenRouter answer.
  When the list is known and lacks the model, the placement reads `unavailable` as above.
- **A list that cannot be fetched.** If an OpenRouter repository's list is unknown (never
  fetched, no cache), validation of its models fails with
  `model list unavailable for repository {name}`, and saving is refused. This is my call:
  better than accepting a typo.

### The provider seam

`Provider` is today's `ModelHost` trait plus one method:

```rust
fn capabilities(&self) -> ProviderCapabilities;

pub struct ProviderCapabilities {
    pub measure_prompt: Measure,   // Exact | Estimated
    pub tokenize: bool,
    pub raw_prompt: bool,
    pub logprobs: bool,
    pub load_unload: bool,         // false: always ready, nothing to load or evict
}
```

| | Pragmatic | OpenRouter |
|---|---|---|
| measure_prompt | Exact (worker) | Estimated |
| tokenize | yes | no |
| raw_prompt | yes | no |
| logprobs | yes | no |
| load_unload | yes | no |
| thinking tokens | counted from the stream | `usage.completion_tokens_details.reasoning_tokens` |
| cost | none | `usage.cost` (USD) |

An **always-ready provider** behaves like this:

- `serves` is true for any model in its repository's list.
- `current_link` and `ensure_ready` always return a link.
- `state().loaded` reports the requirement.
- `identity()` is stable per repository and model.
- `unload_if` returns `Ok(false)`.
- Placement therefore always reads `ready`. `evicts` is never set, and the app hides Load and
  Unload when `load_unload` is false.

**`capabilities` on the wire:**

- `attach_session` and `list_sessions` rows gain `capabilities` (the struct above, with
  `measure_prompt` as `"exact"` or `"estimated"`).
- The `list_catalog` result gains it per repository.

### The OpenRouter link

`OpenRouterLink` implements `WorkerLink`. The loop and compaction use it unchanged.

**Calls and transport.**

- **Generate.** `generate` sends `POST {base_url}/chat/completions` with `stream: true` and
  `usage: {include: true}`. `base_url` defaults to `https://openrouter.ai/api/v1` and is
  overridable for tests.
  - The headers are `Authorization: Bearer {key}` and
    `X-Title: Pragmatic LLM Harness`.
  - No `HTTP-Referer` is sent. This is my call, so the owner's details stay out of it.
- **Messages.** These are our `ChatMessage`s in OpenAI form:
  - system, user and assistant messages as they are;
  - assistant tool calls as `tool_calls: [{id, type: "function", function: {name, arguments: <JSON string>}}]`;
  - tool results as `{role: "tool", tool_call_id, content: <JSON string>}`.
- **Tools.** They are the same OpenAI-shape tool list the loop builds today.
- **Sampling.** Each setting passes through when set:
  - `temperature`, `top_p` and `top_k`;
  - `repeat_penalty` as `repetition_penalty`;
  - `seed`, when it is not -1;
  - `max_tokens`, when it is not -1.
- **Thinking.** `enable_thinking` sends `reasoning: {enabled: true}` when the model has
  `reasoning`. Otherwise nothing is sent.

**How the stream maps to our notes,** in order:

- **`turn_started`,** sent once before the first delta. It carries:
  - `prompt: null`;
  - `prompt_tokens`, the estimate;
  - `n_ctx`, the requirement's;
  - `estimated: true`.
- **`turn_chunk`,** for each content delta. It carries `{text, tokens: [], stop: false}`.
  Reasoning deltas are sent as `turn_chunk {text, tokens: [], stop: false, reasoning: true}`.
  - The app shows reasoning text dimmed in the assistant message.
  - Notes are loose JSON, so older consumers ignore the new field.
  - Thinking tokens for OpenRouter come only from `usage.reasoning_tokens`, not from counting
    chunks.
  - It is never part of the committed content.
- **Tool calls.** Their deltas are accumulated by index and emitted once, complete, in
  `turn_done`.
- **`turn_done`.** At the end it carries `{content, tool_calls, usage}`, where:
  - `usage` is `{prompt_tokens, completion_tokens, reasoning_tokens, cost}` from the final
    usage chunk;
  - `tool_calls` uses the same parsed-call shape the worker produces.
  - Arguments that are not valid JSON become a call with `parse_error` set, which is today's
    path.

**Errors.**

- **Context too long.** An HTTP 400 whose error message mentions the context length becomes
  `turn_rejected {needed: 0, total: n_ctx}`, so the turn ends `window_full`.
- **Other errors.** Any other HTTP status, or a mid-stream `error` object, becomes
  `turn_error {message: "openrouter: {status} {error.message}"}`. Nothing from the request,
  and never the key, is echoed.
- **Network failure** becomes `turn_error {message: "openrouter: {reqwest error}"}`.

**The other link methods.**

- **`measure_prompt`** returns an estimate. The estimate is the serialized messages' and tools'
  character count divided by 4. After the first answered step of the session in this process,
  it is scaled by the ratio of the last real `usage.prompt_tokens` to its own estimate for
  that prompt. It is labelled `estimated: true`.
  - The ratio is kept per repository and model inside the provider, shared by every session
    on that model. This is my call: the trait carries no session, and the ratio describes the
    model's tokenizer, not the session.
  - The estimate only decides when compaction runs. It never refuses a step; only a real 400
    from OpenRouter ends a turn `window_full`.
  - The template's `n_ctx` is the compaction window, as for local models.
  - The summarize strategy's summary call goes through the same link and costs money. Its cost
    is recorded in the compaction record as `summary_cost`, serde default and skipped when
    absent.
- **`model_props`** returns `{n_ctx, model_name: id, supports_tools: true}`.
- **`tokenize`** returns the error `not supported by this provider`.

**The trace.**

- **The step meter.** It prefers `turn_done.usage` over counted chunks when usage is present,
  for prompt, output and thinking tokens.
- **New step fields.** `StepRecord` gains:
  - `cost: Option<f64>`;
  - `estimated: bool`, which marks a prompt token count that is an estimate;
  - both serde default and skipped when empty or false.
- **The trace line.** It shows `≈` before an estimated ctx and `· ${cost:.4}` when a cost is
  present.

### HTTPS

- **The feature.** `reqwest` gains the `native-tls` feature. Its crates are vendored into the
  backend's vendor directory once, and `Cargo.lock` is updated.
- **Scope.** This lifts the "no new crates" rule for exactly these crates.
- **Who vendors.** `cargo vendor vendor` runs once on the dev machine. It only fetches and
  compiles nothing, and it vendors every platform's crates, the Windows-only ones included.
  The result is committed, and the hive builds offline as today.
- **Plain HTTP in tests.** Tests run OpenRouter against a local plain-HTTP fake server through
  `base_url`.

### RPCs

| Method | Params → result | Notes |
|---|---|---|
| `list_repositories` | → `[{name, kind: "pragmatic"\|"openrouter", editable, capabilities, model_count, has_key?, key_hint?, id_prefixes?, fetched_at?, error?}]` | `key_hint` = last 4 chars; the key itself never |
| `list_repository_models` | `{name, refresh?: bool}` → `{models: [...], fetched_at, error?}` | Pragmatic: discovery models; OpenRouter: cached list, refetched when `refresh` |
| `save_repository` | `{name, previous_name?, id_prefixes, api_key?}` → `{name, fetch_error?}` | OpenRouter only; omitted `api_key` keeps the stored one (a rename carries it); validates the name; a rename is refused while templates use the old name, with `repository is used by templates: {names}` |
| `delete_repository` | `{name}` → `{}` | OpenRouter only; refused while a template references it: `repository is used by templates: {names}` |
| `list_catalog` | unchanged shape + `repositories: [{name, kind, capabilities, models: [...]}]` | the editor's source for repository/model choices; `hosts` stays for older apps |

A `repositories_changed` notification is broadcast after a save, a delete or a refresh.

### App

**The "Models" rail item.** It is a third capability, registered like Agents and Sessions:

```
Models                                   ┌ OpenRouter ─────────────────────────────┐
  ● hive        Pragmatic   12 models    │ Name      [openrouter      ]            │
  ● openrouter  OpenRouter  48 models    │ API key   [••••••••••••7f2a] [Change]   │
  [Add OpenRouter repository]            │ Filter    [anthropic/, google/      ]   │
                                         │ [Save] [Refresh models]  fetched 14:02  │
                                         │ id                      ctx    $in/$out │
                                         │ anthropic/claude-…      200K   3.00/15  │
                                         └─────────────────────────────────────────┘
```

- **The side panel** lists repositories with their kind and model count.
- **The editor.**
  - The Pragmatic repository shows its runtimes and models, read-only.
  - An OpenRouter repository shows its name, a masked key with a Change field, its filter (id
    prefixes, comma separated), Save, Delete, "Refresh models", and the model table: id,
    context, and the price in and out per million tokens.
  - Unsaved changes are guarded when leaving the rail, as the template editor does.

**The template editor.**

- The Host combo becomes a **Repository** combo.
- The Model combo lists that repository's models.
- The Runtime combo shows only for Pragmatic repositories.
- The context default follows the rule above.

**The session view and instrument panel.** They read the session's `capabilities`.

- Load and Unload are hidden when `load_unload` is false.
- Tokenize, Raw prompt and Logprobs show `not available for this provider` when unsupported.
- Context fill shows `≈` when estimated.
- Reasoning chunks render dimmed in the assistant message.
- The trace shows cost and `≈`, as described above.

## Left to the builder

- **Type shapes.** The Rust module layout (`backend/src/providers/` is suggested) and the exact
  types of the repository registry.
- **Message mapping.** How chat messages map to the OpenAI format, within the rules above.
- **SSE parsing.** reqwest streams the body, and we split on blank lines and `data:` lines.
  No new crate is used for SSE.
- **Panel placement.** Exact layout of the Models editor, as long as it carries the listed
  fields.
- **Test layout.** Unit tests sit next to the code, as today. OpenRouter is tested against a
  local fake server.

## Live proof (after deploy)

The owner supplies an OpenRouter API key in the app. The proof costs cents.

1. Add an OpenRouter repository with the filter `google/`, then refresh. Models appear with
   context and prices, and the key shows only as its last 4 characters.
2. Make a template on one inexpensive tool-capable model, with `read_file` and `note_add`.
   Ask it to read `/sandbox/notes.txt` and note its first line.
   - The tool calls run.
   - The trace shows tokens from usage and a cost.
   - The Notes panel fills.
3. Ask for a long answer three times with `compact_at_pct` 20. A trim compaction shows `≈`
   counts.
4. Check the instrument panel. Tokenize, Raw prompt and Logprobs say "not available for this
   provider", and Load and Unload are hidden.
5. Check that a Pragmatic session on gemma-4 still works as before, including the raw prompt,
   logprobs and exact counts.
6. Search for the key on the hive. `grep` over the trace logs, sessions and supervisor output
   finds nothing.
