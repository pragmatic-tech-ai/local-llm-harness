### Task 5: Built-in tools and the sandbox

**Files:**
- Create: `backend/src/runtime/builtin.rs`

**Interfaces:**
- Consumes: Task 4 `Tool`, `ToolSource`, `ToolSpec`, `ToolOutcome`.
- Produces: `Sandbox::new(root: &Path) -> std::io::Result<Self>` (canonicalises the root); `Sandbox::resolve(&self, rel: &str) -> Result<PathBuf, String>`; `CivilTime::utc_iso8601(unix_secs: u64) -> String` (`YYYY-MM-DDTHH:MM:SSZ`, using the days-to-civil algorithm, no crates); `BuiltinSource::new(sandbox: Option<Sandbox>)`, which always provides `get_time` and adds `list_dir` and `read_file` only when it has a sandbox.

Tool shapes (all `side_effects: false`):
- `get_time {}` returns `{"utc": "<iso>", "unix": <secs>}`. UTC only — the spec leaves the zone to the builder.
- `list_dir {path?: string = "."}` returns `{"path", "entries": [{"name", "kind": "file"|"dir"|"other", "size"}]}`, sorted by name, at most 500 entries, plus `"truncated": bool`.
- `read_file {path: string (required), max_bytes?: integer}` returns `{"path", "content", "bytes", "truncated"}`. The cap is `min(max_bytes, 65536)`, default 65536. A truncation that lands mid-character backs off to the last full character. Non-UTF-8 content fails with `file is not valid UTF-8 text`.

`resolve` rules:
- An absolute path, or one with a drive or root prefix, fails with `path must be relative to the sandbox`.
- The path is joined to the root and canonicalised; a missing path fails with `no such path: {rel}`.
- A result that does not start with the canonical root fails with `path escapes the sandbox`. This catches `..` and also symlinks or junctions that point outside.

- [ ] **Step 1: Write failing tests** (use a fresh temp dir per test as the sandbox):

```rust
civil_time_formats_known_instants   // 0 → "1970-01-01T00:00:00Z"; 951782400 → "2000-02-29T00:00:00Z"; 1700000000 → "2023-11-14T22:13:20Z"
get_time_returns_utc_and_unix
list_dir_lists_sorted_entries       // files b.txt, a.txt, dir sub → names ["a.txt","b.txt","sub"], kinds file,file,dir
read_file_returns_text
read_file_truncates_on_char_boundary // file "aé" (3 bytes), max_bytes 2 → content "a", bytes 1, truncated true
read_file_rejects_non_utf8          // bytes [0xff,0xfe] → Err("file is not valid UTF-8 text")
resolve_rejects_parent_escape       // "../x" → Err("path escapes the sandbox") (create the target outside so canonicalize succeeds)
resolve_rejects_absolute            // "C:\\Windows" → Err("path must be relative to the sandbox")
#[cfg(windows)] resolve_rejects_junction_escape // `cmd /c mklink /J <sandbox>\link <outside>` then "link" → Err("path escapes the sandbox")
source_without_sandbox_has_only_get_time
```

- [ ] **Step 2: Run, expecting FAIL:** `scripts/hive-test.sh -- runtime::builtin`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run, expecting PASS.**
- [ ] **Step 5: Commit:** `feat(runtime): sandboxed read-only built-in tools`.

