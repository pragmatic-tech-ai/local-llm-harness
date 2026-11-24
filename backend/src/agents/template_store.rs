//! File-backed store of agent templates: a directory tree of TOML files, directories are groups.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

use super::atomic_file::AtomicFile;
use super::template::{
    AgentTemplate, ContextStrategy, RepositoryKind, RepositoryRules, RuleModel, RulesSource,
    TemplateRules, WorkspaceConfig, DEFAULT_COMPACT_AT_PCT,
};

const EXT: &str = "toml";
const SEED_GROUP: &str = "Examples";
const SEED_NAME: &str = "Default";
const SEED_ID: &str = "default";
const SEED_RUNTIME: &str = "llamacpp";
const SEED_MODEL_HINT: &str = "gemma-4";
const SEED_N_CTX: u32 = 8192;
const SEED_STEP_BUDGET: usize = 8;
const WORKER_NAME: &str = "Worker";
const WORKER_ID: &str = "worker";
const WORKER_STEP_BUDGET: usize = 16;
const WORKER_SYSTEM_PROMPT: &str = "You work in a private workspace folder. Save your results \
     there as files with write_file and append_file. Keep a running plan with note_add, and \
     check your notes before each step. Paths are relative to your workspace; prefix /sandbox/ \
     to read the shared sandbox.";
const FALLBACK_SLUG: &str = "agent";
const ERR_INVALID_PATH: &str = "invalid path";
const ERR_NOT_EMPTY: &str = "group is not empty";
const ERR_TEMPLATE_NOT_FOUND: &str = "template not found";
const ERR_GROUP_NOT_FOUND: &str = "group not found";
const ERR_GROUP_EXISTS: &str = "group already exists";
const ERR_DUPLICATE: &str = "duplicate template id: ";
const RENAME_TMP: &str = "rename-tmp";
const ERR_LOCK: &str = "template store lock poisoned";

/// An error from the template store; the message is user-presentable.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateError(pub String);

impl TemplateError {
    fn of(msg: &str) -> Self {
        Self(msg.to_string())
    }
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TemplateError {}

impl From<io::Error> for TemplateError {
    fn from(e: io::Error) -> Self {
        Self(e.to_string())
    }
}

/// A node of the template tree. `path` is `/`-separated, relative to the templates root.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TemplateNode {
    Group {
        name: String,
        path: String,
        children: Vec<TemplateNode>,
    },
    Template {
        id: String,
        path: String,
        name: String,
        runtime: String,
        model: String,
        error: Option<String>,
    },
}

impl TemplateNode {
    /// Groups first, then by name; the path (and id) break ties so ordering is deterministic.
    fn sort_key(&self) -> (u8, String, String, String) {
        match self {
            TemplateNode::Group { name, path, .. } => {
                (0, name.to_ascii_lowercase(), path.clone(), String::new())
            }
            TemplateNode::Template { name, path, id, .. } => {
                (1, name.to_ascii_lowercase(), path.clone(), id.clone())
            }
        }
    }

    /// Counts template ids (case-insensitively) across the whole tree.
    fn count_ids(nodes: &[TemplateNode], counts: &mut HashMap<String, usize>) {
        for n in nodes {
            match n {
                TemplateNode::Group { children, .. } => Self::count_ids(children, counts),
                TemplateNode::Template { id, .. } => {
                    *counts.entry(id.to_ascii_lowercase()).or_insert(0) += 1;
                }
            }
        }
    }

    /// Marks every template whose id occurs more than once with a duplicate error.
    fn flag_duplicates(nodes: &mut [TemplateNode], counts: &HashMap<String, usize>) {
        for n in nodes {
            match n {
                TemplateNode::Group { children, .. } => Self::flag_duplicates(children, counts),
                TemplateNode::Template { id, error, .. } => {
                    if counts.get(&id.to_ascii_lowercase()).copied().unwrap_or(0) > 1 {
                        let dup = format!("{ERR_DUPLICATE}{id}");
                        *error = Some(match error.take() {
                            Some(parse) => format!("{parse}; {dup}"),
                            None => dup,
                        });
                    }
                }
            }
        }
    }
}

pub struct TemplateStore {
    root: PathBuf,
    /// Read on every validation, so the rules follow the catalog (see [`RulesSource`]).
    rules: Box<dyn RulesSource>,
    write_lock: Mutex<()>,
}

impl TemplateStore {
    /// Opens (creating if needed) the store at `root`; seeds `Examples/Default.toml` and
    /// `Examples/Worker.toml` when the root is missing or empty.
    pub fn open(root: &Path, rules: impl RulesSource + 'static) -> io::Result<Self> {
        let fresh = !root.exists() || fs::read_dir(root)?.next().is_none();
        fs::create_dir_all(root)?;
        let store = Self {
            root: root.to_path_buf(),
            rules: Box::new(rules),
            write_lock: Mutex::new(()),
        };
        if fresh {
            let rules = store.rules.rules();
            let dir = root.join(SEED_GROUP);
            fs::create_dir_all(&dir)?;
            Self::write_atomic(&dir, SEED_ID, &Self::seed_template(&rules))?;
            Self::write_atomic(&dir, WORKER_ID, &Self::worker_template(&rules))?;
        }
        Ok(store)
    }

    /// `Examples/Default`: every tool without side effects, in a session workspace.
    fn seed_template(rules: &TemplateRules) -> AgentTemplate {
        let mut seed = Self::seed_base(rules);
        seed.tools.retain(|t| !rules.writing_tools.contains(t));
        seed
    }

    /// `Examples/Worker`: every tool, a larger budget and trimming, told to save its results
    /// in its workspace and keep a plan in its notes.
    fn worker_template(rules: &TemplateRules) -> AgentTemplate {
        AgentTemplate {
            name: WORKER_NAME.to_string(),
            system_prompt: WORKER_SYSTEM_PROMPT.to_string(),
            step_budget: WORKER_STEP_BUDGET,
            context_strategy: ContextStrategy::Trim,
            ..Self::seed_base(rules)
        }
    }

    /// What the seeds share: the runtime, the model picked for it, the context size and every
    /// tool.
    fn seed_base(rules: &TemplateRules) -> AgentTemplate {
        let models = rules.pragmatic_models();
        let capable = |m: &&RuleModel| m.runtimes.iter().any(|r| r == SEED_RUNTIME);
        let model = models
            .iter()
            .filter(capable)
            .find(|m| m.id.contains(SEED_MODEL_HINT))
            .or_else(|| models.iter().find(capable))
            .map(|m| m.id.clone())
            .unwrap_or_default();
        AgentTemplate {
            name: SEED_NAME.to_string(),
            system_prompt: String::new(),
            tools: rules.tools.clone(),
            step_budget: SEED_STEP_BUDGET,
            runtime: SEED_RUNTIME.to_string(),
            model,
            n_ctx: SEED_N_CTX,
            enable_thinking: false,
            sampling: None,
            repository: None,
            context_strategy: ContextStrategy::default(),
            compact_at_pct: DEFAULT_COMPACT_AT_PCT,
            workspace: WorkspaceConfig::default(),
        }
    }

    /// The whole tree. A template that fails to parse, or parses but fails the same validation
    /// as `save`, carries the error text. Templates whose id (file stem) occurs more than once
    /// anywhere in the tree are all flagged with `duplicate template id: {id}`.
    pub fn tree(&self) -> Vec<TemplateNode> {
        let rules = self.rules.rules();
        let mut nodes = Self::read_level(&self.root, "", &rules);
        let mut counts = HashMap::new();
        TemplateNode::count_ids(&nodes, &mut counts);
        TemplateNode::flag_duplicates(&mut nodes, &counts);
        nodes
    }

    fn read_level(dir: &Path, rel: &str, rules: &TemplateRules) -> Vec<TemplateNode> {
        let mut nodes = Vec::new();
        let Ok(entries) = fs::read_dir(dir) else {
            return nodes;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            let file_name = entry.file_name().to_string_lossy().to_string();
            let child_rel = if rel.is_empty() {
                file_name.clone()
            } else {
                format!("{rel}/{file_name}")
            };
            if p.is_dir() {
                nodes.push(TemplateNode::Group {
                    name: file_name,
                    path: child_rel.clone(),
                    children: Self::read_level(&p, &child_rel, rules),
                });
            } else if p.extension().and_then(|e| e.to_str()) == Some(EXT) {
                let id = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                let parsed = fs::read_to_string(&p)
                    .map_err(|e| e.to_string())
                    .and_then(|s| toml::from_str::<AgentTemplate>(&s).map_err(|e| e.to_string()));
                nodes.push(match parsed {
                    Ok(t) => TemplateNode::Template {
                        error: rules.validate(&t).err(),
                        id,
                        path: rel.to_string(),
                        name: t.name,
                        runtime: t.runtime,
                        model: t.model,
                    },
                    Err(e) => TemplateNode::Template {
                        name: id.clone(),
                        id,
                        path: rel.to_string(),
                        runtime: String::new(),
                        model: String::new(),
                        error: Some(e),
                    },
                });
            }
        }
        nodes.sort_by_key(TemplateNode::sort_key);
        nodes
    }

    pub fn get(&self, id: &str) -> Result<(AgentTemplate, String), TemplateError> {
        let (file, rel) = self.locate(id)?;
        let text = fs::read_to_string(&file)?;
        let t = toml::from_str::<AgentTemplate>(&text)
            .map_err(|e| TemplateError(format!("{id}: {e}")))?;
        Ok((t, rel))
    }

    /// Why `t` fails validation against the current rules, or `None` when it passes.
    pub fn check(&self, t: &AgentTemplate) -> Option<String> {
        self.rules.rules().validate(t).err()
    }

    pub fn save(
        &self,
        id: Option<&str>,
        path: &str,
        t: &AgentTemplate,
    ) -> Result<String, TemplateError> {
        if let Some(e) = self.check(t) {
            return Err(TemplateError(e));
        }
        let t = &AgentTemplate {
            workspace: t.workspace.clone().normalized(),
            ..t.clone()
        };
        let dir = self.dir_for(path)?;
        let _g = self.lock()?;
        let (id, old) = match id {
            Some(id) => {
                let (old_file, _) = self.locate(id)?;
                // Keep the on-disk stem (and its casing) as the id: a save never renames a file.
                let stem = old_file
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| id.to_string());
                (stem, Some(old_file))
            }
            None => (self.unique_id(&t.name), None),
        };
        fs::create_dir_all(&dir)?;
        // When the template moves between groups the new file is written first and the old one
        // removed after. A crash in between leaves the id in two groups; `tree` then flags both
        // as duplicates and `locate` refuses the id until the user deletes or renames one file
        // by hand (and refreshes). That is preferred to losing the template.
        Self::write_atomic(&dir, &id, t)?;
        if let Some(old) = old {
            let new_file = dir.join(format!("{id}.{EXT}"));
            if !Self::same_file(&old, &new_file) {
                if let Err(e) = fs::remove_file(&old) {
                    // Roll back so no unresolvable duplicate is left behind.
                    let _ = fs::remove_file(&new_file);
                    return Err(e.into());
                }
            }
        }
        Ok(id)
    }

    /// File identity: compares canonicalized paths (the filesystem's own casing, so non-ASCII
    /// case differences are handled); falls back to ASCII-insensitive name equality if either
    /// path cannot be canonicalized. Call only after the new file has been written.
    fn same_file(a: &Path, b: &Path) -> bool {
        match (fs::canonicalize(a), fs::canonicalize(b)) {
            (Ok(x), Ok(y)) => x == y,
            _ => a
                .to_string_lossy()
                .eq_ignore_ascii_case(&b.to_string_lossy()),
        }
    }

    pub fn delete(&self, id: &str) -> Result<(), TemplateError> {
        let _g = self.lock()?;
        let (file, _) = self.locate(id)?;
        fs::remove_file(file)?;
        Ok(())
    }

    pub fn create_group(&self, path: &str) -> Result<(), TemplateError> {
        let dir = self.dir_for(path)?;
        let _g = self.lock()?;
        fs::create_dir_all(dir)?;
        Ok(())
    }

    pub fn rename_group(&self, path: &str, new_name: &str) -> Result<(), TemplateError> {
        let dir = self.dir_for(path)?;
        if path.is_empty() || !Self::valid_segment(new_name) {
            return Err(TemplateError::of(ERR_INVALID_PATH));
        }
        let _g = self.lock()?;
        if !dir.is_dir() {
            return Err(TemplateError::of(ERR_GROUP_NOT_FOUND));
        }
        let current = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if current == new_name {
            return Ok(());
        }
        let target = dir.with_file_name(new_name);
        if current.eq_ignore_ascii_case(new_name) {
            // Case-only rename: on a case-insensitive filesystem the target "exists" (it is the
            // source), so go through a temporary name.
            let temp = dir.with_file_name(format!(".{new_name}.{RENAME_TMP}"));
            fs::rename(&dir, &temp)?;
            if let Err(e) = fs::rename(&temp, &target) {
                // Put the group back under its original name.
                let _ = fs::rename(&temp, &dir);
                return Err(e.into());
            }
            return Ok(());
        }
        if target.exists() {
            return Err(TemplateError::of(ERR_GROUP_EXISTS));
        }
        fs::rename(dir, target)?;
        Ok(())
    }

    pub fn delete_group(&self, path: &str) -> Result<(), TemplateError> {
        let dir = self.dir_for(path)?;
        if path.is_empty() {
            return Err(TemplateError::of(ERR_INVALID_PATH));
        }
        let _g = self.lock()?;
        if !dir.is_dir() {
            return Err(TemplateError::of(ERR_GROUP_NOT_FOUND));
        }
        if fs::read_dir(&dir)?.next().is_some() {
            return Err(TemplateError::of(ERR_NOT_EMPTY));
        }
        fs::remove_dir(dir)?;
        Ok(())
    }

    pub fn move_template(&self, id: &str, path: &str) -> Result<(), TemplateError> {
        let dir = self.dir_for(path)?;
        let _g = self.lock()?;
        let (old, _) = self.locate(id)?;
        fs::create_dir_all(&dir)?;
        let stem = old
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| id.to_string());
        fs::rename(old, dir.join(format!("{stem}.{EXT}")))?;
        Ok(())
    }

    // ---- helpers -------------------------------------------------------------------------

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ()>, TemplateError> {
        self.write_lock
            .lock()
            .map_err(|_| TemplateError::of(ERR_LOCK))
    }

    /// True for Windows device names (CON, PRN, AUX, NUL, COM1-9, LPT1-9), case-insensitive,
    /// with or without an extension.
    fn is_reserved(s: &str) -> bool {
        let stem = s.split('.').next().unwrap_or_default().trim_end();
        let up = stem.to_ascii_uppercase();
        if matches!(up.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
            return true;
        }
        let digit = |p: &str| {
            up.strip_prefix(p)
                .is_some_and(|d| matches!(d.as_bytes(), [b'1'..=b'9']))
        };
        digit("COM") || digit("LPT")
    }

    fn valid_segment(s: &str) -> bool {
        !s.is_empty()
            && s != "."
            && s != ".."
            && !s.contains(['/', '\\', ':'])
            && !s.ends_with('.')
            && !s.ends_with(' ')
            && !Self::is_reserved(s)
    }

    /// Resolves a `/`-separated relative group path to a directory under the root. The empty
    /// string is the root. Trailing slashes (`a/`), leading slashes, empty segments, `..`,
    /// drive prefixes and reserved names are all rejected with `invalid path`.
    fn dir_for(&self, path: &str) -> Result<PathBuf, TemplateError> {
        let mut dir = self.root.clone();
        if path.is_empty() {
            return Ok(dir);
        }
        for seg in path.split('/') {
            if !Self::valid_segment(seg) {
                return Err(TemplateError::of(ERR_INVALID_PATH));
            }
            dir.push(seg);
        }
        Ok(dir)
    }

    /// Finds the file for `id` anywhere in the tree: (file path, relative group path).
    /// Any id that is a valid path segment without `.` is accepted, so hand-named files such as
    /// `My Agent.toml` work. An id present in more than one group is refused with
    /// `duplicate template id: {id}` (for every operation, `delete` and `move_template`
    /// included: the user resolves it by renaming or deleting one file by hand and refreshing).
    fn locate(&self, id: &str) -> Result<(PathBuf, String), TemplateError> {
        let not_found = || TemplateError(format!("{ERR_TEMPLATE_NOT_FOUND}: {id}"));
        if !Self::valid_segment(id) || id.contains('.') {
            return Err(not_found());
        }
        let mut found = Vec::new();
        Self::find(&self.root, "", &format!("{id}.{EXT}"), &mut found);
        match found.len() {
            0 => Err(not_found()),
            1 => Ok(found.remove(0)),
            _ => Err(TemplateError(format!("{ERR_DUPLICATE}{id}"))),
        }
    }

    fn find(dir: &Path, rel: &str, file_name: &str, out: &mut Vec<(PathBuf, String)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                let child = if rel.is_empty() {
                    name
                } else {
                    format!("{rel}/{name}")
                };
                Self::find(&p, &child, file_name, out);
            } else if name.eq_ignore_ascii_case(file_name) {
                out.push((p, rel.to_string()));
            }
        }
    }

    fn collect_ids(dir: &Path, out: &mut HashSet<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                Self::collect_ids(&p, out);
            } else if p.extension().and_then(|e| e.to_str()) == Some(EXT) {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    out.insert(stem.to_ascii_lowercase());
                }
            }
        }
    }

    /// A fresh slug id for `name`. A slug that is a Windows reserved name gets a `-agent`
    /// suffix (`con` becomes `con-agent`).
    fn unique_id(&self, name: &str) -> String {
        let mut taken = HashSet::new();
        Self::collect_ids(&self.root, &mut taken);
        let base = Self::slugify(name);
        let base = if base.is_empty() {
            FALLBACK_SLUG.to_string()
        } else if Self::is_reserved(&base) {
            format!("{base}-{FALLBACK_SLUG}")
        } else {
            base
        };
        if !taken.contains(&base) {
            return base;
        }
        (2..)
            .map(|n| format!("{base}-{n}"))
            .find(|c| !taken.contains(c))
            .expect("unbounded range")
    }

    /// Lowercase ASCII alphanumerics and `-`; other runs collapse to one `-`; edges trimmed.
    fn slugify(name: &str) -> String {
        let mut out = String::new();
        for c in name.chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c.to_ascii_lowercase());
            } else if !out.ends_with('-') {
                out.push('-');
            }
        }
        out.trim_matches('-').to_string()
    }

    fn write_atomic(dir: &Path, id: &str, t: &AgentTemplate) -> io::Result<()> {
        let body = toml::to_string_pretty(t)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        AtomicFile::write(dir, &format!("{id}.{EXT}"), body.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A unique temp directory removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!("tpl-{tag}-{}-{nanos}", std::process::id())))
        }

        fn store(&self) -> TemplateStore {
            TemplateStore::open(&self.0.join("templates"), TemplateRules::for_tests()).unwrap()
        }

        fn invalid() -> TemplateError {
            TemplateError("invalid path".into())
        }

        fn hand_file(&self, rel: &str, name: &str) {
            let dir = self.0.join("templates").join(rel);
            fs::create_dir_all(&dir).unwrap();
            let body = toml::to_string_pretty(&TempDir::sample(name)).unwrap();
            fs::write(dir.join(format!("{name}.toml")), body).unwrap();
        }

        fn sample(name: &str) -> AgentTemplate {
            AgentTemplate {
                name: name.into(),
                system_prompt: "be helpful".into(),
                tools: vec!["read_file".into()],
                step_budget: 4,
                runtime: "llamacpp".into(),
                model: "gemma-4-e4b".into(),
                n_ctx: 4096,
                enable_thinking: true,
                sampling: Some(crate::engine::SamplingConfig::default()),
                repository: Some("hive".into()),
                context_strategy: ContextStrategy::default(),
                compact_at_pct: DEFAULT_COMPACT_AT_PCT,
                workspace: WorkspaceConfig::default(),
            }
        }

        /// The test rules with one writing tool and one note tool added.
        fn writing_rules() -> TemplateRules {
            let mut rules = TemplateRules::for_tests();
            rules.tools.push("write_file".into());
            rules.tools.push("note_add".into());
            rules.writing_tools = vec!["write_file".into()];
            rules
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn tree_flags_an_unknown_context_strategy_verbatim() {
        let d = TempDir::new("badstrategy");
        let s = d.store();
        let dir = d.0.join("templates").join("grp");
        fs::create_dir_all(&dir).unwrap();
        let body = toml::to_string_pretty(&TempDir::sample("Bad")).unwrap();
        let body = body.replace(
            "context_strategy = \"off\"",
            "context_strategy = \"shrink\"",
        );
        assert!(body.contains("shrink"));
        fs::write(dir.join("Bad.toml"), body).unwrap();
        let tree = s.tree();
        let bad = |c: &TemplateNode| match c {
            TemplateNode::Template { error: Some(e), .. } => {
                e.contains("unknown context strategy: shrink")
            }
            _ => false,
        };
        let found = tree.iter().any(|g| match g {
            TemplateNode::Group { children, .. } => children.iter().any(bad),
            _ => false,
        });
        assert!(found, "{tree:?}");
    }

    #[test]
    fn seeds_examples_default_when_empty() {
        let d = TempDir::new("seed");
        let s = d.store();
        assert_eq!(
            s.tree(),
            vec![TemplateNode::Group {
                name: "Examples".into(),
                path: "Examples".into(),
                children: vec![
                    TemplateNode::Template {
                        id: "default".into(),
                        path: "Examples".into(),
                        name: "Default".into(),
                        runtime: "llamacpp".into(),
                        model: "gemma-4-e4b".into(),
                        error: None,
                    },
                    TemplateNode::Template {
                        id: "worker".into(),
                        path: "Examples".into(),
                        name: "Worker".into(),
                        runtime: "llamacpp".into(),
                        model: "gemma-4-e4b".into(),
                        error: None,
                    },
                ],
            }]
        );
        let (t, _) = s.get("default").unwrap();
        assert_eq!(t.tools, TemplateRules::for_tests().tools);
        assert_eq!(t.n_ctx, 8192);
    }

    #[test]
    fn seeds_default_without_writing_tools_and_worker_with_all() {
        let d = TempDir::new("seedtools");
        let s = TemplateStore::open(&d.0.join("templates"), TempDir::writing_rules()).unwrap();
        let (default, _) = s.get("default").unwrap();
        assert_eq!(default.tools, ["read_file", "web_search", "note_add"]);
        assert_eq!(default.workspace, WorkspaceConfig::default());
        // Written explicitly, not left to the serde default.
        let file = fs::read_to_string(d.0.join("templates/Examples/default.toml")).unwrap();
        assert!(
            file.contains("[workspace]") && file.contains("mode = \"session\""),
            "{file}"
        );
        let (worker, _) = s.get("worker").unwrap();
        assert_eq!(worker.tools, TempDir::writing_rules().tools);
        assert_eq!(s.check(&default), None);
        assert_eq!(s.check(&worker), None);
    }

    #[test]
    fn worker_seed_has_budget_16_and_trim() {
        let d = TempDir::new("seedworker");
        let s = TemplateStore::open(&d.0.join("templates"), TempDir::writing_rules()).unwrap();
        let (default, _) = s.get("default").unwrap();
        let (worker, path) = s.get("worker").unwrap();
        assert_eq!(path, "Examples");
        assert_eq!(worker.name, "Worker");
        assert_eq!(worker.step_budget, 16);
        assert_eq!(worker.context_strategy, ContextStrategy::Trim);
        assert_eq!(worker.workspace, WorkspaceConfig::default());
        assert_eq!(
            worker.system_prompt,
            "You work in a private workspace folder. Save your results there as files with \
             write_file and append_file. Keep a running plan with note_add, and check your notes \
             before each step. Paths are relative to your workspace; prefix /sandbox/ to read \
             the shared sandbox."
        );
        assert_eq!(
            (&worker.runtime, &worker.model, worker.n_ctx),
            (&default.runtime, &default.model, default.n_ctx)
        );
        assert_eq!(worker.model, "gemma-4-e4b");
        // An existing root gets no seed, Worker included.
        s.delete("worker").unwrap();
        let reopened =
            TemplateStore::open(&d.0.join("templates"), TempDir::writing_rules()).unwrap();
        assert!(reopened.get("worker").is_err());
    }

    #[test]
    fn round_trips_template_toml() {
        let d = TempDir::new("rt");
        let s = d.store();
        let id = s.save(None, "", &TempDir::sample("Round")).unwrap();
        let (t, path) = s.get(&id).unwrap();
        assert_eq!(t, TempDir::sample("Round"));
        assert_eq!(path, "");
    }

    #[test]
    fn directory_tree_maps_to_groups() {
        let d = TempDir::new("tree");
        let s = d.store();
        s.save(None, "a/b", &TempDir::sample("x")).unwrap();
        let tree = s.tree();
        let TemplateNode::Group { name, children, .. } = &tree[0] else {
            panic!("expected group");
        };
        assert_eq!(name, "a");
        let TemplateNode::Group {
            name,
            path,
            children,
        } = &children[0]
        else {
            panic!("expected group");
        };
        assert_eq!((name.as_str(), path.as_str()), ("b", "a/b"));
        assert!(
            matches!(&children[0], TemplateNode::Template { id, path, .. }
            if id == "x" && path == "a/b")
        );
    }

    #[test]
    fn rejects_unknown_tool_runtime_model() {
        let d = TempDir::new("val");
        let s = d.store();
        let mut t = TempDir::sample("v");
        t.tools = vec!["nope".into()];
        assert_eq!(
            s.save(None, "", &t),
            Err(TemplateError("unknown tool: nope".into()))
        );
        let mut t = TempDir::sample("v");
        t.runtime = "zzz".into();
        assert_eq!(
            s.save(None, "", &t),
            Err(TemplateError("unknown runtime: zzz".into()))
        );
        let mut t = TempDir::sample("v");
        t.model = "mmm".into();
        assert_eq!(
            s.save(None, "", &t),
            Err(TemplateError("unknown model: mmm".into()))
        );
        let mut t = TempDir::sample("v");
        t.runtime = "candle".into();
        assert_eq!(
            s.save(None, "", &t),
            Err(TemplateError(
                "runtime candle cannot load model gemma-4-e4b".into()
            ))
        );
        let mut t = TempDir::sample("v");
        t.step_budget = 0;
        assert_eq!(
            s.save(None, "", &t),
            Err(TemplateError("step_budget must be at least 1".into()))
        );
    }

    #[test]
    fn broken_template_is_listed_with_error() {
        let d = TempDir::new("broken");
        let s = d.store();
        fs::write(d.0.join("templates").join("bad.toml"), "not = [valid").unwrap();
        let tree = s.tree();
        assert_eq!(tree.len(), 2);
        assert!(matches!(&tree[0], TemplateNode::Group { name, .. } if name == "Examples"));
        assert!(
            matches!(&tree[1], TemplateNode::Template { id, error: Some(_), .. } if id == "bad")
        );
        assert!(s.get("bad").is_err());
        assert!(s.get("default").is_ok());
    }

    /// A rule set the test can change after the store is open.
    struct ChangingRules(std::sync::Arc<Mutex<TemplateRules>>);

    impl RulesSource for ChangingRules {
        fn rules(&self) -> TemplateRules {
            self.0.lock().unwrap().clone()
        }
    }

    #[test]
    fn template_failing_validation_is_listed_with_its_error() {
        let d = TempDir::new("invalid");
        let s = d.store();
        // (id, runtime, extra TOML, the validation error)
        let cases = [
            (
                "tool",
                "llamacpp",
                "tools = [\"nope\"]",
                "unknown tool: nope",
            ),
            (
                "budget",
                "llamacpp",
                "step_budget = 0",
                "step_budget must be at least 1",
            ),
            (
                "runtime",
                "candle",
                "",
                "runtime candle cannot load model gemma-4-e4b",
            ),
            (
                "repository",
                "llamacpp",
                "repository = \"far\"",
                "unknown repository: far",
            ),
        ];
        for (id, runtime, extra, _) in cases {
            let body = format!(
                "name = \"{id}\"
runtime = \"{runtime}\"
model = \"gemma-4-e4b\"
n_ctx = 4096
{extra}
"
            );
            fs::write(d.0.join("templates").join(format!("{id}.toml")), body).unwrap();
        }
        let tree = s.tree();
        for (id, _, _, want) in cases {
            let node = tree
                .iter()
                .find(|n| matches!(n, TemplateNode::Template { id: i, .. } if i == id))
                .unwrap_or_else(|| panic!("{id} missing"));
            let TemplateNode::Template {
                error, name, model, ..
            } = node
            else {
                unreachable!()
            };
            assert_eq!(error.as_deref(), Some(want), "{id}");
            // Parsed, so the node carries the template's real fields.
            assert_eq!(name, id);
            assert_eq!(model, "gemma-4-e4b");
            // Still readable, so the editor can show and repair it.
            let (t, _) = s.get(id).unwrap();
            assert_eq!(s.check(&t).as_deref(), Some(want));
        }
    }

    #[test]
    fn rules_are_read_on_every_use() {
        let d = TempDir::new("liverules");
        let shared = std::sync::Arc::new(Mutex::new(TemplateRules::for_tests()));
        let s = TemplateStore::open(
            &d.0.join("templates"),
            ChangingRules(std::sync::Arc::clone(&shared)),
        )
        .unwrap();
        let mut t = TempDir::sample("Fresh");
        t.model = "dropped-in".into();
        assert_eq!(
            s.save(None, "", &t).unwrap_err().0,
            "unknown model: dropped-in"
        );
        shared.lock().unwrap().repositories[0]
            .models
            .as_mut()
            .unwrap()
            .push(RuleModel::of("dropped-in", &["llamacpp"]));
        let id = s.save(None, "", &t).unwrap();
        assert!(s
            .tree()
            .iter()
            .any(|n| matches!(n, TemplateNode::Template { id: i, error: None, .. } if *i == id)));
        // Removed again: the tree flags it without a restart.
        shared.lock().unwrap().repositories[0]
            .models
            .as_mut()
            .unwrap()
            .pop();
        assert!(s.tree().iter().any(
            |n| matches!(n, TemplateNode::Template { id: i, error: Some(_), .. } if *i == id)
        ));
    }

    #[test]
    fn slug_ids_are_unique() {
        let d = TempDir::new("slug");
        let s = d.store();
        assert_eq!(
            s.save(None, "", &TempDir::sample("My Agent")).unwrap(),
            "my-agent"
        );
        assert_eq!(
            s.save(None, "g", &TempDir::sample("My Agent")).unwrap(),
            "my-agent-2"
        );
    }

    #[test]
    fn delete_group_requires_empty() {
        let d = TempDir::new("delg");
        let s = d.store();
        assert_eq!(
            s.delete_group("Examples"),
            Err(TemplateError("group is not empty".into()))
        );
        s.create_group("empty").unwrap();
        s.delete_group("empty").unwrap();
        s.delete("default").unwrap();
        assert_eq!(
            s.delete_group("Examples"),
            Err(TemplateError("group is not empty".into()))
        );
        s.delete("worker").unwrap();
        s.delete_group("Examples").unwrap();
    }

    #[test]
    fn move_template_changes_path_keeps_id() {
        let d = TempDir::new("mv");
        let s = d.store();
        s.move_template("default", "other/deep").unwrap();
        let (_, path) = s.get("default").unwrap();
        assert_eq!(path, "other/deep");
    }

    #[test]
    fn rejects_escaping_paths() {
        let d = TempDir::new("esc");
        let s = d.store();
        let bad = Err(TemplateError("invalid path".into()));
        assert_eq!(s.save(None, "../x", &TempDir::sample("e")), bad);
        assert_eq!(s.save(None, "C:/x", &TempDir::sample("e")), bad);
        assert_eq!(s.save(None, "/x", &TempDir::sample("e")), bad);
        assert_eq!(
            s.create_group("a/../../x"),
            Err(TemplateError("invalid path".into()))
        );
    }

    #[test]
    fn rename_group_and_resave_in_place() {
        let d = TempDir::new("ren");
        let s = d.store();
        s.rename_group("Examples", "Samples").unwrap();
        let (_, path) = s.get("default").unwrap();
        assert_eq!(path, "Samples");
        let mut t = s.get("default").unwrap().0;
        t.name = "Renamed".into();
        assert_eq!(s.save(Some("default"), "Samples", &t).unwrap(), "default");
    }

    #[test]
    fn hand_named_file_is_fully_usable() {
        let d = TempDir::new("hand");
        let root = d.0.join("templates");
        fs::create_dir_all(&root).unwrap();
        d.hand_file("", "My Agent");
        let s = d.store();
        assert_eq!(s.tree().len(), 1, "root is non-empty so no seed");
        assert!(
            matches!(&s.tree()[0], TemplateNode::Template { id, error: None, .. } if id == "My Agent")
        );
        assert_eq!(s.get("My Agent").unwrap().1, "");
        let mut t = TempDir::sample("Renamed");
        t.name = "Renamed".into();
        assert_eq!(s.save(Some("My Agent"), "g", &t).unwrap(), "My Agent");
        assert_eq!(s.get("My Agent").unwrap().1, "g");
        s.move_template("My Agent", "h").unwrap();
        s.delete("My Agent").unwrap();
        assert!(s.get("My Agent").is_err());
    }

    #[test]
    fn duplicate_ids_are_flagged_and_refused() {
        let d = TempDir::new("dup");
        d.hand_file("a", "x");
        d.hand_file("b", "x");
        let s = d.store();
        let msg = "duplicate template id: x";
        let flat = |nodes: &[TemplateNode]| {
            nodes
                .iter()
                .flat_map(|n| match n {
                    TemplateNode::Group { children, .. } => children.clone(),
                    t => vec![t.clone()],
                })
                .collect::<Vec<_>>()
        };
        let leaves = flat(&s.tree());
        assert_eq!(leaves.len(), 2);
        for l in leaves {
            assert!(matches!(l, TemplateNode::Template { error: Some(e), .. } if e == msg));
        }
        assert_eq!(s.get("x"), Err(TemplateError(msg.into())));
        assert_eq!(s.delete("x"), Err(TemplateError(msg.into())));
        assert_eq!(s.move_template("x", "c"), Err(TemplateError(msg.into())));
    }

    #[test]
    fn group_rename_same_name_and_case_only() {
        let d = TempDir::new("case");
        let s = d.store();
        s.rename_group("Examples", "Examples").unwrap();
        assert_eq!(s.get("default").unwrap().1, "Examples");
        s.rename_group("Examples", "examples").unwrap();
        assert_eq!(s.get("default").unwrap().1, "examples");
        assert!(matches!(&s.tree()[0], TemplateNode::Group { name, .. } if name == "examples"));
        s.create_group("other").unwrap();
        assert_eq!(
            s.rename_group("other", "examples"),
            Err(TemplateError("group already exists".into()))
        );
    }

    #[test]
    fn reserved_names_are_rejected_or_suffixed() {
        let d = TempDir::new("resv");
        let s = d.store();
        for bad in [
            "con", "AUX", "Com1", "lpt9.txt", "nul", "trail ", "dots.", "a/prn",
        ] {
            assert_eq!(s.create_group(bad), Err(TempDir::invalid()), "{bad}");
        }
        s.create_group("com10").unwrap();
        assert_eq!(
            s.save(None, "", &TempDir::sample("CON")).unwrap(),
            "con-agent"
        );
        assert_eq!(
            s.save(None, "", &TempDir::sample("com1")).unwrap(),
            "com1-agent"
        );
    }

    #[test]
    fn failed_write_removes_tmp_file() {
        let d = TempDir::new("tmp");
        let s = d.store();
        let dir = d.0.join("templates");
        fs::create_dir_all(dir.join("x.toml")).unwrap();
        assert!(TemplateStore::write_atomic(&dir, "x", &TempDir::sample("x")).is_err());
        assert!(!dir.join(".x.toml.tmp").exists());
        drop(s);
    }

    #[test]
    fn save_unknown_id_is_not_found() {
        let d = TempDir::new("nope");
        let s = d.store();
        assert_eq!(
            s.save(Some("nope"), "", &TempDir::sample("n")),
            Err(TemplateError("template not found: nope".into()))
        );
    }

    #[test]
    fn id_is_stable_across_name_change() {
        let d = TempDir::new("stable");
        let s = d.store();
        let id = s.save(None, "", &TempDir::sample("First")).unwrap();
        let mut t = TempDir::sample("Second");
        t.name = "Second".into();
        assert_eq!(s.save(Some(&id), "", &t).unwrap(), id);
        assert_eq!(s.get(&id).unwrap().0.name, "Second");
        assert!(d.0.join("templates").join(format!("{id}.toml")).is_file());
    }

    #[test]
    fn rename_and_move_reject_bad_paths_and_names() {
        let d = TempDir::new("badpath");
        let s = d.store();
        for bad in ["../x", "/x", "C:/x", "Examples/"] {
            assert_eq!(
                s.move_template("default", bad),
                Err(TempDir::invalid()),
                "{bad}"
            );
            assert_eq!(s.rename_group(bad, "n"), Err(TempDir::invalid()), "{bad}");
        }
        for bad in ["", "..", "a/b", "C:x", "con"] {
            assert_eq!(
                s.rename_group("Examples", bad),
                Err(TempDir::invalid()),
                "{bad}"
            );
        }
    }

    #[test]
    fn concurrent_saves_get_unique_ids() {
        let d = TempDir::new("conc");
        let s = d.store();
        let ids: Vec<String> = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..8)
                .map(|_| sc.spawn(|| s.save(None, "", &TempDir::sample("Same")).unwrap()))
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let set: HashSet<_> = ids.iter().collect();
        assert_eq!(set.len(), 8);
    }

    #[test]
    fn root_with_only_empty_groups_is_not_seeded() {
        let d = TempDir::new("emptyg");
        fs::create_dir_all(d.0.join("templates").join("empty")).unwrap();
        let s = d.store();
        assert_eq!(s.tree().len(), 1);
        assert!(s.get("default").is_err());
    }

    #[test]
    fn seed_falls_back_to_first_llamacpp_model() {
        let d = TempDir::new("fallback");
        let rules = TemplateRules {
            runtimes: vec!["llamacpp".into(), "candle".into()],
            tools: vec![],
            repositories: vec![RepositoryRules {
                name: "hive".into(),
                kind: RepositoryKind::Pragmatic,
                models: Some(vec![
                    RuleModel::of("only-candle", &["candle"]),
                    RuleModel::of("alpha", &["candle", "llamacpp"]),
                    RuleModel::of("beta", &["llamacpp"]),
                ]),
            }],
            writing_tools: vec![],
        };
        let s = TemplateStore::open(&d.0.join("templates"), rules).unwrap();
        assert_eq!(s.get("default").unwrap().0.model, "alpha");
    }

    #[test]
    fn sort_ties_break_on_path() {
        let node = |id: &str, path: &str| TemplateNode::Template {
            id: id.into(),
            path: path.into(),
            name: "Same".into(),
            runtime: String::new(),
            model: String::new(),
            error: None,
        };
        let mut nodes = vec![node("a", "z"), node("a", "y")];
        nodes.sort_by_key(TemplateNode::sort_key);
        assert!(matches!(&nodes[0], TemplateNode::Template { path, .. } if path == "y"));
    }

    #[test]
    fn save_with_differently_cased_id_keeps_file() {
        let d = TempDir::new("casesave");
        d.hand_file("g", "X");
        let s = d.store();
        let mut t = TempDir::sample("X");
        t.system_prompt = "new content".into();
        assert_eq!(s.save(Some("x"), "g", &t).unwrap(), "X");
        let file = d.0.join("templates").join("g").join("X.toml");
        assert!(file.is_file());
        assert_eq!(s.get("X").unwrap().0.system_prompt, "new content");
        // a cross-group save also keeps the on-disk casing
        assert_eq!(s.save(Some("x"), "h", &t).unwrap(), "X");
        assert_eq!(s.get("X").unwrap().1, "h");
    }

    #[cfg(windows)]
    #[test]
    fn failed_old_removal_rolls_back_new_copy() {
        let d = TempDir::new("rollback");
        d.hand_file("a", "x");
        let s = d.store();
        let old = d.0.join("templates").join("a").join("x.toml");
        // Hold the old file open without FILE_SHARE_DELETE so removing it fails.
        use std::os::windows::fs::OpenOptionsExt;
        let guard = fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(&old)
            .unwrap();
        let res = s.save(Some("x"), "b", &TempDir::sample("x"));
        drop(guard);
        assert!(res.is_err());
        assert!(!d.0.join("templates").join("b").join("x.toml").exists());
        assert!(old.is_file());
    }

    #[test]
    fn non_ascii_ids_tree_and_locate_agree() {
        let d = TempDir::new("unicode");
        d.hand_file("a", "\u{c9}x");
        d.hand_file("b", "\u{e9}x");
        let s = d.store();
        // ASCII folding: the two are distinct ids everywhere, so nothing is flagged.
        let mut errors = 0;
        for g in s.tree() {
            if let TemplateNode::Group { children, .. } = g {
                for c in children {
                    if matches!(c, TemplateNode::Template { error: Some(_), .. }) {
                        errors += 1;
                    }
                }
            }
        }
        assert_eq!(errors, 0);
        assert_eq!(s.get("\u{c9}x").unwrap().1, "a");
        assert_eq!(s.get("\u{e9}x").unwrap().1, "b");
    }

    #[test]
    fn duplicate_flag_keeps_parse_error() {
        let d = TempDir::new("duperr");
        d.hand_file("a", "x");
        let bad = d.0.join("templates").join("b");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("x.toml"), "not = [valid").unwrap();
        let s = d.store();
        let mut seen = 0;
        for g in s.tree() {
            if let TemplateNode::Group { path, children, .. } = g {
                if let TemplateNode::Template { error: Some(e), .. } = &children[0] {
                    assert!(e.ends_with("duplicate template id: x"));
                    if path == "b" {
                        assert!(e.contains("; duplicate"), "{e}");
                    }
                    seen += 1;
                }
            }
        }
        assert_eq!(seen, 2);
    }

    #[cfg(windows)]
    #[test]
    fn save_into_non_ascii_case_variant_group_keeps_file() {
        let d = TempDir::new("nonascii");
        d.hand_file("\u{c9}", "a");
        let s = d.store();
        let mut t = TempDir::sample("a");
        t.system_prompt = "new content".into();
        s.save(Some("a"), "\u{e9}", &t).unwrap();
        assert_eq!(s.get("a").unwrap().0.system_prompt, "new content");
    }

    #[test]
    fn save_drops_name_on_a_non_named_workspace() {
        let d = TempDir::new("wsnormalize");
        let s = d.store();
        let mut t = TempDir::sample("Shared");
        t.workspace = WorkspaceConfig {
            mode: crate::agents::template::WorkspaceMode::Shared,
            name: Some("research".into()),
        };
        let id = s.save(None, "grp", &t).unwrap();
        let (saved, _) = s.get(&id).unwrap();
        assert_eq!(
            saved.workspace.mode,
            crate::agents::template::WorkspaceMode::Shared
        );
        assert_eq!(saved.workspace.name, None);
        let file = fs::read_to_string(d.0.join("templates").join("grp").join(format!("{id}.toml")))
            .unwrap();
        assert!(!file.contains("research"), "{file}");
    }
}
