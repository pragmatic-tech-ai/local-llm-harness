### Spec Compliance
- ✅ `context_strategy` default Off, wire values off/trim/summarize; unknown gives `unknown context strategy: {v}` (template.rs, custom Deserialize).
- ✅ `compact_at_pct` default 75, 10..=95 inclusive, exact message (template.rs validate).
- ✅ `SessionRecord.compaction` optional, schema stays 1; `set_compaction` uses Bump::Kept and does not broadcast; `reset` clears it.
- ✅ R2 tree path: template_store.rs:211 `toml::from_str(...).map_err(|e| e.to_string())`; toml's Display includes the custom message. Wire path: rpc.rs:128 `RpcError::new(INVALID_PARAMS, e)` carries the serde_json text (contains the message plus " at line/column" suffix). Both are by-inspection only.
- ⚠️ Not verified: cargo not run; the 426-pass claim is unverified.

### Strengths
Custom Deserialize gives one error source for TOML and JSON. Tests cover defaults, round trip, both bounds, old JSON, reset, and updated kept.

### Issues
#### Critical
None.
#### Important
- session_store.rs (tests, `fn state()`): a free module-level function in new test code; violates the no-free-functions rule. Move it into `impl TempDir` (like `sample`) or an associated fn.
- template_store.rs test fixture, `context_strategy: Default::default(),` line is mis-indented (rustfmt would change it). Fix indentation / run rustfmt.
- template.rs `ContextStrategy::parse`: "off"/"trim"/"summarize" are inline literals duplicating the serde `rename_all` names, so they can drift. Hoist to consts (or derive the parse from one table).
#### Minor
- No RED run (process): accepted as a non-blocking process note. The tests are not vacuous (new fields and fns would not compile or would fail without the implementation), so GREEN is meaningful evidence; no rework needed.
- No `save_template` RPC test for R2: valid concern, coverage only. The behaviour is correct by inspection and the unit test already asserts the serde_json text. Add a test next to server.rs:1918 asserting `err.message.contains("unknown context strategy: shrink")` (raw JSON `"context_strategy":"shrink"`), plus one tree() test asserting the node `error` contains it.
- `compact_at_pct: u8`: a TOML/JSON value such as 300 or -1 fails with a serde "invalid value ... expected u8" error, not the range message. Spec says outside 10-95 gives the range message; consider parsing as i64/u32 and validating.
- `compact_at_pct: 75` literal in the seed (template_store.rs) and test fixtures duplicates the private default const; use `AgentTemplate::default_compact_at_pct()` / make the const reachable, and `ContextStrategy::default()` instead of `Default::default()` in production seed.

### Assessment
**Task quality:** Needs fixes
**Reasoning:** Behaviour matches the spec and R2, but new code has three style-rule violations (free test fn, rustfmt indentation, inline literals) that must be fixed before approval.
