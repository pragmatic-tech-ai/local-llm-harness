### Task 1: Template fields and the saved summary on the session

**Files:**
- Modify: `backend/src/agents/template.rs` (the fields, a `ContextStrategy` enum and validation)
- Modify: `backend/src/agents/session.rs` (`SessionRecord.compaction`)
- Modify: `backend/src/agents/session_store.rs` (`set_compaction`; `reset` clears it)

**Interfaces:**
- Produces:
  - `#[serde(rename_all = "lowercase")] pub enum ContextStrategy { Off, Trim, Summarize }`, with `Default = Off`.
  - On `AgentTemplate`:
    - `#[serde(default)] pub context_strategy: ContextStrategy`
    - `#[serde(default = "AgentTemplate::default_compact_at_pct")] pub compact_at_pct: u8`, where the default is 75
  - `pub struct CompactionState { pub summary: String, pub covers: usize, pub tokens: u32, pub at: u64 }`, which derives Serialize, Deserialize, Clone, Debug and PartialEq.
  - `SessionRecord.compaction: Option<CompactionState>`, with `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  - `SessionStore::set_compaction(&self, id: &str, state: Option<CompactionState>) -> Result<(), SessionError>`. It does not bump `updated`, and it does not broadcast.

- [ ] **Step 1: Write the failing tests**
  - In template.rs:
    - `context_fields_default_when_absent`: TOML without the fields gives `Off` and `75`.
    - `context_fields_round_trip`: `context_strategy = "summarize"` and `compact_at_pct = 60` round-trip.
    - `unknown_context_strategy_is_rejected`: the validation or parse error text contains `unknown context strategy: shrink`.
    - `compact_at_pct_out_of_range_is_rejected`: 9 and 96 give `compact_at_pct must be between 10 and 95`, while 10 and 95 pass.
  - In session_store.rs:
    - `set_compaction_persists_and_reopens`
    - `set_compaction_keeps_updated`
    - `reset_clears_compaction`
    - `old_session_json_without_compaction_loads`
- [ ] **Step 2:** Run `scripts/hive-test.sh -- agents::`. Expected: FAIL.
- [ ] **Step 3: Implement.** The wire shape follows from serde. Use the template validator's existing error style for the two messages.
- [ ] **Step 4:** Run `scripts/hive-test.sh`, then `--restore`. Expected: PASS.
- [ ] **Step 5: Commit** with the message `feat(agents): context strategy template fields and saved summary state`.

