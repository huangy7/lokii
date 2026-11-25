use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::cache;
use crate::config::{self, AppConfig, ConfigStore};
use crate::indexer::IndexStore;
use crate::metrics::WatcherMetrics;
use crate::path_utils::path_starts_with_ci;
use crate::permissions;
use crate::scanner;
use crate::search::{self, MultiSearchResult, SearchMode};
use crate::startup;
use crate::volume_manager::{VolumeCoordinator, VolumeState, VolumeStatusInfo};
use crate::volumes::{self, VolumeKind};
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
    pub is_offline: bool,
    pub volume_name: Option<String>,
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

/// Search response including optional error string and case sensitivity flag.
#[derive(uniffi::Record)]
pub struct FfiSearchResponse {
    pub results: Vec<FfiSearchResult>,
    pub error: Option<String>,
    pub is_case_sensitive: bool,
    pub effective_mode: FfiSearchMode,
    /// 结果为空且查询路径命中排除规则时的命中片段，供界面解释「为什么搜不到」。
    pub hint: Option<String>,
}

/// Status snapshot for a configured mounted volume slot.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiVolumeStatus {
    pub volume_id: String,
    pub configured_path: String,
    pub active_path: Option<String>,
    pub display_name: String,
    pub kind: String,
    pub state: FfiVolumeState,
    pub entry_count: u64,
    pub last_sync_at: Option<u64>,
    pub is_remounted: bool,
}

/// Real-time query semantic preview flags for frontend toolbar badges.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FfiQueryPreview {
    pub has_effective_uppercase: bool,
    pub has_regex_sugar: bool,
}

/// Typed general settings exposed via UniFFI for compile-time safe defaults.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiGeneralConfig {
    pub language: String,
    pub show_in_dock: bool,
    pub terminal_bundle_id: String,
    pub hide_results_when_empty: bool,
}

/// Typed index settings exposed via UniFFI for compile-time safe defaults.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiIndexConfig {
    pub index_paths: Vec<String>,
    pub index_apps: bool,
    pub exclude: Vec<String>,
    pub exclude_paths: Vec<String>,
    pub include_hidden: bool,
    pub exclude_sensitive: bool,
    pub sensitive_names: Vec<String>,
    pub max_results: u64,
    pub network_rescan_minutes: u64,
}

/// Typed window settings exposed via UniFFI for compile-time safe defaults.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiWindowConfig {
    pub hide_on_blur: bool,
    pub remember_position: bool,
}

/// Typed application configuration snapshot exposed via UniFFI.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAppConfig {
    pub general: FfiGeneralConfig,
    pub index: FfiIndexConfig,
    pub window: FfiWindowConfig,
}

// ── UniFFI Enums & Errors ───────────────────────────────────────────────

/// Errors surfaced across the UniFFI boundary.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Error)]
pub enum FfiError {
    IoError { message: String },
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FfiError::IoError { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for FfiError {}

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

#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq)]
pub enum FfiCaseSensitivity {
    Auto,
    Sensitive,
    Insensitive,
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiSearchMode {
    Auto,
    Substring,
    Wildcard,
    Regex,
    Fuzzy,
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiVolumeState {
    Online,
    Syncing,
    Offline,
    Missing,
}

#[derive(uniffi::Enum)]
pub enum FfiStartupMode {
    HotStart { cached_count: u64 },
    ColdStart { scanned_count: u64 },
    Failed { message: String },
}

// ── Callback Interface ──────────────────────────────────────────────────

/// Callback interface for Rust -> Swift event notifications.
#[uniffi::export(with_foreign)]
pub trait EventHandler: Send + Sync {
    fn on_index_progress(&self, phase: String, scanned_files: u64);
    fn on_index_ready(&self, total: u64);
    fn on_index_updated(&self, new_total: u64);
    fn on_watcher_error(&self, msg: String);
    fn on_config_reloaded(&self);
    fn on_volume_statuses_changed(&self);
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

impl From<FfiCaseSensitivity> for search::CaseSensitivity {
    fn from(mode: FfiCaseSensitivity) -> Self {
        match mode {
            FfiCaseSensitivity::Auto => search::CaseSensitivity::Auto,
            FfiCaseSensitivity::Sensitive => search::CaseSensitivity::Sensitive,
            FfiCaseSensitivity::Insensitive => search::CaseSensitivity::Insensitive,
        }
    }
}

impl From<VolumeState> for FfiVolumeState {
    fn from(state: VolumeState) -> Self {
        match state {
            VolumeState::Online => FfiVolumeState::Online,
            VolumeState::Syncing => FfiVolumeState::Syncing,
            VolumeState::Offline => FfiVolumeState::Offline,
            VolumeState::Missing => FfiVolumeState::Missing,
        }
    }
}

fn volume_kind_str(kind: VolumeKind) -> String {
    match kind {
        VolumeKind::LocalExternal => "local_external".to_string(),
        VolumeKind::Network => "network".to_string(),
    }
}

impl From<VolumeStatusInfo> for FfiVolumeStatus {
    fn from(info: VolumeStatusInfo) -> Self {
        Self {
            volume_id: info.volume_id,
            configured_path: info.configured_path,
            active_path: info.active_path,
            display_name: info.display_name,
            kind: volume_kind_str(info.kind),
            state: info.state.into(),
            entry_count: info.entry_count,
            last_sync_at: info.last_sync_at,
            is_remounted: info.is_remounted,
        }
    }
}

fn convert_multi_results(results: Vec<MultiSearchResult>) -> Vec<FfiSearchResult> {
    results
        .into_iter()
        .map(|item| FfiSearchResult {
            name: item.result.entry.name,
            path: item.result.entry.path,
            is_dir: item.result.entry.is_dir,
            size: item.result.entry.size,
            modified: item.result.entry.modified,
            created: item.result.entry.created_at,
            extension: item.result.entry.extension,
            score: item.result.score,
            match_positions: convert_ranges(item.result.match_positions),
            parent_match_positions: convert_ranges(item.result.parent_match_positions),
            search_mode: item.result.search_mode.into(),
            is_offline: item.is_offline,
            volume_name: item.volume_name,
        })
        .collect()
}

/// Convert search byte ranges to FFI match range records.
fn convert_ranges(ranges: Vec<(usize, usize)>) -> Vec<FfiMatchRange> {
    ranges
        .into_iter()
        .map(|(s, e)| FfiMatchRange {
            start: s as u32,
            end: e as u32,
        })
        .collect()
}

const MAX_LOCAL_SYMLINK_HOPS: usize = 40;
const MAX_VOLUME_GUARD_HOPS: usize = 8;

#[derive(Clone, Debug)]
enum LexComp {
    Root,
    Parent,
    Normal(OsString),
}

fn push_path_components_front(queue: &mut VecDeque<LexComp>, path: &Path) {
    let mut comps = Vec::new();
    for c in path.components() {
        match c {
            Component::RootDir => comps.push(LexComp::Root),
            Component::ParentDir => comps.push(LexComp::Parent),
            Component::Normal(s) => comps.push(LexComp::Normal(s.to_os_string())),
            Component::CurDir | Component::Prefix(_) => {}
        }
    }
    for c in comps.into_iter().rev() {
        queue.push_front(c);
    }
}

/// Normalize `.` and `..` lexically without querying the filesystem.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(
                    out.components().next_back(),
                    None | Some(Component::RootDir) | Some(Component::ParentDir)
                ) {
                    out.pop();
                } else if !out.has_root() {
                    out.push(comp.as_os_str());
                }
            }
            Component::Normal(s) => out.push(s),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

fn assemble_normalized(prefix: &Path, remaining: &VecDeque<LexComp>) -> PathBuf {
    let mut combined = prefix.to_path_buf();
    for comp in remaining {
        match comp {
            LexComp::Root => combined = PathBuf::from("/"),
            LexComp::Parent => {
                combined.push("..");
            }
            LexComp::Normal(s) => combined.push(s),
        }
    }
    normalize_lexical(&combined)
}

/// Extract `(<first_component>, <remaining_subpath>)` if `path` lies inside `/Volumes/<entry>`.
fn volumes_first_component(path: &Path) -> Option<(&std::ffi::OsStr, PathBuf)> {
    let mut comps = path.components();
    if !matches!(comps.next(), Some(Component::RootDir)) {
        return None;
    }
    let Some(Component::Normal(volumes_dir)) = comps.next() else {
        return None;
    };
    if !volumes_dir.to_string_lossy().eq_ignore_ascii_case("Volumes") {
        return None;
    }
    let Some(Component::Normal(first_entry)) = comps.next() else {
        return None;
    };
    let mut rest = PathBuf::new();
    for comp in comps {
        rest.push(comp.as_os_str());
    }
    Some((first_entry, rest))
}

fn find_matching_slot<'a>(
    candidate: &Path,
    statuses: &'a [VolumeStatusInfo],
) -> Option<&'a VolumeStatusInfo> {
    let mut best_match: Option<(usize, &VolumeStatusInfo)> = None;
    for status in statuses {
        for slot_path in [
            Some(status.configured_path.as_str()),
            status.active_path.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if path_starts_with_ci(candidate, Path::new(slot_path)) {
                let depth = slot_path.len();
                if best_match.is_none_or(|(best_len, _)| depth > best_len) {
                    best_match = Some((depth, status));
                }
            }
        }
    }
    best_match.map(|(_, status)| status)
}

fn matches_live_non_system_mount(
    candidate: &Path,
    live_mounts: &[volumes::MountedFsInfo],
) -> bool {
    live_mounts.iter().any(|m| {
        !volumes::is_system_mount_point(&m.mount_point)
            && path_starts_with_ci(candidate, Path::new(&m.mount_point))
    })
}

fn has_volume_or_slot_boundary(
    candidate: &Path,
    statuses: &[VolumeStatusInfo],
    live_mounts: &[volumes::MountedFsInfo],
) -> bool {
    find_matching_slot(candidate, statuses).is_some()
        || matches_live_non_system_mount(candidate, live_mounts)
        || volumes_first_component(candidate).is_some()
}

/// Inspect a top-level `/Volumes/<entry>` entry only when neither configured slots nor kernel
/// mounts match it, so macOS boot volume symlinks (e.g. `/Volumes/Macintosh HD -> /`) can be
/// resolved without ever calling `lstat`/`stat` on mounted or hung network mountpoints.
fn read_volumes_boot_symlink(
    top_entry: &Path,
    live_mounts: &[volumes::MountedFsInfo],
) -> Option<PathBuf> {
    if matches_live_non_system_mount(top_entry, live_mounts) {
        return None;
    }
    let meta = std::fs::symlink_metadata(top_entry).ok()?;
    if !meta.file_type().is_symlink() {
        return None;
    }
    std::fs::read_link(top_entry).ok()
}

enum VolumeGuard {
    Allow,
    Deny(String),
    FollowBootSymlink(PathBuf),
    LocalPath,
}

fn check_volume_guard_with(
    candidate: &Path,
    statuses: &[VolumeStatusInfo],
    live_mounts: &[volumes::MountedFsInfo],
    read_boot_symlink: &impl Fn(&Path) -> Option<PathBuf>,
) -> VolumeGuard {
    if let Some(status) = find_matching_slot(candidate, statuses) {
        if matches!(status.state, VolumeState::Online | VolumeState::Syncing) {
            return VolumeGuard::Allow;
        }
        return VolumeGuard::Deny(format!("Volume '{}' is offline", status.display_name));
    }

    if matches_live_non_system_mount(candidate, live_mounts) {
        return VolumeGuard::Allow;
    }

    if let Some((first_entry, rest)) = volumes_first_component(candidate) {
        let top_entry = Path::new("/Volumes").join(first_entry);
        if let Some(target) = read_boot_symlink(&top_entry) {
            let base = if target.is_absolute() {
                target
            } else {
                Path::new("/Volumes").join(target)
            };
            let remapped = normalize_lexical(&base.join(rest));
            return VolumeGuard::FollowBootSymlink(remapped);
        }
        return VolumeGuard::Deny(format!(
            "Volume for path '{}' is not mounted",
            candidate.to_string_lossy()
        ));
    }

    VolumeGuard::LocalPath
}

/// Walk local path components safely to detect symlinks targeting offline or unmounted
/// volumes, stopping immediately before querying metadata on any volume boundary.
fn resolve_local_symlinks_safely(
    normalized: &Path,
    statuses: &[VolumeStatusInfo],
    live_mounts: &[volumes::MountedFsInfo],
) -> Result<PathBuf, FfiError> {
    let mut queue = VecDeque::new();
    push_path_components_front(&mut queue, normalized);
    let mut resolved = PathBuf::new();
    let mut symlink_hops = 0usize;

    while let Some(comp) = queue.pop_front() {
        match comp {
            LexComp::Root => {
                resolved = PathBuf::from("/");
                continue;
            }
            LexComp::Parent => {
                if !matches!(
                    resolved.components().next_back(),
                    None | Some(Component::RootDir) | Some(Component::ParentDir)
                ) {
                    resolved.pop();
                } else if !resolved.has_root() {
                    resolved.push("..");
                }
                continue;
            }
            LexComp::Normal(seg) => {
                resolved.push(&seg);
            }
        }

        if has_volume_or_slot_boundary(&resolved, statuses, live_mounts) {
            return Ok(assemble_normalized(&resolved, &queue));
        }

        match std::fs::symlink_metadata(&resolved) {
            Ok(meta) if meta.file_type().is_symlink() => {
                symlink_hops += 1;
                if symlink_hops > MAX_LOCAL_SYMLINK_HOPS {
                    return Err(FfiError::IoError {
                        message: format!(
                            "Too many symbolic links while resolving '{}'",
                            normalized.to_string_lossy()
                        ),
                    });
                }
                if let Ok(target) = std::fs::read_link(&resolved) {
                    if target.is_absolute() {
                        resolved.clear();
                    } else {
                        resolved.pop();
                    }
                    push_path_components_front(&mut queue, &target);
                    let rewritten = assemble_normalized(&resolved, &queue);
                    if has_volume_or_slot_boundary(&rewritten, statuses, live_mounts) {
                        return Ok(rewritten);
                    }
                }
            }
            Ok(_) => {}
            Err(_) => {
                return Ok(assemble_normalized(&resolved, &queue));
            }
        }
    }

    Ok(normalize_lexical(&resolved))
}

fn ensure_path_accessible_with(
    path: &str,
    statuses: &[VolumeStatusInfo],
    live_mounts: &[volumes::MountedFsInfo],
    read_boot_symlink: &impl Fn(&Path) -> Option<PathBuf>,
) -> Result<(), FfiError> {
    let mut current = normalize_lexical(Path::new(path));

    for _hop in 0..MAX_VOLUME_GUARD_HOPS {
        match check_volume_guard_with(&current, statuses, live_mounts, read_boot_symlink) {
            VolumeGuard::Allow => return Ok(()),
            VolumeGuard::Deny(message) => return Err(FfiError::IoError { message }),
            VolumeGuard::FollowBootSymlink(remapped) => {
                current = remapped;
            }
            VolumeGuard::LocalPath => {
                let resolved = resolve_local_symlinks_safely(&current, statuses, live_mounts)?;
                if resolved == current {
                    return Ok(());
                }
                current = resolved;
            }
        }
    }

    Err(FfiError::IoError {
        message: format!("Too many symlink redirects while checking path '{path}'"),
    })
}

/// RAII progress polling guard that emits live scan counts every 50ms and cleanly
/// joins its background thread when the scan completes or unwinds.
struct ProgressPoller {
    done: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ProgressPoller {
    fn start(
        phase: &'static str,
        progress: Arc<AtomicUsize>,
        event_handler: Arc<dyn EventHandler>,
    ) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let poll_done = Arc::clone(&done);
        let handle = std::thread::spawn(move || {
            while !poll_done.load(Ordering::Relaxed) {
                let count = progress.load(Ordering::Relaxed);
                event_handler.on_index_progress(phase.to_string(), count as u64);
                std::thread::park_timeout(std::time::Duration::from_millis(50));
            }
        });
        Self {
            done,
            handle: Some(handle),
        }
    }

    fn finish(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            handle.thread().unpark();
            let _ = handle.join();
        }
    }
}

impl Drop for ProgressPoller {
    fn drop(&mut self) {
        self.finish();
    }
}

// ── LokiiCore Object ───────────────────────────────────────────────────

/// Main entry point for the Lokii Rust core, exposed to Swift via UniFFI.
#[derive(uniffi::Object)]
pub struct LokiiCore {
    store: IndexStore,
    config_store: ConfigStore,
    volumes: Arc<VolumeCoordinator>,
    cache_dir: PathBuf,
    /// Where the index service picks up once started: set by initialization.
    resume: Mutex<Option<watcher::Resume>>,
    /// The running index service, once `start_watcher` has been called.
    watcher: Mutex<Option<watcher::WatcherHandle>>,
    /// The running config file watcher, once `start_config_watcher` has been called.
    config_watcher: Mutex<Option<config::ConfigWatcherHandle>>,
    /// Prevents concurrent full scans (rebuild vs the service's reconciliation).
    scan_lock: Arc<Mutex<()>>,
    /// Serializes `update_config_from_json` read-merge-persist-rollback transactions.
    config_update_lock: Mutex<()>,
    /// Watcher diagnostics metrics.
    metrics: Arc<WatcherMetrics>,
    /// True once initial startup has completed and `on_index_ready` has been emitted.
    initialized: Arc<AtomicBool>,
    /// 启动读取 `config.toml` 时捕获的解析/读取错误，待 `initialize` 拿到 `EventHandler` 后上报。
    startup_config_error: Mutex<Option<String>>,
}

#[uniffi::export]
impl LokiiCore {
    /// Create a new LokiiCore instance. Loads config from disk.
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        let (config, config_error) = config::load_or_create_config();
        if let Some(ref error) = config_error {
            eprintln!("[Lokii] {error}");
        }
        let cache_dir = AppConfig::config_dir();
        cache::cleanup_stale_temp_files(&cache_dir);
        cache::cleanup_stale_temp_files(&cache_dir.join("volumes"));
        let config_store = ConfigStore::new(config);
        let store = IndexStore::new();
        let volumes = Arc::new(VolumeCoordinator::new(cache_dir.clone()));
        Arc::new(Self {
            store,
            config_store,
            volumes,
            cache_dir,
            resume: Mutex::new(None),
            watcher: Mutex::new(None),
            config_watcher: Mutex::new(None),
            scan_lock: Arc::new(Mutex::new(())),
            config_update_lock: Mutex::new(()),
            metrics: Arc::new(WatcherMetrics::new()),
            initialized: Arc::new(AtomicBool::new(false)),
            startup_config_error: Mutex::new(config_error),
        })
    }

    /// Peek whether startup needs a cold scan (true when neither primary cache nor
    /// any configured volume cache exists on disk). Uses zero blocking stat calls on volumes.
    pub fn peek_startup_is_cold_start(&self) -> bool {
        let config = self.config_store.read().clone();
        !self.volumes.peek_has_any_cache(&config)
    }

    /// Initialize the file index (hot start from cache or cold start via scan) and
    /// reconcile configured mounted volume slots.
    /// This is blocking — call from a background thread on the Swift side.
    pub fn initialize(&self, event_handler: Arc<dyn EventHandler>) -> FfiStartupMode {
        self.initialized.store(false, Ordering::SeqCst);
        if let Some(err) = self
            .startup_config_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            event_handler.on_watcher_error(err);
        }
        let config = self.config_store.read().clone();
        let cache_file = self.cache_dir.join("index.cache");
        let volumes_dir = self.cache_dir.join("volumes");
        let (primary_roots, excluded_mount_roots) =
            watcher::primary_scope_inputs(&config, &volumes_dir);

        let progress = Arc::new(AtomicUsize::new(0));
        let poller = (!primary_roots.is_empty() && !cache_file.exists()).then(|| {
            ProgressPoller::start(
                "scanning",
                Arc::clone(&progress),
                Arc::clone(&event_handler),
            )
        });

        let mut on_progress = |phase: &str, count: u64| {
            event_handler.on_index_progress(phase.to_string(), count);
        };

        let (mode, cold_checkpoint) = match startup::initialize_primary_index_deferred_persist(
            &self.store,
            &config,
            &primary_roots,
            &excluded_mount_roots,
            progress,
            &mut on_progress,
            &cache_file,
        ) {
            Ok(res) => res,
            Err(startup::InitError::Index(error)) => (startup::StartupMode::Failed(error), None),
            Err(startup::InitError::Io(message)) => {
                drop(poller);
                return FfiStartupMode::Failed {
                    message: format!("Index initialization failed: {message}"),
                };
            }
        };
        drop(poller);

        self.finish_initialization(mode, cold_checkpoint, event_handler)
    }
}

impl LokiiCore {
    #[cfg(test)]
    fn for_test(
        cache_dir: PathBuf,
        config: AppConfig,
        mount_lister: Arc<dyn Fn() -> Vec<volumes::MountedFsInfo> + Send + Sync>,
    ) -> Self {
        Self {
            store: IndexStore::new(),
            config_store: ConfigStore::new(config),
            volumes: Arc::new(VolumeCoordinator::with_mount_lister(
                cache_dir.clone(),
                mount_lister,
            )),
            cache_dir,
            resume: Mutex::new(None),
            watcher: Mutex::new(None),
            config_watcher: Mutex::new(None),
            scan_lock: Arc::new(Mutex::new(())),
            config_update_lock: Mutex::new(()),
            metrics: Arc::new(WatcherMetrics::new()),
            initialized: Arc::new(AtomicBool::new(false)),
            startup_config_error: Mutex::new(None),
        }
    }

    fn make_status_callback(
        &self,
        event_handler: Arc<dyn EventHandler>,
        status_notified: Option<Arc<AtomicBool>>,
    ) -> Arc<dyn Fn() + Send + Sync> {
        // Capture Weak<VolumeCoordinator> so storing this callback inside VolumeCoordinator
        // cannot form a retain cycle that keeps threads alive after LokiiCore drops.
        let weak_volumes = Arc::downgrade(&self.volumes);
        let store = self.store.clone();
        let initialized = Arc::clone(&self.initialized);
        Arc::new(move || {
            let total_if_initialized = {
                let Some(volumes) = weak_volumes.upgrade() else {
                    return;
                };
                if initialized.load(Ordering::SeqCst) {
                    Some(volumes.total_entry_count(&store))
                } else {
                    None
                }
            };
            // 先释放强引用 Arc<VolumeCoordinator> 再进入跨 FFI 回调，避免外层析构竞态时在后台线程自 join 死锁
            if let Some(ref flag) = status_notified {
                flag.store(true, Ordering::Release);
            }
            event_handler.on_volume_statuses_changed();
            if let Some(total) = total_if_initialized {
                event_handler.on_index_updated(total);
            }
        })
    }

    fn finish_initialization(
        &self,
        mode: startup::StartupMode,
        cold_checkpoint: Option<cache::Checkpoint>,
        event_handler: Arc<dyn EventHandler>,
    ) -> FfiStartupMode {
        if let startup::StartupMode::Failed(error) = mode {
            return FfiStartupMode::Failed {
                message: format!("Index initialization failed: {error}"),
            };
        }

        let config = self.config_store.read().clone();
        let status_notified = Arc::new(AtomicBool::new(false));
        let status_cb =
            self.make_status_callback(Arc::clone(&event_handler), Some(Arc::clone(&status_notified)));
        self.volumes.initialize(&config, status_cb);

        if !self.volumes.statuses().is_empty() && !status_notified.load(Ordering::Acquire) {
            event_handler.on_volume_statuses_changed();
        }

        // Publish initialization completion before reading total_entry_count so concurrent
        // volume scans never miss delivering on_index_updated after on_index_ready.
        self.initialized.store(true, Ordering::SeqCst);
        let total = self.volumes.total_entry_count(&self.store);
        event_handler.on_index_ready(total);

        // 在首屏解锁后于后台统一预热主索引与全部已加载卷分片的查询侧结构（CSR/签名/排序表），
        // 快照卷分片 IndexStore 避免后台线程持有 Arc<VolumeCoordinator> 干扰析构。
        let warm_store = self.store.clone();
        let warm_volume_stores = self.volumes.loaded_stores();
        std::thread::spawn(move || {
            warm_store.warm_query_index();
            for volume_store in warm_volume_stores {
                volume_store.warm_query_index();
            }
        });

        // Hand off the replay checkpoint to the primary watcher service.
        *self.resume.lock().unwrap_or_else(|e| e.into_inner()) = Some(match mode {
            startup::StartupMode::HotStart {
                checkpoint: Some(checkpoint),
                ..
            } => watcher::Resume::From(checkpoint.event_id),
            startup::StartupMode::HotStart { .. } => watcher::Resume::Verify,
            startup::StartupMode::ColdStart { event_id, .. } => watcher::Resume::From(event_id),
            startup::StartupMode::Failed(_) => unreachable!("failed startup returned before ready"),
        });

        // 冷启动已在上面发布索引并派发 on_index_ready 解锁 UI，此刻在短读锁内编码并无锁落盘，
        // 既不阻塞首屏搜索交互，又保证 initialize() 返回时缓存文件已原子就绪。
        if let Some(checkpoint) = cold_checkpoint {
            let encoded = {
                let guard = self.store.read();
                cache::encode_compact(&guard)
            };
            let cache_file = self.cache_dir.join("index.cache");
            startup::save_encoded_index(&encoded, checkpoint, &cache_file);
        }

        match mode {
            startup::StartupMode::HotStart { .. } => FfiStartupMode::HotStart {
                cached_count: total,
            },
            startup::StartupMode::ColdStart { .. } => FfiStartupMode::ColdStart {
                scanned_count: total,
            },
            startup::StartupMode::Failed(_) => unreachable!("failed startup returned before ready"),
        }
    }

    /// Guard file/Finder actions against offline or unmounted volume paths so we never
    /// wake Finder or stall on a disconnected share.
    fn ensure_path_accessible(&self, path: &str) -> Result<(), FfiError> {
        let statuses = self.volumes.statuses();
        // 可达性检查仅依赖挂载点路径，走免 getattrlist 的轻量内核快照，杜绝唤醒休眠外置机械硬盘
        let live_mounts = self.volumes.mounted_volumes_fast();
        ensure_path_accessible_with(path, &statuses, &live_mounts, &|top_entry| {
            read_volumes_boot_symlink(top_entry, &live_mounts)
        })
    }

    fn run_open_command(&self, args: &[&str], target_path: &str) -> Result<bool, FfiError> {
        self.ensure_path_accessible(target_path)?;
        std::process::Command::new("open")
            .args(args)
            .status()
            .map(|s| s.success())
            .map_err(|e| FfiError::IoError {
                message: format!("Failed to invoke open for '{target_path}': {e}"),
            })
    }
}

#[uniffi::export]
impl LokiiCore {
    /// Start the primary file system watcher and the mounted volume monitor.
    /// Non-blocking (spawns background threads internally).
    pub fn start_watcher(&self, event_handler: Arc<dyn EventHandler>) {
        let resume = self
            .resume
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(watcher::Resume::now);
        let batch_handler = Arc::clone(&event_handler);
        let weak_volumes_for_batch = Arc::downgrade(&self.volumes);
        let store_for_batch = self.store.clone();
        let weak_volumes_for_cfg = Arc::downgrade(&self.volumes);
        let cache_dir_for_persist = self.cache_dir.clone();

        let options = watcher::WatcherOptions {
            cache_dir: self.cache_dir.clone(),
            on_config_updated: Some(Arc::new(move |new_config| {
                if let Some(volumes) = weak_volumes_for_cfg.upgrade() {
                    volumes.on_config_updated(new_config);
                }
            })),
        };

        let handle = watcher::start_primary_watcher(
            self.store.clone(),
            self.config_store.clone(),
            Arc::clone(&self.metrics),
            resume,
            Arc::clone(&self.scan_lock),
            options,
            move |_primary_total| {
                let Some(volumes) = weak_volumes_for_batch.upgrade() else {
                    return;
                };
                let total = volumes.total_entry_count(&store_for_batch);
                batch_handler.on_index_updated(total);
            },
            move |encoded, checkpoint| {
                let cache_file = cache_dir_for_persist.join("index.cache");
                startup::save_encoded_index(encoded, checkpoint, &cache_file);
            },
        );
        *self.watcher.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);

        let status_cb = self.make_status_callback(event_handler, None);
        self.volumes
            .start_monitor(self.config_store.clone(), status_cb);
    }

    /// Start the config file watcher for hot-reload.
    pub fn start_config_watcher(&self, event_handler: Arc<dyn EventHandler>) {
        let handler = Arc::clone(&event_handler);
        let error_handler = Arc::clone(&event_handler);
        let weak_volumes = Arc::downgrade(&self.volumes);

        match config::start_config_watcher(
            self.config_store.clone(),
            move |new_config| {
                if let Some(volumes) = weak_volumes.upgrade() {
                    volumes.on_config_updated(new_config);
                }
                handler.on_config_reloaded();
            },
            move |msg| {
                eprintln!("{msg}");
                error_handler.on_watcher_error(msg);
            },
        ) {
            Ok(handle) => {
                *self
                    .config_watcher
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(handle);
            }
            Err(e) => {
                eprintln!("Failed to start config watcher: {e}");
            }
        }
    }

    /// Start the local query socket used by the `lokii` CLI and external scripts.
    pub fn start_query_server(&self) {
        if let Err(e) = crate::query_server::start_with_coordinator(
            self.store.clone(),
            Some(Arc::clone(&self.volumes)),
            self.config_store.clone(),
        ) {
            eprintln!("[Lokii] Failed to start query server: {e}");
        }
    }

    /// Search across primary and mounted volume shards (including offline caches by default).
    pub fn search(
        &self,
        query: String,
        mode: FfiSearchMode,
        case_sensitivity: FfiCaseSensitivity,
    ) -> FfiSearchResponse {
        self.search_with_options(query, mode, case_sensitivity, true)
    }

    /// Search across primary and mounted volume shards with explicit control over offline caches.
    pub fn search_with_options(
        &self,
        query: String,
        mode: FfiSearchMode,
        case_sensitivity: FfiCaseSensitivity,
        include_offline: bool,
    ) -> FfiSearchResponse {
        let max_results = self.config_store.read().index.max_results;
        let search_mode = SearchMode::from(&mode);
        let (results, is_case_sensitive, error, effective_mode) = self.volumes.search(
            &self.store,
            &query,
            search_mode,
            case_sensitivity.into(),
            max_results,
            include_offline,
        );

        let hint = if results.is_empty() {
            crate::filter::exclusion_hint(&query, &self.config_store.read())
        } else {
            None
        };

        FfiSearchResponse {
            results: convert_multi_results(results),
            error,
            is_case_sensitive,
            effective_mode: effective_mode.into(),
            hint,
        }
    }

    /// Get current index statistics across primary and volume shards.
    pub fn get_stats(&self) -> FfiIndexStats {
        let total_count = self.volumes.total_entry_count(&self.store);
        FfiIndexStats {
            total_count,
            is_ready: total_count > 0 || self.initialized.load(Ordering::Acquire),
        }
    }

    /// Check if a primary background scan or any mounted volume sync is currently running.
    pub fn is_scanning(&self) -> bool {
        let primary_scanning = match self.scan_lock.try_lock() {
            Ok(_guard) => false,
            Err(std::sync::TryLockError::WouldBlock) => true,
            Err(std::sync::TryLockError::Poisoned(_)) => false,
        };
        primary_scanning
            || self
                .volumes
                .statuses()
                .iter()
                .any(|s| matches!(s.state, VolumeState::Syncing))
    }

    /// Rebuild the primary index from scratch and schedule rescans for online mounted volumes.
    /// Blocking — call from a background thread.
    pub fn rebuild_index(&self, event_handler: Arc<dyn EventHandler>) -> bool {
        let guard = match self.scan_lock.try_lock() {
            Ok(g) => g,
            Err(std::sync::TryLockError::WouldBlock) => {
                return false;
            }
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        };

        let config = self.config_store.read().clone();
        self.volumes.on_config_updated(&config);
        for status in self.volumes.statuses() {
            if !matches!(status.state, VolumeState::Syncing) {
                let _ = self.volumes.rescan_volume(&status.volume_id);
            }
        }

        let volumes_dir = self.cache_dir.join("volumes");
        let (primary_roots, excluded_mount_roots) =
            watcher::primary_scope_inputs(&config, &volumes_dir);
        let checkpoint = cache::Checkpoint {
            event_id: crate::fsevents::current_event_id(),
            fingerprint: startup::primary_scope_fingerprint(
                &config,
                &primary_roots,
                &excluded_mount_roots,
            ),
        };

        let progress = Arc::new(AtomicUsize::new(0));
        let poller = ProgressPoller::start(
            "rebuilding",
            Arc::clone(&progress),
            Arc::clone(&event_handler),
        );

        let filter = Arc::new(crate::filter::CompiledFilter::from_primary_config(
            &config,
            &primary_roots,
            &excluded_mount_roots,
        ));
        let scan_res = scanner::scan_compact(
            &primary_roots,
            filter,
            config.index.include_hidden,
            progress,
        );
        drop(poller);

        let fresh = match scan_res {
            Ok(fresh) => fresh,
            Err(error) => {
                event_handler.on_watcher_error(format!("Index rebuild failed: {error}"));
                return false;
            }
        };

        // 先在锁外对新索引完成内存编码，随后立即发布新索引并通知 UI 就绪，最后在完全无锁状态下写盘与 sync_data()。
        let encoded = cache::encode_compact(&fresh);

        let watcher = self.watcher.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let unswapped = match watcher {
            Some(watcher) => watcher.replace(fresh, checkpoint).err(),
            None => Some(fresh),
        };
        if let Some(fresh) = unswapped {
            *self.watcher.lock().unwrap_or_else(|e| e.into_inner()) = None;
            self.store.replace_compact(fresh);
            let warm_store = self.store.clone();
            std::thread::spawn(move || warm_store.warm_query_index());
        }

        drop(guard);
        event_handler.on_index_ready(self.volumes.total_entry_count(&self.store));
        let cache_file = self.cache_dir.join("index.cache");
        let _ = cache::save_encoded_compact(&encoded, Some(checkpoint), &cache_file);
        true
    }

    /// Save the primary index checkpoint and all dirty volume shard caches/manifest.
    pub fn persist_index(&self) {
        if let Some(watcher) = self.watcher.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            // 先持索引读锁再取检查点并完成内存编码，确保检查点不超前于快照，随后立即释放读锁再落盘。
            let snapshot = {
                let index = self.store.read();
                watcher
                    .checkpoint()
                    .map(|checkpoint| (cache::encode_compact(&index), checkpoint))
            };
            if let Some((encoded, checkpoint)) = snapshot {
                let cache_file = self.cache_dir.join("index.cache");
                startup::save_encoded_index(&encoded, checkpoint, &cache_file);
            }
        }
        self.volumes.persist_all();
    }

    /// Check if macOS Full Disk Access is granted.
    pub fn check_fda(&self) -> bool {
        permissions::has_full_disk_access()
    }

    /// Open a file with the default application (macOS `open` command).
    /// Returns `FfiError::IoError` immediately if the path belongs to an offline or unmounted volume.
    pub fn open_file(&self, path: String) -> Result<bool, FfiError> {
        self.run_open_command(&[&path], &path)
    }

    /// Reveal a file in Finder (select it in its parent directory).
    /// Returns `FfiError::IoError` immediately if the path belongs to an offline or unmounted volume.
    pub fn reveal_in_finder(&self, path: String) -> Result<bool, FfiError> {
        self.run_open_command(&["-R", &path], &path)
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

    /// Update configuration from a JSON patch string. Returns true on success.
    /// Atomically writes `config.toml` and publishes to ConfigStore only after rename succeeds,
    /// then syncs primary store exclusions and VolumeCoordinator.
    pub fn update_config_from_json(&self, json: String) -> bool {
        let _config_guard = self
            .config_update_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let current = self.config_store.read().clone();
        let new_config = match merge_config_patch(&current, &json) {
            Some(config) => config,
            None => {
                eprintln!("[Lokii] Failed to parse config JSON patch: {json}");
                return false;
            }
        };

        let config_path = self.cache_dir.join("config.toml");
        let toml_str = match toml::to_string_pretty(&new_config) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[Lokii] Failed to serialize config to TOML: {e}");
                return false;
            }
        };

        // Publish to config_store under CONFIG_WRITE_LOCK only after rename succeeds so
        // background pollers never observe uncommitted changes on write failure, while
        // start_config_watcher still sees the updated config before re-reading the file.
        if let Err(e) = config::write_config_atomic_then(&config_path, &toml_str, || {
            self.config_store.update(new_config.clone());
        }) {
            eprintln!("[Lokii] Failed to write config file: {e}");
            return false;
        }

        // Evict newly excluded paths from the primary in-memory index immediately.
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

        // 仅当索引相关配置变更时同步卷协调器，避免修改通用/窗口设置时触发挂载表探测与卷缓存对账
        if current.index != new_config.index {
            self.volumes.on_config_updated(&new_config);
        }
        true
    }

    /// Snapshot current statuses of all configured mounted volume slots.
    pub fn get_volume_statuses(&self) -> Vec<FfiVolumeStatus> {
        self.volumes
            .statuses()
            .into_iter()
            .map(FfiVolumeStatus::from)
            .collect()
    }

    /// Remove a volume slot and delete its cached index shard and manifest record.
    pub fn clear_volume_cache(&self, path_or_id: String) -> Result<bool, FfiError> {
        self.volumes
            .clear_volume_cache(&path_or_id)
            .map_err(|message| FfiError::IoError { message })
    }

    /// Trigger an atomic background rescan for a mounted volume slot.
    pub fn rescan_volume(&self, path_or_id: String) -> Result<bool, FfiError> {
        self.volumes
            .rescan_volume(&path_or_id)
            .map_err(|message| FfiError::IoError { message })
    }

    /// Notify the coordinator that a filesystem was mounted at `mount_path`.
    pub fn notify_volume_mounted(&self, mount_path: String) {
        self.volumes
            .notify_volume_mounted(Path::new(&mount_path));
    }

    /// Notify the coordinator that `mount_path` is about to unmount so scans and
    /// directory handles are cancelled immediately while preserving offline caches.
    pub fn notify_volume_will_unmount(&self, mount_path: String) {
        self.volumes
            .notify_volume_will_unmount(Path::new(&mount_path));
    }

    /// Notify the coordinator that `mount_path` finished unmounting.
    pub fn notify_volume_did_unmount(&self, mount_path: String) {
        self.volumes
            .notify_volume_did_unmount(Path::new(&mount_path));
    }
}

/// Preview Smart Case and inline regex-sugar state using the core query parser.
#[uniffi::export]
pub fn preview_query_flags(query: String, mode: FfiSearchMode) -> FfiQueryPreview {
    let preview = search::preview_query_flags(&query, SearchMode::from(&mode));
    FfiQueryPreview {
        has_effective_uppercase: preview.has_effective_uppercase,
        has_regex_sugar: preview.has_regex_sugar,
    }
}

/// Return `AppConfig::default()` as a strongly-typed UniFFI record so the frontend
/// shares Rust's single source of truth with compile-time schema verification.
#[uniffi::export]
pub fn default_app_config() -> FfiAppConfig {
    let cfg = AppConfig::default();
    FfiAppConfig {
        general: FfiGeneralConfig {
            language: cfg.general.language,
            show_in_dock: cfg.general.show_in_dock,
            terminal_bundle_id: cfg.general.terminal_bundle_id,
            hide_results_when_empty: cfg.general.hide_results_when_empty,
        },
        index: FfiIndexConfig {
            index_paths: cfg.index.index_paths,
            index_apps: cfg.index.index_apps,
            exclude: cfg.index.exclude,
            exclude_paths: cfg.index.exclude_paths,
            include_hidden: cfg.index.include_hidden,
            exclude_sensitive: cfg.index.exclude_sensitive,
            sensitive_names: cfg.index.sensitive_names,
            max_results: cfg.index.max_results as u64,
            network_rescan_minutes: cfg.index.network_rescan_minutes,
        },
        window: FfiWindowConfig {
            hide_on_blur: cfg.window.hide_on_blur,
            remember_position: cfg.window.remember_position,
        },
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
    use crate::volumes::VolumeManifest;

    #[test]
    fn preview_query_flags_matches_smart_case_and_regex_sugar_rules() {
        let auto = |q: &str| preview_query_flags(q.to_string(), FfiSearchMode::Auto);

        assert!(!auto("report").has_effective_uppercase);
        assert!(auto("Report").has_effective_uppercase);
        assert!(!auto("in:/Users/Downloads report").has_effective_uppercase);
        assert!(!auto("ext:PNG report").has_effective_uppercase);
        assert!(auto("ext:png Report").has_effective_uppercase);
        assert!(auto("type:pdf /path/to/File").has_effective_uppercase);
        assert!(!auto("type:pdf /path/to/file").has_effective_uppercase);
        assert!(!auto(".git report").has_effective_uppercase);
        assert!(auto(".GIT Report").has_effective_uppercase);
        assert!(!auto("!Test").has_effective_uppercase);
        assert!(!auto("!\"Test\"").has_effective_uppercase);
        assert!(auto("\"Test\"").has_effective_uppercase);
        assert!(!auto("main !Test").has_effective_uppercase);
        assert!(auto("Main !Test").has_effective_uppercase);
        assert!(auto("\"type:PDF\"").has_effective_uppercase);
        assert!(auto("\"in:Dir\"").has_effective_uppercase);
        assert!(auto("\"foo/Bar\"").has_effective_uppercase);
        assert!(!auto(r"regex:\D+ !Skip").has_effective_uppercase);
        assert!(auto("regex:^Main !skip").has_effective_uppercase);
        assert!(auto("foo | Bar").has_effective_uppercase);
        assert!(!auto("foo | bar").has_effective_uppercase);

        assert!(!auto("").has_regex_sugar);
        assert!(!auto("/").has_regex_sugar);
        assert!(!auto("//").has_regex_sugar);
        assert!(!auto("/pattern/").has_regex_sugar);
        assert!(auto("/pattern/i").has_regex_sugar);
        assert!(auto("/[a-z]+/").has_regex_sugar);
        assert!(auto("/[a-z]+/i").has_regex_sugar);
        assert!(auto("/[a-z]+/I").has_regex_sugar);
        assert!(!auto("/[A-Z]+/i").has_effective_uppercase);
        assert!(auto("/[A-Z]+/").has_effective_uppercase);
        assert!(auto("regex:foo").has_regex_sugar);
        assert!(!auto("\"regex:foo\"").has_regex_sugar);
        assert!(auto("  /[a-z]+/  ").has_regex_sugar);
        assert!(auto(r"/foo\/bar/").has_regex_sugar);
        assert!(auto(r"in:src /^test_.*\.rs/").has_regex_sugar);
        assert!(!auto(r"in:SRC /^test_.*\.rs/").has_effective_uppercase);
        assert!(!auto("/unclosed").has_regex_sugar);
        assert!(!auto(r"/escaped\/").has_regex_sugar);
        assert!(!auto("/usr/local/bin/").has_regex_sugar);
        assert!(!auto("plain text").has_regex_sugar);

        // 显式正则锁定下，反斜杠转义元字符不触发 Smart Case
        assert!(!preview_query_flags(r"\D+\S+".to_string(), FfiSearchMode::Regex).has_effective_uppercase);
        assert!(preview_query_flags(r"Main\d+".to_string(), FfiSearchMode::Regex).has_effective_uppercase);
    }

    #[test]
    fn default_app_config_matches_app_config_default() {
        let ffi_cfg = default_app_config();
        let expected = AppConfig::default();
        assert_eq!(ffi_cfg.general.language, expected.general.language);
        assert_eq!(ffi_cfg.general.show_in_dock, expected.general.show_in_dock);
        assert_eq!(ffi_cfg.general.terminal_bundle_id, expected.general.terminal_bundle_id);
        assert_eq!(ffi_cfg.general.hide_results_when_empty, expected.general.hide_results_when_empty);
        assert_eq!(ffi_cfg.index.index_paths, expected.index.index_paths);
        assert_eq!(ffi_cfg.index.index_apps, expected.index.index_apps);
        assert_eq!(ffi_cfg.index.exclude, expected.index.exclude);
        assert_eq!(ffi_cfg.index.exclude_paths, expected.index.exclude_paths);
        assert_eq!(ffi_cfg.index.include_hidden, expected.index.include_hidden);
        assert_eq!(ffi_cfg.index.exclude_sensitive, expected.index.exclude_sensitive);
        assert_eq!(ffi_cfg.index.sensitive_names, expected.index.sensitive_names);
        assert_eq!(ffi_cfg.index.max_results, expected.index.max_results as u64);
        assert_eq!(ffi_cfg.index.network_rescan_minutes, expected.index.network_rescan_minutes);
        assert_eq!(ffi_cfg.window.hide_on_blur, expected.window.hide_on_blur);
        assert_eq!(ffi_cfg.window.remember_position, expected.window.remember_position);
    }

    #[derive(Default)]
    struct RecordingHandler {
        events: Mutex<Vec<String>>,
        volume_status_changes: std::sync::atomic::AtomicUsize,
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

        fn on_volume_statuses_changed(&self) {
            self.volume_status_changes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    struct TestCore {
        _dir: tempfile::TempDir,
        inner: LokiiCore,
    }

    impl std::ops::Deref for TestCore {
        type Target = LokiiCore;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    fn core() -> TestCore {
        let dir = tempfile::tempdir().unwrap();
        let inner = LokiiCore::for_test(
            dir.path().to_path_buf(),
            AppConfig::default(),
            Arc::new(Vec::new),
        );
        TestCore { _dir: dir, inner }
    }

    fn make_entry(name: &str, path: &str) -> crate::indexer::FileEntry {
        crate::indexer::FileEntry {
            name: name.to_string(),
            path: path.to_string(),
            is_dir: false,
            size: 16,
            modified: 1_700_000_000,
            created_at: 1_700_000_000,
            extension: std::path::Path::new(name)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default()
                .to_string(),
        }
    }

    fn seed_volume_cache(
        volumes_dir: &std::path::Path,
        configured_path: &str,
        active_configured_path: &str,
        active_mount_root: Option<&str>,
        display_name: &str,
        kind: VolumeKind,
        entries: Vec<crate::indexer::FileEntry>,
    ) -> crate::volumes::VolumeRecord {
        use crate::compact_index::CompactIndex;
        use crate::volumes::{VolumeIdentity, VolumeManifest, VolumeRecord};

        let identity = match kind {
            VolumeKind::Network => VolumeIdentity::Network {
                fstype: "smbfs".to_string(),
                normalized_source: format!("//nas.local/{display_name}"),
                subpath: String::new(),
            },
            VolumeKind::LocalExternal => VolumeIdentity::LocalUuid {
                uuid: format!("UUID-{display_name}"),
                subpath: String::new(),
            },
        };
        let vid = identity.volume_id();
        let entry_count = entries.len() as u64;
        let compact = CompactIndex::from_entries(entries).unwrap();
        cache::save_compact(&compact, None, &volumes_dir.join(format!("{vid}.cache"))).unwrap();
        let record = VolumeRecord {
            volume_id: vid,
            configured_path: configured_path.to_string(),
            active_configured_path: active_configured_path.to_string(),
            active_mount_root: active_mount_root.map(str::to_string),
            subpath: String::new(),
            display_name: display_name.to_string(),
            kind,
            fstype: if kind == VolumeKind::Network {
                "smbfs".to_string()
            } else {
                "apfs".to_string()
            },
            identity,
            last_sync_at: Some(1_700_000_000),
            last_event_id: None,
            journal_uuid: None,
            entry_count,
            dirty_filter: false,
            filter_fingerprint: None,
            pruned_for_fp: None,
        };
        let mut manifest = VolumeManifest::load_from_dir(volumes_dir);
        manifest.upsert(record.clone());
        manifest.save_atomic(volumes_dir).unwrap();
        record
    }

    #[test]
    fn initialization_failure_returns_error_without_ready_or_verification() {
        for error in [IndexError::IdOverflow, IndexError::ReferenceOverflow] {
            let core = core();
            let handler = Arc::new(RecordingHandler::default());
            let result = core.finish_initialization(
                startup::StartupMode::Failed(error),
                None,
                handler.clone(),
            );
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
            None,
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

    #[test]
    fn search_honors_case_sensitivity_and_reports_effective_flag() {
        let core = core();
        core.store.replace(vec![crate::indexer::FileEntry {
            name: "FooBar.txt".to_string(),
            path: "/path/FooBar.txt".to_string(),
            is_dir: false,
            size: 10,
            modified: 100,
            created_at: 100,
            extension: "txt".to_string(),
        }]);

        // Auto + lowercase -> Case-insensitive (is_case_sensitive: false)
        let res_auto_lower = core.search(
            "foo".to_string(),
            FfiSearchMode::Substring,
            FfiCaseSensitivity::Auto,
        );
        assert_eq!(res_auto_lower.results.len(), 1);
        assert!(!res_auto_lower.is_case_sensitive);
        assert_eq!(res_auto_lower.effective_mode, FfiSearchMode::Substring);

        // Auto + uppercase -> Smart case triggers sensitive (is_case_sensitive: true)
        let res_auto_upper = core.search(
            "FOO".to_string(),
            FfiSearchMode::Substring,
            FfiCaseSensitivity::Auto,
        );
        assert_eq!(res_auto_upper.results.len(), 0);
        assert!(res_auto_upper.is_case_sensitive);
        assert_eq!(res_auto_upper.effective_mode, FfiSearchMode::Substring);

        // Force Insensitive -> is_case_sensitive: false
        let res_insens = core.search(
            "FOO".to_string(),
            FfiSearchMode::Substring,
            FfiCaseSensitivity::Insensitive,
        );
        assert_eq!(res_insens.results.len(), 1);
        assert!(!res_insens.is_case_sensitive);
        assert_eq!(res_insens.effective_mode, FfiSearchMode::Substring);

        // Force Sensitive -> is_case_sensitive: true
        let res_sens = core.search(
            "foo".to_string(),
            FfiSearchMode::Substring,
            FfiCaseSensitivity::Sensitive,
        );
        assert_eq!(res_sens.results.len(), 0);
        assert!(res_sens.is_case_sensitive);
        assert_eq!(res_sens.effective_mode, FfiSearchMode::Substring);

        // Auto search mode with inline regex sugar /pattern/i
        let res_regex_sugar = core.search(
            "/foo/i".to_string(),
            FfiSearchMode::Auto,
            FfiCaseSensitivity::Sensitive,
        );
        assert_eq!(res_regex_sugar.results.len(), 1);
        assert_eq!(res_regex_sugar.effective_mode, FfiSearchMode::Regex);
        assert!(!res_regex_sugar.is_case_sensitive);

        // Explicit locked regex mode
        let res_regex_locked = core.search(
            "Foo.*".to_string(),
            FfiSearchMode::Regex,
            FfiCaseSensitivity::Sensitive,
        );
        assert_eq!(res_regex_locked.results.len(), 1);
        assert_eq!(res_regex_locked.effective_mode, FfiSearchMode::Regex);
        assert!(res_regex_locked.is_case_sensitive);
    }

    #[test]
    fn offline_path_open_and_reveal_rejected_with_ffi_error() {
        let dir = tempfile::tempdir().unwrap();
        let volumes_dir = dir.path().join("volumes");
        seed_volume_cache(
            &volumes_dir,
            "/Volumes/OfflineNAS/Projects",
            "/Volumes/OfflineNAS/Projects",
            Some("/Volumes/OfflineNAS"),
            "OfflineNAS/Projects",
            VolumeKind::Network,
            vec![make_entry(
                "spec.md",
                "/Volumes/OfflineNAS/Projects/spec.md",
            )],
        );

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec!["/Volumes/OfflineNAS/Projects".to_string()];
        cfg.index.exclude_paths = vec![];

        let core = LokiiCore::for_test(dir.path().to_path_buf(), cfg, Arc::new(Vec::new));
        let handler = Arc::new(RecordingHandler::default());
        let _ = core.initialize(handler);

        // Configured offline slot must be rejected immediately with FfiError::IoError.
        let open_err = core
            .open_file("/Volumes/OfflineNAS/Projects/spec.md".to_string())
            .expect_err("offline slot open_file must return FfiError");
        assert!(matches!(open_err, FfiError::IoError { .. }));

        let reveal_err = core
            .reveal_in_finder("/Volumes/OfflineNAS/Projects/spec.md".to_string())
            .expect_err("offline slot reveal_in_finder must return FfiError");
        assert!(matches!(reveal_err, FfiError::IoError { .. }));

        // Unconfigured /Volumes/Disconnected/... must also be rejected immediately without invoking Finder.
        let disc_open = core
            .open_file("/Volumes/Disconnected/report.pdf".to_string())
            .expect_err("unmounted /Volumes path must return FfiError");
        assert!(matches!(disc_open, FfiError::IoError { .. }));

        let disc_reveal = core
            .reveal_in_finder("/Volumes/Disconnected/report.pdf".to_string())
            .expect_err("unmounted /Volumes path reveal_in_finder must return FfiError");
        assert!(matches!(disc_reveal, FfiError::IoError { .. }));
    }

    #[test]
    fn peek_startup_is_cold_start_respects_volume_cache_and_primary_cache() {
        let dir = tempfile::tempdir().unwrap();
        let volumes_dir = dir.path().join("volumes");

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec!["/Volumes/Archive/Docs".to_string()];
        cfg.index.exclude_paths = vec![];

        let core = LokiiCore::for_test(dir.path().to_path_buf(), cfg.clone(), Arc::new(Vec::new));
        assert!(
            core.peek_startup_is_cold_start(),
            "fresh cache dir with no primary or volume cache must report cold start"
        );

        // Seed only volumes/manifest.json + <volume_id>.cache (no index.cache).
        seed_volume_cache(
            &volumes_dir,
            "/Volumes/Archive/Docs",
            "/Volumes/Archive/Docs",
            Some("/Volumes/Archive"),
            "Archive/Docs",
            VolumeKind::Network,
            vec![make_entry("notes.txt", "/Volumes/Archive/Docs/notes.txt")],
        );
        assert!(
            !core.peek_startup_is_cold_start(),
            "existing configured volume cache must prevent cold start"
        );

        // Unconfigured manifest record does not satisfy current config.
        let mut other_cfg = cfg;
        other_cfg.index.index_paths = vec!["/Volumes/Other".to_string()];
        let core_other =
            LokiiCore::for_test(dir.path().to_path_buf(), other_cfg, Arc::new(Vec::new));
        assert!(core_other.peek_startup_is_cold_start());
    }

    #[test]
    fn cold_and_warm_startup_with_coordinator_and_status_mapping() {
        let cache_dir = tempfile::tempdir().unwrap();
        let primary_dir = tempfile::tempdir().unwrap();
        // Use a non-hidden directory inside TempDir so default `include_hidden = false` scans it cleanly.
        let primary_root = primary_dir.path().join("workspace");
        std::fs::create_dir_all(&primary_root).unwrap();
        std::fs::write(primary_root.join("local_a.txt"), "a").unwrap();
        std::fs::write(primary_root.join("local_b.txt"), "b").unwrap();

        let volumes_dir = cache_dir.path().join("volumes");
        let rec = seed_volume_cache(
            &volumes_dir,
            "/Volumes/Share/Work",
            "/Volumes/Share-1/Work",
            Some("/Volumes/Share-1"),
            "Share/Work",
            VolumeKind::Network,
            vec![
                make_entry("vol_1.txt", "/Volumes/Share-1/Work/vol_1.txt"),
                make_entry("vol_2.txt", "/Volumes/Share-1/Work/vol_2.txt"),
                make_entry("vol_3.txt", "/Volumes/Share-1/Work/vol_3.txt"),
            ],
        );

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![
            primary_root.to_string_lossy().to_string(),
            "/Volumes/Share/Work".to_string(),
        ];
        cfg.index.exclude_paths = vec![];

        // Cold primary start + pre-existing offline volume cache (3 primary entries + 3 volume entries = 6).
        let cold_core = LokiiCore::for_test(
            cache_dir.path().to_path_buf(),
            cfg.clone(),
            Arc::new(Vec::new),
        );
        let cold_handler = Arc::new(RecordingHandler::default());
        let cold_mode = cold_core.initialize(cold_handler.clone());
        let FfiStartupMode::ColdStart { scanned_count } = cold_mode else {
            panic!("expected ColdStart when index.cache is missing");
        };
        assert_eq!(scanned_count, 6);
        assert!(cold_handler
            .volume_status_changes
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 1);
        assert!(cold_handler
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e == "ready:6"));

        let stats = cold_core.get_stats();
        assert_eq!(stats.total_count, 6);
        assert!(stats.is_ready);

        let statuses = cold_core.get_volume_statuses();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].volume_id, rec.volume_id);
        assert_eq!(statuses[0].configured_path, "/Volumes/Share/Work");
        assert_eq!(statuses[0].active_path, None);
        assert_eq!(statuses[0].display_name, "Share/Work");
        assert_eq!(statuses[0].kind, "network");
        assert_eq!(statuses[0].state, FfiVolumeState::Offline);
        assert_eq!(statuses[0].entry_count, 3);
        assert_eq!(statuses[0].last_sync_at, Some(1_700_000_000));
        assert!(!statuses[0].is_remounted);

        // Subsequent launch with index.cache present must perform HotStart with combined count.
        let warm_core = LokiiCore::for_test(
            cache_dir.path().to_path_buf(),
            cfg,
            Arc::new(Vec::new),
        );
        let warm_handler = Arc::new(RecordingHandler::default());
        let warm_mode = warm_core.initialize(warm_handler.clone());
        let FfiStartupMode::HotStart { cached_count } = warm_mode else {
            panic!("expected HotStart when index.cache exists");
        };
        assert_eq!(cached_count, 6);
        assert!(warm_handler
            .volume_status_changes
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 1);
        assert!(warm_handler
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e == "ready:6"));
    }

    #[test]
    fn search_maps_offline_metadata_and_honors_include_offline_toggle() {
        let dir = tempfile::tempdir().unwrap();
        let volumes_dir = dir.path().join("volumes");
        seed_volume_cache(
            &volumes_dir,
            "/Volumes/Docs",
            "/Volumes/Docs",
            Some("/Volumes/Docs"),
            "Docs",
            VolumeKind::Network,
            vec![make_entry("nas_report.txt", "/Volumes/Docs/nas_report.txt")],
        );

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec!["/Volumes/Docs".to_string()];
        cfg.index.exclude_paths = vec![];

        let core = LokiiCore::for_test(dir.path().to_path_buf(), cfg, Arc::new(Vec::new));
        let handler = Arc::new(RecordingHandler::default());
        let _ = core.initialize(handler);
        core.store.replace(vec![make_entry(
            "local_report.txt",
            "/Users/me/local_report.txt",
        )]);

        let all = core.search(
            "report".to_string(),
            FfiSearchMode::Substring,
            FfiCaseSensitivity::Auto,
        );
        assert_eq!(all.results.len(), 2);
        let local = all
            .results
            .iter()
            .find(|r| r.name == "local_report.txt")
            .expect("local result must exist");
        assert!(!local.is_offline);
        assert_eq!(local.volume_name, None);

        let offline = all
            .results
            .iter()
            .find(|r| r.name == "nas_report.txt")
            .expect("offline volume result must exist");
        assert!(offline.is_offline);
        assert_eq!(offline.volume_name.as_deref(), Some("Docs"));

        let online_only = core.search_with_options(
            "report".to_string(),
            FfiSearchMode::Substring,
            FfiCaseSensitivity::Auto,
            false,
        );
        assert_eq!(online_only.results.len(), 1);
        assert_eq!(online_only.results[0].name, "local_report.txt");
    }

    #[test]
    fn volume_lifecycle_notifications_rescan_and_clear_cache() {
        let cache_dir = tempfile::tempdir().unwrap();
        let vol_root = tempfile::tempdir().unwrap();
        let vol_path = vol_root.path().to_string_lossy().to_string();
        std::fs::write(vol_root.path().join("initial_file.txt"), "hello").unwrap();

        let volumes_dir = cache_dir.path().join("volumes");
        let seeded = seed_volume_cache(
            &volumes_dir,
            &vol_path,
            &vol_path,
            Some(&vol_path),
            "ExtDisk",
            VolumeKind::Network,
            vec![make_entry(
                "initial_file.txt",
                &format!("{vol_path}/initial_file.txt"),
            )],
        );

        let mounted_list = Arc::new(Mutex::new(vec![crate::volumes::MountedFsInfo {
            mount_point: vol_path.clone(),
            fstype: "smbfs".to_string(),
            from_name: "//nas.local/ExtDisk".to_string(),
            is_local: false,
            uuid: None,
            total_bytes: 1_000_000,
            volume_name: "ExtDisk".to_string(),
        }]));
        let mount_reader = Arc::clone(&mounted_list);

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![vol_path.clone()];
        cfg.index.exclude_paths = vec![];

        let core = LokiiCore::for_test(
            cache_dir.path().to_path_buf(),
            cfg,
            Arc::new(move || mount_reader.lock().unwrap().clone()),
        );
        let handler = Arc::new(RecordingHandler::default());
        let _ = core.initialize(handler.clone());

        // Wait for background scan to settle Online.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let st = core.get_volume_statuses();
            if st.len() == 1 && st[0].state == FfiVolumeState::Online {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let online_statuses = core.get_volume_statuses();
        assert_eq!(online_statuses.len(), 1);
        assert_eq!(online_statuses[0].state, FfiVolumeState::Online);
        assert_eq!(online_statuses[0].active_path.as_deref(), Some(vol_path.as_str()));

        // Add a new file and trigger rescan_volume -> new file becomes searchable.
        std::fs::write(vol_root.path().join("rescanned_asset.txt"), "new").unwrap();
        let before_rescan_events = handler
            .volume_status_changes
            .load(std::sync::atomic::Ordering::SeqCst);
        assert!(core.rescan_volume(vol_path.clone()).unwrap());

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let st = core.get_volume_statuses();
            if st[0].state == FfiVolumeState::Online && st[0].entry_count >= 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            handler
                .volume_status_changes
                .load(std::sync::atomic::Ordering::SeqCst)
                > before_rescan_events
        );
        let hit = core.search(
            "rescanned_asset".to_string(),
            FfiSearchMode::Substring,
            FfiCaseSensitivity::Auto,
        );
        assert_eq!(hit.results.len(), 1);
        assert!(!hit.results[0].is_offline);

        // Simulate willUnmount -> immediately goes Offline while keeping cached results searchable.
        mounted_list.lock().unwrap().clear();
        core.notify_volume_will_unmount(vol_path.clone());
        let offline_statuses = core.get_volume_statuses();
        assert_eq!(offline_statuses[0].state, FfiVolumeState::Offline);
        assert_eq!(offline_statuses[0].active_path, None);
        let offline_hit = core.search(
            "rescanned_asset".to_string(),
            FfiSearchMode::Substring,
            FfiCaseSensitivity::Auto,
        );
        assert_eq!(offline_hit.results.len(), 1);
        assert!(offline_hit.results[0].is_offline);

        // Simulate didUnmount and remount -> transitions back online.
        core.notify_volume_did_unmount(vol_path.clone());
        *mounted_list.lock().unwrap() = vec![crate::volumes::MountedFsInfo {
            mount_point: vol_path.clone(),
            fstype: "smbfs".to_string(),
            from_name: "//nas.local/ExtDisk".to_string(),
            is_local: false,
            uuid: None,
            total_bytes: 1_000_000,
            volume_name: "ExtDisk".to_string(),
        }];
        core.notify_volume_mounted(vol_path.clone());
        assert_ne!(core.get_volume_statuses()[0].state, FfiVolumeState::Offline);

        // Clear volume cache -> removes slot, deletes .cache, and updates manifest.
        let cache_file = volumes_dir.join(format!("{}.cache", seeded.volume_id));
        assert!(cache_file.exists());
        assert!(core.clear_volume_cache(seeded.volume_id.clone()).unwrap());
        assert!(core.get_volume_statuses().is_empty());
        assert!(!cache_file.exists());
        assert!(!core.clear_volume_cache(seeded.volume_id).unwrap());
    }

    #[test]
    fn update_config_from_json_forwards_changes_to_volume_coordinator() {
        let cache_dir = tempfile::tempdir().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");

        let keep_rec = seed_volume_cache(
            &volumes_dir,
            "/Volumes/KeepVol",
            "/Volumes/KeepVol",
            Some("/Volumes/KeepVol"),
            "KeepVol",
            VolumeKind::Network,
            vec![make_entry("keep.txt", "/Volumes/KeepVol/keep.txt")],
        );
        let drop_rec = seed_volume_cache(
            &volumes_dir,
            "/Volumes/DropVol",
            "/Volumes/DropVol",
            Some("/Volumes/DropVol"),
            "DropVol",
            VolumeKind::Network,
            vec![make_entry("drop.txt", "/Volumes/DropVol/drop.txt")],
        );

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![
            "/Volumes/KeepVol".to_string(),
            "/Volumes/DropVol".to_string(),
        ];
        cfg.index.exclude_paths = vec![];

        let core = LokiiCore::for_test(cache_dir.path().to_path_buf(), cfg, Arc::new(Vec::new));
        let handler = Arc::new(RecordingHandler::default());
        let _ = core.initialize(handler);
        assert_eq!(core.get_volume_statuses().len(), 2);

        // Replace index_paths to drop DropVol and add AddedVol, plus append an exclude rule.
        let patch = r#"{
            "index": {
                "index_paths": ["/Volumes/KeepVol", "/Volumes/AddedVol"],
                "exclude": ["node_modules", "secret_folder"]
            }
        }"#;
        assert!(core.update_config_from_json(patch.to_string()));

        let statuses = core.get_volume_statuses();
        let paths: Vec<&str> = statuses
            .iter()
            .map(|s| s.configured_path.as_str())
            .collect();
        assert!(paths.contains(&"/Volumes/KeepVol"));
        assert!(paths.contains(&"/Volumes/AddedVol"));
        assert!(!paths.contains(&"/Volumes/DropVol"));

        assert!(!volumes_dir
            .join(format!("{}.cache", drop_rec.volume_id))
            .exists());
        let manifest = VolumeManifest::load_from_dir(&volumes_dir);
        let keep_manifest = manifest
            .find_by_path("/Volumes/KeepVol")
            .expect("KeepVol must remain in manifest");
        assert_eq!(keep_manifest.volume_id, keep_rec.volume_id);
        assert!(
            keep_manifest.dirty_filter,
            "filter change while offline must mark KeepVol dirty_filter=true"
        );
    }

    #[test]
    fn rebuild_index_rescans_primary_and_preserves_offline_volume_cache() {
        let cache_dir = tempfile::tempdir().unwrap();
        let primary_dir = tempfile::tempdir().unwrap();
        let primary_root = primary_dir.path().join("workspace");
        std::fs::create_dir_all(&primary_root).unwrap();
        std::fs::write(primary_root.join("alpha.txt"), "a").unwrap();

        let volumes_dir = cache_dir.path().join("volumes");
        let rec = seed_volume_cache(
            &volumes_dir,
            "/Volumes/OfflineVault",
            "/Volumes/OfflineVault",
            Some("/Volumes/OfflineVault"),
            "OfflineVault",
            VolumeKind::Network,
            vec![make_entry("vault_item.txt", "/Volumes/OfflineVault/vault_item.txt")],
        );

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![
            primary_root.to_string_lossy().to_string(),
            "/Volumes/OfflineVault".to_string(),
        ];
        cfg.index.exclude_paths = vec![];

        let core = LokiiCore::for_test(cache_dir.path().to_path_buf(), cfg, Arc::new(Vec::new));
        let handler = Arc::new(RecordingHandler::default());
        let _ = core.initialize(handler.clone());
        assert_eq!(core.get_stats().total_count, 3); // workspace + alpha.txt + vault_item.txt
        assert!(!core.is_scanning());

        std::fs::write(primary_root.join("beta.txt"), "b").unwrap();
        assert!(core.rebuild_index(handler.clone()));
        assert_eq!(core.get_stats().total_count, 4);
        assert!(!core.is_scanning());

        let hit_beta = core.search(
            "beta.txt".to_string(),
            FfiSearchMode::Substring,
            FfiCaseSensitivity::Auto,
        );
        assert_eq!(hit_beta.results.len(), 1);
        assert!(!hit_beta.results[0].is_offline);

        let hit_vault = core.search(
            "vault_item.txt".to_string(),
            FfiSearchMode::Substring,
            FfiCaseSensitivity::Auto,
        );
        assert_eq!(hit_vault.results.len(), 1);
        assert!(hit_vault.results[0].is_offline);

        core.persist_index();
        assert!(volumes_dir.join(format!("{}.cache", rec.volume_id)).exists());
        assert!(cache_dir.path().join("index.cache").exists());
    }

    #[test]
    fn dropping_core_releases_volume_coordinator_without_cycle() {
        let cache_dir = tempfile::tempdir().unwrap();
        let primary_dir = tempfile::tempdir().unwrap();
        let primary_root = primary_dir.path().join("workspace");
        std::fs::create_dir_all(&primary_root).unwrap();
        std::fs::write(primary_root.join("note.txt"), "hello").unwrap();

        let volumes_dir = cache_dir.path().join("volumes");
        seed_volume_cache(
            &volumes_dir,
            "/Volumes/OfflineShare",
            "/Volumes/OfflineShare",
            Some("/Volumes/OfflineShare"),
            "OfflineShare",
            VolumeKind::Network,
            vec![make_entry("doc.txt", "/Volumes/OfflineShare/doc.txt")],
        );

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![
            primary_root.to_string_lossy().to_string(),
            "/Volumes/OfflineShare".to_string(),
        ];
        cfg.index.exclude_paths = vec![];

        let core = LokiiCore::for_test(cache_dir.path().to_path_buf(), cfg, Arc::new(Vec::new));
        let handler = Arc::new(RecordingHandler::default());
        let _ = core.initialize(handler.clone());
        core.start_watcher(handler);

        let weak_volumes = Arc::downgrade(&core.volumes);
        assert!(
            weak_volumes.upgrade().is_some(),
            "VolumeCoordinator must be alive while LokiiCore owns it"
        );

        drop(core);
        assert!(
            weak_volumes.upgrade().is_none(),
            "Dropping LokiiCore must release VolumeCoordinator with no Arc cycle"
        );
    }

    #[test]
    fn progress_poller_and_cold_start_emit_scanning_progress() {
        let cache_dir = tempfile::tempdir().unwrap();
        let primary_dir = tempfile::tempdir().unwrap();
        let primary_root = primary_dir.path().join("workspace");
        std::fs::create_dir_all(&primary_root).unwrap();
        for i in 0..10 {
            std::fs::write(primary_root.join(format!("item_{i}.txt")), "x").unwrap();
        }

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![primary_root.to_string_lossy().to_string()];
        cfg.index.exclude_paths = vec![];

        let core = LokiiCore::for_test(cache_dir.path().to_path_buf(), cfg, Arc::new(Vec::new));
        let handler = Arc::new(RecordingHandler::default());
        let mode = core.initialize(handler.clone());
        assert!(matches!(
            mode,
            FfiStartupMode::ColdStart { scanned_count: 11 }
        ));

        let events = handler.events.lock().unwrap();
        assert!(
            events.iter().any(|e| e.starts_with("scanning:")),
            "Cold start must emit scanning progress events, got: {:?}",
            *events
        );
        assert!(
            events.iter().any(|e| e == "scanned:11"),
            "Cold start must emit final scanned count, got: {:?}",
            *events
        );

        // Verify ProgressPoller observes live atomic increments while active.
        let poll_handler = Arc::new(RecordingHandler::default());
        let live_progress = Arc::new(AtomicUsize::new(7));
        let poller = ProgressPoller::start(
            "scanning",
            Arc::clone(&live_progress),
            poll_handler.clone(),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if poll_handler
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|e| e == "scanning:42")
            {
                break;
            }
            live_progress.store(42, Ordering::Relaxed);
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        drop(poller);
        assert!(poll_handler
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e == "scanning:42"));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_path_accessible_guards_non_ascii_traversal_and_symlinks() {
        use std::os::unix::fs::symlink;

        let cache_dir = tempfile::tempdir().unwrap();
        let local_dir = tempfile::tempdir().unwrap();
        let local_workspace = local_dir.path().join("local_ws");
        std::fs::create_dir_all(&local_workspace).unwrap();
        let local_file = local_workspace.join("local.txt");
        std::fs::write(&local_file, "ok").unwrap();

        let volumes_dir = cache_dir.path().join("volumes");
        seed_volume_cache(
            &volumes_dir,
            "/Volumes/OfflineNAS/Projects",
            "/Volumes/OfflineNAS/Projects",
            Some("/Volumes/OfflineNAS"),
            "OfflineNAS/Projects",
            VolumeKind::Network,
            vec![make_entry(
                "spec.md",
                "/Volumes/OfflineNAS/Projects/spec.md",
            )],
        );

        let online_vol_root = tempfile::tempdir().unwrap();
        let _ = online_vol_root; // Only mocked as mounted non-ASCII share path
        let mounted_list = vec![crate::volumes::MountedFsInfo {
            mount_point: "/Volumes/共享盘".to_string(),
            fstype: "smbfs".to_string(),
            from_name: "//nas.local/共享盘".to_string(),
            is_local: false,
            uuid: None,
            total_bytes: 1_000_000,
            volume_name: "共享盘".to_string(),
        }];

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![
            local_workspace.to_string_lossy().to_string(),
            "/Volumes/OfflineNAS/Projects".to_string(),
        ];
        cfg.index.exclude_paths = vec![];

        let core = LokiiCore::for_test(
            cache_dir.path().to_path_buf(),
            cfg,
            Arc::new(move || mounted_list.clone()),
        );
        let handler = Arc::new(RecordingHandler::default());
        let _ = core.initialize(handler);

        // Non-ASCII mounted volume passes; sibling prefix boundary fails.
        assert!(core
            .ensure_path_accessible("/Volumes/共享盘/项目/报告.pdf")
            .is_ok());
        assert!(matches!(
            core.ensure_path_accessible("/Volumes/共享盘2/报告.pdf"),
            Err(FfiError::IoError { .. })
        ));

        // Lexical `..` traversal into offline or unmounted volumes is rejected.
        assert!(matches!(
            core.ensure_path_accessible("/Volumes/共享盘/../OfflineNAS/Projects/spec.md"),
            Err(FfiError::IoError { .. })
        ));
        assert!(matches!(
            core.ensure_path_accessible("/Users/me/../../Volumes/OfflineNAS/Projects/spec.md"),
            Err(FfiError::IoError { .. })
        ));

        // Local symlink targeting offline volume (even non-existent subpath) is rejected.
        let offline_link = local_workspace.join("shortcut_offline");
        symlink("/Volumes/OfflineNAS/Projects", &offline_link).unwrap();
        let via_symlink = offline_link.join("missing_nested/spec.md");
        assert!(matches!(
            core.open_file(via_symlink.to_string_lossy().to_string()),
            Err(FfiError::IoError { .. })
        ));

        // Local symlink targeting online mounted non-ASCII volume passes.
        let online_link = local_workspace.join("shortcut_online");
        symlink("/Volumes/共享盘/项目", &online_link).unwrap();
        assert!(core
            .ensure_path_accessible(&online_link.join("报告.pdf").to_string_lossy())
            .is_ok());

        // Local symlink to ordinary local file passes.
        let local_link = local_workspace.join("shortcut_local.txt");
        symlink(&local_file, &local_link).unwrap();
        assert!(core
            .ensure_path_accessible(&local_link.to_string_lossy())
            .is_ok());

        // Cyclic symlink returns Err cleanly without hanging.
        let loop_a = local_workspace.join("loop_a");
        let loop_b = local_workspace.join("loop_b");
        symlink(&loop_b, &loop_a).unwrap();
        symlink(&loop_a, &loop_b).unwrap();
        assert!(matches!(
            core.ensure_path_accessible(&loop_a.join("file.txt").to_string_lossy()),
            Err(FfiError::IoError { .. })
        ));

        // Boot volume symlink `/Volumes/Macintosh HD -> /` passes for local paths and still
        // catches local symlinks pointing back to offline volumes.
        let statuses = core.volumes.statuses();
        let mounts = core.volumes.mounted_volumes();
        let boot_reader = |entry: &Path| {
            if entry == Path::new("/Volumes/Macintosh HD") {
                Some(PathBuf::from("/"))
            } else {
                None
            }
        };
        let via_boot_local = format!("/Volumes/Macintosh HD{}", local_file.to_string_lossy());
        assert!(ensure_path_accessible_with(
            &via_boot_local,
            &statuses,
            &mounts,
            &boot_reader
        )
        .is_ok());
        let via_boot_to_offline = format!(
            "/Volumes/Macintosh HD{}/spec.md",
            offline_link.to_string_lossy()
        );
        assert!(matches!(
            ensure_path_accessible_with(
                &via_boot_to_offline,
                &statuses,
                &mounts,
                &boot_reader
            ),
            Err(FfiError::IoError { .. })
        ));
    }

    #[test]
    fn is_scanning_reports_true_while_volume_is_syncing() {
        let cache_dir = tempfile::tempdir().unwrap();
        let vol_root = tempfile::tempdir().unwrap();
        let vol_path = vol_root.path().to_string_lossy().to_string();
        std::fs::write(vol_root.path().join("asset.txt"), "data").unwrap();

        // Gate inside mount_lister lets the test hold a background rescan worker deterministically
        // without blocking foreground `rescan_volume` calls on the test thread.
        let main_thread = std::thread::current().id();
        let gate = Arc::new((Mutex::new((false, false)), std::sync::Condvar::new()));
        let gate_reader = Arc::clone(&gate);
        let vol_path_clone = vol_path.clone();

        let mount_lister: Arc<dyn Fn() -> Vec<crate::volumes::MountedFsInfo> + Send + Sync> =
            Arc::new(move || {
                if std::thread::current().id() != main_thread {
                    let (lock, cvar) = &*gate_reader;
                    let mut state = lock.lock().unwrap();
                    if state.0 {
                        state.1 = true;
                        cvar.notify_all();
                        while state.0 {
                            state = cvar.wait(state).unwrap();
                        }
                    }
                }
                vec![crate::volumes::MountedFsInfo {
                    mount_point: vol_path_clone.clone(),
                    fstype: "smbfs".to_string(),
                    from_name: "//nas.local/HoldVol".to_string(),
                    is_local: false,
                    uuid: None,
                    total_bytes: 1_000_000,
                    volume_name: "HoldVol".to_string(),
                }]
            });

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![vol_path.clone()];
        cfg.index.exclude_paths = vec![];

        let core = LokiiCore::for_test(cache_dir.path().to_path_buf(), cfg, mount_lister);
        let handler = Arc::new(RecordingHandler::default());
        let _ = core.initialize(handler);

        // Wait for initial online scan to finish before arming gate.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if !core.is_scanning()
                && core
                    .get_volume_statuses()
                    .first()
                    .is_some_and(|s| s.state == FfiVolumeState::Online)
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        assert!(!core.is_scanning());

        // Arm gate, trigger rescan_volume, wait for worker to pause inside Syncing scan.
        {
            let (lock, _cvar) = &*gate;
            let mut state = lock.lock().unwrap();
            state.0 = true;
            state.1 = false;
        }
        assert!(core.rescan_volume(vol_path).unwrap());

        {
            let (lock, cvar) = &*gate;
            let mut state = lock.lock().unwrap();
            while !state.1 {
                state = cvar.wait(state).unwrap();
            }
        }

        let statuses = core.get_volume_statuses();
        assert_eq!(statuses[0].state, FfiVolumeState::Syncing);
        assert!(
            core.is_scanning(),
            "is_scanning must return true while any volume is in Syncing state"
        );

        // Release gate and confirm is_scanning returns false once scan completes.
        {
            let (lock, cvar) = &*gate;
            let mut state = lock.lock().unwrap();
            state.0 = false;
            cvar.notify_all();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if !core.is_scanning() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        assert!(!core.is_scanning());
    }

    #[test]
    fn startup_config_error_is_delivered_once_on_initialize() {
        let cache_dir = tempfile::tempdir().unwrap();
        let primary_dir = tempfile::tempdir().unwrap();
        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![primary_dir.path().to_string_lossy().to_string()];
        cfg.index.exclude_paths = vec![];

        let core = LokiiCore::for_test(cache_dir.path().to_path_buf(), cfg, Arc::new(Vec::new));
        *core.startup_config_error.lock().unwrap() =
            Some("Config parse error (backed up to config.toml.bak)".to_string());

        let handler = Arc::new(RecordingHandler::default());
        let _ = core.initialize(handler.clone());
        let events = handler.events.lock().unwrap().clone();
        assert!(
            events
                .iter()
                .any(|e| e == "Config parse error (backed up to config.toml.bak)"),
            "startup config parse error must be forwarded via on_watcher_error, got: {events:?}"
        );

        // 二次初始化不应重复消费已上报的启动错误
        let handler2 = Arc::new(RecordingHandler::default());
        let _ = core.initialize(handler2.clone());
        let events2 = handler2.events.lock().unwrap().clone();
        assert!(
            !events2
                .iter()
                .any(|e| e.contains("config.toml.bak")),
            "startup config error must only be delivered once"
        );
    }

    #[test]
    fn update_config_from_json_writes_atomically_and_rolls_back_on_failure() {
        let cache_dir = tempfile::tempdir().unwrap();
        let cfg = AppConfig::default();
        let core = LokiiCore::for_test(cache_dir.path().to_path_buf(), cfg.clone(), Arc::new(Vec::new));

        assert!(core.update_config_from_json(r#"{"index":{"max_results":250}}"#.to_string()));
        assert_eq!(core.config_store.read().index.max_results, 250);
        let parsed =
            AppConfig::from_toml(&std::fs::read_to_string(cache_dir.path().join("config.toml")).unwrap())
                .unwrap();
        assert_eq!(parsed.index.max_results, 250);

        // Replace config.toml with a directory so atomic rename fails, verifying no
        // concurrent reader ever observes the uncommitted patch value.
        let config_path = cache_dir.path().join("config.toml");
        std::fs::remove_file(&config_path).unwrap();
        std::fs::create_dir(&config_path).unwrap();

        let core = Arc::new(core);
        let stop_reader = Arc::new(AtomicBool::new(false));
        let saw_dirty = Arc::new(AtomicBool::new(false));
        let core_reader = Arc::clone(&core);
        let stop_flag = Arc::clone(&stop_reader);
        let dirty_flag = Arc::clone(&saw_dirty);
        let reader_handle = std::thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                if core_reader.config_store.read().index.max_results == 777 {
                    dirty_flag.store(true, Ordering::SeqCst);
                }
            }
        });
        for _ in 0..20 {
            assert!(!core.update_config_from_json(r#"{"index":{"max_results":777}}"#.to_string()));
        }
        stop_reader.store(true, Ordering::Relaxed);
        reader_handle.join().unwrap();
        assert!(
            !saw_dirty.load(Ordering::SeqCst),
            "concurrent ConfigStore reader must never observe uncommitted config on write failure"
        );
        assert_eq!(core.config_store.read().index.max_results, 250);
        let leftovers: Vec<_> = std::fs::read_dir(cache_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files after failure: {leftovers:?}");
    }

    #[test]
    fn concurrent_update_config_from_json_serializes_without_lost_updates() {
        let cache_dir = tempfile::tempdir().unwrap();
        let core = Arc::new(LokiiCore::for_test(
            cache_dir.path().to_path_buf(),
            AppConfig::default(),
            Arc::new(Vec::new),
        ));

        let patches = [
            r#"{"general":{"language":"en"}}"#,
            r#"{"general":{"show_in_dock":false}}"#,
            r#"{"window":{"remember_position":true}}"#,
            r#"{"index":{"max_results":321}}"#,
        ];

        std::thread::scope(|scope| {
            for patch in patches {
                let c = Arc::clone(&core);
                scope.spawn(move || {
                    assert!(c.update_config_from_json(patch.to_string()));
                });
            }
        });

        let in_mem = core.config_store.read().clone();
        let on_disk = AppConfig::from_toml(
            &std::fs::read_to_string(cache_dir.path().join("config.toml")).unwrap(),
        )
        .unwrap();
        assert_eq!(in_mem, on_disk);
        assert_eq!(in_mem.general.language, "en");
        assert!(!in_mem.general.show_in_dock);
        assert!(in_mem.window.remember_position);
        assert_eq!(in_mem.index.max_results, 321);
    }
}
