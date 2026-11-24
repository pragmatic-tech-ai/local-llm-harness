### Spec Compliance
- ✅ Records, `"v":1` via private `TraceLine` wrapper with flattened `kind` (trace.rs TraceLine/TraceRecord); snake_case outcomes; `StepOutcome` name.
- ✅ Append is create+append, single `write_all`, flush, no fsync; path `{dir}/{id}.trace.jsonl` (TraceStore::log); LF-terminated.
- ✅ Unknown kinds fail the tagged-enum parse and are skipped; unknown fields ignored by serde default; partial last line unparseable and skipped.
- ✅ No store/hub lock touched; no new crates; no free functions (tests included, helpers on `TempDir`/`TraceLog`); reused strings are consts.
- ✅ next_turn = highest turn in step/turn records + 1, cached, missing file gives 1; bump only when scanned (correct, no lost update: scan holds `next`, append bumps after write).
- ✅ page: newest first, exclusive `before`, default 20, cap 100, `more`, `reset_before` exact (whole file read), `reset_pending` only when `before` is None.
- ✅ Store: cached per id, `delete` evicts and tolerates missing file.
- ⚠️ Cannot verify the hive run (366 passed) here; the code reads as compiling.
- ✅ session_store loader filters on extension `json`; `.trace.jsonl` is not loaded. Test added.

### Strengths
Small, cohesive module; the lock order (write_lock then next, never nested the other way) is deadlock-free; poisoned mutexes are tolerated; tests assert real content (ordering, flags, caps, cache eviction via `Arc::ptr_eq`).

### Extra behaviour: newline before append on a cut tail
Judged correct and worth keeping. Without it, the first append after a crash fuses a good record onto the partial line and that record is lost, which defeats "ignore a partial last line" once the session continues. It is tested (`append_after_cut_tail_does_not_fuse_lines`), runs under `write_lock`, and is written in the same `write_all`. Cost: one extra open+seek+read per append (see Minor 1).

### Missing RED run: judged a process deviation only. The module did not exist, so a compile failure is the only possible RED, and the report says a first (compile-error) hive run happened. The tests assert real behaviour, so I see no sign of vacuous tests. Not blocking.

### Issues
#### Critical
None.

#### Important
1. trace.rs `read_records` (and `page`/`next_turn`): any read error other than "missing" (permissions, sharing violation on Windows, I/O) is swallowed into an empty record list. `page` therefore never returns `Err` despite its `io::Result`, and `next_turn` would cache 1 on a transient read failure, so a later turn reuses an existing turn number and collides with real history. Fix: have `read_records` return `io::Result<Vec<_>>` (NotFound = empty), propagate from `page`, and have `next_turn` not cache on error (return a `Result`, or fall back without caching).

#### Minor
1. trace.rs `append`/`tail_is_cut`: an extra `File::open` + seek + read on every append, plus a TOCTOU between that open and the append open (safe in-process only). Could keep a "tail known clean" flag after the first successful append to skip the check.
2. trace.rs `TraceStore::delete`: `log()` then separate `remove` leaves a window where another caller gets a fresh `Arc` and appends before `log.delete()` removes the file; a holder of the old `Arc` can also recreate the file after delete. Later tasks must not append after session delete. `log(id)` joins an unvalidated id into a path, so callers (get_trace) must check the session exists first.
3. trace.rs `page`: `before.map_or(true, ..)` is flagged by newer clippy (`is_none_or`).
4. Test gaps: `reset_flags` never asserts `reset_before: true` on the oldest turn of a later page; the round-trip test compares only the step and the summary's `turn`, not the turn record or reset record; the cut-line literal `{"v":1,"kind":"st` is duplicated in two tests (hoist to a const).
5. session_store.rs test `session_store_ignores_trace_files`: it does not check that no broken-session error is logged (the brief asks for it); it only asserts list/get results. It passes with no production change, so it is a characterization test only.
6. A reset before the first turn sets `reset_before: true` on turn 1 although there is no previous turn; spec wording "between this turn and the previous one" is ambiguous. Confirm intent for the UI.

### Assessment
**Task quality:** Needs fixes
**Reasoning:** The design matches the spec and the extra newline guard is sound, but swallowing non-NotFound read errors can silently return an empty history and make `next_turn` reuse turn numbers, so Important 1 should be fixed before later tasks build on it.
