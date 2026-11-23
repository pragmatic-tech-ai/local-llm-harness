### Task 1: Agent templates and the template store

**Files:**
- Create: `backend/src/agents/mod.rs`, `backend/src/agents/template.rs`, `backend/src/agents/template_store.rs`. Add `pub mod agents;` to `backend/src/lib.rs`, keeping module order alphabetical.

**Interfaces:**
- Produces (`template.rs`):
  - `AgentTemplate { name: String, system_prompt: String, tools: Vec<String>, step_budget: usize, runtime: String, model: String, n_ctx: u32, enable_thinking: bool, sampling: Option<SamplingConfig>, host: Option<String> }`. It is TOML-serde, with defaults for `system_prompt` (`""`), `step_budget` (8), `enable_thinking` (false), and `sampling`/`host` (`None`).
  - `TemplateRules { runtimes: Vec<String>, models: Vec<(String, Vec<String>)>, tools: Vec<String> }`, where `models` pairs each model name with its runtimes.
  - `TemplateRules::validate(&self, t: &AgentTemplate) -> Result<(), String>`. Failure messages: `unknown tool: {name}`, `unknown runtime: {rt}`, `unknown model: {m}`, `runtime {rt} cannot load model {m}`, and `step_budget must be at least 1`.
- Produces (`template_store.rs`):
  - `TemplateNode::Group { name, path: String, children: Vec<TemplateNode> } | Template { id, path, name, runtime, model, error: Option<String> }`. It is Serialize, tagged `kind` with the values `group` and `template`. `path` is `/`-separated relative to `templates/`, and `""` is the root.
  - `TemplateStore::open(root: &Path, rules: TemplateRules) -> io::Result<Self>`. It creates `root` and seeds `Examples/Default.toml` when `root` is missing or empty.
  - `tree(&self) -> Vec<TemplateNode>`, sorted with groups first, then by name.
  - `get(&self, id) -> Result<(AgentTemplate, String /*path*/), TemplateError>`
  - `save(&self, id: Option<&str>, path: &str, t: &AgentTemplate) -> Result<String /*id*/, TemplateError>`, which validates first.
  - `delete(&self, id)`
  - `create_group(&self, path)`
  - `rename_group(&self, path, new_name)`
  - `delete_group(&self, path)`. Fails with `group is not empty` unless the group is empty.
  - `move_template(&self, id, path)`
  - `TemplateError(String)`
- **Ids:** an id is the file stem: a slug of `name` (lowercase ASCII alphanumerics and `-`, other runs collapse to `-`), with `-2`, `-3`, … appended on a collision anywhere in the tree. Ids are unique across the whole tree, so `id` alone locates a file.
- **Writes:** writes are atomic (write a temp file, then rename) and serialised behind a `Mutex`.
- **Path safety:** paths are validated. A `..` component, an absolute path or a drive prefix fails with `invalid path`.
- **Seed `Default`:** runtime `llamacpp`. The model is the first entry in `rules.models` whose runtimes include `llamacpp` and whose name contains `gemma-4`, otherwise the first one llamacpp lists. It uses `n_ctx` 8192, every tool in `rules.tools`, `step_budget` 8 and an empty prompt.

- [ ] **Step 1: Write the failing tests** (temp dir per test):

```rust
seeds_examples_default_when_empty         // tree == [Group{name:"Examples", children:[Template{id:"default", name:"Default"}]}]
round_trips_template_toml                 // save then get returns an equal AgentTemplate
directory_tree_maps_to_groups             // a/b/x.toml → Group a → Group b → Template x
rejects_unknown_tool_runtime_model        // the three exact messages above
broken_template_is_listed_with_error      // invalid TOML file → Template{error: Some(_)} with the rest of the tree intact; get() returns Err
slug_ids_are_unique                       // two "My Agent" saves → ids "my-agent" and "my-agent-2"
delete_group_requires_empty               // Err("group is not empty")
move_template_changes_path_keeps_id
rejects_escaping_paths                    // "../x", "C:/x" → Err("invalid path")
```

- [ ] **Step 2: Run** `scripts/hive-test.sh -- agents::` and expect it to FAIL (missing module).
- [ ] **Step 3: Implement** the three files.
- [ ] **Step 4: Run, expecting PASS.** Also run the whole lib.
- [ ] **Step 5: Commit:** `feat(agents): agent templates and the template store`.

