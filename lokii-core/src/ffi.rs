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

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiFileType {
    All,
    Document,
    Code,
    Image,
    Media,
    Archive,
    Folder,
}

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
    /// Where the index service picks up once started: set by initialization.
    resume: Mutex<Option<watcher::Resume>>,
    /// The running index service, once `start_watcher` has been called.
    watcher: Mutex<Option<watcher::WatcherHandle>>,
    /// Prevents concurrent full scans (rebuild vs the service's reconciliation).
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
            resume: Mutex::new(None),
            watcher: Mutex::new(None),
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
        self.finish_initialization(mode, event_handler)
    }
}

impl LokiiCore {
    fn finish_initialization(
        &self,
        mode: startup::StartupMode,
        event_handler: Arc<dyn EventHandler>,
    ) -> FfiStartupMode {
        if let startup::StartupMode::Failed(error) = mode {
            return FfiStartupMode::Failed {
                message: format!("Index initialization failed: {error}"),
            };
        }
        event_handler.on_index_ready(self.store.len() as u64);

        // 先让首屏可用，再在后台把查询侧 CSR/签名/排序表建好；
        // 用户第一次按键通常已经命中预热结果。
        let warm_store = self.store.clone();
        std::thread::spawn(move || warm_store.warm_query_index());

        // 索引服务启动后从这里接上：有检查点就回放追赶，没有就先全量校验。
        *self.resume.lock().unwrap() = Some(match mode {
            startup::StartupMode::HotStart {
                checkpoint: Some(checkpoint),
                ..
            } => watcher::Resume::From(checkpoint.event_id),
            startup::StartupMode::HotStart { .. } => watcher::Resume::Verify,
            startup::StartupMode::ColdStart { event_id, .. } => watcher::Resume::From(event_id),
            startup::StartupMode::Failed(_) => unreachable!("failed startup returned before ready"),
        });

        match mode {
            startup::StartupMode::HotStart { cached_count, .. } => FfiStartupMode::HotStart {
                cached_count: cached_count as u64,
            },
            startup::StartupMode::ColdStart { scanned_count, .. } => FfiStartupMode::ColdStart {
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
        let resume = self
            .resume
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(watcher::Resume::now);
        let handler = Arc::clone(&event_handler);
        let handle = watcher::start_watcher(
            self.store.clone(),
            self.config_store.clone(),
            Arc::clone(&self.metrics),
            resume,
            Arc::clone(&self.scan_lock),
            move |new_total| {
                handler.on_index_updated(new_total as u64);
            },
            |index, checkpoint| {
                let cache_file = AppConfig::config_dir().join("index.cache");
                startup::save_index(index, checkpoint, &cache_file);
            },
        );
        *self.watcher.lock().unwrap() = Some(handle);
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

    /// Start the local query socket used by the `lokii` CLI and external scripts.
    pub fn start_query_server(&self) {
        if let Err(e) = crate::query_server::start(self.store.clone(), self.config_store.clone()) {
            eprintln!("[Lokii] Failed to start query server: {e}");
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
        // Try to acquire scan lock — if the index service is reconciling, skip.
        let guard = match self.scan_lock.try_lock() {
            Ok(g) => g,
            Err(std::sync::TryLockError::WouldBlock) => {
                // A full scan is already running — skip.
                return false;
            }
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        };

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
        let checkpoint = startup::checkpoint(&config);
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
        let _ = cache::save_compact(&fresh, Some(checkpoint), &cache_file);

        // 交给索引服务换入：它会从扫描前的事件 ID 重放，补上扫描期间写入旧索引的变更。
        // 不持着 watcher 锁等待，免得退出时的存盘被卡住。
        let watcher = self.watcher.lock().unwrap().clone();
        let unswapped = match watcher {
            Some(watcher) => watcher.replace(fresh, checkpoint).err(),
            None => Some(fresh),
        };
        if let Some(fresh) = unswapped {
            // 服务已不在：它的检查点不再对应索引，退出时不能拿来存盘。
            *self.watcher.lock().unwrap() = None;
            self.store.replace_compact(fresh);
            let warm_store = self.store.clone();
            std::thread::spawn(move || warm_store.warm_query_index());
        }

        drop(guard);
        event_handler.on_index_ready(self.store.len() as u64);
        true
    }

    /// Save the index with its replay checkpoint. Call before the app quits so
    /// the next launch only replays what changed after this point.
    pub fn persist_index(&self) {
        let Some(watcher) = self.watcher.lock().unwrap().clone() else {
            return;
        };
        // 先锁索引再读检查点：服务换入新索引时检查点先降、指纹后升，这样读到的不会比索引新。
        let index = self.store.read();
        if let Some(checkpoint) = watcher.checkpoint() {
            let cache_file = AppConfig::config_dir().join("index.cache");
            startup::save_index(&index, checkpoint, &cache_file);
        }
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

        // 识别新增的排除目录，就地从当前内存倒排索引中立即剔除条目
        let current_resolved: std::collections::HashSet<String> = current
            .resolve_exclude_paths()
            .into_iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let new_resolved: Vec<String> = new_config
            .resolve_exclude_paths()
            .into_iter()
            .map(|p| p.to_string_lossy().to_string())
            .filter(|p| !current_resolved.contains(p))
            .collect();

        if !new_resolved.is_empty() {
            self.store.exclude_paths(&new_resolved);
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
            resume: Mutex::new(None),
            watcher: Mutex::new(None),
            scan_lock: Arc::new(Mutex::new(())),
            metrics: Arc::new(WatcherMetrics::new()),
        }
    }

    #[test]
    fn initialization_failure_returns_error_without_ready_or_verification() {
        for error in [IndexError::IdOverflow, IndexError::ReferenceOverflow] {
            let core = core();
            let handler = Arc::new(RecordingHandler::default());
            let result =
                core.finish_initialization(startup::StartupMode::Failed(error), handler.clone());
            let FfiStartupMode::Failed { message } = result else {
                panic!("failed initialization must not report a successful startup");
            };
            assert_eq!(message, format!("Index initialization failed: {error}"));
            assert!(handler.events.lock().unwrap().is_empty());
            assert!(core.resume.lock().unwrap().is_none());
            assert!(core.store.is_empty());
        }
    }

    #[test]
    fn empty_cold_start_is_success_and_still_reports_ready() {
        let core = core();
        let handler = Arc::new(RecordingHandler::default());
        let result = core.finish_initialization(
            startup::StartupMode::ColdStart {
                scanned_count: 0,
                event_id: 7,
            },
            handler.clone(),
        );
        assert!(matches!(
            result,
            FfiStartupMode::ColdStart { scanned_count: 0 }
        ));
        assert_eq!(*handler.events.lock().unwrap(), ["ready:0"]);
        assert_eq!(*core.resume.lock().unwrap(), Some(watcher::Resume::From(7)));
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
        assert_eq!(merged.general.terminal_bundle_id, "com.apple.Terminal");
        assert_eq!(merged.index.max_results, 100);
        assert_eq!(merged.index.exclude, vec!["node_modules".to_string()]);
        assert_eq!(
            merged.index.exclude_paths,
            vec!["~/Library".to_string()],
            "未建模的 exclude_paths 必须保留"
        );

        let patch_terminal = r#"{
            "general": { "terminal_bundle_id": "com.googlecode.iterm2" }
        }"#;
        let merged_terminal = merge_config_patch(&base, patch_terminal).expect("patch should merge");
        assert_eq!(merged_terminal.general.terminal_bundle_id, "com.googlecode.iterm2");
        assert_eq!(merged_terminal.general.language, "zh-Hans");
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
