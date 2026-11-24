//! A session's write folder inside the sandbox: where its file tools resolve paths.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::builtin::Sandbox;

/// The canonical write folder of one call, inside a sandbox.
///
/// Reads resolve against the folder, or against the sandbox root behind the `/sandbox`
/// prefix. Writes resolve against the folder only, and every existing component of the
/// target must resolve inside it, so a link that leads out is refused before anything is
/// written.
#[derive(Debug, Clone)]
pub struct Workspace {
    sandbox: Sandbox,
    folder: PathBuf,
}

impl Workspace {
    pub const SANDBOX_PREFIX: &'static str = "/sandbox";
    pub const WRITES_CONFINED: &'static str = "writes are confined to your workspace";
    const CANNOT_CREATE: &'static str = "cannot create workspace folder: ";
    const CANNOT_CREATE_PARENT: &'static str = "cannot create parent folders: ";
    const NOT_A_FOLDER: &'static str = "workspace folder must name a folder below the sandbox";
    const BACKSLASH: char = '\\';

    /// Opens the write folder `folder` (relative to the sandbox; None is the sandbox root),
    /// creating it if missing, and checks that it lies inside the sandbox.
    pub fn open(sandbox: &Sandbox, folder: Option<&Path>) -> Result<Workspace, String> {
        let root = sandbox.root();
        let Some(folder) = folder else {
            return Ok(Self {
                sandbox: sandbox.clone(),
                folder: root.to_path_buf(),
            });
        };
        let rel = Sandbox::normalise(folder)?;
        // Check the existing part first, so a link at the sandbox root can never make
        // `create_dir_all` build folders outside.
        let target = Self::confine(root, &rel)?;
        fs::create_dir_all(&target).map_err(|e| format!("{}{e}", Self::CANNOT_CREATE))?;
        let canonical = fs::canonicalize(&target).map_err(|_| Sandbox::ESCAPES.to_string())?;
        if !canonical.starts_with(root) {
            return Err(Sandbox::ESCAPES.to_string());
        }
        Ok(Self {
            sandbox: sandbox.clone(),
            folder: canonical,
        })
    }

    /// The workspace a read of `path` resolves in. A `/sandbox` read never touches the write
    /// folder, so it neither creates the folder nor fails when the folder cannot be made (say,
    /// `sessions` is a file); any other read opens the folder like a write does.
    pub fn open_read(
        sandbox: &Sandbox,
        folder: Option<&Path>,
        path: &str,
    ) -> Result<Workspace, String> {
        if Self::sandbox_relative(path).is_none() {
            return Self::open(sandbox, folder);
        }
        let root = sandbox.root();
        let folder = match folder {
            // Only located, for display: the part that exists must still lie inside.
            Some(folder) => Self::confine(root, &Sandbox::normalise(folder)?)?,
            None => root.to_path_buf(),
        };
        Ok(Self {
            sandbox: sandbox.clone(),
            folder,
        })
    }

    /// Resolves a read path to an existing canonical path inside the sandbox.
    pub fn resolve_read(&self, path: &str) -> Result<PathBuf, String> {
        match Self::sandbox_relative(path) {
            Some(rest) => self.sandbox.resolve_in(self.sandbox.root(), &rest, path),
            None => self.sandbox.resolve_in(&self.folder, path, path),
        }
    }

    /// Resolves a write path inside the folder. The target may not exist: its nearest
    /// existing ancestor is canonicalised and must lie inside the folder, and the rest are
    /// plain names.
    pub fn resolve_write(&self, path: &str) -> Result<PathBuf, String> {
        if Self::sandbox_relative(path).is_some() {
            return Err(Self::WRITES_CONFINED.to_string());
        }
        let rel = Sandbox::normalise(Path::new(path))?;
        Self::confine(&self.folder, &rel)
    }

    /// Resolves a write path without following its last component: the parent is confined
    /// like any write path and the leaf name is joined as it is, so a link there is the link
    /// itself. A path naming the folder itself resolves to the folder.
    pub fn resolve_leaf(&self, path: &str) -> Result<PathBuf, String> {
        if Self::sandbox_relative(path).is_some() {
            return Err(Self::WRITES_CONFINED.to_string());
        }
        let rel = Sandbox::normalise(Path::new(path))?;
        let Some(name) = rel.file_name() else {
            return Ok(self.folder.clone());
        };
        let parent = Self::confine(&self.folder, rel.parent().unwrap_or(Path::new("")))?;
        Ok(parent.join(name))
    }

    /// Resolves a write path and creates its missing parent folders, then resolves it again
    /// so a link swapped in while creating them is still caught.
    pub fn prepare_write(&self, path: &str) -> Result<PathBuf, String> {
        let target = self.resolve_write(path)?;
        match target.parent() {
            Some(parent) if !parent.is_dir() => {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("{}{e}", Self::CANNOT_CREATE_PARENT))?;
                self.resolve_write(path)
            }
            _ => Ok(target),
        }
    }

    /// Display form: relative to the folder ("." for the folder itself), or `/sandbox/...`
    /// elsewhere in the sandbox. Never exposes the host path.
    pub fn display(&self, resolved: &Path) -> String {
        if let Ok(rel) = resolved.strip_prefix(&self.folder) {
            return Sandbox::slashed(rel).unwrap_or_else(|| Sandbox::CURRENT.to_string());
        }
        match resolved
            .strip_prefix(self.sandbox.root())
            .ok()
            .and_then(Sandbox::slashed)
        {
            Some(rel) => format!("{}{}{rel}", Self::SANDBOX_PREFIX, Sandbox::SEPARATOR),
            None => Self::SANDBOX_PREFIX.to_string(),
        }
    }

    /// True when `resolved` is the write folder itself.
    pub fn is_root(&self, resolved: &Path) -> bool {
        resolved == self.folder
    }

    /// True when the write folder is a folder below the sandbox root, not the root itself.
    pub fn confined(&self) -> bool {
        self.folder != self.sandbox.root()
    }

    /// Removes a write folder (relative to the sandbox) and everything in it. Links inside
    /// are removed, never followed. A folder that does not exist is not an error.
    pub fn remove_folder(sandbox: &Sandbox, folder: &Path) -> io::Result<()> {
        let invalid = |msg: String| io::Error::new(io::ErrorKind::InvalidInput, msg);
        let rel = Sandbox::normalise(folder).map_err(invalid)?;
        let Some(name) = rel.file_name() else {
            return Err(invalid(Self::NOT_A_FOLDER.to_string()));
        };
        let root = sandbox.root();
        let parent = root.join(rel.parent().unwrap_or(Path::new("")));
        let parent = match fs::canonicalize(&parent) {
            Ok(p) => p,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        // The parent must resolve inside the sandbox, so nothing outside is ever touched.
        if !parent.starts_with(root) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                Sandbox::ESCAPES,
            ));
        }
        // `remove_dir_all` removes links themselves rather than following them.
        match fs::remove_dir_all(parent.join(name)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    /// The sandbox-relative rest of a `/sandbox` or `/sandbox/...` path; None otherwise.
    /// Backslashes count as slashes on Windows.
    fn sandbox_relative(path: &str) -> Option<String> {
        let unified = if cfg!(windows) {
            path.replace(Self::BACKSLASH, Sandbox::SEPARATOR)
        } else {
            path.to_string()
        };
        let rest = unified.strip_prefix(Self::SANDBOX_PREFIX)?;
        if rest.is_empty() {
            return Some(String::new());
        }
        rest.strip_prefix(Sandbox::SEPARATOR).map(str::to_string)
    }

    /// `base` (canonical) joined with `rel` (plain names), checked without following links
    /// past what exists: the nearest existing ancestor must canonicalise inside `base`.
    fn confine(base: &Path, rel: &Path) -> Result<PathBuf, String> {
        let mut existing = base.to_path_buf();
        let mut missing = PathBuf::new();
        for component in rel.components() {
            let Component::Normal(part) = component else {
                return Err(Sandbox::ESCAPES.to_string());
            };
            if missing.as_os_str().is_empty() {
                let next = existing.join(part);
                // `symlink_metadata` sees a dangling link too: it then fails to
                // canonicalise below and is refused, instead of being written through.
                if fs::symlink_metadata(&next).is_ok() {
                    existing = next;
                    continue;
                }
            }
            missing.push(part);
        }
        let canonical = fs::canonicalize(&existing).map_err(|_| Sandbox::ESCAPES.to_string())?;
        if !canonical.starts_with(base) {
            return Err(Sandbox::ESCAPES.to_string());
        }
        let mut target = canonical;
        target.extend(missing.components());
        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::builtin::tests::TestDir;
    use crate::runtime::tool::{ToolOutcome, ToolScope};
    use serde_json::json;
    use std::fs;

    struct Fixture;

    impl Fixture {
        const FOLDER: &'static str = "sessions/s1";

        fn session(dir: &TestDir) -> Workspace {
            Workspace::open(&dir.sandbox(), Some(Path::new(Self::FOLDER))).unwrap()
        }

        fn folder(dir: &TestDir) -> PathBuf {
            dir.path.join("sessions").join("s1")
        }
    }

    #[test]
    fn reads_resolve_against_the_write_folder() {
        let dir = TestDir::new();
        let ws = Fixture::session(&dir);
        assert!(Fixture::folder(&dir).is_dir(), "open creates the folder");
        fs::write(Fixture::folder(&dir).join("a.txt"), "mine").unwrap();
        fs::write(dir.path.join("a.txt"), "root").unwrap();
        let resolved = ws.resolve_read("a.txt").unwrap();
        assert_eq!(fs::read_to_string(&resolved).unwrap(), "mine");
        assert_eq!(ws.display(&resolved), "a.txt");
        assert_eq!(ws.resolve_read("nope"), Err("no such path: nope".into()));
        assert_eq!(
            ws.resolve_read("/etc"),
            Err("path must be relative to the sandbox".into())
        );
    }

    #[test]
    fn sandbox_prefix_reads_the_root() {
        let dir = TestDir::new();
        let ws = Fixture::session(&dir);
        fs::write(dir.path.join("notes.txt"), "n").unwrap();
        let notes = ws.resolve_read("/sandbox/notes.txt").unwrap();
        assert_eq!(fs::read_to_string(&notes).unwrap(), "n");
        assert_eq!(ws.display(&notes), "/sandbox/notes.txt");
        let root = ws.resolve_read("/sandbox").unwrap();
        assert_eq!(root, fs::canonicalize(&dir.path).unwrap());
        assert_eq!(ws.display(&root), "/sandbox");
        assert_eq!(ws.resolve_read("/sandbox/").unwrap(), root);
        assert_eq!(
            ws.resolve_read("/sandboxes/notes.txt"),
            Err("path must be relative to the sandbox".into())
        );
        assert_eq!(
            ws.resolve_read("/sandbox/../x"),
            Err("path escapes the sandbox".into())
        );
        assert_eq!(
            ws.resolve_read("/sandbox/missing"),
            Err("no such path: /sandbox/missing".into())
        );
        #[cfg(windows)]
        assert_eq!(ws.resolve_read(r"\sandbox\notes.txt").unwrap(), notes);
    }

    #[test]
    fn write_to_sandbox_prefix_is_refused() {
        let dir = TestDir::new();
        let ws = Fixture::session(&dir);
        for path in [
            "/sandbox/x.txt",
            "/sandbox",
            "/sandbox/sessions/s1/x.txt",
            r"\sandbox\x.txt",
        ] {
            if !cfg!(windows) && path.starts_with('\\') {
                continue;
            }
            assert_eq!(
                ws.resolve_write(path),
                Err("writes are confined to your workspace".into()),
                "{path}"
            );
        }
        assert_eq!(
            ws.resolve_write("/sandboxes/x.txt"),
            Err("path must be relative to the sandbox".into())
        );
    }

    #[test]
    fn dotdot_out_of_folder_is_refused() {
        let dir = TestDir::new();
        let ws = Fixture::session(&dir);
        fs::write(dir.path.join("sessions").join("x.txt"), "x").unwrap();
        for path in ["../x.txt", "a/../../x.txt", "../s2/x.txt"] {
            assert_eq!(
                ws.resolve_write(path),
                Err("path escapes the sandbox".into()),
                "{path}"
            );
            assert_eq!(
                ws.resolve_read(path),
                Err("path escapes the sandbox".into()),
                "{path}"
            );
        }
        let inner = ws.resolve_write("a/../b.txt").unwrap();
        assert_eq!(ws.display(&inner), "b.txt");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn write_through_inner_junction_is_refused() {
        let dir = TestDir::new();
        let outside = TestDir::new();
        let ws = Fixture::session(&dir);
        let link = Fixture::folder(&dir).join("link");
        TestDir::junction(&link, &outside.path);
        let shared = dir.path.join("shared");
        fs::create_dir(&shared).unwrap();
        let inner = Fixture::folder(&dir).join("inner");
        TestDir::junction(&inner, &shared);

        let escapes = Err("path escapes the sandbox".to_string());
        let direct = [
            ws.resolve_write("link/new.txt"),
            ws.resolve_write("link/deep/new.txt"),
            ws.resolve_write("link"),
            // A junction to elsewhere in the sandbox is still outside the write folder.
            ws.resolve_write("inner/new.txt"),
        ];
        let scope = ToolScope {
            workspace: Some(PathBuf::from(Fixture::FOLDER)),
            session: None,
        };
        let tools = [
            dir.run_scoped(
                "write_file",
                json!({"path": "link/new.txt", "content": "x"}),
                &scope,
            )
            .await,
            dir.run_scoped(
                "append_file",
                json!({"path": "link/sub/new.txt", "content": "x"}),
                &scope,
            )
            .await,
            dir.run_scoped("make_dir", json!({"path": "link/made"}), &scope)
                .await,
            dir.run_scoped("delete_path", json!({"path": "link/new.txt"}), &scope)
                .await,
        ];
        let outside_entries = fs::read_dir(&outside.path).unwrap().count();
        let shared_entries = fs::read_dir(&shared).unwrap().count();
        // Deleting the junction itself removes the link, never what it points at.
        let unlinked = dir
            .run_scoped("delete_path", json!({"path": "link"}), &scope)
            .await;
        let link_gone = fs::symlink_metadata(&link).is_err();
        // Reads through the inner junction reach the shared folder: it is in the sandbox.
        let readable = ws.resolve_read("inner").is_ok();
        // Remove the junctions themselves so cleanup never follows them.
        let _ = fs::remove_dir(&link);
        let _ = fs::remove_dir(&inner);

        for result in direct {
            assert_eq!(result, escapes);
        }
        for out in tools {
            assert_eq!(out, ToolOutcome::Err("path escapes the sandbox".into()));
        }
        assert_eq!(outside_entries, 0, "nothing written outside");
        assert_eq!(
            shared_entries, 0,
            "nothing written through the inner junction"
        );
        assert!(readable);
        assert_eq!(
            unlinked,
            ToolOutcome::Ok(json!({"path": "link", "deleted": "dir"}))
        );
        assert!(link_gone);
        assert!(outside.path.is_dir());
    }

    #[test]
    fn display_is_relative_or_sandbox_prefixed() {
        let dir = TestDir::new();
        let ws = Fixture::session(&dir);
        fs::create_dir(Fixture::folder(&dir).join("sub")).unwrap();
        fs::create_dir(dir.path.join("shared")).unwrap();
        fs::write(dir.path.join("shared").join("y.txt"), "y").unwrap();
        let folder = ws.resolve_read(".").unwrap();
        assert!(ws.is_root(&folder));
        assert_eq!(ws.display(&folder), ".");
        assert_eq!(ws.display(&ws.resolve_read("sub").unwrap()), "sub");
        assert_eq!(
            ws.display(&ws.resolve_write("sub/x.txt").unwrap()),
            "sub/x.txt"
        );
        assert_eq!(
            ws.display(&ws.resolve_read("/sandbox/shared/y.txt").unwrap()),
            "/sandbox/shared/y.txt"
        );
        assert_eq!(
            ws.display(&ws.resolve_read("/sandbox/sessions").unwrap()),
            "/sandbox/sessions"
        );
        // A sandbox-prefixed path into the write folder displays relative to it.
        assert_eq!(
            ws.display(&ws.resolve_read("/sandbox/sessions/s1/sub").unwrap()),
            "sub"
        );
        assert!(!ws.is_root(&ws.resolve_read("sub").unwrap()));
    }

    #[cfg(windows)]
    #[test]
    fn open_through_root_junction_is_refused() {
        let dir = TestDir::new();
        let outside = TestDir::new();
        let sessions = dir.path.join("sessions");
        TestDir::junction(&sessions, &outside.path);
        let result = Workspace::open(&dir.sandbox(), Some(Path::new(Fixture::FOLDER)));
        let created = fs::read_dir(&outside.path).unwrap().count();
        // Remove the junction itself so cleanup never follows it.
        let _ = fs::remove_dir(&sessions);
        assert_eq!(result.unwrap_err(), "path escapes the sandbox");
        assert_eq!(created, 0, "no folder created outside");
    }

    #[test]
    fn no_scope_folder_is_the_sandbox_root() {
        let dir = TestDir::new();
        let ws = Workspace::open(&dir.sandbox(), None).unwrap();
        fs::create_dir(dir.path.join("sub")).unwrap();
        fs::write(dir.path.join("sub").join("a.txt"), "a").unwrap();
        let root = ws.resolve_read(".").unwrap();
        assert!(ws.is_root(&root));
        assert_eq!(ws.display(&root), ".");
        assert_eq!(
            ws.display(&ws.resolve_read("sub/a.txt").unwrap()),
            "sub/a.txt"
        );
        assert_eq!(
            ws.display(&ws.resolve_read("/sandbox/sub/a.txt").unwrap()),
            "sub/a.txt"
        );
        assert_eq!(ws.display(&ws.resolve_write("new.txt").unwrap()), "new.txt");
        assert_eq!(
            ws.resolve_read("../x"),
            Err("path escapes the sandbox".into())
        );
        assert!(!dir.path.join("sessions").exists());
    }

    #[test]
    fn open_refuses_folder_outside_the_sandbox() {
        let dir = TestDir::new();
        assert_eq!(
            Workspace::open(&dir.sandbox(), Some(Path::new("../out"))).unwrap_err(),
            "path escapes the sandbox"
        );
    }

    #[test]
    fn remove_folder_removes_and_tolerates_missing() {
        let dir = TestDir::new();
        Fixture::session(&dir);
        fs::write(Fixture::folder(&dir).join("a.txt"), "a").unwrap();
        Workspace::remove_folder(&dir.sandbox(), Path::new(Fixture::FOLDER)).unwrap();
        assert!(!Fixture::folder(&dir).exists());
        assert!(dir.path.join("sessions").is_dir());
        Workspace::remove_folder(&dir.sandbox(), Path::new(Fixture::FOLDER)).unwrap();
        assert!(Workspace::remove_folder(&dir.sandbox(), Path::new("")).is_err());
        assert!(Workspace::remove_folder(&dir.sandbox(), Path::new("../x")).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn remove_folder_does_not_follow_junctions() {
        let dir = TestDir::new();
        let outside = TestDir::new();
        fs::write(outside.path.join("keep.txt"), "keep").unwrap();
        Fixture::session(&dir);
        TestDir::junction(&Fixture::folder(&dir).join("link"), &outside.path);
        Workspace::remove_folder(&dir.sandbox(), Path::new(Fixture::FOLDER)).unwrap();
        assert!(!Fixture::folder(&dir).exists());
        assert!(outside.path.join("keep.txt").is_file());
    }
}
