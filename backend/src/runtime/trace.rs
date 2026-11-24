//! Per-session append-only trace log (`{id}.trace.jsonl`): step and turn records plus reset
//! markers, one JSON object per line, and the store that hands out one log per session id.
//! [`TurnTrace`] is where the tool loop reports each step; [`SessionTrace`] is the one that
//! appends to a session's log and sends the records live.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::tool_loop::{LoopSink, SinkClosed};

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
    /// The call failed exactly as the one before it did, so its result carries the retry
    /// hint.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub repeat_failure: bool,
}

impl ToolTiming {
    /// The byte length of a tool result's serialized JSON: the timing's `bytes`, and the
    /// `{bytes}` of a trim stub. The one home of that measure.
    pub fn result_bytes(result: &Value) -> usize {
        serde_json::to_string(result).map_or(0, |s| s.len())
    }
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
    /// Present only on steps where compaction ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionRecord>,
    /// The provider-reported cost of the step; absent for providers that report none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// The prompt size is an estimate, not a measurement.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
}

/// What compaction did to one step's prompt. Token fields are measured prompt sizes; the
/// other `u32` fields are counts. All saturate at `u32::MAX`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CompactionRecord {
    pub strategy: String,
    pub before_tokens: u32,
    pub after_tokens: u32,
    pub elided_results: u32,
    pub dropped_turns: u32,
    pub summarized_messages: u32,
    pub summary_ms: Option<u64>,
    pub reused_summary: bool,
    pub fallback: bool,
    pub summary_prompt_tokens: Option<u32>,
    pub summary_out_tokens: Option<u32>,
    pub summary_error: Option<String>,
    /// The provider-reported cost of the summary calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_cost: Option<f64>,
}

impl CompactionRecord {
    /// A record for `strategy` that measured `before` tokens and sent `after`, with every
    /// count zero and every summary field empty.
    pub fn new(strategy: &str, before: usize, after: usize) -> Self {
        Self {
            strategy: strategy.to_string(),
            before_tokens: Self::count(before),
            after_tokens: Self::count(after),
            elided_results: 0,
            dropped_turns: 0,
            summarized_messages: 0,
            summary_ms: None,
            reused_summary: false,
            fallback: false,
            summary_prompt_tokens: None,
            summary_out_tokens: None,
            summary_error: None,
            summary_cost: None,
        }
    }

    /// `n` as a record count, saturating at `u32::MAX`.
    pub fn count(n: usize) -> u32 {
        u32::try_from(n).unwrap_or(u32::MAX)
    }
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

impl TraceRecord {
    /// The record as its log line's JSON object (`"v"` and `"kind"` included).
    pub fn line_value(&self) -> Value {
        json!(TraceLine {
            v: SCHEMA_VERSION,
            record: self,
        })
    }
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

impl TracePage {
    /// The page as `get_trace` sends it: the [`TracePage`] shape, with every step and summary
    /// in its log-line shape (`"v"` and `"kind"` included).
    pub fn wire(self) -> Value {
        let turns = self
            .turns
            .into_iter()
            .map(|t| WireTurn {
                turn: t.turn,
                steps: t
                    .steps
                    .into_iter()
                    .map(|s| TraceRecord::Step(s).line_value())
                    .collect(),
                summary: t.summary.map(|s| TraceRecord::Turn(s).line_value()),
                reset_before: t.reset_before,
            })
            .collect();
        json!(WirePage {
            turns,
            more: self.more,
            reset_pending: self.reset_pending,
        })
    }
}

/// [`TraceTurn`] on the wire: its records as log lines.
#[derive(Serialize)]
struct WireTurn {
    turn: u64,
    steps: Vec<Value>,
    summary: Option<Value>,
    reset_before: bool,
}

/// [`TracePage`] on the wire.
#[derive(Serialize)]
struct WirePage {
    turns: Vec<WireTurn>,
    more: bool,
    reset_pending: bool,
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

    /// Hands out the next turn number and reserves it at once, so two turns never share a
    /// number even when their records never reach the file. The first call scans the log for
    /// the highest `turn` in any step or turn record (a missing file counts as none); later
    /// calls only advance the counter. A read error is returned and nothing is cached.
    pub fn next_turn(&self) -> io::Result<u64> {
        let mut next = Self::guard(&self.next);
        let turn = match *next {
            Some(n) => n,
            None => {
                Self::read_records(&self.path)?
                    .iter()
                    .filter_map(Self::turn_of)
                    .max()
                    .unwrap_or(0)
                    + 1
            }
        };
        *next = Some(turn + 1);
        Ok(turn)
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

    /// Deletes the file (a missing file is fine) and drops every later append: the session
    /// is gone.
    fn retire(&self) -> io::Result<()> {
        let mut state = Self::guard(&self.write);
        state.deleted = true;
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

/// Where the tool loop reports each finished step of a turn.
#[async_trait]
pub trait TurnTrace: Send {
    /// One step, as the loop measured it; `turn` and `at` are 0 and the trace sets them.
    async fn step(&mut self, record: StepRecord, sink: &mut dyn LoopSink)
        -> Result<(), SinkClosed>;
}

/// A trace that keeps nothing.
pub struct NoTrace;

#[async_trait]
impl TurnTrace for NoTrace {
    async fn step(
        &mut self,
        _record: StepRecord,
        _sink: &mut dyn LoopSink,
    ) -> Result<(), SinkClosed> {
        Ok(())
    }
}

/// The trace of one turn of one session: every record is appended to the session's log
/// first, then sent live (`trace_warning` first when the append failed). A closed sink stops
/// the notes, never the log.
pub struct SessionTrace {
    log: Arc<TraceLog>,
    session: String,
    turn: u64,
    user: String,
    started: Instant,
    steps: u32,
    out_tokens: u32,
    think_tokens: u32,
    peak_fill: f64,
}

impl SessionTrace {
    /// Sent `{id, record}` after a step record is appended.
    pub const M_TRACE_STEP: &'static str = "trace_step";
    /// Sent `{id, record}` after the turn record is appended.
    pub const M_TRACE_TURN: &'static str = "trace_turn";
    /// Sent `{id, message}` when writing the log failed.
    pub const M_TRACE_WARNING: &'static str = "trace_warning";
    /// The turn reason of a turn cut off because no connection was left.
    pub const REASON_ABORTED: &'static str = "aborted";
    /// The stored user message is cut to this many characters.
    const USER_CHARS: usize = 200;
    const FIELD_ID: &'static str = "id";
    const FIELD_RECORD: &'static str = "record";
    const FIELD_MESSAGE: &'static str = "message";

    /// Starts the trace of a new turn: starts its clock and reserves its number from `log`.
    /// The first allocation reads the whole log, so it runs on the blocking pool. A log that
    /// cannot be read is an error: the caller runs the turn untraced.
    pub async fn start(
        log: Arc<TraceLog>,
        session: String,
        user: &str,
    ) -> io::Result<SessionTrace> {
        let started = Instant::now();
        let allocating = Arc::clone(&log);
        let turn = tokio::task::spawn_blocking(move || allocating.next_turn())
            .await
            .map_err(io::Error::other)??;
        Ok(SessionTrace {
            log,
            session,
            turn,
            user: user.chars().take(Self::USER_CHARS).collect(),
            started,
            steps: 0,
            out_tokens: 0,
            think_tokens: 0,
            peak_fill: 0.0,
        })
    }

    /// Appends the turn record, then sends it as `trace_turn`.
    pub async fn finish(
        mut self,
        reason: &str,
        committed: bool,
        sink: &mut dyn LoopSink,
    ) -> Result<(), SinkClosed> {
        let record = TurnRecord {
            turn: self.turn,
            at: Self::now_ms(),
            steps: self.steps,
            reason: reason.to_string(),
            total_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            out_tokens: self.out_tokens,
            think_tokens: self.think_tokens,
            peak_fill: self.peak_fill,
            committed,
            user: std::mem::take(&mut self.user),
        };
        self.deliver(TraceRecord::Turn(record), Self::M_TRACE_TURN, sink)
            .await
    }

    /// Appends a `reset` marker to `log`.
    pub(crate) fn reset(log: &TraceLog) -> io::Result<()> {
        log.append(&TraceRecord::Reset(ResetRecord { at: Self::now_ms() }))
    }

    /// Appends `record`, warns about anything that failed, then sends it as `method`, in its
    /// log-line shape (the same record the log and `get_trace` hold).
    async fn deliver(
        &mut self,
        record: TraceRecord,
        method: &str,
        sink: &mut dyn LoopSink,
    ) -> Result<(), SinkClosed> {
        if let Err(e) = self.log.append(&record) {
            self.warn(e.to_string(), sink).await?;
        }
        let live = record.line_value();
        let params = json!({ (Self::FIELD_ID): self.session, (Self::FIELD_RECORD): live });
        sink.notify(method, params).await
    }

    async fn warn(&self, message: String, sink: &mut dyn LoopSink) -> Result<(), SinkClosed> {
        sink.notify(
            Self::M_TRACE_WARNING,
            Self::warning(&self.session, &message),
        )
        .await
    }

    /// The `trace_warning` params for `session`.
    pub(crate) fn warning(session: &str, message: &str) -> Value {
        json!({ (Self::FIELD_ID): session, (Self::FIELD_MESSAGE): message })
    }

    /// Wall-clock Unix milliseconds.
    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }
}

#[async_trait]
impl TurnTrace for SessionTrace {
    async fn step(
        &mut self,
        mut record: StepRecord,
        sink: &mut dyn LoopSink,
    ) -> Result<(), SinkClosed> {
        record.turn = self.turn;
        record.at = Self::now_ms();
        self.steps = self.steps.saturating_add(1);
        self.out_tokens = self.out_tokens.saturating_add(record.out_tokens);
        self.think_tokens = self.think_tokens.saturating_add(record.think_tokens);
        if let (Some(used), Some(total)) = (record.prompt_tokens, record.n_ctx) {
            if total > 0 {
                self.peak_fill = self.peak_fill.max(f64::from(used) / f64::from(total));
            }
        }
        self.deliver(TraceRecord::Step(record), Self::M_TRACE_STEP, sink)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::test_support::RecordingSink;
    use std::time::{SystemTime, UNIX_EPOCH};

    const FILE: &str = "s-1-0.trace.jsonl";
    const SESSION: &str = "s-1-0";
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
                    repeat_failure: false,
                }],
                error: None,
                compaction: None,
                cost: None,
                estimated: false,
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

        /// A session trace over this directory's log, for the user message "hi".
        async fn session_trace(&self) -> SessionTrace {
            SessionTrace::start(Arc::new(self.log()), SESSION.into(), "hi")
                .await
                .unwrap()
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
    fn step_record_without_compaction_has_no_field_and_old_lines_parse() {
        let plain = TempDir::step(1, 1);
        let line = plain.line_value();
        assert!(line.get("compaction").is_none(), "{line}");
        // A line written before the field existed parses with `compaction: None`.
        let old = serde_json::to_string(&line).unwrap();
        assert!(!old.contains("compaction"), "{old}");
        assert_eq!(serde_json::from_str::<TraceRecord>(&old).unwrap(), plain);

        let mut record = TempDir::step_record(1, 2);
        let mut compaction = CompactionRecord::new("trim", 7420, 5980);
        compaction.elided_results = 3;
        record.compaction = Some(compaction);
        let with = TraceRecord::Step(record);
        let value = with.line_value();
        assert_eq!(value["compaction"]["strategy"], "trim");
        assert_eq!(value["compaction"]["before_tokens"], 7420);
        assert_eq!(value["compaction"]["after_tokens"], 5980);
        assert_eq!(value["compaction"]["elided_results"], 3);
        assert_eq!(value["compaction"]["summary_ms"], Value::Null);
        assert_eq!(value["compaction"]["summary_error"], Value::Null);
        let text = serde_json::to_string(&value).unwrap();
        assert_eq!(serde_json::from_str::<TraceRecord>(&text).unwrap(), with);
    }

    #[test]
    fn compaction_record_counts_saturate() {
        let record = CompactionRecord::new("trim", usize::MAX, 5);
        assert_eq!(record.before_tokens, u32::MAX);
        assert_eq!(record.after_tokens, 5);
        assert_eq!(CompactionRecord::count(usize::MAX), u32::MAX);
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

    #[tokio::test]
    async fn session_trace_appends_then_notifies_and_finishes() {
        let d = TempDir::new("strace");
        let mut trace = d.session_trace().await;
        let mut sink = RecordingSink::new();
        trace
            .step(TempDir::step_record(0, 1), &mut sink)
            .await
            .unwrap();
        let file = d.0.join(FILE);
        assert_eq!(TraceLog::read_records(&file).unwrap().len(), 1);
        assert_eq!(sink.methods(), [SessionTrace::M_TRACE_STEP]);
        let note = &sink.notes[0].params;
        assert_eq!(note["id"], SESSION);
        assert_eq!(note["record"]["turn"], 1);
        assert_eq!(note["record"]["step"], 1);
        assert!(note["record"]["at"].as_u64().unwrap() > 0);

        trace.finish("answered", true, &mut sink).await.unwrap();
        let records = TraceLog::read_records(&file).unwrap();
        assert_eq!(records.len(), 2);
        let TraceRecord::Turn(turn) = &records[1] else {
            panic!("not a turn record: {:?}", records[1]);
        };
        assert_eq!((turn.turn, turn.steps), (1, 1));
        assert_eq!(turn.peak_fill, 100.0 / 1000.0);
        assert!(turn.committed);
        assert_eq!(turn.reason, "answered");
        assert_eq!((turn.out_tokens, turn.think_tokens), (5, 0));
        assert_eq!(turn.user, "hi");
        assert_eq!(
            sink.methods(),
            [SessionTrace::M_TRACE_STEP, SessionTrace::M_TRACE_TURN]
        );
        assert_eq!(sink.notes[1].params["id"], SESSION);
        assert_eq!(
            sink.notes[1].params["record"],
            TraceRecord::Turn(turn.clone()).line_value()
        );
    }

    #[tokio::test]
    async fn session_trace_numbers_turns_and_cuts_the_user_message() {
        let d = TempDir::new("snumber");
        let log = Arc::new(d.log());
        let mut sink = RecordingSink::new();
        let first = SessionTrace::start(Arc::clone(&log), SESSION.into(), &"é".repeat(300))
            .await
            .unwrap();
        first.finish("answered", true, &mut sink).await.unwrap();
        let second = SessionTrace::start(log, SESSION.into(), "x").await.unwrap();
        second
            .finish(SessionTrace::REASON_ABORTED, false, &mut sink)
            .await
            .unwrap();
        let records = TraceLog::read_records(&d.0.join(FILE)).unwrap();
        let turns: Vec<&TurnRecord> = records
            .iter()
            .filter_map(|r| match r {
                TraceRecord::Turn(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!((turns[0].turn, turns[1].turn), (1, 2));
        assert_eq!(turns[0].user.chars().count(), 200);
        assert_eq!((turns[1].steps, turns[1].peak_fill), (0, 0.0));
        assert_eq!(turns[1].reason, "aborted");
    }

    #[tokio::test]
    async fn session_trace_totals_add_up_over_steps() {
        let d = TempDir::new("stotals");
        let mut trace = d.session_trace().await;
        let mut sink = RecordingSink::new();
        let fills = [
            (Some(100), Some(1000), 5, 1),
            (Some(400), Some(1000), 7, 2),
            (Some(900), None, 11, 3),
            (Some(900), Some(0), 13, 4),
            (None, Some(1000), 17, 5),
            (Some(200), Some(1000), 19, 6),
        ];
        for (step, (prompt_tokens, n_ctx, out_tokens, think_tokens)) in
            fills.into_iter().enumerate()
        {
            let mut record = TempDir::step_record(0, step as u32 + 1);
            record.prompt_tokens = prompt_tokens;
            record.n_ctx = n_ctx;
            record.out_tokens = out_tokens;
            record.think_tokens = think_tokens;
            trace.step(record, &mut sink).await.unwrap();
        }
        trace.finish("answered", true, &mut sink).await.unwrap();
        let records = TraceLog::read_records(&d.0.join(FILE)).unwrap();
        let TraceRecord::Turn(turn) = records.last().unwrap() else {
            panic!("not a turn record: {:?}", records.last());
        };
        assert_eq!(turn.steps, 6);
        assert_eq!(turn.peak_fill, 400.0 / 1000.0);
        assert_eq!(turn.out_tokens, 5 + 7 + 11 + 13 + 17 + 19);
        assert_eq!(turn.think_tokens, 1 + 2 + 3 + 4 + 5 + 6);
    }

    #[tokio::test]
    async fn session_trace_write_failure_warns_and_still_notifies() {
        let d = TempDir::new("sfail");
        let mut trace = d.session_trace().await;
        fs::create_dir(d.0.join(FILE)).unwrap();
        let mut sink = RecordingSink::new();
        trace
            .step(TempDir::step_record(0, 1), &mut sink)
            .await
            .unwrap();
        assert_eq!(
            sink.methods(),
            [SessionTrace::M_TRACE_WARNING, SessionTrace::M_TRACE_STEP]
        );
        assert_eq!(sink.notes[0].params["id"], SESSION);
        assert!(!sink.notes[0].params["message"].as_str().unwrap().is_empty());
        assert_eq!(sink.notes[1].params["record"]["turn"], 1);
    }

    #[tokio::test]
    async fn session_trace_over_an_unreadable_log_does_not_start() {
        let d = TempDir::new("sunread");
        fs::create_dir(d.0.join(FILE)).unwrap();
        let started = SessionTrace::start(Arc::new(d.log()), SESSION.into(), "hi").await;
        assert!(started.is_err());
    }

    #[test]
    fn next_turn_reserves_each_number_without_a_write() {
        let d = TempDir::new("reserve");
        let log = d.log();
        log.append(&TempDir::step(4, 1)).unwrap();
        assert_eq!(log.next_turn().unwrap(), 5);
        // Neither turn wrote anything, yet each got its own number.
        assert_eq!(log.next_turn().unwrap(), 6);
        assert_eq!(log.next_turn().unwrap(), 7);
        // A record above the counter still moves it past that turn.
        log.append(&TempDir::step(9, 1)).unwrap();
        assert_eq!(log.next_turn().unwrap(), 10);
    }

    #[tokio::test]
    async fn session_trace_appends_even_when_sink_is_closed() {
        let d = TempDir::new("sclosed");
        let mut trace = d.session_trace().await;
        let mut sink = RecordingSink::closing_after(0);
        assert_eq!(
            trace.step(TempDir::step_record(0, 1), &mut sink).await,
            Err(SinkClosed)
        );
        assert_eq!(
            trace
                .finish(SessionTrace::REASON_ABORTED, false, &mut sink)
                .await,
            Err(SinkClosed)
        );
        let records = TraceLog::read_records(&d.0.join(FILE)).unwrap();
        assert_eq!(records.len(), 2);
        assert!(matches!(&records[1], TraceRecord::Turn(t) if t.steps == 1 && !t.committed));
        assert!(sink.notes.is_empty());
    }

    #[test]
    fn reset_appends_a_marker() {
        let d = TempDir::new("sreset");
        let log = d.log();
        log.append(&TempDir::step(1, 1)).unwrap();
        SessionTrace::reset(&log).unwrap();
        let records = TraceLog::read_records(&d.0.join(FILE)).unwrap();
        assert!(matches!(&records[1], TraceRecord::Reset(r) if r.at > 0));
        assert!(log.page(None, None).unwrap().reset_pending);
    }

    #[test]
    fn new_fields_skipped_when_empty() {
        let plain = serde_json::to_value(TempDir::step_record(1, 1)).unwrap();
        assert!(plain.get("cost").is_none());
        assert!(plain.get("estimated").is_none());
        let mut record = TempDir::step_record(1, 1);
        record.cost = Some(0.5);
        record.estimated = true;
        let full = serde_json::to_value(&record).unwrap();
        assert_eq!(full["cost"], 0.5);
        assert_eq!(full["estimated"], true);
        // An old record without the fields still reads.
        let back: StepRecord = serde_json::from_value(plain).unwrap();
        assert_eq!(back.cost, None);
        assert!(!back.estimated);
        let mut c = CompactionRecord::new("summarize", 10, 5);
        assert!(serde_json::to_value(&c)
            .unwrap()
            .get("summary_cost")
            .is_none());
        c.summary_cost = Some(0.25);
        assert_eq!(serde_json::to_value(&c).unwrap()["summary_cost"], 0.25);
    }

    #[test]
    fn wire_page_is_the_page_shape_with_log_line_records() {
        let d = TempDir::new("wire");
        let log = d.log();
        log.append(&TempDir::step(1, 1)).unwrap();
        log.append(&TempDir::summary(1)).unwrap();
        log.append(&TempDir::step(2, 1)).unwrap();
        let page = log.page(None, None).unwrap();
        let plain = json!(&page);
        let mut wire = page.wire();
        assert_eq!(wire["turns"][1]["steps"][0]["v"], SCHEMA_VERSION);
        assert_eq!(wire["turns"][1]["steps"][0]["kind"], "step");
        assert_eq!(wire["turns"][1]["summary"]["kind"], "turn");
        assert_eq!(wire["turns"][0]["summary"], Value::Null);
        // Without `v` and `kind`, the wire page is exactly the page.
        for turn in wire["turns"].as_array_mut().unwrap() {
            for step in turn["steps"].as_array_mut().unwrap() {
                let step = step.as_object_mut().unwrap();
                step.remove("v");
                step.remove("kind");
            }
            if let Some(summary) = turn["summary"].as_object_mut() {
                summary.remove("v");
                summary.remove("kind");
            }
        }
        assert_eq!(wire, plain);
    }
}
