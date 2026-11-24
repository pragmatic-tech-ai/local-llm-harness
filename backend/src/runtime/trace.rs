//! Per-session append-only trace log (`{id}.trace.jsonl`): step and turn records plus reset
//! markers, one JSON object per line, and the store that hands out one log per session id.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// File-name suffix of a session's trace log, appended to the session id.
pub const TRACE_FILE_SUFFIX: &str = ".trace.jsonl";

/// Schema version written on every record.
const SCHEMA_VERSION: u32 = 1;
/// Error text for a session id that is not a single file-name component.
const INVALID_ID: &str = "invalid session id";
/// Page size when the caller gives none.
const DEFAULT_LIMIT: usize = 20;

/// How a loop step ended.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    ToolCalls,
    Answered,
    Budget,
    Rejected,
    Error,
    Aborted,
}

/// Timing of one tool call inside a step.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ToolTiming {
    pub name: String,
    pub id: String,
    pub ok: bool,
    pub ms: u64,
    pub bytes: u64,
}

/// One loop step, written when the step's tools (if any) have finished.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StepRecord {
    pub turn: u64,
    pub step: u32,
    pub at: u64,
    pub prompt_tokens: Option<u32>,
    pub n_ctx: Option<u32>,
    pub out_tokens: u32,
    pub think_tokens: u32,
    pub load_ms: Option<u64>,
    pub ttft_ms: Option<u64>,
    pub gen_ms: Option<u64>,
    pub end: StepOutcome,
    pub tools: Vec<ToolTiming>,
    pub error: Option<String>,
}

/// One per turn, written after the loop is done.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TurnRecord {
    pub turn: u64,
    pub at: u64,
    pub steps: u32,
    pub reason: String,
    pub total_ms: u64,
    pub out_tokens: u32,
    pub think_tokens: u32,
    pub peak_fill: f64,
    pub committed: bool,
    pub user: String,
}

/// Written when the session is reset; the log itself is kept.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ResetRecord {
    pub at: u64,
}

/// A log record. Serialized with `"kind"` flattened in (see [`TraceLine`] for `"v"`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TraceRecord {
    Step(StepRecord),
    Turn(TurnRecord),
    Reset(ResetRecord),
}

/// The on-disk shape of a line: the schema version next to the flattened record.
#[derive(Serialize)]
struct TraceLine<'a> {
    v: u32,
    #[serde(flatten)]
    record: &'a TraceRecord,
}

/// One turn on a page: its steps, its summary (absent while running or crash-cut) and
/// whether a reset sits between it and the previous turn.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TraceTurn {
    pub turn: u64,
    pub steps: Vec<StepRecord>,
    pub summary: Option<TurnRecord>,
    pub reset_before: bool,
}

/// A page of turns, newest first.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TracePage {
    pub turns: Vec<TraceTurn>,
    pub more: bool,
    pub reset_pending: bool,
}

/// What appends need to know, guarded by one lock.
struct WriteState {
    /// The file is known to end in a line feed (or to be absent/empty).
    tail_clean: bool,
    /// The log was deleted with its session; appends are dropped.
    deleted: bool,
}

/// The append-only trace log of one session. Opening is lazy: nothing is read until used.
pub struct TraceLog {
    path: PathBuf,
    /// Serializes appends so the tail check and the write are one step.
    write: Mutex<WriteState>,
    /// The next turn number, once scanned.
    next: Mutex<Option<u64>>,
}

impl TraceLog {
    /// Hard cap on a page's `limit`.
    pub const MAX_LIMIT: usize = 100;

    pub fn open(path: PathBuf) -> TraceLog {
        TraceLog {
            path,
            write: Mutex::new(WriteState {
                tail_clean: false,
                deleted: false,
            }),
            next: Mutex::new(None),
        }
    }

    /// Appends one record as a single line, then flushes. A crash-cut tail without a line
    /// feed is terminated first so the new record never fuses with it.
    pub fn append(&self, record: &TraceRecord) -> io::Result<()> {
        let line = serde_json::to_string(&TraceLine {
            v: SCHEMA_VERSION,
            record,
        })
        .map_err(io::Error::other)?;
        {
            let mut state = Self::guard(&self.write);
            if state.deleted {
                return Ok(());
            }
            let written = self.write_line(&mut state, &line);
            state.tail_clean = written.is_ok();
            written?;
        }
        if let Some(turn) = Self::turn_of(record) {
            let mut next = Self::guard(&self.next);
            if let Some(cached) = *next {
                if turn >= cached {
                    *next = Some(turn + 1);
                }
            }
        }
        Ok(())
    }

    /// Writes one line (checking a possibly cut tail first), under the write lock.
    fn write_line(&self, state: &mut WriteState, line: &str) -> io::Result<()> {
        let mut buf = Vec::with_capacity(line.len() + 2);
        if !state.tail_clean && Self::tail_is_cut(&self.path)? {
            buf.push(b'\n');
        }
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(&buf)?;
        file.flush()
    }

    /// The highest `turn` in any step or turn record, plus 1; scanned once, then cached.
    /// A missing file gives 1. A read error is returned and nothing is cached.
    pub fn next_turn(&self) -> io::Result<u64> {
        let mut next = Self::guard(&self.next);
        if let Some(n) = *next {
            return Ok(n);
        }
        let highest = Self::read_records(&self.path)?
            .iter()
            .filter_map(Self::turn_of)
            .max()
            .unwrap_or(0);
        *next = Some(highest + 1);
        Ok(highest + 1)
    }

    /// Turns newest first, those below `before` (exclusive), at most `limit` of them
    /// (default 20, capped at [`MAX_LIMIT`](Self::MAX_LIMIT)).
    pub fn page(&self, before: Option<u64>, limit: Option<usize>) -> io::Result<TracePage> {
        let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, Self::MAX_LIMIT);
        let mut turns: BTreeMap<u64, TraceTurn> = BTreeMap::new();
        let mut reset_open = false;
        // A reset before the first turn still sets `reset_before` on that turn: the UI draws
        // the divider under the oldest turn, which records the history faithfully.
        for record in Self::read_records(&self.path)? {
            match record {
                TraceRecord::Reset(_) => reset_open = true,
                TraceRecord::Step(step) => {
                    Self::entry(&mut turns, step.turn, &mut reset_open)
                        .steps
                        .push(step);
                }
                TraceRecord::Turn(summary) => {
                    let turn = summary.turn;
                    Self::entry(&mut turns, turn, &mut reset_open).summary = Some(summary);
                }
            }
        }
        let mut eligible: Vec<TraceTurn> = turns
            .into_values()
            .rev()
            .filter(|t| before.is_none_or(|b| t.turn < b))
            .collect();
        let more = eligible.len() > limit;
        eligible.truncate(limit);
        Ok(TracePage {
            turns: eligible,
            more,
            reset_pending: before.is_none() && reset_open,
        })
    }

    /// Removes the log file; a missing file is fine.
    pub fn delete(&self) -> io::Result<()> {
        let mut state = Self::guard(&self.write);
        self.remove_file(&mut state)
    }

    /// Deletes the file and drops every later append: the session is gone.
    fn retire(&self) -> io::Result<()> {
        let mut state = Self::guard(&self.write);
        state.deleted = true;
        self.remove_file(&mut state)
    }

    fn remove_file(&self, state: &mut WriteState) -> io::Result<()> {
        *Self::guard(&self.next) = None;
        state.tail_clean = false;
        match fs::remove_file(&self.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// The turn's entry, created on first sight and taking over a pending reset marker.
    fn entry<'a>(
        turns: &'a mut BTreeMap<u64, TraceTurn>,
        turn: u64,
        reset_open: &mut bool,
    ) -> &'a mut TraceTurn {
        turns.entry(turn).or_insert_with(|| TraceTurn {
            turn,
            steps: Vec::new(),
            summary: None,
            reset_before: std::mem::take(reset_open),
        })
    }

    fn turn_of(record: &TraceRecord) -> Option<u64> {
        match record {
            TraceRecord::Step(s) => Some(s.turn),
            TraceRecord::Turn(t) => Some(t.turn),
            TraceRecord::Reset(_) => None,
        }
    }

    /// True when the file exists, is non-empty and does not end in a line feed.
    fn tail_is_cut(path: &Path) -> io::Result<bool> {
        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        if file.metadata()?.len() == 0 {
            return Ok(false);
        }
        file.seek(SeekFrom::End(-1))?;
        let mut last = [0u8; 1];
        file.read_exact(&mut last)?;
        Ok(last[0] != b'\n')
    }

    /// Every readable record in log order. A missing file is empty; partial, unparseable
    /// and unknown-kind lines are skipped; any other read error is returned.
    fn read_records(path: &Path) -> io::Result<Vec<TraceRecord>> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(|l| serde_json::from_str::<TraceRecord>(l).ok())
            .collect())
    }

    fn guard<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// One [`TraceLog`] per session id, cached, under the sessions directory.
pub struct TraceStore {
    dir: PathBuf,
    logs: Mutex<HashMap<String, Arc<TraceLog>>>,
}

impl TraceStore {
    pub fn new(sessions_dir: PathBuf) -> TraceStore {
        TraceStore {
            dir: sessions_dir,
            logs: Mutex::new(HashMap::new()),
        }
    }

    /// The session's log, created lazily and cached. The id must be a single plain
    /// file-name component (no separators, `..` or empty).
    pub fn log(&self, id: &str) -> io::Result<Arc<TraceLog>> {
        Self::check_id(id)?;
        let mut logs = TraceLog::guard(&self.logs);
        Ok(Arc::clone(logs.entry(id.to_string()).or_insert_with(
            || Arc::new(TraceLog::open(self.path_for(id))),
        )))
    }

    /// Deletes the session's log file and evicts the cache entry. A handle still held
    /// elsewhere is retired: its later appends are dropped instead of recreating the file.
    pub fn delete(&self, id: &str) -> io::Result<()> {
        Self::check_id(id)?;
        let mut logs = TraceLog::guard(&self.logs);
        let log = logs
            .remove(id)
            .unwrap_or_else(|| Arc::new(TraceLog::open(self.path_for(id))));
        log.retire()
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}{TRACE_FILE_SUFFIX}"))
    }

    fn check_id(id: &str) -> io::Result<()> {
        let mut parts = Path::new(id).components();
        let plain = matches!(
            (parts.next(), parts.next()),
            (Some(std::path::Component::Normal(_)), None)
        ) && !id.contains(['/', '\\']);
        if plain {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{INVALID_ID}: {id:?}"),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    const FILE: &str = "s-1-0.trace.jsonl";
    /// A crash-cut record: cut mid-kind, no line feed.
    const CUT_LINE: &str = "{\"v\":1,\"kind\":\"st";

    /// A unique temp directory removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir =
                std::env::temp_dir().join(format!("trace-{tag}-{}-{nanos}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn log(&self) -> TraceLog {
            TraceLog::open(self.0.join(FILE))
        }

        fn raw(&self, text: &str) {
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.0.join(FILE))
                .unwrap();
            f.write_all(text.as_bytes()).unwrap();
        }

        fn step_record(turn: u64, step: u32) -> StepRecord {
            StepRecord {
                turn,
                step,
                at: 1000 + turn,
                prompt_tokens: Some(100),
                n_ctx: Some(1000),
                out_tokens: 5,
                think_tokens: 0,
                load_ms: None,
                ttft_ms: Some(10),
                gen_ms: Some(20),
                end: StepOutcome::Answered,
                tools: vec![ToolTiming {
                    name: "read_file".into(),
                    id: "c1".into(),
                    ok: true,
                    ms: 4,
                    bytes: 26,
                }],
                error: None,
            }
        }

        fn step(turn: u64, step: u32) -> TraceRecord {
            TraceRecord::Step(Self::step_record(turn, step))
        }

        fn summary(turn: u64) -> TraceRecord {
            TraceRecord::Turn(Self::turn_record(turn))
        }

        fn turn_record(turn: u64) -> TurnRecord {
            TurnRecord {
                turn,
                at: 2000 + turn,
                steps: 1,
                reason: "answered".into(),
                total_ms: 30,
                out_tokens: 5,
                think_tokens: 0,
                peak_fill: 0.1,
                committed: true,
                user: "hi".into(),
            }
        }

        fn reset() -> TraceRecord {
            TraceRecord::Reset(ResetRecord { at: 3000 })
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn records_round_trip_with_version_and_kind() {
        let d = TempDir::new("rt");
        let log = d.log();
        let records = vec![TempDir::step(1, 1), TempDir::summary(1), TempDir::reset()];
        for r in &records {
            log.append(r).unwrap();
        }
        assert_eq!(TraceLog::read_records(&d.0.join(FILE)).unwrap(), records);
        let text = fs::read_to_string(d.0.join(FILE)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        for (line, kind) in lines.iter().zip(["step", "turn", "reset"]) {
            assert!(line.contains("\"v\":1"), "{line}");
            assert!(line.contains(&format!("\"kind\":\"{kind}\"")), "{line}");
        }
        assert!(lines[0].contains("\"end\":\"answered\""));
        let page = log.page(None, None).unwrap();
        assert_eq!(page.turns.len(), 1);
        assert_eq!(page.turns[0].steps, vec![TempDir::step_record(1, 1)]);
        assert_eq!(page.turns[0].summary, Some(TempDir::turn_record(1)));
        assert!(page.reset_pending);
    }

    #[test]
    fn partial_last_line_is_ignored() {
        let d = TempDir::new("partial");
        let log = d.log();
        log.append(&TempDir::step(1, 1)).unwrap();
        log.append(&TempDir::step(1, 2)).unwrap();
        d.raw(CUT_LINE);
        let page = log.page(None, None).unwrap();
        assert_eq!(page.turns.len(), 1);
        assert_eq!(page.turns[0].steps.len(), 2);
        assert_eq!(log.next_turn().unwrap(), 2);
    }

    #[test]
    fn append_after_cut_tail_does_not_fuse_lines() {
        let d = TempDir::new("fuse");
        let log = d.log();
        log.append(&TempDir::step(1, 1)).unwrap();
        d.raw(CUT_LINE);
        let reopened = d.log();
        reopened.append(&TempDir::step(1, 2)).unwrap();
        assert_eq!(reopened.page(None, None).unwrap().turns[0].steps.len(), 2);
    }

    #[test]
    fn unknown_kind_and_fields_are_skipped() {
        let d = TempDir::new("unknown");
        let log = d.log();
        d.raw("{\"v\":1,\"kind\":\"future\",\"x\":1}\n");
        let line = serde_json::to_string(&TraceLine {
            v: 1,
            record: &TempDir::step(1, 1),
        })
        .unwrap();
        d.raw(&format!("{},\"new\":5}}\n", &line[..line.len() - 1]));
        let page = log.page(None, None).unwrap();
        assert_eq!(page.turns.len(), 1);
        assert_eq!(page.turns[0].steps.len(), 1);
    }

    #[test]
    fn page_is_newest_first_with_limit_and_cap() {
        let d = TempDir::new("paging");
        let log = d.log();
        for t in 1..=25 {
            log.append(&TempDir::step(t, 1)).unwrap();
            log.append(&TempDir::summary(t)).unwrap();
        }
        let p = log.page(None, None).unwrap();
        let ids: Vec<u64> = p.turns.iter().map(|t| t.turn).collect();
        assert_eq!(ids, (6..=25).rev().collect::<Vec<_>>());
        assert!(p.more);
        let p = log.page(Some(6), None).unwrap();
        let ids: Vec<u64> = p.turns.iter().map(|t| t.turn).collect();
        assert_eq!(ids, vec![5, 4, 3, 2, 1]);
        assert!(!p.more);
        assert_eq!(log.page(None, Some(500)).unwrap().turns.len(), 25);
    }

    #[test]
    fn paging_past_the_ends() {
        let d = TempDir::new("ends");
        let log = d.log();
        assert_eq!(
            log.page(None, None).unwrap(),
            TracePage {
                turns: vec![],
                more: false,
                reset_pending: false
            }
        );
        for t in 1..=3 {
            log.append(&TempDir::step(t, 1)).unwrap();
        }
        let p = log.page(Some(1), None).unwrap();
        assert!(p.turns.is_empty() && !p.more);
        let p = log.page(Some(99), None).unwrap();
        assert_eq!(p.turns.len(), 3);
        assert_eq!(p.turns[0].turn, 3);
    }

    #[test]
    fn reset_flags() {
        let d = TempDir::new("reset");
        let log = d.log();
        log.append(&TempDir::step(1, 1)).unwrap();
        log.append(&TempDir::reset()).unwrap();
        log.append(&TempDir::step(2, 1)).unwrap();
        log.append(&TempDir::reset()).unwrap();
        let p = log.page(None, None).unwrap();
        assert!(p.reset_pending);
        assert!(p.turns[0].reset_before);
        assert!(!p.turns[1].reset_before);
        let p = log.page(Some(2), None).unwrap();
        assert!(!p.reset_pending);
        assert!(!p.turns[0].reset_before);
        let p = log.page(None, Some(1)).unwrap();
        assert!(p.more);
        assert_eq!(p.turns.len(), 1);
        assert!(p.turns[0].reset_before);
    }

    #[test]
    fn unreadable_log_is_an_error_and_not_cached() {
        let d = TempDir::new("unreadable");
        fs::create_dir(d.0.join(FILE)).unwrap();
        let log = d.log();
        assert!(log.page(None, None).is_err());
        assert!(log.next_turn().is_err());
        assert!(log.next_turn().is_err());
    }

    #[test]
    fn store_rejects_ids_that_are_not_plain_file_names() {
        let d = TempDir::new("ids");
        let store = TraceStore::new(d.0.clone());
        for bad in ["", ".", "..", "a/b", r"a\b", "../x", "/abs"] {
            assert!(store.log(bad).is_err(), "{bad:?}");
            assert!(store.delete(bad).is_err(), "{bad:?}");
        }
        assert!(store.log("s-1-0").is_ok());
    }

    #[test]
    fn append_on_a_retired_handle_is_dropped() {
        let d = TempDir::new("retired");
        let store = TraceStore::new(d.0.clone());
        let held = store.log("s-1-0").unwrap();
        held.append(&TempDir::step(1, 1)).unwrap();
        store.delete("s-1-0").unwrap();
        held.append(&TempDir::step(2, 1)).unwrap();
        assert!(!d.0.join(FILE).exists());
    }

    #[test]
    fn next_turn_counts_crash_cut_turns() {
        let d = TempDir::new("next");
        let log = d.log();
        assert_eq!(log.next_turn().unwrap(), 1);
        log.append(&TempDir::step(1, 1)).unwrap();
        log.append(&TempDir::summary(1)).unwrap();
        log.append(&TempDir::step(2, 1)).unwrap();
        assert_eq!(log.next_turn().unwrap(), 3);
        log.append(&TempDir::step(3, 1)).unwrap();
        assert_eq!(log.next_turn().unwrap(), 4);
        let fresh = d.log();
        assert_eq!(fresh.next_turn().unwrap(), 4);
        let p = fresh.page(None, None).unwrap();
        let two = p.turns.iter().find(|t| t.turn == 2).unwrap();
        assert!(two.summary.is_none());
    }

    #[test]
    fn store_delete_removes_file_and_cache() {
        let d = TempDir::new("store");
        let store = TraceStore::new(d.0.clone());
        let a = store.log("s-1-0").unwrap();
        assert!(Arc::ptr_eq(&a, &store.log("s-1-0").unwrap()));
        a.append(&TempDir::step(1, 1)).unwrap();
        let file = d.0.join(FILE);
        assert!(file.exists());
        store.delete("s-1-0").unwrap();
        assert!(!file.exists());
        store.delete("s-1-0").unwrap();
        let b = store.log("s-1-0").unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(b.next_turn().unwrap(), 1);
    }
}
