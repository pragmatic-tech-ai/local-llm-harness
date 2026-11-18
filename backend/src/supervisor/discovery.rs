//! Scan model directories and report which catalog runtimes can load each model.

use std::path::{Path, PathBuf};

use super::catalog::{Catalog, RuntimeKind};
use super::gguf_meta::{read_gguf_meta, GgufMeta};

/// A model the supervisor can offer. Carries no filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredModel {
    pub display_name: String,
    pub runtimes: Vec<String>,
    pub size_hint: String,
}

/// Architectures the candle engine can run here.
const CANDLE_ARCHITECTURES: [&str; 1] = ["gemma3"];

/// Whether a runtime of `kind` can load a model with this metadata.
pub fn runtime_can_load(kind: RuntimeKind, meta: &GgufMeta, tokenizer_present: bool) -> bool {
    match kind {
        RuntimeKind::Candle => {
            tokenizer_present
                && CANDLE_ARCHITECTURES
                    .iter()
                    .any(|a| a.eq_ignore_ascii_case(&meta.architecture))
        }
        RuntimeKind::Llamacpp | RuntimeKind::Http => true,
    }
}

struct Scanner;

impl Scanner {
    const GGUF_EXT: &'static str = "gguf";
    const TOKENIZER_JSON: &'static str = "tokenizer.json";
    const TOKENIZER_MARK: &'static str = "tokenizer";
    const JSON_EXT: &'static str = "json";
    const UNITS: [&'static str; 5] = ["B", "KB", "MB", "GB", "TB"];

    fn tokenizer_path(dir: &Path) -> Option<PathBuf> {
        let direct = dir.join(Self::TOKENIZER_JSON);
        if direct.is_file() {
            return Some(direct);
        }
        let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_lowercase();
                e.path().extension().is_some_and(|x| x == Self::JSON_EXT)
                    && name.contains(Self::TOKENIZER_MARK)
            })
            .map(|e| e.path())
            .collect();
        found.sort();
        found.into_iter().next()
    }

    fn tokenizer_present(dir: &Path) -> bool {
        Self::tokenizer_path(dir).is_some()
    }

    fn human_size(bytes: u64) -> String {
        let mut value = bytes as f64;
        let mut unit = 0;
        while value >= 1024.0 && unit < Self::UNITS.len() - 1 {
            value /= 1024.0;
            unit += 1;
        }
        if unit == 0 {
            format!("{} {}", bytes, Self::UNITS[0])
        } else {
            format!("{:.1} {}", value, Self::UNITS[unit])
        }
    }

    fn scan_dir(catalog: &Catalog, dir: &Path, out: &mut Vec<DiscoveredModel>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let tokenizer = Self::tokenizer_present(dir);
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .is_some_and(|x| x.eq_ignore_ascii_case(Self::GGUF_EXT))
            })
            .collect();
        paths.sort();
        for path in paths {
            let meta = match read_gguf_meta(&path) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("skipping unreadable model {}: {e}", path.display());
                    continue;
                }
            };
            let Some(stem) = path.file_stem() else {
                continue;
            };
            let runtimes = catalog
                .runtimes
                .iter()
                .filter(|r| runtime_can_load(r.kind, &meta, tokenizer))
                .map(|r| r.name.clone())
                .collect();
            out.push(DiscoveredModel {
                display_name: stem.to_string_lossy().into_owned(),
                runtimes,
                size_hint: Self::human_size(meta.size_bytes),
            });
        }
    }
}

/// Discover models across the catalog's model directories.
pub fn discover(catalog: &Catalog) -> Vec<DiscoveredModel> {
    let mut out = Vec::new();
    for dir in &catalog.model_dirs {
        Scanner::scan_dir(catalog, dir, &mut out);
    }
    out
}

/// A model resolved to real server-side paths. Never sent to clients.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub gguf: PathBuf,
    pub tokenizer: Option<PathBuf>,
    pub meta: GgufMeta,
}

/// Find the `*.gguf` whose file stem equals `display_name` across the catalog's model dirs.
pub fn resolve_model(catalog: &Catalog, display_name: &str) -> Option<ResolvedModel> {
    for dir in &catalog.model_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .is_some_and(|x| x.eq_ignore_ascii_case(Scanner::GGUF_EXT))
                    && p.file_stem().is_some_and(|s| s == display_name)
            })
            .collect();
        paths.sort();
        for gguf in paths {
            if let Ok(meta) = read_gguf_meta(&gguf) {
                return Some(ResolvedModel {
                    tokenizer: Scanner::tokenizer_path(dir),
                    gguf,
                    meta,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::catalog::RuntimeDef;
    use super::*;
    use std::path::PathBuf;

    fn meta(arch: &str) -> GgufMeta {
        GgufMeta {
            architecture: arch.to_string(),
            name: None,
            size_bytes: 0,
        }
    }

    fn rt(name: &str, kind: RuntimeKind) -> RuntimeDef {
        RuntimeDef {
            name: name.to_string(),
            kind,
            exe: PathBuf::from("x"),
            requires_tokenizer: false,
            n_ctx: None,
            n_gpu_layers: None,
            internal_port: None,
            server_exe: None,
        }
    }

    fn catalog(dirs: Vec<PathBuf>) -> Catalog {
        Catalog {
            runtimes: vec![
                rt("candle", RuntimeKind::Candle),
                rt("llamacpp", RuntimeKind::Llamacpp),
                rt("http", RuntimeKind::Http),
            ],
            model_dirs: dirs,
            advertise_host: "h".to_string(),
            worker_port: 1,
        }
    }

    fn gguf(arch: &str) -> Vec<u8> {
        // Minimal valid header: one string KV general.architecture.
        let mut b = b"GGUF".to_vec();
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes());
        b.extend_from_slice(&1u64.to_le_bytes());
        let key = "general.architecture";
        b.extend_from_slice(&(key.len() as u64).to_le_bytes());
        b.extend_from_slice(key.as_bytes());
        b.extend_from_slice(&8u32.to_le_bytes());
        b.extend_from_slice(&(arch.len() as u64).to_le_bytes());
        b.extend_from_slice(arch.as_bytes());
        b
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("disc-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn candle_requires_gemma_arch_and_tokenizer() {
        assert!(runtime_can_load(RuntimeKind::Candle, &meta("gemma3"), true));
        assert!(!runtime_can_load(
            RuntimeKind::Candle,
            &meta("gemma3"),
            false
        ));
        assert!(!runtime_can_load(RuntimeKind::Candle, &meta("llama"), true));
    }

    #[test]
    fn llamacpp_and_http_accept_any_arch() {
        for kind in [RuntimeKind::Llamacpp, RuntimeKind::Http] {
            assert!(runtime_can_load(kind, &meta("llama"), false));
            assert!(runtime_can_load(kind, &meta("qwen2"), true));
        }
    }

    #[test]
    fn discover_lists_models_with_capable_runtimes() {
        let d = dir("list");
        std::fs::write(d.join("gem.gguf"), gguf("gemma3")).unwrap();
        std::fs::write(d.join("lla.gguf"), gguf("llama")).unwrap();
        std::fs::write(d.join("tokenizer.json"), b"{}").unwrap();
        let found = discover(&catalog(vec![d.clone()]));
        assert_eq!(found.len(), 2);
        let gem = found.iter().find(|m| m.display_name == "gem").unwrap();
        assert_eq!(gem.runtimes, vec!["candle", "llamacpp", "http"]);
        let lla = found.iter().find(|m| m.display_name == "lla").unwrap();
        assert_eq!(lla.runtimes, vec!["llamacpp", "http"]);
        let dump = format!("{found:?}");
        assert!(!dump.contains(&*d.to_string_lossy()));
        assert!(!dump.contains('\\') && !dump.contains('/'));
    }

    #[test]
    fn candle_excluded_without_tokenizer() {
        let d = dir("notok");
        std::fs::write(d.join("gem.gguf"), gguf("gemma3")).unwrap();
        let found = discover(&catalog(vec![d]));
        assert_eq!(found[0].runtimes, vec!["llamacpp", "http"]);
    }

    #[test]
    fn display_name_is_file_stem() {
        let d = dir("stem");
        std::fs::write(d.join("my-model.Q4.gguf"), gguf("llama")).unwrap();
        let found = discover(&catalog(vec![d]));
        assert_eq!(found[0].display_name, "my-model.Q4");
    }

    #[test]
    fn uppercase_extension_is_found() {
        let d = dir("upper");
        std::fs::write(d.join("BIG.GGUF"), gguf("llama")).unwrap();
        let found = discover(&catalog(vec![d]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].display_name, "BIG");
    }

    #[test]
    fn malformed_and_missing_dirs_are_skipped() {
        let d = dir("bad");
        std::fs::write(d.join("junk.gguf"), b"not a gguf").unwrap();
        std::fs::write(d.join("ok.gguf"), gguf("llama")).unwrap();
        let missing = d.join("nope");
        let found = discover(&catalog(vec![missing, d]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].display_name, "ok");
    }

    #[test]
    fn resolve_model_returns_real_paths() {
        let d = dir("resolve");
        std::fs::write(d.join("gem.gguf"), gguf("gemma3")).unwrap();
        std::fs::write(d.join("tokenizer.json"), b"{}").unwrap();
        let c = catalog(vec![d.clone()]);
        let r = resolve_model(&c, "gem").unwrap();
        assert_eq!(r.gguf, d.join("gem.gguf"));
        assert_eq!(r.tokenizer, Some(d.join("tokenizer.json")));
        assert_eq!(r.meta.architecture, "gemma3");
        assert!(resolve_model(&c, "nope").is_none());
    }

    #[test]
    fn human_size_formats() {
        assert_eq!(Scanner::human_size(512), "512 B");
        assert_eq!(Scanner::human_size(2_362_232_013), "2.2 GB");
    }
}
