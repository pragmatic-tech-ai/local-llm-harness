### Task 2: Sessions and the session store

**Files:**
- Create: `backend/src/agents/session.rs`, `backend/src/agents/session_store.rs`
- Modify: `backend/src/engine/types.rs` (round-trip test only, if `Deserialize` is already derived)

**Interfaces:**
- Consumes: Task 1 `AgentTemplate`, `TemplateStore::get`.
- Produces (`session.rs`):
  - `SessionRecord { schema: u32 /*1*/, id: String, title: String, template_id: String, snapshot: AgentTemplate, created: u64, updated: u64, sampling: Option<SamplingConfig>, enable_thinking: Option<bool>, transcript: Vec<ChatMessage> }`. It is JSON-serde, and times are unix seconds.
  - `SessionSummary { id, title, template_id, template_name, runtime, model, updated }`.
- Produces (`session_store.rs`):
  - `SessionStore::open(root: &Path) -> io::Result<Self>`, which loads every `*.json`. A file that fails to parse is skipped and the error is printed to stderr.
  - `create(&self, template_id: &str, template: &AgentTemplate) -> SessionRecord`. The transcript is `[System(system_prompt)]` when the prompt is non-empty, otherwise `[]`. The title is `"{name} #{n}"`, where n is 1 + the count of existing sessions with that `template_id`. Ids are `s-{unix_millis}-{counter}`.
  - `list(&self) -> Vec<SessionSummary>`, newest `updated` first.
  - `get(&self, id) -> Option<SessionRecord>`
  - `rename(&self, id, title)`
  - `delete(&self, id)`
  - `commit(&self, id, transcript: Vec<ChatMessage>)`
  - `set_overrides(&self, id, sampling: Option<SamplingConfig>, enable_thinking: Option<bool>)`
  - `reset(&self, id)`, which goes back to the initial transcript from the snapshot.
  - Each of these persists atomically and returns `Result<_, SessionError>`, where `SessionError(String)` uses `unknown session: {id}`.
- **Busy state is not stored here.** It is runtime state (Task 6) and never touches the record.

- [ ] **Step 1: Write the failing tests:**

```rust
create_snapshots_template_and_titles      // titles "Researcher #1", "Researcher #2"; transcript[0] is System(prompt)
empty_prompt_starts_empty_transcript
snapshot_is_independent_of_template_edits // edit + save the template after create → record.snapshot is unchanged
session_survives_template_delete          // delete template → get(id) is still Some with the same snapshot
commit_persists_across_reopen             // commit, reopen the store → same transcript (ChatMessage round-trip, tool_calls included)
busy_is_never_persisted                   // the JSON has no busy field; reopen → nothing is busy
reset_restores_initial_transcript
overrides_round_trip
corrupt_session_file_is_skipped
```

- [ ] **Step 2: Run, expecting FAIL.** **Step 3: Implement.** **Step 4: Run, expecting PASS**, then run the whole lib.
- [ ] **Step 5: Commit:** `feat(agents): persisted sessions with template snapshots`.

