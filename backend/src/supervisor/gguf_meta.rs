//! Minimal self-contained gguf header reader (architecture + name only).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// Errors reading a gguf header or scanning for models.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("not a gguf file (bad magic)")]
    NotGguf,
    #[error("truncated or unreadable gguf header: {0}")]
    Truncated(#[from] std::io::Error),
    #[error("unsupported gguf version {0}")]
    UnsupportedVersion(u32),
    #[error("gguf header missing general.architecture")]
    MissingArchitecture,
    #[error("gguf header is malformed: {0}")]
    Malformed(&'static str),
}

/// The few header fields the supervisor cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GgufMeta {
    pub architecture: String,
    pub name: Option<String>,
    pub size_bytes: u64,
}

/// Walks a gguf header from any seekable reader.
struct HeaderReader<R: Read + Seek> {
    inner: R,
}

impl<R: Read + Seek> HeaderReader<R> {
    const MAGIC: [u8; 4] = *b"GGUF";
    const MIN_VERSION: u32 = 2;
    const MAX_VERSION: u32 = 3;
    const KEY_ARCH: &'static str = "general.architecture";
    const KEY_NAME: &'static str = "general.name";
    const TYPE_STRING: u32 = 8;
    const TYPE_ARRAY: u32 = 9;
    /// Sanity cap on a captured string so a corrupt length cannot allocate wildly.
    const MAX_STRING: u64 = 1 << 20;
    /// Sanity cap on array nesting depth.
    const MAX_DEPTH: u32 = 8;

    fn new(inner: R) -> Self {
        Self { inner }
    }

    fn read_u32(&mut self) -> Result<u32, DiscoveryError> {
        let mut b = [0u8; 4];
        self.inner.read_exact(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    fn read_u64(&mut self) -> Result<u64, DiscoveryError> {
        let mut b = [0u8; 8];
        self.inner.read_exact(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    fn skip(&mut self, n: u64) -> Result<(), DiscoveryError> {
        let n = i64::try_from(n).map_err(|_| DiscoveryError::Malformed("skip too large"))?;
        self.inner.seek(SeekFrom::Current(n))?;
        Ok(())
    }

    /// Read a string, capturing it (bounded).
    fn read_string(&mut self) -> Result<String, DiscoveryError> {
        let len = self.read_u64()?;
        if len > Self::MAX_STRING {
            return Err(DiscoveryError::Malformed("string too long"));
        }
        let mut buf = vec![0u8; len as usize];
        self.inner.read_exact(&mut buf)?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    fn skip_string(&mut self) -> Result<(), DiscoveryError> {
        let len = self.read_u64()?;
        self.skip(len)
    }

    fn scalar_size(value_type: u32) -> Option<u64> {
        match value_type {
            0 | 1 | 7 => Some(1),
            2 | 3 => Some(2),
            4..=6 => Some(4),
            10..=12 => Some(8),
            _ => None,
        }
    }

    fn skip_value(&mut self, value_type: u32, depth: u32) -> Result<(), DiscoveryError> {
        if let Some(size) = Self::scalar_size(value_type) {
            return self.skip(size);
        }
        match value_type {
            Self::TYPE_STRING => self.skip_string(),
            Self::TYPE_ARRAY => {
                if depth >= Self::MAX_DEPTH {
                    return Err(DiscoveryError::Malformed("array nesting too deep"));
                }
                let elem_type = self.read_u32()?;
                let count = self.read_u64()?;
                if let Some(size) = Self::scalar_size(elem_type) {
                    let total = size
                        .checked_mul(count)
                        .ok_or(DiscoveryError::Malformed("array too large"))?;
                    return self.skip(total);
                }
                for _ in 0..count {
                    self.skip_value(elem_type, depth + 1)?;
                }
                Ok(())
            }
            _ => Err(DiscoveryError::Malformed("unknown value type")),
        }
    }

    fn read_meta(&mut self, size_bytes: u64) -> Result<GgufMeta, DiscoveryError> {
        let mut magic = [0u8; 4];
        self.inner.read_exact(&mut magic)?;
        if magic != Self::MAGIC {
            return Err(DiscoveryError::NotGguf);
        }
        let version = self.read_u32()?;
        if !(Self::MIN_VERSION..=Self::MAX_VERSION).contains(&version) {
            return Err(DiscoveryError::UnsupportedVersion(version));
        }
        let _tensor_count = self.read_u64()?;
        let kv_count = self.read_u64()?;

        let mut architecture = None;
        let mut name = None;
        for _ in 0..kv_count {
            let key = self.read_string()?;
            let value_type = self.read_u32()?;
            let wanted = key == Self::KEY_ARCH || key == Self::KEY_NAME;
            if wanted && value_type == Self::TYPE_STRING {
                let value = self.read_string()?;
                if key == Self::KEY_ARCH {
                    architecture = Some(value);
                } else {
                    name = Some(value);
                }
                if architecture.is_some() && name.is_some() {
                    break;
                }
            } else {
                self.skip_value(value_type, 0)?;
            }
        }
        Ok(GgufMeta {
            architecture: architecture.ok_or(DiscoveryError::MissingArchitecture)?,
            name,
            size_bytes,
        })
    }
}

/// Read the architecture/name from a gguf file header without decoding tensors.
pub fn read_gguf_meta(path: &Path) -> Result<GgufMeta, DiscoveryError> {
    let size_bytes = std::fs::metadata(path)?.len();
    let file = File::open(path)?;
    HeaderReader::new(BufReader::new(file)).read_meta(size_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn gstr(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&(s.len() as u64).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }

    fn header(version: u32, kv_count: u64) -> Vec<u8> {
        let mut b = b"GGUF".to_vec();
        b.extend_from_slice(&version.to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes());
        b.extend_from_slice(&kv_count.to_le_bytes());
        b
    }

    fn sample_gguf(arch: &str) -> Vec<u8> {
        let mut b = header(3, 4);
        // uint32 first, to be skipped
        gstr(&mut b, "general.alignment");
        b.extend_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(&32u32.to_le_bytes());
        // array of strings, to be skipped
        gstr(&mut b, "tokenizer.ggml.tokens");
        b.extend_from_slice(&9u32.to_le_bytes());
        b.extend_from_slice(&8u32.to_le_bytes());
        b.extend_from_slice(&2u64.to_le_bytes());
        gstr(&mut b, "ab");
        gstr(&mut b, "cde");
        // architecture
        gstr(&mut b, "general.architecture");
        b.extend_from_slice(&8u32.to_le_bytes());
        gstr(&mut b, arch);
        // name
        gstr(&mut b, "general.name");
        b.extend_from_slice(&8u32.to_le_bytes());
        gstr(&mut b, "My Model");
        b
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gguf-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        File::create(&p).unwrap().write_all(bytes).unwrap();
        p
    }

    #[test]
    fn reads_architecture_from_header() {
        let dir = temp_dir("arch");
        let bytes = sample_gguf("gemma3");
        let p = write(&dir, "m.gguf", &bytes);
        let meta = read_gguf_meta(&p).unwrap();
        assert_eq!(meta.architecture, "gemma3");
        assert_eq!(meta.name.as_deref(), Some("My Model"));
        assert_eq!(meta.size_bytes, bytes.len() as u64);
    }

    #[test]
    fn non_gguf_is_error_not_panic() {
        let dir = temp_dir("bad");
        let p = write(&dir, "x.gguf", b"this is not a gguf file at all");
        assert!(matches!(read_gguf_meta(&p), Err(DiscoveryError::NotGguf)));
    }

    #[test]
    fn truncated_is_error() {
        let dir = temp_dir("trunc");
        let mut bytes = sample_gguf("gemma3");
        bytes.truncate(bytes.len() - 20);
        let p = write(&dir, "t.gguf", &bytes);
        assert!(read_gguf_meta(&p).is_err());
        let p2 = write(&dir, "e.gguf", b"GG");
        assert!(read_gguf_meta(&p2).is_err());
    }

    #[test]
    fn unsupported_version_is_error() {
        let dir = temp_dir("ver");
        let p = write(&dir, "v.gguf", &header(1, 0));
        assert!(matches!(
            read_gguf_meta(&p),
            Err(DiscoveryError::UnsupportedVersion(1))
        ));
    }
}
