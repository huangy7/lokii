use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::cache;
use crate::config::{self, AppConfig, ConfigStore};
use serde_json;
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
            match_positions: r
                .match_positions
                .into_iter()
                .map(|(s, e)| FfiMatchRange {
                    start: s as u32,
                    end: e as u32,
                })
                .collect(),
            search_mode: r.search_mode.into(),
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

        std::thread::spawn(move || {
            let phase = "scanning".to_string();
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
        let mode = startup::initialize_index(&self.store, &config, progress, &cache_file);

        done.store(true, Ordering::Relaxed);
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
                startup::verify_index(&store, &verify_config, &cache_file, &cancel);
                // Only update if not cancelled (rebuild took over)
                if !cancel.load(Ordering::Relaxed) {
                    verify_handler.on_index_updated(store.len() as u64);
                }
            });
        }

        match mode {
            startup::StartupMode::HotStart { cached_count } => {
                FfiStartupMode::HotStart {
                    cached_count: cached_count as u64,
                }
            }
            startup::StartupMode::ColdStart { scanned_count } => {
                FfiStartupMode::ColdStart {
                    scanned_count: scanned_count as u64,
                }
            }
        }
    }

    /// Start the file system watcher. Calls event_handler.on_index_updated
    /// whenever the index changes. Non-blocking (spawns a thread internally).
    pub fn start_watcher(&self, event_handler: Arc<dyn EventHandler>) {
        let config = self.config_store.read().clone();
        let handler = Arc::clone(&event_handler);
        let metrics = Arc::clone(&self.metrics);

        if let Err(e) = watcher::start_watcher(self.store.clone(), config.clone(), metrics, move |new_total| {
            handler.on_index_updated(new_total as u64);
        }) {
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
        let fresh = scanner::scan_directories(
            &paths,
            filter,
            config.index.include_hidden,
            progress,
        );

        done.store(true, Ordering::Relaxed);
        self.store.replace(fresh);

        // Save cache
        let cache_file = AppConfig::config_dir().join("index.cache");
        let entries = self.store.entries();
        let _ = cache::save_cache(&entries, &cache_file);

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
        serde_json::to_string_pretty(&*config).unwrap_or_else(|_| {
            serde_json::to_string_pretty(&AppConfig::default()).unwrap()
        })
    }

    /// Get watcher diagnostic metrics as a JSON string.
    pub fn get_metrics(&self) -> String {
        self.metrics.snapshot_json()
    }

    /// Update configuration from a JSON string. Returns true on success.
    /// Writes to TOML file first, then syncs in-memory ConfigStore.
    pub fn update_config_from_json(&self, json: String) -> bool {
        let new_config: AppConfig = match serde_json::from_str(&json) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[Lokii] Failed to parse config JSON: {e}");
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
