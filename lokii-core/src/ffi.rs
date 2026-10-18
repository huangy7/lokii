use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::cache;
use crate::config::{self, AppConfig, ConfigStore};
use crate::indexer::IndexStore;
use crate::metrics::WatcherMetrics;
use crate::permissions;
use crate::scanner;
use crate::search::{self, AdvancedSearchResult, SearchMode};
use crate::startup;
use crate::watcher;

// ── UniFFI Records ──────────────────────────────────────────────────────

/// Search result exposed to Swift via UniFFI.
#[derive(uniffi::Record)]
pub struct FfiSearchResult {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: u64,
    pub created: u64,
    pub extension: String,
    pub score: f64,
    pub match_positions: Vec<FfiMatchRange>,
    pub parent_match_positions: Vec<FfiMatchRange>,
    pub search_mode: FfiSearchMode,
}

/// A byte range for match highlighting.
#[derive(uniffi::Record)]
pub struct FfiMatchRange {
    pub start: u32,
    pub end: u32,
}

/// Index statistics exposed to Swift.
#[derive(uniffi::Record)]
pub struct FfiIndexStats {
    pub total_count: u64,
    pub is_ready: bool,
}

/// Search response including optional error string.
#[derive(uniffi::Record)]
pub struct FfiSearchResponse {
    pub results: Vec<FfiSearchResult>,
    pub error: Option<String>,
}

// ── UniFFI Enums ────────────────────────────────────────────────────────

#[derive(uniffi::Enum)]
pub enum FfiSearchMode {
    Auto,
    Substring,
    Wildcard,
    Regex,
    Fuzzy,
}

#[derive(uniffi::Enum)]
pub enum FfiStartupMode {
    HotStart { cached_count: u64 },
    ColdStart { scanned_count: u64 },
    Failed { message: String },
}

// ── Callback Interface ──────────────────────────────────────────────────

/// Callback interface for Rust → Swift event notifications.
#[uniffi::export(with_foreign)]
pub trait EventHandler: Send + Sync {
    fn on_index_progress(&self, phase: String, scanned_files: u64);
    fn on_index_ready(&self, total: u64);
    fn on_index_updated(&self, new_total: u64);
    fn on_watcher_error(&self, msg: String);
    fn on_config_reloaded(&self);
}

// ── Conversion helpers ──────────────────────────────────────────────────

impl From<&FfiSearchMode> for SearchMode {
    fn from(m: &FfiSearchMode) -> Self {
        match m {
            FfiSearchMode::Auto => SearchMode::Auto,
            FfiSearchMode::Substring => SearchMode::Substring,
            FfiSearchMode::Wildcard => SearchMode::Wildcard,
            FfiSearchMode::Regex => SearchMode::Regex,
            FfiSearchMode::Fuzzy => SearchMode::Fuzzy,
        }
    }
}

impl From<SearchMode> for FfiSearchMode {
    fn from(m: SearchMode) -> Self {
        match m {
            SearchMode::Auto => FfiSearchMode::Auto,
            SearchMode::Substring => FfiSearchMode::Substring,
            SearchMode::Wildcard => FfiSearchMode::Wildcard,
            SearchMode::Regex => FfiSearchMode::Regex,
            SearchMode::Fuzzy => FfiSearchMode::Fuzzy,
        }
    }
}

fn convert_results(results: Vec<AdvancedSearchResult>) -> Vec<FfiSearchResult> {
    results
        .into_iter()
        .map(|r| FfiSearchResult {
            name: r.entry.name,
            path: r.entry.path,
            is_dir: r.entry.is_dir,
            size: r.entry.size,
            modified: r.entry.modified,
            created: r.entry.created_at,
            extension: r.entry.extension,
            score: r.score,
            match_positions: convert_ranges(r.match_positions),
            parent_match_positions: convert_ranges(r.parent_match_positions),
            search_mode: r.search_mode.into(),
        })
        .collect()
}

/// 把搜索核心的字节区间转换为 FFI 记录形态。
fn convert_ranges(ranges: Vec<(usize, usize)>) -> Vec<FfiMatchRange> {
    ranges
        .into_iter()
        .map(|(s, e)| FfiMatchRange {
            start: s as u32,
            end: e as u32,
        })
        .collect()
}

// ── LokiiCore Object ───────────────────────────────────────────────────

/// Main entry point for the Lokii Rust core, exposed to Swift via UniFFI.
#[derive(uniffi::Object)]
pub struct LokiiCore {
    store: IndexStore,
    config_store: ConfigStore,
    /// Set to true to cancel a running background verification scan.
    cancel_verify: Arc<AtomicBool>,
    /// Prevents concurrent scan operations (rebuild vs verify).
    scan_lock: Arc<Mutex<()>>,
    /// Watcher diagnostics metrics.
    metrics: Arc<WatcherMetrics>,
}

#[uniffi::export]
impl LokiiCore {
    /// Create a new LokiiCore instance. Loads config from disk.
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        let (config, _error) = config::load_or_create_config();
        let config_store = ConfigStore::new(config);
        let store = IndexStore::new();
        Arc::new(Self {
            store,
            config_store,
            cancel_verify: Arc::new(AtomicBool::new(false)),
            scan_lock: Arc::new(Mutex::new(())),
            metrics: Arc::new(WatcherMetrics::new()),
        })
    }

    /// Initialize the file index (hot start from cache or cold start via scan).
    /// Calls event_handler callbacks for progress and completion.
    /// This is blocking — call from a background thread on the Swift side.
    pub fn initialize(&self, event_handler: Arc<dyn EventHandler>) -> FfiStartupMode {
        let progress = Arc::new(AtomicUsize::new(0));
        let cache_file = AppConfig::config_dir().join("index.cache");

        // Spawn progress poller
        let poll_progress = Arc::clone(&progress);
        let poll_handler = Arc::clone(&event_handler);
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let poll_done = Arc::clone(&done);

        let poller = std::thread::spawn(move || {
            let phase = "scanning".to_string();
            while !poll_done.load(Ordering::Relaxed) {
                let count = poll_progress.load(Ordering::Relaxed);
                poll_handler.on_index_progress(phase.clone(), count as u64);
                std::thread::park_timeout(std::time::Duration::from_millis(50));
            }
        });

        let config = self.config_store.read().clone();
        let mode = startup::initialize_index(&self.store, &config, progress, &cache_file);

        done.store(true, Ordering::Relaxed);
        poller.thread().unpark();
        let _ = poller.join();
        self.finish_initialization(mode, config, cache_file, event_handler)
    }
}

impl LokiiCore {
    fn finish_initialization(
        &self,
        mode: startup::StartupMode,
        config: AppConfig,
        cache_file: std::path::PathBuf,
        event_handler: Arc<dyn EventHandler>,
    ) -> FfiStartupMode {
        if let startup::StartupMode::Failed(error) = mode {
            return FfiStartupMode::Failed {
                message: format!("Index initialization failed: {error}"),
            };
        }
        event_handler.on_index_ready(self.store.len() as u64);

        // For hot start, spawn background verification (non-blocking)
        if matches!(mode, startup::StartupMode::HotStart { .. }) {
            self.cancel_verify.store(false, Ordering::Relaxed);
            let cancel = Arc::clone(&self.cancel_verify);
            let store = self.store.clone();
            let verify_config = config.clone();
            let verify_handler = Arc::clone(&event_handler);
            let verify_lock = Arc::clone(&self.scan_lock);
            std::thread::spawn(move || {
                let _guard = verify_lock.lock().unwrap();
                verify_handler.on_index_progress("verifying".to_string(), 0);
                if let Err(error) =
                    startup::verify_index(&store, &verify_config, &cache_file, &cancel)
                {
                    verify_handler.on_watcher_error(format!("Index verification failed: {error}"));
                    return;
                }
                // Only update if not cancelled (rebuild took over)
                if !cancel.load(Ordering::Relaxed) {
                    verify_handler.on_index_updated(store.len() as u64);
                }
            });
        }

        match mode {
            startup::StartupMode::HotStart { cached_count } => FfiStartupMode::HotStart {
                cached_count: cached_count as u64,
            },
            startup::StartupMode::ColdStart { scanned_count } => FfiStartupMode::ColdStart {
                scanned_count: scanned_count as u64,
            },
            startup::StartupMode::Failed(_) => unreachable!("failed startup returned before ready"),
        }
    }
}

#[uniffi::export]
impl LokiiCore {
    /// Start the file system watcher. Calls event_handler.on_index_updated
    /// whenever the index changes. Non-blocking (spawns a thread internally).
    pub fn start_watcher(&self, event_handler: Arc<dyn EventHandler>) {
        let config = self.config_store.read().clone();
        let handler = Arc::clone(&event_handler);
        let metrics = Arc::clone(&self.metrics);

        if let Err(e) = watcher::start_watcher(
            self.store.clone(),
            config.clone(),
            metrics,
            move |new_total| {
                handler.on_index_updated(new_total as u64);
            },
        ) {
            event_handler.on_watcher_error(format!("{e}"));
        }

        // Start reconciliation timer
        let recon_store = self.store.clone();
        let recon_handler = Arc::clone(&event_handler);
        watcher::start_reconciliation_timer(
            recon_store,
            config,
            || {},
            move |new_total| {
                recon_handler.on_index_updated(new_total as u64);
            },
        );
    }

    /// Start the config file watcher for hot-reload.
    pub fn start_config_watcher(&self, event_handler: Arc<dyn EventHandler>) {
        let handler = Arc::clone(&event_handler);

        if let Err(e) = config::start_config_watcher(
            self.config_store.clone(),
            move |_| {
                handler.on_config_reloaded();
            },
            move |msg| {
                eprintln!("{msg}");
            },
        ) {
            eprintln!("Failed to start config watcher: {e}");
        }
    }

    /// Search the index.
    pub fn search(&self, query: String, mode: FfiSearchMode) -> FfiSearchResponse {
        let max_results = self.config_store.read().index.max_results;
        let search_mode = SearchMode::from(&mode);
        let (results, error) = search::dispatch(&self.store, &query, search_mode, max_results);

        FfiSearchResponse {
            results: convert_results(results),
            error,
        }
    }

    /// Get current index statistics.
    pub fn get_stats(&self) -> FfiIndexStats {
        FfiIndexStats {
            total_count: self.store.len() as u64,
            is_ready: !self.store.is_empty(),
        }
    }

    /// Check if a background scan (verify) is currently running.
    pub fn is_scanning(&self) -> bool {
        match self.scan_lock.try_lock() {
            Ok(_guard) => false, // Lock acquired = no scan running
            Err(std::sync::TryLockError::WouldBlock) => true,
            Err(std::sync::TryLockError::Poisoned(_)) => false,
        }
    }

    /// Rebuild the index from scratch. Blocking — call from background thread.
    /// Caller should check `is_scanning()` first; if true, skip the rebuild.
    pub fn rebuild_index(&self, event_handler: Arc<dyn EventHandler>) -> bool {
        // Try to acquire scan lock — if verify is still running, skip.
        let guard = match self.scan_lock.try_lock() {
            Ok(g) => g,
            Err(std::sync::TryLockError::WouldBlock) => {
                // Background verify is already doing a full scan — skip.
                return false;
            }
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        };

        // No concurrent scan running — cancel verify flag (in case it's
        // about to start) and do a full rebuild.
        self.cancel_verify.store(true, Ordering::Relaxed);

        let progress = Arc::new(AtomicUsize::new(0));

        // Spawn progress poller
        let poll_progress = Arc::clone(&progress);
        let poll_handler = Arc::clone(&event_handler);
        let done = Arc::new(AtomicBool::new(false));
        let poll_done = Arc::clone(&done);

        std::thread::spawn(move || {
            let phase = "rebuilding".to_string();
            loop {
                let count = poll_progress.load(Ordering::Relaxed);
                poll_handler.on_index_progress(phase.clone(), count as u64);
                if poll_done.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });

        let config = self.config_store.read().clone();
        let paths = config.resolve_paths();

        let filter = Arc::new(crate::filter::CompiledFilter::from_config(&config));
        let fresh =
            match scanner::scan_compact(&paths, filter, config.index.include_hidden, progress) {
                Ok(fresh) => fresh,
                Err(error) => {
                    done.store(true, Ordering::Relaxed);
                    event_handler.on_watcher_error(format!("Index rebuild failed: {error}"));
                    return false;
                }
            };

        done.store(true, Ordering::Relaxed);

        // Save cache before publishing the index: encoding a multi-million
        // entry index must not happen while holding the store lock.
        let cache_file = AppConfig::config_dir().join("index.cache");
        let _ = cache::save_compact(&fresh, &cache_file);

        self.store.replace_compact(fresh);

        drop(guard);
        event_handler.on_index_ready(self.store.len() as u64);
        true
    }

    /// Check if macOS Full Disk Access is granted.
    pub fn check_fda(&self) -> bool {
        permissions::has_full_disk_access()
    }

    /// Open a file with the default application (macOS `open` command).
    pub fn open_file(&self, path: String) -> bool {
        std::process::Command::new("open")
            .arg(&path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Reveal a file in Finder (select it in its parent directory).
    pub fn reveal_in_finder(&self, path: String) -> bool {
        std::process::Command::new("open")
            .arg("-R")
            .arg(&path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Get the current configuration as a JSON string.
    pub fn get_config_json(&self) -> String {
        let config = self.config_store.read();
        serde_json::to_string_pretty(&*config)
            .unwrap_or_else(|_| serde_json::to_string_pretty(&AppConfig::default()).unwrap())
    }

    /// Get watcher diagnostic metrics as a JSON string.
    pub fn get_metrics(&self) -> String {
        self.metrics.snapshot_json()
    }

    /// Update configuration from a JSON string. Returns true on success.
    /// Writes to TOML file first, then syncs in-memory ConfigStore.
    ///
    /// The payload is treated as a patch over the currently effective config.
    /// The frontend models only a subset of the schema, so keys it omits must
    /// keep their existing values rather than fall back to defaults — otherwise
    /// every preference toggle would silently discard hand-edited settings.
    pub fn update_config_from_json(&self, json: String) -> bool {
        let current = self.config_store.read().clone();
        let new_config = match merge_config_patch(&current, &json) {
            Some(config) => config,
            None => {
                eprintln!("[Lokii] Failed to parse config JSON patch: {json}");
                return false;
            }
        };

        // Write to TOML file
        let config_path = AppConfig::config_dir().join("config.toml");
        let toml_str = match toml::to_string_pretty(&new_config) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[Lokii] Failed to serialize config to TOML: {e}");
                return false;
            }
        };

        if let Err(e) = std::fs::write(&config_path, &toml_str) {
            eprintln!("[Lokii] Failed to write config file: {e}");
            return false;
        }

        // Sync memory directly (don't wait for file watcher)
        self.config_store.update(new_config);
        true
    }
}

/// Overlay a frontend-supplied config patch onto the currently effective config.
///
/// The frontend mirrors only part of the schema (it has no notion of
/// `index.exclude_paths`, for instance). Treating its payload as a complete
/// config and rewriting the file from it resets every unmodelled key to its
/// default, so user-edited values vanish on the next preference toggle.
/// Layering the patch over the current config keeps absent keys intact.
fn merge_config_patch(base: &AppConfig, patch: &str) -> Option<AppConfig> {
    let patch: serde_json::Value = serde_json::from_str(patch).ok()?;
    let base = serde_json::to_value(base).ok()?;
    serde_json::from_value(deep_merge(base, patch)).ok()
}

/// Recursively overlay `patch` onto `base`: objects merge key by key, while
/// arrays and scalars are replaced outright so a list can still be cleared.
fn deep_merge(base: serde_json::Value, patch: serde_json::Value) -> serde_json::Value {
    match (base, patch) {
        (serde_json::Value::Object(mut base_map), serde_json::Value::Object(patch_map)) => {
            for (key, patch_value) in patch_map {
                let merged = match base_map.remove(&key) {
                    Some(base_value) => deep_merge(base_value, patch_value),
                    None => patch_value,
                };
                base_map.insert(key, merged);
            }
            serde_json::Value::Object(base_map)
        }
        (_, patch) => patch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_index::IndexError;

    #[derive(Default)]
    struct RecordingHandler {
        events: Mutex<Vec<String>>,
    }

    impl EventHandler for RecordingHandler {
        fn on_index_progress(&self, phase: String, scanned_files: u64) {
            self.events
                .lock()
                .unwrap()
                .push(format!("{phase}:{scanned_files}"));
        }

        fn on_index_ready(&self, total: u64) {
            self.events.lock().unwrap().push(format!("ready:{total}"));
        }

        fn on_index_updated(&self, new_total: u64) {
            self.events
                .lock()
                .unwrap()
                .push(format!("updated:{new_total}"));
        }

        fn on_watcher_error(&self, msg: String) {
            self.events.lock().unwrap().push(msg);
        }

        fn on_config_reloaded(&self) {}
    }

    fn core() -> LokiiCore {
        LokiiCore {
            store: IndexStore::new(),
            config_store: ConfigStore::new(AppConfig::default()),
            cancel_verify: Arc::new(AtomicBool::new(true)),
            scan_lock: Arc::new(Mutex::new(())),
            metrics: Arc::new(WatcherMetrics::new()),
        }
    }

    #[test]
    fn initialization_failure_returns_error_without_ready_or_verification() {
        for error in [IndexError::IdOverflow, IndexError::ReferenceOverflow] {
            let core = core();
            let handler = Arc::new(RecordingHandler::default());
            let directory = tempfile::tempdir().unwrap();
            let result = core.finish_initialization(
                startup::StartupMode::Failed(error),
                AppConfig::default(),
                directory.path().join("index.cache"),
                handler.clone(),
            );
            let FfiStartupMode::Failed { message } = result else {
                panic!("failed initialization must not report a successful startup");
            };
            assert_eq!(message, format!("Index initialization failed: {error}"));
            assert!(handler.events.lock().unwrap().is_empty());
            assert!(core.cancel_verify.load(Ordering::Relaxed));
            assert!(core.store.is_empty());
        }
    }

    #[test]
    fn empty_cold_start_is_success_and_still_reports_ready() {
        let core = core();
        let handler = Arc::new(RecordingHandler::default());
        let directory = tempfile::tempdir().unwrap();
        let result = core.finish_initialization(
            startup::StartupMode::ColdStart { scanned_count: 0 },
            AppConfig::default(),
            directory.path().join("index.cache"),
            handler.clone(),
        );
        assert!(matches!(
            result,
            FfiStartupMode::ColdStart { scanned_count: 0 }
        ));
        assert_eq!(*handler.events.lock().unwrap(), ["ready:0"]);
    }

    /// 前端下发的配置只覆盖它建模过的字段，未建模字段必须原样保留，
    /// 否则用户手改 config.toml 里的 exclude_paths 会在下次切换偏好设置时被静默重置
    #[test]
    fn config_patch_preserves_keys_the_frontend_does_not_model() {
        let mut base = AppConfig::default();
        base.index.exclude_paths = vec!["~/Library".to_string()];

        let patch = r#"{
            "general": { "language": "en", "show_in_dock": false },
            "index": {
                "index_paths": ["~"],
                "exclude": ["node_modules"],
                "include_hidden": false,
                "max_results": 100
            },
            "window": { "hide_on_blur": true, "remember_position": false }
        }"#;

        let merged = merge_config_patch(&base, patch).expect("patch should merge");

        assert_eq!(merged.general.language, "en");
        assert!(!merged.general.show_in_dock);
        assert_eq!(merged.index.max_results, 100);
        assert_eq!(merged.index.exclude, vec!["node_modules".to_string()]);
        assert_eq!(
            merged.index.exclude_paths,
            vec!["~/Library".to_string()],
            "未建模的 exclude_paths 必须保留"
        );
    }

    /// 显式下发的空数组表示清空，必须能覆盖基底，不能被合并逻辑吞掉
    #[test]
    fn config_patch_can_explicitly_clear_a_list() {
        let base = AppConfig::default();

        let merged = merge_config_patch(&base, r#"{ "index": { "exclude": [] } }"#)
            .expect("patch should merge");

        assert!(merged.index.exclude.is_empty(), "空数组应清空列表");
        assert!(
            !merged.index.exclude_paths.is_empty(),
            "同一节里未被提及的字段不受影响"
        );
    }

    /// 非法补丁必须被拒绝，而不是回退成一份默认配置覆盖用户设置
    #[test]
    fn malformed_config_patch_is_rejected() {
        assert!(merge_config_patch(&AppConfig::default(), "not json").is_none());
        assert!(merge_config_patch(&AppConfig::default(), r#"{ "index": 42 }"#).is_none());
    }
}
