//! File-backed store of agent templates: a directory tree of TOML files, directories are groups.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

use super::template::{AgentTemplate, TemplateRules};

const EXT: &str = "toml";
const TMP_EXT: &str = "tmp";
const SEED_GROUP: &str = "Examples";
const SEED_NAME: &str = "Default";
const SEED_ID: &str = "default";
const SEED_RUNTIME: &str = "llamacpp";
const SEED_MODEL_HINT: &str = "gemma-4";
const SEED_N_CTX: u32 = 8192;
const SEED_STEP_BUDGET: usize = 8;
const FALLBACK_SLUG: &str = "agent";
const ERR_INVALID_PATH: &str = "invalid path";
const ERR_NOT_EMPTY: &str = "group is not empty";
const ERR_TEMPLATE_NOT_FOUND: &str = "template not found";
const ERR_GROUP_NOT_FOUND: &str = "group not found";
const ERR_GROUP_EXISTS: &str = "group already exists";
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
    fn sort_key(&self) -> (u8, String) {
        match self {
            TemplateNode::Group { name, .. } => (0, name.to_lowercase()),
            TemplateNode::Template { name, .. } => (1, name.to_lowercase()),
        }
    }
}

pub struct TemplateStore {
    root: PathBuf,
    rules: TemplateRules,
    write_lock: Mutex<()>,
}

impl TemplateStore {
    /// Opens (creating if needed) the store at `root`; seeds `Examples/Default.toml` when the
    /// root is missing or empty.
    pub fn open(root: &Path, rules: TemplateRules) -> io::Result<Self> {
        let fresh = !root.exists() || fs::read_dir(root)?.next().is_none();
        fs::create_dir_all(root)?;
        let store = Self {
            root: root.to_path_buf(),
            rules,
            write_lock: Mutex::new(()),
        };
        if fresh {
            let seed = store.seed_template();
            let dir = root.join(SEED_GROUP);
            fs::create_dir_all(&dir)?;
            Self::write_atomic(&dir, SEED_ID, &seed)?;
        }
        Ok(store)
    }

    fn seed_template(&self) -> AgentTemplate {
        let capable = |m: &&(String, Vec<String>)| m.1.iter().any(|r| r == SEED_RUNTIME);
        let model = self
            .rules
            .models
            .iter()
            .filter(capable)
            .find(|m| m.0.contains(SEED_MODEL_HINT))
            .or_else(|| self.rules.models.iter().find(capable))
            .map(|m| m.0.clone())
            .unwrap_or_default();
        AgentTemplate {
            name: SEED_NAME.to_string(),
            system_prompt: String::new(),
            tools: self.rules.tools.clone(),
            step_budget: SEED_STEP_BUDGET,
            runtime: SEED_RUNTIME.to_string(),
            model,
            n_ctx: SEED_N_CTX,
            enable_thinking: false,
            sampling: None,
            host: None,
        }
    }

    pub fn tree(&self) -> Vec<TemplateNode> {
        Self::read_level(&self.root, "")
    }

    fn read_level(dir: &Path, rel: &str) -> Vec<TemplateNode> {
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
                    children: Self::read_level(&p, &child_rel),
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
                        id,
                        path: rel.to_string(),
                        name: t.name,
                        runtime: t.runtime,
                        model: t.model,
                        error: None,
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

    pub fn save(
        &self,
        id: Option<&str>,
        path: &str,
        t: &AgentTemplate,
    ) -> Result<String, TemplateError> {
        self.rules.validate(t).map_err(TemplateError)?;
        let dir = self.dir_for(path)?;
        let _g = self.lock()?;
        let (id, old) = match id {
            Some(id) => {
                let (old_file, _) = self.locate(id)?;
                (id.to_string(), Some(old_file))
            }
            None => (self.unique_id(&t.name), None),
        };
        fs::create_dir_all(&dir)?;
        Self::write_atomic(&dir, &id, t)?;
        if let Some(old) = old {
            if old != dir.join(format!("{id}.{EXT}")) {
                fs::remove_file(old)?;
            }
        }
        Ok(id)
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
        let target = dir.with_file_name(new_name);
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
        fs::rename(old, dir.join(format!("{id}.{EXT}")))?;
        Ok(())
    }

    // ---- helpers -------------------------------------------------------------------------

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ()>, TemplateError> {
        self.write_lock
            .lock()
            .map_err(|_| TemplateError::of(ERR_LOCK))
    }

    fn valid_segment(s: &str) -> bool {
        !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\', ':']) && !s.ends_with('.')
    }

    /// Resolves a `/`-separated relative group path to a directory under the root.
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
    fn locate(&self, id: &str) -> Result<(PathBuf, String), TemplateError> {
        let not_found = || TemplateError(format!("{ERR_TEMPLATE_NOT_FOUND}: {id}"));
        if id.is_empty() || Self::slugify(id) != id {
            return Err(not_found());
        }
        Self::find(&self.root, "", &format!("{id}.{EXT}")).ok_or_else(not_found)
    }

    fn find(dir: &Path, rel: &str, file_name: &str) -> Option<(PathBuf, String)> {
        let direct = dir.join(file_name);
        if direct.is_file() {
            return Some((direct, rel.to_string()));
        }
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                let child = if rel.is_empty() {
                    name
                } else {
                    format!("{rel}/{name}")
                };
                if let Some(found) = Self::find(&p, &child, file_name) {
                    return Some(found);
                }
            }
        }
        None
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
                    out.insert(stem.to_string());
                }
            }
        }
    }

    fn unique_id(&self, name: &str) -> String {
        let mut taken = HashSet::new();
        Self::collect_ids(&self.root, &mut taken);
        let base = Self::slugify(name);
        let base = if base.is_empty() {
            FALLBACK_SLUG.to_string()
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
        let tmp = dir.join(format!(".{id}.{EXT}.{TMP_EXT}"));
        fs::write(&tmp, body)?;
        fs::rename(&tmp, dir.join(format!("{id}.{EXT}")))
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
                host: Some("laptop".into()),
            }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
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
                children: vec![TemplateNode::Template {
                    id: "default".into(),
                    path: "Examples".into(),
                    name: "Default".into(),
                    runtime: "llamacpp".into(),
                    model: "gemma-4-e4b".into(),
                    error: None,
                }],
            }]
        );
        let (t, _) = s.get("default").unwrap();
        assert_eq!(t.tools, TemplateRules::for_tests().tools);
        assert_eq!(t.n_ctx, 8192);
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
}
