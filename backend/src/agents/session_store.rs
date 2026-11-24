//! File-backed store of sessions: one `{id}.json` per session, cached in memory.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use super::atomic_file::AtomicFile;
use super::host::ModelRequirement;
use super::session::{SessionRecord, SessionSummary};
use super::template::{AgentTemplate, CompactionState};
use crate::engine::types::ChatMessage;
use crate::engine::SamplingConfig;

const EXT: &str = "json";
const ID_PREFIX: &str = "s";
const TITLE_SEP: &str = " #";
const MILLIS_PER_SEC: u64 = 1000;
const ERR_UNKNOWN: &str = "unknown session: ";
const LOG_SKIPPED: &str = "skipping unreadable session file";
const ERR_NO_NAME: &str = "no file name";

/// An error from the session store; the message is user-presentable.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionError(pub String);

impl SessionError {
    /// `unknown session: {id}`.
    pub fn unknown(id: &str) -> Self {
        Self(format!("{ERR_UNKNOWN}{id}"))
    }
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SessionError {}

impl From<io::Error> for SessionError {
    fn from(e: io::Error) -> Self {
        Self(e.to_string())
    }
}

/// Sessions persisted under one directory. Every mutation is written atomically before the
/// in-memory copy changes, so a failed write leaves both unchanged.
///
/// All transcripts are held in memory and `get` returns a clone of the whole record, so
/// callers should avoid calling it in a hot loop. A poisoned lock is recovered, not hidden.
pub struct SessionStore {
    root: PathBuf,
    sessions: Mutex<HashMap<String, SessionRecord>>,
    counter: AtomicU64,
    clock: Clock,
}

/// Whether a mutation moves `updated` (last activity: a committed turn, create or rename).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bump {
    Updated,
    Kept,
}

/// A source of unix milliseconds.
type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

impl SessionStore {
    /// Opens (creating if needed) the store at `root` and loads every `*.json`. Files that
    /// fail to parse are skipped with a message on stderr.
    pub fn open(root: &Path) -> io::Result<Self> {
        Self::open_with_clock(root, Box::new(Self::system_millis))
    }

    /// Like [`open`](Self::open) with an injected clock returning unix milliseconds. Ids use
    /// the clock's value directly and `created`/`updated` use it divided by 1000.
    pub fn open_with_clock(root: &Path, clock: Clock) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let mut sessions = HashMap::new();
        for entry in fs::read_dir(root)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some(EXT) {
                continue;
            }
            match Self::load(&path) {
                Ok(rec) => {
                    sessions.insert(rec.id.clone(), rec);
                }
                Err(e) => eprintln!("{LOG_SKIPPED} {}: {e}", path.display()),
            }
        }
        Ok(Self {
            root: root.to_path_buf(),
            sessions: Mutex::new(sessions),
            counter: AtomicU64::new(0),
            clock,
        })
    }

    /// Reads one session file. The file stem is authoritative for the id, so `{id}.json`
    /// always names the file the record came from and no path comparison is ever needed.
    fn load(path: &Path) -> Result<SessionRecord, String> {
        let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut rec: SessionRecord = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .ok_or_else(|| ERR_NO_NAME.to_string())?;
        rec.id = stem;
        Ok(rec)
    }

    /// Starts a session from `template`, snapshotting it.
    pub fn create(
        &self,
        template_id: &str,
        template: &AgentTemplate,
    ) -> Result<SessionRecord, SessionError> {
        let mut map = self.lock();
        let n = 1 + Self::highest_number(map.values(), template_id, &template.name);
        let title = format!("{}{TITLE_SEP}{n}", template.name);
        let mut id = self.next_id();
        // Never reuse an id whose file exists on disk (e.g. a skipped corrupt file).
        while map.contains_key(&id) || self.file_for(&id).exists() {
            id = self.next_id();
        }
        let rec = SessionRecord::new(id, title, template_id, template.clone(), self.now_secs());
        self.persist(&rec)?;
        map.insert(rec.id.clone(), rec.clone());
        Ok(rec)
    }

    /// The highest `n` among titles of the form `{name} #{n}` for this template; renamed
    /// sessions (titles without the template's name prefix) are ignored.
    fn highest_number<'a>(
        records: impl Iterator<Item = &'a SessionRecord>,
        template_id: &str,
        name: &str,
    ) -> u64 {
        let prefix = format!("{name}{TITLE_SEP}");
        records
            .filter(|r| r.template_id == template_id)
            .filter_map(|r| r.title.strip_prefix(&prefix)?.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
    }

    /// All sessions, newest `updated` first (ties by id descending for a stable order).
    pub fn list(&self) -> Vec<SessionSummary> {
        let map = self.lock();
        let mut out: Vec<SessionSummary> = map.values().map(SessionRecord::summary).collect();
        out.sort_by(Self::list_order);
        out
    }

    /// Every session's summary with the model requirement of its snapshot, in [`list`](Self::list)
    /// order. Built under the lock without cloning any transcript.
    pub fn list_with_requirements(&self) -> Vec<(SessionSummary, ModelRequirement)> {
        let map = self.lock();
        let mut out: Vec<(SessionSummary, ModelRequirement)> = map
            .values()
            .map(|r| (r.summary(), ModelRequirement::of(&r.snapshot)))
            .collect();
        out.sort_by(|(a, _), (b, _)| Self::list_order(a, b));
        out
    }

    /// Newest `updated` first, ties by id descending.
    fn list_order(a: &SessionSummary, b: &SessionSummary) -> std::cmp::Ordering {
        b.updated.cmp(&a.updated).then_with(|| b.id.cmp(&a.id))
    }

    pub fn contains(&self, id: &str) -> bool {
        self.lock().contains_key(id)
    }

    pub fn get(&self, id: &str) -> Option<SessionRecord> {
        self.lock().get(id).cloned()
    }

    /// Retitles the session and bumps `updated`.
    pub fn rename(&self, id: &str, title: &str) -> Result<(), SessionError> {
        self.mutate(id, Bump::Updated, |r| r.title = title.to_string())
    }

    pub fn delete(&self, id: &str) -> Result<(), SessionError> {
        let mut map = self.lock();
        if !map.contains_key(id) {
            return Err(SessionError::unknown(id));
        }
        match fs::remove_file(self.file_for(id)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        map.remove(id);
        Ok(())
    }

    /// Replaces the transcript and bumps `updated`.
    pub fn commit(&self, id: &str, transcript: Vec<ChatMessage>) -> Result<(), SessionError> {
        self.mutate(id, Bump::Updated, |r| r.transcript = transcript)
    }

    /// Replaces the sampling and thinking overrides; `updated` (last activity) is kept.
    pub fn set_overrides(
        &self,
        id: &str,
        sampling: Option<SamplingConfig>,
        enable_thinking: Option<bool>,
    ) -> Result<(), SessionError> {
        self.mutate(id, Bump::Kept, |r| {
            r.sampling = sampling;
            r.enable_thinking = enable_thinking;
        })
    }

    /// Returns the transcript to its initial state, derived from the snapshot; `updated` is
    /// kept.
    pub fn reset(&self, id: &str) -> Result<(), SessionError> {
        self.mutate(id, Bump::Kept, |r| {
            r.transcript = SessionRecord::initial_transcript(&r.snapshot);
            r.compaction = None;
        })
    }

    /// Saves (or clears) the summary of the older transcript; `updated` is kept and nothing is
    /// broadcast.
    pub fn set_compaction(
        &self,
        id: &str,
        state: Option<CompactionState>,
    ) -> Result<(), SessionError> {
        self.mutate(id, Bump::Kept, |r| r.compaction = state)
    }

    /// Applies `f` to a copy of the record, persists it, then swaps it in.
    fn mutate(
        &self,
        id: &str,
        bump: Bump,
        f: impl FnOnce(&mut SessionRecord),
    ) -> Result<(), SessionError> {
        let mut map = self.lock();
        let mut rec = map
            .get(id)
            .cloned()
            .ok_or_else(|| SessionError::unknown(id))?;
        f(&mut rec);
        if bump == Bump::Updated {
            rec.updated = self.now_secs();
        }
        self.persist(&rec)?;
        map.insert(rec.id.clone(), rec);
        Ok(())
    }

    fn persist(&self, rec: &SessionRecord) -> io::Result<()> {
        let body = serde_json::to_vec_pretty(rec)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        AtomicFile::write(&self.root, &format!("{}.{EXT}", rec.id), &body)
    }

    fn file_for(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.{EXT}"))
    }

    /// Locks the map, recovering the data if a panicking thread poisoned the mutex.
    fn lock(&self) -> MutexGuard<'_, HashMap<String, SessionRecord>> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `s-{unix_millis}-{counter}`: lowercase ASCII, safe as a filename on every platform.
    fn next_id(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("{ID_PREFIX}-{}-{n}", (self.clock)())
    }

    fn now_secs(&self) -> u64 {
        (self.clock)() / MILLIS_PER_SEC
    }

    fn system_millis() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::template::{ContextStrategy, TemplateRules, DEFAULT_COMPACT_AT_PCT};
    use crate::agents::template_store::TemplateStore;
    use crate::engine::types::{FunctionCall, Role, ToolCall};
    use std::sync::Arc;

    /// A unique temp directory removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!("ses-{tag}-{}-{nanos}", std::process::id())))
        }

        fn store(&self) -> SessionStore {
            SessionStore::open(&self.0.join("sessions")).unwrap()
        }

        /// A store whose clock is the shared millisecond counter `ms`.
        fn clocked(&self, ms: &Arc<AtomicU64>) -> SessionStore {
            let c = Arc::clone(ms);
            SessionStore::open_with_clock(
                &self.0.join("sessions"),
                Box::new(move || c.load(Ordering::SeqCst)),
            )
            .unwrap()
        }

        fn sample(name: &str, prompt: &str) -> AgentTemplate {
            AgentTemplate {
                name: name.into(),
                system_prompt: prompt.into(),
                tools: vec!["read_file".into()],
                step_budget: 4,
                runtime: "llamacpp".into(),
                model: "gemma-4-e4b".into(),
                n_ctx: 4096,
                enable_thinking: false,
                sampling: None,
                host: None,
                context_strategy: ContextStrategy::default(),
                compact_at_pct: DEFAULT_COMPACT_AT_PCT,
            }
        }

        fn state() -> CompactionState {
            CompactionState {
                summary: "so far".into(),
                covers: 3,
                tokens: 42,
                at: 7,
            }
        }

        fn rich_transcript() -> Vec<ChatMessage> {
            let mut call = ChatMessage::new(Role::Assistant, "");
            call.tool_calls = Some(vec![ToolCall {
                id: "c1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                },
            }]);
            let mut result = ChatMessage::new(Role::Tool, "ok");
            result.tool_call_id = Some("c1".into());
            result.tool_result = Some(serde_json::json!({"text": "hello"}));
            vec![
                ChatMessage::new(Role::System, "be helpful"),
                ChatMessage::new(Role::User, "read it"),
                call,
                result,
                ChatMessage::new(Role::Assistant, "done"),
            ]
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn create_snapshots_template_and_titles() {
        let d = TempDir::new("create");
        let s = d.store();
        let t = TempDir::sample("Researcher", "be helpful");
        let a = s.create("researcher", &t).unwrap();
        let b = s.create("researcher", &t).unwrap();
        assert_eq!(a.title, "Researcher #1");
        assert_eq!(b.title, "Researcher #2");
        assert_ne!(a.id, b.id);
        assert!(a.id.starts_with("s-"));
        assert_eq!(a.schema, 1);
        assert_eq!(a.snapshot, t);
        assert_eq!(
            a.transcript,
            vec![ChatMessage::new(Role::System, "be helpful")]
        );
        assert!(d.0.join("sessions").join(format!("{}.json", a.id)).exists());
    }

    #[test]
    fn empty_prompt_starts_empty_transcript() {
        let d = TempDir::new("empty");
        let s = d.store();
        let r = s.create("x", &TempDir::sample("X", "")).unwrap();
        assert!(r.transcript.is_empty());
    }

    #[test]
    fn snapshot_is_independent_of_template_edits() {
        let d = TempDir::new("snap");
        let ts = TemplateStore::open(&d.0.join("templates"), TemplateRules::for_tests()).unwrap();
        let id = ts.save(None, "", &TempDir::sample("Snap", "v1")).unwrap();
        let (t, _) = ts.get(&id).unwrap();
        let s = d.store();
        let rec = s.create(&id, &t).unwrap();
        ts.save(Some(&id), "", &TempDir::sample("Snap", "v2"))
            .unwrap();
        assert_eq!(s.get(&rec.id).unwrap().snapshot.system_prompt, "v1");
    }

    #[test]
    fn session_survives_template_delete() {
        let d = TempDir::new("tdel");
        let ts = TemplateStore::open(&d.0.join("templates"), TemplateRules::for_tests()).unwrap();
        let id = ts.save(None, "", &TempDir::sample("Gone", "p")).unwrap();
        let (t, _) = ts.get(&id).unwrap();
        let s = d.store();
        let rec = s.create(&id, &t).unwrap();
        ts.delete(&id).unwrap();
        assert_eq!(s.get(&rec.id).unwrap().snapshot, rec.snapshot);
        drop(s);
        assert_eq!(d.store().get(&rec.id).unwrap().snapshot, rec.snapshot);
    }

    #[test]
    fn commit_persists_across_reopen() {
        let d = TempDir::new("commit");
        let s = d.store();
        let rec = s.create("r", &TempDir::sample("R", "be helpful")).unwrap();
        let transcript = TempDir::rich_transcript();
        s.commit(&rec.id, transcript.clone()).unwrap();
        drop(s);
        let got = d.store().get(&rec.id).unwrap();
        assert_eq!(got.transcript, transcript);
        assert_eq!(got.created, rec.created);
    }

    #[test]
    fn busy_is_never_persisted() {
        let d = TempDir::new("busy");
        let s = d.store();
        let rec = s.create("r", &TempDir::sample("R", "p")).unwrap();
        s.commit(&rec.id, TempDir::rich_transcript()).unwrap();
        let text =
            fs::read_to_string(d.0.join("sessions").join(format!("{}.json", rec.id))).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v.get("busy").is_none());
        assert!(!text.contains("busy"));
        drop(s);
        assert!(d.store().get(&rec.id).is_some());
    }

    #[test]
    fn reset_restores_initial_transcript() {
        let d = TempDir::new("reset");
        let s = d.store();
        let rec = s.create("r", &TempDir::sample("R", "be helpful")).unwrap();
        s.commit(&rec.id, TempDir::rich_transcript()).unwrap();
        s.reset(&rec.id).unwrap();
        assert_eq!(s.get(&rec.id).unwrap().transcript, rec.transcript);
    }

    #[test]
    fn set_compaction_persists_and_reopens() {
        let d = TempDir::new("comp");
        let s = d.store();
        let rec = s.create("r", &TempDir::sample("R", "p")).unwrap();
        assert_eq!(rec.compaction, None);
        s.set_compaction(&rec.id, Some(TempDir::state())).unwrap();
        drop(s);
        let s = d.store();
        assert_eq!(s.get(&rec.id).unwrap().compaction, Some(TempDir::state()));
        s.set_compaction(&rec.id, None).unwrap();
        assert_eq!(s.get(&rec.id).unwrap().compaction, None);
        assert!(s.set_compaction("nope", None).is_err());
    }

    #[test]
    fn set_compaction_keeps_updated() {
        let d = TempDir::new("compkeep");
        let ms = Arc::new(AtomicU64::new(10_000));
        let s = d.clocked(&ms);
        let rec = s.create("r", &TempDir::sample("R", "p")).unwrap();
        ms.store(20_000, Ordering::SeqCst);
        s.set_compaction(&rec.id, Some(TempDir::state())).unwrap();
        assert_eq!(s.get(&rec.id).unwrap().updated, 10);
    }

    #[test]
    fn reset_clears_compaction() {
        let d = TempDir::new("compreset");
        let s = d.store();
        let rec = s.create("r", &TempDir::sample("R", "p")).unwrap();
        s.set_compaction(&rec.id, Some(TempDir::state())).unwrap();
        s.reset(&rec.id).unwrap();
        assert_eq!(s.get(&rec.id).unwrap().compaction, None);
    }

    #[test]
    fn old_session_json_without_compaction_loads() {
        let d = TempDir::new("compold");
        let s = d.store();
        let rec = s.create("r", &TempDir::sample("R", "p")).unwrap();
        drop(s);
        let path = d.0.join("sessions").join(format!("{}.json", rec.id));
        let text = fs::read_to_string(&path).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v.get("compaction").is_none());
        v["snapshot"]
            .as_object_mut()
            .unwrap()
            .remove("context_strategy");
        v["snapshot"]
            .as_object_mut()
            .unwrap()
            .remove("compact_at_pct");
        fs::write(&path, v.to_string()).unwrap();
        let got = d.store().get(&rec.id).unwrap();
        assert_eq!(got.compaction, None);
        assert_eq!(got.snapshot.compact_at_pct, 75);
    }

    #[test]
    fn overrides_round_trip() {
        let d = TempDir::new("over");
        let s = d.store();
        let rec = s.create("r", &TempDir::sample("R", "p")).unwrap();
        let sampling = SamplingConfig {
            temperature: 0.25,
            ..SamplingConfig::default()
        };
        s.set_overrides(&rec.id, Some(sampling.clone()), Some(true))
            .unwrap();
        drop(s);
        let s = d.store();
        let got = s.get(&rec.id).unwrap();
        assert_eq!(got.sampling, Some(sampling));
        assert_eq!(got.enable_thinking, Some(true));
        s.set_overrides(&rec.id, None, None).unwrap();
        assert_eq!(s.get(&rec.id).unwrap().sampling, None);
    }

    #[test]
    fn corrupt_session_file_is_skipped() {
        let d = TempDir::new("corrupt");
        let s = d.store();
        let rec = s.create("r", &TempDir::sample("R", "p")).unwrap();
        drop(s);
        fs::write(d.0.join("sessions").join("bad.json"), "{ not json").unwrap();
        let s = d.store();
        assert_eq!(s.list().len(), 1);
        assert!(s.get(&rec.id).is_some());
        assert!(s.get("bad").is_none());
    }

    #[test]
    fn session_store_ignores_trace_files() {
        let d = TempDir::new("trace");
        let s = d.store();
        let rec = s.create("r", &TempDir::sample("R", "p")).unwrap();
        drop(s);
        fs::write(
            d.0.join("sessions").join("s-1-0.trace.jsonl"),
            "{\"v\":1,\"kind\":\"reset\",\"at\":1}\n",
        )
        .unwrap();
        let s = d.store();
        assert_eq!(s.list().len(), 1);
        assert!(s.get(&rec.id).is_some());
        assert!(s.get("s-1-0").is_none());
    }

    #[test]
    fn rename_delete_list_and_unknown_ids() {
        let d = TempDir::new("misc");
        let s = d.store();
        let a = s.create("r", &TempDir::sample("R", "p")).unwrap();
        s.rename(&a.id, "Mine").unwrap();
        let list = s.list();
        assert_eq!(list[0].title, "Mine");
        assert_eq!(list[0].template_name, "R");
        assert_eq!(list[0].runtime, "llamacpp");
        s.delete(&a.id).unwrap();
        assert!(s.get(&a.id).is_none());
        assert!(!d.0.join("sessions").join(format!("{}.json", a.id)).exists());
        let err = s.rename("nope", "x").unwrap_err();
        assert_eq!(err.0, "unknown session: nope");
        assert!(s.delete("nope").is_err());
        assert!(s.commit("nope", vec![]).is_err());
        assert!(s.reset("nope").is_err());
        assert!(s.set_overrides("nope", None, None).is_err());
    }

    #[test]
    fn create_never_overwrites_an_existing_id_file() {
        let d = TempDir::new("collide");
        let ms = Arc::new(AtomicU64::new(5_000));
        let s = d.clocked(&ms);
        let planted = d.0.join("sessions").join("s-5000-0.json");
        fs::write(&planted, "{ corrupt").unwrap();
        let rec = s.create("r", &TempDir::sample("R", "p")).unwrap();
        assert_eq!(rec.id, "s-5000-1");
        assert_eq!(fs::read_to_string(&planted).unwrap(), "{ corrupt");
    }

    #[test]
    fn title_numbers_do_not_repeat_after_delete() {
        let d = TempDir::new("titles");
        let s = d.store();
        let t = TempDir::sample("R", "p");
        let a = s.create("r", &t).unwrap();
        let b = s.create("r", &t).unwrap();
        assert_eq!(b.title, "R #2");
        s.delete(&a.id).unwrap();
        assert_eq!(s.create("r", &t).unwrap().title, "R #3");
        // Renamed sessions and other templates are ignored.
        s.rename(&b.id, "Custom #9").unwrap();
        assert_eq!(s.create("r", &t).unwrap().title, "R #4");
        assert_eq!(s.create("other", &t).unwrap().title, "R #1");
    }

    #[test]
    fn list_with_requirements_matches_list_order_and_snapshots() {
        let d = TempDir::new("reqs");
        let ms = Arc::new(AtomicU64::new(10_000));
        let s = d.clocked(&ms);
        let t = TempDir::sample("R", "p");
        s.create("r", &t).unwrap();
        ms.store(11_000, Ordering::SeqCst);
        s.create("r", &t).unwrap();
        let pairs = s.list_with_requirements();
        let ids: Vec<String> = pairs.iter().map(|(x, _)| x.id.clone()).collect();
        let listed: Vec<String> = s.list().into_iter().map(|x| x.id).collect();
        assert_eq!(ids, listed);
        assert_eq!(pairs[0].1, ModelRequirement::of(&t));
    }

    #[test]
    fn list_orders_by_updated_then_id_descending_and_updated_bumps() {
        let d = TempDir::new("order");
        let ms = Arc::new(AtomicU64::new(10_000));
        let s = d.clocked(&ms);
        let t = TempDir::sample("R", "p");
        let a = s.create("r", &t).unwrap();
        let b = s.create("r", &t).unwrap();
        assert_eq!(a.updated, 10);
        // Same `updated`: id descending.
        let ids: Vec<String> = s.list().into_iter().map(|x| x.id).collect();
        assert_eq!(ids, vec![b.id.clone(), a.id.clone()]);
        // A rename and a commit bump `updated`; overrides and a reset keep it.
        ms.store(11_000, Ordering::SeqCst);
        s.rename(&a.id, "n").unwrap();
        assert_eq!(s.get(&a.id).unwrap().updated, 11);
        ms.store(12_000, Ordering::SeqCst);
        s.set_overrides(&a.id, None, Some(true)).unwrap();
        assert_eq!(s.get(&a.id).unwrap().updated, 11);
        ms.store(13_000, Ordering::SeqCst);
        s.reset(&a.id).unwrap();
        assert_eq!(s.get(&a.id).unwrap().updated, 11);
        ms.store(14_000, Ordering::SeqCst);
        s.commit(&a.id, vec![]).unwrap();
        assert_eq!(s.get(&a.id).unwrap().updated, 14);
        assert_eq!(s.get(&b.id).unwrap().updated, 10);
        assert_eq!(s.list()[0].id, a.id);
    }

    #[test]
    fn concurrent_mutations_keep_both_effects_and_a_parseable_file() {
        let d = TempDir::new("conc");
        let s = Arc::new(d.store());
        let rec = s.create("r", &TempDir::sample("R", "p")).unwrap();
        let transcript = TempDir::rich_transcript();
        let mut handles = Vec::new();
        for i in 0..8 {
            let s = Arc::clone(&s);
            let id = rec.id.clone();
            let tr = transcript.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..10 {
                    if i % 2 == 0 {
                        s.commit(&id, tr.clone()).unwrap();
                    } else {
                        s.set_overrides(&id, None, Some(true)).unwrap();
                    }
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let got = s.get(&rec.id).unwrap();
        assert_eq!(got.transcript, transcript);
        assert_eq!(got.enable_thinking, Some(true));
        let text =
            fs::read_to_string(d.0.join("sessions").join(format!("{}.json", rec.id))).unwrap();
        assert_eq!(serde_json::from_str::<SessionRecord>(&text).unwrap(), got);
    }

    #[test]
    fn poisoned_lock_is_recovered() {
        let d = TempDir::new("poison");
        let s = Arc::new(d.store());
        let rec = s.create("r", &TempDir::sample("R", "p")).unwrap();
        let s2 = Arc::clone(&s);
        let _ = std::thread::spawn(move || {
            let _g = s2.sessions.lock().unwrap();
            panic!("poison");
        })
        .join();
        assert!(s.sessions.is_poisoned());
        assert!(s.get(&rec.id).is_some());
        assert_eq!(s.list().len(), 1);
    }
}
