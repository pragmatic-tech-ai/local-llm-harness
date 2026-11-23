//! Crash-safe file writes shared by the agent stores.

use std::fs;
use std::io;
use std::path::Path;

const TMP_EXT: &str = "tmp";

/// Atomic file replacement: write a hidden temp file in the same directory, then rename it
/// over the target. The temp file is removed if either step fails.
pub struct AtomicFile;

impl AtomicFile {
    /// Writes `bytes` to `dir/file_name` atomically.
    pub fn write(dir: &Path, file_name: &str, bytes: &[u8]) -> io::Result<()> {
        let tmp = dir.join(format!(".{file_name}.{TMP_EXT}"));
        let result = fs::write(&tmp, bytes).and_then(|_| fs::rename(&tmp, dir.join(file_name)));
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}
