use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};

use super::tool::{Tool, ToolContext, ToolOutcome, ToolSource, ToolSpec};
use super::workspace::Workspace;

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
    pub const NOT_RELATIVE: &'static str = "path must be relative to the sandbox";
    pub const ESCAPES: &'static str = "path escapes the sandbox";
    const NO_SUCH_PATH: &'static str = "no such path: ";
    const CURRENT: &'static str = ".";
    const SEPARATOR: &'static str = "/";

    /// Canonicalises the root; fails if it does not exist.
    pub fn new(root: &Path) -> std::io::Result<Self> {
        Ok(Self {
            root: fs::canonicalize(root)?,
        })
    }

    /// The canonical sandbox root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a sandbox-relative path to a canonical path inside the sandbox.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, String> {
        self.resolve_in(&self.root, rel, rel)
    }

    /// Resolves `rel` against `base` (a canonical folder inside the sandbox) to a canonical,
    /// existing path inside the sandbox. `shown` is the path named in a missing-path error.
    pub fn resolve_in(&self, base: &Path, rel: &str, shown: &str) -> Result<PathBuf, String> {
        let mut joined = base.to_path_buf();
        joined.extend(Self::normalise(Path::new(rel))?.components());
        // Canonicalising still catches symlinks and junctions that point outside.
        let resolved = fs::canonicalize(joined).map_err(|_| Self::no_such_path(shown))?;
        if !resolved.starts_with(&self.root) {
            return Err(Self::ESCAPES.to_string());
        }
        Ok(resolved)
    }

    /// Lexically normalises a relative path to its plain (`Normal`) components. Anchored or
    /// absolute paths, and `..` that climbs above the start, are refused.
    pub fn normalise(rel: &Path) -> Result<PathBuf, String> {
        let anchored = rel
            .components()
            .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir));
        if rel.is_absolute() || anchored {
            return Err(Self::NOT_RELATIVE.to_string());
        }
        // Normalise lexically first so `..` escapes are rejected identically whether or
        // not the target exists (no existence probing outside the base).
        let mut stack: Vec<&std::ffi::OsStr> = Vec::new();
        for component in rel.components() {
            match component {
                Component::Normal(part) => stack.push(part),
                Component::ParentDir => {
                    if stack.pop().is_none() {
                        return Err(Self::ESCAPES.to_string());
                    }
                }
                _ => {}
            }
        }
        Ok(stack.into_iter().collect())
    }

    pub fn no_such_path(shown: &str) -> String {
        format!("{}{shown}", Self::NO_SUCH_PATH)
    }

    /// Sandbox-relative display form with forward slashes ("." for the root).
    /// Never exposes the absolute host path.
    pub fn display(&self, resolved: &Path) -> String {
        let rel = resolved.strip_prefix(&self.root).unwrap_or(Path::new(""));
        Self::slashed(rel).unwrap_or_else(|| Self::CURRENT.to_string())
    }

    /// `rel`'s components joined with forward slashes; None when it has none.
    pub fn slashed(rel: &Path) -> Option<String> {
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        (!parts.is_empty()).then(|| parts.join(Self::SEPARATOR))
    }
}

/// Civil-time formatting without external crates.
pub struct CivilTime;

impl CivilTime {
    const SECS_PER_DAY: i64 = 86_400;

    /// Formats Unix seconds as `YYYY-MM-DDTHH:MM:SSZ` (days-to-civil algorithm).
    pub fn utc_iso8601(unix_secs: u64) -> String {
        format!("{}Z", Self::civil(unix_secs as i64))
    }

    /// Formats Unix seconds shifted by `offset_minutes` east of UTC as
    /// `YYYY-MM-DDTHH:MM:SS+HH:MM` (the wall-clock time in that zone).
    pub fn local_iso8601(unix_secs: u64, offset_minutes: i32) -> String {
        let local = unix_secs as i64 + i64::from(offset_minutes) * 60;
        let sign = if offset_minutes < 0 { '-' } else { '+' };
        let abs = offset_minutes.unsigned_abs();
        format!(
            "{}{sign}{:02}:{:02}",
            Self::civil(local),
            abs / 60,
            abs % 60
        )
    }

    /// `YYYY-MM-DDTHH:MM:SS` for signed Unix seconds.
    fn civil(secs: i64) -> String {
        let days = secs.div_euclid(Self::SECS_PER_DAY);
        let rem = secs.rem_euclid(Self::SECS_PER_DAY);
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
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    }
}

/// The machine's current UTC offset, daylight saving included.
pub struct LocalOffset;

impl LocalOffset {
    #[cfg(windows)]
    const TIME_ZONE_ID_STANDARD: u32 = 1;
    #[cfg(windows)]
    const TIME_ZONE_ID_DAYLIGHT: u32 = 2;

    /// Minutes east of UTC; 0 when the zone cannot be read.
    #[cfg(windows)]
    pub fn current_minutes() -> i32 {
        use windows_sys::Win32::System::Time::{
            GetDynamicTimeZoneInformation, DYNAMIC_TIME_ZONE_INFORMATION, TIME_ZONE_ID_INVALID,
        };
        // SAFETY: DYNAMIC_TIME_ZONE_INFORMATION is plain data (integers and u16 arrays), so the
        // all-zero value is valid; the call only writes into the struct we own.
        let mut info: DYNAMIC_TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
        let id = unsafe { GetDynamicTimeZoneInformation(&mut info) };
        // UTC = local + bias, in minutes; the extra bias depends on which period is in effect.
        let extra = match id {
            TIME_ZONE_ID_INVALID => return 0,
            Self::TIME_ZONE_ID_STANDARD => info.StandardBias,
            Self::TIME_ZONE_ID_DAYLIGHT => info.DaylightBias,
            _ => 0,
        };
        -(info.Bias + extra)
    }

    /// Minutes east of UTC; this build only knows UTC.
    #[cfg(not(windows))]
    pub fn current_minutes() -> i32 {
        0
    }
}

/// `get_time`: current time, UTC and local.
pub struct GetTimeTool;

#[async_trait]
impl Tool for GetTimeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "get_time".into(),
            description: "Get the current date and time, in UTC and in the local time zone.".into(),
            parameters: json!({"type": "object", "properties": {}}),
            side_effects: false,
        }
    }

    async fn call(&self, _args: Value, _ctx: &ToolContext) -> ToolOutcome {
        let secs = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs(),
            Err(_) => return ToolOutcome::Err("system clock is before the Unix epoch".to_string()),
        };
        let offset = LocalOffset::current_minutes();
        ToolOutcome::Ok(json!({
            "utc": CivilTime::utc_iso8601(secs),
            "local": CivilTime::local_iso8601(secs, offset),
            "utc_offset_minutes": offset,
            "unix": secs,
        }))
    }
}

/// Shared plumbing for the file tools: argument reading, the workspace, the blocking pool.
struct FileWork;

impl FileWork {
    const PATH: &'static str = "path";
    const CONTENT: &'static str = "content";
    const CURRENT: &'static str = ".";
    const PATH_NOT_STRING: &'static str = "path must be a string";
    const CONTENT_NOT_STRING: &'static str = "content must be a string";
    const TASK_FAILED: &'static str = "tool task failed: ";
    const SESSION_DELETED: &'static str = "session deleted";
    const PATHS_HINT: &'static str =
        "Paths are relative to your workspace; prefix /sandbox/ to read elsewhere in the sandbox.";
    const WRITE_HINT: &'static str = "Paths are relative to your workspace.";

    /// Runs `work` on the blocking pool against the call's write folder.
    async fn run<F>(ctx: &ToolContext, work: F) -> ToolOutcome
    where
        F: FnOnce(Option<&Path>) -> Result<Value, String> + Send + 'static,
    {
        let folder = ctx.scope.workspace.clone();
        match tokio::task::spawn_blocking(move || work(folder.as_deref())).await {
            Ok(Ok(value)) => ToolOutcome::Ok(value),
            Ok(Err(e)) => ToolOutcome::Err(e),
            Err(e) => ToolOutcome::Err(format!("{}{e}", Self::TASK_FAILED)),
        }
    }

    /// Like `run`, but refused first when the call's session has been deleted.
    async fn run_write<F>(ctx: &ToolContext, work: F) -> ToolOutcome
    where
        F: FnOnce(Option<&Path>) -> Result<Value, String> + Send + 'static,
    {
        if ctx.scope.session_deleted() {
            return ToolOutcome::Err(Self::SESSION_DELETED.to_string());
        }
        Self::run(ctx, work).await
    }

    /// The string argument `key`, or `err` when it is missing or not a string.
    fn string(args: &Value, key: &str, err: &str) -> Result<String, ToolOutcome> {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ToolOutcome::Err(err.to_string()))
    }

    /// The `path` and `content` arguments of a write.
    fn path_and_content(args: &Value) -> Result<(String, String), ToolOutcome> {
        Ok((
            Self::string(args, Self::PATH, Self::PATH_NOT_STRING)?,
            Self::string(args, Self::CONTENT, Self::CONTENT_NOT_STRING)?,
        ))
    }

    fn path_schema(description: &str) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": description}
            },
            "required": ["path"]
        })
    }

    fn path_and_content_schema(path: &str, content: &str) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": path},
                "content": {"type": "string", "description": content}
            },
            "required": ["path", "content"]
        })
    }
}

/// Size limits and refusals shared by the write tools.
struct WriteLimits;

impl WriteLimits {
    /// Most bytes of `content` one call may write.
    const MAX_CONTENT_BYTES: usize = 262_144;
    /// Most bytes a file may hold after a write.
    const MAX_FILE_BYTES: u64 = 1_048_576;
    const CONTENT_TOO_LARGE: &'static str = "content exceeds 262144 bytes";
    const FILE_TOO_LARGE: &'static str = "file would exceed 1048576 bytes";
    const IS_DIRECTORY: &'static str = "path is a directory";
    const CANNOT_WRITE: &'static str = "cannot write file: ";

    fn check_content(content: &str) -> Result<(), String> {
        if content.len() > Self::MAX_CONTENT_BYTES {
            return Err(Self::CONTENT_TOO_LARGE.to_string());
        }
        Ok(())
    }

    fn refuse_directory(target: &Path) -> Result<(), String> {
        if target.is_dir() {
            return Err(Self::IS_DIRECTORY.to_string());
        }
        Ok(())
    }

    fn write_error(e: std::io::Error) -> String {
        format!("{}{e}", Self::CANNOT_WRITE)
    }
}

/// `list_dir`: lists a directory in the workspace or, behind `/sandbox/`, the sandbox.
pub struct ListDirTool {
    sandbox: Sandbox,
}

impl ListDirTool {
    const NAME: &'static str = "list_dir";
    const DESCRIPTION: &'static str = "List the entries of a directory.";
    const PATH_DESCRIPTION: &'static str = "Directory; defaults to '.'";
    const NOT_A_DIRECTORY: &'static str = "path is not a directory";
    const CANNOT_READ: &'static str = "cannot read directory: ";
    const EMPTY_NOTE: &'static str =
        "your workspace is empty; prefix /sandbox/ to list the shared sandbox";
    const NOTE: &'static str = "note";

    fn list(sandbox: &Sandbox, folder: Option<&Path>, rel: &str) -> Result<Value, String> {
        let workspace = Workspace::open(sandbox, folder)?;
        let dir = workspace.resolve_read(rel)?;
        if !dir.is_dir() {
            return Err(Self::NOT_A_DIRECTORY.to_string());
        }
        let reader = fs::read_dir(&dir).map_err(|e| format!("{}{e}", Self::CANNOT_READ))?;
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
        let empty = entries.is_empty() && errors == 0;
        let listed: Vec<Value> = entries
            .into_iter()
            .map(|(name, kind, size)| json!({"name": name, "kind": kind, "size": size}))
            .collect();
        let mut result = json!({
            "path": workspace.display(&dir),
            "entries": listed,
            "truncated": truncated,
            "errors": errors,
        });
        if empty && workspace.confined() && workspace.is_root(&dir) {
            result[Self::NOTE] = json!(Self::EMPTY_NOTE);
        }
        Ok(result)
    }
}

#[async_trait]
impl Tool for ListDirTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Self::NAME.into(),
            description: format!("{} {}", Self::DESCRIPTION, FileWork::PATHS_HINT),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": Self::PATH_DESCRIPTION}
                }
            }),
            side_effects: false,
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutcome {
        let rel = match args.get(FileWork::PATH) {
            None | Some(Value::Null) => FileWork::CURRENT.to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(_) => return ToolOutcome::Err(FileWork::PATH_NOT_STRING.to_string()),
        };
        let sandbox = self.sandbox.clone();
        FileWork::run(ctx, move |folder| Self::list(&sandbox, folder, &rel)).await
    }
}

/// `read_file`: reads a UTF-8 text file in the workspace or, behind `/sandbox/`, the sandbox.
pub struct ReadFileTool {
    sandbox: Sandbox,
}

impl ReadFileTool {
    const NAME: &'static str = "read_file";
    const DESCRIPTION: &'static str = "Read a UTF-8 text file (up to 65536 bytes).";
    const PATH_DESCRIPTION: &'static str = "File to read";
    const MAX_BYTES: &'static str = "max_bytes";
    const IS_DIRECTORY: &'static str = "path is a directory, not a file";
    const CANNOT_READ: &'static str = "cannot read file: ";
    const NOT_UTF8: &'static str = "file is not valid UTF-8 text";
    const BAD_MAX_BYTES: &'static str = "max_bytes must be a non-negative integer";

    fn read(
        sandbox: &Sandbox,
        folder: Option<&Path>,
        rel: &str,
        cap: u64,
    ) -> Result<Value, String> {
        let workspace = Workspace::open(sandbox, folder)?;
        let path = workspace.resolve_read(rel)?;
        if path.is_dir() {
            return Err(Self::IS_DIRECTORY.to_string());
        }
        let cannot_read = |e: std::io::Error| format!("{}{e}", Self::CANNOT_READ);
        let file = fs::File::open(&path).map_err(cannot_read)?;
        let mut buf = Vec::new();
        file.take(cap + 1)
            .read_to_end(&mut buf)
            .map_err(cannot_read)?;
        let truncated = buf.len() as u64 > cap;
        buf.truncate(cap as usize);
        let content = match std::str::from_utf8(&buf) {
            Ok(s) => s.to_string(),
            // An incomplete sequence at the cut point backs off to the last full character.
            Err(e) if truncated && e.error_len().is_none() => {
                String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned()
            }
            Err(_) => return Err(Self::NOT_UTF8.to_string()),
        };
        Ok(json!({
            "path": workspace.display(&path),
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
            name: Self::NAME.into(),
            description: format!("{} {}", Self::DESCRIPTION, FileWork::PATHS_HINT),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": Self::PATH_DESCRIPTION},
                    "max_bytes": {"type": "integer", "description": "Maximum bytes to read (capped at 65536)"}
                },
                "required": ["path"]
            }),
            side_effects: false,
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutcome {
        let rel = match FileWork::string(&args, FileWork::PATH, FileWork::PATH_NOT_STRING) {
            Ok(rel) => rel,
            Err(out) => return out,
        };
        let cap = match args.get(Self::MAX_BYTES) {
            None | Some(Value::Null) => MAX_READ_BYTES,
            Some(v) => match v.as_u64() {
                Some(n) => n.min(MAX_READ_BYTES),
                None => return ToolOutcome::Err(Self::BAD_MAX_BYTES.to_string()),
            },
        };
        let sandbox = self.sandbox.clone();
        FileWork::run(ctx, move |folder| Self::read(&sandbox, folder, &rel, cap)).await
    }
}

/// `write_file`: creates or replaces a file in the write folder.
pub struct WriteFileTool {
    sandbox: Sandbox,
}

impl WriteFileTool {
    const NAME: &'static str = "write_file";
    const DESCRIPTION: &'static str =
        "Create or overwrite a text file in your workspace, creating missing parent folders.";
    const PATH_DESCRIPTION: &'static str = "File to write";
    const CONTENT_DESCRIPTION: &'static str = "The whole new content (at most 262144 bytes)";
    const TEMP_SUFFIX: &'static str = ".tmp";
    const NO_PARENT: &'static str = "path has no parent folder";

    fn write(
        sandbox: &Sandbox,
        folder: Option<&Path>,
        path: &str,
        content: &str,
    ) -> Result<Value, String> {
        WriteLimits::check_content(content)?;
        let workspace = Workspace::open(sandbox, folder)?;
        let target = workspace.prepare_write(path)?;
        WriteLimits::refuse_directory(&target)?;
        Self::replace(&target, content.as_bytes())?;
        Ok(json!({"path": workspace.display(&target), "bytes": content.len()}))
    }

    /// Writes a temp file beside `target`, then renames it over `target`, so an interrupted
    /// write never leaves half a file.
    fn replace(target: &Path, bytes: &[u8]) -> Result<(), String> {
        let (Some(parent), Some(name)) = (target.parent(), target.file_name()) else {
            return Err(Self::NO_PARENT.to_string());
        };
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp = parent.join(format!(
            ".{}.{}-{nanos}{}",
            name.to_string_lossy(),
            std::process::id(),
            Self::TEMP_SUFFIX
        ));
        let written = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .and_then(|mut file| {
                file.write_all(bytes)?;
                file.sync_all()
            })
            .and_then(|()| fs::rename(&temp, target));
        if let Err(e) = written {
            let _ = fs::remove_file(&temp);
            return Err(WriteLimits::write_error(e));
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Self::NAME.into(),
            description: format!("{} {}", Self::DESCRIPTION, FileWork::WRITE_HINT),
            parameters: FileWork::path_and_content_schema(
                Self::PATH_DESCRIPTION,
                Self::CONTENT_DESCRIPTION,
            ),
            side_effects: true,
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutcome {
        let (path, content) = match FileWork::path_and_content(&args) {
            Ok(pair) => pair,
            Err(out) => return out,
        };
        let sandbox = self.sandbox.clone();
        FileWork::run_write(ctx, move |folder| {
            Self::write(&sandbox, folder, &path, &content)
        })
        .await
    }
}

/// `append_file`: appends to a file in the write folder, creating it if needed.
pub struct AppendFileTool {
    sandbox: Sandbox,
}

impl AppendFileTool {
    const NAME: &'static str = "append_file";
    const DESCRIPTION: &'static str = "Append text to a file in your workspace, creating the file and missing parent folders if needed. Files are capped at 1048576 bytes.";
    const PATH_DESCRIPTION: &'static str = "File to append to";
    const CONTENT_DESCRIPTION: &'static str = "Text to append (at most 262144 bytes)";

    fn append(
        sandbox: &Sandbox,
        folder: Option<&Path>,
        path: &str,
        content: &str,
    ) -> Result<Value, String> {
        WriteLimits::check_content(content)?;
        let workspace = Workspace::open(sandbox, folder)?;
        let target = workspace.prepare_write(path)?;
        WriteLimits::refuse_directory(&target)?;
        let existing = match fs::metadata(&target) {
            Ok(meta) => meta.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(WriteLimits::write_error(e)),
        };
        // Checked before opening, so a refused append leaves the file as it was.
        let size = existing + content.len() as u64;
        if size > WriteLimits::MAX_FILE_BYTES {
            return Err(WriteLimits::FILE_TOO_LARGE.to_string());
        }
        fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&target)
            .and_then(|mut file| file.write_all(content.as_bytes()))
            .map_err(WriteLimits::write_error)?;
        Ok(json!({"path": workspace.display(&target), "bytes": size}))
    }
}

#[async_trait]
impl Tool for AppendFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Self::NAME.into(),
            description: format!("{} {}", Self::DESCRIPTION, FileWork::WRITE_HINT),
            parameters: FileWork::path_and_content_schema(
                Self::PATH_DESCRIPTION,
                Self::CONTENT_DESCRIPTION,
            ),
            side_effects: true,
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutcome {
        let (path, content) = match FileWork::path_and_content(&args) {
            Ok(pair) => pair,
            Err(out) => return out,
        };
        let sandbox = self.sandbox.clone();
        FileWork::run_write(ctx, move |folder| {
            Self::append(&sandbox, folder, &path, &content)
        })
        .await
    }
}

/// `make_dir`: creates a folder and its parents in the write folder.
pub struct MakeDirTool {
    sandbox: Sandbox,
}

impl MakeDirTool {
    const NAME: &'static str = "make_dir";
    const DESCRIPTION: &'static str =
        "Create a folder and its parents in your workspace; no error if it exists.";
    const PATH_DESCRIPTION: &'static str = "Folder to create";
    const IS_FILE: &'static str = "path is a file";
    const CANNOT_CREATE: &'static str = "cannot create directory: ";

    fn make(sandbox: &Sandbox, folder: Option<&Path>, path: &str) -> Result<Value, String> {
        let workspace = Workspace::open(sandbox, folder)?;
        let target = workspace.resolve_write(path)?;
        if target.is_file() {
            return Err(Self::IS_FILE.to_string());
        }
        fs::create_dir_all(&target).map_err(|e| format!("{}{e}", Self::CANNOT_CREATE))?;
        // Resolve again: what now exists must still lie inside the write folder.
        let made = workspace.resolve_write(path)?;
        Ok(json!({"path": workspace.display(&made)}))
    }
}

#[async_trait]
impl Tool for MakeDirTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Self::NAME.into(),
            description: format!("{} {}", Self::DESCRIPTION, FileWork::WRITE_HINT),
            parameters: FileWork::path_schema(Self::PATH_DESCRIPTION),
            side_effects: true,
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutcome {
        let path = match FileWork::string(&args, FileWork::PATH, FileWork::PATH_NOT_STRING) {
            Ok(path) => path,
            Err(out) => return out,
        };
        let sandbox = self.sandbox.clone();
        FileWork::run_write(ctx, move |folder| Self::make(&sandbox, folder, &path)).await
    }
}

/// `delete_path`: deletes one file or one empty folder in the write folder.
pub struct DeletePathTool {
    sandbox: Sandbox,
}

impl DeletePathTool {
    const NAME: &'static str = "delete_path";
    const DESCRIPTION: &'static str = "Delete one file or one empty folder in your workspace.";
    const PATH_DESCRIPTION: &'static str = "File or empty folder to delete";
    const NOT_EMPTY: &'static str = "directory is not empty";
    const ROOT: &'static str = "cannot delete the workspace root";
    const CANNOT_DELETE: &'static str = "cannot delete: ";
    const FILE: &'static str = "file";
    const DIR: &'static str = "dir";

    fn delete(sandbox: &Sandbox, folder: Option<&Path>, path: &str) -> Result<Value, String> {
        let workspace = Workspace::open(sandbox, folder)?;
        let target = workspace.resolve_write(path)?;
        if workspace.is_root(&target) {
            return Err(Self::ROOT.to_string());
        }
        let meta = fs::symlink_metadata(&target).map_err(|_| Sandbox::no_such_path(path))?;
        let shown = workspace.display(&target);
        let cannot = |e: std::io::Error| format!("{}{e}", Self::CANNOT_DELETE);
        let deleted = if meta.is_dir() {
            if fs::read_dir(&target).map_err(cannot)?.next().is_some() {
                return Err(Self::NOT_EMPTY.to_string());
            }
            fs::remove_dir(&target).map_err(cannot)?;
            Self::DIR
        } else {
            fs::remove_file(&target).map_err(cannot)?;
            Self::FILE
        };
        Ok(json!({"path": shown, "deleted": deleted}))
    }
}

#[async_trait]
impl Tool for DeletePathTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Self::NAME.into(),
            description: format!("{} {}", Self::DESCRIPTION, FileWork::WRITE_HINT),
            parameters: FileWork::path_schema(Self::PATH_DESCRIPTION),
            side_effects: true,
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutcome {
        let path = match FileWork::string(&args, FileWork::PATH, FileWork::PATH_NOT_STRING) {
            Ok(path) => path,
            Err(out) => return out,
        };
        let sandbox = self.sandbox.clone();
        FileWork::run_write(ctx, move |folder| Self::delete(&sandbox, folder, &path)).await
    }
}

/// Built-in tools: `get_time` always; the file tools only with a sandbox.
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
            let sandbox = sandbox.clone();
            tools.push(Arc::new(ListDirTool {
                sandbox: sandbox.clone(),
            }));
            tools.push(Arc::new(ReadFileTool {
                sandbox: sandbox.clone(),
            }));
            tools.push(Arc::new(WriteFileTool {
                sandbox: sandbox.clone(),
            }));
            tools.push(Arc::new(AppendFileTool {
                sandbox: sandbox.clone(),
            }));
            tools.push(Arc::new(MakeDirTool {
                sandbox: sandbox.clone(),
            }));
            tools.push(Arc::new(DeletePathTool { sandbox }));
        }
        tools
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::runtime::tool::{SessionHandle, ToolScope};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A unique temp directory removed on drop.
    pub(crate) struct TestDir {
        pub(crate) path: PathBuf,
    }

    impl TestDir {
        pub(crate) fn new() -> Self {
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

        pub(crate) fn sandbox(&self) -> Sandbox {
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
            self.run_scoped(name, args, &ToolScope::default()).await
        }

        /// Runs a builtin tool with `scope` in its context.
        pub(crate) async fn run_scoped(
            &self,
            name: &str,
            args: Value,
            scope: &ToolScope,
        ) -> ToolOutcome {
            let ctx = ToolContext {
                call_id: "c1".into(),
                scope: scope.clone(),
            };
            self.tool(name).call(args, &ctx).await
        }

        /// A scope whose write folder is `folder`, with no session.
        fn scope(folder: &str) -> ToolScope {
            ToolScope {
                workspace: Some(PathBuf::from(folder)),
                session: None,
            }
        }

        /// Creates a directory junction `link` pointing at `target`.
        #[cfg(windows)]
        pub(crate) fn junction(link: &Path, target: &Path) {
            let made = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(made.status.success(), "mklink failed");
        }
    }

    /// A session handle whose existence the test controls.
    struct FakeSession {
        alive: AtomicBool,
    }

    impl SessionHandle for FakeSession {
        fn notes(&self) -> Option<Vec<String>> {
            self.exists().then(Vec::new)
        }

        fn set_notes(&self, _notes: Vec<String>) -> bool {
            self.exists()
        }

        fn exists(&self) -> bool {
            self.alive.load(Ordering::SeqCst)
        }
    }

    /// Runs write tools in the `sessions/s1` write folder.
    struct Writes;

    impl Writes {
        const FOLDER: &'static str = "sessions/s1";

        fn folder(dir: &TestDir) -> PathBuf {
            dir.path.join("sessions").join("s1")
        }

        async fn run(dir: &TestDir, name: &str, args: Value) -> ToolOutcome {
            dir.run_scoped(name, args, &TestDir::scope(Self::FOLDER))
                .await
        }

        fn err(msg: &str) -> ToolOutcome {
            ToolOutcome::Err(msg.to_string())
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

        let offset = v["utc_offset_minutes"].as_i64().unwrap() as i32;
        assert!((-14 * 60..=14 * 60).contains(&offset), "{offset}");
        assert_eq!(offset, LocalOffset::current_minutes());
        assert_eq!(v["local"], CivilTime::local_iso8601(unix, offset));
    }

    #[test]
    fn local_time_formats_offsets() {
        assert_eq!(CivilTime::local_iso8601(0, 0), "1970-01-01T00:00:00+00:00");
        assert_eq!(CivilTime::local_iso8601(0, 60), "1970-01-01T01:00:00+01:00");
        assert_eq!(
            CivilTime::local_iso8601(0, -330),
            "1969-12-31T18:30:00-05:30"
        );
        // 2026-11-23T13:05:00Z
        assert_eq!(
            CivilTime::local_iso8601(1_795_439_100, 60),
            "2026-11-23T14:05:00+01:00"
        );
        assert_eq!(
            CivilTime::utc_iso8601(1_795_439_100),
            "2026-11-23T13:05:00Z"
        );
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
        TestDir::junction(&link, &outside);
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
        TestDir::junction(&link, &sibling);
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
        let specs: Vec<ToolSpec> = BuiltinSource::new(Some(dir.sandbox()))
            .tools()
            .iter()
            .map(|t| t.spec())
            .collect();
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "get_time",
                "list_dir",
                "read_file",
                "write_file",
                "append_file",
                "make_dir",
                "delete_path"
            ]
        );
        let writers: Vec<&str> = specs
            .iter()
            .filter(|s| s.side_effects)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(
            writers,
            ["write_file", "append_file", "make_dir", "delete_path"]
        );
        let hint =
            "Paths are relative to your workspace; prefix /sandbox/ to read elsewhere in the sandbox.";
        assert!(specs[1].description.ends_with(hint));
        assert!(specs[2].description.ends_with(hint));
    }

    #[tokio::test]
    async fn write_file_creates_parents_and_reports_bytes() {
        let dir = TestDir::new();
        let args = json!({"path": "a/b/c.txt", "content": "hello"});
        let out = Writes::run(&dir, "write_file", args).await;
        assert_eq!(
            out,
            ToolOutcome::Ok(json!({"path": "a/b/c.txt", "bytes": 5}))
        );
        let written = Writes::folder(&dir).join("a").join("b");
        assert_eq!(fs::read_to_string(written.join("c.txt")).unwrap(), "hello");
        assert_eq!(fs::read_dir(&written).unwrap().count(), 1, "no temp file");
    }

    #[tokio::test]
    async fn write_file_overwrites() {
        let dir = TestDir::new();
        let first = json!({"path": "f.txt", "content": "a much longer first version"});
        assert!(Writes::run(&dir, "write_file", first).await.is_ok());
        let second = json!({"path": "f.txt", "content": "x"});
        let out = Writes::run(&dir, "write_file", second).await;
        assert_eq!(out, ToolOutcome::Ok(json!({"path": "f.txt", "bytes": 1})));
        let folder = Writes::folder(&dir);
        assert_eq!(fs::read_to_string(folder.join("f.txt")).unwrap(), "x");
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 1, "no temp file");
    }

    #[tokio::test]
    async fn content_over_cap_is_refused() {
        let dir = TestDir::new();
        let over = "a".repeat(262_145);
        for tool in ["write_file", "append_file"] {
            let args = json!({"path": "big.txt", "content": over});
            let out = Writes::run(&dir, tool, args).await;
            assert_eq!(out, Writes::err("content exceeds 262144 bytes"), "{tool}");
        }
        assert!(!Writes::folder(&dir).join("big.txt").exists());
        let at_cap = json!({"path": "big.txt", "content": "a".repeat(262_144)});
        let out = Writes::run(&dir, "write_file", at_cap).await;
        assert_eq!(
            out,
            ToolOutcome::Ok(json!({"path": "big.txt", "bytes": 262_144}))
        );
    }

    #[tokio::test]
    async fn append_creates_then_appends_and_reports_size() {
        let dir = TestDir::new();
        let first = json!({"path": "d/log.txt", "content": "ab"});
        let out = Writes::run(&dir, "append_file", first).await;
        assert_eq!(
            out,
            ToolOutcome::Ok(json!({"path": "d/log.txt", "bytes": 2}))
        );
        let second = json!({"path": "d/log.txt", "content": "cde"});
        let out = Writes::run(&dir, "append_file", second).await;
        assert_eq!(
            out,
            ToolOutcome::Ok(json!({"path": "d/log.txt", "bytes": 5}))
        );
        let log = Writes::folder(&dir).join("d").join("log.txt");
        assert_eq!(fs::read_to_string(log).unwrap(), "abcde");
    }

    #[tokio::test]
    async fn append_over_file_cap_leaves_file_unchanged() {
        let dir = TestDir::new();
        fs::create_dir_all(Writes::folder(&dir)).unwrap();
        let file = Writes::folder(&dir).join("big.txt");
        let original = vec![b'x'; 1_048_576 - 3];
        fs::write(&file, &original).unwrap();
        let over = json!({"path": "big.txt", "content": "abcd"});
        let out = Writes::run(&dir, "append_file", over).await;
        assert_eq!(out, Writes::err("file would exceed 1048576 bytes"));
        assert_eq!(fs::read(&file).unwrap(), original, "file unchanged on disk");
        let fits = json!({"path": "big.txt", "content": "abc"});
        let out = Writes::run(&dir, "append_file", fits).await;
        assert_eq!(
            out,
            ToolOutcome::Ok(json!({"path": "big.txt", "bytes": 1_048_576}))
        );
    }

    #[tokio::test]
    async fn write_over_directory_is_refused() {
        let dir = TestDir::new();
        fs::create_dir_all(Writes::folder(&dir).join("sub")).unwrap();
        for tool in ["write_file", "append_file"] {
            for path in ["sub", "."] {
                let args = json!({"path": path, "content": "x"});
                let out = Writes::run(&dir, tool, args).await;
                assert_eq!(out, Writes::err("path is a directory"), "{tool} {path}");
            }
        }
        assert!(Writes::folder(&dir).join("sub").is_dir());
    }

    #[tokio::test]
    async fn make_dir_is_idempotent_and_refuses_file() {
        let dir = TestDir::new();
        for _ in 0..2 {
            let out = Writes::run(&dir, "make_dir", json!({"path": "x/y"})).await;
            assert_eq!(out, ToolOutcome::Ok(json!({"path": "x/y"})));
        }
        assert!(Writes::folder(&dir).join("x").join("y").is_dir());
        fs::write(Writes::folder(&dir).join("f"), "f").unwrap();
        let out = Writes::run(&dir, "make_dir", json!({"path": "f"})).await;
        assert_eq!(out, Writes::err("path is a file"));
    }

    #[tokio::test]
    async fn delete_path_file_and_empty_dir() {
        let dir = TestDir::new();
        let folder = Writes::folder(&dir);
        fs::create_dir_all(folder.join("e")).unwrap();
        fs::write(folder.join("f.txt"), "f").unwrap();
        let out = Writes::run(&dir, "delete_path", json!({"path": "f.txt"})).await;
        assert_eq!(
            out,
            ToolOutcome::Ok(json!({"path": "f.txt", "deleted": "file"}))
        );
        let out = Writes::run(&dir, "delete_path", json!({"path": "e"})).await;
        assert_eq!(out, ToolOutcome::Ok(json!({"path": "e", "deleted": "dir"})));
        assert!(!folder.join("f.txt").exists());
        assert!(!folder.join("e").exists());
    }

    #[tokio::test]
    async fn delete_non_empty_dir_is_refused() {
        let dir = TestDir::new();
        let folder = Writes::folder(&dir);
        fs::create_dir_all(folder.join("d")).unwrap();
        fs::write(folder.join("d").join("x"), "x").unwrap();
        let out = Writes::run(&dir, "delete_path", json!({"path": "d"})).await;
        assert_eq!(out, Writes::err("directory is not empty"));
        assert!(folder.join("d").join("x").is_file());
    }

    #[tokio::test]
    async fn delete_root_is_refused() {
        let dir = TestDir::new();
        for path in [".", "", "sub/.."] {
            let out = Writes::run(&dir, "delete_path", json!({"path": path})).await;
            assert_eq!(
                out,
                Writes::err("cannot delete the workspace root"),
                "{path:?}"
            );
        }
        assert!(Writes::folder(&dir).is_dir());
        // Without a write folder the root is the sandbox itself.
        let out = dir.run("delete_path", json!({"path": "."})).await;
        assert_eq!(out, Writes::err("cannot delete the workspace root"));
        assert!(dir.path.is_dir());
    }

    #[tokio::test]
    async fn delete_missing_reports_no_such_path() {
        let dir = TestDir::new();
        let out = Writes::run(&dir, "delete_path", json!({"path": "nope"})).await;
        assert_eq!(out, Writes::err("no such path: nope"));
    }

    #[tokio::test]
    async fn write_after_session_deleted_is_refused() {
        let dir = TestDir::new();
        let session = Arc::new(FakeSession {
            alive: AtomicBool::new(false),
        });
        let scope = ToolScope {
            workspace: Some(PathBuf::from(Writes::FOLDER)),
            session: Some(session.clone()),
        };
        let calls = [
            ("write_file", json!({"path": "a.txt", "content": "x"})),
            ("append_file", json!({"path": "a.txt", "content": "x"})),
            ("make_dir", json!({"path": "d"})),
            ("delete_path", json!({"path": "a.txt"})),
        ];
        for (tool, args) in calls.clone() {
            let out = dir.run_scoped(tool, args, &scope).await;
            assert_eq!(out, Writes::err("session deleted"), "{tool}");
        }
        assert!(!dir.path.join("sessions").exists(), "nothing created");
        session.alive.store(true, Ordering::SeqCst);
        for (tool, args) in calls {
            assert!(dir.run_scoped(tool, args, &scope).await.is_ok(), "{tool}");
        }
    }

    #[tokio::test]
    async fn empty_workspace_list_carries_note() {
        let dir = TestDir::new();
        let out = Writes::run(&dir, "list_dir", json!({})).await;
        assert_eq!(
            out,
            ToolOutcome::Ok(json!({
                "path": ".",
                "entries": [],
                "truncated": false,
                "errors": 0,
                "note": "your workspace is empty; prefix /sandbox/ to list the shared sandbox",
            }))
        );
        fs::write(Writes::folder(&dir).join("a.txt"), "a").unwrap();
        let ToolOutcome::Ok(v) = Writes::run(&dir, "list_dir", json!({})).await else {
            panic!("expected Ok");
        };
        assert!(v.get("note").is_none());
        assert_eq!(v["entries"][0]["name"], "a.txt");
        let root = Writes::run(&dir, "list_dir", json!({"path": "/sandbox"})).await;
        let ToolOutcome::Ok(v) = root else {
            panic!("expected Ok");
        };
        assert_eq!(v["path"], "/sandbox");
        assert_eq!(v["entries"][0]["name"], "sessions");
    }

    #[tokio::test]
    async fn read_file_reads_workspace_and_sandbox_prefix() {
        let dir = TestDir::new();
        fs::create_dir_all(Writes::folder(&dir)).unwrap();
        fs::write(Writes::folder(&dir).join("n.txt"), "mine").unwrap();
        fs::write(dir.path.join("n.txt"), "root").unwrap();
        let out = Writes::run(&dir, "read_file", json!({"path": "n.txt"})).await;
        assert_eq!(
            out,
            ToolOutcome::Ok(
                json!({"path": "n.txt", "content": "mine", "bytes": 4, "truncated": false})
            )
        );
        let out = Writes::run(&dir, "read_file", json!({"path": "/sandbox/n.txt"})).await;
        assert_eq!(
            out,
            ToolOutcome::Ok(
                json!({"path": "/sandbox/n.txt", "content": "root", "bytes": 4, "truncated": false})
            )
        );
    }
}
