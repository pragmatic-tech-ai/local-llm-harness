//! The repository registry: every repository a template can name, and the provider of each.
//!
//! The Pragmatic repositories (`hive`, or the catalog's `[[host]]` names) are fixed at startup
//! and read-only; their models are the catalog's discovery. The OpenRouter repositories are
//! read from `{agent data dir}/repositories.toml`:
//!
//! ```toml
//! [[openrouter]]
//! name = "openrouter"
//! api_key = "sk-or-..."
//! id_prefixes = ["anthropic/", "google/"]
//! ```
//!
//! The file is rewritten atomically on every save and delete. It holds the key in plain text;
//! the key goes nowhere else: [`OpenRouterConfig`]'s `Debug` prints `***`, and summaries carry
//! only `has_key` and `key_hint` (the last 4 characters).
//!
//! Each OpenRouter repository keeps its tools-capable model list in memory and in
//! `{agent data dir}/cache/openrouter-{name}.json`, so a restart without network still knows
//! the models. The list is fetched on a save, on a refresh, and on the first listing of a
//! repository whose list is unknown; a failed fetch keeps the previous list and is reported.
//! `id_prefixes` narrow the list the provider serves and templates validate against.
//!
//! Names are 1 to 64 characters of `A-Z a-z 0-9 _ -`, unique across kinds. A rename or a
//! delete is refused while templates use the repository (`used_by` names them).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::atomic_file::AtomicFile;
use crate::agents::template::{RepositoryKind, RepositoryRules, RuleModel};
use crate::providers::openrouter::{OpenRouterApi, OpenRouterModel, OpenRouterProvider};
use crate::providers::{Provider, ProviderCapabilities, ProviderSource};
use crate::runtime::catalog_source::CatalogSource;

/// One OpenRouter repository as stored in `repositories.toml`. Unknown keys are refused, so a
/// typo (`apikey =`) is reported instead of silently lost on the next save.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRouterConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default)]
    pub id_prefixes: Vec<String>,
}

impl std::fmt::Debug for OpenRouterConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(Self::DEBUG_NAME)
            .field(Self::F_NAME, &self.name)
            .field(Self::F_API_KEY, &self.api_key.as_ref().map(|_| Self::MASK))
            .field(Self::F_ID_PREFIXES, &self.id_prefixes)
            .finish()
    }
}

impl OpenRouterConfig {
    const DEBUG_NAME: &'static str = "OpenRouterConfig";
    const F_NAME: &'static str = "name";
    const F_API_KEY: &'static str = "api_key";
    const F_ID_PREFIXES: &'static str = "id_prefixes";
    const MASK: &'static str = "***";
    const HINT_CHARS: usize = 4;

    /// The last 4 characters of the key; none for a key that short, which would be the whole
    /// key.
    fn key_hint(&self) -> Option<String> {
        let key = self.api_key.as_deref()?;
        let count = key.chars().count();
        (count > Self::HINT_CHARS).then(|| key.chars().skip(count - Self::HINT_CHARS).collect())
    }

    /// Whether `model` passes the id prefixes (every model when there are none).
    fn keeps(&self, model: &OpenRouterModel) -> bool {
        self.id_prefixes.is_empty() || self.id_prefixes.iter().any(|p| model.id.starts_with(p))
    }
}

/// The whole `repositories.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoriesFile {
    #[serde(default)]
    openrouter: Vec<OpenRouterConfig>,
}

/// A fetched model list (tools-capable, before the id prefixes) and when it was fetched; the
/// shape of the cache file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ModelCache {
    fetched_at: u64,
    models: Vec<OpenRouterModel>,
}

/// What one repository's model list is known to be.
#[derive(Debug, Clone, Default)]
struct Listing {
    cache: Option<ModelCache>,
    /// Why the last fetch failed; cleared by a successful one.
    error: Option<String>,
}

/// A `list_repositories` row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepositorySummary {
    pub name: String,
    pub kind: RepositoryKind,
    pub editable: bool,
    pub capabilities: ProviderCapabilities,
    pub model_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_key: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_prefixes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A `list_repository_models` result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepositoryModels {
    pub models: Vec<Value>,
    pub fetched_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Whether this listing fetched the list (so what the repository serves may have
    /// changed). Not on the wire.
    #[serde(skip)]
    pub fetched: bool,
}

/// A `save_repository` result: the saved name, and why the model list could not be fetched.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SaveResult {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetch_error: Option<String>,
}

/// A repository as `list_catalog` offers it to the template editor.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CatalogRepository {
    pub name: String,
    pub kind: RepositoryKind,
    pub capabilities: ProviderCapabilities,
    pub models: Vec<Value>,
}

/// One OpenRouter repository at run time: its config, its provider and its model list.
struct OpenRouterEntry {
    config: OpenRouterConfig,
    api: Arc<OpenRouterApi>,
    provider: Arc<OpenRouterProvider>,
    /// The list after the id prefixes, shared with the provider; `None` while unknown.
    served: Arc<RwLock<Option<Vec<OpenRouterModel>>>>,
    listing: Mutex<Listing>,
}

impl OpenRouterEntry {
    const CACHE_PREFIX: &'static str = "openrouter-";
    const CACHE_EXT: &'static str = ".json";
    const ERR_CACHE_WRITE: &'static str = "model cache not written for repository";

    fn new(config: OpenRouterConfig, base_url: &str, listing: Listing) -> Self {
        let api = Arc::new(OpenRouterApi::new(
            base_url.to_string(),
            config.api_key.clone(),
        ));
        let served = Arc::new(RwLock::new(None));
        let provider = Arc::new(OpenRouterProvider::new(
            config.name.clone(),
            Arc::clone(&api),
            Arc::clone(&served),
        ));
        let entry = OpenRouterEntry {
            config,
            api,
            provider,
            served,
            listing: Mutex::new(Listing::default()),
        };
        entry.set_listing(listing);
        entry
    }

    fn cache_file_name(name: &str) -> String {
        format!("{}{name}{}", Self::CACHE_PREFIX, Self::CACHE_EXT)
    }

    /// The cached list in `cache_dir`, if there is a readable one.
    fn read_cache(cache_dir: &Path, name: &str) -> Option<ModelCache> {
        let text = fs::read_to_string(cache_dir.join(Self::cache_file_name(name))).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn lock_listing(&self) -> MutexGuard<'_, Listing> {
        self.listing.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn listing(&self) -> Listing {
        self.lock_listing().clone()
    }

    /// Stores `listing` and narrows it to what the provider serves.
    fn set_listing(&self, listing: Listing) {
        let served = listing.cache.as_ref().map(|cache| {
            cache
                .models
                .iter()
                .filter(|m| self.config.keeps(m))
                .cloned()
                .collect()
        });
        *self.lock_listing() = listing;
        *self.served.write().unwrap_or_else(|e| e.into_inner()) = served;
    }

    /// The served list; `None` while unknown.
    fn served(&self) -> Option<Vec<OpenRouterModel>> {
        self.served
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Keeps a fetch's outcome: a list is cached in memory and in `cache_dir`; a failure keeps
    /// the previous list and is remembered as the listing's error.
    fn keep_fetched(&self, fetched: &Result<Vec<OpenRouterModel>, String>, cache_dir: &Path) {
        match fetched {
            Ok(models) => {
                let cache = ModelCache {
                    fetched_at: RepositoryRegistry::now(),
                    models: models.clone(),
                };
                if let Err(e) = Self::write_cache(cache_dir, &self.config.name, &cache) {
                    eprintln!("{} {}: {e}", Self::ERR_CACHE_WRITE, self.config.name);
                }
                self.set_listing(Listing {
                    cache: Some(cache),
                    error: None,
                });
            }
            Err(error) => self.lock_listing().error = Some(error.clone()),
        }
    }

    fn write_cache(cache_dir: &Path, name: &str, cache: &ModelCache) -> io::Result<()> {
        fs::create_dir_all(cache_dir)?;
        let bytes = serde_json::to_vec(cache).map_err(io::Error::other)?;
        AtomicFile::write(cache_dir, &Self::cache_file_name(name), &bytes)
    }

    fn summary(&self) -> RepositorySummary {
        let listing = self.listing();
        RepositorySummary {
            name: self.config.name.clone(),
            kind: RepositoryKind::OpenRouter,
            editable: true,
            capabilities: self.provider.capabilities(),
            model_count: self.served().map_or(0, |m| m.len()),
            has_key: Some(self.api.has_key()),
            key_hint: self.config.key_hint(),
            id_prefixes: Some(self.config.id_prefixes.clone()),
            fetched_at: listing.cache.map(|c| c.fetched_at),
            error: listing.error,
        }
    }

    fn models(&self, fetched: bool) -> RepositoryModels {
        let listing = self.listing();
        RepositoryModels {
            models: self.model_values(),
            fetched_at: listing.cache.map(|c| c.fetched_at),
            error: listing.error,
            fetched,
        }
    }

    fn model_values(&self) -> Vec<Value> {
        self.served()
            .unwrap_or_default()
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect()
    }

    fn rules(&self) -> RepositoryRules {
        RepositoryRules {
            name: self.config.name.clone(),
            kind: RepositoryKind::OpenRouter,
            models: self.served().map(|list| {
                list.into_iter()
                    .map(|m| RuleModel {
                        id: m.id,
                        runtimes: vec![OpenRouterProvider::RUNTIME.to_string()],
                        context_length: Some(m.context_length),
                    })
                    .collect()
            }),
        }
    }
}

/// The checked contents of `repositories.toml`, from [`RepositoryRegistry::load`].
#[derive(Debug)]
pub struct LoadedRepositories(Vec<OpenRouterConfig>);

/// Every repository and its provider; the dispatcher's [`ProviderSource`].
pub struct RepositoryRegistry {
    data_dir: PathBuf,
    base_url: String,
    pragmatic: Vec<(String, Arc<dyn Provider>)>,
    discovery: Arc<dyn CatalogSource>,
    /// The OpenRouter repositories in file order. Held while the file is rewritten, so saves
    /// and deletes are serialized.
    openrouter: Mutex<Vec<Arc<OpenRouterEntry>>>,
}

impl RepositoryRegistry {
    pub const FILE_NAME: &'static str = "repositories.toml";
    pub const CACHE_DIR: &'static str = "cache";
    const NAME_MAX: usize = 64;
    const NAME_PUNCTUATION: [char; 2] = ['_', '-'];
    const NAMES_JOIN: &'static str = ", ";
    const ERR_NOT_FOUND: &'static str = "repository not found: ";
    const ERR_USED: &'static str = "repository is used by templates: ";
    const ERR_NAME: &'static str = "repository name must be 1-64 letters, digits, _ or -";
    const ERR_EXISTS: &'static str = "repository already exists: ";
    const ERR_NOT_EDITABLE: &'static str = "repository is not editable: ";
    const ERR_FILE: &'static str = "repositories file: ";
    const ERR_PARSE: &'static str = "does not parse at line ";
    const ERR_CACHE_RENAME: &'static str = "model cache not moved for repository";

    /// Opens the registry over `data_dir`: [`Self::load`] then [`Self::build`], with the
    /// Pragmatic repositories' names taken from `pragmatic`.
    pub fn open(
        data_dir: &Path,
        pragmatic: Vec<(String, Arc<dyn Provider>)>,
        discovery: Arc<dyn CatalogSource>,
        base_url: String,
    ) -> io::Result<Self> {
        let names: Vec<String> = pragmatic.iter().map(|(n, _)| n.clone()).collect();
        let loaded = Self::load(data_dir, &names)?;
        Ok(Self::build(
            data_dir, loaded, pragmatic, discovery, base_url,
        ))
    }

    /// Reads and checks `repositories.toml` in `data_dir` (none when the file is missing)
    /// against the Pragmatic repository names. A file that does not parse, or names a
    /// repository invalidly, twice or like a Pragmatic one, is an `InvalidData` error that
    /// never quotes the file.
    pub fn load(data_dir: &Path, pragmatic_names: &[String]) -> io::Result<LoadedRepositories> {
        let path = data_dir.join(Self::FILE_NAME);
        let file: RepositoriesFile = match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| Self::unparsable(&text, &e))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => RepositoriesFile::default(),
            Err(e) => return Err(e),
        };
        let mut seen: Vec<&str> = Vec::new();
        for config in &file.openrouter {
            Self::check_name(&config.name).map_err(Self::invalid)?;
            if pragmatic_names.contains(&config.name) || seen.contains(&config.name.as_str()) {
                return Err(Self::invalid(format!(
                    "{}{}",
                    Self::ERR_EXISTS,
                    config.name
                )));
            }
            seen.push(&config.name);
        }
        Ok(LoadedRepositories(file.openrouter))
    }

    /// The registry over `loaded` (from [`Self::load`] with these Pragmatic names): the
    /// Pragmatic repositories as given (models from `discovery`) and the OpenRouter ones with
    /// their cached model lists.
    pub fn build(
        data_dir: &Path,
        loaded: LoadedRepositories,
        pragmatic: Vec<(String, Arc<dyn Provider>)>,
        discovery: Arc<dyn CatalogSource>,
        base_url: String,
    ) -> Self {
        let registry = RepositoryRegistry {
            data_dir: data_dir.to_path_buf(),
            base_url,
            pragmatic,
            discovery,
            openrouter: Mutex::new(Vec::new()),
        };
        let cache_dir = registry.cache_dir();
        let entries = loaded
            .0
            .into_iter()
            .map(|config| {
                let listing = Listing {
                    cache: OpenRouterEntry::read_cache(&cache_dir, &config.name),
                    error: None,
                };
                Arc::new(OpenRouterEntry::new(config, &registry.base_url, listing))
            })
            .collect();
        *registry.lock() = entries;
        registry
    }

    /// Fetches `entry`'s list. The outcome is kept (in memory and in the cache file) only if
    /// `entry` is still the live repository of its name, checked under the lock, so a fetch
    /// that a rename or delete overtook writes nothing back. Returns the fetch's error.
    async fn fetch(&self, entry: &Arc<OpenRouterEntry>) -> Option<String> {
        let fetched = entry.api.models().await;
        let entries = self.lock();
        if entries.iter().any(|e| Arc::ptr_eq(e, entry)) {
            entry.keep_fetched(&fetched, &self.cache_dir());
        }
        drop(entries);
        fetched.err()
    }

    /// The parse error of `text`, by line only: toml's own message quotes the offending
    /// line, which may hold a key.
    fn unparsable(text: &str, error: &toml::de::Error) -> io::Error {
        let at = error.span().map_or(0, |span| span.start).min(text.len());
        let line = text.as_bytes()[..at]
            .iter()
            .filter(|b| **b == b'\n')
            .count()
            + 1;
        Self::invalid(format!("{}{line}", Self::ERR_PARSE))
    }

    fn invalid(message: String) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}{message}", Self::ERR_FILE),
        )
    }

    /// Seconds since the Unix epoch.
    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default()
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Arc<OpenRouterEntry>>> {
        self.openrouter.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn cache_dir(&self) -> PathBuf {
        self.data_dir.join(Self::CACHE_DIR)
    }

    fn is_pragmatic(&self, name: &str) -> bool {
        self.pragmatic.iter().any(|(n, _)| n == name)
    }

    fn entry(&self, name: &str) -> Option<Arc<OpenRouterEntry>> {
        self.lock()
            .iter()
            .find(|e| e.config.name == name)
            .map(Arc::clone)
    }

    fn not_found(name: &str) -> String {
        format!("{}{name}", Self::ERR_NOT_FOUND)
    }

    fn check_name(name: &str) -> Result<(), String> {
        let valid = (1..=Self::NAME_MAX).contains(&name.chars().count())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || Self::NAME_PUNCTUATION.contains(&c));
        if valid {
            Ok(())
        } else {
            Err(Self::ERR_NAME.to_string())
        }
    }

    /// Refuses when templates use `name`.
    fn check_unused(users: &[String]) -> Result<(), String> {
        if users.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "{}{}",
                Self::ERR_USED,
                users.join(Self::NAMES_JOIN)
            ))
        }
    }

    /// Rewrites `repositories.toml` with `entries`' configs, atomically.
    fn write_file(&self, configs: Vec<OpenRouterConfig>) -> Result<(), String> {
        let file = RepositoriesFile {
            openrouter: configs,
        };
        let text = toml::to_string(&file).map_err(|e| e.to_string())?;
        fs::create_dir_all(&self.data_dir).map_err(|e| e.to_string())?;
        AtomicFile::write(&self.data_dir, Self::FILE_NAME, text.as_bytes())
            .map_err(|e| e.to_string())
    }

    /// Every repository: the Pragmatic ones first, then the OpenRouter ones in file order.
    pub fn list(&self) -> Vec<RepositorySummary> {
        let discovered = self.discovery.view().models.len();
        let mut rows: Vec<RepositorySummary> = self
            .pragmatic
            .iter()
            .map(|(name, provider)| RepositorySummary {
                name: name.clone(),
                kind: RepositoryKind::Pragmatic,
                editable: false,
                capabilities: provider.capabilities(),
                model_count: discovered,
                has_key: None,
                key_hint: None,
                id_prefixes: None,
                fetched_at: None,
                error: None,
            })
            .collect();
        let entries = self.lock().clone();
        rows.extend(entries.iter().map(|e| e.summary()));
        rows
    }

    /// The repositories with their models, for the template editor.
    pub fn catalog(&self) -> Vec<CatalogRepository> {
        let discovered = self.discovery.view().models;
        let mut repositories: Vec<CatalogRepository> = self
            .pragmatic
            .iter()
            .map(|(name, provider)| CatalogRepository {
                name: name.clone(),
                kind: RepositoryKind::Pragmatic,
                capabilities: provider.capabilities(),
                models: discovered.clone(),
            })
            .collect();
        let entries = self.lock().clone();
        repositories.extend(entries.iter().map(|e| CatalogRepository {
            name: e.config.name.clone(),
            kind: RepositoryKind::OpenRouter,
            capabilities: e.provider.capabilities(),
            models: e.model_values(),
        }));
        repositories
    }

    /// A repository's models. A Pragmatic one lists discovery's models; an OpenRouter one its
    /// served list, fetched first when `refresh` asks or the list is unknown.
    pub async fn models(&self, name: &str, refresh: bool) -> Result<RepositoryModels, String> {
        if self.is_pragmatic(name) {
            return Ok(RepositoryModels {
                models: self.discovery.view().models,
                fetched_at: None,
                error: None,
                fetched: false,
            });
        }
        let entry = self.entry(name).ok_or_else(|| Self::not_found(name))?;
        let fetch = refresh || entry.served().is_none();
        if fetch {
            self.fetch(&entry).await;
        }
        Ok(entry.models(fetch))
    }

    /// Creates or updates an OpenRouter repository, then fetches its model list.
    ///
    /// `previous_name` (else `name`) picks the repository to update; none by that name means
    /// a new one, which a `previous_name` must not ask for. A `name` other than the updated
    /// one's is a rename: refused while `used_by` lists templates of the old name, and it
    /// carries the key, the model list and its cache. An omitted or blank `api_key` keeps the
    /// stored one. Blank prefixes are dropped. The file is written before the fetch, so a
    /// failed fetch still saves; the result then carries `fetch_error`.
    pub async fn save(
        &self,
        name: &str,
        previous_name: Option<&str>,
        id_prefixes: Vec<String>,
        api_key: Option<String>,
        used_by: &(dyn Fn(&str) -> Vec<String> + Sync),
    ) -> Result<SaveResult, String> {
        let entry = self.store(name, previous_name, id_prefixes, api_key, used_by)?;
        let fetch_error = self.fetch(&entry).await;
        Ok(SaveResult {
            name: name.to_string(),
            fetch_error,
        })
    }

    /// The synchronous part of `save`: checks, the file write and the swap in memory.
    fn store(
        &self,
        name: &str,
        previous_name: Option<&str>,
        id_prefixes: Vec<String>,
        api_key: Option<String>,
        used_by: &dyn Fn(&str) -> Vec<String>,
    ) -> Result<Arc<OpenRouterEntry>, String> {
        Self::check_name(name)?;
        let target = previous_name.unwrap_or(name);
        if self.is_pragmatic(target) {
            return Err(format!("{}{target}", Self::ERR_NOT_EDITABLE));
        }
        // Before the lock: `used_by` walks and parses the templates.
        let users = if target != name {
            used_by(target)
        } else {
            Vec::new()
        };
        let mut entries = self.lock();
        let existing = entries.iter().position(|e| e.config.name == target);
        if existing.is_none() && previous_name.is_some() {
            return Err(Self::not_found(target));
        }
        let taken = self.is_pragmatic(name)
            || entries
                .iter()
                .enumerate()
                .any(|(i, e)| e.config.name == name && Some(i) != existing);
        if taken {
            return Err(format!("{}{name}", Self::ERR_EXISTS));
        }
        let renaming = existing.is_some() && target != name;
        if renaming {
            Self::check_unused(&users)?;
        }
        let previous = existing.map(|i| Arc::clone(&entries[i]));
        let api_key = api_key
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .or_else(|| previous.as_ref().and_then(|p| p.config.api_key.clone()));
        let config = OpenRouterConfig {
            name: name.to_string(),
            api_key,
            id_prefixes: id_prefixes
                .into_iter()
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect(),
        };
        let mut configs: Vec<OpenRouterConfig> = entries.iter().map(|e| e.config.clone()).collect();
        match existing {
            Some(i) => configs[i] = config.clone(),
            None => configs.push(config.clone()),
        }
        self.write_file(configs)?;
        let listing = previous.map(|p| Listing {
            cache: p.listing().cache,
            error: None,
        });
        let entry = Arc::new(OpenRouterEntry::new(
            config,
            &self.base_url,
            listing.unwrap_or_default(),
        ));
        match existing {
            Some(i) => entries[i] = Arc::clone(&entry),
            None => entries.push(Arc::clone(&entry)),
        }
        // Under the lock, so no fetch of either name writes its cache in between.
        if renaming {
            self.move_cache(target, name);
        }
        drop(entries);
        Ok(entry)
    }

    /// Moves the cache file of `from` to `to`. No cache is fine; any other failure is logged
    /// (the list stays in memory and is refetched by the next save or refresh).
    fn move_cache(&self, from: &str, to: &str) {
        let cache_dir = self.cache_dir();
        let moved = fs::rename(
            cache_dir.join(OpenRouterEntry::cache_file_name(from)),
            cache_dir.join(OpenRouterEntry::cache_file_name(to)),
        );
        if let Err(e) = moved {
            if e.kind() != io::ErrorKind::NotFound {
                eprintln!("{} {from} -> {to}: {e}", Self::ERR_CACHE_RENAME);
            }
        }
    }

    /// Deletes an OpenRouter repository and its cache, refused while `used_by` lists
    /// templates of it.
    pub fn delete(&self, name: &str, used_by: &dyn Fn(&str) -> Vec<String>) -> Result<(), String> {
        if self.is_pragmatic(name) {
            return Err(format!("{}{name}", Self::ERR_NOT_EDITABLE));
        }
        // Before the lock: `used_by` walks and parses the templates.
        let users = used_by(name);
        let mut entries = self.lock();
        let index = entries
            .iter()
            .position(|e| e.config.name == name)
            .ok_or_else(|| Self::not_found(name))?;
        Self::check_unused(&users)?;
        let configs = entries
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, e)| e.config.clone())
            .collect();
        self.write_file(configs)?;
        entries.remove(index);
        // Under the lock, so a fetch finishing now finds the entry gone and writes nothing.
        let _ = fs::remove_file(
            self.cache_dir()
                .join(OpenRouterEntry::cache_file_name(name)),
        );
        drop(entries);
        Ok(())
    }

    /// The OpenRouter repositories' template rules (the Pragmatic ones come from the
    /// catalog).
    pub fn rules(&self) -> Vec<RepositoryRules> {
        self.lock().iter().map(|e| e.rules()).collect()
    }
}

impl ProviderSource for RepositoryRegistry {
    fn provider(&self, repository: &str) -> Option<Arc<dyn Provider>> {
        if let Some((_, provider)) = self.pragmatic.iter().find(|(n, _)| n == repository) {
            return Some(Arc::clone(provider));
        }
        self.entry(repository)
            .map(|e| Arc::clone(&e.provider) as Arc<dyn Provider>)
    }

    fn providers(&self) -> Vec<Arc<dyn Provider>> {
        let mut providers: Vec<Arc<dyn Provider>> =
            self.pragmatic.iter().map(|(_, p)| Arc::clone(p)).collect();
        providers.extend(
            self.lock()
                .iter()
                .map(|e| Arc::clone(&e.provider) as Arc<dyn Provider>),
        );
        providers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::openrouter::{Canned, FakeHttp};
    use crate::providers::test_support::FakeHost;
    use crate::runtime::catalog_source::CatalogView;
    use crate::runtime::test_support::TempDir;
    use serde_json::json;

    struct Fixtures;

    impl Fixtures {
        const KEY: &'static str = "sk-or-secret-value-7f2a";
        const HINT: &'static str = "7f2a";
        const REPO: &'static str = "or1";
        const OFFLINE: &'static str = "http://127.0.0.1:1";

        /// A `/models` body listing `ids`, all tools-capable, plus one model without tools.
        fn models_body(ids: &[&str]) -> String {
            let mut data: Vec<Value> = ids
                .iter()
                .map(|id| {
                    json!({"id": id, "name": id, "context_length": 65536,
                        "pricing": {"prompt": "0.000001", "completion": "0.000002"},
                        "supported_parameters": ["tools"]})
                })
                .collect();
            data.push(json!({"id": "x/no-tools", "name": "n", "context_length": 1,
                "supported_parameters": []}));
            json!({ "data": data }).to_string()
        }

        fn models(ids: &[&str]) -> Canned {
            Canned::new(200, &[&Self::models_body(ids)])
        }

        fn registry(dir: &Path, base_url: &str) -> RepositoryRegistry {
            let hive = FakeHost::serving("hive", &[("llamacpp", "tiny")]) as Arc<dyn Provider>;
            let discovery = CatalogView {
                runtimes: vec![],
                models: vec![json!({"name": "tiny", "runtimes": ["llamacpp"]})],
                hosts: vec!["hive".into()],
            };
            RepositoryRegistry::open(
                dir,
                vec![("hive".into(), hive)],
                Arc::new(discovery),
                base_url.to_string(),
            )
            .expect("registry")
        }

        fn unused(_: &str) -> Vec<String> {
            Vec::new()
        }

        fn summary(registry: &RepositoryRegistry, name: &str) -> RepositorySummary {
            registry
                .list()
                .into_iter()
                .find(|s| s.name == name)
                .expect("listed")
        }

        fn ids(models: &RepositoryModels) -> Vec<String> {
            models
                .models
                .iter()
                .map(|m| m["id"].as_str().unwrap().to_string())
                .collect()
        }
    }

    #[tokio::test]
    async fn file_round_trips_and_key_never_in_summary() {
        let dir = TempDir::new("repos-roundtrip");
        let (base, _requests) = FakeHttp::serve(vec![Fixtures::models(&["google/a"])]).await;
        let registry = Fixtures::registry(&dir.0, &base);
        let saved = registry
            .save(
                Fixtures::REPO,
                None,
                vec!["google/".into(), " ".into()],
                Some(Fixtures::KEY.into()),
                &Fixtures::unused,
            )
            .await
            .expect("save");
        assert_eq!(
            saved,
            SaveResult {
                name: Fixtures::REPO.into(),
                fetch_error: None
            }
        );
        let text = fs::read_to_string(dir.0.join(RepositoryRegistry::FILE_NAME)).unwrap();
        let file: RepositoriesFile = toml::from_str(&text).unwrap();
        assert_eq!(
            file.openrouter,
            vec![OpenRouterConfig {
                name: Fixtures::REPO.into(),
                api_key: Some(Fixtures::KEY.into()),
                id_prefixes: vec!["google/".into()],
            }]
        );

        let reopened = Fixtures::registry(&dir.0, Fixtures::OFFLINE);
        let rows = reopened.list();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "hive");
        assert_eq!(rows[0].kind, RepositoryKind::Pragmatic);
        assert!(!rows[0].editable);
        assert_eq!(rows[0].model_count, 1);
        assert_eq!(rows[0].capabilities, ProviderCapabilities::FULL);
        let row = &rows[1];
        assert_eq!(row.kind, RepositoryKind::OpenRouter);
        assert!(row.editable);
        assert_eq!(row.has_key, Some(true));
        assert_eq!(row.key_hint.as_deref(), Some(Fixtures::HINT));
        assert_eq!(row.id_prefixes, Some(vec!["google/".to_string()]));
        assert_eq!(row.model_count, 1);
        assert!(row.fetched_at.is_some());
        assert_eq!(row.capabilities, OpenRouterProvider::CAPABILITIES);
        let wire = serde_json::to_string(&rows).unwrap();
        assert!(!wire.contains(Fixtures::KEY), "{wire}");
        let catalog = serde_json::to_string(&reopened.catalog()).unwrap();
        assert!(!catalog.contains(Fixtures::KEY), "{catalog}");
        let provider = reopened.provider(Fixtures::REPO).expect("provider");
        assert_eq!(provider.unavailable(), None);
        assert!(reopened.provider("missing").is_none());
        assert_eq!(reopened.providers().len(), 2);
    }

    #[tokio::test]
    async fn save_fetches_and_reports_fetch_error_but_saves() {
        let dir = TempDir::new("repos-fetcherr");
        let (base, _requests) = FakeHttp::serve(vec![
            Fixtures::models(&["google/a", "google/b"]),
            Canned::new(500, &["{\"error\":{\"message\":\"down\"}}"]),
        ])
        .await;
        let registry = Fixtures::registry(&dir.0, &base);
        let first = registry
            .save(Fixtures::REPO, None, vec![], None, &Fixtures::unused)
            .await
            .expect("save");
        assert_eq!(first.fetch_error, None);
        assert_eq!(Fixtures::summary(&registry, Fixtures::REPO).model_count, 2);
        let second = registry
            .save(
                Fixtures::REPO,
                Some(Fixtures::REPO),
                vec!["google/a".into()],
                None,
                &Fixtures::unused,
            )
            .await
            .expect("saved despite the failed fetch");
        assert_eq!(second.fetch_error.as_deref(), Some("openrouter: 500 down"));
        let row = Fixtures::summary(&registry, Fixtures::REPO);
        assert_eq!(row.error.as_deref(), Some("openrouter: 500 down"));
        // The previous list is kept, narrowed by the new prefixes.
        assert_eq!(row.model_count, 1);
        assert_eq!(row.has_key, Some(false));
        let reopened = Fixtures::registry(&dir.0, Fixtures::OFFLINE);
        assert_eq!(
            Fixtures::summary(&reopened, Fixtures::REPO).id_prefixes,
            Some(vec!["google/a".to_string()])
        );
        let keyless = reopened.provider(Fixtures::REPO).unwrap();
        assert_eq!(
            keyless.unavailable().as_deref(),
            Some("repository or1 has no API key")
        );
    }

    #[tokio::test]
    async fn used_by_runs_outside_the_registry_lock() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = TempDir::new("repos-used-by-unlocked");
        let (base, _requests) = FakeHttp::serve(vec![
            Fixtures::models(&["google/a"]),
            Fixtures::models(&["google/a"]),
        ])
        .await;
        let registry = Fixtures::registry(&dir.0, &base);
        registry
            .save(Fixtures::REPO, None, vec![], None, &Fixtures::unused)
            .await
            .unwrap();
        let unlocked = AtomicBool::new(true);
        let used = |_: &str| {
            if registry.openrouter.try_lock().is_err() {
                unlocked.store(false, Ordering::SeqCst);
            }
            Vec::new()
        };
        registry
            .save("or2", Some(Fixtures::REPO), vec![], None, &used)
            .await
            .unwrap();
        registry.delete("or2", &used).unwrap();
        assert!(unlocked.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn rename_carries_key() {
        let dir = TempDir::new("repos-rename");
        let (base, _requests) = FakeHttp::serve(vec![
            Fixtures::models(&["google/a"]),
            Fixtures::models(&["google/a"]),
        ])
        .await;
        let registry = Fixtures::registry(&dir.0, &base);
        registry
            .save(
                Fixtures::REPO,
                None,
                vec![],
                Some(Fixtures::KEY.into()),
                &Fixtures::unused,
            )
            .await
            .unwrap();
        let used = |name: &str| {
            (name == Fixtures::REPO)
                .then(|| vec!["A".to_string(), "B".to_string()])
                .unwrap_or_default()
        };
        assert_eq!(
            registry
                .save("or2", Some(Fixtures::REPO), vec![], None, &used)
                .await,
            Err("repository is used by templates: A, B".into())
        );
        assert_eq!(
            registry
                .save(
                    "hive",
                    Some(Fixtures::REPO),
                    vec![],
                    None,
                    &Fixtures::unused
                )
                .await,
            Err("repository already exists: hive".into())
        );
        assert_eq!(
            registry
                .save("bad name", None, vec![], None, &Fixtures::unused)
                .await,
            Err(RepositoryRegistry::ERR_NAME.into())
        );
        let renamed = registry
            .save("or2", Some(Fixtures::REPO), vec![], None, &Fixtures::unused)
            .await
            .unwrap();
        assert_eq!(renamed.name, "or2");
        let row = Fixtures::summary(&registry, "or2");
        assert_eq!(row.has_key, Some(true));
        assert_eq!(row.key_hint.as_deref(), Some(Fixtures::HINT));
        assert!(registry.provider(Fixtures::REPO).is_none());
        assert!(registry.provider("or2").is_some());
        let reopened = Fixtures::registry(&dir.0, Fixtures::OFFLINE);
        assert_eq!(
            reopened
                .list()
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["hive", "or2"]
        );
        assert_eq!(Fixtures::summary(&reopened, "or2").model_count, 1);
        assert_eq!(
            reopened.delete("or2", &used),
            Ok(()),
            "templates of the old name do not hold the new one"
        );
        assert_eq!(
            reopened.delete("or2", &Fixtures::unused),
            Err("repository not found: or2".into())
        );
        assert_eq!(
            reopened.delete("hive", &Fixtures::unused),
            Err("repository is not editable: hive".into())
        );
    }

    #[tokio::test]
    async fn id_prefix_filter_applies() {
        let dir = TempDir::new("repos-prefix");
        let (base, _requests) = FakeHttp::serve(vec![Fixtures::models(&[
            "google/a",
            "anthropic/b",
            "google/c",
        ])])
        .await;
        let registry = Fixtures::registry(&dir.0, &base);
        registry
            .save(
                Fixtures::REPO,
                None,
                vec!["google/".into()],
                Some(Fixtures::KEY.into()),
                &Fixtures::unused,
            )
            .await
            .unwrap();
        let models = registry.models(Fixtures::REPO, false).await.unwrap();
        assert_eq!(Fixtures::ids(&models), ["google/a", "google/c"]);
        let rules = registry.rules();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].kind, RepositoryKind::OpenRouter);
        let ids: Vec<&str> = rules[0]
            .models
            .as_ref()
            .unwrap()
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(ids, ["google/a", "google/c"]);
        assert_eq!(
            rules[0].models.as_ref().unwrap()[0].runtimes,
            ["openrouter"]
        );
        assert_eq!(
            rules[0].models.as_ref().unwrap()[0].context_length,
            Some(65536)
        );
        let provider = registry.provider(Fixtures::REPO).unwrap();
        let req = |model: &str| crate::agents::host::ModelRequirement {
            runtime: "openrouter".into(),
            model: model.into(),
            n_ctx: 4096,
            repository: Fixtures::REPO.into(),
        };
        assert!(provider.serves(&req("google/a")));
        assert!(!provider.serves(&req("anthropic/b")));
        let hive = registry.models("hive", true).await.unwrap();
        assert_eq!(
            hive.models,
            [json!({"name": "tiny", "runtimes": ["llamacpp"]})]
        );
        assert_eq!(
            registry.models("missing", false).await,
            Err("repository not found: missing".into())
        );
    }

    #[tokio::test]
    async fn cache_survives_restart_without_network() {
        let dir = TempDir::new("repos-cache");
        let (base, _requests) = FakeHttp::serve(vec![Fixtures::models(&["google/a"])]).await;
        let registry = Fixtures::registry(&dir.0, &base);
        registry
            .save(Fixtures::REPO, None, vec![], None, &Fixtures::unused)
            .await
            .unwrap();
        let before = registry.models(Fixtures::REPO, false).await.unwrap();
        let cache = dir
            .0
            .join(RepositoryRegistry::CACHE_DIR)
            .join("openrouter-or1.json");
        assert!(cache.is_file());
        drop(registry);

        // Offline: a known list is not refetched, and the rules know it.
        let reopened = Fixtures::registry(&dir.0, Fixtures::OFFLINE);
        let after = reopened.models(Fixtures::REPO, false).await.unwrap();
        assert_eq!(after, before);
        assert_eq!(Fixtures::ids(&after), ["google/a"]);
        assert!(after.fetched_at.is_some());
        assert!(reopened.rules()[0].models.is_some());
        reopened.delete(Fixtures::REPO, &Fixtures::unused).unwrap();
        assert!(!cache.exists());
    }

    #[tokio::test]
    async fn a_fetch_overtaken_by_a_delete_writes_no_cache() {
        let dir = TempDir::new("repos-overtaken");
        // The list arrives in slow parts, so the delete lands while it is fetched.
        let body = Fixtures::models_body(&["google/a"]);
        let parts: Vec<String> = body
            .as_bytes()
            .chunks(body.len() / 8 + 1)
            .map(|c| String::from_utf8(c.to_vec()).unwrap())
            .collect();
        let (base, _requests) = FakeHttp::serve(vec![Canned { status: 200, parts }]).await;
        let registry = Fixtures::registry(&dir.0, &base);
        let save = registry.save(Fixtures::REPO, None, vec![], None, &Fixtures::unused);
        let delete = async {
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            registry.delete(Fixtures::REPO, &Fixtures::unused)
        };
        let (saved, deleted) = tokio::join!(save, delete);
        assert_eq!(saved.unwrap().fetch_error, None);
        assert_eq!(deleted, Ok(()));
        assert!(registry.provider(Fixtures::REPO).is_none());
        let cache = dir
            .0
            .join(RepositoryRegistry::CACHE_DIR)
            .join("openrouter-or1.json");
        assert!(!cache.exists(), "the overtaken fetch wrote its cache back");
    }

    #[test]
    fn load_checks_the_file_against_the_pragmatic_names() {
        let dir = TempDir::new("repos-load");
        let hive = vec!["hive".to_string()];
        assert!(RepositoryRegistry::load(&dir.0, &hive)
            .unwrap()
            .0
            .is_empty());
        let file = dir.0.join(RepositoryRegistry::FILE_NAME);
        fs::write(&file, "[[openrouter]]\nname = \"hive\"\n").unwrap();
        assert_eq!(
            RepositoryRegistry::load(&dir.0, &hive)
                .unwrap_err()
                .to_string(),
            "repositories file: repository already exists: hive"
        );
        fs::write(
            &file,
            "[[openrouter]]\nname = \"a\"\n[[openrouter]]\nname = \"a\"\n",
        )
        .unwrap();
        assert_eq!(
            RepositoryRegistry::load(&dir.0, &hive)
                .unwrap_err()
                .to_string(),
            "repositories file: repository already exists: a"
        );
        fs::write(&file, "[[openrouter]]\nname = \"bad name\"\n").unwrap();
        assert_eq!(
            RepositoryRegistry::load(&dir.0, &hive).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn unparsable_file_error_never_quotes_the_key() {
        let dir = TempDir::new("repos-badfile");
        let body = format!(
            "[[openrouter]]\nname = \"or1\"\napi_key = \"{}\" junk\n",
            Fixtures::KEY
        );
        fs::write(dir.0.join(RepositoryRegistry::FILE_NAME), body).unwrap();
        let hive = FakeHost::serving("hive", &[]) as Arc<dyn Provider>;
        let err = RepositoryRegistry::open(
            &dir.0,
            vec![("hive".into(), hive)],
            Arc::new(CatalogView::default()),
            Fixtures::OFFLINE.into(),
        )
        .err()
        .expect("unparsable");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            err.to_string(),
            "repositories file: does not parse at line 3"
        );
    }

    #[test]
    fn unknown_keys_are_refused_by_line() {
        let dir = TempDir::new("repos-unknown-key");
        let hive = vec!["hive".to_string()];
        let file = dir.0.join(RepositoryRegistry::FILE_NAME);
        let typo = format!(
            "[[openrouter]]
name = \"or1\"
apikey = \"{}\"
",
            Fixtures::KEY
        );
        fs::write(&file, typo).unwrap();
        let err = RepositoryRegistry::load(&dir.0, &hive).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            err.to_string(),
            "repositories file: does not parse at line 3"
        );
        fs::write(
            &file,
            "
openroutr = []
",
        )
        .unwrap();
        assert_eq!(
            RepositoryRegistry::load(&dir.0, &hive)
                .unwrap_err()
                .to_string(),
            "repositories file: does not parse at line 2"
        );
    }

    #[test]
    fn debug_redacts_key() {
        let config = OpenRouterConfig {
            name: Fixtures::REPO.into(),
            api_key: Some(Fixtures::KEY.into()),
            id_prefixes: vec!["google/".into()],
        };
        let text = format!("{config:?}");
        assert!(text.contains("***"), "{text}");
        assert!(!text.contains(Fixtures::KEY), "{text}");
        assert!(!text.contains(Fixtures::HINT), "{text}");
        assert_eq!(config.key_hint().as_deref(), Some(Fixtures::HINT));
        let short = OpenRouterConfig {
            api_key: Some("abcd".into()),
            ..config
        };
        assert_eq!(short.key_hint(), None);
    }
}
