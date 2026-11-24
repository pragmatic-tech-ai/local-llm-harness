### Task 1: Trace records, log and store

**Files:**
- Create: `backend/src/runtime/trace.rs` (records, `TraceLog`, `TraceStore`, `TracePage`, `TraceTurn`)
- Modify: `backend/src/runtime/mod.rs` (`pub mod trace;`)

**Interfaces:**
- Produces:
  - Records:
    - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)] pub struct StepRecord { pub turn: u64, pub step: u32, pub at: u64, pub prompt_tokens: Option<u32>, pub n_ctx: Option<u32>, pub out_tokens: u32, pub think_tokens: u32, pub load_ms: Option<u64>, pub ttft_ms: Option<u64>, pub gen_ms: Option<u64>, pub end: StepOutcome, pub tools: Vec<ToolTiming>, pub error: Option<String> }`
    - `pub enum StepOutcome { ToolCalls, Answered, Budget, Rejected, Error, Aborted }` (not `StepEnd`, which is already a private enum in `tool_loop.rs`), serialized snake_case.
    - `pub struct ToolTiming { pub name: String, pub id: String, pub ok: bool, pub ms: u64, pub bytes: u64 }`
    - `pub struct TurnRecord { pub turn: u64, pub at: u64, pub steps: u32, pub reason: String, pub total_ms: u64, pub out_tokens: u32, pub think_tokens: u32, pub peak_fill: f64, pub committed: bool, pub user: String }`
    - `pub struct ResetRecord { pub at: u64 }`
    - `pub enum TraceRecord { Step(StepRecord), Turn(TurnRecord), Reset(ResetRecord) }`. It serializes with `"v":1` and `"kind":"step"|"turn"|"reset"` flattened in; use a small wrapper struct rather than a hand-written `Serialize`, if serde's internally-tagged enum plus a `v` field allows it.
  - Pages:
    - `pub struct TraceTurn { pub turn: u64, pub steps: Vec<StepRecord>, pub summary: Option<TurnRecord>, pub reset_before: bool }`
    - `pub struct TracePage { pub turns: Vec<TraceTurn>, pub more: bool, pub reset_pending: bool }`
  - `TraceLog`:
    - `TraceLog::open(path: PathBuf) -> TraceLog`. This is lazy: nothing is read until it is used.
    - `append(&self, record: &TraceRecord) -> io::Result<()>`
    - `next_turn(&self) -> u64`: the highest `turn` in any step or turn record, plus 1, scanned once and then cached. A missing file gives 1.
    - `page(&self, before: Option<u64>, limit: Option<usize>) -> io::Result<TracePage>`. `limit` defaults to 20, and its cap is `TraceLog::MAX_LIMIT = 100`.
    - `delete(&self) -> io::Result<()>`. A missing file is fine.
  - `TraceStore`:
    - `TraceStore::new(sessions_dir: PathBuf) -> TraceStore`
    - `log(&self, id: &str) -> Arc<TraceLog>`, cached per id.
    - `delete(&self, id: &str) -> io::Result<()>`, which also evicts the cache entry.
  - `TRACE_FILE_SUFFIX = ".trace.jsonl"`. `SessionStore` only loads `*.json`; check that `.trace.jsonl` files are not picked up by its loader, and add the test below.

- [ ] **Step 1: Write the failing tests** in `trace.rs`'s `mod tests` (using a temp-dir guard struct):
  - `records_round_trip_with_version_and_kind`: one record of each kind is appended. The raw lines contain `"v":1` and the right `"kind"`, and `page` returns them.
  - `partial_last_line_is_ignored`: two steps of turn 1 are appended, then `{"v":1,"kind":"st` is written raw with no LF. `page(None, None)` has one turn with 2 steps, and `next_turn()` is 2.
  - `unknown_kind_and_fields_are_skipped`: a raw line `{"v":1,"kind":"future","x":1}`, then a step with an extra field `"new":5`. The step is read, and nothing fails.
  - `page_is_newest_first_with_limit_and_cap`: 25 turns, each with one step and a summary. `page(None, None)` returns turns 25..=6 and `more: true`. `page(Some(6), None)` returns 5..=1 and `more: false`. `page(None, Some(500))` returns 25 turns.
  - `paging_past_the_ends`: `page(Some(1), None)` gives no turns and `more: false`. `page(Some(99), None)` on a log with 3 turns gives the 3 newest. A missing file gives an empty page.
  - `reset_flags`: the log holds turn 1, a reset, turn 2, then a reset. Turn 2 has `reset_before: true` and turn 1 has `false`, and the first page has `reset_pending: true`. `page(Some(2), None)` has `reset_pending: false`, and its turn 1 has `reset_before: false`.
  - `next_turn_counts_crash_cut_turns`: turn 1 has a summary, and turn 2 has a step but no summary. `next_turn()` is 3, and turn 2's `summary` is `None`.
  - `store_delete_removes_file_and_cache`
  - `session_store_ignores_trace_files` (in `agents/session_store.rs` tests): a planted `s-1-0.trace.jsonl` in the sessions dir is not loaded as a session, and it does not log a broken-session error.
- [ ] **Step 2:** Run `scripts/hive-test.sh -- trace`. Expected: FAIL to compile, because there is no module.
- [ ] **Step 3: Implement** in `trace.rs`.
  - Append is `OpenOptions::new().create(true).append(true)`, writing `serde_json::to_string(record) + "\n"`, then `flush`.
  - `page` reads the whole file, parses each line, groups the steps and summaries by turn, and marks resets in log order.
  - The cache is a `Mutex<Option<u64>>` for the next turn number. It is bumped by `append` of a step or turn record whose turn is ≥ the cached value.
- [ ] **Step 4:** Run `scripts/hive-test.sh`. Expected: the whole lib passes. Then run `scripts/hive-test.sh --restore`.
- [ ] **Step 5: Commit** `feat(runtime): trace records, per-session trace log and store`.

