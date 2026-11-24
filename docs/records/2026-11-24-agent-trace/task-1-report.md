# Task 1 report: trace records, log and store

Built `backend/src/runtime/trace.rs` (+ `pub mod trace;` in runtime/mod.rs) and test `session_store_ignores_trace_files` in agents/session_store.rs. The session loader needed no change: it filters on extension "json", and "x.trace.jsonl" has extension "jsonl".

TDD: no separate RED run (module and tests were written together); first hive run failed to compile (moved value, fixed), then `scripts/hive-test.sh -- trace` 10 passed; full lib 366 passed; `--restore` done.

## Public signatures (crate::runtime::trace)
- `pub const TRACE_FILE_SUFFIX: &str = ".trace.jsonl";`
- `pub enum StepOutcome { ToolCalls, Answered, Budget, Rejected, Error, Aborted }` (Copy, snake_case)
- `pub struct ToolTiming { name: String, id: String, ok: bool, ms: u64, bytes: u64 }`
- `pub struct StepRecord { turn: u64, step: u32, at: u64, prompt_tokens: Option<u32>, n_ctx: Option<u32>, out_tokens: u32, think_tokens: u32, load_ms: Option<u64>, ttft_ms: Option<u64>, gen_ms: Option<u64>, end: StepOutcome, tools: Vec<ToolTiming>, error: Option<String> }`
- `pub struct TurnRecord { turn: u64, at: u64, steps: u32, reason: String, total_ms: u64, out_tokens: u32, think_tokens: u32, peak_fill: f64, committed: bool, user: String }`
- `pub struct ResetRecord { at: u64 }`
- `pub enum TraceRecord { Step(StepRecord), Turn(TurnRecord), Reset(ResetRecord) }` (serde tag "kind"; the "v":1 is added by a private TraceLine wrapper on append)
- `pub struct TraceTurn { turn: u64, steps: Vec<StepRecord>, summary: Option<TurnRecord>, reset_before: bool }`
- `pub struct TracePage { turns: Vec<TraceTurn>, more: bool, reset_pending: bool }`
- All of the above derive Serialize, Deserialize, Clone, Debug, PartialEq.
- `TraceLog::MAX_LIMIT: usize = 100`
- `TraceLog::open(path: PathBuf) -> TraceLog`
- `fn append(&self, record: &TraceRecord) -> io::Result<()>`
- `fn next_turn(&self) -> u64`
- `fn page(&self, before: Option<u64>, limit: Option<usize>) -> io::Result<TracePage>`
- `fn delete(&self) -> io::Result<()>`
- `TraceStore::new(sessions_dir: PathBuf) -> TraceStore`
- `fn log(&self, id: &str) -> Arc<TraceLog>`
- `fn delete(&self, id: &str) -> io::Result<()>`

## Notes
- append terminates a crash-cut tail (no LF) with a newline first, so a new record never fuses with it (extra test `append_after_cut_tail_does_not_fuse_lines`).
- limit is clamped to 1..=100 (default 20). next_turn cache is bumped by append only once scanned.
- TraceLog and TraceStore are Send + Sync (Mutex fields); no Cargo changes.

## Fix round 1
Fixed Important 1 and Minor 1-6. Full lib on hive: 369 passed; `--restore` done. `is_none_or` compiled on the hive toolchain.
- Important 1: `read_records` returns `io::Result` (NotFound = empty); `page` propagates; `next_turn` now returns `io::Result<u64>` and caches nothing on error. Test `unreadable_log_is_an_error_and_not_cached` (path is a directory).
- Minor 1: `WriteState { tail_clean, deleted }` under one lock; tail is checked only on the first append after open, or after a failed write/delete. Consequence: a cut tail is repaired by the first append of a freshly opened log (the real crash case); test reopens the log.
- Minor 2: `TraceStore::log(id) -> io::Result<Arc<TraceLog>>` rejects ids that are not a single plain file-name component (InvalidInput); `TraceStore::delete` also validates, removes the cache entry and retires the log under the map lock (later appends on a held Arc are no-ops), then removes the file. Tests: `store_rejects_ids_that_are_not_plain_file_names`, `append_on_a_retired_handle_is_dropped`.
- Minor 3: `is_none_or`. Minor 4: round trip compares full records via `read_records` and the turn summary; `reset_flags` asserts oldest-turn-of-page `reset_before`; `CUT_LINE` const. Minor 5: `SessionStore` exposes no load-error list, so the test stays as is. Minor 6: ruling documented in a comment in `page`.
- Changed signatures: `TraceLog::next_turn(&self) -> io::Result<u64>`; `TraceStore::log(&self, id: &str) -> io::Result<Arc<TraceLog>>`. `TraceLog::delete` is unchanged (plain remove, does not retire).
