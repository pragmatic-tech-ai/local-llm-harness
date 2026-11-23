use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};

use super::tool::{Tool, ToolContext, ToolOutcome, ToolSource, ToolSpec};

/// Hard cap on bytes returned by `read_file`.
const MAX_READ_BYTES: u64 = 65536;
/// Hard cap on entries returned by `list_dir`.
const MAX_LIST_ENTRIES: usize = 500;

/// A canonical directory that file tools are confined to.
#[derive(Debug, Clone)]
pub struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// Canonicalises the root; fails if it does not exist.
    pub fn new(root: &Path) -> std::io::Result<Self> {
        Ok(Self {
            root: fs::canonicalize(root)?,
        })
    }

    /// Resolves a sandbox-relative path to a canonical path inside the sandbox.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, String> {
        let rel_path = Path::new(rel);
        let anchored = rel_path
            .components()
            .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir));
        if rel_path.is_absolute() || anchored {
            return Err("path must be relative to the sandbox".to_string());
        }
        // Normalise lexically first so `..` escapes are rejected identically whether or
        // not the target exists (no existence probing outside the sandbox).
        let mut stack: Vec<&std::ffi::OsStr> = Vec::new();
        for component in rel_path.components() {
            match component {
                Component::Normal(part) => stack.push(part),
                Component::ParentDir => {
                    if stack.pop().is_none() {
                        return Err("path escapes the sandbox".to_string());
                    }
                }
                _ => {}
            }
        }
        let mut joined = self.root.clone();
        joined.extend(stack);
        // Canonicalising still catches symlinks and junctions that point outside.
        let resolved = fs::canonicalize(joined).map_err(|_| format!("no such path: {rel}"))?;
        if !resolved.starts_with(&self.root) {
            return Err("path escapes the sandbox".to_string());
        }
        Ok(resolved)
    }

    /// Sandbox-relative display form with forward slashes ("." for the root).
    /// Never exposes the absolute host path.
    pub fn display(&self, resolved: &Path) -> String {
        let rel = resolved.strip_prefix(&self.root).unwrap_or(Path::new(""));
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if parts.is_empty() {
            ".".to_string()
        } else {
            parts.join("/")
        }
    }
}

/// Civil-time formatting without external crates.
pub struct CivilTime;

impl CivilTime {
    /// Formats Unix seconds as `YYYY-MM-DDTHH:MM:SSZ` (days-to-civil algorithm).
    pub fn utc_iso8601(unix_secs: u64) -> String {
        let days = (unix_secs / 86400) as i64;
        let rem = unix_secs % 86400;
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    }
}

/// `get_time`: current UTC time.
pub struct GetTimeTool;

#[async_trait]
impl Tool for GetTimeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "get_time".into(),
            description: "Get the current date and time in UTC.".into(),
            parameters: json!({"type": "object", "properties": {}}),
            side_effects: false,
        }
    }

    async fn call(&self, _args: Value, _ctx: &ToolContext) -> ToolOutcome {
        let secs = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs(),
            Err(_) => return ToolOutcome::Err("system clock is before the Unix epoch".to_string()),
        };
        ToolOutcome::Ok(json!({"utc": CivilTime::utc_iso8601(secs), "unix": secs}))
    }
}

/// `list_dir`: lists a directory inside the sandbox.
pub struct ListDirTool {
    sandbox: Sandbox,
}

impl ListDirTool {
    fn list(sandbox: &Sandbox, rel: &str) -> ToolOutcome {
        let dir = match sandbox.resolve(rel) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::Err(e),
        };
        if !dir.is_dir() {
            return ToolOutcome::Err("path is not a directory".to_string());
        }
        let reader = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) => return ToolOutcome::Err(format!("cannot read directory: {e}")),
        };
        // The directory is read in full before sorting and capping on purpose: that makes
        // the 500-entry window deterministic (first 500 by name), not OS enumeration order.
        // Entries that cannot be enumerated at all (no name available) are counted in
        // `errors`; entries whose metadata is unreadable are listed with `size: null`.
        let mut errors = 0usize;
        let mut entries: Vec<(String, &'static str, Option<u64>)> = reader
            .filter_map(|e| match e {
                Ok(entry) => Some(entry),
                Err(_) => {
                    errors += 1;
                    None
                }
            })
            .map(|e| {
                let kind = match e.file_type() {
                    Ok(t) if t.is_dir() => "dir",
                    Ok(t) if t.is_file() => "file",
                    _ => "other",
                };
                let size = e.metadata().ok().map(|m| m.len());
                (e.file_name().to_string_lossy().into_owned(), kind, size)
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let truncated = entries.len() > MAX_LIST_ENTRIES;
        entries.truncate(MAX_LIST_ENTRIES);
        let listed: Vec<Value> = entries
            .into_iter()
            .map(|(name, kind, size)| json!({"name": name, "kind": kind, "size": size}))
            .collect();
        ToolOutcome::Ok(json!({
            "path": sandbox.display(&dir),
            "entries": listed,
            "truncated": truncated,
            "errors": errors,
        }))
    }
}

#[async_trait]
impl Tool for ListDirTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_dir".into(),
            description: "List the entries of a directory inside the sandbox.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Directory relative to the sandbox; defaults to '.'"}
                }
            }),
            side_effects: false,
        }
    }

    async fn call(&self, args: Value, _ctx: &ToolContext) -> ToolOutcome {
        let rel = match args.get("path") {
            None | Some(Value::Null) => ".".to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(_) => return ToolOutcome::Err("path must be a string".to_string()),
        };
        let sandbox = self.sandbox.clone();
        tokio::task::spawn_blocking(move || Self::list(&sandbox, &rel))
            .await
            .unwrap_or_else(|e| ToolOutcome::Err(format!("tool task failed: {e}")))
    }
}

/// `read_file`: reads a UTF-8 text file inside the sandbox.
pub struct ReadFileTool {
    sandbox: Sandbox,
}

impl ReadFileTool {
    fn read(sandbox: &Sandbox, rel: &str, cap: u64) -> ToolOutcome {
        let path = match sandbox.resolve(rel) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::Err(e),
        };
        if path.is_dir() {
            return ToolOutcome::Err("path is a directory, not a file".to_string());
        }
        let file = match fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => return ToolOutcome::Err(format!("cannot read file: {e}")),
        };
        let mut buf = Vec::new();
        if let Err(e) = file.take(cap + 1).read_to_end(&mut buf) {
            return ToolOutcome::Err(format!("cannot read file: {e}"));
        }
        let truncated = buf.len() as u64 > cap;
        buf.truncate(cap as usize);
        let content = match std::str::from_utf8(&buf) {
            Ok(s) => s.to_string(),
            // An incomplete sequence at the cut point backs off to the last full character.
            Err(e) if truncated && e.error_len().is_none() => {
                String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned()
            }
            Err(_) => return ToolOutcome::Err("file is not valid UTF-8 text".to_string()),
        };
        ToolOutcome::Ok(json!({
            "path": sandbox.display(&path),
            "bytes": content.len(),
            "content": content,
            "truncated": truncated,
        }))
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Read a UTF-8 text file inside the sandbox (up to 65536 bytes).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File relative to the sandbox"},
                    "max_bytes": {"type": "integer", "description": "Maximum bytes to read (capped at 65536)"}
                },
                "required": ["path"]
            }),
            side_effects: false,
        }
    }

    async fn call(&self, args: Value, _ctx: &ToolContext) -> ToolOutcome {
        let Some(rel) = args.get("path").and_then(Value::as_str) else {
            return ToolOutcome::Err("path must be a string".to_string());
        };
        let cap = match args.get("max_bytes") {
            None | Some(Value::Null) => MAX_READ_BYTES,
            Some(v) => match v.as_u64() {
                Some(n) => n.min(MAX_READ_BYTES),
                None => {
                    return ToolOutcome::Err("max_bytes must be a non-negative integer".to_string())
                }
            },
        };
        let sandbox = self.sandbox.clone();
        let rel = rel.to_string();
        tokio::task::spawn_blocking(move || Self::read(&sandbox, &rel, cap))
            .await
            .unwrap_or_else(|e| ToolOutcome::Err(format!("tool task failed: {e}")))
    }
}

/// Built-in tools: `get_time` always; `list_dir`/`read_file` only with a sandbox.
pub struct BuiltinSource {
    sandbox: Option<Sandbox>,
}

impl BuiltinSource {
    pub fn new(sandbox: Option<Sandbox>) -> Self {
        Self { sandbox }
    }
}

impl ToolSource for BuiltinSource {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools: Vec<Arc<dyn Tool>> = vec![Arc::new(GetTimeTool)];
        if let Some(sandbox) = &self.sandbox {
            tools.push(Arc::new(ListDirTool {
                sandbox: sandbox.clone(),
            }));
            tools.push(Arc::new(ReadFileTool {
                sandbox: sandbox.clone(),
            }));
        }
        tools
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A unique temp directory removed on drop.
    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir()
                .join(format!("llmh-builtin-{}-{nanos}-{n}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn sandbox(&self) -> Sandbox {
            Sandbox::new(&self.path).unwrap()
        }

        fn tool(&self, name: &str) -> Arc<dyn Tool> {
            BuiltinSource::new(Some(self.sandbox()))
                .tools()
                .into_iter()
                .find(|t| t.spec().name == name)
                .unwrap()
        }

        async fn run(&self, name: &str, args: Value) -> ToolOutcome {
            let ctx = ToolContext {
                call_id: "c1".into(),
            };
            self.tool(name).call(args, &ctx).await
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn civil_time_formats_known_instants() {
        assert_eq!(CivilTime::utc_iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(CivilTime::utc_iso8601(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(
            CivilTime::utc_iso8601(1_700_000_000),
            "2023-11-14T22:13:20Z"
        );
    }

    #[tokio::test]
    async fn get_time_returns_utc_and_unix() {
        let dir = TestDir::new();
        let ToolOutcome::Ok(v) = dir.run("get_time", json!({})).await else {
            panic!("expected Ok");
        };
        let unix = v["unix"].as_u64().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(now.abs_diff(unix) <= 5, "unix {unix} vs now {now}");
        let utc = v["utc"].as_str().unwrap();
        assert_eq!(utc, CivilTime::utc_iso8601(unix));
        assert_eq!(utc.len(), 20);
        for (i, c) in utc.bytes().enumerate() {
            match i {
                4 | 7 => assert_eq!(c, b'-'),
                10 => assert_eq!(c, b'T'),
                13 | 16 => assert_eq!(c, b':'),
                19 => assert_eq!(c, b'Z'),
                _ => assert!(c.is_ascii_digit(), "{utc}"),
            }
        }
        let num = |r: std::ops::Range<usize>| utc[r].parse::<u32>().unwrap();
        assert!((1..=12).contains(&num(5..7)));
        assert!((1..=31).contains(&num(8..10)));
        assert!(num(11..13) < 24 && num(14..16) < 60 && num(17..19) < 60);
    }

    #[tokio::test]
    async fn list_dir_lists_sorted_entries() {
        let dir = TestDir::new();
        fs::write(dir.path.join("b.txt"), "bb").unwrap();
        fs::write(dir.path.join("a.txt"), "a").unwrap();
        fs::create_dir(dir.path.join("sub")).unwrap();
        let ToolOutcome::Ok(v) = dir.run("list_dir", json!({})).await else {
            panic!("expected Ok");
        };
        assert_eq!(v["path"], ".");
        assert_eq!(v["truncated"], false);
        let entries = v["entries"].as_array().unwrap();
        let names: Vec<&str> = entries
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["a.txt", "b.txt", "sub"]);
        let kinds: Vec<&str> = entries
            .iter()
            .map(|e| e["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["file", "file", "dir"]);
        assert_eq!(entries[1]["size"], 2);
    }

    #[tokio::test]
    async fn list_dir_on_file_errors() {
        let dir = TestDir::new();
        fs::write(dir.path.join("a.txt"), "a").unwrap();
        let out = dir.run("list_dir", json!({"path": "a.txt"})).await;
        assert_eq!(out, ToolOutcome::Err("path is not a directory".into()));
    }

    #[tokio::test]
    async fn read_file_returns_text() {
        let dir = TestDir::new();
        fs::create_dir(dir.path.join("sub")).unwrap();
        fs::write(dir.path.join("sub").join("n.txt"), "hello").unwrap();
        let out = dir.run("read_file", json!({"path": "sub/n.txt"})).await;
        assert_eq!(
            out,
            ToolOutcome::Ok(
                json!({"path": "sub/n.txt", "content": "hello", "bytes": 5, "truncated": false})
            )
        );
    }

    #[tokio::test]
    async fn read_file_on_directory_errors() {
        let dir = TestDir::new();
        fs::create_dir(dir.path.join("sub")).unwrap();
        let out = dir.run("read_file", json!({"path": "sub"})).await;
        assert_eq!(
            out,
            ToolOutcome::Err("path is a directory, not a file".into())
        );
    }

    #[tokio::test]
    async fn read_file_truncates_on_char_boundary() {
        let dir = TestDir::new();
        fs::write(dir.path.join("u.txt"), "a\u{e9}").unwrap();
        let out = dir
            .run("read_file", json!({"path": "u.txt", "max_bytes": 2}))
            .await;
        assert_eq!(
            out,
            ToolOutcome::Ok(
                json!({"path": "u.txt", "content": "a", "bytes": 1, "truncated": true})
            )
        );
    }

    #[tokio::test]
    async fn read_file_rejects_non_utf8() {
        let dir = TestDir::new();
        fs::write(dir.path.join("bin"), [0xffu8, 0xfe]).unwrap();
        let out = dir.run("read_file", json!({"path": "bin"})).await;
        assert_eq!(out, ToolOutcome::Err("file is not valid UTF-8 text".into()));
    }

    #[test]
    fn resolve_rejects_parent_escape() {
        let outer = TestDir::new();
        fs::create_dir(outer.path.join("box")).unwrap();
        fs::write(outer.path.join("x"), "x").unwrap();
        let sandbox = Sandbox::new(&outer.path.join("box")).unwrap();
        assert_eq!(
            sandbox.resolve("../x"),
            Err("path escapes the sandbox".to_string())
        );
    }

    #[test]
    fn resolve_rejects_absolute() {
        let dir = TestDir::new();
        assert_eq!(
            dir.sandbox().resolve("C:\\Windows"),
            Err("path must be relative to the sandbox".to_string())
        );
        assert_eq!(
            dir.sandbox().resolve("/etc"),
            Err("path must be relative to the sandbox".to_string())
        );
    }

    #[test]
    fn resolve_rejects_nonexistent_parent_escape() {
        let dir = TestDir::new();
        assert_eq!(
            dir.sandbox().resolve("../nope"),
            Err("path escapes the sandbox".to_string())
        );
        assert_eq!(
            dir.sandbox().resolve("a/../../nope"),
            Err("path escapes the sandbox".to_string())
        );
    }

    #[test]
    fn resolve_allows_inner_dotdot() {
        let dir = TestDir::new();
        fs::create_dir(dir.path.join("sub")).unwrap();
        let resolved = dir.sandbox().resolve("sub/../sub/.").unwrap();
        assert_eq!(dir.sandbox().display(&resolved), "sub");
    }

    #[test]
    fn resolve_rejects_sibling_prefix() {
        let outer = TestDir::new();
        fs::create_dir(outer.path.join("sandbox2")).unwrap();
        fs::create_dir(outer.path.join("sandbox2_x")).unwrap();
        fs::write(outer.path.join("sandbox2_x").join("f"), "f").unwrap();
        let sandbox = Sandbox::new(&outer.path.join("sandbox2")).unwrap();
        assert_eq!(
            sandbox.resolve("../sandbox2_x/f"),
            Err("path escapes the sandbox".to_string())
        );
    }

    #[cfg(windows)]
    #[test]
    fn resolve_rejects_windows_anchored_paths() {
        let dir = TestDir::new();
        for rel in ["C:foo", r"\\server\share", r"\x"] {
            assert_eq!(
                dir.sandbox().resolve(rel),
                Err("path must be relative to the sandbox".to_string()),
                "{rel}"
            );
        }
    }

    #[tokio::test]
    async fn list_dir_caps_at_500_entries() {
        let dir = TestDir::new();
        for i in 0..501 {
            fs::write(dir.path.join(format!("f{i:04}")), "").unwrap();
        }
        let ToolOutcome::Ok(v) = dir.run("list_dir", json!({})).await else {
            panic!("expected Ok");
        };
        assert_eq!(v["entries"].as_array().unwrap().len(), 500);
        assert_eq!(v["truncated"], true);
        assert_eq!(v["errors"], 0);
        assert_eq!(v["entries"][499]["name"], "f0499");
    }

    #[tokio::test]
    async fn list_dir_rejects_non_string_path() {
        let dir = TestDir::new();
        let out = dir.run("list_dir", json!({"path": 5})).await;
        assert_eq!(out, ToolOutcome::Err("path must be a string".into()));
    }

    #[tokio::test]
    async fn read_file_clamps_to_65536() {
        let dir = TestDir::new();
        fs::write(dir.path.join("big"), vec![b'x'; 70000]).unwrap();
        for args in [
            json!({"path": "big"}),
            json!({"path": "big", "max_bytes": 1_000_000}),
        ] {
            let ToolOutcome::Ok(v) = dir.run("read_file", args).await else {
                panic!("expected Ok");
            };
            assert_eq!(v["bytes"], 65536);
            assert_eq!(v["truncated"], true);
        }
    }

    #[tokio::test]
    async fn read_file_max_bytes_zero_returns_empty_truncated() {
        let dir = TestDir::new();
        fs::write(dir.path.join("a"), "abc").unwrap();
        let out = dir
            .run("read_file", json!({"path": "a", "max_bytes": 0}))
            .await;
        assert_eq!(
            out,
            ToolOutcome::Ok(json!({"path": "a", "content": "", "bytes": 0, "truncated": true}))
        );
    }

    #[tokio::test]
    async fn read_file_rejects_invalid_max_bytes() {
        let dir = TestDir::new();
        fs::write(dir.path.join("a"), "abc").unwrap();
        for bad in [json!(-1), json!("5"), json!(1.5)] {
            let out = dir
                .run("read_file", json!({"path": "a", "max_bytes": bad}))
                .await;
            assert_eq!(
                out,
                ToolOutcome::Err("max_bytes must be a non-negative integer".into())
            );
        }
    }

    #[tokio::test]
    async fn outputs_never_contain_absolute_sandbox_path() {
        let dir = TestDir::new();
        fs::create_dir(dir.path.join("sub")).unwrap();
        fs::write(dir.path.join("sub").join("n.txt"), "hi").unwrap();
        let root = dir.path.to_string_lossy().into_owned();
        let canonical = fs::canonicalize(&dir.path)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let outputs = [
            dir.run("list_dir", json!({"path": "sub"})).await,
            dir.run("list_dir", json!({})).await,
            dir.run("read_file", json!({"path": "sub/n.txt"})).await,
        ];
        for out in outputs {
            let text = out.result_value().to_string();
            assert!(!text.contains(&root), "{text}");
            assert!(!text.contains(&canonical), "{text}");
        }
    }

    #[test]
    fn resolve_reports_missing_path() {
        let dir = TestDir::new();
        assert_eq!(
            dir.sandbox().resolve("nope"),
            Err("no such path: nope".to_string())
        );
    }

    #[cfg(windows)]
    #[test]
    fn resolve_rejects_junction_escape() {
        let outer = TestDir::new();
        let outside = outer.path.join("outside");
        let boxed = outer.path.join("box");
        fs::create_dir(&outside).unwrap();
        fs::create_dir(&boxed).unwrap();
        let link = boxed.join("link");
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(made.status.success(), "mklink failed");
        let result = Sandbox::new(&boxed).unwrap().resolve("link");
        // Remove the junction itself so cleanup never follows it.
        let _ = fs::remove_dir(&link);
        assert_eq!(result, Err("path escapes the sandbox".to_string()));
    }

    #[cfg(windows)]
    #[test]
    fn resolve_rejects_sibling_prefix_junction_escape() {
        let outer = TestDir::new();
        let boxed = outer.path.join("box");
        let sibling = outer.path.join("box2");
        fs::create_dir(&boxed).unwrap();
        fs::create_dir(&sibling).unwrap();
        fs::write(sibling.join("f"), "f").unwrap();
        let link = boxed.join("link");
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&sibling)
            .output()
            .unwrap();
        assert!(made.status.success(), "mklink failed");
        let result = Sandbox::new(&boxed).unwrap().resolve("link/f");
        // Remove the junction itself so cleanup never follows it.
        let _ = fs::remove_dir(&link);
        assert_eq!(result, Err("path escapes the sandbox".to_string()));
    }

    #[test]
    fn source_without_sandbox_has_only_get_time() {
        let names: Vec<String> = BuiltinSource::new(None)
            .tools()
            .iter()
            .map(|t| t.spec().name)
            .collect();
        assert_eq!(names, ["get_time"]);
        let dir = TestDir::new();
        let names: Vec<String> = BuiltinSource::new(Some(dir.sandbox()))
            .tools()
            .iter()
            .map(|t| t.spec().name)
            .collect();
        assert_eq!(names, ["get_time", "list_dir", "read_file"]);
    }
}
