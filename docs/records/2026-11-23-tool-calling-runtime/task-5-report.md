# Task 5 report: built-in tools and sandbox

Built backend/src/runtime/builtin.rs (+ `pub mod builtin;` in mod.rs): Sandbox (new/resolve/display), CivilTime::utc_iso8601, GetTimeTool, ListDirTool, ReadFileTool, BuiltinSource. Blocking fs work runs in tokio spawn_blocking (no new crates, no tokio fs feature).

Decisions: absolute/prefix/root paths rejected; root and target both canonicalised, starts_with on canonical paths; output `path` is sandbox-relative with forward slashes ("." for root). read_file on a directory -> Err "path is a directory, not a file"; list_dir on a file -> Err "path is a file, not a directory" (both tested). Negative/non-integer max_bytes -> Err "max_bytes must be a non-negative integer". Symlinked/other entries listed as kind "other".

Tests (13, all in builtin::tests, TestDir helper struct with Drop cleanup): the 10 from the brief plus list_dir_on_file_errors, read_file_on_directory_errors, resolve_reports_missing_path.

TDD: RED = `scripts/hive-test.sh -- runtime::builtin` with utc_iso8601 temporarily stubbed to return "": 11 passed, 2 failed (civil_time_formats_known_instants, get_time_returns_utc_and_unix). GREEN = same command with real impl: 13 passed, 0 failed (junction test ran and passed on the hive). Whole lib: `scripts/hive-test.sh --` -> 148 passed, 0 failed. `--restore` run afterwards.

Self-review: no free functions, no new crates, LF endings verified, rustfmt applied. Pre-existing uncommitted changes in executor.rs/registry.rs/test_tools.rs were left alone and not committed.

Concerns: none significant. Directory entry sizes use entry metadata (not following symlinks). Junction cleanup in the test removes the link before dropping the dir.

## Fix report (review follow-ups)

1. resolve() normalises lexically first (component stack); `..` above root -> "path escapes the sandbox" without touching disk; then canonicalise + starts_with. Test: nonexistent `../nope`, `a/../../nope`, plus inner `sub/../sub/.` allowed.
2. Unreadable list_dir entries: entries failing enumeration (no name) are counted in `"errors": n` (always present); unreadable metadata gives `size: null`. Documented in a code comment.
3. list_dir non-string `path` -> Err "path must be a string".
4. Message now "path is not a directory".
5. Comment added on deliberate full read before sort/cap.
6. New tests: sibling prefix, windows anchored paths (C:foo, UNC, leading backslash; cfg(windows)), 500-entry cap (501 files), 65536 clamp (default and 1_000_000), max_bytes 0 (decision: empty content, truncated true), invalid max_bytes (-1, "5", 1.5), no absolute path in list_dir/read_file output, stronger get_time (format, ranges, within 5s of now, utc == utc_iso8601(unix)).
7. get_time returns Err when the clock is before the epoch.

Evidence: `scripts/hive-test.sh --` whole lib: 158 passed, 0 failed (TEST_EXIT=0). rustfmt applied, 0 CR bytes, `--restore` run.

Sibling-prefix junction test (resolve_rejects_sibling_prefix_junction_escape, cfg(windows)) added: junction box/link -> box2, resolve("link/f") rejected by the starts_with layer. Hive run of runtime::builtin passed.
