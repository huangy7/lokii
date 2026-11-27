use crate::cache;
use crate::compact_index::CompactIndex;
use crate::config::{AppConfig, ConfigStore, IndexConfig};
use crate::filter::CompiledFilter;
use crate::fsevents;
use crate::indexer::IndexStore;
use crate::path_utils::{path_starts_with_ci, paths_equal_ci};
use crate::scanner::{self, VolumeScanError};
use crate::scope_diff::{diff_volume_scope, VolumeScopeAction, VolumeScopeSnapshot};
pub use crate::scope_diff::VolumeConfirmReason;
use crate::search::{
    self, CaseSensitivity, MultiSearchResult, QueryFilter, SearchMode, SearchShard,
};
use crate::volumes::{
    fstype_supports_persistent_fsevents_journal, is_rotational_device,
    legacy_volume_filter_fingerprint, list_mounted_volumes, list_mounted_volumes_fast,
    normalize_network_source, partition_index_paths, strip_mount_drift_suffix, MountedFsInfo,
    MountedPathTarget, ResolvedMountMatch, VolumeIdentity, VolumeKind, VolumeManifest,
    VolumeRecord,
};
use crate::watcher::{
    self, apply_batch, resolve_changes, Change, DEFAULT_DEBOUNCE_MS, MAX_BATCH_SIZE, MAX_DEBOUNCE,
    PERSIST_EVENTS, PERSIST_EVERY,
};
use parking_lot::{Condvar, Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 生产环境已有 NSWorkspace 实时挂载/卸载通知，监控线程仅作兜底与网络卷周期重扫触发；
/// 单测保持 3 秒周期避免拉长异步等待测试耗时。
#[cfg(not(test))]
const MONITOR_POLL_INTERVAL: Duration = Duration::from_secs(10);
#[cfg(test)]
const MONITOR_POLL_INTERVAL: Duration = Duration::from_secs(3);
const UNMOUNT_GRACE_PERIOD: Duration = Duration::from_secs(15);
/// shortcut: 固定 4 个卷任务 worker；每个卡死的卷各占住一个，其余卷继续，卷更多且多个同时卡死时仍会排队，按实测再调。
const WORKER_THREADS: usize = 4;
const VOLUME_GAP_QUIET: Duration = Duration::from_secs(2);
const VOLUME_GAP_REPLAYS: u8 = 3;
const SCAN_PROGRESS_NOTIFY_INTERVAL: Duration = Duration::from_millis(150);

type JournalUuidProvider = Arc<dyn Fn(&Path) -> Option<String> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolumeState {
    Online,
    Syncing,
    NeedsConfirmation,
    Offline,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeStatusInfo {
    pub volume_id: String,
    pub configured_path: String,
    pub active_path: Option<String>,
    pub display_name: String,
    pub kind: VolumeKind,
    pub state: VolumeState,
    pub entry_count: u64,
    pub last_sync_at: Option<u64>,
    pub is_remounted: bool,
    pub has_cache: bool,
    pub confirm_reason: Option<VolumeConfirmReason>,
    pub scanned_count: u64,
    pub supports_persistent_journal: bool,
}

struct VolumeStreamHandle {
    stream: Arc<Mutex<Option<fsevents::Stream>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for VolumeStreamHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Stop and release the FSEventStream first so the sender channel closes
        // and unblocks the consumer thread before joining.
        drop(self.stream.lock().take());
        if let Some(handle) = self.thread.take() {
            if handle.thread().id() != std::thread::current().id() {
                let _ = handle.join();
            }
        }
    }
}

fn str_paths_equal_ci(a: &str, b: &str) -> bool {
    paths_equal_ci(Path::new(a), Path::new(b))
}

struct CachePersistence {
    applied_events: usize,
    saved_events: usize,
    last_save: Instant,
}

impl Default for CachePersistence {
    fn default() -> Self {
        Self {
            applied_events: 0,
            saved_events: 0,
            last_save: Instant::now(),
        }
    }
}

impl CachePersistence {
    fn is_due(&self, now: Instant) -> bool {
        self.applied_events.saturating_sub(self.saved_events) >= PERSIST_EVENTS
            || now.duration_since(self.last_save) >= PERSIST_EVERY
    }
}

struct VolumeSlot {
    volume_id: String,
    configured_path: String,
    /// Prefix reflected in `store`; retained across offline periods so alias searches
    /// and reverse mount rebases know the previous path prefix.
    active_configured_path: String,
    active_mount_root: Option<String>,
    subpath: String,
    display_name: String,
    kind: VolumeKind,
    fstype: String,
    from_name: String,
    identity: VolumeIdentity,
    state: VolumeState,
    entry_count: u64,
    last_sync_at: Option<u64>,
    last_event_id: Option<u64>,
    journal_uuid: Option<String>,
    scope_snapshot: Option<VolumeScopeSnapshot>,
    dirty_upper_bound: bool,
    pending_confirm_reason: Option<VolumeConfirmReason>,
    in_flight_scan_snapshot: Option<VolumeScopeSnapshot>,
    in_flight_prune_snapshot: Option<VolumeScopeSnapshot>,
    stream_snapshot: Option<VolumeScopeSnapshot>,
    scanned_count: Arc<AtomicU64>,
    is_remounted: bool,
    has_cache: bool,
    dirty_cache: bool,
    cache_seq: u64,
    saved_cache_seq: u64,
    cache_persistence: CachePersistence,
    generation: u64,
    cancel_token: Arc<AtomicBool>,
    store: IndexStore,
    stream_handle: Option<VolumeStreamHandle>,
    last_rescan_triggered: Option<Instant>,
    /// 最近一次扫描耗时，用于对慢速网络卷做周期重扫自适应冷却，防止扫完立刻再扫
    last_scan_duration: Option<Duration>,
    /// 扫描中遇到临时断连转离线后的下次自动重连探测时刻，防止假离线永久卡死又避免死卷高频重试
    next_reconnect_retry: Option<Instant>,
    unmounting_until: Option<Instant>,
    skip_missing_rescan: bool,
}

impl VolumeSlot {
    fn to_record(&self) -> VolumeRecord {
        VolumeRecord {
            volume_id: self.volume_id.clone(),
            configured_path: self.configured_path.clone(),
            active_configured_path: self.active_configured_path.clone(),
            active_mount_root: self.active_mount_root.clone(),
            subpath: self.subpath.clone(),
            display_name: self.display_name.clone(),
            kind: self.kind,
            fstype: self.fstype.clone(),
            identity: self.identity.clone(),
            last_sync_at: self.last_sync_at,
            last_event_id: self.last_event_id,
            journal_uuid: self.journal_uuid.clone(),
            entry_count: self.entry_count,
            scope_snapshot: self.scope_snapshot.clone(),
            dirty_upper_bound: self.dirty_upper_bound,
            pending_confirm_reason: self.pending_confirm_reason,
            dirty_filter: false,
            filter_fingerprint: None,
        }
    }

    fn status_info(&self) -> VolumeStatusInfo {
        let active_path = match self.state {
            VolumeState::Online
            | VolumeState::Syncing
            | VolumeState::NeedsConfirmation
            | VolumeState::Missing => Some(self.active_configured_path.clone()),
            VolumeState::Offline => None,
        };
        VolumeStatusInfo {
            volume_id: self.volume_id.clone(),
            configured_path: self.configured_path.clone(),
            active_path,
            display_name: self.display_name.clone(),
            kind: self.kind,
            state: self.state,
            entry_count: self.entry_count,
            last_sync_at: self.last_sync_at,
            is_remounted: self.is_remounted,
            has_cache: self.has_cache,
            confirm_reason: (self.state == VolumeState::NeedsConfirmation)
                .then_some(self.pending_confirm_reason)
                .flatten(),
            scanned_count: if self.state == VolumeState::Syncing {
                self.scanned_count.load(Ordering::Relaxed)
            } else {
                0
            },
            supports_persistent_journal: self.kind == VolumeKind::LocalExternal
                && fstype_supports_persistent_fsevents_journal(&self.fstype),
        }
    }

    fn checkpoint(&self) -> Option<cache::Checkpoint> {
        self.last_event_id.map(|event_id| cache::Checkpoint {
            event_id,
            fingerprint: self
                .scope_snapshot
                .as_ref()
                .map_or(0, VolumeScopeSnapshot::fingerprint_fnv1a),
        })
    }

    fn cache_save(&self, volumes_dir: &Path) -> CacheSave {
        CacheSave {
            volume_id: self.volume_id.clone(),
            generation: self.generation,
            cache_seq: self.cache_seq,
            applied_events: self.cache_persistence.applied_events,
            cache_path: volumes_dir.join(format!("{}.cache", self.volume_id)),
            store: self.store.clone(),
            checkpoint: self.checkpoint(),
        }
    }

    fn matches_path_or_id(&self, path_or_id: &str) -> bool {
        self.volume_id == path_or_id
            || str_paths_equal_ci(&self.configured_path, path_or_id)
            || str_paths_equal_ci(&self.active_configured_path, path_or_id)
    }

    fn matches_mount_path(&self, mount_path: &Path) -> bool {
        if let Some(mr) = self.active_mount_root.as_deref() {
            path_starts_with_ci(Path::new(mr), mount_path)
                || path_starts_with_ci(Path::new(&self.active_configured_path), mount_path)
        } else {
            path_starts_with_ci(Path::new(&self.active_configured_path), mount_path)
                || path_starts_with_ci(Path::new(&self.configured_path), mount_path)
        }
    }

    fn live_stream_allowed(&self) -> bool {
        self.state == VolumeState::Online
            || (self.state == VolumeState::NeedsConfirmation
                && self
                    .pending_confirm_reason
                    .is_some_and(VolumeConfirmReason::allows_live_stream))
    }
}

struct CoordinatorInner {
    config: AppConfig,
    manifest: VolumeManifest,
    slots: Vec<VolumeSlot>,
    on_status_change: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    prune_runs: usize,
    #[cfg(test)]
    scan_runs: usize,
}

struct ScanJob {
    volume_id: String,
    generation: u64,
    kind: VolumeKind,
    fstype: String,
    mount_root: PathBuf,
    subpath: String,
    root: PathBuf,
    target_snapshot: VolumeScopeSnapshot,
    use_serial_scan: bool,
    scanned_count: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}

struct AttachStreamJob {
    volume_id: String,
    generation: u64,
    mount_root: PathBuf,
    subpath: String,
    fstype: String,
    active_path: PathBuf,
    since_event_id: Option<u64>,
    stored_journal_uuid: Option<String>,
    require_journal_verify: bool,
    target_snapshot: VolumeScopeSnapshot,
    cancel: Arc<AtomicBool>,
    store: IndexStore,
}

enum WorkerTask {
    Scan(ScanJob),
    AttachStream(AttachStreamJob),
    Prune(PruneJob),
}

impl WorkerTask {
    fn volume_id(&self) -> &str {
        match self {
            WorkerTask::Scan(job) => &job.volume_id,
            WorkerTask::AttachStream(job) => &job.volume_id,
            WorkerTask::Prune(job) => &job.volume_id,
        }
    }

    fn generation(&self) -> u64 {
        match self {
            WorkerTask::Scan(job) => job.generation,
            WorkerTask::AttachStream(job) => job.generation,
            WorkerTask::Prune(job) => job.generation,
        }
    }

    fn is_cancelled(&self) -> bool {
        match self {
            WorkerTask::Scan(job) => job.cancel.load(Ordering::SeqCst),
            WorkerTask::AttachStream(job) => job.cancel.load(Ordering::SeqCst),
            WorkerTask::Prune(job) => job.cancel.load(Ordering::SeqCst),
        }
    }
}

struct WorkerQueueState {
    shutdown: bool,
    queue: VecDeque<WorkerTask>,
    /// 正在执行的卷：同一卷的扫描/挂流串行，不同卷之间并行。
    running: HashSet<String>,
}

struct WorkerQueue {
    state: Mutex<WorkerQueueState>,
    cvar: Condvar,
}

impl WorkerQueue {
    fn new() -> Self {
        Self {
            state: Mutex::new(WorkerQueueState {
                shutdown: false,
                queue: VecDeque::new(),
                running: HashSet::new(),
            }),
            cvar: Condvar::new(),
        }
    }

    fn enqueue(&self, task: WorkerTask) {
        if task.is_cancelled() {
            return;
        }
        let mut guard = self.state.lock();
        if guard.shutdown {
            return;
        }
        let vid = task.volume_id();
        let incoming_gen = task.generation();
        if guard.queue.iter().any(|existing| {
            existing.volume_id() == vid
                && !existing.is_cancelled()
                && existing.generation() > incoming_gen
        }) {
            return;
        }
        guard.queue.retain(|existing| existing.volume_id() != vid);
        guard.queue.push_back(task);
        self.cvar.notify_all();
    }

    fn remove_volume(&self, volume_id: &str) {
        let mut guard = self.state.lock();
        guard.queue.retain(|existing| existing.volume_id() != volume_id);
    }

    /// 取出首个所属卷当前不在执行的任务，并把它标记为执行中。
    /// 返回的 guard 在 drop 时释放标记，调用方无法忘记释放。
    fn pop_blocking(&self) -> Option<(WorkerTask, RunningTaskGuard<'_>)> {
        let mut guard = self.state.lock();
        loop {
            if let Some(idx) = guard
                .queue
                .iter()
                .position(|task| !guard.running.contains(task.volume_id()))
            {
                let task = guard.queue.remove(idx)?;
                let volume_id = task.volume_id().to_string();
                guard.running.insert(volume_id.clone());
                return Some((
                    task,
                    RunningTaskGuard {
                        queue: self,
                        volume_id,
                    },
                ));
            }
            if guard.shutdown {
                return None;
            }
            self.cvar.wait(&mut guard);
        }
    }

    fn finish(&self, volume_id: &str) {
        let mut guard = self.state.lock();
        guard.running.remove(volume_id);
        self.cvar.notify_all();
    }

    fn shutdown(&self) {
        let mut guard = self.state.lock();
        guard.shutdown = true;
        guard.queue.clear();
        self.cvar.notify_all();
    }

    #[cfg(test)]
    fn wait_for_idle(&self, timeout: Duration) -> bool {
        let mut guard = self.state.lock();
        let deadline = Instant::now() + timeout;
        while !guard.shutdown && (!guard.queue.is_empty() || !guard.running.is_empty()) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || self.cvar.wait_for(&mut guard, remaining).timed_out() {
                return guard.shutdown || (guard.queue.is_empty() && guard.running.is_empty());
            }
        }
        true
    }
}

struct CacheSave {
    volume_id: String,
    generation: u64,
    cache_seq: u64,
    applied_events: usize,
    cache_path: PathBuf,
    store: IndexStore,
    checkpoint: Option<cache::Checkpoint>,
}

struct ShardSnapshot {
    store: IndexStore,
    configured_path: String,
    active_configured_path: String,
    display_name: String,
    is_offline: bool,
}

struct PruneJob {
    volume_id: String,
    generation: u64,
    configured_path: String,
    active_configured_path: String,
    target_snapshot: VolumeScopeSnapshot,
    clear_filter_widened: bool,
    reattach_stream_after: bool,
    cancel: Arc<AtomicBool>,
    store: IndexStore,
}

#[derive(Default)]
struct TransitionEffects {
    handles_to_drop: Vec<VolumeStreamHandle>,
    cancelled_volume_ids: Vec<String>,
    files_to_delete: Vec<(String, PathBuf)>,
    cache_saves: Vec<CacheSave>,
    tasks: Vec<WorkerTask>,
    save_manifest: bool,
    notify: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl TransitionEffects {
    fn push_cache_delete(&mut self, volume_id: String, cache_path: PathBuf) {
        if !self.files_to_delete.iter().any(|(vid, _)| vid == &volume_id) {
            self.files_to_delete.push((volume_id, cache_path));
        }
    }
}

fn save_dirty_caches_and_mark(
    inner: &Arc<RwLock<CoordinatorInner>>,
    saves: Vec<CacheSave>,
) -> HashSet<String> {
    let mut saved_ids = HashSet::new();
    for save in saves {
        let path_lock = cache::path_write_lock(&save.cache_path);
        let _path_guard = path_lock.lock();
        let snapshot_meta = {
            let guard = inner.read();
            guard
                .slots
                .iter()
                .find(|s| s.volume_id == save.volume_id && s.generation == save.generation)
                .map(|s| (s.cache_seq, s.saved_cache_seq, s.dirty_cache, s.checkpoint()))
        };
        let (target_seq, target_cp) = match snapshot_meta {
            // Drop saves whose slot was removed or moved to a newer generation so stale
            // snapshots can never overwrite a newer generation's .cache file.
            None => continue,
            Some((current_seq, saved_seq, dirty_cache, current_cp)) => {
                if !dirty_cache && saved_seq >= save.cache_seq && saved_seq >= current_seq {
                    // Another concurrent saver already persisted a snapshot covering this seq.
                    saved_ids.insert(save.volume_id.clone());
                    continue;
                }
                (
                    current_seq.max(save.cache_seq),
                    current_cp.or(save.checkpoint),
                )
            }
        };
        // 仅在短读锁内完成内存字节编码，立即释放读锁后再落盘与 sync_data()，避免阻塞增量写入与并发搜索
        let encoded = {
            let guard = save.store.read();
            cache::encode_compact(&guard)
        };
        if cache::save_encoded_compact_unlocked(&encoded, target_cp, &save.cache_path).is_ok() {
            saved_ids.insert(save.volume_id.clone());
            let mut guard = inner.write();
            if let Some(slot) = guard.slots.iter_mut().find(|s| {
                s.volume_id == save.volume_id && s.generation == save.generation
            }) {
                slot.cache_persistence.saved_events =
                    slot.cache_persistence.saved_events.max(save.applied_events);
                slot.cache_persistence.last_save = Instant::now();
                slot.saved_cache_seq = slot.saved_cache_seq.max(target_seq);
                if slot.cache_seq <= target_seq {
                    slot.dirty_cache = false;
                }
            }
        }
    }
    saved_ids
}

/// Removes an orphan shard cache file under its per-path lock only after re-verifying
/// that no active slot or manifest record has reclaimed `volume_id` in the meantime.
fn unlink_orphan_cache_if_still_absent(
    inner: &Arc<RwLock<CoordinatorInner>>,
    volume_id: &str,
    cache_path: &Path,
) {
    let path_lock = cache::path_write_lock(cache_path);
    let _path_guard = path_lock.lock();
    let still_orphan = {
        let guard = inner.read();
        !guard.slots.iter().any(|s| s.volume_id == volume_id)
            && !guard
                .manifest
                .records
                .iter()
                .any(|r| r.volume_id == volume_id)
    };
    if still_orphan {
        let _ = std::fs::remove_file(cache_path);
    }
}

fn save_latest_manifest_locked(
    inner: &Arc<RwLock<CoordinatorInner>>,
    manifest_save_lock: &Mutex<()>,
    volumes_dir: &Path,
) {
    // Acquire manifest_save_lock before reading inner so the newest in-memory
    // manifest is always serialized in commit order onto disk.
    let _guard = manifest_save_lock.lock();
    let latest_manifest = inner.read().manifest.clone();
    let _ = latest_manifest.save_atomic(volumes_dir);
}

impl TransitionEffects {
    fn execute(
        self,
        inner: &Arc<RwLock<CoordinatorInner>>,
        manifest_save_lock: &Mutex<()>,
        volumes_dir: &Path,
        queue: &WorkerQueue,
    ) {
        drop(self.handles_to_drop);
        for volume_id in &self.cancelled_volume_ids {
            queue.remove_volume(volume_id);
        }
        for (volume_id, path) in self.files_to_delete {
            unlink_orphan_cache_if_still_absent(inner, &volume_id, &path);
        }
        save_dirty_caches_and_mark(inner, self.cache_saves);
        if self.save_manifest {
            save_latest_manifest_locked(inner, manifest_save_lock, volumes_dir);
        }
        for task in self.tasks {
            queue.enqueue(task);
        }
        if let Some(cb) = self.notify {
            cb();
        }
    }
}

/// Manages mounted external/network volume slots, offline caches, remount rebasing,
/// and non-blocking search aggregation across primary and volume shards.
pub struct VolumeCoordinator {
    cache_dir: PathBuf,
    inner: Arc<RwLock<CoordinatorInner>>,
    manifest_save_lock: Arc<Mutex<()>>,
    mount_lister: Arc<dyn Fn() -> Vec<MountedFsInfo> + Send + Sync>,
    fast_mount_lister: Arc<dyn Fn() -> Vec<MountedFsInfo> + Send + Sync>,
    journal_uuid_provider: JournalUuidProvider,
    /// 挂载瞬间等待 `fseventsd` 加载卷 `.fseventsd/fseventsd-uuid` 的最大退避窗口（生产环境 3s，单测默认 0）。
    journal_ready_wait: Duration,
    worker_queue: Arc<WorkerQueue>,
    worker_threads: Mutex<Vec<JoinHandle<()>>>,
    monitor_stop: Arc<(Mutex<bool>, Condvar)>,
    monitor_thread: Mutex<Option<JoinHandle<()>>>,
}

const JOURNAL_READY_MAX_WAIT: Duration = Duration::from_secs(3);

impl VolumeCoordinator {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self::with_providers(
            cache_dir,
            Arc::new(list_mounted_volumes),
            Arc::new(list_mounted_volumes_fast),
            Arc::new(fsevents::journal_uuid_for_path),
            JOURNAL_READY_MAX_WAIT,
        )
    }

    #[cfg(test)]
    pub(crate) fn with_mount_lister(
        cache_dir: PathBuf,
        mount_lister: Arc<dyn Fn() -> Vec<MountedFsInfo> + Send + Sync>,
    ) -> Self {
        let fast_mount_lister = Arc::clone(&mount_lister);
        Self::with_providers(
            cache_dir,
            mount_lister,
            fast_mount_lister,
            Arc::new(fsevents::journal_uuid_for_path),
            Duration::ZERO,
        )
    }

    #[cfg(test)]
    pub(crate) fn with_mount_and_fda_lister(
        cache_dir: PathBuf,
        mount_lister: Arc<dyn Fn() -> Vec<MountedFsInfo> + Send + Sync>,
        _fda_checker: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        let fast_mount_lister = Arc::clone(&mount_lister);
        Self::with_providers(
            cache_dir,
            mount_lister,
            fast_mount_lister,
            Arc::new(fsevents::journal_uuid_for_path),
            Duration::ZERO,
        )
    }

    #[cfg(test)]
    pub(crate) fn with_mount_and_journal_provider(
        cache_dir: PathBuf,
        mount_lister: Arc<dyn Fn() -> Vec<MountedFsInfo> + Send + Sync>,
        journal_uuid_provider: JournalUuidProvider,
    ) -> Self {
        let fast_mount_lister = Arc::clone(&mount_lister);
        Self::with_providers(
            cache_dir,
            mount_lister,
            fast_mount_lister,
            journal_uuid_provider,
            Duration::ZERO,
        )
    }

    fn with_providers(
        cache_dir: PathBuf,
        mount_lister: Arc<dyn Fn() -> Vec<MountedFsInfo> + Send + Sync>,
        fast_mount_lister: Arc<dyn Fn() -> Vec<MountedFsInfo> + Send + Sync>,
        journal_uuid_provider: JournalUuidProvider,
        journal_ready_wait: Duration,
    ) -> Self {
        let default_cfg = AppConfig::default();
        let inner = Arc::new(RwLock::new(CoordinatorInner {
            config: default_cfg,
            manifest: VolumeManifest::default(),
            slots: Vec::new(),
            on_status_change: None,
            #[cfg(test)]
            prune_runs: 0,
            #[cfg(test)]
            scan_runs: 0,
        }));
        let worker_queue = Arc::new(WorkerQueue::new());
        Self {
            cache_dir,
            inner,
            manifest_save_lock: Arc::new(Mutex::new(())),
            mount_lister,
            fast_mount_lister,
            journal_uuid_provider,
            journal_ready_wait,
            worker_queue,
            worker_threads: Mutex::new(Vec::new()),
            monitor_stop: Arc::new((Mutex::new(false), Condvar::new())),
            monitor_thread: Mutex::new(None),
        }
    }

    fn volumes_dir(&self) -> PathBuf {
        self.cache_dir.join("volumes")
    }

    fn ensure_worker_started(&self) {
        let mut guard = self.worker_threads.lock();
        if !guard.is_empty() {
            return;
        }
        for index in 0..WORKER_THREADS {
            let inner = Arc::clone(&self.inner);
            let manifest_save_lock = Arc::clone(&self.manifest_save_lock);
            let queue = Arc::clone(&self.worker_queue);
            let mount_lister = Arc::clone(&self.mount_lister);
            let journal_uuid_provider = Arc::clone(&self.journal_uuid_provider);
            let journal_ready_wait = self.journal_ready_wait;
            let volumes_dir = self.volumes_dir();
            let handle = std::thread::Builder::new()
                .name(format!("volume-worker-{index}"))
                .spawn(move || {
                    run_worker_loop(
                        inner,
                        manifest_save_lock,
                        queue,
                        mount_lister,
                        journal_uuid_provider,
                        journal_ready_wait,
                        volumes_dir,
                    )
                })
                .expect("spawn volume-worker");
            guard.push(handle);
        }
    }

    fn execute_effects(&self, effects: TransitionEffects) {
        if !effects.tasks.is_empty() {
            self.ensure_worker_started();
        }
        let volumes_dir = self.volumes_dir();
        effects.execute(
            &self.inner,
            &self.manifest_save_lock,
            &volumes_dir,
            &self.worker_queue,
        );
    }

    pub fn peek_has_any_cache(&self, config: &AppConfig) -> bool {
        if self.cache_dir.join("index.cache").exists() {
            return true;
        }
        let volumes_dir = self.volumes_dir();
        let manifest = VolumeManifest::load_from_dir(&volumes_dir);
        let configured: Vec<PathBuf> = config
            .index
            .index_paths
            .iter()
            .map(|raw| crate::config::expand_tilde(raw))
            .collect();
        manifest.records.iter().any(|record| {
            let configured_match = configured
                .iter()
                .any(|p| paths_equal_ci(p, Path::new(&record.configured_path)));
            configured_match
                && volumes_dir
                    .join(format!("{}.cache", record.volume_id))
                    .exists()
        })
    }

    /// Return mounted filesystems via the coordinator's mount lister.
    pub fn mounted_volumes(&self) -> Vec<MountedFsInfo> {
        (self.mount_lister)()
    }

    /// 轻量挂载表查询（生产环境仅读取内核 `getfsstat(MNT_NOWAIT)`，跳过外置卷 UUID `getattrlist`），
    /// 供文件打开/定位可达性校验调用，避免唤醒休眠机械硬盘。
    pub fn mounted_volumes_fast(&self) -> Vec<MountedFsInfo> {
        (self.fast_mount_lister)()
    }

    pub fn initialize(
        &self,
        config: &AppConfig,
        on_status_change: Arc<dyn Fn() + Send + Sync>,
    ) -> u64 {
        let volumes_dir = self.volumes_dir();
        let manifest = VolumeManifest::load_from_dir(&volumes_dir);
        let mounts = (self.mount_lister)();
        let partition = partition_index_paths(config, &manifest, &mounts);

        let (loaded_count, effects) = {
            let (old_handles, manifest_snapshot) = {
                let mut inner = self.inner.write();
                let mut handles = Vec::new();
                for slot in inner.slots.drain(..) {
                    slot.cancel_token.store(true, Ordering::SeqCst);
                    if let Some(h) = slot.stream_handle {
                        handles.push(h);
                    }
                }
                inner.config = config.clone();
                inner.manifest = manifest;
                inner.on_status_change = Some(on_status_change);
                let snap = inner.manifest.clone();
                (handles, snap)
            };

            // Pre-load local volume caches outside the coordinator lock using the cloned
            // manifest snapshot so inner.read() is never entered re-entrantly.
            let mut preloaded = preload_missing_target_caches(
                &self.inner,
                &manifest_snapshot,
                &partition.mounted_targets,
                &mounts,
                &volumes_dir,
            );

            let mut inner = self.inner.write();
            let mut effects = TransitionEffects::default();
            effects.handles_to_drop = old_handles;

            let empty_added = HashSet::new();
            let loaded = reconcile_partition_locked(
                &mut inner,
                &partition.mounted_targets,
                &mounts,
                &volumes_dir,
                &mut preloaded,
                false,
                &empty_added,
                &mut effects,
            );
            (loaded, effects)
        };

        self.execute_effects(effects);
        loaded_count
    }

    pub fn start_monitor(
        &self,
        config_store: ConfigStore,
        on_status_change: Arc<dyn Fn() + Send + Sync>,
    ) {
        let mut guard = self.monitor_thread.lock();
        if guard.is_some() {
            return;
        }
        self.inner.write().on_status_change = Some(on_status_change);

        let inner = Arc::clone(&self.inner);
        let manifest_save_lock = Arc::clone(&self.manifest_save_lock);
        let mount_lister = Arc::clone(&self.mount_lister);
        let fast_mount_lister = Arc::clone(&self.fast_mount_lister);
        let worker_queue = Arc::clone(&self.worker_queue);
        let volumes_dir = self.volumes_dir();
        let stop = Arc::clone(&self.monitor_stop);
        // Start worker thread unconditionally when monitor is active so monitor-detected mounts can enqueue scans.
        self.ensure_worker_started();

        let handle = std::thread::Builder::new()
            .name("volume-monitor".to_string())
            .spawn(move || {
                let (lock, cvar) = &*stop;
                let mut last_fast_sig: Option<Vec<(String, String, String, bool)>> = None;
                let mut last_mounts: Option<Vec<MountedFsInfo>> = None;
                let mut last_index_cfg: Option<IndexConfig> = None;
                loop {
                    {
                        let mut stopped = lock.lock();
                        if *stopped {
                            break;
                        }
                        let _ = cvar.wait_for(&mut stopped, MONITOR_POLL_INTERVAL);
                        if *stopped {
                            break;
                        }
                    }

                    let (cfg_changed, rescan_enabled, rescan_minutes, cfg_opt) = {
                        let guard = config_store.read();
                        let rescan_enabled = guard.index.network_rescan_enabled;
                        let rescan_mins = guard.index.network_rescan_minutes;
                        if last_index_cfg.as_ref() == Some(&guard.index) {
                            (false, rescan_enabled, rescan_mins, None)
                        } else {
                            let inner_guard = inner.read();
                            if inner_guard.config.index == guard.index {
                                // 已由前序 on_config_updated 完成对账，仅同步本地游标避免重复全量对账
                                last_index_cfg = Some(guard.index.clone());
                                (false, rescan_enabled, rescan_mins, None)
                            } else {
                                (true, rescan_enabled, rescan_mins, Some(guard.clone()))
                            }
                        }
                    };

                    // 先通过零 getattrlist 的轻量挂载表探测是否发生挂载点/设备变化，避免定期唤醒休眠机械硬盘
                    let fast_mounts = (fast_mount_lister)();
                    let fast_sig: Vec<(String, String, String, bool)> = fast_mounts
                        .iter()
                        .map(|m| {
                            (
                                m.mount_point.clone(),
                                m.from_name.clone(),
                                m.fstype.clone(),
                                m.is_local,
                            )
                        })
                        .collect();
                    let fast_changed = last_fast_sig.as_ref() != Some(&fast_sig);

                    let now = Instant::now();
                    let has_pending_grace_or_retry = {
                        let guard = inner.read();
                        guard.slots.iter().any(|slot| {
                            slot.unmounting_until.is_some()
                                || (slot.state == VolumeState::Offline
                                    && slot.next_reconnect_retry.is_some_and(|until| now >= until))
                        })
                    };

                    if cfg_changed {
                        let mounts = (mount_lister)();
                        let cfg = cfg_opt.unwrap_or_else(|| config_store.read().clone());
                        last_index_cfg = Some(cfg.index.clone());
                        last_fast_sig = Some(fast_sig);
                        last_mounts = Some(mounts.clone());
                        let effects = apply_config_update(&inner, &mounts, &volumes_dir, &cfg);
                        effects.execute(&inner, &manifest_save_lock, &volumes_dir, &worker_queue);
                    } else if fast_changed || has_pending_grace_or_retry {
                        let mounts = (mount_lister)();
                        let mounts_changed = last_mounts.as_ref() != Some(&mounts);
                        last_fast_sig = Some(fast_sig);
                        let needs_slot_reconcile = {
                            let guard = inner.read();
                            guard.slots.iter().any(|slot| {
                                slot.unmounting_until.is_some()
                                    || (slot.state == VolumeState::Offline
                                        && slot
                                            .next_reconnect_retry
                                            .is_none_or(|until| now >= until)
                                        && resolve_slot_mount(slot, &mounts).is_some())
                            })
                        };
                        if mounts_changed || needs_slot_reconcile {
                            last_mounts = Some(mounts.clone());
                            let effects =
                                refresh_mounts_locked(&inner, &mounts, &volumes_dir, false);
                            effects.execute(
                                &inner,
                                &manifest_save_lock,
                                &volumes_dir,
                                &worker_queue,
                            );
                        }
                    }

                    // Periodic rescan for online network volumes when explicitly enabled.
                    if let Some(base_interval) =
                        effective_network_rescan_interval(rescan_enabled, rescan_minutes)
                    {
                        let now_unix = current_unix_secs();
                        let any_due = {
                            let guard = inner.read();
                            guard.slots.iter().any(|slot| {
                                network_slot_rescan_due(slot, base_interval, now_unix)
                            })
                        };
                        if any_due {
                            let rescan_effects = {
                                let mut guard = inner.write();
                                let cfg_snap = guard.config.clone();
                                let mut eff = TransitionEffects::default();
                                let mut changed = false;
                                #[cfg(test)]
                                let mut started_scans = 0usize;
                                for slot in &mut guard.slots {
                                    if network_slot_rescan_due(slot, base_interval, now_unix) {
                                        let target_snapshot = VolumeScopeSnapshot::from_config(
                                            &cfg_snap,
                                            Path::new(&slot.configured_path),
                                            Path::new(&slot.active_configured_path),
                                        );
                                        prepare_slot_for_scan(
                                            slot,
                                            &cfg_snap,
                                            target_snapshot,
                                            &mut eff,
                                        );
                                        #[cfg(test)]
                                        {
                                            started_scans += 1;
                                        }
                                        changed = true;
                                    }
                                }
                                #[cfg(test)]
                                {
                                    guard.scan_runs += started_scans;
                                }
                                if changed {
                                    eff.notify = guard.on_status_change.clone();
                                }
                                eff
                            };
                            rescan_effects.execute(
                                &inner,
                                &manifest_save_lock,
                                &volumes_dir,
                                &worker_queue,
                            );
                        }
                    }

                    persist_dirty_caches(&inner, &manifest_save_lock, &volumes_dir);
                }
            })
            .expect("spawn volume-monitor");
        *guard = Some(handle);
    }

    pub fn search(
        &self,
        primary: &IndexStore,
        query: &str,
        mode: SearchMode,
        case_sensitivity: CaseSensitivity,
        max_results: usize,
        include_offline: bool,
    ) -> (Vec<MultiSearchResult>, bool, Option<String>, SearchMode) {
        self.search_with_filter(
            primary,
            query,
            mode,
            QueryFilter::default(),
            case_sensitivity,
            max_results,
            include_offline,
        )
    }

    pub fn search_with_filter(
        &self,
        primary: &IndexStore,
        query: &str,
        mode: SearchMode,
        filter: QueryFilter,
        case_sensitivity: CaseSensitivity,
        max_results: usize,
        include_offline: bool,
    ) -> (Vec<MultiSearchResult>, bool, Option<String>, SearchMode) {
        // Snapshot slot stores under a short read lock, then release coordinator lock before reading
        // indexes or invoking search::dispatch_multi.
        let snapshots: Vec<ShardSnapshot> = {
            let inner = self.inner.read();
            inner
                .slots
                .iter()
                .map(|slot| ShardSnapshot {
                    store: slot.store.clone(),
                    configured_path: slot.configured_path.clone(),
                    active_configured_path: slot.active_configured_path.clone(),
                    display_name: slot.display_name.clone(),
                    // NeedsConfirmation 保持旧缓存在线可搜（is_offline = false）
                    is_offline: matches!(slot.state, VolumeState::Offline | VolumeState::Missing),
                })
                .collect()
        };

        let primary_guard = primary.read();
        let slot_guards: Vec<_> = snapshots.iter().map(|s| s.store.read()).collect();
        let mut shards: Vec<SearchShard<'_>> = Vec::with_capacity(slot_guards.len() + 1);

        shards.push(SearchShard {
            index: &primary_guard,
            configured_path: None,
            active_path: None,
            volume_name: None,
            is_offline: false,
        });

        for (snap, guard) in snapshots.iter().zip(slot_guards.iter()) {
            shards.push(SearchShard {
                index: guard,
                configured_path: Some(snap.configured_path.as_str()),
                active_path: Some(snap.active_configured_path.as_str()),
                volume_name: Some(snap.display_name.as_str()),
                is_offline: snap.is_offline,
            });
        }

        search::dispatch_multi(
            &shards,
            query,
            mode,
            filter,
            case_sensitivity,
            max_results,
            include_offline,
        )
    }

    pub fn notify_volume_mounted(&self, mount_path: &Path) {
        let volumes_dir = self.volumes_dir();
        let mounts = (self.mount_lister)();
        {
            let mut inner = self.inner.write();
            let mounted_fs = mounts
                .iter()
                .find(|m| paths_equal_ci(Path::new(&m.mount_point), mount_path));
            for slot in &mut inner.slots {
                let mount_matches_identity = mounted_fs.is_some_and(|m| {
                    match_mount_for_identity(&slot.identity, std::slice::from_ref(m)).is_some()
                });
                if slot.matches_mount_path(mount_path)
                    || path_starts_with_ci(Path::new(&slot.configured_path), mount_path)
                    || mount_matches_identity
                {
                    slot.unmounting_until = None;
                    slot.next_reconnect_retry = None;
                    slot.skip_missing_rescan = false;
                }
            }
        };
        let effects = refresh_mounts_locked(&self.inner, &mounts, &volumes_dir, true);
        self.execute_effects(effects);
    }

    pub fn notify_volume_will_unmount(&self, mount_path: &Path) {
        let volumes_dir = self.volumes_dir();
        let unmount_deadline = Instant::now() + UNMOUNT_GRACE_PERIOD;
        let effects = {
            let mut inner = self.inner.write();
            let cfg_snap = inner.config.clone();
            let mut effects = TransitionEffects::default();
            let mut changed_records = Vec::new();
            let mut any_matched = false;
            for slot in &mut inner.slots {
                if slot.matches_mount_path(mount_path) {
                    cancel_slot_work(slot, &mut effects);
                    slot.state = VolumeState::Offline;
                    slot.active_mount_root = None;
                    slot.is_remounted = false;
                    slot.unmounting_until = Some(unmount_deadline);
                    slot.skip_missing_rescan = false;
                    reconcile_offline_slot_scope(slot, &cfg_snap, &mut effects);
                    slot.entry_count = slot.store.len() as u64;
                    if !matches!(slot.identity, VolumeIdentity::PendingPath { .. }) {
                        if slot.dirty_cache {
                            effects.cache_saves.push(slot.cache_save(&volumes_dir));
                        }
                        changed_records.push(slot.to_record());
                    }
                    any_matched = true;
                }
            }
            if any_matched {
                effects.notify = inner.on_status_change.clone();
            }
            if !changed_records.is_empty() {
                for rec in changed_records {
                    inner.manifest.upsert(rec);
                }
                effects.save_manifest = true;
            }
            effects
        };
        self.execute_effects(effects);
    }

    pub fn notify_volume_did_unmount(&self, mount_path: &Path) {
        let volumes_dir = self.volumes_dir();
        let mounts = (self.mount_lister)();
        {
            let mut inner = self.inner.write();
            for slot in &mut inner.slots {
                if slot.matches_mount_path(mount_path) {
                    slot.unmounting_until = None;
                }
            }
        }
        let effects = refresh_mounts_locked(&self.inner, &mounts, &volumes_dir, false);
        self.execute_effects(effects);
    }

    pub fn on_config_updated(&self, new_config: &AppConfig) {
        // 外置卷完全不受本地 FDA 权限影响，仅按 IndexConfig 变更对账；
        // 若卷协调器内部已与目标索引配置一致，跳过重复挂载表探测与对账。
        {
            let inner = self.inner.read();
            if inner.config.index == new_config.index {
                return;
            }
        }
        let volumes_dir = self.volumes_dir();
        let mounts = (self.mount_lister)();
        let effects = apply_config_update(&self.inner, &mounts, &volumes_dir, new_config);
        self.execute_effects(effects);
    }

    pub fn clear_volume_cache(&self, path_or_id: &str) -> Result<bool, String> {
        let volumes_dir = self.volumes_dir();
        let (removed, effects) = {
            let mut inner = self.inner.write();
            let mut effects = TransitionEffects::default();
            let mut matched_ids = Vec::new();
            let mut kept = Vec::with_capacity(inner.slots.len());
            for mut slot in inner.slots.drain(..) {
                if slot.matches_path_or_id(path_or_id) {
                    cancel_slot_work(&mut slot, &mut effects);
                    matched_ids.push(slot.volume_id);
                } else {
                    kept.push(slot);
                }
            }
            inner.slots = kept;

            let mut removed_from_manifest = false;
            while let Some(removed_rec) = inner.manifest.remove_by_path_or_id(path_or_id) {
                removed_from_manifest = true;
                let cache_path = volumes_dir.join(format!("{}.cache", removed_rec.volume_id));
                effects.push_cache_delete(removed_rec.volume_id, cache_path);
            }
            for vid in &matched_ids {
                effects.push_cache_delete(vid.clone(), volumes_dir.join(format!("{vid}.cache")));
                if inner.manifest.remove_by_path_or_id(vid).is_some() {
                    removed_from_manifest = true;
                }
            }

            let any_removed = !matched_ids.is_empty() || removed_from_manifest;
            if any_removed {
                effects.save_manifest = true;
                effects.notify = inner.on_status_change.clone();
            }
            (any_removed, effects)
        };

        self.execute_effects(effects);
        Ok(removed)
    }

    pub fn rescan_volume(&self, path_or_id: &str) -> Result<bool, String> {
        self.start_scan_for_slot(path_or_id, false)
    }

    /// 确认并启动单个处于 `NeedsConfirmation` 状态的卷扫描。
    pub fn confirm_volume_scan(&self, path_or_id: &str) -> Result<bool, String> {
        self.start_scan_for_slot(path_or_id, true)
    }

    /// 一键确认所有当前在线且处于 `NeedsConfirmation` 状态的卷，返回实际启动的卷数量。
    pub fn confirm_all_pending_volume_scans(&self) -> u32 {
        let mounts = (self.mount_lister)();
        let (started, effects) = {
            let mut inner = self.inner.write();
            let cfg_snap = inner.config.clone();
            let manifest_snap = inner.manifest.clone();
            let mut effects = TransitionEffects::default();
            let mut started = 0u32;

            for slot in &mut inner.slots {
                if slot.state != VolumeState::NeedsConfirmation {
                    continue;
                }
                let Some(resolved) = resolve_slot_mount(slot, &mounts) else {
                    continue;
                };
                bind_resolved_mount_for_scan(slot, &resolved, &manifest_snap, &cfg_snap, &mut effects);
                let target_snapshot = VolumeScopeSnapshot::from_config(
                    &cfg_snap,
                    Path::new(&slot.configured_path),
                    Path::new(&slot.active_configured_path),
                );
                prepare_slot_for_scan(slot, &cfg_snap, target_snapshot, &mut effects);
                started += 1;
            }
            #[cfg(test)]
            {
                inner.scan_runs += started as usize;
            }
            if started > 0 {
                effects.notify = inner.on_status_change.clone();
            }
            (started, effects)
        };

        self.execute_effects(effects);
        started
    }

    /// 取消指定正在扫描的外置卷/网络卷，无损保留原有缓存与待确认原因。
    pub fn cancel_volume_scan(&self, path_or_id: &str) -> Result<bool, String> {
        let (cancelled, effects) = {
            let mut inner = self.inner.write();
            let cfg_snap = inner.config.clone();
            let mut effects = TransitionEffects::default();
            let Some(slot) = inner
                .slots
                .iter_mut()
                .find(|s| s.matches_path_or_id(path_or_id) && s.state == VolumeState::Syncing)
            else {
                return Ok(false);
            };

            cancel_slot_work(slot, &mut effects);
            let target_snapshot = VolumeScopeSnapshot::from_config(
                &cfg_snap,
                Path::new(&slot.configured_path),
                Path::new(&slot.active_configured_path),
            );

            if !slot.has_cache {
                slot.pending_confirm_reason = Some(
                    slot.pending_confirm_reason
                        .unwrap_or(VolumeConfirmReason::InitialScan),
                );
                slot.state = VolumeState::NeedsConfirmation;
            } else if let Some(reason) = slot.pending_confirm_reason {
                slot.state = VolumeState::NeedsConfirmation;
                if reason.allows_live_stream() && slot.kind == VolumeKind::LocalExternal {
                    enqueue_attach_stream_for_slot(slot, target_snapshot, true, &mut effects);
                }
            } else {
                slot.state = VolumeState::Online;
                if slot.kind == VolumeKind::LocalExternal {
                    enqueue_attach_stream_for_slot(slot, target_snapshot, true, &mut effects);
                }
            }

            let record = (!matches!(slot.identity, VolumeIdentity::PendingPath { .. }))
                .then(|| slot.to_record());
            if let Some(rec) = record {
                inner.manifest.upsert(rec);
                effects.save_manifest = true;
            }
            effects.notify = inner.on_status_change.clone();
            (true, effects)
        };

        self.execute_effects(effects);
        Ok(cancelled)
    }

    fn start_scan_for_slot(
        &self,
        path_or_id: &str,
        require_needs_confirmation: bool,
    ) -> Result<bool, String> {
        let mounts = (self.mount_lister)();
        let (started, effects) = {
            let mut inner = self.inner.write();
            let cfg_snap = inner.config.clone();
            let manifest_snap = inner.manifest.clone();
            let mut effects = TransitionEffects::default();
            let Some(slot) = inner
                .slots
                .iter_mut()
                .find(|s| s.matches_path_or_id(path_or_id))
            else {
                return Ok(false);
            };
            if require_needs_confirmation && slot.state != VolumeState::NeedsConfirmation {
                return Ok(false);
            }

            let Some(resolved) = resolve_slot_mount(slot, &mounts) else {
                return Ok(false);
            };

            bind_resolved_mount_for_scan(slot, &resolved, &manifest_snap, &cfg_snap, &mut effects);
            let target_snapshot = VolumeScopeSnapshot::from_config(
                &cfg_snap,
                Path::new(&slot.configured_path),
                Path::new(&slot.active_configured_path),
            );
            prepare_slot_for_scan(slot, &cfg_snap, target_snapshot, &mut effects);
            #[cfg(test)]
            {
                inner.scan_runs += 1;
            }
            effects.notify = inner.on_status_change.clone();
            (true, effects)
        };

        self.execute_effects(effects);
        Ok(started)
    }

    pub fn statuses(&self) -> Vec<VolumeStatusInfo> {
        self.inner
            .read()
            .slots
            .iter()
            .map(VolumeSlot::status_info)
            .collect()
    }

    pub(crate) fn manifest_snapshot(&self) -> VolumeManifest {
        self.inner.read().manifest.clone()
    }

    pub fn total_entry_count(&self, primary: &IndexStore) -> u64 {
        let vol_sum: u64 = self
            .inner
            .read()
            .slots
            .iter()
            .map(|s| s.store.len() as u64)
            .sum();
        primary.len() as u64 + vol_sum
    }

    /// 导出当前已加载缓存的非空卷分片存储句柄快照，供后台预热线程在无锁状态下独立预热，
    /// 不持有 `Arc<VolumeCoordinator>` 强引用以免干扰协调器析构生命周期。
    pub(crate) fn loaded_stores(&self) -> Vec<IndexStore> {
        self.inner
            .read()
            .slots
            .iter()
            .filter(|slot| slot.has_cache && !slot.store.is_empty())
            .map(|slot| slot.store.clone())
            .collect()
    }

    /// 为当前已加载缓存的所有卷分片预热查询侧索引（CSR/签名/排序表），消除外置与离线卷首次搜索冷启动毛刺。
    pub fn warm_query_indexes(&self) {
        for store in self.loaded_stores() {
            store.warm_query_index();
        }
    }

    pub fn persist_all(&self) {
        let volumes_dir = self.volumes_dir();
        let (to_save, clean_records, dirty_records) = {
            let mut inner = self.inner.write();
            let mut clean_records = Vec::new();
            let mut dirty_records = Vec::new();
            let mut saves = Vec::new();
            for slot in &mut inner.slots {
                if matches!(slot.identity, VolumeIdentity::PendingPath { .. }) {
                    continue;
                }
                slot.entry_count = slot.store.len() as u64;
                if slot.dirty_cache {
                    saves.push(slot.cache_save(&volumes_dir));
                    dirty_records.push((slot.generation, slot.to_record()));
                } else {
                    clean_records.push((slot.generation, slot.to_record()));
                }
            }
            (saves, clean_records, dirty_records)
        };

        // 先落盘 .cache，仅将未脏或 .cache 确实落盘成功的同代次槽位记录写入 manifest，
        // 避免 manifest.last_event_id 超前于磁盘 .cache 快照或旧代次覆盖新代次记录。
        let saved_ids = save_dirty_caches_and_mark(&self.inner, to_save);
        {
            let mut inner = self.inner.write();
            for (expected_gen, rec) in clean_records {
                let same_gen = inner
                    .slots
                    .iter()
                    .any(|s| s.volume_id == rec.volume_id && s.generation == expected_gen);
                if same_gen {
                    inner.manifest.upsert(rec);
                }
            }
            for (expected_gen, rec) in dirty_records {
                if !saved_ids.contains(&rec.volume_id) {
                    continue;
                }
                let latest_clean = inner
                    .slots
                    .iter()
                    .find(|s| s.volume_id == rec.volume_id && s.generation == expected_gen)
                    .map(|s| if s.dirty_cache { rec.clone() } else { s.to_record() });
                if let Some(record_to_upsert) = latest_clean {
                    inner.manifest.upsert(record_to_upsert);
                }
            }
        }
        save_latest_manifest_locked(&self.inner, &self.manifest_save_lock, &volumes_dir);
    }

    #[cfg(test)]
    pub(crate) fn prune_run_count(&self) -> usize {
        self.inner.read().prune_runs
    }

    #[cfg(test)]
    pub(crate) fn scan_run_count(&self) -> usize {
        self.inner.read().scan_runs
    }

    #[cfg(test)]
    pub(crate) fn wait_for_workers_idle(&self, timeout: Duration) -> bool {
        self.worker_queue.wait_for_idle(timeout)
    }

    #[cfg(test)]
    pub(crate) fn slot_cancel_token(&self, volume_id: &str) -> Option<Arc<AtomicBool>> {
        self.inner
            .read()
            .slots
            .iter()
            .find(|s| s.volume_id == volume_id)
            .map(|s| Arc::clone(&s.cancel_token))
    }

    #[cfg(test)]
    pub(crate) fn slot_generation(&self, configured_path: &str) -> Option<u64> {
        self.inner
            .read()
            .slots
            .iter()
            .find(|s| str_paths_equal_ci(&s.configured_path, configured_path))
            .map(|s| s.generation)
    }
}

impl Drop for VolumeCoordinator {
    fn drop(&mut self) {
        {
            let (lock, cvar) = &*self.monitor_stop;
            *lock.lock() = true;
            cvar.notify_all();
        }
        self.worker_queue.shutdown();

        // Cancel in-flight scans and extract FSEventStream handles under the write lock,
        // then drop handles and join background threads outside any lock.
        let handles_to_drop: Vec<VolumeStreamHandle> = {
            let mut inner = self.inner.write();
            let mut handles = Vec::new();
            for slot in &mut inner.slots {
                slot.cancel_token.store(true, Ordering::SeqCst);
                slot.generation = slot.generation.wrapping_add(1);
                if let Some(h) = slot.stream_handle.take() {
                    handles.push(h);
                }
            }
            handles
        };
        drop(handles_to_drop);

        let current_tid = std::thread::current().id();
        if let Some(handle) = self.monitor_thread.lock().take() {
            if handle.thread().id() != current_tid {
                let _ = handle.join();
            }
        }
        for handle in self.worker_threads.lock().drain(..) {
            if handle.thread().id() != current_tid {
                let _ = handle.join();
            }
        }
    }
}

struct PreloadedCache {
    compact: CompactIndex,
    checkpoint: Option<cache::Checkpoint>,
}

fn preload_missing_target_caches(
    inner: &Arc<RwLock<CoordinatorInner>>,
    manifest: &VolumeManifest,
    targets: &[MountedPathTarget],
    mounts: &[MountedFsInfo],
    volumes_dir: &Path,
) -> HashMap<String, PreloadedCache> {
    let loaded_ids: Vec<String> = {
        let guard = inner.read();
        guard
            .slots
            .iter()
            .filter(|s| s.has_cache)
            .map(|s| s.volume_id.clone())
            .collect()
    };

    let mut out = HashMap::new();
    let mut candidate_ids = Vec::new();
    for target in targets {
        let configured_str = target.configured_path.to_string_lossy();
        if let Some(ref resolved) = target.mounted {
            candidate_ids.push(resolved.identity.volume_id());
        }
        if let Some(rec) = manifest.find_by_path(&configured_str) {
            candidate_ids.push(rec.volume_id.clone());
            if let Some(m) = match_mount_for_identity(&rec.identity, mounts) {
                let (_, id) = build_identity_for_mount(m, &rec.subpath);
                candidate_ids.push(id.volume_id());
            }
        }
    }
    candidate_ids.sort();
    candidate_ids.dedup();
    for vid in candidate_ids {
        if loaded_ids.contains(&vid) {
            continue;
        }
        let path = volumes_dir.join(format!("{vid}.cache"));
        if path.exists() {
            if let Ok((compact, checkpoint)) = cache::load_compact(&path) {
                out.insert(vid, PreloadedCache { compact, checkpoint });
            }
        }
    }
    out
}

fn join_mount_subpath(mount_root: &Path, subpath: &str) -> String {
    let trimmed_root = mount_root.to_string_lossy().trim_end_matches('/').to_string();
    let base = if trimmed_root.is_empty() {
        "/".to_string()
    } else {
        trimmed_root
    };
    let sub = subpath.trim_matches('/');
    if sub.is_empty() {
        base
    } else if base == "/" {
        format!("/{sub}")
    } else {
        format!("{base}/{sub}")
    }
}

fn build_identity_for_mount(m: &MountedFsInfo, subpath: &str) -> (VolumeKind, VolumeIdentity) {
    let kind = if m.is_local {
        VolumeKind::LocalExternal
    } else {
        VolumeKind::Network
    };
    let identity = match kind {
        VolumeKind::Network => VolumeIdentity::Network {
            fstype: m.fstype.clone(),
            normalized_source: normalize_network_source(&m.fstype, &m.from_name),
            subpath: subpath.to_string(),
        },
        VolumeKind::LocalExternal => match &m.uuid {
            Some(uuid) => VolumeIdentity::LocalUuid {
                uuid: uuid.clone(),
                subpath: subpath.to_string(),
            },
            None => VolumeIdentity::LocalFallback {
                fstype: m.fstype.clone(),
                source: m.from_name.clone(),
                total_bytes: m.total_bytes,
                volume_name: m.volume_name.clone(),
                subpath: subpath.to_string(),
                exact_name_key: false,
            },
        },
    };
    (kind, identity)
}

fn match_mount_for_identity<'a>(
    identity: &VolumeIdentity,
    mounts: &'a [MountedFsInfo],
) -> Option<&'a MountedFsInfo> {
    if let VolumeIdentity::LocalFallback {
        fstype,
        source,
        total_bytes,
        volume_name,
        exact_name_key,
        ..
    } = identity
    {
        // 两级匹配：仅匹配同样无有效 UUID 的本地外置盘（防止劫持带 UUID 的同规格盘）；
        // 先按精确卷名匹配（同规格双盘优先原设备节点），单盘模式（exact_name_key == false）未命中时再回退剥离 -1/-2 冲突后缀匹配。
        let candidates = mounts.iter().filter(|m| {
            !crate::volumes::is_system_mount_point(&m.mount_point)
                && m.is_local
                && m.uuid.is_none()
                && m.fstype.eq_ignore_ascii_case(fstype)
                && m.total_bytes == *total_bytes
        });

        let mut first_exact: Option<&MountedFsInfo> = None;
        let mut first_drifted: Option<&MountedFsInfo> = None;
        let target_base = strip_mount_drift_suffix(volume_name);

        for m in candidates {
            if m.volume_name.eq_ignore_ascii_case(volume_name) {
                if m.from_name == *source {
                    return Some(m);
                }
                if first_exact.is_none() {
                    first_exact = Some(m);
                }
            } else if !exact_name_key
                && first_drifted.is_none()
                && strip_mount_drift_suffix(&m.volume_name).eq_ignore_ascii_case(target_base)
            {
                first_drifted = Some(m);
            }
        }
        return first_exact.or(first_drifted);
    }

    mounts.iter().find(|m| {
        if crate::volumes::is_system_mount_point(&m.mount_point) {
            return false;
        }
        match identity {
            VolumeIdentity::LocalUuid { uuid, .. } => {
                m.is_local && m.uuid.as_deref().is_some_and(|u| u.eq_ignore_ascii_case(uuid))
            }
            VolumeIdentity::LocalFallback { .. } => unreachable!(),
            VolumeIdentity::Network {
                fstype,
                normalized_source,
                ..
            } => {
                !m.is_local
                    && m.fstype.eq_ignore_ascii_case(fstype)
                    && normalize_network_source(&m.fstype, &m.from_name) == *normalized_source
            }
            VolumeIdentity::PendingPath { .. } => false,
        }
    })
}

fn resolve_mounted_for_target(
    target: &MountedPathTarget,
    existing_slot: Option<&VolumeSlot>,
    existing_record: Option<&VolumeRecord>,
    mounts: &[MountedFsInfo],
) -> Option<ResolvedMountMatch> {
    if let Some(ref resolved) = target.mounted {
        return Some(resolved.clone());
    }
    let (identity, subpath, stored_display_name) = if let Some(slot) = existing_slot {
        (&slot.identity, &slot.subpath, &slot.display_name)
    } else if let Some(rec) = existing_record {
        (&rec.identity, &rec.subpath, &rec.display_name)
    } else {
        return None;
    };

    let m = match_mount_for_identity(identity, mounts)?;
    let (kind, mut matched_identity) = build_identity_for_mount(m, subpath);
    if let (
        VolumeIdentity::LocalFallback {
            exact_name_key: orig_exact,
            ..
        },
        VolumeIdentity::LocalFallback {
            ref mut exact_name_key,
            ..
        },
    ) = (identity, &mut matched_identity)
    {
        *exact_name_key = *orig_exact;
    }
    let display_name = if subpath.is_empty() {
        if m.volume_name.is_empty() {
            stored_display_name.clone()
        } else {
            m.volume_name.clone()
        }
    } else if m.volume_name.is_empty() {
        subpath.clone()
    } else {
        format!("{}/{}", m.volume_name, subpath)
    };
    Some(ResolvedMountMatch {
        mount_root: PathBuf::from(&m.mount_point),
        subpath: subpath.clone(),
        display_name,
        kind,
        fstype: m.fstype.clone(),
        from_name: m.from_name.clone(),
        identity: matched_identity,
    })
}

fn cancel_slot_work(slot: &mut VolumeSlot, effects: &mut TransitionEffects) {
    slot.cancel_token.store(true, Ordering::SeqCst);
    slot.generation = slot.generation.wrapping_add(1);
    slot.cancel_token = Arc::new(AtomicBool::new(false));
    slot.in_flight_scan_snapshot = None;
    slot.in_flight_prune_snapshot = None;
    slot.stream_snapshot = None;
    slot.scanned_count.store(0, Ordering::Relaxed);
    effects.cancelled_volume_ids.push(slot.volume_id.clone());
    if let Some(handle) = slot.stream_handle.take() {
        effects.handles_to_drop.push(handle);
    }
}

fn config_matches_serial_scan_paths(
    config: &AppConfig,
    configured_path: &str,
    active_configured_path: &str,
) -> bool {
    let serial_paths = config.resolve_serial_scan_paths();
    if serial_paths.is_empty() {
        return false;
    }
    let configured = Path::new(configured_path);
    let active = Path::new(active_configured_path);
    serial_paths.iter().any(|sp| {
        paths_equal_ci(configured, sp)
            || path_starts_with_ci(configured, sp)
            || paths_equal_ci(active, sp)
            || path_starts_with_ci(active, sp)
    })
}

fn prepare_slot_for_scan(
    slot: &mut VolumeSlot,
    config: &AppConfig,
    target_snapshot: VolumeScopeSnapshot,
    effects: &mut TransitionEffects,
) {
    cancel_slot_work(slot, effects);
    slot.state = VolumeState::Syncing;
    slot.skip_missing_rescan = false;
    slot.last_rescan_triggered = Some(Instant::now());
    slot.scanned_count.store(0, Ordering::Relaxed);
    slot.in_flight_scan_snapshot = Some(target_snapshot.clone());

    let mount_root = slot
        .active_mount_root
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&slot.active_configured_path));
    let use_serial_scan = slot.kind == VolumeKind::Network
        || is_rotational_device(&slot.from_name)
        || config_matches_serial_scan_paths(
            config,
            &slot.configured_path,
            &slot.active_configured_path,
        );

    effects.tasks.push(WorkerTask::Scan(ScanJob {
        volume_id: slot.volume_id.clone(),
        generation: slot.generation,
        kind: slot.kind,
        fstype: slot.fstype.clone(),
        mount_root,
        subpath: slot.subpath.clone(),
        root: PathBuf::from(&slot.active_configured_path),
        target_snapshot,
        use_serial_scan,
        scanned_count: Arc::clone(&slot.scanned_count),
        cancel: Arc::clone(&slot.cancel_token),
    }));
}

fn enqueue_attach_stream_for_slot(
    slot: &mut VolumeSlot,
    target_snapshot: VolumeScopeSnapshot,
    require_journal_verify: bool,
    effects: &mut TransitionEffects,
) {
    if slot.kind != VolumeKind::LocalExternal {
        return;
    }
    let mount_root = slot
        .active_mount_root
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&slot.active_configured_path));
    if let Some(old) = slot.stream_handle.take() {
        effects.handles_to_drop.push(old);
    }
    slot.stream_snapshot = Some(target_snapshot.clone());
    effects.tasks.push(WorkerTask::AttachStream(AttachStreamJob {
        volume_id: slot.volume_id.clone(),
        generation: slot.generation,
        mount_root,
        subpath: slot.subpath.clone(),
        fstype: slot.fstype.clone(),
        active_path: PathBuf::from(&slot.active_configured_path),
        since_event_id: slot.last_event_id,
        stored_journal_uuid: slot.journal_uuid.clone(),
        require_journal_verify,
        target_snapshot,
        cancel: Arc::clone(&slot.cancel_token),
        store: slot.store.clone(),
    }));
}

fn enqueue_prune_for_slot(
    slot: &mut VolumeSlot,
    target_snapshot: VolumeScopeSnapshot,
    clear_filter_widened: bool,
    reattach_stream_after: bool,
    effects: &mut TransitionEffects,
) {
    if !slot.has_cache || slot.store.is_empty() {
        return;
    }
    if slot
        .in_flight_prune_snapshot
        .as_ref()
        .is_some_and(|s| s.semantically_eq(&target_snapshot))
    {
        return;
    }
    if let Some(old) = slot.stream_handle.take() {
        effects.handles_to_drop.push(old);
    }
    slot.stream_snapshot = None;
    slot.in_flight_prune_snapshot = Some(target_snapshot.clone());
    effects.tasks.push(WorkerTask::Prune(PruneJob {
        volume_id: slot.volume_id.clone(),
        generation: slot.generation,
        configured_path: slot.configured_path.clone(),
        active_configured_path: slot.active_configured_path.clone(),
        target_snapshot,
        clear_filter_widened,
        reattach_stream_after,
        cancel: Arc::clone(&slot.cancel_token),
        store: slot.store.clone(),
    }));
}

fn bind_resolved_mount_for_scan(
    slot: &mut VolumeSlot,
    resolved: &ResolvedMountMatch,
    manifest: &VolumeManifest,
    config: &AppConfig,
    effects: &mut TransitionEffects,
) {
    let new_vid = resolved.identity.volume_id();
    if slot.volume_id != new_vid {
        reset_slot_for_volume_swap(slot, resolved, manifest, None, config, effects);
    } else {
        slot.identity = resolved.identity.clone();
        slot.kind = resolved.kind;
        slot.fstype = resolved.fstype.clone();
        slot.from_name = resolved.from_name.clone();
        slot.subpath = resolved.subpath.clone();
        slot.display_name = resolved.display_name.clone();
    }

    let mount_root_str = resolved.mount_root.to_string_lossy().to_string();
    let active_path_str = join_mount_subpath(&resolved.mount_root, &slot.subpath);
    let is_remounted = !str_paths_equal_ci(&active_path_str, &slot.configured_path);

    slot.unmounting_until = None;
    slot.next_reconnect_retry = None;
    slot.active_mount_root = Some(mount_root_str);
    slot.active_configured_path = active_path_str;
    slot.is_remounted = is_remounted;
}

/// 兼容 `v0.2.0` / `main` 旧版 `manifest.json` 升级认领与新版快照恢复。
/// 返回 `(scope_snapshot, dirty_upper_bound, pending_confirm_reason, needs_manifest_upgrade)`。
fn resolve_record_scope_on_load(
    rec: &VolumeRecord,
    config: &AppConfig,
    active_configured_path: &str,
    has_cache: bool,
    loaded_cp_fp: Option<u64>,
) -> (
    Option<VolumeScopeSnapshot>,
    bool,
    Option<VolumeConfirmReason>,
    bool,
) {
    if rec.scope_snapshot.is_some() {
        return (
            rec.scope_snapshot.clone(),
            rec.dirty_upper_bound,
            rec.pending_confirm_reason,
            false,
        );
    }
    if !has_cache {
        return (None, false, rec.pending_confirm_reason, false);
    }
    if rec.pending_confirm_reason.is_some() {
        // 新版 manifest 显式落盘的 scope_snapshot=None 待确认记录（例如旧版未认领或损坏恢复），
        // 绝不按旧版无指纹记录认领为当前配置。
        return (None, rec.dirty_upper_bound, rec.pending_confirm_reason, false);
    }

    // 旧版 manifest 升级认领：同时匹配 has_fda=false/true 两个旧指纹，彻底免疫升级前后 FDA 变化误伤外置盘
    let legacy_fps = [
        legacy_volume_filter_fingerprint(config, false),
        legacy_volume_filter_fingerprint(config, true),
    ];
    let saved_legacy_fp = rec.filter_fingerprint.or(loaded_cp_fp);
    let legacy_claimable = !rec.dirty_filter
        && (saved_legacy_fp.is_none()
            || saved_legacy_fp.is_some_and(|fp| legacy_fps.contains(&fp)));

    if legacy_claimable {
        let claimed = VolumeScopeSnapshot::from_config(
            config,
            Path::new(&rec.configured_path),
            Path::new(active_configured_path),
        );
        (Some(claimed), false, rec.pending_confirm_reason, true)
    } else {
        // 旧 dirty_filter=true 或离线改配置导致旧指纹不匹配：保持 scope_snapshot=None，
        // 转入 NeedsConfirmation(FilterWidened)，不做 ABA 自动回退。
        let merged = VolumeConfirmReason::merge(
            rec.pending_confirm_reason,
            VolumeConfirmReason::FilterWidened,
        );
        (None, false, Some(merged), true)
    }
}

fn reset_slot_for_volume_swap(
    slot: &mut VolumeSlot,
    res: &ResolvedMountMatch,
    manifest: &VolumeManifest,
    preloaded: Option<PreloadedCache>,
    config: &AppConfig,
    effects: &mut TransitionEffects,
) -> u64 {
    cancel_slot_work(slot, effects);

    let new_vid = res.identity.volume_id();
    let active_str = join_mount_subpath(&res.mount_root, &res.subpath);
    slot.volume_id = new_vid.clone();
    slot.identity = res.identity.clone();
    slot.kind = res.kind;
    slot.fstype = res.fstype.clone();
    slot.from_name = res.from_name.clone();
    slot.subpath = res.subpath.clone();
    slot.display_name = res.display_name.clone();
    slot.active_configured_path = active_str.clone();
    slot.active_mount_root = None;
    slot.store = IndexStore::new();
    slot.entry_count = 0;
    slot.has_cache = false;
    slot.dirty_cache = false;
    slot.cache_seq = slot.cache_seq.wrapping_add(1);
    slot.saved_cache_seq = slot.cache_seq;
    slot.cache_persistence = CachePersistence::default();
    slot.scope_snapshot = None;
    slot.dirty_upper_bound = false;
    slot.pending_confirm_reason = None;
    slot.in_flight_scan_snapshot = None;
    slot.in_flight_prune_snapshot = None;
    slot.stream_snapshot = None;
    slot.scanned_count.store(0, Ordering::Relaxed);
    slot.is_remounted = false;
    slot.last_sync_at = None;
    slot.last_event_id = None;
    slot.journal_uuid = None;
    slot.last_rescan_triggered = None;
    slot.last_scan_duration = None;
    slot.next_reconnect_retry = None;
    slot.unmounting_until = None;
    slot.skip_missing_rescan = false;

    if let Some(loaded) = preloaded {
        let count = loaded.compact.len() as u64;
        slot.store.replace_compact(loaded.compact);
        slot.has_cache = true;
        slot.entry_count = count;
        let cp_fp = loaded
            .checkpoint
            .and_then(|cp| (cp.fingerprint != 0).then_some(cp.fingerprint));
        slot.last_event_id = loaded.checkpoint.map(|cp| cp.event_id);
        let rec_opt = manifest.records.iter().find(|r| r.volume_id == new_vid);
        if let Some(rec) = rec_opt {
            slot.active_configured_path = rec.active_configured_path.clone();
            slot.last_sync_at = rec.last_sync_at;
            slot.journal_uuid = rec.journal_uuid.clone();
            let (snap, dirty_ub, reason, _) =
                resolve_record_scope_on_load(rec, config, &active_str, true, cp_fp);
            slot.scope_snapshot = snap;
            slot.dirty_upper_bound = dirty_ub;
            slot.pending_confirm_reason = reason;
        } else {
            // manifest 缺失/损坏但分片 .cache 存在：无快照且无日志证明 -> JournalInvalidated
            slot.scope_snapshot = None;
            slot.dirty_upper_bound = false;
            slot.pending_confirm_reason = Some(VolumeConfirmReason::JournalInvalidated);
        }
        return count;
    }
    0
}

fn resolve_slot_mount(slot: &VolumeSlot, mounts: &[MountedFsInfo]) -> Option<ResolvedMountMatch> {
    if let Some(m) = match_mount_for_identity(&slot.identity, mounts) {
        let (kind, mut identity) = build_identity_for_mount(m, &slot.subpath);
        if let (
            VolumeIdentity::LocalFallback {
                exact_name_key: orig_exact,
                ..
            },
            VolumeIdentity::LocalFallback {
                ref mut exact_name_key,
                ..
            },
        ) = (&slot.identity, &mut identity)
        {
            *exact_name_key = *orig_exact;
        }
        return Some(ResolvedMountMatch {
            mount_root: PathBuf::from(&m.mount_point),
            subpath: slot.subpath.clone(),
            display_name: slot.display_name.clone(),
            kind,
            fstype: m.fstype.clone(),
            from_name: m.from_name.clone(),
            identity,
        });
    }
    // Only unmounted PendingPath slots may fall back to matching a live mount prefix;
    // identified slots must never bind to a different disk sharing the mount point.
    if !matches!(slot.identity, VolumeIdentity::PendingPath { .. }) {
        return None;
    }
    let configured = PathBuf::from(&slot.configured_path);
    let best = mounts
        .iter()
        .filter(|m| {
            !crate::volumes::is_system_mount_point(&m.mount_point)
                && path_starts_with_ci(&configured, Path::new(&m.mount_point))
        })
        .max_by_key(|m| m.mount_point.len())?;
    let mount_root = PathBuf::from(&best.mount_point);
    let subpath = configured
        .strip_prefix(&mount_root)
        .map(|rel| rel.to_string_lossy().trim_matches('/').to_string())
        .unwrap_or_default();
    let (kind, identity) = build_identity_for_mount(best, &subpath);
    Some(ResolvedMountMatch {
        mount_root,
        subpath,
        display_name: slot.display_name.clone(),
        kind,
        fstype: best.fstype.clone(),
        from_name: best.from_name.clone(),
        identity,
    })
}

/// 对处于 `Offline` 状态的已缓存槽位按新配置执行三分法对账，返回是否需要更新 manifest。
fn reconcile_offline_slot_scope(
    slot: &mut VolumeSlot,
    config: &AppConfig,
    effects: &mut TransitionEffects,
) -> bool {
    if !slot.has_cache {
        return false;
    }
    let Some(old_snap) = slot.scope_snapshot.clone() else {
        // scope_snapshot == None（旧版未认领或损坏 manifest）不做 ABA 自动回退
        return false;
    };
    let target_snapshot = VolumeScopeSnapshot::from_config(
        config,
        Path::new(&slot.configured_path),
        Path::new(&slot.active_configured_path),
    );
    match diff_volume_scope(&old_snap, &target_snapshot, slot.dirty_upper_bound) {
        VolumeScopeAction::FullRebuild { reason } => {
            slot.in_flight_prune_snapshot = None;
            let merged = VolumeConfirmReason::merge(slot.pending_confirm_reason, reason);
            if slot.pending_confirm_reason != Some(merged) {
                slot.pending_confirm_reason = Some(merged);
                true
            } else {
                false
            }
        }
        VolumeScopeAction::PruneOnly => {
            let clear_widened = slot.pending_confirm_reason.is_none()
                || slot.pending_confirm_reason == Some(VolumeConfirmReason::FilterWidened);
            if slot.store.is_empty() {
                slot.scope_snapshot = Some(target_snapshot);
                slot.dirty_upper_bound = false;
                slot.in_flight_prune_snapshot = None;
                if clear_widened && slot.pending_confirm_reason == Some(VolumeConfirmReason::FilterWidened) {
                    slot.pending_confirm_reason = None;
                }
                true
            } else {
                enqueue_prune_for_slot(slot, target_snapshot, clear_widened, false, effects);
                false
            }
        }
        VolumeScopeAction::NoOp => {
            slot.in_flight_prune_snapshot = None;
            if slot.pending_confirm_reason == Some(VolumeConfirmReason::FilterWidened) {
                slot.pending_confirm_reason = None;
                true
            } else {
                false
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn reconcile_partition_locked(
    inner: &mut CoordinatorInner,
    targets: &[MountedPathTarget],
    mounts: &[MountedFsInfo],
    volumes_dir: &Path,
    preloaded: &mut HashMap<String, PreloadedCache>,
    force_retry_missing: bool,
    newly_added_paths: &HashSet<String>,
    effects: &mut TransitionEffects,
) -> u64 {
    let mut loaded_total = 0u64;
    let mut manifest_changed = false;
    let mut status_changed = false;
    // 将本轮对账产出的槽位先暂存至独立容器，循环结束后统一写回 inner.slots，
    // 防止同批次不同配置路径在循环中途互相抢占刚完成对账的槽位。
    let mut reconciled_slots = Vec::with_capacity(targets.len());
    let cfg_snap = inner.config.clone();

    for target in targets {
        let configured_str = target.configured_path.to_string_lossy().to_string();
        let manifest_record = inner.manifest.find_by_path(&configured_str).cloned();
        let mut slot_manifest_dirty = false;

        let existing_idx = inner.slots.iter().position(|s| {
            str_paths_equal_ci(&s.configured_path, &configured_str)
                || target
                    .mounted
                    .as_ref()
                    .is_some_and(|m| m.identity.volume_id() == s.volume_id)
                || manifest_record
                    .as_ref()
                    .is_some_and(|r| r.volume_id == s.volume_id)
        });

        let resolved = resolve_mounted_for_target(
            target,
            existing_idx.map(|i| &inner.slots[i]),
            manifest_record.as_ref(),
            mounts,
        );

        let mut slot = if let Some(idx) = existing_idx {
            inner.slots.remove(idx)
        } else if let Some(rec) = manifest_record.clone() {
            let store = IndexStore::new();
            let mut has_cache = false;
            let mut loaded_event_id = None;
            let mut loaded_cp_fp = None;
            // Skip loading the old manifest record's cache if a different disk already
            // occupies this configured path and will immediately swap the slot.
            let keep_manifest_identity = resolved
                .as_ref()
                .is_none_or(|res| res.identity.volume_id() == rec.volume_id);
            if keep_manifest_identity {
                if let Some(loaded) = preloaded.remove(&rec.volume_id) {
                    let count = loaded.compact.len() as u64;
                    store.replace_compact(loaded.compact);
                    loaded_total += count;
                    has_cache = true;
                    loaded_event_id = loaded.checkpoint.map(|cp| cp.event_id);
                    loaded_cp_fp = loaded
                        .checkpoint
                        .and_then(|cp| (cp.fingerprint != 0).then_some(cp.fingerprint));
                }
            }
            let active_for_claim = resolved
                .as_ref()
                .map(|r| join_mount_subpath(&r.mount_root, &r.subpath))
                .unwrap_or_else(|| rec.active_configured_path.clone());
            let (scope_snapshot, dirty_upper_bound, pending_confirm_reason, upgraded) =
                resolve_record_scope_on_load(
                    &rec,
                    &cfg_snap,
                    &active_for_claim,
                    has_cache,
                    loaded_cp_fp,
                );
            if upgraded {
                slot_manifest_dirty = true;
            }
            VolumeSlot {
                volume_id: rec.volume_id,
                configured_path: configured_str.clone(),
                active_configured_path: rec.active_configured_path,
                active_mount_root: rec.active_mount_root,
                subpath: rec.subpath,
                display_name: rec.display_name,
                kind: rec.kind,
                fstype: rec.fstype,
                from_name: resolved
                    .as_ref()
                    .map(|r| r.from_name.clone())
                    .unwrap_or_default(),
                identity: rec.identity,
                state: VolumeState::Offline,
                entry_count: store.len() as u64,
                last_sync_at: rec.last_sync_at,
                // 以 .cache 内嵌检查点为准，防止 manifest 超前导致漏重放 FSEvents 事件
                last_event_id: if has_cache {
                    loaded_event_id.or(rec.last_event_id)
                } else {
                    rec.last_event_id
                },
                journal_uuid: rec.journal_uuid,
                scope_snapshot,
                dirty_upper_bound,
                pending_confirm_reason,
                in_flight_scan_snapshot: None,
                in_flight_prune_snapshot: None,
                stream_snapshot: None,
                scanned_count: Arc::new(AtomicU64::new(0)),
                is_remounted: false,
                has_cache,
                dirty_cache: false,
                cache_seq: 0,
                saved_cache_seq: 0,
                cache_persistence: CachePersistence::default(),
                generation: 0,
                cancel_token: Arc::new(AtomicBool::new(false)),
                store,
                stream_handle: None,
                last_rescan_triggered: None,
                last_scan_duration: None,
                next_reconnect_retry: None,
                unmounting_until: None,
                skip_missing_rescan: false,
            }
        } else if let Some(ref res) = resolved {
            let vid = res.identity.volume_id();
            let active_str = join_mount_subpath(&res.mount_root, &res.subpath);
            let store = IndexStore::new();
            let mut has_cache = false;
            let mut last_event_id = None;
            let mut pending_confirm_reason = None;
            if let Some(loaded) = preloaded.remove(&vid) {
                let count = loaded.compact.len() as u64;
                store.replace_compact(loaded.compact);
                loaded_total += count;
                has_cache = true;
                last_event_id = loaded.checkpoint.map(|cp| cp.event_id);
                // manifest.json 损坏或丢失但分片 .cache 尚存：缺失 scope_snapshot 与 journal_uuid，
                // 无论 APFS 还是 ExFAT 均直接进入 NeedsConfirmation(JournalInvalidated) 且不挂流
                pending_confirm_reason = Some(VolumeConfirmReason::JournalInvalidated);
                slot_manifest_dirty = true;
            }
            VolumeSlot {
                volume_id: vid,
                configured_path: configured_str.clone(),
                active_configured_path: active_str,
                active_mount_root: Some(res.mount_root.to_string_lossy().to_string()),
                subpath: res.subpath.clone(),
                display_name: res.display_name.clone(),
                kind: res.kind,
                fstype: res.fstype.clone(),
                from_name: res.from_name.clone(),
                identity: res.identity.clone(),
                state: VolumeState::Offline,
                entry_count: store.len() as u64,
                last_sync_at: None,
                last_event_id,
                journal_uuid: None,
                scope_snapshot: None,
                dirty_upper_bound: false,
                pending_confirm_reason,
                in_flight_scan_snapshot: None,
                in_flight_prune_snapshot: None,
                stream_snapshot: None,
                scanned_count: Arc::new(AtomicU64::new(0)),
                is_remounted: false,
                has_cache,
                dirty_cache: false,
                cache_seq: 0,
                saved_cache_seq: 0,
                cache_persistence: CachePersistence::default(),
                generation: 0,
                cancel_token: Arc::new(AtomicBool::new(false)),
                store,
                stream_handle: None,
                last_rescan_triggered: None,
                last_scan_duration: None,
                next_reconnect_retry: None,
                unmounting_until: None,
                skip_missing_rescan: false,
            }
        } else {
            let pending_id = VolumeIdentity::PendingPath {
                normalized_path: configured_str.trim_end_matches('/').to_ascii_lowercase(),
            };
            let display_name = target
                .configured_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| configured_str.clone());
            VolumeSlot {
                volume_id: pending_id.volume_id(),
                configured_path: configured_str.clone(),
                active_configured_path: configured_str.clone(),
                active_mount_root: None,
                subpath: String::new(),
                display_name,
                kind: VolumeKind::Network,
                fstype: String::new(),
                from_name: String::new(),
                identity: pending_id,
                state: VolumeState::Offline,
                entry_count: 0,
                last_sync_at: None,
                last_event_id: None,
                journal_uuid: None,
                scope_snapshot: None,
                dirty_upper_bound: false,
                pending_confirm_reason: None,
                in_flight_scan_snapshot: None,
                in_flight_prune_snapshot: None,
                stream_snapshot: None,
                scanned_count: Arc::new(AtomicU64::new(0)),
                is_remounted: false,
                has_cache: false,
                dirty_cache: false,
                cache_seq: 0,
                saved_cache_seq: 0,
                cache_persistence: CachePersistence::default(),
                generation: 0,
                cancel_token: Arc::new(AtomicBool::new(false)),
                store: IndexStore::new(),
                stream_handle: None,
                last_rescan_triggered: None,
                last_scan_duration: None,
                next_reconnect_retry: None,
                unmounting_until: None,
                skip_missing_rescan: false,
            }
        };

        let prev_state = slot.state;
        let prev_active = slot.active_mount_root.clone();
        let prev_reason = slot.pending_confirm_reason;

        let suppressed_by_unmount = resolved.as_ref().is_some_and(|res| {
            slot.volume_id == res.identity.volume_id()
                && slot
                    .unmounting_until
                    .is_some_and(|until| Instant::now() < until)
        });
        let effective_resolved = if suppressed_by_unmount {
            None
        } else {
            resolved
        };

        if let Some(res) = effective_resolved {
            slot.unmounting_until = None;
            slot.next_reconnect_retry = None;
            let new_vid = res.identity.volume_id();
            let swapped = slot.volume_id != new_vid;
            let mut meta_changed = false;
            if swapped {
                loaded_total += reset_slot_for_volume_swap(
                    &mut slot,
                    &res,
                    &inner.manifest,
                    preloaded.remove(&new_vid),
                    &cfg_snap,
                    effects,
                );
                status_changed = true;
            } else {
                meta_changed = slot.display_name != res.display_name
                    || slot.fstype != res.fstype
                    || slot.subpath != res.subpath;
                slot.identity = res.identity;
                slot.kind = res.kind;
                slot.fstype = res.fstype;
                slot.from_name = res.from_name;
                slot.subpath = res.subpath;
                slot.display_name = res.display_name;
                if meta_changed {
                    status_changed = true;
                }
            }

            let mount_root_str = res.mount_root.to_string_lossy().to_string();
            let new_active_path = join_mount_subpath(&res.mount_root, &slot.subpath);
            let is_remounted = !str_paths_equal_ci(&new_active_path, &slot.configured_path);

            // Rebase in-memory paths when the OS shifted the mount root (for example -1 drift
            // or restoring the canonical mount point).
            let path_shifted = !str_paths_equal_ci(&slot.active_configured_path, &new_active_path);
            if path_shifted && slot.has_cache && !slot.store.is_empty() {
                if let Err(err) = slot
                    .store
                    .write()
                    .rebase_mount_root(&slot.active_configured_path, &new_active_path)
                {
                    eprintln!("[Lokii] failed to rebase volume {}: {err}", slot.display_name);
                    slot.has_cache = false;
                    slot.scope_snapshot = None;
                    slot.dirty_upper_bound = false;
                } else {
                    slot.active_configured_path = new_active_path.clone();
                    slot.is_remounted = is_remounted;
                    slot.dirty_cache = true;
                    slot.cache_seq = slot.cache_seq.wrapping_add(1);
                    effects.cache_saves.push(slot.cache_save(volumes_dir));
                    manifest_changed = true;
                }
            } else {
                slot.active_configured_path = new_active_path.clone();
                slot.is_remounted = is_remounted;
            }

            if force_retry_missing {
                slot.skip_missing_rescan = false;
            }
            let same_missing_mount = !swapped
                && prev_state == VolumeState::Missing
                && prev_active.as_deref() == Some(mount_root_str.as_str())
                && slot.skip_missing_rescan
                && !force_retry_missing;

            slot.active_mount_root = Some(mount_root_str.clone());
            slot.entry_count = slot.store.len() as u64;

            let target_snapshot = VolumeScopeSnapshot::from_config(
                &cfg_snap,
                Path::new(&slot.configured_path),
                Path::new(&slot.active_configured_path),
            );
            let is_newly_added =
                newly_added_paths.contains(&slot.configured_path.to_ascii_lowercase());

            // 若槽位正在同一挂载根上扫描：配置未影响本卷过滤快照时保持 Syncing 不中断；
            // 若本卷过滤规则实际发生变化则取消旧扫并按新快照重新决策。
            let keep_syncing = if !swapped
                && prev_state == VolumeState::Syncing
                && prev_active.as_deref() == Some(mount_root_str.as_str())
            {
                if slot
                    .in_flight_scan_snapshot
                    .as_ref()
                    .is_some_and(|s| s.semantically_eq(&target_snapshot))
                {
                    true
                } else {
                    cancel_slot_work(&mut slot, effects);
                    false
                }
            } else {
                false
            };

            if keep_syncing {
                slot.state = VolumeState::Syncing;
            } else if !slot.has_cache {
                if same_missing_mount {
                    slot.state = VolumeState::Missing;
                } else if is_newly_added {
                    // 设置页主动新加且当前已挂载、无既有缓存 -> 直接进入 Syncing
                    prepare_slot_for_scan(&mut slot, &cfg_snap, target_snapshot, effects);
                    #[cfg(test)]
                    {
                        inner.scan_runs += 1;
                    }
                } else {
                    // 启动/热插拔/取消首扫等被动触发 -> NeedsConfirmation(InitialScan)
                    if let Some(h) = slot.stream_handle.take() {
                        effects.handles_to_drop.push(h);
                    }
                    slot.stream_snapshot = None;
                    slot.pending_confirm_reason = Some(VolumeConfirmReason::merge(
                        slot.pending_confirm_reason,
                        VolumeConfirmReason::InitialScan,
                    ));
                    slot.state = VolumeState::NeedsConfirmation;
                    if !matches!(slot.identity, VolumeIdentity::PendingPath { .. }) {
                        inner.manifest.upsert(slot.to_record());
                        manifest_changed = manifest_changed
                            || slot_manifest_dirty
                            || swapped
                            || prev_state != VolumeState::NeedsConfirmation
                            || prev_reason != slot.pending_confirm_reason;
                    }
                }
            } else if let Some(old_snap) = slot.scope_snapshot.clone() {
                let diff = diff_volume_scope(&old_snap, &target_snapshot, slot.dirty_upper_bound);
                if same_missing_mount {
                    match diff {
                        VolumeScopeAction::NoOp => {
                            slot.state = VolumeState::Missing;
                        }
                        VolumeScopeAction::PruneOnly => {
                            slot.state = VolumeState::Missing;
                            let clear_widened = slot.pending_confirm_reason.is_none()
                                || slot.pending_confirm_reason
                                    == Some(VolumeConfirmReason::FilterWidened);
                            if slot.store.is_empty() {
                                slot.scope_snapshot = Some(target_snapshot);
                                slot.dirty_upper_bound = false;
                                if clear_widened
                                    && slot.pending_confirm_reason
                                        == Some(VolumeConfirmReason::FilterWidened)
                                {
                                    slot.pending_confirm_reason = None;
                                }
                                inner.manifest.upsert(slot.to_record());
                                manifest_changed = true;
                            } else {
                                enqueue_prune_for_slot(
                                    &mut slot,
                                    target_snapshot,
                                    clear_widened,
                                    false,
                                    effects,
                                );
                            }
                        }
                        VolumeScopeAction::FullRebuild { reason } => {
                            slot.skip_missing_rescan = false;
                            slot.pending_confirm_reason = Some(VolumeConfirmReason::merge(
                                slot.pending_confirm_reason,
                                reason,
                            ));
                            slot.state = VolumeState::NeedsConfirmation;
                            if !matches!(slot.identity, VolumeIdentity::PendingPath { .. }) {
                                inner.manifest.upsert(slot.to_record());
                                manifest_changed = true;
                            }
                        }
                    }
                } else {
                    match diff {
                        VolumeScopeAction::FullRebuild { reason } => {
                            let merged =
                                VolumeConfirmReason::merge(slot.pending_confirm_reason, reason);
                            slot.pending_confirm_reason = Some(merged);
                            slot.state = VolumeState::NeedsConfirmation;
                            slot.in_flight_prune_snapshot = None;

                            if merged.allows_live_stream() && slot.kind == VolumeKind::LocalExternal
                            {
                                let stream_matches = slot.stream_handle.is_some()
                                    && slot
                                        .stream_snapshot
                                        .as_ref()
                                        .is_some_and(|s| s.semantically_eq(&target_snapshot));
                                if !stream_matches {
                                    enqueue_attach_stream_for_slot(
                                        &mut slot,
                                        target_snapshot,
                                        true,
                                        effects,
                                    );
                                }
                            } else {
                                if let Some(h) = slot.stream_handle.take() {
                                    effects.handles_to_drop.push(h);
                                }
                                slot.stream_snapshot = None;
                            }

                            if !matches!(slot.identity, VolumeIdentity::PendingPath { .. }) {
                                inner.manifest.upsert(slot.to_record());
                                manifest_changed = manifest_changed
                                    || slot_manifest_dirty
                                    || swapped
                                    || path_shifted
                                    || meta_changed
                                    || prev_state != VolumeState::NeedsConfirmation
                                    || prev_reason != slot.pending_confirm_reason;
                            }
                        }
                        VolumeScopeAction::PruneOnly => {
                            let blocked_by_higher = slot
                                .pending_confirm_reason
                                .is_some_and(|r| r != VolumeConfirmReason::FilterWidened);
                            let clear_widened = !blocked_by_higher;
                            let reattach_after =
                                !blocked_by_higher && slot.kind == VolumeKind::LocalExternal;

                            if slot.store.is_empty() {
                                slot.scope_snapshot = Some(target_snapshot.clone());
                                slot.dirty_upper_bound = false;
                                slot.in_flight_prune_snapshot = None;
                                if clear_widened {
                                    slot.pending_confirm_reason = None;
                                    slot.state = VolumeState::Online;
                                    if slot.kind == VolumeKind::LocalExternal
                                        && slot.stream_handle.is_none()
                                    {
                                        enqueue_attach_stream_for_slot(
                                            &mut slot,
                                            target_snapshot,
                                            true,
                                            effects,
                                        );
                                    }
                                } else {
                                    slot.state = VolumeState::NeedsConfirmation;
                                    if let Some(h) = slot.stream_handle.take() {
                                        effects.handles_to_drop.push(h);
                                    }
                                    slot.stream_snapshot = None;
                                }
                                if !matches!(slot.identity, VolumeIdentity::PendingPath { .. }) {
                                    inner.manifest.upsert(slot.to_record());
                                    manifest_changed = true;
                                }
                            } else {
                                slot.state = if blocked_by_higher
                                    || prev_state == VolumeState::NeedsConfirmation
                                {
                                    VolumeState::NeedsConfirmation
                                } else {
                                    VolumeState::Online
                                };
                                enqueue_prune_for_slot(
                                    &mut slot,
                                    target_snapshot,
                                    clear_widened,
                                    reattach_after,
                                    effects,
                                );
                            }
                        }
                        VolumeScopeAction::NoOp => {
                            if slot.pending_confirm_reason
                                == Some(VolumeConfirmReason::FilterWidened)
                            {
                                // 离线或网络卷放宽后未产生 dirty_upper_bound，ABA 回退直接恢复干净状态
                                slot.pending_confirm_reason = None;
                                slot_manifest_dirty = true;
                            }
                            if slot.pending_confirm_reason.is_some() {
                                // 存在高优先级原因（JournalInvalidated / SchemaUpgrade / InitialScan）
                                slot.state = VolumeState::NeedsConfirmation;
                                if let Some(h) = slot.stream_handle.take() {
                                    effects.handles_to_drop.push(h);
                                }
                                slot.stream_snapshot = None;
                                if !matches!(slot.identity, VolumeIdentity::PendingPath { .. }) {
                                    inner.manifest.upsert(slot.to_record());
                                    manifest_changed = manifest_changed
                                        || slot_manifest_dirty
                                        || swapped
                                        || path_shifted
                                        || meta_changed
                                        || prev_state != VolumeState::NeedsConfirmation
                                        || prev_reason != slot.pending_confirm_reason;
                                }
                            } else {
                                slot.state = VolumeState::Online;
                                if !matches!(slot.identity, VolumeIdentity::PendingPath { .. }) {
                                    inner.manifest.upsert(slot.to_record());
                                    manifest_changed = manifest_changed
                                        || slot_manifest_dirty
                                        || swapped
                                        || path_shifted
                                        || meta_changed
                                        || prev_state != VolumeState::Online
                                        || prev_reason != slot.pending_confirm_reason;
                                }
                                if slot.kind == VolumeKind::LocalExternal
                                    && slot.stream_handle.is_none()
                                {
                                    enqueue_attach_stream_for_slot(
                                        &mut slot,
                                        target_snapshot,
                                        true,
                                        effects,
                                    );
                                }
                            }
                        }
                    }
                }
            } else {
                // has_cache == true 且 scope_snapshot == None（旧版未认领或损坏 manifest）：
                // 不做 ABA 自动回退；按有效原因决定是否允许挂实时流
                let reason = slot
                    .pending_confirm_reason
                    .unwrap_or(VolumeConfirmReason::FilterWidened);
                slot.pending_confirm_reason = Some(reason);
                slot.state = VolumeState::NeedsConfirmation;

                if reason.allows_live_stream() && slot.kind == VolumeKind::LocalExternal {
                    let stream_matches = slot.stream_handle.is_some()
                        && slot
                            .stream_snapshot
                            .as_ref()
                            .is_some_and(|s| s.semantically_eq(&target_snapshot));
                    if !stream_matches {
                        enqueue_attach_stream_for_slot(&mut slot, target_snapshot, true, effects);
                    }
                } else {
                    if let Some(h) = slot.stream_handle.take() {
                        effects.handles_to_drop.push(h);
                    }
                    slot.stream_snapshot = None;
                }

                if !matches!(slot.identity, VolumeIdentity::PendingPath { .. }) {
                    inner.manifest.upsert(slot.to_record());
                    manifest_changed = manifest_changed
                        || slot_manifest_dirty
                        || swapped
                        || path_shifted
                        || meta_changed
                        || prev_state != VolumeState::NeedsConfirmation
                        || prev_reason != slot.pending_confirm_reason;
                }
            }
        } else {
            if prev_state != VolumeState::Offline || slot.stream_handle.is_some() {
                cancel_slot_work(&mut slot, effects);
            }
            if !suppressed_by_unmount
                && slot
                    .unmounting_until
                    .is_some_and(|until| Instant::now() >= until)
            {
                slot.unmounting_until = None;
            }
            slot.state = VolumeState::Offline;
            slot.active_mount_root = None;
            slot.is_remounted = false;
            slot.skip_missing_rescan = false;
            if reconcile_offline_slot_scope(&mut slot, &cfg_snap, effects) {
                slot_manifest_dirty = true;
            }
            slot.entry_count = slot.store.len() as u64;
            if (prev_state != VolumeState::Offline || slot_manifest_dirty)
                && !matches!(slot.identity, VolumeIdentity::PendingPath { .. })
            {
                if slot.dirty_cache {
                    effects.cache_saves.push(slot.cache_save(volumes_dir));
                }
                inner.manifest.upsert(slot.to_record());
                manifest_changed = true;
            }
        }

        if slot.state != prev_state
            || slot.active_mount_root != prev_active
            || slot.pending_confirm_reason != prev_reason
        {
            status_changed = true;
        }
        reconciled_slots.push(slot);
    }
    inner.slots.extend(reconciled_slots);

    if manifest_changed {
        effects.save_manifest = true;
    }
    if status_changed {
        effects.notify = inner.on_status_change.clone();
    }
    loaded_total
}

/// 在后台 `worker_queue` 执行已缓存卷分片的内存裁剪与落盘（允许作用于 Online / NeedsConfirmation / Offline / Missing，
/// 仅跳过 Syncing），并在 `.cache` 安全刷盘后再原子推进 `scope_snapshot` 与持久化 `manifest.json`。
#[allow(clippy::too_many_arguments)]
fn execute_prune_job(
    inner: &Arc<RwLock<CoordinatorInner>>,
    manifest_save_lock: &Arc<Mutex<()>>,
    queue: &Arc<WorkerQueue>,
    journal_uuid_provider: &JournalUuidProvider,
    journal_ready_wait: Duration,
    volumes_dir: &Path,
    job: PruneJob,
) {
    if job.cancel.load(Ordering::SeqCst) {
        return;
    }

    let should_prune = {
        let guard = inner.read();
        guard.slots.iter().any(|slot| {
            if slot.volume_id != job.volume_id
                || slot.generation != job.generation
                || slot.cancel_token.load(Ordering::SeqCst)
                || !str_paths_equal_ci(&slot.configured_path, &job.configured_path)
                || !str_paths_equal_ci(&slot.active_configured_path, &job.active_configured_path)
                || !slot.has_cache
                || slot.store.is_empty()
                || slot.state == VolumeState::Syncing
            {
                return false;
            }
            let live_target = VolumeScopeSnapshot::from_config(
                &guard.config,
                Path::new(&slot.configured_path),
                Path::new(&slot.active_configured_path),
            );
            live_target.semantically_eq(&job.target_snapshot)
        })
    };
    if !should_prune {
        let mut guard = inner.write();
        if let Some(slot) = guard.slots.iter_mut().find(|slot| {
            slot.volume_id == job.volume_id
                && slot.generation == job.generation
                && slot
                    .in_flight_prune_snapshot
                    .as_ref()
                    .is_some_and(|s| s.semantically_eq(&job.target_snapshot))
        }) {
            slot.in_flight_prune_snapshot = None;
        }
        return;
    }

    let active_root = Path::new(&job.active_configured_path);
    let filter = CompiledFilter::for_volume_snapshot(&job.target_snapshot, active_root);
    let include_hidden = job.target_snapshot.include_hidden;
    let removed = job.store.retain_paths(|path, is_dir| {
        // If the store was concurrently rebased onto a drifted mount root while off-lock,
        // leave entries untouched rather than letting a stale prefix HardDrop the whole shard.
        if !path_starts_with_ci(Path::new(path), active_root) {
            return true;
        }
        filter.evaluate_with_hidden(Path::new(path), is_dir, include_hidden)
            != crate::filter::ScopeDecision::HardDrop
    });

    let cache_save = {
        let mut guard = inner.write();
        let cfg_snap = guard.config.clone();
        let Some(slot) = guard.slots.iter_mut().find(|slot| {
            slot.volume_id == job.volume_id
                && slot.generation == job.generation
                && !slot.cancel_token.load(Ordering::SeqCst)
                && str_paths_equal_ci(&slot.configured_path, &job.configured_path)
                && str_paths_equal_ci(&slot.active_configured_path, &job.active_configured_path)
                && slot.has_cache
                && slot.state != VolumeState::Syncing
        }) else {
            if let Some(stale_slot) = guard.slots.iter_mut().find(|slot| {
                slot.volume_id == job.volume_id
                    && slot.generation == job.generation
                    && slot
                        .in_flight_prune_snapshot
                        .as_ref()
                        .is_some_and(|s| s.semantically_eq(&job.target_snapshot))
            }) {
                stale_slot.in_flight_prune_snapshot = None;
            }
            return;
        };
        let live_target = VolumeScopeSnapshot::from_config(
            &cfg_snap,
            Path::new(&slot.configured_path),
            Path::new(&slot.active_configured_path),
        );
        if !live_target.semantically_eq(&job.target_snapshot) && removed == 0 {
            // 内存索引未发生删减，直接安全放弃且不写盘
            slot.in_flight_prune_snapshot = None;
            return;
        }
        slot.entry_count = slot.store.len() as u64;
        if removed > 0 {
            slot.dirty_cache = true;
            slot.cache_seq = slot.cache_seq.wrapping_add(1);
            (!matches!(slot.identity, VolumeIdentity::PendingPath { .. }))
                .then(|| slot.cache_save(volumes_dir))
        } else {
            None
        }
    };

    // 先刷盘裁剪后的分片 .cache，确认落盘后再推进 manifest 中的 scope_snapshot
    if let Some(save) = cache_save {
        save_dirty_caches_and_mark(inner, vec![save]);
    }

    enum PruneCommitState {
        Ready {
            manifest_to_save: Option<VolumeManifest>,
            reattach_job: Option<AttachStreamJob>,
            follow_up_handles: Vec<VolumeStreamHandle>,
            follow_up_tasks: Vec<WorkerTask>,
            notify: Option<Arc<dyn Fn() + Send + Sync>>,
        },
        UnlinkOrphan,
        Aborted,
    }

    let _mguard = manifest_save_lock.lock();
    let commit = {
        let mut guard = inner.write();
        let cfg_snap = guard.config.clone();
        let slot_idx = guard.slots.iter().position(|slot| {
            slot.volume_id == job.volume_id
                && slot.generation == job.generation
                && !slot.cancel_token.load(Ordering::SeqCst)
                && slot.state != VolumeState::Syncing
        });
        if let Some(idx) = slot_idx {
            let live_target = VolumeScopeSnapshot::from_config(
                &cfg_snap,
                Path::new(&guard.slots[idx].configured_path),
                Path::new(&guard.slots[idx].active_configured_path),
            );
            let target_matches_live = live_target.semantically_eq(&job.target_snapshot);
            if !target_matches_live && removed == 0 {
                guard.slots[idx].in_flight_prune_snapshot = None;
                PruneCommitState::Aborted
            } else {
                let mut state_changed = false;
                let mut reattach_job = None;
                let mut follow_up_eff = TransitionEffects::default();
                let can_advance_manifest = !guard.slots[idx].dirty_cache;
                let mut should_upsert_manifest = can_advance_manifest;

                if can_advance_manifest {
                    let slot = &mut guard.slots[idx];
                    slot.scope_snapshot = Some(job.target_snapshot.clone());
                    slot.dirty_upper_bound = false;
                    slot.in_flight_prune_snapshot = None;

                    if target_matches_live {
                        if job.clear_filter_widened
                            && slot.pending_confirm_reason == Some(VolumeConfirmReason::FilterWidened)
                        {
                            slot.pending_confirm_reason = None;
                            if slot.state == VolumeState::NeedsConfirmation {
                                slot.state = VolumeState::Online;
                                state_changed = true;
                            }
                        }
                        if job.reattach_stream_after
                            && slot.state == VolumeState::Online
                            && slot.kind == VolumeKind::LocalExternal
                            && slot.stream_handle.is_none()
                        {
                            if let Some(mr) = slot.active_mount_root.as_deref() {
                                slot.stream_snapshot = Some(job.target_snapshot.clone());
                                reattach_job = Some(AttachStreamJob {
                                    volume_id: slot.volume_id.clone(),
                                    generation: slot.generation,
                                    mount_root: PathBuf::from(mr),
                                    subpath: slot.subpath.clone(),
                                    fstype: slot.fstype.clone(),
                                    active_path: PathBuf::from(&slot.active_configured_path),
                                    since_event_id: slot.last_event_id,
                                    stored_journal_uuid: slot.journal_uuid.clone(),
                                    require_journal_verify: true,
                                    target_snapshot: job.target_snapshot.clone(),
                                    cancel: Arc::clone(&slot.cancel_token),
                                    store: slot.store.clone(),
                                });
                            }
                        }
                    } else {
                        // 裁剪期间配置发生变更且内存/磁盘已按 job.target_snapshot 删减条目：
                        // 基于已落盘的 job.target_snapshot 与最新 live_target 重新对账，杜绝改回宽配置后静默丢条目。
                        match diff_volume_scope(&job.target_snapshot, &live_target, false) {
                            VolumeScopeAction::FullRebuild { reason } => {
                                let merged =
                                    VolumeConfirmReason::merge(slot.pending_confirm_reason, reason);
                                if slot.pending_confirm_reason != Some(merged) {
                                    slot.pending_confirm_reason = Some(merged);
                                    state_changed = true;
                                }
                                if slot.state == VolumeState::Online {
                                    slot.state = VolumeState::NeedsConfirmation;
                                    state_changed = true;
                                }
                                if slot.state == VolumeState::NeedsConfirmation
                                    && merged.allows_live_stream()
                                    && slot.kind == VolumeKind::LocalExternal
                                    && slot.stream_handle.is_none()
                                {
                                    if let Some(mr) = slot.active_mount_root.as_deref() {
                                        slot.stream_snapshot = Some(live_target.clone());
                                        reattach_job = Some(AttachStreamJob {
                                            volume_id: slot.volume_id.clone(),
                                            generation: slot.generation,
                                            mount_root: PathBuf::from(mr),
                                            subpath: slot.subpath.clone(),
                                            fstype: slot.fstype.clone(),
                                            active_path: PathBuf::from(&slot.active_configured_path),
                                            since_event_id: slot.last_event_id,
                                            stored_journal_uuid: slot.journal_uuid.clone(),
                                            require_journal_verify: true,
                                            target_snapshot: live_target,
                                            cancel: Arc::clone(&slot.cancel_token),
                                            store: slot.store.clone(),
                                        });
                                    }
                                }
                            }
                            VolumeScopeAction::PruneOnly => {
                                let blocked_by_higher = slot
                                    .pending_confirm_reason
                                    .is_some_and(|r| r != VolumeConfirmReason::FilterWidened);
                                let clear_widened = !blocked_by_higher;
                                let reattach_after = !blocked_by_higher
                                    && slot.state == VolumeState::Online
                                    && slot.kind == VolumeKind::LocalExternal;
                                enqueue_prune_for_slot(
                                    slot,
                                    live_target,
                                    clear_widened,
                                    reattach_after,
                                    &mut follow_up_eff,
                                );
                            }
                            VolumeScopeAction::NoOp => {}
                        }
                    }
                } else {
                    let slot = &mut guard.slots[idx];
                    slot.in_flight_prune_snapshot = None;
                    if !target_matches_live && removed > 0 {
                        // 即使本次 .cache 未能推进落盘，内存已被裁减，必须标记脏上界/待确认防止永久漏文件
                        slot.dirty_upper_bound = true;
                        if let VolumeScopeAction::FullRebuild { reason } =
                            diff_volume_scope(&job.target_snapshot, &live_target, true)
                        {
                            let merged =
                                VolumeConfirmReason::merge(slot.pending_confirm_reason, reason);
                            if slot.pending_confirm_reason != Some(merged) {
                                slot.pending_confirm_reason = Some(merged);
                                state_changed = true;
                            }
                            if slot.state == VolumeState::Online {
                                slot.state = VolumeState::NeedsConfirmation;
                                state_changed = true;
                            }
                            should_upsert_manifest = true;
                        }
                    }
                }

                #[cfg(test)]
                if can_advance_manifest {
                    guard.prune_runs += 1;
                }

                let manifest_to_save = if should_upsert_manifest
                    && !matches!(guard.slots[idx].identity, VolumeIdentity::PendingPath { .. })
                {
                    let rec = guard.slots[idx].to_record();
                    guard.manifest.upsert(rec);
                    Some(guard.manifest.clone())
                } else {
                    None
                };
                let notify = (removed > 0 || state_changed)
                    .then(|| guard.on_status_change.clone())
                    .flatten();
                PruneCommitState::Ready {
                    manifest_to_save,
                    reattach_job,
                    follow_up_handles: follow_up_eff.handles_to_drop,
                    follow_up_tasks: follow_up_eff.tasks,
                    notify,
                }
            }
        } else {
            let has_slot = guard.slots.iter().any(|s| s.volume_id == job.volume_id);
            let has_record = guard
                .manifest
                .records
                .iter()
                .any(|r| r.volume_id == job.volume_id);
            if !has_slot && !has_record {
                PruneCommitState::UnlinkOrphan
            } else {
                PruneCommitState::Aborted
            }
        }
    };

    match commit {
        PruneCommitState::Ready {
            manifest_to_save,
            reattach_job,
            follow_up_handles,
            follow_up_tasks,
            notify,
        } => {
            if let Some(manifest) = manifest_to_save {
                let _ = manifest.save_atomic(volumes_dir);
            }
            drop(_mguard);
            drop(follow_up_handles);
            for task in follow_up_tasks {
                queue.enqueue(task);
            }
            if let Some(attach_job) = reattach_job {
                attach_stream_for_job(
                    inner,
                    manifest_save_lock,
                    queue,
                    journal_uuid_provider,
                    journal_ready_wait,
                    volumes_dir,
                    attach_job,
                );
            }
            if let Some(cb) = notify {
                cb();
            }
        }
        PruneCommitState::UnlinkOrphan => {
            drop(_mguard);
            let cache_path = volumes_dir.join(format!("{}.cache", job.volume_id));
            unlink_orphan_cache_if_still_absent(inner, &job.volume_id, &cache_path);
        }
        PruneCommitState::Aborted => {}
    }
}

/// 统一处理索引配置变更对卷协调器的影响：按卷独立计算 `VolumeScopeSnapshot` 三分法 Diff、
/// 识别主动新加卷、回收已移除卷槽位与缓存文件，更新 `inner.config` 并对账分片。
fn apply_config_update(
    inner: &Arc<RwLock<CoordinatorInner>>,
    mounts: &[MountedFsInfo],
    volumes_dir: &Path,
    new_config: &AppConfig,
) -> TransitionEffects {
    let manifest_snapshot = inner.read().manifest.clone();
    let partition = partition_index_paths(new_config, &manifest_snapshot, mounts);
    let mut preloaded = preload_missing_target_caches(
        inner,
        &manifest_snapshot,
        &partition.mounted_targets,
        mounts,
        volumes_dir,
    );

    let mut guard = inner.write();
    let mut effects = TransitionEffects::default();
    let mut manifest_dirty = false;

    let prev_configured_paths: Vec<PathBuf> = guard
        .config
        .index
        .index_paths
        .iter()
        .map(|raw| crate::config::expand_tilde(raw))
        .collect();
    let mut newly_added_paths: HashSet<String> = HashSet::new();
    for target in &partition.mounted_targets {
        let already_in_cfg = prev_configured_paths
            .iter()
            .any(|p| paths_equal_ci(p, &target.configured_path));
        let already_in_slots = guard.slots.iter().any(|s| {
            paths_equal_ci(Path::new(&s.configured_path), &target.configured_path)
                || target
                    .mounted
                    .as_ref()
                    .is_some_and(|m| m.identity.volume_id() == s.volume_id)
        });
        if !already_in_cfg && !already_in_slots {
            newly_added_paths.insert(target.configured_path.to_string_lossy().to_ascii_lowercase());
        }
    }

    // Identify slots removed from index_paths.
    let mut kept_slots = Vec::with_capacity(guard.slots.len());
    let mut removed_ids = Vec::new();
    let mut removed_paths = Vec::new();
    for mut slot in guard.slots.drain(..) {
        let still_configured = partition.mounted_targets.iter().any(|target| {
            paths_equal_ci(Path::new(&slot.configured_path), &target.configured_path)
                || target
                    .mounted
                    .as_ref()
                    .is_some_and(|m| m.identity.volume_id() == slot.volume_id)
        });
        if still_configured {
            kept_slots.push(slot);
        } else {
            cancel_slot_work(&mut slot, &mut effects);
            removed_ids.push(slot.volume_id);
            removed_paths.push(slot.configured_path);
        }
    }
    guard.slots = kept_slots;

    for vid in &removed_ids {
        effects.push_cache_delete(vid.clone(), volumes_dir.join(format!("{vid}.cache")));
        if guard.manifest.remove_by_path_or_id(vid).is_some() {
            manifest_dirty = true;
        }
    }
    for p in &removed_paths {
        if let Some(removed_rec) = guard.manifest.remove_by_path_or_id(p) {
            let cache_path = volumes_dir.join(format!("{}.cache", removed_rec.volume_id));
            effects.push_cache_delete(removed_rec.volume_id, cache_path);
            manifest_dirty = true;
        }
    }

    guard.config = new_config.clone();
    reconcile_partition_locked(
        &mut guard,
        &partition.mounted_targets,
        mounts,
        volumes_dir,
        &mut preloaded,
        false,
        &newly_added_paths,
        &mut effects,
    );

    if manifest_dirty {
        effects.save_manifest = true;
    }
    if !removed_ids.is_empty() {
        effects.notify = guard.on_status_change.clone();
    }
    effects
}

fn refresh_mounts_locked(
    inner: &Arc<RwLock<CoordinatorInner>>,
    mounts: &[MountedFsInfo],
    volumes_dir: &Path,
    force_retry_missing: bool,
) -> TransitionEffects {
    let (config_snapshot, manifest_snapshot) = {
        let guard = inner.read();
        (guard.config.clone(), guard.manifest.clone())
    };
    let partition = partition_index_paths(&config_snapshot, &manifest_snapshot, mounts);
    let mut preloaded = preload_missing_target_caches(
        inner,
        &manifest_snapshot,
        &partition.mounted_targets,
        mounts,
        volumes_dir,
    );
    let mut guard = inner.write();
    let mut effects = TransitionEffects::default();
    let empty_added = HashSet::new();
    reconcile_partition_locked(
        &mut guard,
        &partition.mounted_targets,
        mounts,
        volumes_dir,
        &mut preloaded,
        force_retry_missing,
        &empty_added,
        &mut effects,
    );
    effects
}

fn run_worker_loop(
    inner: Arc<RwLock<CoordinatorInner>>,
    manifest_save_lock: Arc<Mutex<()>>,
    queue: Arc<WorkerQueue>,
    mount_lister: Arc<dyn Fn() -> Vec<MountedFsInfo> + Send + Sync>,
    journal_uuid_provider: JournalUuidProvider,
    journal_ready_wait: Duration,
    volumes_dir: PathBuf,
) {
    while let Some((task, _running)) = queue.pop_blocking() {
        match task {
            WorkerTask::Scan(job) => {
                execute_scan_job(
                    &inner,
                    &manifest_save_lock,
                    &queue,
                    &mount_lister,
                    &journal_uuid_provider,
                    journal_ready_wait,
                    &volumes_dir,
                    job,
                );
            }
            WorkerTask::AttachStream(job) => {
                attach_stream_for_job(
                    &inner,
                    &manifest_save_lock,
                    &queue,
                    &journal_uuid_provider,
                    journal_ready_wait,
                    &volumes_dir,
                    job,
                );
            }
            WorkerTask::Prune(job) => {
                execute_prune_job(
                    &inner,
                    &manifest_save_lock,
                    &queue,
                    &journal_uuid_provider,
                    journal_ready_wait,
                    &volumes_dir,
                    job,
                );
            }
        }
    }
}

/// 任务结束（含 panic 展开）时释放该卷的执行标记，避免 worker 永久卡在一个卷上。
struct RunningTaskGuard<'a> {
    queue: &'a WorkerQueue,
    volume_id: String,
}

impl Drop for RunningTaskGuard<'_> {
    fn drop(&mut self) {
        self.queue.finish(&self.volume_id);
    }
}

enum ScanFinalizeResult {
    Committed {
        notify: Option<Arc<dyn Fn() + Send + Sync>>,
    },
    Aborted,
}

fn finalize_scan_commit(
    inner: &Arc<RwLock<CoordinatorInner>>,
    manifest_save_lock: &Mutex<()>,
    volumes_dir: &Path,
    volume_id: &str,
    generation: u64,
    cache_save: CacheSave,
) -> ScanFinalizeResult {
    let still_before_save = {
        let guard = inner.read();
        guard.slots.iter().any(|s| {
            s.volume_id == volume_id
                && s.generation == generation
                && !s.cancel_token.load(Ordering::SeqCst)
        })
    };
    if !still_before_save {
        return ScanFinalizeResult::Aborted;
    }

    let cache_path = cache_save.cache_path.clone();
    save_dirty_caches_and_mark(inner, vec![cache_save]);

    commit_scan_phase3(
        inner,
        manifest_save_lock,
        volumes_dir,
        volume_id,
        generation,
        &cache_path,
    )
}

fn commit_scan_phase3(
    inner: &Arc<RwLock<CoordinatorInner>>,
    manifest_save_lock: &Mutex<()>,
    volumes_dir: &Path,
    volume_id: &str,
    generation: u64,
    cache_path: &Path,
) -> ScanFinalizeResult {
    let _mguard = manifest_save_lock.lock();
    enum Phase3State {
        Ready(Option<VolumeManifest>),
        UnlinkOrphan,
        Aborted,
    }
    let phase3 = {
        let mut guard = inner.write();
        if let Some(slot) = guard.slots.iter().find(|s| {
            s.volume_id == volume_id
                && s.generation == generation
                && !s.cancel_token.load(Ordering::SeqCst)
        }) {
            let pending_rec = (!slot.dirty_cache
                && !matches!(slot.identity, VolumeIdentity::PendingPath { .. }))
            .then(|| slot.to_record());
            if let Some(rec) = pending_rec {
                guard.manifest.upsert(rec);
                Phase3State::Ready(Some(guard.manifest.clone()))
            } else {
                Phase3State::Ready(None)
            }
        } else {
            let has_slot = guard.slots.iter().any(|s| s.volume_id == volume_id);
            let has_record = guard
                .manifest
                .records
                .iter()
                .any(|r| r.volume_id == volume_id);
            if !has_slot && !has_record {
                Phase3State::UnlinkOrphan
            } else {
                Phase3State::Aborted
            }
        }
    };

    match phase3 {
        Phase3State::Ready(manifest_to_save) => {
            if let Some(manifest) = manifest_to_save {
                let _ = manifest.save_atomic(volumes_dir);
            }
            let notify = {
                let mut guard = inner.write();
                let Some(slot) = guard.slots.iter_mut().find(|s| {
                    s.volume_id == volume_id
                        && s.generation == generation
                        && !s.cancel_token.load(Ordering::SeqCst)
                }) else {
                    return ScanFinalizeResult::Aborted;
                };
                slot.state = VolumeState::Online;
                guard.on_status_change.clone()
            };
            ScanFinalizeResult::Committed { notify }
        }
        Phase3State::UnlinkOrphan => {
            drop(_mguard);
            unlink_orphan_cache_if_still_absent(inner, volume_id, cache_path);
            ScanFinalizeResult::Aborted
        }
        Phase3State::Aborted => ScanFinalizeResult::Aborted,
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_scan_job(
    inner: &Arc<RwLock<CoordinatorInner>>,
    manifest_save_lock: &Arc<Mutex<()>>,
    queue: &Arc<WorkerQueue>,
    mount_lister: &Arc<dyn Fn() -> Vec<MountedFsInfo> + Send + Sync>,
    journal_uuid_provider: &JournalUuidProvider,
    journal_ready_wait: Duration,
    volumes_dir: &Path,
    job: ScanJob,
) {
    if job.cancel.load(Ordering::SeqCst) {
        return;
    }

    let event_id = if job.kind == VolumeKind::LocalExternal {
        fsevents::current_event_id()
    } else {
        0
    };
    let journal_uuid = if job.kind == VolumeKind::LocalExternal {
        let wait = if fstype_supports_persistent_fsevents_journal(&job.fstype) {
            journal_ready_wait
        } else {
            journal_ready_wait.min(Duration::from_millis(500))
        };
        poll_live_journal_uuid(journal_uuid_provider, &job.root, &job.cancel, wait)
    } else {
        None
    };

    let filter = CompiledFilter::for_volume_snapshot(&job.target_snapshot, &job.root);
    let mount_root_check = job.root.clone();
    let lister = Arc::clone(mount_lister);
    let is_mounted = move || {
        let mounts = (lister)();
        mounts.iter().any(|m| {
            !crate::volumes::is_system_mount_point(&m.mount_point)
                && path_starts_with_ci(&mount_root_check, Path::new(&m.mount_point))
        })
    };

    let scan_started = Instant::now();
    let notify_cb = inner.read().on_status_change.clone();
    let scanned_counter = Arc::clone(&job.scanned_count);
    let mut last_notify = Instant::now()
        .checked_sub(SCAN_PROGRESS_NOTIFY_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut progress = |count: u64| {
        scanned_counter.store(count, Ordering::Relaxed);
        if last_notify.elapsed() >= SCAN_PROGRESS_NOTIFY_INTERVAL {
            last_notify = Instant::now();
            if let Some(ref cb) = notify_cb {
                cb();
            }
        }
    };

    // 非旋转本地外置卷（SSD）启用多线程并行扫描；机械硬盘（HDD）、用户指定串行卷与网络卷采用单线程批量读取
    let scan_result = if job.kind == VolumeKind::LocalExternal && !job.use_serial_scan {
        scanner::scan_volume_compact_atomic_parallel(
            &job.root,
            &filter,
            &job.cancel,
            &is_mounted,
            &mut progress,
        )
    } else {
        scanner::scan_volume_compact_atomic(
            &job.root,
            &filter,
            &job.cancel,
            &is_mounted,
            &mut progress,
        )
    };

    match scan_result {
        Ok(compact) => {
            let scan_duration = scan_started.elapsed();
            if job.cancel.load(Ordering::SeqCst) {
                return;
            }
            // Pre-build search query structures off-lock before touching the slot.
            compact.warm_query_index();

            let commit = {
                let mut guard = inner.write();
                let Some(slot) = guard.slots.iter_mut().find(|s| {
                    s.volume_id == job.volume_id
                        && s.generation == job.generation
                        && !s.cancel_token.load(Ordering::SeqCst)
                }) else {
                    return;
                };

                let stale_handle = slot.stream_handle.take();
                slot.stream_snapshot = None;
                slot.in_flight_scan_snapshot = None;
                slot.in_flight_prune_snapshot = None;
                slot.scanned_count.store(0, Ordering::Relaxed);
                slot.store.replace_compact(compact);
                slot.scope_snapshot = Some(job.target_snapshot.clone());
                slot.dirty_upper_bound = false;
                slot.pending_confirm_reason = None;
                // Retain dirty_cache = true and leave manifest untouched until .cache is saved on disk.
                slot.dirty_cache = true;
                slot.cache_seq = slot.cache_seq.wrapping_add(1);
                slot.has_cache = true;
                slot.skip_missing_rescan = false;
                slot.entry_count = slot.store.len() as u64;
                slot.last_sync_at = Some(current_unix_secs());
                slot.last_scan_duration = Some(scan_duration);
                slot.last_event_id = (event_id != 0).then_some(event_id);
                slot.journal_uuid = journal_uuid.clone();
                let cp = slot.checkpoint();
                let cache_save = slot.cache_save(volumes_dir);
                let store_clone = slot.store.clone();
                (stale_handle, cache_save, store_clone, cp)
            };

            let (stale_handle, cache_save, store, checkpoint) = commit;
            drop(stale_handle);

            let ScanFinalizeResult::Committed { notify } = finalize_scan_commit(
                inner,
                manifest_save_lock,
                volumes_dir,
                &job.volume_id,
                job.generation,
                cache_save,
            ) else {
                return;
            };

            if job.kind == VolumeKind::LocalExternal && !job.cancel.load(Ordering::SeqCst) {
                attach_stream_for_job(
                    inner,
                    manifest_save_lock,
                    queue,
                    journal_uuid_provider,
                    journal_ready_wait,
                    volumes_dir,
                    AttachStreamJob {
                        volume_id: job.volume_id,
                        generation: job.generation,
                        mount_root: job.mount_root,
                        subpath: job.subpath,
                        fstype: job.fstype,
                        active_path: job.root,
                        since_event_id: checkpoint.map(|c| c.event_id),
                        stored_journal_uuid: journal_uuid,
                        require_journal_verify: false,
                        target_snapshot: job.target_snapshot,
                        cancel: job.cancel,
                        store,
                    },
                );
            }
            if let Some(cb) = notify {
                cb();
            }
        }
        Err(VolumeScanError::Cancelled) => {}
        Err(VolumeScanError::Disconnected(_)) => {
            if job.cancel.load(Ordering::SeqCst) {
                return;
            }
            let (dropped_handles, pending_save, pending_prune, notify) = {
                let mut guard = inner.write();
                let cfg_snap = guard.config.clone();
                let Some(slot) = guard.slots.iter_mut().find(|s| {
                    s.volume_id == job.volume_id
                        && s.generation == job.generation
                        && !s.cancel_token.load(Ordering::SeqCst)
                }) else {
                    return;
                };
                let mut eff = TransitionEffects::default();
                cancel_slot_work(slot, &mut eff);
                slot.state = VolumeState::Offline;
                slot.active_mount_root = None;
                slot.is_remounted = false;
                slot.skip_missing_rescan = false;
                // 扫描期间遇到临时断连时设置 30 秒退避重试，避免网络恢复后永远停留在 Offline，也避免死挂载高频打扰
                slot.next_reconnect_retry = Some(Instant::now() + Duration::from_secs(30));
                slot.entry_count = slot.store.len() as u64;
                reconcile_offline_slot_scope(slot, &cfg_snap, &mut eff);
                let save = (slot.dirty_cache
                    && !matches!(slot.identity, VolumeIdentity::PendingPath { .. }))
                .then(|| slot.cache_save(volumes_dir));
                let rec = (!matches!(slot.identity, VolumeIdentity::PendingPath { .. }))
                    .then(|| slot.to_record());
                if let Some(rec) = rec {
                    guard.manifest.upsert(rec);
                }
                (
                    eff.handles_to_drop,
                    save,
                    eff.tasks,
                    guard.on_status_change.clone(),
                )
            };
            drop(dropped_handles);
            if let Some(save) = pending_save {
                save_dirty_caches_and_mark(inner, vec![save]);
            }
            save_latest_manifest_locked(inner, manifest_save_lock, volumes_dir);
            for task in pending_prune {
                queue.enqueue(task);
            }
            if let Some(cb) = notify {
                cb();
            }
        }
        Err(VolumeScanError::RootUnavailable(_)) | Err(VolumeScanError::IndexLimit(_)) => {
            if job.cancel.load(Ordering::SeqCst) {
                return;
            }
            let (stale_handles, pending_prune, notify) = {
                let mut guard = inner.write();
                let cfg_snap = guard.config.clone();
                let Some(slot) = guard.slots.iter_mut().find(|s| {
                    s.volume_id == job.volume_id
                        && s.generation == job.generation
                        && !s.cancel_token.load(Ordering::SeqCst)
                }) else {
                    return;
                };
                let mut eff = TransitionEffects::default();
                if let Some(h) = slot.stream_handle.take() {
                    eff.handles_to_drop.push(h);
                }
                slot.in_flight_scan_snapshot = None;
                slot.stream_snapshot = None;
                slot.scanned_count.store(0, Ordering::Relaxed);
                slot.state = VolumeState::Missing;
                slot.skip_missing_rescan = true;
                reconcile_offline_slot_scope(slot, &cfg_snap, &mut eff);
                (eff.handles_to_drop, eff.tasks, guard.on_status_change.clone())
            };
            drop(stale_handles);
            for task in pending_prune {
                queue.enqueue(task);
            }
            if let Some(cb) = notify {
                cb();
            }
        }
    }
}

fn open_volume_stream(
    mount_root: &Path,
    subpath: &str,
    active_path: &Path,
    since: u64,
    tx: mpsc::Sender<Vec<fsevents::Event>>,
) -> Option<fsevents::Stream> {
    match fsevents::watch_device(mount_root, subpath, since, tx.clone()) {
        Some(stream) => Some(stream),
        None => fsevents::watch(&[active_path.to_path_buf()], since, tx),
    }
}

enum StreamStartPlan {
    Start {
        since: u64,
        new_journal_uuid: Option<String>,
        persist_manifest: bool,
    },
    KeepWithoutStream {
        new_journal_uuid: Option<String>,
        persist_manifest: bool,
    },
    InvalidateJournal,
}

fn plan_stream_start(
    persistent: bool,
    require_journal_verify: bool,
    stored_journal_uuid: Option<&str>,
    live_uuid: Option<&str>,
    since_event_id: Option<u64>,
    now_id: u64,
) -> StreamStartPlan {
    if !require_journal_verify {
        let since = since_event_id
            .filter(|&id| id > 0 && id <= now_id)
            .unwrap_or(fsevents::SINCE_NOW);
        if live_uuid.is_some() || !persistent {
            return StreamStartPlan::Start {
                since,
                new_journal_uuid: live_uuid.map(str::to_owned),
                persist_manifest: false,
            };
        }
        return StreamStartPlan::KeepWithoutStream {
            new_journal_uuid: None,
            persist_manifest: false,
        };
    }

    // require_journal_verify == true（冷/热重启、拔插重挂、在线裁剪续接、FilterWidened 实时流）：
    // 同挂载会话内（UUID 匹配且检查点有效）：APFS/HFS+ 与 ExFAT 均从 last_event_id 回放历史并续接实时流
    if live_uuid.is_some() && stored_journal_uuid == live_uuid {
        if let Some(id) = since_event_id.filter(|&id| id > 0 && id <= now_id) {
            return StreamStartPlan::Start {
                since: id,
                new_journal_uuid: live_uuid.map(str::to_owned),
                persist_manifest: false,
            };
        }
        if persistent {
            return StreamStartPlan::InvalidateJournal;
        }
        return StreamStartPlan::Start {
            since: fsevents::SINCE_NOW,
            new_journal_uuid: live_uuid.map(str::to_owned),
            persist_manifest: false,
        };
    }

    // 跨重启/拔插导致 UUID 轮换或缺失：
    // APFS/HFS+ 持久日志卷明确失效 -> NeedsConfirmation(JournalInvalidated)；
    // ExFAT/FAT32/NTFS 等非持久日志卷静默更新 UUID、复用旧缓存并从 SINCE_NOW 接实时流。
    if persistent {
        StreamStartPlan::InvalidateJournal
    } else {
        StreamStartPlan::Start {
            since: fsevents::SINCE_NOW,
            new_journal_uuid: live_uuid.map(str::to_owned),
            persist_manifest: stored_journal_uuid != live_uuid,
        }
    }
}

/// macOS 挂载卷时，`NSWorkspace.didMountNotification` 常比 `fseventsd` 加载 `.fseventsd/fseventsd-uuid`
/// 早数十至数百毫秒（大容量机械盘唤醒时可达 1~2 秒）；仅在首次返回 `None` 时按短退避重试，
/// 一旦拿到 UUID 或取消标记置位立即返回，彻底消除插盘瞬间的 `JournalInvalidated` 误判。
fn poll_live_journal_uuid(
    provider: &JournalUuidProvider,
    path: &Path,
    cancel: &AtomicBool,
    max_wait: Duration,
) -> Option<String> {
    if let Some(uuid) = (provider)(path) {
        return Some(uuid);
    }
    if max_wait.is_zero() {
        return None;
    }
    let deadline = Instant::now() + max_wait;
    let mut step = Duration::from_millis(100);
    while Instant::now() < deadline {
        if cancel.load(Ordering::SeqCst) {
            return None;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(step.min(remaining));
        if cancel.load(Ordering::SeqCst) {
            return None;
        }
        if let Some(uuid) = (provider)(path) {
            return Some(uuid);
        }
        step = (step * 2).min(Duration::from_millis(500));
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn attach_stream_for_job(
    inner: &Arc<RwLock<CoordinatorInner>>,
    manifest_save_lock: &Arc<Mutex<()>>,
    _queue: &Arc<WorkerQueue>,
    journal_uuid_provider: &JournalUuidProvider,
    journal_ready_wait: Duration,
    volumes_dir: &Path,
    job: AttachStreamJob,
) {
    if job.cancel.load(Ordering::SeqCst) {
        return;
    }

    let persistent = fstype_supports_persistent_fsevents_journal(&job.fstype);
    let wait = if persistent || job.stored_journal_uuid.is_some() {
        journal_ready_wait
    } else {
        Duration::ZERO
    };
    let live_uuid = poll_live_journal_uuid(journal_uuid_provider, &job.active_path, &job.cancel, wait);
    if job.cancel.load(Ordering::SeqCst) {
        return;
    }
    let now_id = fsevents::current_event_id();

    let (since, new_journal_uuid, mut persist_manifest) = match plan_stream_start(
        persistent,
        job.require_journal_verify,
        job.stored_journal_uuid.as_deref(),
        live_uuid.as_deref(),
        job.since_event_id,
        now_id,
    ) {
        StreamStartPlan::Start {
            since,
            new_journal_uuid,
            persist_manifest,
        } => (since, new_journal_uuid, persist_manifest),
        StreamStartPlan::KeepWithoutStream {
            new_journal_uuid,
            persist_manifest,
        } => {
            update_slot_journal_metadata_only(
                inner,
                manifest_save_lock,
                volumes_dir,
                &job.volume_id,
                job.generation,
                new_journal_uuid,
                persist_manifest,
            );
            return;
        }
        StreamStartPlan::InvalidateJournal => {
            mark_journal_invalidated_from_stream(
                inner,
                manifest_save_lock,
                volumes_dir,
                &job.volume_id,
                job.generation,
            );
            return;
        }
    };

    let (tx, rx) = mpsc::channel();
    let stream_opt = open_volume_stream(&job.mount_root, &job.subpath, &job.active_path, since, tx);
    let Some(stream) = stream_opt else {
        if persistent && job.require_journal_verify {
            mark_journal_invalidated_from_stream(
                inner,
                manifest_save_lock,
                volumes_dir,
                &job.volume_id,
                job.generation,
            );
        } else {
            update_slot_journal_metadata_only(
                inner,
                manifest_save_lock,
                volumes_dir,
                &job.volume_id,
                job.generation,
                new_journal_uuid,
                persist_manifest,
            );
        }
        return;
    };

    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    let stream_slot = Arc::new(Mutex::new(Some(stream)));
    let stream_slot_loop = Arc::clone(&stream_slot);
    let weak_inner = Arc::downgrade(inner);
    let manifest_lock_loop = Arc::clone(manifest_save_lock);
    let volumes_dir_loop = volumes_dir.to_path_buf();
    let volume_id = job.volume_id.clone();
    let generation = job.generation;
    let active_root = job.active_path.clone();
    let reopen_mount_root = job.mount_root.clone();
    let reopen_subpath = job.subpath.clone();
    let reopen_active_path = job.active_path.clone();
    let filter = CompiledFilter::for_volume_snapshot(&job.target_snapshot, &job.active_path);
    let include_hidden = job.target_snapshot.include_hidden;
    let store = job.store.clone();
    let notify = inner.read().on_status_change.clone();
    let initial_seen = if since == fsevents::SINCE_NOW {
        now_id
    } else {
        since
    };

    let loop_mount_root = reopen_mount_root.clone();
    let thread = std::thread::Builder::new()
        .name(format!("vol-fs-{}", &volume_id[..volume_id.len().min(8)]))
        .spawn(move || {
            run_volume_stream_loop(
                rx,
                stream_slot_loop,
                stop_flag,
                weak_inner,
                manifest_lock_loop,
                volumes_dir_loop,
                volume_id,
                generation,
                loop_mount_root,
                active_root,
                store,
                filter,
                include_hidden,
                initial_seen,
                VOLUME_GAP_QUIET,
                notify,
                move |replay_since| {
                    let (new_tx, new_rx) = mpsc::channel();
                    let new_stream = open_volume_stream(
                        &reopen_mount_root,
                        &reopen_subpath,
                        &reopen_active_path,
                        replay_since,
                        new_tx,
                    )?;
                    Some((new_stream, new_rx))
                },
            );
        })
        .ok();

    let handle = VolumeStreamHandle {
        stream: stream_slot,
        stop,
        thread,
    };

    let (stale, should_save_manifest) = {
        let mut guard = inner.write();
        if let Some(idx) = guard.slots.iter().position(|s| {
            s.volume_id == job.volume_id
                && s.generation == job.generation
                && s.live_stream_allowed()
                && !s.cancel_token.load(Ordering::SeqCst)
        }) {
            let slot = &mut guard.slots[idx];
            if slot.journal_uuid != new_journal_uuid {
                slot.journal_uuid = new_journal_uuid;
                persist_manifest = true;
            }
            slot.stream_snapshot = Some(job.target_snapshot);
            if slot.state == VolumeState::NeedsConfirmation && !slot.dirty_upper_bound {
                slot.dirty_upper_bound = true;
                persist_manifest = true;
            }
            let stale_handle = slot.stream_handle.replace(handle);
            let save = if persist_manifest
                && !matches!(slot.identity, VolumeIdentity::PendingPath { .. })
            {
                let rec = slot.to_record();
                guard.manifest.upsert(rec);
                true
            } else {
                false
            };
            (stale_handle, save)
        } else {
            (Some(handle), false)
        }
    };
    drop(stale);
    if should_save_manifest {
        save_latest_manifest_locked(inner, manifest_save_lock, volumes_dir);
    }
}

fn update_slot_journal_metadata_only(
    inner: &Arc<RwLock<CoordinatorInner>>,
    manifest_save_lock: &Mutex<()>,
    volumes_dir: &Path,
    volume_id: &str,
    generation: u64,
    new_journal_uuid: Option<String>,
    persist_manifest: bool,
) {
    let should_save = {
        let mut guard = inner.write();
        if let Some(idx) = guard.slots.iter().position(|s| {
            s.volume_id == volume_id
                && s.generation == generation
                && s.live_stream_allowed()
                && !s.cancel_token.load(Ordering::SeqCst)
        }) {
            let changed = guard.slots[idx].journal_uuid != new_journal_uuid;
            guard.slots[idx].journal_uuid = new_journal_uuid;
            guard.slots[idx].stream_snapshot = None;
            if (persist_manifest || changed)
                && !matches!(guard.slots[idx].identity, VolumeIdentity::PendingPath { .. })
            {
                let rec = guard.slots[idx].to_record();
                guard.manifest.upsert(rec);
                true
            } else {
                false
            }
        } else {
            false
        }
    };
    if should_save {
        save_latest_manifest_locked(inner, manifest_save_lock, volumes_dir);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_volume_stream_loop<R>(
    mut rx: mpsc::Receiver<Vec<fsevents::Event>>,
    stream_slot: Arc<Mutex<Option<fsevents::Stream>>>,
    stop: Arc<AtomicBool>,
    weak_inner: Weak<RwLock<CoordinatorInner>>,
    manifest_save_lock: Arc<Mutex<()>>,
    volumes_dir: PathBuf,
    volume_id: String,
    generation: u64,
    mount_root: PathBuf,
    active_root: PathBuf,
    store: IndexStore,
    filter: CompiledFilter,
    include_hidden: bool,
    initial_seen: u64,
    gap_quiet: Duration,
    notify: Option<Arc<dyn Fn() + Send + Sync>>,
    mut reopen_stream: R,
) where
    R: FnMut(u64) -> Option<(fsevents::Stream, mpsc::Receiver<Vec<fsevents::Event>>)>,
{
    let mut pending: HashMap<PathBuf, Change> = HashMap::new();
    let mut pending_events = 0usize;
    let mut first_pending: Option<Instant> = None;
    let mut applied_id = initial_seen;
    let mut seen = initial_seen;
    let mut gap_at: Option<Instant> = None;
    let mut gap_replays: u8 = 0;
    let mut journal_invalidated = false;

    while !stop.load(Ordering::Acquire) {
        if let Some(drop_instant) = gap_at {
            if drop_instant.elapsed() >= gap_quiet {
                // 静默窗口结束，从已安全应用的 applied_id 重开设备级流补齐历史事件
                if let Some((new_stream, new_rx)) = reopen_stream(applied_id) {
                    let old_stream = {
                        let mut slot_guard = stream_slot.lock();
                        if stop.load(Ordering::Acquire) {
                            drop(new_stream);
                            break;
                        }
                        slot_guard.replace(new_stream)
                    };
                    drop(old_stream);
                    rx = new_rx;
                    gap_replays = gap_replays.saturating_add(1);
                    gap_at = None;
                    pending.clear();
                    pending_events = 0;
                    first_pending = None;
                    seen = applied_id;
                    continue;
                } else {
                    journal_invalidated = true;
                    break;
                }
            }
        }

        let timeout = if let Some(drop_instant) = gap_at {
            gap_quiet
                .saturating_sub(drop_instant.elapsed())
                .max(Duration::from_millis(5))
        } else {
            match first_pending {
                Some(first) => Duration::from_millis(DEFAULT_DEBOUNCE_MS)
                    .min(MAX_DEBOUNCE.saturating_sub(first.elapsed())),
                None => Duration::from_millis(DEFAULT_DEBOUNCE_MS),
            }
        };

        let quiet = match rx.recv_timeout(timeout) {
            Ok(batch) => {
                for ev in batch {
                    if ev.flags & fsevents::HISTORY_DONE != 0 {
                        if gap_at.is_none() {
                            gap_replays = 0;
                        }
                        continue;
                    }
                    if ev.flags & (fsevents::IDS_WRAPPED | fsevents::KERNEL_DROPPED) != 0 {
                        journal_invalidated = true;
                        break;
                    }
                    if ev.flags & fsevents::USER_DROPPED != 0 {
                        if gap_replays >= VOLUME_GAP_REPLAYS {
                            journal_invalidated = true;
                            break;
                        }
                        // 纯 USER_DROPPED：冻结 applied_id，丢弃未刷盘暂存事件，待静默 2s 后历史重放
                        gap_at = Some(Instant::now());
                        pending.clear();
                        pending_events = 0;
                        first_pending = None;
                        continue;
                    }
                    if gap_at.is_some() {
                        // 等待 USER_DROPPED 静默重放期间不推进 seen/applied_id
                        continue;
                    }
                    seen = seen.max(ev.id);

                    // 仅检查 MUST_SCAN_SUBDIRS；不再监听 MOUNT_CHANGED（挂载生命周期由 NSWorkspace 与协调器管理）
                    if ev.flags & fsevents::MUST_SCAN_SUBDIRS != 0 {
                        if path_starts_with_ci(&active_root, &ev.path) {
                            // 根级或根祖先 MUST_SCAN_SUBDIRS 表示本会话改动已明确丢失 -> 受控确认
                            journal_invalidated = true;
                            break;
                        }
                        if path_starts_with_ci(&ev.path, &active_root)
                            && !filter.is_excluded(&ev.path)
                            && (include_hidden || !is_under_hidden_dir(&active_root, &ev.path))
                        {
                            // 监听根内部局部子目录：按常规子树回扫处理
                            let entry = pending.entry(ev.path).or_default();
                            entry.flags |= fsevents::ITEM_CREATED;
                            pending_events += 1;
                            first_pending.get_or_insert_with(Instant::now);
                        }
                        continue;
                    }
                    if ev.flags & fsevents::MOUNT_CHANGED != 0 {
                        continue;
                    }
                    // 根路径普通事件纵深防御：
                    // 1. 整卷挂载点根（active_root == mount_root）始终跳过：既防挂载幽灵事件触发同步全扫，也防卸载时误清离线缓存；
                    // 2. 卷内子路径根（active_root != mount_root）若在卷仍挂载时被删移，则仅清理该子树；
                    //    若被原位替换（ROOT_CHANGED）则标为日志失效交由用户确认。
                    if paths_equal_ci(&ev.path, &active_root) {
                        let is_subpath_root = !paths_equal_ci(&active_root, &mount_root);
                        if is_subpath_root
                            && ev.flags
                                & (fsevents::ITEM_REMOVED
                                    | fsevents::ITEM_RENAMED
                                    | fsevents::ROOT_CHANGED)
                                != 0
                            && mount_root.exists()
                        {
                            if std::fs::symlink_metadata(&active_root).is_err() {
                                let entry = pending.entry(ev.path).or_default();
                                entry.flags |= fsevents::ITEM_REMOVED;
                                pending_events += 1;
                                first_pending.get_or_insert_with(Instant::now);
                            } else if ev.flags & fsevents::ROOT_CHANGED != 0 {
                                journal_invalidated = true;
                                break;
                            }
                        }
                        continue;
                    }
                    if !path_starts_with_ci(&ev.path, &active_root)
                        || filter.is_excluded(&ev.path)
                        || (!include_hidden && is_under_hidden_dir(&active_root, &ev.path))
                    {
                        continue;
                    }
                    let entry = pending.entry(ev.path).or_default();
                    entry.flags |= ev.flags;
                    pending_events += 1;
                    if ev.inode.is_some() {
                        entry.inode = ev.inode;
                    }
                    first_pending.get_or_insert_with(Instant::now);
                }
                false
            }
            Err(mpsc::RecvTimeoutError::Timeout) => true,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        if journal_invalidated || stop.load(Ordering::Acquire) {
            break;
        }
        if gap_at.is_some() {
            continue;
        }

        let overdue = first_pending.is_some_and(|t| t.elapsed() >= MAX_DEBOUNCE);
        if !pending.is_empty() && (quiet || overdue || pending.len() >= MAX_BATCH_SIZE) {
            let drained = std::mem::take(&mut pending);
            let applied_events = std::mem::take(&mut pending_events);
            first_pending = None;
            let (batch, renames) = resolve_changes(drained, &store);
            match apply_batch(batch, renames, &store, &filter, include_hidden) {
                Ok((added, removed)) => {
                    applied_id = seen;
                    if let Some(arc) = weak_inner.upgrade() {
                        let mut guard = arc.write();
                        if let Some(slot) = guard.slots.iter_mut().find(|s| {
                            s.volume_id == volume_id
                                && s.generation == generation
                                && s.live_stream_allowed()
                        }) {
                            slot.last_event_id = Some(applied_id);
                            slot.entry_count = slot.store.len() as u64;
                            slot.dirty_cache = true;
                            slot.cache_seq = slot.cache_seq.wrapping_add(1);
                            slot.cache_persistence.applied_events = slot
                                .cache_persistence
                                .applied_events
                                .saturating_add(applied_events);
                            if slot.state == VolumeState::NeedsConfirmation {
                                slot.dirty_upper_bound = true;
                            }
                        }
                    }
                    if added + removed > 0 {
                        if let Some(ref cb) = notify {
                            cb();
                        }
                    }
                }
                Err(_) => {
                    journal_invalidated = true;
                    break;
                }
            }
        }
    }

    if journal_invalidated && !stop.load(Ordering::Acquire) {
        if let Some(arc) = weak_inner.upgrade() {
            mark_journal_invalidated_from_stream(
                &arc,
                &manifest_save_lock,
                &volumes_dir,
                &volume_id,
                generation,
            );
        }
    }
}

/// 当持久日志卷 UUID 失效或运行期发生不可恢复丢事件（根级 MUST_SCAN_SUBDIRS / KERNEL_DROPPED /
/// IDS_WRAPPED / 连续 3 次 USER_DROPPED 重放失败）时，停止实时流、保留旧缓存只读可搜，
/// 并按优先级升格为 `NeedsConfirmation(JournalInvalidated)`，绝不静默排入 `ScanJob`。
fn mark_journal_invalidated_from_stream(
    inner: &Arc<RwLock<CoordinatorInner>>,
    manifest_save_lock: &Mutex<()>,
    volumes_dir: &Path,
    volume_id: &str,
    generation: u64,
) {
    let (stale_handle, should_save_manifest, notify) = {
        let mut guard = inner.write();
        let Some(idx) = guard.slots.iter().position(|s| {
            s.volume_id == volume_id
                && s.generation == generation
                && matches!(s.state, VolumeState::Online | VolumeState::NeedsConfirmation)
                && !s.cancel_token.load(Ordering::SeqCst)
        }) else {
            return;
        };
        let slot = &mut guard.slots[idx];
        slot.cancel_token.store(true, Ordering::SeqCst);
        slot.generation = slot.generation.wrapping_add(1);
        slot.cancel_token = Arc::new(AtomicBool::new(false));
        slot.stream_snapshot = None;
        slot.in_flight_prune_snapshot = None;
        let stale = slot.stream_handle.take();
        slot.pending_confirm_reason = Some(VolumeConfirmReason::merge(
            slot.pending_confirm_reason,
            VolumeConfirmReason::JournalInvalidated,
        ));
        slot.state = VolumeState::NeedsConfirmation;
        let save = if !matches!(slot.identity, VolumeIdentity::PendingPath { .. }) {
            let rec = slot.to_record();
            guard.manifest.upsert(rec);
            true
        } else {
            false
        };
        (stale, save, guard.on_status_change.clone())
    };
    // VolumeStreamHandle::drop 内已规避当前线程 self-join，可安全在 vol-fs-* 线程上释放流句柄
    drop(stale_handle);
    if should_save_manifest {
        save_latest_manifest_locked(inner, manifest_save_lock, volumes_dir);
    }
    if let Some(cb) = notify {
        cb();
    }
}

fn is_under_hidden_dir(root: &Path, path: &Path) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.parent().is_some_and(|dirs| {
        dirs.components()
            .any(|part| watcher::is_hidden(Path::new(part.as_os_str())))
    })
}

/// 将用户配置的开关与分钟数换算为周期重扫基础间隔；关闭或非正值返回 `None`，并使用饱和乘法防止极端手工配置溢出。
fn effective_network_rescan_interval(enabled: bool, minutes: u64) -> Option<Duration> {
    if !enabled || minutes == 0 {
        return None;
    }
    Some(Duration::from_secs(minutes.saturating_mul(60)))
}

/// 判断在线网络卷是否到达周期重扫时机：仅允许干净 `Online` 且无待确认原因的网络卷触发，
/// 在用户配置间隔基础上引入上次扫描耗时 5 倍的自适应冷却（上限 6 小时）。
fn network_slot_rescan_due(slot: &VolumeSlot, base_interval: Duration, now_unix: u64) -> bool {
    if slot.kind != VolumeKind::Network
        || slot.state != VolumeState::Online
        || slot.pending_confirm_reason.is_some()
    {
        return false;
    }
    let adaptive_cap = base_interval.max(Duration::from_secs(6 * 3600));
    let adaptive_cooldown = slot
        .last_scan_duration
        .unwrap_or_default()
        .saturating_mul(5)
        .min(adaptive_cap);
    let effective_interval = base_interval.max(adaptive_cooldown);
    let elapsed_since_sync = slot
        .last_sync_at
        .map(|ts| now_unix.saturating_sub(ts) >= effective_interval.as_secs())
        .unwrap_or(true);
    let recently_triggered = slot
        .last_rescan_triggered
        .is_some_and(|at| at.elapsed() < effective_interval);
    elapsed_since_sync && !recently_triggered
}

fn persist_dirty_caches(
    inner: &Arc<RwLock<CoordinatorInner>>,
    manifest_save_lock: &Mutex<()>,
    volumes_dir: &Path,
) {
    let now = Instant::now();
    // 先持读锁快速检查，绝大多数周期内无待落盘脏分片，避免无谓抢占 CoordinatorInner 写锁阻塞搜索
    let any_due = {
        let guard = inner.read();
        guard.slots.iter().any(|slot| {
            slot.dirty_cache && slot.live_stream_allowed() && slot.cache_persistence.is_due(now)
        })
    };
    if !any_due {
        return;
    }

    let (saves, pending_records) = {
        let guard = inner.read();
        let mut saves = Vec::new();
        let mut records = Vec::new();
        for slot in &guard.slots {
            if slot.dirty_cache && slot.live_stream_allowed() && slot.cache_persistence.is_due(now)
            {
                saves.push(slot.cache_save(volumes_dir));
                records.push((slot.generation, slot.to_record()));
            }
        }
        (saves, records)
    };

    // 仅在对应 .cache 实际写盘成功后再更新 manifest，确保 manifest 检查点不超前于磁盘快照
    let saved_ids = save_dirty_caches_and_mark(inner, saves);
    if saved_ids.is_empty() {
        return;
    }
    let mut updated = false;
    {
        let mut guard = inner.write();
        for (expected_gen, rec) in pending_records {
            if !saved_ids.contains(&rec.volume_id) {
                continue;
            }
            if let Some(slot) = guard
                .slots
                .iter()
                .find(|s| s.volume_id == rec.volume_id && s.generation == expected_gen)
            {
                let record = if slot.dirty_cache { rec } else { slot.to_record() };
                guard.manifest.upsert(record);
                updated = true;
            }
        }
    }
    if updated {
        save_latest_manifest_locked(inner, manifest_save_lock, volumes_dir);
    }
}

fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache;
    use crate::compact_index::CompactIndex;
    use crate::indexer::FileEntry;
    use crate::volumes::{MountedFsInfo, VolumeIdentity, VolumeManifest, VolumeRecord};
    use parking_lot::Mutex;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    fn make_entry(name: &str, path: &str, is_dir: bool) -> FileEntry {
        let extension = if is_dir {
            String::new()
        } else {
            Path::new(name)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        };
        FileEntry {
            name: name.to_string(),
            path: path.to_string(),
            is_dir,
            size: 64,
            modified: 1_700_000_000,
            created_at: 1_700_000_000,
            extension,
        }
    }

    fn seed_volume_cache(
        volumes_dir: &Path,
        record: &VolumeRecord,
        entries: Vec<FileEntry>,
    ) {
        let compact = CompactIndex::from_entries(entries).unwrap();
        let cache_file = volumes_dir.join(format!("{}.cache", record.volume_id));
        let cp = record.last_event_id.map(|id| cache::Checkpoint {
            event_id: id,
            fingerprint: 0,
        });
        cache::save_compact(&compact, cp, &cache_file).unwrap();
    }

    fn base_mounts() -> Vec<MountedFsInfo> {
        vec![MountedFsInfo {
            mount_point: "/".to_string(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk3s1s1".to_string(),
            is_local: true,
            uuid: Some("SYS-ROOT-UUID".to_string()),
            total_bytes: 1_000_000,
            volume_name: "Macintosh HD".to_string(),
        }]
    }

    fn make_network_record(
        configured_path: &str,
        active_configured_path: &str,
        active_mount_root: Option<&str>,
        subpath: &str,
        display_name: &str,
        share_source: &str,
        entry_count: u64,
        dirty_filter: bool,
    ) -> VolumeRecord {
        let identity = VolumeIdentity::Network {
            fstype: "smbfs".to_string(),
            normalized_source: share_source.to_string(),
            subpath: subpath.to_string(),
        };
        VolumeRecord {
            volume_id: identity.volume_id(),
            configured_path: configured_path.to_string(),
            active_configured_path: active_configured_path.to_string(),
            active_mount_root: active_mount_root.map(str::to_string),
            subpath: subpath.to_string(),
            display_name: display_name.to_string(),
            kind: VolumeKind::Network,
            fstype: "smbfs".to_string(),
            identity,
            last_sync_at: Some(1_700_000_000),
            last_event_id: None,
            journal_uuid: None,
            entry_count,
            scope_snapshot: None,
            dirty_upper_bound: false,
            pending_confirm_reason: None,
            dirty_filter,
            filter_fingerprint: None,
        }
    }

    #[test]
    fn incremental_volume_cache_waits_for_threshold_but_exit_flushes() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let record = make_network_record(
            "/Volumes/Share",
            "/Volumes/Share",
            Some("/Volumes/Share"),
            "",
            "Share",
            "//nas.local/Share",
            1,
            false,
        );
        seed_volume_cache(
            &volumes_dir,
            &record,
            vec![make_entry("note.txt", "/Volumes/Share/note.txt", false)],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(record.clone());
        manifest.save_atomic(&volumes_dir).unwrap();

        let mut config = AppConfig::default();
        config.index.index_paths = vec![record.configured_path.clone()];
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(base_mounts),
        );
        assert_eq!(coord.initialize(&config, Arc::new(|| {})), 1);
        {
            coord.inner.write().slots[0].state = VolumeState::Online;
        }

        let mark_dirty = |event_id, events, time_since_save| {
            let mut inner = coord.inner.write();
            let slot = &mut inner.slots[0];
            slot.dirty_cache = true;
            slot.last_event_id = Some(event_id);
            slot.cache_seq += 1;
            slot.cache_persistence.applied_events += events;
            slot.cache_persistence.last_save = Instant::now() - time_since_save;
        };
        let path = volumes_dir.join(format!("{}.cache", record.volume_id));
        let checkpoint = || {
            cache::load_compact(&path)
                .unwrap()
                .1
                .map(|checkpoint| checkpoint.event_id)
        };
        let autosave = || persist_dirty_caches(&coord.inner, &coord.manifest_save_lock, &volumes_dir);

        mark_dirty(101, PERSIST_EVENTS - 1, Duration::ZERO);
        autosave();
        assert_eq!(checkpoint(), None);
        assert!(coord.inner.read().slots[0].dirty_cache);

        mark_dirty(102, 1, Duration::ZERO);
        autosave();
        assert_eq!(checkpoint(), Some(102));
        assert_eq!(coord.inner.read().slots[0].cache_persistence.saved_events, PERSIST_EVENTS);

        mark_dirty(103, 1, PERSIST_EVERY);
        autosave();
        assert_eq!(checkpoint(), Some(103));

        mark_dirty(104, 1, Duration::ZERO);
        autosave();
        assert_eq!(checkpoint(), Some(103));
        coord.persist_all();
        assert_eq!(checkpoint(), Some(104));
        assert!(!coord.inner.read().slots[0].dirty_cache);
    }

    #[test]
    fn offline_cache_loads_on_init_and_remains_searchable() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");

        let record = make_network_record(
            "/Volumes/Share",
            "/Volumes/Share",
            Some("/Volumes/Share"),
            "",
            "Share",
            "//nas.local/Share",
            2,
            false,
        );
        seed_volume_cache(
            &volumes_dir,
            &record,
            vec![
                make_entry("Share", "/Volumes/Share", true),
                make_entry("offline_notes.txt", "/Volumes/Share/offline_notes.txt", false),
            ],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(record.clone());
        manifest.save_atomic(&volumes_dir).unwrap();

        let mut config = AppConfig::default();
        config.index.index_paths = vec!["/Volumes/Share".to_string()];

        let mounts = Arc::new(Mutex::new(base_mounts()));
        let mounts_read = Arc::clone(&mounts);
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
        );

        assert!(coord.peek_has_any_cache(&config));
        let loaded = coord.initialize(&config, Arc::new(|| {}));
        assert_eq!(loaded, 2);

        let statuses = coord.statuses();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].state, VolumeState::Offline);
        assert_eq!(statuses[0].active_path, None);
        assert_eq!(statuses[0].entry_count, 2);
        assert!(!statuses[0].is_remounted);

        let primary = IndexStore::new();
        assert_eq!(coord.total_entry_count(&primary), 2);

        let (results_with_offline, _, err, _) = coord.search(
            &primary,
            "offline_notes",
            SearchMode::Auto,
            CaseSensitivity::Auto,
            50,
            true,
        );
        assert!(err.is_none());
        assert_eq!(results_with_offline.len(), 1);
        assert!(results_with_offline[0].is_offline);
        assert_eq!(results_with_offline[0].volume_name.as_deref(), Some("Share"));

        let (results_online_only, _, _, _) = coord.search(
            &primary,
            "offline_notes",
            SearchMode::Auto,
            CaseSensitivity::Auto,
            50,
            false,
        );
        assert!(results_online_only.is_empty());
    }

    #[test]
    fn remount_dash_one_drift_rebases_in_place_without_rescan() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");

        let record = make_network_record(
            "/Volumes/Share/Projects",
            "/Volumes/Share/Projects",
            Some("/Volumes/Share"),
            "Projects",
            "Share/Projects",
            "//nas.local/share",
            2,
            false,
        );
        seed_volume_cache(
            &volumes_dir,
            &record,
            vec![
                make_entry("Projects", "/Volumes/Share/Projects", true),
                make_entry("spec.md", "/Volumes/Share/Projects/spec.md", false),
            ],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(record.clone());
        manifest.save_atomic(&volumes_dir).unwrap();

        let mut config = AppConfig::default();
        config.index.index_paths = vec!["/Volumes/Share/Projects".to_string()];

        let mut initial_mounts = base_mounts();
        initial_mounts.push(MountedFsInfo {
            mount_point: "/Volumes/Share-1".to_string(),
            fstype: "smbfs".to_string(),
            from_name: "//bob:pass@NAS.LOCAL/share/".to_string(),
            is_local: false,
            uuid: None,
            total_bytes: 0,
            volume_name: "Share-1".to_string(),
        });
        let mounts = Arc::new(Mutex::new(initial_mounts));
        let mounts_read = Arc::clone(&mounts);
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
        );

        let loaded = coord.initialize(&config, Arc::new(|| {}));
        assert_eq!(loaded, 2);

        let statuses = coord.statuses();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].state, VolumeState::Online);
        assert!(statuses[0].is_remounted);
        assert_eq!(
            statuses[0].active_path.as_deref(),
            Some("/Volumes/Share-1/Projects")
        );

        let primary = IndexStore::new();
        let (hits, _, _, _) = coord.search(
            &primary,
            "in:/Volumes/Share spec.md",
            SearchMode::Auto,
            CaseSensitivity::Auto,
            50,
            true,
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].result.entry.path, "/Volumes/Share-1/Projects/spec.md");
        assert!(!hits[0].is_offline);

        // Cache on disk must reflect rebased paths
        let (reloaded_idx, _) =
            cache::load_compact(&volumes_dir.join(format!("{}.cache", record.volume_id))).unwrap();
        let reloaded_paths: Vec<String> = reloaded_idx
            .iter()
            .map(|(_, v)| v.materialize().path)
            .collect();
        assert!(reloaded_paths.contains(&"/Volumes/Share-1/Projects/spec.md".to_string()));

        // Now unmount -> Offline, then remount back at /Volumes/Share -> reverse rebase
        *mounts.lock() = base_mounts();
        coord.notify_volume_did_unmount(Path::new("/Volumes/Share-1"));
        assert_eq!(coord.statuses()[0].state, VolumeState::Offline);

        let mut restored_mounts = base_mounts();
        restored_mounts.push(MountedFsInfo {
            mount_point: "/Volumes/Share".to_string(),
            fstype: "smbfs".to_string(),
            from_name: "//nas.local/share".to_string(),
            is_local: false,
            uuid: None,
            total_bytes: 0,
            volume_name: "Share".to_string(),
        });
        *mounts.lock() = restored_mounts;
        coord.notify_volume_mounted(Path::new("/Volumes/Share"));

        let statuses = coord.statuses();
        assert_eq!(statuses[0].state, VolumeState::Online);
        assert!(!statuses[0].is_remounted);
        assert_eq!(
            statuses[0].active_path.as_deref(),
            Some("/Volumes/Share/Projects")
        );
        let (hits_back, _, _, _) = coord.search(
            &primary,
            "spec.md",
            SearchMode::Auto,
            CaseSensitivity::Auto,
            50,
            true,
        );
        assert_eq!(hits_back[0].result.entry.path, "/Volumes/Share/Projects/spec.md");
    }

    #[test]
    fn will_unmount_sets_cancel_token_preserves_cache_and_goes_offline() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let disk_dir = TempDir::new().unwrap();
        let disk_root = disk_dir.path().canonicalize().unwrap();

        // Populate a non-trivial tree so scanning takes non-zero time
        for d in 0..25 {
            let sub = disk_root.join(format!("dir_{d}"));
            std::fs::create_dir_all(&sub).unwrap();
            for f in 0..20 {
                std::fs::write(sub.join(format!("item_{f}.txt")), "payload").unwrap();
            }
        }

        let disk_str = disk_root.to_string_lossy().to_string();
        let identity = VolumeIdentity::LocalUuid {
            uuid: "EXT-CANCEL-UUID".to_string(),
            subpath: String::new(),
        };
        let record = VolumeRecord {
            volume_id: identity.volume_id(),
            configured_path: disk_str.clone(),
            active_configured_path: disk_str.clone(),
            active_mount_root: Some(disk_str.clone()),
            subpath: String::new(),
            display_name: "ExtCancel".to_string(),
            kind: VolumeKind::LocalExternal,
            fstype: "apfs".to_string(),
            identity: identity.clone(),
            last_sync_at: Some(1_700_000_000),
            last_event_id: None,
            journal_uuid: None,
            entry_count: 1,
            scope_snapshot: None,
            dirty_upper_bound: false,
            pending_confirm_reason: Some(VolumeConfirmReason::FilterWidened),
            dirty_filter: false,
            filter_fingerprint: None,
        };
        let sentinel_path = format!("{disk_str}/prior_snapshot.txt");
        seed_volume_cache(
            &volumes_dir,
            &record,
            vec![make_entry("prior_snapshot.txt", &sentinel_path, false)],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(record.clone());
        manifest.save_atomic(&volumes_dir).unwrap();

        let mut config = AppConfig::default();
        config.index.index_paths = vec![disk_str.clone()];
        config.index.include_hidden = true;

        let mut initial_mounts = base_mounts();
        initial_mounts.push(MountedFsInfo {
            mount_point: disk_str.clone(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk9s1".to_string(),
            is_local: true,
            uuid: Some("EXT-CANCEL-UUID".to_string()),
            total_bytes: 500_000,
            volume_name: "ExtCancel".to_string(),
        });
        let mounts = Arc::new(Mutex::new(initial_mounts));
        let mounts_read = Arc::clone(&mounts);
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
        );

        let loaded = coord.initialize(&config, Arc::new(|| {}));
        assert_eq!(loaded, 1);
        assert_eq!(coord.statuses()[0].state, VolumeState::NeedsConfirmation);
        assert_eq!(coord.confirm_volume_scan(&record.volume_id), Ok(true));

        // Grab cancel token before unmount
        let token = coord.slot_cancel_token(&record.volume_id).unwrap();

        // Immediately trigger willUnmount
        *mounts.lock() = base_mounts();
        coord.notify_volume_will_unmount(&disk_root);

        assert!(token.load(Ordering::SeqCst));
        let statuses = coord.statuses();
        assert_eq!(statuses[0].state, VolumeState::Offline);

        // Give worker thread time to observe cancellation and finish aborting
        std::thread::sleep(Duration::from_millis(200));

        // Prior cache on disk and in store must remain untouched (sentinel preserved)
        let primary = IndexStore::new();
        let (hits, _, _, _) = coord.search(
            &primary,
            "prior_snapshot",
            SearchMode::Auto,
            CaseSensitivity::Auto,
            10,
            true,
        );
        assert_eq!(hits.len(), 1);
        assert!(hits[0].is_offline);

        let (disk_idx, _) =
            cache::load_compact(&volumes_dir.join(format!("{}.cache", record.volume_id))).unwrap();
        let disk_paths: Vec<String> = disk_idx.iter().map(|(_, v)| v.materialize().path).collect();
        assert_eq!(disk_paths, vec![sentinel_path]);
    }

    #[test]
    fn config_removal_and_clear_cache_delete_files_and_filter_change_marks_dirty() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");

        let rec_a = make_network_record(
            "/Volumes/VolA",
            "/Volumes/VolA",
            Some("/Volumes/VolA"),
            "",
            "VolA",
            "//nas/VolA",
            1,
            false,
        );
        let rec_b = make_network_record(
            "/Volumes/VolB",
            "/Volumes/VolB",
            Some("/Volumes/VolB"),
            "",
            "VolB",
            "//nas/VolB",
            1,
            false,
        );
        seed_volume_cache(
            &volumes_dir,
            &rec_a,
            vec![make_entry("a.txt", "/Volumes/VolA/a.txt", false)],
        );
        seed_volume_cache(
            &volumes_dir,
            &rec_b,
            vec![make_entry("b.txt", "/Volumes/VolB/b.txt", false)],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(rec_a.clone());
        manifest.upsert(rec_b.clone());
        manifest.save_atomic(&volumes_dir).unwrap();

        let mut config = AppConfig::default();
        config.index.index_paths = vec!["/Volumes/VolA".to_string(), "/Volumes/VolB".to_string()];

        let mounts = Arc::new(Mutex::new(base_mounts()));
        let mounts_read = Arc::clone(&mounts);
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
        );
        coord.initialize(&config, Arc::new(|| {}));

        // 1. Widen filter rule while offline -> pending_confirm_reason = Some(FilterWidened) in manifest
        let mut updated_filter_cfg = config.clone();
        updated_filter_cfg.index.include_hidden = true;
        coord.on_config_updated(&updated_filter_cfg);

        let m_after_filter = VolumeManifest::load_from_dir(&volumes_dir);
        assert!(m_after_filter
            .records
            .iter()
            .all(|r| r.pending_confirm_reason == Some(VolumeConfirmReason::FilterWidened)));

        // 2. Remove VolA from index_paths -> VolA .cache & manifest record removed
        let mut removed_cfg = updated_filter_cfg.clone();
        removed_cfg.index.index_paths = vec!["/Volumes/VolB".to_string()];
        coord.on_config_updated(&removed_cfg);

        assert!(!volumes_dir.join(format!("{}.cache", rec_a.volume_id)).exists());
        assert!(volumes_dir.join(format!("{}.cache", rec_b.volume_id)).exists());
        let m_after_remove = VolumeManifest::load_from_dir(&volumes_dir);
        assert_eq!(m_after_remove.records.len(), 1);
        assert_eq!(m_after_remove.records[0].volume_id, rec_b.volume_id);
        assert_eq!(coord.statuses().len(), 1);

        // 3. Explicit clear_volume_cache on VolB -> VolB .cache & manifest record removed
        assert_eq!(coord.clear_volume_cache("/Volumes/VolB"), Ok(true));
        assert!(!volumes_dir.join(format!("{}.cache", rec_b.volume_id)).exists());
        let m_after_clear = VolumeManifest::load_from_dir(&volumes_dir);
        assert!(m_after_clear.records.is_empty());
        assert!(coord.statuses().is_empty());
    }

    #[test]
    fn dirty_offline_volume_rescans_with_new_filters_when_mounted() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let disk_dir = TempDir::new().unwrap();
        let disk_root = disk_dir.path().canonicalize().unwrap();
        std::fs::write(disk_root.join("keep.txt"), "ok").unwrap();
        std::fs::create_dir_all(disk_root.join("secret")).unwrap();
        std::fs::write(disk_root.join("secret/hidden.txt"), "no").unwrap();

        let disk_str = disk_root.to_string_lossy().to_string();
        let identity = VolumeIdentity::LocalUuid {
            uuid: "EXT-DIRTY-UUID".to_string(),
            subpath: String::new(),
        };
        let record = VolumeRecord {
            volume_id: identity.volume_id(),
            configured_path: disk_str.clone(),
            active_configured_path: disk_str.clone(),
            active_mount_root: None,
            subpath: String::new(),
            display_name: "ExtDirty".to_string(),
            kind: VolumeKind::LocalExternal,
            fstype: "apfs".to_string(),
            identity: identity.clone(),
            last_sync_at: Some(1_700_000_000),
            last_event_id: Some(fsevents::current_event_id()),
            journal_uuid: fsevents::journal_uuid_for_path(&disk_root),
            entry_count: 2,
            scope_snapshot: None,
            dirty_upper_bound: false,
            pending_confirm_reason: None,
            dirty_filter: false,
            filter_fingerprint: None,
        };
        seed_volume_cache(
            &volumes_dir,
            &record,
            vec![
                make_entry("keep.txt", &format!("{disk_str}/keep.txt"), false),
                make_entry("hidden.txt", &format!("{disk_str}/secret/hidden.txt"), false),
            ],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(record.clone());
        manifest.save_atomic(&volumes_dir).unwrap();

        let mut config = AppConfig::default();
        config.index.index_paths = vec![disk_str.clone()];
        config.index.include_hidden = true;

        let mounts = Arc::new(Mutex::new(base_mounts()));
        let mounts_read = Arc::clone(&mounts);
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
        );
        coord.initialize(&config, Arc::new(|| {}));

        // Add exclusion while offline -> PruneOnly evicts hidden.txt and advances snapshot
        let mut new_cfg = config.clone();
        new_cfg.index.exclude_paths.push(format!("{disk_str}/secret"));
        coord.on_config_updated(&new_cfg);
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord.statuses()[0].state, VolumeState::Offline);

        // Now plug disk in and notify -> directly Online without full rescan
        let mut online_mounts = base_mounts();
        online_mounts.push(MountedFsInfo {
            mount_point: disk_str.clone(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk10s1".to_string(),
            is_local: true,
            uuid: Some("EXT-DIRTY-UUID".to_string()),
            total_bytes: 500_000,
            volume_name: "ExtDirty".to_string(),
        });
        *mounts.lock() = online_mounts;
        coord.notify_volume_mounted(&disk_root);
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord.statuses()[0].state, VolumeState::Online);
        assert_eq!(coord.scan_run_count(), 0);

        let primary = IndexStore::new();
        let (keep_hits, _, _, _) = coord.search(
            &primary,
            "keep.txt",
            SearchMode::Auto,
            CaseSensitivity::Auto,
            10,
            true,
        );
        assert_eq!(keep_hits.len(), 1);
        let (secret_hits, _, _, _) = coord.search(
            &primary,
            "hidden.txt",
            SearchMode::Auto,
            CaseSensitivity::Auto,
            10,
            true,
        );
        assert!(secret_hits.is_empty());

        let m_final = VolumeManifest::load_from_dir(&volumes_dir);
        assert_eq!(m_final.records[0].pending_confirm_reason, None);
        assert!(m_final.records[0].scope_snapshot.is_some());
    }

    #[test]
    fn missing_subpath_while_mounted_transitions_to_missing_and_rescan_recovers() {
        let cache_dir = TempDir::new().unwrap();
        let disk_dir = TempDir::new().unwrap();
        let disk_root = disk_dir.path().canonicalize().unwrap();
        let subpath_dir = disk_root.join("Projects");

        let disk_str = disk_root.to_string_lossy().to_string();
        let configured_str = subpath_dir.to_string_lossy().to_string();

        let mut config = AppConfig::default();
        config.index.index_paths = vec![configured_str.clone()];
        config.index.include_hidden = true;

        let mut mounts = base_mounts();
        mounts.push(MountedFsInfo {
            mount_point: disk_str.clone(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk11s1".to_string(),
            is_local: true,
            uuid: Some("EXT-MISSING-UUID".to_string()),
            total_bytes: 500_000,
            volume_name: "ExtMissing".to_string(),
        });
        let mounts_state = Arc::new(Mutex::new(mounts));
        let mounts_read = Arc::clone(&mounts_state);
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
        );

        coord.initialize(&config, Arc::new(|| {}));
        assert_eq!(coord.statuses()[0].state, VolumeState::NeedsConfirmation);
        assert_eq!(coord.confirm_all_pending_volume_scans(), 1);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if coord.statuses()[0].state == VolumeState::Missing {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(coord.statuses()[0].state, VolumeState::Missing);

        // Now create the missing subpath on the mounted volume and trigger rescan
        std::fs::create_dir_all(&subpath_dir).unwrap();
        std::fs::write(subpath_dir.join("restored.md"), "ok").unwrap();
        assert_eq!(coord.rescan_volume(&configured_str), Ok(true));

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if coord.statuses()[0].state == VolumeState::Online {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(coord.statuses()[0].state, VolumeState::Online);

        let primary = IndexStore::new();
        let (hits, _, _, _) = coord.search(
            &primary,
            "restored.md",
            SearchMode::Auto,
            CaseSensitivity::Auto,
            10,
            false,
        );
        assert_eq!(hits.len(), 1);

        // Unmount -> rescan_volume returns Ok(false)
        *mounts_state.lock() = base_mounts();
        coord.notify_volume_did_unmount(&disk_root);
        assert_eq!(coord.statuses()[0].state, VolumeState::Offline);
        assert_eq!(coord.rescan_volume(&configured_str), Ok(false));
    }

    #[test]
    fn volume_swap_at_same_configured_path_resets_old_cache_and_scans_new_volume() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let disk_dir = TempDir::new().unwrap();
        let disk_root = disk_dir.path().canonicalize().unwrap();
        let disk_str = disk_root.to_string_lossy().to_string();

        // Create Disk B's file BEFORE seeding Disk A's cache + event checkpoint so FSEvents
        // cannot synthesize beta_only.txt if Disk A's store fails to reset on swap.
        std::fs::write(disk_root.join("beta_only.txt"), "disk-b").unwrap();

        let id_a = VolumeIdentity::LocalUuid {
            uuid: "SWAP-DISK-A".to_string(),
            subpath: String::new(),
        };
        let rec_a = VolumeRecord {
            volume_id: id_a.volume_id(),
            configured_path: disk_str.clone(),
            active_configured_path: disk_str.clone(),
            active_mount_root: Some(disk_str.clone()),
            subpath: String::new(),
            display_name: "ExtDisk".to_string(),
            kind: VolumeKind::LocalExternal,
            fstype: "apfs".to_string(),
            identity: id_a,
            last_sync_at: Some(1_700_000_000),
            last_event_id: Some(fsevents::current_event_id()),
            journal_uuid: fsevents::journal_uuid_for_path(&disk_root),
            entry_count: 1,
            scope_snapshot: None,
            dirty_upper_bound: false,
            pending_confirm_reason: None,
            dirty_filter: false,
            filter_fingerprint: None,
        };
        seed_volume_cache(
            &volumes_dir,
            &rec_a,
            vec![make_entry(
                "alpha_only.txt",
                &format!("{disk_str}/alpha_only.txt"),
                false,
            )],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(rec_a);
        manifest.save_atomic(&volumes_dir).unwrap();

        let mut config = AppConfig::default();
        config.index.index_paths = vec![disk_str.clone()];
        config.index.include_hidden = true;

        let mut mounts_a = base_mounts();
        mounts_a.push(MountedFsInfo {
            mount_point: disk_str.clone(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk20s1".to_string(),
            is_local: true,
            uuid: Some("SWAP-DISK-A".to_string()),
            total_bytes: 500_000,
            volume_name: "ExtDisk".to_string(),
        });
        let mounts_state = Arc::new(Mutex::new(mounts_a));
        let mounts_read = Arc::clone(&mounts_state);
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
        );

        coord.initialize(&config, Arc::new(|| {}));
        assert_eq!(coord.statuses()[0].state, VolumeState::Online);

        let primary = IndexStore::new();
        let (alpha_hits, _, _, _) = coord.search(
            &primary,
            "alpha_only.txt",
            SearchMode::Auto,
            CaseSensitivity::Auto,
            10,
            false,
        );
        assert_eq!(alpha_hits.len(), 1);

        // Unmount Disk A and mount a completely different Disk B at the same path
        *mounts_state.lock() = base_mounts();
        coord.notify_volume_did_unmount(&disk_root);
        assert_eq!(coord.statuses()[0].state, VolumeState::Offline);

        let mut mounts_b = base_mounts();
        mounts_b.push(MountedFsInfo {
            mount_point: disk_str.clone(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk21s1".to_string(),
            is_local: true,
            uuid: Some("SWAP-DISK-B".to_string()),
            total_bytes: 800_000,
            volume_name: "ExtDisk".to_string(),
        });
        *mounts_state.lock() = mounts_b;
        coord.notify_volume_mounted(&disk_root);

        let expected_vid_b = VolumeIdentity::LocalUuid {
            uuid: "SWAP-DISK-B".to_string(),
            subpath: String::new(),
        }
        .volume_id();

        // Passive swap to uncached Disk B wipes Disk A and waits for confirmation
        assert_eq!(coord.statuses()[0].volume_id, expected_vid_b);
        assert_eq!(coord.statuses()[0].state, VolumeState::NeedsConfirmation);
        assert_eq!(
            coord.statuses()[0].confirm_reason,
            Some(VolumeConfirmReason::InitialScan)
        );
        assert_eq!(coord.confirm_volume_scan(&expected_vid_b), Ok(true));

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let st = coord.statuses();
            if st[0].volume_id == expected_vid_b && st[0].state == VolumeState::Online {
                let (beta_hits, _, _, _) = coord.search(
                    &primary,
                    "beta_only.txt",
                    SearchMode::Auto,
                    CaseSensitivity::Auto,
                    10,
                    false,
                );
                if !beta_hits.is_empty() {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(30));
        }

        let st = coord.statuses();
        assert_eq!(st[0].volume_id, expected_vid_b);
        assert_eq!(st[0].state, VolumeState::Online);

        let (alpha_after, _, _, _) = coord.search(
            &primary,
            "alpha_only.txt",
            SearchMode::Auto,
            CaseSensitivity::Auto,
            10,
            true,
        );
        assert!(
            alpha_after.is_empty(),
            "old Disk A entries must be wiped when Disk B mounts at the same path"
        );

        let (beta_after, _, _, _) = coord.search(
            &primary,
            "beta_only.txt",
            SearchMode::Auto,
            CaseSensitivity::Auto,
            10,
            false,
        );
        assert_eq!(beta_after.len(), 1);
    }

    #[test]
    fn live_fsevents_updates_are_flushed_to_cache_on_will_unmount() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let disk_dir = TempDir::new().unwrap();
        let disk_root = disk_dir.path().canonicalize().unwrap();
        let disk_str = disk_root.to_string_lossy().to_string();

        std::fs::write(disk_root.join("initial_file.txt"), "v1").unwrap();

        let mut config = AppConfig::default();
        config.index.index_paths = vec![disk_str.clone()];
        config.index.include_hidden = true;

        let mut mounts = base_mounts();
        mounts.push(MountedFsInfo {
            mount_point: disk_str.clone(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk22s1".to_string(),
            is_local: true,
            uuid: Some("EXT-LIVE-FLUSH-UUID".to_string()),
            total_bytes: 500_000,
            volume_name: "ExtLive".to_string(),
        });
        let mounts_state = Arc::new(Mutex::new(mounts));
        let mounts_read = Arc::clone(&mounts_state);
        let vid = VolumeIdentity::LocalUuid {
            uuid: "EXT-LIVE-FLUSH-UUID".to_string(),
            subpath: String::new(),
        }
        .volume_id();

        {
            let coord = VolumeCoordinator::with_mount_lister(
                cache_dir.path().to_path_buf(),
                Arc::new(move || mounts_read.lock().clone()),
            );
            coord.initialize(&config, Arc::new(|| {}));
            assert_eq!(coord.statuses()[0].state, VolumeState::NeedsConfirmation);
            assert_eq!(coord.confirm_all_pending_volume_scans(), 1);
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if coord.statuses()[0].state == VolumeState::Online {
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            assert_eq!(coord.statuses()[0].state, VolumeState::Online);

            // Give the per-volume FSEventStream thread a brief moment to register
            std::thread::sleep(Duration::from_millis(300));
            std::fs::write(disk_root.join("live_added.txt"), "live").unwrap();

            let primary = IndexStore::new();
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut seen_live = false;
            while Instant::now() < deadline {
                let (hits, _, _, _) = coord.search(
                    &primary,
                    "live_added.txt",
                    SearchMode::Auto,
                    CaseSensitivity::Auto,
                    10,
                    false,
                );
                if !hits.is_empty() {
                    seen_live = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(seen_live, "per-volume FSEvents must ingest live_added.txt");

            // Unmount immediately before the 60s periodic autosave fires
            coord.notify_volume_will_unmount(&disk_root);
            assert_eq!(coord.statuses()[0].state, VolumeState::Offline);
        }

        // Verify .cache on disk was flushed by willUnmount and retained live_added.txt
        let (reloaded_compact, _) = cache::load_compact(&volumes_dir.join(format!("{vid}.cache"))).unwrap();
        let paths: Vec<String> = reloaded_compact
            .iter()
            .map(|(_, v)| v.materialize().path)
            .collect();
        assert!(
            paths.iter().any(|p| p.ends_with("/live_added.txt")),
            "dirty store must be flushed on willUnmount before saving manifest"
        );
    }

    #[test]
    fn reconnecting_cached_local_external_with_missing_or_mismatched_journal_enters_needs_confirmation()
    {
        for stored_journal in [None, Some("STALE-MISMATCHED-UUID".to_string())] {
            let cache_dir = TempDir::new().unwrap();
            let volumes_dir = cache_dir.path().join("volumes");
            let disk_dir = TempDir::new().unwrap();
            let disk_root = disk_dir.path().canonicalize().unwrap();
            let disk_str = disk_root.to_string_lossy().to_string();

            std::fs::write(disk_root.join("offline_catchup.txt"), "new").unwrap();

            let identity = VolumeIdentity::LocalUuid {
                uuid: "EXT-JOURNAL-CHECK".to_string(),
                subpath: String::new(),
            };
            let record = VolumeRecord {
                volume_id: identity.volume_id(),
                configured_path: disk_str.clone(),
                active_configured_path: disk_str.clone(),
                active_mount_root: Some(disk_str.clone()),
                subpath: String::new(),
                display_name: "ExtJournal".to_string(),
                kind: VolumeKind::LocalExternal,
                fstype: "apfs".to_string(),
                identity: identity.clone(),
                last_sync_at: Some(1_700_000_000),
                last_event_id: stored_journal.as_ref().map(|_| 1),
                journal_uuid: stored_journal,
                entry_count: 1,
                scope_snapshot: None,
                dirty_upper_bound: false,
                pending_confirm_reason: None,
                dirty_filter: false,
                filter_fingerprint: None,
            };
            seed_volume_cache(
                &volumes_dir,
                &record,
                vec![make_entry(
                    "stale_cached.txt",
                    &format!("{disk_str}/stale_cached.txt"),
                    false,
                )],
            );
            let mut manifest = VolumeManifest::default();
            manifest.upsert(record);
            manifest.save_atomic(&volumes_dir).unwrap();

            let mut config = AppConfig::default();
            config.index.index_paths = vec![disk_str.clone()];
            config.index.include_hidden = true;

            let mut mounts = base_mounts();
            mounts.push(MountedFsInfo {
                mount_point: disk_str.clone(),
                fstype: "apfs".to_string(),
                from_name: "/dev/disk23s1".to_string(),
                is_local: true,
                uuid: Some("EXT-JOURNAL-CHECK".to_string()),
                total_bytes: 500_000,
                volume_name: "ExtJournal".to_string(),
            });
            let mounts_state = Arc::new(Mutex::new(mounts));
            let mounts_read = Arc::clone(&mounts_state);
            let coord = VolumeCoordinator::with_mount_lister(
                cache_dir.path().to_path_buf(),
                Arc::new(move || mounts_read.lock().clone()),
            );

            coord.initialize(&config, Arc::new(|| {}));
            assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
            // 持久日志卷 UUID 缺失或不匹配：绝不静默全扫，转入 NeedsConfirmation(JournalInvalidated) 并保留旧缓存可搜
            assert_eq!(coord.scan_run_count(), 0);
            assert_eq!(coord.statuses()[0].state, VolumeState::NeedsConfirmation);
            assert_eq!(
                coord.statuses()[0].confirm_reason,
                Some(VolumeConfirmReason::JournalInvalidated)
            );
            let primary = IndexStore::new();
            let (stale_hits, _, _, _) = coord.search(
                &primary,
                "stale_cached.txt",
                SearchMode::Auto,
                CaseSensitivity::Auto,
                10,
                false,
            );
            assert_eq!(stale_hits.len(), 1);

            // 用户确认后才执行扫描并补齐离线新增文件
            assert_eq!(coord.confirm_all_pending_volume_scans(), 1);
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut found = false;
            while Instant::now() < deadline {
                let (hits, _, _, _) = coord.search(
                    &primary,
                    "offline_catchup.txt",
                    SearchMode::Auto,
                    CaseSensitivity::Auto,
                    10,
                    false,
                );
                if !hits.is_empty() && coord.statuses()[0].state == VolumeState::Online {
                    found = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            assert!(found, "confirming JournalInvalidated volume must rescan cleanly");
        }
    }

    #[test]
    fn will_unmount_grace_prevents_monitor_bounce_until_did_unmount_or_mount() {
        let cache_dir = TempDir::new().unwrap();
        let disk_dir = TempDir::new().unwrap();
        let disk_root = disk_dir.path().canonicalize().unwrap();
        let disk_str = disk_root.to_string_lossy().to_string();
        std::fs::write(disk_root.join("eject_test.txt"), "ok").unwrap();

        let mut config = AppConfig::default();
        config.index.index_paths = vec![disk_str.clone()];
        config.index.include_hidden = true;
        let config_store = ConfigStore::new(config.clone());

        let mut mounts = base_mounts();
        mounts.push(MountedFsInfo {
            mount_point: disk_str.clone(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk24s1".to_string(),
            is_local: true,
            uuid: Some("EXT-GRACE-UUID".to_string()),
            total_bytes: 500_000,
            volume_name: "ExtGrace".to_string(),
        });
        let mounts_state = Arc::new(Mutex::new(mounts));
        let mounts_read = Arc::clone(&mounts_state);
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
        );
        coord.initialize(&config, Arc::new(|| {}));
        assert_eq!(coord.confirm_all_pending_volume_scans(), 1);
        coord.start_monitor(config_store, Arc::new(|| {}));

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if coord.statuses()[0].state == VolumeState::Online {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(coord.statuses()[0].state, VolumeState::Online);

        // Simulate macOS lag: willUnmount arrives while mount list still contains disk_root
        coord.notify_volume_will_unmount(&disk_root);
        assert_eq!(coord.statuses()[0].state, VolumeState::Offline);

        // Wait past one 3-second monitor poll cycle while mount list still includes disk_root
        std::thread::sleep(Duration::from_millis(3400));
        assert_eq!(
            coord.statuses()[0].state,
            VolumeState::Offline,
            "monitor poll must not bounce volume back Online during unmount grace period"
        );

        // Remount notification explicitly clears the grace window
        coord.notify_volume_mounted(&disk_root);
        assert_eq!(coord.statuses()[0].state, VolumeState::Online);
    }

    #[test]
    fn dirty_filter_missing_subpath_does_not_spin_rescan_across_monitor_polls() {
        let cache_dir = TempDir::new().unwrap();
        let disk_dir = TempDir::new().unwrap();
        let disk_root = disk_dir.path().canonicalize().unwrap();
        let missing_sub = disk_root.join("DoesNotExistYet");
        let configured_str = missing_sub.to_string_lossy().to_string();

        let mut config = AppConfig::default();
        config.index.index_paths = vec![configured_str.clone()];
        config.index.include_hidden = true;
        let config_store = ConfigStore::new(config.clone());

        let mut mounts = base_mounts();
        mounts.push(MountedFsInfo {
            mount_point: disk_root.to_string_lossy().to_string(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk25s1".to_string(),
            is_local: true,
            uuid: Some("EXT-MISSING-DIRTY-UUID".to_string()),
            total_bytes: 500_000,
            volume_name: "ExtMissingDirty".to_string(),
        });
        let mounts_state = Arc::new(Mutex::new(mounts));
        let mounts_read = Arc::clone(&mounts_state);
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
        );
        coord.initialize(&config, Arc::new(|| {}));
        assert_eq!(coord.confirm_all_pending_volume_scans(), 1);

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if coord.statuses()[0].state == VolumeState::Missing {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(coord.statuses()[0].state, VolumeState::Missing);

        // Trigger filter narrowing while subpath is Missing: stays Missing and never spins.
        let mut updated = config.clone();
        updated.index.exclude.push("tmp_ignored".to_string());
        config_store.update(updated.clone());
        coord.on_config_updated(&updated);
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord.statuses()[0].state, VolumeState::Missing);

        let gen_after_settle = coord.slot_generation(&configured_str).unwrap();
        coord.start_monitor(config_store, Arc::new(|| {}));
        std::thread::sleep(Duration::from_millis(3500));

        assert_eq!(coord.statuses()[0].state, VolumeState::Missing);
        assert_eq!(
            coord.slot_generation(&configured_str),
            Some(gen_after_settle),
            "monitor poll must not repeatedly re-trigger Syncing on Missing"
        );
    }

    #[test]
    fn worker_queue_enqueue_enforces_generation_and_cancellation_monotonicity() {
        let queue = WorkerQueue::new();
        let active_cancel_2 = Arc::new(AtomicBool::new(false));
        let active_cancel_1 = Arc::new(AtomicBool::new(false));
        let cancelled_3 = Arc::new(AtomicBool::new(true));
        let snap = VolumeScopeSnapshot::from_config(
            &AppConfig::default(),
            Path::new("/Volumes/A"),
            Path::new("/Volumes/A"),
        );

        queue.enqueue(WorkerTask::Scan(ScanJob {
            volume_id: "vid-a".to_string(),
            generation: 2,
            kind: VolumeKind::LocalExternal,
            fstype: "apfs".to_string(),
            mount_root: PathBuf::from("/Volumes/A"),
            subpath: String::new(),
            root: PathBuf::from("/Volumes/A"),
            target_snapshot: snap.clone(),
            use_serial_scan: false,
            scanned_count: Arc::new(AtomicU64::new(0)),
            cancel: Arc::clone(&active_cancel_2),
        }));

        // Stale lower-generation task must not evict generation 2
        queue.enqueue(WorkerTask::AttachStream(AttachStreamJob {
            volume_id: "vid-a".to_string(),
            generation: 1,
            mount_root: PathBuf::from("/Volumes/A"),
            subpath: String::new(),
            fstype: "apfs".to_string(),
            active_path: PathBuf::from("/Volumes/A"),
            since_event_id: Some(1),
            stored_journal_uuid: None,
            require_journal_verify: false,
            target_snapshot: snap.clone(),
            cancel: active_cancel_1,
            store: IndexStore::new(),
        }));

        // Cancelled task (even with higher generation) must not enqueue or evict
        queue.enqueue(WorkerTask::AttachStream(AttachStreamJob {
            volume_id: "vid-a".to_string(),
            generation: 3,
            mount_root: PathBuf::from("/Volumes/A"),
            subpath: String::new(),
            fstype: "apfs".to_string(),
            active_path: PathBuf::from("/Volumes/A"),
            since_event_id: Some(1),
            stored_journal_uuid: None,
            require_journal_verify: false,
            target_snapshot: snap.clone(),
            cancel: cancelled_3,
            store: IndexStore::new(),
        }));

        // Unrelated volume coexists independently
        queue.enqueue(WorkerTask::Scan(ScanJob {
            volume_id: "vid-b".to_string(),
            generation: 1,
            kind: VolumeKind::Network,
            fstype: "smbfs".to_string(),
            mount_root: PathBuf::from("/Volumes/B"),
            subpath: String::new(),
            root: PathBuf::from("/Volumes/B"),
            target_snapshot: snap.clone(),
            use_serial_scan: true,
            scanned_count: Arc::new(AtomicU64::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
        }));

        let (first, running_a) = queue.pop_blocking().unwrap();
        assert_eq!(first.volume_id(), "vid-a");
        assert!(matches!(first, WorkerTask::Scan(ref j) if j.generation == 2));
        drop(running_a);

        let (second, _running_b) = queue.pop_blocking().unwrap();
        assert_eq!(second.volume_id(), "vid-b");

        // Newer non-cancelled task for same volume replaces older generation
        queue.enqueue(WorkerTask::Scan(ScanJob {
            volume_id: "vid-a".to_string(),
            generation: 2,
            kind: VolumeKind::LocalExternal,
            fstype: "apfs".to_string(),
            mount_root: PathBuf::from("/Volumes/A"),
            subpath: String::new(),
            root: PathBuf::from("/Volumes/A"),
            target_snapshot: snap.clone(),
            use_serial_scan: false,
            scanned_count: Arc::new(AtomicU64::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
        }));
        queue.enqueue(WorkerTask::Scan(ScanJob {
            volume_id: "vid-a".to_string(),
            generation: 4,
            kind: VolumeKind::LocalExternal,
            fstype: "apfs".to_string(),
            mount_root: PathBuf::from("/Volumes/A"),
            subpath: String::new(),
            root: PathBuf::from("/Volumes/A"),
            target_snapshot: snap,
            use_serial_scan: false,
            scanned_count: Arc::new(AtomicU64::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
        }));
        let newest = queue.pop_blocking().unwrap().0;
        assert!(matches!(newest, WorkerTask::Scan(ref j) if j.generation == 4));
    }

    #[test]
    fn initialize_same_path_swap_counts_only_active_volume_preloaded_entries() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let disk_dir = TempDir::new().unwrap();
        let disk_root = disk_dir.path().canonicalize().unwrap();
        let disk_str = disk_root.to_string_lossy().to_string();

        // Seed cached manifest + 2 entries for old Disk A at disk_str
        let id_a = VolumeIdentity::LocalUuid {
            uuid: "COUNT-DISK-A".to_string(),
            subpath: String::new(),
        };
        let rec_a = VolumeRecord {
            volume_id: id_a.volume_id(),
            configured_path: disk_str.clone(),
            active_configured_path: disk_str.clone(),
            active_mount_root: Some(disk_str.clone()),
            subpath: String::new(),
            display_name: "DiskA".to_string(),
            kind: VolumeKind::LocalExternal,
            fstype: "apfs".to_string(),
            identity: id_a,
            last_sync_at: Some(1_700_000_000),
            last_event_id: None,
            journal_uuid: None,
            entry_count: 2,
            scope_snapshot: None,
            dirty_upper_bound: false,
            pending_confirm_reason: None,
            dirty_filter: false,
            filter_fingerprint: None,
        };
        seed_volume_cache(
            &volumes_dir,
            &rec_a,
            vec![
                make_entry("a1.txt", &format!("{disk_str}/a1.txt"), false),
                make_entry("a2.txt", &format!("{disk_str}/a2.txt"), false),
            ],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(rec_a);
        manifest.save_atomic(&volumes_dir).unwrap();

        let mut config = AppConfig::default();
        config.index.index_paths = vec![disk_str.clone()];
        config.index.include_hidden = true;

        // Live mount at disk_str is uncached Disk B
        let mut mounts = base_mounts();
        mounts.push(MountedFsInfo {
            mount_point: disk_str.clone(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk30s1".to_string(),
            is_local: true,
            uuid: Some("COUNT-DISK-B".to_string()),
            total_bytes: 500_000,
            volume_name: "DiskB".to_string(),
        });
        let mounts_state = Arc::new(Mutex::new(mounts));
        let mounts_read = Arc::clone(&mounts_state);
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
        );

        let loaded = coord.initialize(&config, Arc::new(|| {}));
        assert_eq!(
            loaded, 0,
            "discarded manifest cache from old Disk A must not be counted in initialize return value"
        );
    }

    #[test]
    fn reconcile_updates_display_name_and_fstype_without_swap() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");

        let record = make_network_record(
            "/Volumes/Share",
            "/Volumes/Share",
            Some("/Volumes/Share"),
            "",
            "OldLabel",
            "//nas.local/share",
            1,
            false,
        );
        seed_volume_cache(
            &volumes_dir,
            &record,
            vec![make_entry("doc.txt", "/Volumes/Share/doc.txt", false)],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(record);
        manifest.save_atomic(&volumes_dir).unwrap();

        let mut config = AppConfig::default();
        config.index.index_paths = vec!["/Volumes/Share".to_string()];

        let mut mounts = base_mounts();
        mounts.push(MountedFsInfo {
            mount_point: "/Volumes/Share".to_string(),
            fstype: "smbfs".to_string(),
            from_name: "//nas.local/share".to_string(),
            is_local: false,
            uuid: None,
            total_bytes: 0,
            volume_name: "RenamedShare".to_string(),
        });
        let mounts_state = Arc::new(Mutex::new(mounts));
        let mounts_read = Arc::clone(&mounts_state);
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
        );

        coord.initialize(&config, Arc::new(|| {}));
        assert_eq!(coord.statuses()[0].display_name, "RenamedShare");
    }

    #[test]
    fn finalize_scan_commit_unlinks_orphaned_cache_and_flushes_on_interleaved_will_unmount() {
        // Case A: Phase 2 writes .cache concurrently as clear_volume_cache removes slot & manifest ->
        // Phase 3 detects missing slot & manifest and unlinks orphaned .cache.
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let vid = VolumeIdentity::Network {
            fstype: "smbfs".to_string(),
            normalized_source: "//nas/orphan".to_string(),
            subpath: String::new(),
        }
        .volume_id();
        let cache_path = volumes_dir.join(format!("{vid}.cache"));
        let store = IndexStore::new();
        store.replace_compact(
            CompactIndex::from_entries(vec![make_entry(
                "orphan.txt",
                "/Volumes/Orphan/orphan.txt",
                false,
            )])
            .unwrap(),
        );

        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(base_mounts),
        );
        {
            let mut inner = coord.inner.write();
            inner.slots.push(VolumeSlot {
                volume_id: vid.clone(),
                configured_path: "/Volumes/Orphan".to_string(),
                active_configured_path: "/Volumes/Orphan".to_string(),
                active_mount_root: Some("/Volumes/Orphan".to_string()),
                subpath: String::new(),
                display_name: "Orphan".to_string(),
                kind: VolumeKind::Network,
                fstype: "smbfs".to_string(),
                from_name: "//nas/orphan".to_string(),
                identity: VolumeIdentity::Network {
                    fstype: "smbfs".to_string(),
                    normalized_source: "//nas/orphan".to_string(),
                    subpath: String::new(),
                },
                state: VolumeState::Syncing,
                entry_count: 1,
                last_sync_at: Some(1_700_000_000),
                last_event_id: None,
                journal_uuid: None,
                scope_snapshot: None,
                dirty_upper_bound: false,
                pending_confirm_reason: None,
                in_flight_scan_snapshot: None,
                in_flight_prune_snapshot: None,
                stream_snapshot: None,
                scanned_count: Arc::new(AtomicU64::new(0)),
                is_remounted: false,
                has_cache: true,
                dirty_cache: true,
                cache_seq: 1,
                saved_cache_seq: 0,
                cache_persistence: CachePersistence::default(),
                generation: 1,
                cancel_token: Arc::new(AtomicBool::new(false)),
                store: store.clone(),
                stream_handle: None,
                last_rescan_triggered: None,
                last_scan_duration: None,
                next_reconnect_retry: None,
                unmounting_until: None,
                skip_missing_rescan: false,
            });
        }

        // Remove slot via clear_volume_cache, then simulate Phase 2 finishing disk write late
        assert_eq!(coord.clear_volume_cache(&vid), Ok(true));
        cache::save_compact(&store.read(), None, &cache_path).unwrap();
        assert!(cache_path.exists());

        let outcome = commit_scan_phase3(
            &coord.inner,
            &coord.manifest_save_lock,
            &volumes_dir,
            &vid,
            1,
            &cache_path,
        );
        assert!(matches!(outcome, ScanFinalizeResult::Aborted));
        assert!(
            !cache_path.exists(),
            "orphaned .cache written during clear_volume_cache race must be unlinked"
        );
        assert!(VolumeManifest::load_from_dir(&volumes_dir).records.is_empty());

        // Case B: willUnmount interleaves right after Phase 1 while dirty_cache == true ->
        // willUnmount flushes the newly scanned store to .cache and saves manifest;
        // Phase 2/3 aborts without flipping state back to Online.
        let cache_dir2 = TempDir::new().unwrap();
        let volumes_dir2 = cache_dir2.path().join("volumes");
        let vid2 = VolumeIdentity::Network {
            fstype: "smbfs".to_string(),
            normalized_source: "//nas/race".to_string(),
            subpath: String::new(),
        }
        .volume_id();
        let cache_path2 = volumes_dir2.join(format!("{vid2}.cache"));
        let store2 = IndexStore::new();
        store2.replace_compact(
            CompactIndex::from_entries(vec![make_entry(
                "fresh_before_unmount.txt",
                "/Volumes/Race/fresh_before_unmount.txt",
                false,
            )])
            .unwrap(),
        );
        let coord2 = VolumeCoordinator::with_mount_lister(
            cache_dir2.path().to_path_buf(),
            Arc::new(base_mounts),
        );
        let cache_save2 = {
            let mut inner = coord2.inner.write();
            inner.slots.push(VolumeSlot {
                volume_id: vid2.clone(),
                configured_path: "/Volumes/Race".to_string(),
                active_configured_path: "/Volumes/Race".to_string(),
                active_mount_root: Some("/Volumes/Race".to_string()),
                subpath: String::new(),
                display_name: "Race".to_string(),
                kind: VolumeKind::Network,
                fstype: "smbfs".to_string(),
                from_name: "//nas/race".to_string(),
                identity: VolumeIdentity::Network {
                    fstype: "smbfs".to_string(),
                    normalized_source: "//nas/race".to_string(),
                    subpath: String::new(),
                },
                state: VolumeState::Syncing,
                entry_count: 1,
                last_sync_at: Some(1_700_000_000),
                last_event_id: Some(77),
                journal_uuid: None,
                scope_snapshot: None,
                dirty_upper_bound: false,
                pending_confirm_reason: None,
                in_flight_scan_snapshot: None,
                in_flight_prune_snapshot: None,
                stream_snapshot: None,
                scanned_count: Arc::new(AtomicU64::new(0)),
                is_remounted: false,
                has_cache: true,
                dirty_cache: true,
                cache_seq: 1,
                saved_cache_seq: 0,
                cache_persistence: CachePersistence::default(),
                generation: 1,
                cancel_token: Arc::new(AtomicBool::new(false)),
                store: store2.clone(),
                stream_handle: None,
                last_rescan_triggered: None,
                last_scan_duration: None,
                next_reconnect_retry: None,
                unmounting_until: None,
                skip_missing_rescan: false,
            });
            inner.slots[0].cache_save(&volumes_dir2)
        };

        coord2.notify_volume_will_unmount(Path::new("/Volumes/Race"));
        assert_eq!(coord2.statuses()[0].state, VolumeState::Offline);

        let finalize_res = finalize_scan_commit(
            &coord2.inner,
            &coord2.manifest_save_lock,
            &volumes_dir2,
            &vid2,
            1,
            cache_save2,
        );
        assert!(matches!(finalize_res, ScanFinalizeResult::Aborted));
        assert_eq!(coord2.statuses()[0].state, VolumeState::Offline);
        assert!(cache_path2.exists(), "willUnmount must flush phase-1 store to .cache");
        let (reloaded, cp) = cache::load_compact(&cache_path2).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(cp.map(|c| c.event_id), Some(77));
    }

    fn attach_task(volume_id: &str, generation: u64) -> WorkerTask {
        WorkerTask::AttachStream(AttachStreamJob {
            volume_id: volume_id.to_string(),
            generation,
            mount_root: PathBuf::from("/tmp"),
            subpath: String::new(),
            fstype: "apfs".to_string(),
            active_path: PathBuf::from("/tmp"),
            since_event_id: None,
            stored_journal_uuid: None,
            require_journal_verify: false,
            target_snapshot: VolumeScopeSnapshot::from_config(
                &AppConfig::default(),
                Path::new("/tmp"),
                Path::new("/tmp"),
            ),
            cancel: Arc::new(AtomicBool::new(false)),
            store: IndexStore::new(),
        })
    }

    #[test]
    fn worker_queue_runs_volumes_in_parallel_but_serializes_the_same_volume() {
        let queue = WorkerQueue::new();
        queue.enqueue(attach_task("a", 1));
        let (first, running_a) = queue.pop_blocking().unwrap();
        assert_eq!(first.volume_id(), "a");

        // 同一卷正在执行时，后续任务必须排队；其他卷可以立即取走
        queue.enqueue(attach_task("a", 2));
        queue.enqueue(attach_task("b", 1));
        let (second, _running_b) = queue.pop_blocking().unwrap();
        assert_eq!(second.volume_id(), "b");

        drop(running_a);
        let (next, _running_a2) = queue.pop_blocking().unwrap();
        assert_eq!(next.volume_id(), "a");
        assert_eq!(next.generation(), 2);
    }

    #[test]
    fn monitor_refresh_preserves_syncing_state_for_cached_volume_rescan() {
        let cache_dir = tempfile::tempdir().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let mount = MountedFsInfo {
            mount_point: "/Volumes/NasSync".to_string(),
            fstype: "smbfs".to_string(),
            from_name: "//nas.local/NasSync".to_string(),
            is_local: false,
            uuid: None,
            total_bytes: 0,
            volume_name: "NasSync".to_string(),
        };
        let mounts = vec![mount.clone()];
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts.clone()),
        );
        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec!["/Volumes/NasSync".to_string()];

        let vid = VolumeIdentity::Network {
            fstype: "smbfs".to_string(),
            normalized_source: "//nas.local/NasSync".to_string(),
            subpath: String::new(),
        }
        .volume_id();

        let store = IndexStore::new();
        store.replace_compact(
            CompactIndex::from_entries(vec![make_entry(
                "existing.txt",
                "/Volumes/NasSync/existing.txt",
                false,
            )])
            .unwrap(),
        );

        {
            let mut inner = coord.inner.write();
            inner.config = cfg.clone();
            let snap = VolumeScopeSnapshot::from_config(
                &cfg,
                Path::new("/Volumes/NasSync"),
                Path::new("/Volumes/NasSync"),
            );
            inner.slots.push(VolumeSlot {
                volume_id: vid,
                configured_path: "/Volumes/NasSync".to_string(),
                active_configured_path: "/Volumes/NasSync".to_string(),
                active_mount_root: Some("/Volumes/NasSync".to_string()),
                subpath: String::new(),
                display_name: "NasSync".to_string(),
                kind: VolumeKind::Network,
                fstype: "smbfs".to_string(),
                from_name: "//nas.local/NasSync".to_string(),
                identity: VolumeIdentity::Network {
                    fstype: "smbfs".to_string(),
                    normalized_source: "//nas.local/NasSync".to_string(),
                    subpath: String::new(),
                },
                state: VolumeState::Syncing,
                entry_count: 1,
                last_sync_at: Some(1_700_000_000),
                last_event_id: None,
                journal_uuid: None,
                scope_snapshot: Some(snap.clone()),
                dirty_upper_bound: false,
                pending_confirm_reason: None,
                in_flight_scan_snapshot: Some(snap),
                in_flight_prune_snapshot: None,
                stream_snapshot: None,
                scanned_count: Arc::new(AtomicU64::new(0)),
                is_remounted: false,
                has_cache: true,
                dirty_cache: false,
                cache_seq: 0,
                saved_cache_seq: 0,
                cache_persistence: CachePersistence::default(),
                generation: 2,
                cancel_token: Arc::new(AtomicBool::new(false)),
                store,
                stream_handle: None,
                last_rescan_triggered: Some(Instant::now()),
                last_scan_duration: None,
                next_reconnect_retry: None,
                unmounting_until: None,
                skip_missing_rescan: false,
            });
        }

        let effects = refresh_mounts_locked(
            &coord.inner,
            std::slice::from_ref(&mount),
            &volumes_dir,
            false,
        );
        assert!(effects.tasks.is_empty(), "in-flight rescan must not be duplicated");
        assert!(effects.notify.is_none(), "unchanged Syncing state should not emit spurious status notifications");
        assert_eq!(coord.statuses()[0].state, VolumeState::Syncing);
    }

    #[test]
    fn network_periodic_rescan_applies_adaptive_cooldown_for_slow_scans() {
        let now_unix = current_unix_secs();
        let mut slot = VolumeSlot {
            volume_id: "slow-nas".to_string(),
            configured_path: "/Volumes/SlowNas".to_string(),
            active_configured_path: "/Volumes/SlowNas".to_string(),
            active_mount_root: Some("/Volumes/SlowNas".to_string()),
            subpath: String::new(),
            display_name: "SlowNas".to_string(),
            kind: VolumeKind::Network,
            fstype: "smbfs".to_string(),
            from_name: "//nas.local/SlowNas".to_string(),
            identity: VolumeIdentity::Network {
                fstype: "smbfs".to_string(),
                normalized_source: "//nas.local/SlowNas".to_string(),
                subpath: String::new(),
            },
            state: VolumeState::Online,
            entry_count: 100,
            last_sync_at: Some(now_unix.saturating_sub(120)),
            last_event_id: None,
            journal_uuid: None,
            scope_snapshot: None,
            dirty_upper_bound: false,
            pending_confirm_reason: None,
            in_flight_scan_snapshot: None,
            in_flight_prune_snapshot: None,
            stream_snapshot: None,
            scanned_count: Arc::new(AtomicU64::new(0)),
            is_remounted: false,
            has_cache: true,
            dirty_cache: false,
            cache_seq: 0,
            saved_cache_seq: 0,
            cache_persistence: CachePersistence::default(),
            generation: 1,
            cancel_token: Arc::new(AtomicBool::new(false)),
            store: IndexStore::new(),
            stream_handle: None,
            last_rescan_triggered: None,
            // 上次扫描耗了 60 秒，自适应冷却至少 300 秒，即使配置了 60 秒基础间隔也不应立刻重扫
            last_scan_duration: Some(Duration::from_secs(60)),
            next_reconnect_retry: None,
            unmounting_until: None,
            skip_missing_rescan: false,
        };

        assert!(!network_slot_rescan_due(&slot, Duration::from_secs(60), now_unix));

        // 超过 300 秒冷却期后应触发周期重扫
        slot.last_sync_at = Some(now_unix.saturating_sub(305));
        assert!(network_slot_rescan_due(&slot, Duration::from_secs(60), now_unix));

        // 存在待确认原因或处于 NeedsConfirmation 时禁止周期重扫越权覆盖
        slot.pending_confirm_reason = Some(VolumeConfirmReason::FilterWidened);
        assert!(!network_slot_rescan_due(&slot, Duration::from_secs(60), now_unix));
        slot.state = VolumeState::NeedsConfirmation;
        assert!(!network_slot_rescan_due(&slot, Duration::from_secs(60), now_unix));

        // 开关关闭或间隔为 0 时一律不调度周期重扫；超大分钟数使用饱和乘法避免溢出
        assert_eq!(effective_network_rescan_interval(false, 1440), None);
        assert_eq!(effective_network_rescan_interval(true, 0), None);
        assert_eq!(
            effective_network_rescan_interval(true, 1440),
            Some(Duration::from_secs(24 * 3600))
        );
        assert_eq!(
            effective_network_rescan_interval(true, u64::MAX),
            Some(Duration::from_secs(u64::MAX))
        );
    }

    #[test]
    fn on_config_updated_skips_redundant_reconcile_and_warms_loaded_shards() {
        let cache_dir = tempfile::tempdir().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let rec = make_network_record(
            "/Volumes/Archive",
            "/Volumes/Archive",
            Some("/Volumes/Archive"),
            "",
            "Archive",
            "//nas.local/archive",
            1,
            false,
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(rec.clone());
        manifest.save_atomic(&volumes_dir).unwrap();
        seed_volume_cache(
            &volumes_dir,
            &rec,
            vec![make_entry(
                "quarterly_report.pdf",
                "/Volumes/Archive/quarterly_report.pdf",
                false,
            )],
        );

        let mut config = AppConfig::default();
        config.index.index_paths = vec!["/Volumes/Archive".to_string()];
        config.index.exclude_paths = vec![];

        let mount_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mount_calls_ref = Arc::clone(&mount_calls);
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || {
                mount_calls_ref.fetch_add(1, Ordering::SeqCst);
                base_mounts()
            }),
        );
        coord.initialize(&config, Arc::new(|| {}));
        assert_eq!(mount_calls.load(Ordering::SeqCst), 1);

        // 传入与当前 index 完全一致的配置应直接短路，不触发重复挂载表探测
        coord.on_config_updated(&config);
        assert_eq!(
            mount_calls.load(Ordering::SeqCst),
            1,
            "unchanged IndexConfig must short-circuit without calling mount_lister"
        );

        // 真正修改 index 配置时应触发一次对账，再次重复传入则继续短路
        let mut next = config.clone();
        next.index.exclude.push("cache_tmp".to_string());
        coord.on_config_updated(&next);
        assert_eq!(mount_calls.load(Ordering::SeqCst), 2);
        coord.on_config_updated(&next);
        assert_eq!(mount_calls.load(Ordering::SeqCst), 2);

        // 后台预热应覆盖已加载的离线卷分片并保证立即可搜
        let stores = coord.loaded_stores();
        assert_eq!(stores.len(), 1);
        coord.warm_query_indexes();
        let primary = IndexStore::new();
        let (hits, _, err, _) = coord.search(
            &primary,
            "quarterly_report",
            SearchMode::Substring,
            CaseSensitivity::Auto,
            10,
            true,
        );
        assert!(err.is_none());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].result.entry.name, "quarterly_report.pdf");
    }

    #[test]
    fn local_fallback_matches_replugged_device_and_drifted_mount_without_collision() {
        let identity = VolumeIdentity::LocalFallback {
            fstype: "msdos".into(),
            source: "/dev/disk4s1".into(),
            total_bytes: 32_000_000_000,
            volume_name: "MyUSB".into(),
            subpath: String::new(),
            exact_name_key: false,
        };
        let drifted_mount = MountedFsInfo {
            mount_point: "/Volumes/MyUSB-1".into(),
            fstype: "msdos".into(),
            from_name: "/dev/disk5s1".into(),
            is_local: true,
            uuid: None,
            total_bytes: 32_000_000_000,
            volume_name: "MyUSB-1".into(),
        };
        let exact_other = MountedFsInfo {
            mount_point: "/Volumes/Photos-2025".into(),
            fstype: "msdos".into(),
            from_name: "/dev/disk6s1".into(),
            is_local: true,
            uuid: None,
            total_bytes: 32_000_000_000,
            volume_name: "Photos-2025".into(),
        };
        assert_eq!(
            match_mount_for_identity(&identity, &[exact_other.clone(), drifted_mount.clone()])
                .map(|m| m.mount_point.as_str()),
            Some("/Volumes/MyUSB-1")
        );

        let photos_id = VolumeIdentity::LocalFallback {
            fstype: "msdos".into(),
            source: "/dev/disk6s1".into(),
            total_bytes: 32_000_000_000,
            volume_name: "Photos".into(),
            subpath: String::new(),
            exact_name_key: false,
        };
        assert!(match_mount_for_identity(&photos_id, &[exact_other]).is_none());

        // 两张同容量同卷名无 UUID 盘在线时，精确卷名与原设备节点优先
        let same_name_other_dev = MountedFsInfo {
            mount_point: "/Volumes/MyUSB".into(),
            fstype: "msdos".into(),
            from_name: "/dev/disk7s1".into(),
            is_local: true,
            uuid: None,
            total_bytes: 32_000_000_000,
            volume_name: "MyUSB".into(),
        };
        let same_name_orig_dev = MountedFsInfo {
            mount_point: "/Volumes/MyUSB-orig".into(),
            fstype: "msdos".into(),
            from_name: "/dev/disk4s1".into(),
            is_local: true,
            uuid: None,
            total_bytes: 32_000_000_000,
            volume_name: "MyUSB".into(),
        };
        assert_eq!(
            match_mount_for_identity(&identity, &[same_name_other_dev, same_name_orig_dev, drifted_mount])
                .map(|m| m.from_name.as_str()),
            Some("/dev/disk4s1")
        );

        // 带有效 UUID 的同规格同名卷绝不能被无 UUID 的 LocalFallback 身份误匹配劫持
        let uuid_bearing_mount = MountedFsInfo {
            mount_point: "/Volumes/MyUSB".into(),
            fstype: "msdos".into(),
            from_name: "/dev/disk8s1".into(),
            is_local: true,
            uuid: Some("REAL-EXFAT-UUID-1234".into()),
            total_bytes: 32_000_000_000,
            volume_name: "MyUSB".into(),
        };
        assert!(
            match_mount_for_identity(&identity, &[uuid_bearing_mount]).is_none(),
            "LocalFallback must never hijack a local mount that carries a valid UUID"
        );
    }

    #[test]
    fn twin_local_fallback_drives_do_not_collide_or_steal_slots() {
        let cache_dir = TempDir::new().unwrap();
        let mounts_parent = TempDir::new().unwrap();
        let parent_canon = mounts_parent.path().canonicalize().unwrap();
        let drive1_root = parent_canon.join("Backup-1");
        let drive2_root = parent_canon.join("Backup-2");
        std::fs::create_dir_all(&drive1_root).unwrap();
        std::fs::create_dir_all(&drive2_root).unwrap();
        std::fs::write(drive1_root.join("alpha.txt"), "one").unwrap();
        std::fs::write(drive2_root.join("beta.txt"), "two").unwrap();

        let drive1_str = drive1_root.to_string_lossy().to_string();
        let drive2_str = drive2_root.to_string_lossy().to_string();

        let mut config = AppConfig::default();
        config.index.index_paths = vec![drive1_str.clone(), drive2_str.clone()];
        config.index.include_hidden = true;

        let mut twin_mounts = base_mounts();
        twin_mounts.push(MountedFsInfo {
            mount_point: drive1_str.clone(),
            fstype: "exfat".into(),
            from_name: "/dev/disk4s1".into(),
            is_local: true,
            uuid: None,
            total_bytes: 64_000_000_000,
            volume_name: "Backup-1".into(),
        });
        twin_mounts.push(MountedFsInfo {
            mount_point: drive2_str.clone(),
            fstype: "exfat".into(),
            from_name: "/dev/disk5s1".into(),
            is_local: true,
            uuid: None,
            total_bytes: 64_000_000_000,
            volume_name: "Backup-2".into(),
        });
        let mounts = Arc::new(Mutex::new(twin_mounts));
        let mounts_read = Arc::clone(&mounts);
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
        );

        coord.initialize(&config, Arc::new(|| {}));
        let statuses = coord.statuses();
        assert_eq!(statuses.len(), 2, "两张同容量编号无 UUID 外置盘必须各自保留独立槽位");
        assert_ne!(
            statuses[0].volume_id, statuses[1].volume_id,
            "两张并发配置的编号无 UUID 外置盘必须拥有互不冲突的 volume_id"
        );
        let active_paths: HashSet<String> = statuses
            .iter()
            .filter_map(|s| s.active_path.clone())
            .collect();
        assert!(active_paths.contains(&drive1_str));
        assert!(active_paths.contains(&drive2_str));
    }

    #[test]
    fn local_fallback_replug_drift_rebases_cached_slot_without_rescan() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let mounts_parent = TempDir::new().unwrap();
        let parent_canon = mounts_parent.path().canonicalize().unwrap();
        let orig_root = parent_canon.join("MyUSB");
        let drifted_root = parent_canon.join("MyUSB-1");
        std::fs::create_dir_all(&drifted_root).unwrap();

        let orig_str = orig_root.to_string_lossy().to_string();
        let drifted_str = drifted_root.to_string_lossy().to_string();
        let live_journal = fsevents::journal_uuid_for_path(&drifted_root);
        let live_event_id = fsevents::current_event_id();

        let identity = VolumeIdentity::LocalFallback {
            fstype: "exfat".into(),
            source: "/dev/disk4s1".into(),
            total_bytes: 64_000_000_000,
            volume_name: "MyUSB".into(),
            subpath: String::new(),
            exact_name_key: false,
        };
        let record = VolumeRecord {
            volume_id: identity.volume_id(),
            configured_path: orig_str.clone(),
            active_configured_path: orig_str.clone(),
            active_mount_root: Some(orig_str.clone()),
            subpath: String::new(),
            display_name: "MyUSB".into(),
            kind: VolumeKind::LocalExternal,
            fstype: "exfat".into(),
            identity,
            last_sync_at: Some(1_700_000_000),
            last_event_id: Some(live_event_id),
            journal_uuid: live_journal,
            entry_count: 2,
            scope_snapshot: None,
            dirty_upper_bound: false,
            pending_confirm_reason: None,
            dirty_filter: false,
            filter_fingerprint: None,
        };
        seed_volume_cache(
            &volumes_dir,
            &record,
            vec![
                make_entry("MyUSB", &orig_str, true),
                make_entry("invoice.pdf", &format!("{orig_str}/invoice.pdf"), false),
            ],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(record.clone());
        manifest.save_atomic(&volumes_dir).unwrap();

        let mut config = AppConfig::default();
        config.index.index_paths = vec![orig_str.clone()];

        let mut replugged_mounts = base_mounts();
        replugged_mounts.push(MountedFsInfo {
            mount_point: drifted_str.clone(),
            fstype: "exfat".into(),
            from_name: "/dev/disk5s1".into(),
            is_local: true,
            uuid: None,
            total_bytes: 64_000_000_000,
            volume_name: "MyUSB-1".into(),
        });
        let mounts = Arc::new(Mutex::new(replugged_mounts));
        let mounts_read = Arc::clone(&mounts);
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
        );

        let loaded = coord.initialize(&config, Arc::new(|| {}));
        assert_eq!(loaded, 2);

        let statuses = coord.statuses();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].state, VolumeState::Online);
        assert!(statuses[0].is_remounted);
        assert_eq!(statuses[0].active_path.as_deref(), Some(drifted_str.as_str()));

        let primary = IndexStore::new();
        let (hits, _, err, _) = coord.search(
            &primary,
            "invoice.pdf",
            SearchMode::Auto,
            CaseSensitivity::Auto,
            20,
            true,
        );
        assert!(err.is_none());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].result.entry.path, format!("{drifted_str}/invoice.pdf"));
        assert!(!hits[0].is_offline);
    }

    #[test]
    fn offline_config_and_fda_fingerprint_change_prunes_and_rescans_across_restart() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let disk_dir = TempDir::new().unwrap();
        let disk_root = disk_dir.path().canonicalize().unwrap();
        std::fs::write(disk_root.join("keep.txt"), "ok").unwrap();
        std::fs::create_dir_all(disk_root.join("secret")).unwrap();
        std::fs::write(disk_root.join("secret/hidden.txt"), "no").unwrap();

        let disk_str = disk_root.to_string_lossy().to_string();
        let mounted = vec![MountedFsInfo {
            mount_point: disk_str.clone(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk41s1".to_string(),
            is_local: true,
            uuid: Some("EXT-FP-UUID".to_string()),
            total_bytes: 500_000,
            volume_name: "ExtFp".to_string(),
        }];

        let mut config = AppConfig::default();
        config.index.index_paths = vec![disk_str.clone()];
        config.index.include_hidden = true;

        // 1. Initial launch with FDA=false while mounted: confirm scan completes and stores scope_snapshot.
        {
            let mut m = base_mounts();
            m.extend(mounted.clone());
            let coord = VolumeCoordinator::with_mount_and_fda_lister(
                cache_dir.path().to_path_buf(),
                Arc::new(move || m.clone()),
                Arc::new(|| false),
            );
            coord.initialize(&config, Arc::new(|| {}));
            assert_eq!(coord.confirm_all_pending_volume_scans(), 1);
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline && coord.statuses()[0].state != VolumeState::Online {
                std::thread::sleep(Duration::from_millis(25));
            }
            assert_eq!(coord.statuses()[0].state, VolumeState::Online);
            coord.persist_all();
        }

        let manifest_after_scan = VolumeManifest::load_from_dir(&volumes_dir);
        assert_eq!(manifest_after_scan.records.len(), 1);
        assert!(manifest_after_scan.records[0].scope_snapshot.is_some());
        assert_eq!(manifest_after_scan.records[0].pending_confirm_reason, None);

        // 2. While quit and unplugged, user edits config.toml to exclude `secret/` (narrowing -> PruneOnly)
        // and flips FDA (completely isolated from external volumes).
        let mut offline_edited_config = config.clone();
        offline_edited_config
            .index
            .exclude_paths
            .push(format!("{disk_str}/secret"));
        let primary = IndexStore::new();
        let mounts_state = Arc::new(Mutex::new(base_mounts()));
        let mounts_read = Arc::clone(&mounts_state);
        {
            let coord = VolumeCoordinator::with_mount_and_fda_lister(
                cache_dir.path().to_path_buf(),
                Arc::new(move || mounts_read.lock().clone()),
                Arc::new(|| true),
            );
            coord.initialize(&offline_edited_config, Arc::new(|| {}));
            assert!(
                coord.wait_for_workers_idle(Duration::from_secs(5)),
                "offline prune worker must finish in time"
            );
            assert_eq!(coord.statuses()[0].state, VolumeState::Offline);

            // Immediately upon offline relaunch, excluded entries must already be pruned.
            let (secret_hits, _, _, _) = coord.search(
                &primary,
                "hidden.txt",
                SearchMode::Auto,
                CaseSensitivity::Auto,
                10,
                true,
            );
            assert!(
                secret_hits.is_empty(),
                "excluded secret/hidden.txt must be pruned immediately on offline relaunch"
            );
            let (keep_hits, _, _, _) = coord.search(
                &primary,
                "keep.txt",
                SearchMode::Auto,
                CaseSensitivity::Auto,
                10,
                true,
            );
            assert_eq!(keep_hits.len(), 1);
            let m_after_prune = VolumeManifest::load_from_dir(&volumes_dir);
            assert_eq!(m_after_prune.records[0].pending_confirm_reason, None);
            assert!(m_after_prune.records[0]
                .scope_snapshot
                .as_ref()
                .unwrap()
                .exclude_subpaths
                .contains(&"secret".to_string()));

            // Replug after pure narrowing + FDA flip -> directly Online with ZERO full rescan!
            let mut replugged = base_mounts();
            replugged.extend(mounted);
            *mounts_state.lock() = replugged;
            coord.notify_volume_mounted(&disk_root);
            assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
            assert_eq!(coord.statuses()[0].state, VolumeState::Online);
            assert_eq!(coord.scan_run_count(), 0);
        }
    }

    #[test]
    fn runtime_exclude_evicts_offline_volume_cache_immediately() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let mounts_parent = TempDir::new().unwrap();
        let parent_canon = mounts_parent.path().canonicalize().unwrap();
        let orig_root = parent_canon.join("Drive");
        let drifted_root = parent_canon.join("Drive-1");
        std::fs::create_dir_all(drifted_root.join("private")).unwrap();
        std::fs::write(drifted_root.join("public.txt"), "ok").unwrap();
        std::fs::write(drifted_root.join("private/tax.pdf"), "secret").unwrap();

        let orig_str = orig_root.to_string_lossy().to_string();
        let drifted_str = drifted_root.to_string_lossy().to_string();
        let config_initial = {
            let mut cfg = AppConfig::default();
            cfg.index.index_paths = vec![orig_str.clone()];
            cfg.index.include_hidden = true;
            cfg
        };
        let initial_snap =
            VolumeScopeSnapshot::from_config(&config_initial, &orig_root, &drifted_root);

        let identity = VolumeIdentity::LocalUuid {
            uuid: "OFFLINE-EVICT-UUID".to_string(),
            subpath: String::new(),
        };
        let record = VolumeRecord {
            volume_id: identity.volume_id(),
            configured_path: orig_str.clone(),
            active_configured_path: drifted_str.clone(),
            active_mount_root: Some(drifted_str.clone()),
            subpath: String::new(),
            display_name: "Drive".to_string(),
            kind: VolumeKind::LocalExternal,
            fstype: "apfs".to_string(),
            identity,
            last_sync_at: Some(1_700_000_000),
            last_event_id: None,
            journal_uuid: None,
            entry_count: 2,
            scope_snapshot: Some(initial_snap),
            dirty_upper_bound: false,
            pending_confirm_reason: None,
            dirty_filter: false,
            filter_fingerprint: None,
        };
        seed_volume_cache(
            &volumes_dir,
            &record,
            vec![
                make_entry("public.txt", &format!("{drifted_str}/public.txt"), false),
                make_entry("tax.pdf", &format!("{drifted_str}/private/tax.pdf"), false),
            ],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(record.clone());
        manifest.save_atomic(&volumes_dir).unwrap();

        let mounts = Arc::new(Mutex::new(base_mounts()));
        let mounts_read = Arc::clone(&mounts);
        let coord = VolumeCoordinator::with_mount_and_fda_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
            Arc::new(|| true),
        );
        let notify_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let notify_hits_cb = Arc::clone(&notify_hits);
        coord.initialize(
            &config_initial,
            Arc::new(move || {
                notify_hits_cb.fetch_add(1, Ordering::SeqCst);
            }),
        );
        assert_eq!(coord.statuses()[0].state, VolumeState::Offline);

        // Exclude using the configured path while the offline cache holds drifted paths.
        let mut updated = config_initial.clone();
        updated
            .index
            .exclude_paths
            .push(format!("{orig_str}/private"));
        let before_notify = notify_hits.load(Ordering::SeqCst);
        coord.on_config_updated(&updated);
        assert!(
            coord.wait_for_workers_idle(Duration::from_secs(5)),
            "runtime offline prune worker must finish in time"
        );
        assert!(
            notify_hits.load(Ordering::SeqCst) > before_notify,
            "evicting offline entries must emit status change notification"
        );

        let primary = IndexStore::new();
        let (tax_hits, _, _, _) = coord.search(
            &primary,
            "tax.pdf",
            SearchMode::Auto,
            CaseSensitivity::Auto,
            10,
            true,
        );
        assert!(
            tax_hits.is_empty(),
            "newly excluded path on offline remounted drive must disappear from offline search immediately"
        );
        let (pub_hits, _, _, _) = coord.search(
            &primary,
            "public.txt",
            SearchMode::Auto,
            CaseSensitivity::Auto,
            10,
            true,
        );
        assert_eq!(pub_hits.len(), 1);
        assert_eq!(coord.statuses()[0].entry_count, 1);

        // Verify the pruned cache and advanced scope_snapshot were persisted to disk.
        let (persisted_compact, _) =
            cache::load_compact(&volumes_dir.join(format!("{}.cache", record.volume_id))).unwrap();
        assert_eq!(persisted_compact.len(), 1);
        let saved = VolumeManifest::load_from_dir(&volumes_dir);
        assert_eq!(saved.records[0].pending_confirm_reason, None);
        assert!(saved.records[0]
            .scope_snapshot
            .as_ref()
            .unwrap()
            .exclude_subpaths
            .contains(&"private".to_string()));
    }

    #[test]
    fn fda_only_flip_does_not_rescan_external_volume() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let disk_dir = TempDir::new().unwrap();
        let disk_root = disk_dir.path().canonicalize().unwrap();
        std::fs::write(disk_root.join("initial.txt"), "ok").unwrap();

        let disk_str = disk_root.to_string_lossy().to_string();
        let mounted = vec![MountedFsInfo {
            mount_point: disk_str.clone(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk42s1".to_string(),
            is_local: true,
            uuid: Some("EXT-FDA-FLIP-UUID".to_string()),
            total_bytes: 500_000,
            volume_name: "ExtFdaFlip".to_string(),
        }];

        let mut config = AppConfig::default();
        config.index.index_paths = vec![disk_str.clone()];
        config.index.include_hidden = true;

        {
            let mut m = base_mounts();
            m.extend(mounted.clone());
            let coord = VolumeCoordinator::with_mount_and_fda_lister(
                cache_dir.path().to_path_buf(),
                Arc::new(move || m.clone()),
                Arc::new(|| false),
            );
            coord.initialize(&config, Arc::new(|| {}));
            assert_eq!(coord.confirm_all_pending_volume_scans(), 1);
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline && coord.statuses()[0].state != VolumeState::Online {
                std::thread::sleep(Duration::from_millis(25));
            }
            assert_eq!(coord.statuses()[0].state, VolumeState::Online);
            coord.persist_all();
        }

        // Flip only has_fda false -> true while keeping config identical: zero rescan!
        let mut m = base_mounts();
        m.extend(mounted);
        let coord = VolumeCoordinator::with_mount_and_fda_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || m.clone()),
            Arc::new(|| true),
        );
        coord.initialize(&config, Arc::new(|| {}));
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord.statuses()[0].state, VolumeState::Online);
        assert_eq!(
            coord.scan_run_count(),
            0,
            "FDA-only flip must NEVER trigger external volume rescan"
        );
        assert_eq!(
            VolumeManifest::load_from_dir(&volumes_dir).records[0].pending_confirm_reason,
            None
        );
    }

    #[test]
    fn legacy_zero_fingerprint_upgrades_without_spurious_rescan_and_persists_snapshot() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let mounts_parent = TempDir::new().unwrap();
        let disk_root = mounts_parent.path().canonicalize().unwrap().join("LegacyVol");
        std::fs::create_dir_all(&disk_root).unwrap();
        let disk_str = disk_root.to_string_lossy().to_string();

        let identity = VolumeIdentity::LocalUuid {
            uuid: "EXT-LEGACY-ZERO-FP".to_string(),
            subpath: String::new(),
        };
        let record = VolumeRecord {
            volume_id: identity.volume_id(),
            configured_path: disk_str.clone(),
            active_configured_path: disk_str.clone(),
            active_mount_root: None,
            subpath: String::new(),
            display_name: "LegacyVol".to_string(),
            kind: VolumeKind::LocalExternal,
            fstype: "apfs".to_string(),
            identity,
            last_sync_at: Some(1_700_000_000),
            last_event_id: Some(123),
            journal_uuid: None,
            entry_count: 1,
            scope_snapshot: None,
            dirty_upper_bound: false,
            pending_confirm_reason: None,
            dirty_filter: false,
            filter_fingerprint: None,
        };
        let compact = CompactIndex::from_entries(vec![make_entry(
            "legacy_file.txt",
            &format!("{disk_str}/legacy_file.txt"),
            false,
        )])
        .unwrap();
        cache::save_compact(
            &compact,
            Some(cache::Checkpoint {
                event_id: 123,
                fingerprint: 0,
            }),
            &volumes_dir.join(format!("{}.cache", record.volume_id)),
        )
        .unwrap();
        let mut manifest = VolumeManifest::default();
        manifest.upsert(record.clone());
        manifest.save_atomic(&volumes_dir).unwrap();

        let mut config = AppConfig::default();
        config.index.index_paths = vec![disk_str.clone()];
        let expected_snap =
            VolumeScopeSnapshot::from_config(&config, &disk_root, &disk_root);

        let coord = VolumeCoordinator::with_mount_and_fda_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(base_mounts),
            Arc::new(|| true),
        );
        coord.initialize(&config, Arc::new(|| {}));
        assert_eq!(coord.prune_run_count(), 0);
        assert_eq!(coord.scan_run_count(), 0);
        assert_eq!(coord.statuses()[0].state, VolumeState::Offline);

        let upgraded = VolumeManifest::load_from_dir(&volumes_dir);
        assert_eq!(upgraded.records[0].scope_snapshot, Some(expected_snap));
        assert_eq!(upgraded.records[0].pending_confirm_reason, None);
        // 新版 manifest.json 不再写出旧 dirty_filter / filter_fingerprint 字段
        let raw_json = std::fs::read_to_string(volumes_dir.join("manifest.json")).unwrap();
        assert!(!raw_json.contains("\"dirty_filter\""));
        assert!(!raw_json.contains("\"filter_fingerprint\""));
    }

    #[test]
    fn offline_prune_applies_include_hidden_and_exclude_name_rules() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let root_str = "/Volumes/FilterRules";
        let mut config = AppConfig::default();
        config.index.index_paths = vec![root_str.to_string()];
        config.index.include_hidden = true;
        config.index.exclude.clear();

        let mut record = make_network_record(
            root_str,
            root_str,
            None,
            "",
            "FilterRules",
            "//nas.local/FilterRules",
            4,
            false,
        );
        record.scope_snapshot = Some(VolumeScopeSnapshot::from_config(
            &config,
            Path::new(root_str),
            Path::new(root_str),
        ));
        seed_volume_cache(
            &volumes_dir,
            &record,
            vec![
                make_entry("readme.md", &format!("{root_str}/readme.md"), false),
                make_entry(".env", &format!("{root_str}/.env"), false),
                make_entry(".hidden_dir", &format!("{root_str}/.hidden_dir"), true),
                make_entry("secret.txt", &format!("{root_str}/.hidden_dir/secret.txt"), false),
                make_entry("bundle.js", &format!("{root_str}/dist/bundle.js"), false),
            ],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(record);
        manifest.save_atomic(&volumes_dir).unwrap();

        let coord = VolumeCoordinator::with_mount_and_fda_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(base_mounts),
            Arc::new(|| true),
        );
        coord.initialize(&config, Arc::new(|| {}));
        assert_eq!(coord.total_entry_count(&IndexStore::new()), 5);

        let mut updated = config.clone();
        updated.index.include_hidden = false;
        updated.index.exclude = vec!["dist".to_string()];
        coord.on_config_updated(&updated);
        assert!(
            coord.wait_for_workers_idle(Duration::from_secs(5)),
            "offline prune worker must finish in time"
        );

        let primary = IndexStore::new();
        assert_eq!(
            coord.search(&primary, "readme.md", SearchMode::Auto, CaseSensitivity::Auto, 10, true).0.len(),
            1
        );
        assert!(
            coord.search(&primary, ".env", SearchMode::Auto, CaseSensitivity::Auto, 10, true).0.is_empty()
        );
        assert!(
            coord.search(&primary, "secret.txt", SearchMode::Auto, CaseSensitivity::Auto, 10, true).0.is_empty()
        );
        assert!(
            coord.search(&primary, "bundle.js", SearchMode::Auto, CaseSensitivity::Auto, 10, true).0.is_empty()
        );
        assert_eq!(coord.statuses()[0].entry_count, 1);
    }

    #[test]
    fn repeated_reconciles_do_not_reprune_and_mounted_confirm_scan_skips_prune() {
        // Part 1: Offline shard prunes once per scope_snapshot across repeated mount polls and restarts.
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let root_str = "/Volumes/NoReprune";
        let mut config = AppConfig::default();
        config.index.index_paths = vec![root_str.to_string()];
        config.index.include_hidden = true;

        let mut record = make_network_record(
            root_str,
            root_str,
            None,
            "",
            "NoReprune",
            "//nas.local/NoReprune",
            2,
            false,
        );
        record.scope_snapshot = Some(VolumeScopeSnapshot::from_config(
            &config,
            Path::new(root_str),
            Path::new(root_str),
        ));
        seed_volume_cache(
            &volumes_dir,
            &record,
            vec![
                make_entry("keep.txt", &format!("{root_str}/keep.txt"), false),
                make_entry("drop.txt", &format!("{root_str}/blocked/drop.txt"), false),
            ],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(record);
        manifest.save_atomic(&volumes_dir).unwrap();

        let mut edited_cfg = config.clone();
        edited_cfg.index.exclude.push("blocked".to_string());
        let expected_snap = VolumeScopeSnapshot::from_config(
            &edited_cfg,
            Path::new(root_str),
            Path::new(root_str),
        );

        {
            let coord = VolumeCoordinator::with_mount_and_fda_lister(
                cache_dir.path().to_path_buf(),
                Arc::new(base_mounts),
                Arc::new(|| true),
            );
            coord.initialize(&config, Arc::new(|| {}));
            assert_eq!(coord.prune_run_count(), 0);

            coord.on_config_updated(&edited_cfg);
            assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
            assert_eq!(coord.prune_run_count(), 1);
            assert_eq!(
                VolumeManifest::load_from_dir(&volumes_dir).records[0].scope_snapshot,
                Some(expected_snap)
            );

            // Repeated mount-table reconciles while still offline must NOT re-prune.
            let dummy_mounts = base_mounts();
            coord.execute_effects(refresh_mounts_locked(
                &coord.inner,
                &dummy_mounts,
                &volumes_dir,
                false,
            ));
            coord.execute_effects(refresh_mounts_locked(
                &coord.inner,
                &dummy_mounts,
                &volumes_dir,
                false,
            ));
            assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
            assert_eq!(coord.prune_run_count(), 1);
        }

        // Restarting with the same edited config still skips pruning because scope_snapshot persisted.
        {
            let coord_restart = VolumeCoordinator::with_mount_and_fda_lister(
                cache_dir.path().to_path_buf(),
                Arc::new(base_mounts),
                Arc::new(|| true),
            );
            coord_restart.initialize(&edited_cfg, Arc::new(|| {}));
            assert!(coord_restart.wait_for_workers_idle(Duration::from_secs(5)));
            assert_eq!(coord_restart.prune_run_count(), 0);
        }

        // Part 2: Mounted FilterWidened slot enters NeedsConfirmation and skips pruning on confirm scan.
        let mounted_cache = TempDir::new().unwrap();
        let mounted_vols = mounted_cache.path().join("volumes");
        let disk_dir = TempDir::new().unwrap();
        let disk_root = disk_dir.path().canonicalize().unwrap();
        let disk_str = disk_root.to_string_lossy().to_string();
        std::fs::write(disk_root.join("fresh.txt"), "ok").unwrap();

        let id_mounted = VolumeIdentity::LocalUuid {
            uuid: "EXT-MOUNTED-NO-PRUNE".to_string(),
            subpath: String::new(),
        };
        let mounted_record = VolumeRecord {
            volume_id: id_mounted.volume_id(),
            configured_path: disk_str.clone(),
            active_configured_path: disk_str.clone(),
            active_mount_root: Some(disk_str.clone()),
            subpath: String::new(),
            display_name: "MountedNoPrune".to_string(),
            kind: VolumeKind::LocalExternal,
            fstype: "apfs".to_string(),
            identity: id_mounted,
            last_sync_at: Some(1_700_000_000),
            last_event_id: Some(fsevents::current_event_id()),
            journal_uuid: Some("MOUNTED-NO-PRUNE-JUUID".to_string()),
            entry_count: 1,
            scope_snapshot: None,
            dirty_upper_bound: false,
            pending_confirm_reason: Some(VolumeConfirmReason::FilterWidened),
            dirty_filter: false,
            filter_fingerprint: None,
        };
        seed_volume_cache(
            &mounted_vols,
            &mounted_record,
            vec![make_entry("old.txt", &format!("{disk_str}/blocked/old.txt"), false)],
        );
        let mut m2 = VolumeManifest::default();
        m2.upsert(mounted_record);
        m2.save_atomic(&mounted_vols).unwrap();

        let mut online_mounts = base_mounts();
        online_mounts.push(MountedFsInfo {
            mount_point: disk_str.clone(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk44s1".to_string(),
            is_local: true,
            uuid: Some("EXT-MOUNTED-NO-PRUNE".to_string()),
            total_bytes: 500_000,
            volume_name: "MountedNoPrune".to_string(),
        });
        let mut mounted_cfg = AppConfig::default();
        mounted_cfg.index.index_paths = vec![disk_str];
        mounted_cfg.index.exclude.push("blocked".to_string());

        let coord_mounted = VolumeCoordinator::with_mount_and_journal_provider(
            mounted_cache.path().to_path_buf(),
            Arc::new(move || online_mounts.clone()),
            Arc::new(|_| Some("MOUNTED-NO-PRUNE-JUUID".to_string())),
        );
        coord_mounted.initialize(&mounted_cfg, Arc::new(|| {}));
        assert!(coord_mounted.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord_mounted.statuses()[0].state, VolumeState::NeedsConfirmation);
        assert_eq!(
            coord_mounted.statuses()[0].confirm_reason,
            Some(VolumeConfirmReason::FilterWidened)
        );
        assert_eq!(coord_mounted.prune_run_count(), 0);
        assert_eq!(coord_mounted.confirm_all_pending_volume_scans(), 1);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && coord_mounted.statuses()[0].state != VolumeState::Online {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(coord_mounted.statuses()[0].state, VolumeState::Online);
        assert_eq!(coord_mounted.prune_run_count(), 0);
    }

    #[test]
    fn execute_prune_job_skips_when_active_configured_path_drifted() {
        let cache_dir = TempDir::new().unwrap();
        let orig_root = "/Volumes/Drive";
        let drifted_root = "/Volumes/Drive-1";

        let mut config = AppConfig::default();
        config.index.index_paths = vec![orig_root.to_string()];
        config.index.exclude.push("blocked".to_string());
        config.index.include_hidden = true;
        let target_snap = VolumeScopeSnapshot::from_config(
            &config,
            Path::new(orig_root),
            Path::new(orig_root),
        );

        let identity = VolumeIdentity::LocalUuid {
            uuid: "EXT-DRIFT-PRUNE-GUARD".to_string(),
            subpath: String::new(),
        };
        let vid = identity.volume_id();
        let store = IndexStore::new();
        store.replace_compact(
            CompactIndex::from_entries(vec![
                make_entry("Drive", orig_root, true),
                make_entry("keep.txt", &format!("{orig_root}/keep.txt"), false),
                make_entry("secret.txt", &format!("{orig_root}/blocked/secret.txt"), false),
            ])
            .unwrap(),
        );

        let coord = VolumeCoordinator::with_mount_and_fda_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(base_mounts),
            Arc::new(|| true),
        );
        let stale_effects = {
            let mut inner = coord.inner.write();
            inner.config = config.clone();
            inner.slots.push(VolumeSlot {
                volume_id: vid.clone(),
                configured_path: orig_root.to_string(),
                active_configured_path: orig_root.to_string(),
                active_mount_root: None,
                subpath: String::new(),
                display_name: "Drive".to_string(),
                kind: VolumeKind::LocalExternal,
                fstype: "apfs".to_string(),
                from_name: "/dev/disk45s1".to_string(),
                identity,
                state: VolumeState::Offline,
                entry_count: 3,
                last_sync_at: Some(1_700_000_000),
                last_event_id: None,
                journal_uuid: None,
                scope_snapshot: None,
                dirty_upper_bound: false,
                pending_confirm_reason: None,
                in_flight_scan_snapshot: None,
                in_flight_prune_snapshot: None,
                stream_snapshot: None,
                scanned_count: Arc::new(AtomicU64::new(0)),
                is_remounted: false,
                has_cache: true,
                dirty_cache: false,
                cache_seq: 0,
                saved_cache_seq: 0,
                cache_persistence: CachePersistence::default(),
                generation: 1,
                cancel_token: Arc::new(AtomicBool::new(false)),
                store: store.clone(),
                stream_handle: None,
                last_rescan_triggered: None,
                last_scan_duration: None,
                next_reconnect_retry: None,
                unmounting_until: None,
                skip_missing_rescan: false,
            });
            let mut effects = TransitionEffects::default();
            enqueue_prune_for_slot(&mut inner.slots[0], target_snap.clone(), true, false, &mut effects);

            // Simulate mount-root drift between enqueue and off-lock prune execution.
            store.write().rebase_mount_root(orig_root, drifted_root).unwrap();
            inner.slots[0].active_configured_path = drifted_root.to_string();
            inner.slots[0].in_flight_prune_snapshot = None;
            inner.slots[0].is_remounted = true;
            effects
        };

        coord.execute_effects(stale_effects);
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));

        assert_eq!(coord.prune_run_count(), 0, "drifted job must be skipped");
        assert_eq!(store.len(), 3, "drifted store must not be wiped");
        assert_eq!(coord.inner.read().slots[0].scope_snapshot, None);

        // Fresh enqueue against the updated drifted_root prunes only excluded entries.
        let drifted_snap = VolumeScopeSnapshot::from_config(
            &config,
            Path::new(orig_root),
            Path::new(drifted_root),
        );
        let fresh_effects = {
            let mut inner = coord.inner.write();
            let mut effects = TransitionEffects::default();
            enqueue_prune_for_slot(&mut inner.slots[0], drifted_snap.clone(), true, false, &mut effects);
            effects
        };
        coord.execute_effects(fresh_effects);
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));

        assert_eq!(coord.prune_run_count(), 1);
        assert_eq!(coord.inner.read().slots[0].scope_snapshot, Some(drifted_snap));
        let remaining_paths: Vec<String> = store
            .read()
            .iter()
            .map(|(_, v)| v.materialize().path)
            .collect();
        assert_eq!(remaining_paths.len(), 2);
        assert!(remaining_paths.contains(&format!("{drifted_root}/keep.txt")));
        assert!(!remaining_paths.iter().any(|p| p.ends_with("secret.txt")));
    }

    #[test]
    fn execute_prune_job_skips_stale_target_snapshot_and_narrow_then_widen_flow() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let root_str = "/Volumes/FpGuard";

        let mut cfg_a = AppConfig::default();
        cfg_a.index.index_paths = vec![root_str.to_string()];
        cfg_a.index.include_hidden = true;
        cfg_a.index.exclude = vec!["alpha".to_string()];
        let snap_a = VolumeScopeSnapshot::from_config(&cfg_a, Path::new(root_str), Path::new(root_str));

        let mut cfg_b = cfg_a.clone();
        cfg_b.index.exclude.push("beta".to_string());
        let snap_b = VolumeScopeSnapshot::from_config(&cfg_b, Path::new(root_str), Path::new(root_str));
        assert_ne!(snap_a, snap_b);

        let mut record = make_network_record(
            root_str,
            root_str,
            None,
            "",
            "FpGuard",
            "//nas.local/FpGuard",
            3,
            false,
        );
        record.scope_snapshot = Some(snap_a.clone());
        seed_volume_cache(
            &volumes_dir,
            &record,
            vec![
                make_entry("keep.txt", &format!("{root_str}/keep.txt"), false),
                make_entry("beta.txt", &format!("{root_str}/beta/beta.txt"), false),
            ],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(record.clone());
        manifest.save_atomic(&volumes_dir).unwrap();

        let coord = VolumeCoordinator::with_mount_and_fda_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(base_mounts),
            Arc::new(|| true),
        );
        coord.initialize(&cfg_a, Arc::new(|| {}));
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));

        // Enqueue a job with stale target_snapshot while inner.config has moved to cfg_b.
        let mut stale_snap = snap_a.clone();
        stale_snap.exclude.push("stale_only".to_string());
        let stale_effects = {
            let mut inner = coord.inner.write();
            inner.config = cfg_b.clone();
            let mut effects = TransitionEffects::default();
            enqueue_prune_for_slot(&mut inner.slots[0], stale_snap, true, false, &mut effects);
            effects
        };
        coord.execute_effects(stale_effects);
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord.prune_run_count(), 0, "stale target_snapshot job must be skipped");
        assert_eq!(coord.inner.read().slots[0].store.len(), 2);

        // Reset inner.config to cfg_a, then narrow cfg_a -> cfg_b (PruneOnly) and widen cfg_b -> cfg_a (FilterWidened).
        {
            let mut inner = coord.inner.write();
            inner.config = cfg_a.clone();
            inner.slots[0].in_flight_prune_snapshot = None;
        }
        coord.on_config_updated(&cfg_b);
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord.prune_run_count(), 1);
        assert_eq!(coord.inner.read().slots[0].scope_snapshot, Some(snap_b.clone()));
        assert_eq!(coord.inner.read().slots[0].store.len(), 1);

        // Removing "beta" from exclude widens relative to snap_b -> pending_confirm_reason = FilterWidened
        coord.on_config_updated(&cfg_a);
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord.prune_run_count(), 1);
        assert_eq!(
            VolumeManifest::load_from_dir(&volumes_dir).records[0].pending_confirm_reason,
            Some(VolumeConfirmReason::FilterWidened)
        );
        assert_eq!(
            VolumeManifest::load_from_dir(&volumes_dir).records[0].scope_snapshot,
            Some(snap_b)
        );
    }

    #[test]
    fn hot_unplug_and_replug_resets_cancel_token_for_stream_reattach() {
        let cache_dir = TempDir::new().unwrap();
        let disk_dir = TempDir::new().unwrap();
        let disk_root = disk_dir.path().canonicalize().unwrap();
        std::fs::write(disk_root.join("item.txt"), "ok").unwrap();

        let disk_str = disk_root.to_string_lossy().to_string();
        let mounted = vec![MountedFsInfo {
            mount_point: disk_str.clone(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk55s1".to_string(),
            is_local: true,
            uuid: Some("EXT-HOT-REPLUG-STREAM-UUID".to_string()),
            total_bytes: 500_000,
            volume_name: "HotReplug".to_string(),
        }];

        let mut config = AppConfig::default();
        config.index.index_paths = vec![disk_str.clone()];
        let mut initial_mounts = base_mounts();
        initial_mounts.extend(mounted.clone());
        let mounts_state = Arc::new(Mutex::new(initial_mounts));
        let mounts_read = Arc::clone(&mounts_state);

        let coord = VolumeCoordinator::with_mount_and_fda_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
            Arc::new(|| true),
        );
        coord.initialize(&config, Arc::new(|| {}));
        assert_eq!(coord.confirm_all_pending_volume_scans(), 1);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && coord.statuses()[0].state != VolumeState::Online {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(coord.statuses()[0].state, VolumeState::Online);
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));

        let vid = coord.statuses()[0].volume_id.clone();
        let old_token = coord.slot_cancel_token(&vid).unwrap();

        // Simulate hot unplug: old token flips true, while the slot receives a fresh false token.
        *mounts_state.lock() = base_mounts();
        coord.notify_volume_will_unmount(&disk_root);
        assert!(old_token.load(Ordering::SeqCst));
        let reset_token = coord.slot_cancel_token(&vid).unwrap();
        assert!(
            !reset_token.load(Ordering::SeqCst),
            "cancel_slot_work must install a fresh uncancelled token for the new generation"
        );
        assert_eq!(coord.statuses()[0].state, VolumeState::Offline);

        // Hot replug while cache is already clean: AttachStreamJob must enqueue & attach.
        let mut replugged = base_mounts();
        replugged.extend(mounted);
        *mounts_state.lock() = replugged;
        coord.notify_volume_mounted(&disk_root);
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord.statuses()[0].state, VolumeState::Online);
        assert!(
            coord.inner.read().slots[0].stream_handle.is_some(),
            "AttachStreamJob must not be dropped on hot replug after willUnmount"
        );
    }

    #[test]
    fn failed_prune_cache_save_does_not_advance_scope_snapshot_until_persisted() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let root_a = "/Volumes/DirtyPruneA";
        let root_b = "/Volumes/DirtyPruneB";

        let mut config = AppConfig::default();
        config.index.index_paths = vec![root_b.to_string(), root_a.to_string()];
        config.index.include_hidden = true;
        let snap_a_initial =
            VolumeScopeSnapshot::from_config(&config, Path::new(root_a), Path::new(root_a));
        let snap_b_initial =
            VolumeScopeSnapshot::from_config(&config, Path::new(root_b), Path::new(root_b));

        let mut rec_a = make_network_record(
            root_a,
            root_a,
            None,
            "",
            "DirtyPruneA",
            "//nas.local/DirtyPruneA",
            2,
            false,
        );
        rec_a.scope_snapshot = Some(snap_a_initial.clone());
        let mut rec_b = make_network_record(
            root_b,
            root_b,
            None,
            "",
            "DirtyPruneB",
            "//nas.local/DirtyPruneB",
            1,
            false,
        );
        rec_b.scope_snapshot = Some(snap_b_initial);

        seed_volume_cache(
            &volumes_dir,
            &rec_a,
            vec![
                make_entry("keep.txt", &format!("{root_a}/keep.txt"), false),
                make_entry("drop.txt", &format!("{root_a}/blocked/drop.txt"), false),
            ],
        );
        seed_volume_cache(
            &volumes_dir,
            &rec_b,
            vec![make_entry("b.txt", &format!("{root_b}/b.txt"), false)],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(rec_a.clone());
        manifest.upsert(rec_b.clone());
        manifest.save_atomic(&volumes_dir).unwrap();

        let coord = VolumeCoordinator::with_mount_and_fda_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(base_mounts),
            Arc::new(|| true),
        );
        coord.initialize(&config, Arc::new(|| {}));
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));

        // Replace rec_a's .cache with a non-empty directory so rename() fails like a disk/IO error.
        let cache_a = volumes_dir.join(format!("{}.cache", rec_a.volume_id));
        std::fs::remove_file(&cache_a).unwrap();
        std::fs::create_dir_all(cache_a.join("blocker")).unwrap();

        let mut updated = config.clone();
        updated.index.exclude.push("blocked".to_string());
        let snap_a_updated =
            VolumeScopeSnapshot::from_config(&updated, Path::new(root_a), Path::new(root_a));
        coord.on_config_updated(&updated);
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));

        // Slot A's cache flush failed -> scope_snapshot must NOT advance in memory or manifest.json!
        {
            let guard = coord.inner.read();
            let slot_a = guard
                .slots
                .iter()
                .find(|s| s.volume_id == rec_a.volume_id)
                .unwrap();
            assert!(slot_a.dirty_cache);
            assert_eq!(slot_a.scope_snapshot, Some(snap_a_initial.clone()));
        }
        let saved_manifest = VolumeManifest::load_from_dir(&volumes_dir);
        let saved_a = saved_manifest
            .records
            .iter()
            .find(|r| r.volume_id == rec_a.volume_id)
            .unwrap();
        assert_eq!(
            saved_a.scope_snapshot,
            Some(snap_a_initial),
            "manifest must not advance scope_snapshot while pruned shard failed to flush"
        );

        // Once the disk obstacle is cleared, persist_all flushes slot A and next reconcile cleanly advances scope_snapshot.
        std::fs::remove_dir_all(&cache_a).unwrap();
        coord.persist_all();
        let dummy_mounts = base_mounts();
        coord.execute_effects(refresh_mounts_locked(
            &coord.inner,
            &dummy_mounts,
            &volumes_dir,
            false,
        ));
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
        let flushed_manifest = VolumeManifest::load_from_dir(&volumes_dir);
        let flushed_a = flushed_manifest
            .records
            .iter()
            .find(|r| r.volume_id == rec_a.volume_id)
            .unwrap();
        assert_eq!(flushed_a.scope_snapshot, Some(snap_a_updated));
        let (reloaded_a, _) = cache::load_compact(&cache_a).unwrap();
        assert_eq!(reloaded_a.len(), 1);
    }

    #[test]
    fn stale_generation_save_does_not_overwrite_newer_cache_and_already_saved_reports_saved() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let root_str = "/Volumes/StaleGenSave";
        let identity = VolumeIdentity::LocalUuid {
            uuid: "EXT-STALE-GEN-SAVE".to_string(),
            subpath: String::new(),
        };
        let vid = identity.volume_id();
        let cache_path = volumes_dir.join(format!("{vid}.cache"));

        let old_store = IndexStore::new();
        old_store.replace_compact(
            CompactIndex::from_entries(vec![make_entry(
                "old.txt",
                &format!("{root_str}/old.txt"),
                false,
            )])
            .unwrap(),
        );
        let new_store = IndexStore::new();
        new_store.replace_compact(
            CompactIndex::from_entries(vec![
                make_entry("fresh1.txt", &format!("{root_str}/fresh1.txt"), false),
                make_entry("fresh2.txt", &format!("{root_str}/fresh2.txt"), false),
            ])
            .unwrap(),
        );

        let coord = VolumeCoordinator::with_mount_and_fda_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(base_mounts),
            Arc::new(|| true),
        );
        {
            let mut inner = coord.inner.write();
            inner.slots.push(VolumeSlot {
                volume_id: vid.clone(),
                configured_path: root_str.to_string(),
                active_configured_path: root_str.to_string(),
                active_mount_root: Some(root_str.to_string()),
                subpath: String::new(),
                display_name: "StaleGenSave".to_string(),
                kind: VolumeKind::LocalExternal,
                fstype: "apfs".to_string(),
                from_name: "/dev/disk50s1".to_string(),
                identity,
                state: VolumeState::Online,
                entry_count: 2,
                last_sync_at: Some(1_700_000_000),
                last_event_id: Some(200),
                journal_uuid: None,
                scope_snapshot: None,
                dirty_upper_bound: false,
                pending_confirm_reason: None,
                in_flight_scan_snapshot: None,
                in_flight_prune_snapshot: None,
                stream_snapshot: None,
                scanned_count: Arc::new(AtomicU64::new(0)),
                is_remounted: false,
                has_cache: true,
                dirty_cache: true,
                cache_seq: 3,
                saved_cache_seq: 2,
                cache_persistence: CachePersistence::default(),
                generation: 2,
                cancel_token: Arc::new(AtomicBool::new(false)),
                store: new_store.clone(),
                stream_handle: None,
                last_rescan_triggered: None,
                last_scan_duration: None,
                next_reconnect_retry: None,
                unmounting_until: None,
                skip_missing_rescan: false,
            });
        }

        // First persist the current generation (gen=2) snapshot.
        let fresh_save = coord.inner.read().slots[0].cache_save(&volumes_dir);
        let redundant_save = coord.inner.read().slots[0].cache_save(&volumes_dir);
        let saved_fresh = save_dirty_caches_and_mark(&coord.inner, vec![fresh_save]);
        assert!(saved_fresh.contains(&vid));

        // A stale generation (gen=1) save arriving late must be ignored and must not overwrite disk.
        let stale_save = CacheSave {
            volume_id: vid.clone(),
            generation: 1,
            cache_seq: 1,
            applied_events: 0,
            cache_path: cache_path.clone(),
            store: old_store,
            checkpoint: Some(cache::Checkpoint {
                event_id: 10,
                fingerprint: 1,
            }),
        };
        let saved_stale = save_dirty_caches_and_mark(&coord.inner, vec![stale_save]);
        assert!(saved_stale.is_empty(), "stale generation save must not report saved");
        let (reloaded, cp) = cache::load_compact(&cache_path).unwrap();
        assert_eq!(reloaded.len(), 2, "newer generation cache must remain intact");
        assert_eq!(cp.unwrap().event_id, 200);

        // A redundant same-generation save after another saver already persisted seq=3
        // skips disk I/O while still reporting the volume as safely saved.
        let redundant_saved = save_dirty_caches_and_mark(&coord.inner, vec![redundant_save]);
        assert!(
            redundant_saved.contains(&vid),
            "already-persisted snapshot must report saved so persist_all updates manifest"
        );
    }

    #[test]
    fn failed_scan_fallback_to_missing_and_offline_prunes_dirty_cache() {
        // Case 1: RootUnavailable fallback to Missing prunes excluded entries.
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let mount_dir = TempDir::new().unwrap();
        let mount_root = mount_dir.path().canonicalize().unwrap();
        let missing_sub = mount_root.join("vanished_subdir");
        let missing_sub_str = missing_sub.to_string_lossy().to_string();

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![missing_sub_str.clone()];
        cfg.index.include_hidden = true;
        let old_snap = VolumeScopeSnapshot::from_config(&cfg, &missing_sub, &missing_sub);

        let identity = VolumeIdentity::LocalUuid {
            uuid: "EXT-SCAN-FAIL-MISSING".to_string(),
            subpath: "vanished_subdir".to_string(),
        };
        let rec = VolumeRecord {
            volume_id: identity.volume_id(),
            configured_path: missing_sub_str.clone(),
            active_configured_path: missing_sub_str.clone(),
            active_mount_root: Some(mount_root.to_string_lossy().to_string()),
            subpath: "vanished_subdir".to_string(),
            display_name: "MissingFallback".to_string(),
            kind: VolumeKind::LocalExternal,
            fstype: "apfs".to_string(),
            identity,
            last_sync_at: Some(1_700_000_000),
            last_event_id: None,
            journal_uuid: None,
            entry_count: 2,
            scope_snapshot: Some(old_snap),
            dirty_upper_bound: false,
            pending_confirm_reason: None,
            dirty_filter: false,
            filter_fingerprint: None,
        };
        seed_volume_cache(
            &volumes_dir,
            &rec,
            vec![
                make_entry("keep.txt", &format!("{missing_sub_str}/keep.txt"), false),
                make_entry("secret.txt", &format!("{missing_sub_str}/blocked/secret.txt"), false),
            ],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(rec);
        manifest.save_atomic(&volumes_dir).unwrap();

        let mut mounts = base_mounts();
        mounts.push(MountedFsInfo {
            mount_point: mount_root.to_string_lossy().to_string(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk61s1".to_string(),
            is_local: true,
            uuid: Some("EXT-SCAN-FAIL-MISSING".to_string()),
            total_bytes: 500_000,
            volume_name: "MissingFallback".to_string(),
        });

        let mut edited = cfg.clone();
        edited.index.exclude.push("blocked".to_string());

        let coord = VolumeCoordinator::with_mount_and_fda_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts.clone()),
            Arc::new(|| true),
        );
        coord.initialize(&edited, Arc::new(|| {}));
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
        // Explicitly trigger rescan against vanished subpath -> falls back to Missing while keeping pruned cache
        assert_eq!(coord.rescan_volume(&missing_sub_str), Ok(true));
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord.statuses()[0].state, VolumeState::Missing);
        let primary = IndexStore::new();
        assert!(
            coord
                .search(&primary, "secret.txt", SearchMode::Auto, CaseSensitivity::Auto, 10, true)
                .0
                .is_empty(),
            "RootUnavailable scan fallback to Missing must preserve pruned exclusions"
        );
        assert_eq!(
            coord
                .search(&primary, "keep.txt", SearchMode::Auto, CaseSensitivity::Auto, 10, true)
                .0
                .len(),
            1
        );

        // Case 2: Disconnected scan fallback to Offline prunes excluded entries.
        let cache_dir2 = TempDir::new().unwrap();
        let volumes_dir2 = cache_dir2.path().join("volumes");
        let disk_dir2 = TempDir::new().unwrap();
        let disk_root2 = disk_dir2.path().canonicalize().unwrap();
        std::fs::create_dir_all(disk_root2.join("blocked")).unwrap();
        std::fs::write(disk_root2.join("keep.txt"), "ok").unwrap();
        std::fs::write(disk_root2.join("blocked/secret.txt"), "no").unwrap();
        let disk_str2 = disk_root2.to_string_lossy().to_string();

        let mut cfg2 = AppConfig::default();
        cfg2.index.index_paths = vec![disk_str2.clone()];
        cfg2.index.include_hidden = true;
        let snap_old2 = VolumeScopeSnapshot::from_config(&cfg2, &disk_root2, &disk_root2);

        let mut rec2 = make_network_record(
            &disk_str2,
            &disk_str2,
            Some(&disk_str2),
            "",
            "DisconnFallback",
            "//nas.local/DisconnFallback",
            2,
            false,
        );
        rec2.scope_snapshot = Some(snap_old2);
        seed_volume_cache(
            &volumes_dir2,
            &rec2,
            vec![
                make_entry("keep.txt", &format!("{disk_str2}/keep.txt"), false),
                make_entry("secret.txt", &format!("{disk_str2}/blocked/secret.txt"), false),
            ],
        );
        let mut manifest2 = VolumeManifest::default();
        manifest2.upsert(rec2);
        manifest2.save_atomic(&volumes_dir2).unwrap();

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_ref = Arc::clone(&calls);
        let disk_str2_clone = disk_str2.clone();
        // First two mount_lister calls (initialize + rescan_volume) see the share mounted;
        // subsequent call inside scan_volume_compact_atomic's is_mounted check reports disconnected.
        let coord2 = VolumeCoordinator::with_mount_and_fda_lister(
            cache_dir2.path().to_path_buf(),
            Arc::new(move || {
                let mut m = base_mounts();
                if calls_ref.fetch_add(1, Ordering::SeqCst) < 2 {
                    m.push(MountedFsInfo {
                        mount_point: disk_str2_clone.clone(),
                        fstype: "smbfs".to_string(),
                        from_name: "//nas.local/DisconnFallback".to_string(),
                        is_local: false,
                        uuid: None,
                        total_bytes: 500_000,
                        volume_name: "DisconnFallback".to_string(),
                    });
                }
                m
            }),
            Arc::new(|| true),
        );
        let mut edited2 = cfg2.clone();
        edited2.index.exclude.push("blocked".to_string());
        coord2.initialize(&edited2, Arc::new(|| {}));
        assert!(coord2.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord2.rescan_volume(&disk_str2), Ok(true));
        assert!(coord2.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord2.statuses()[0].state, VolumeState::Offline);
        assert!(
            coord2
                .search(&primary, "secret.txt", SearchMode::Auto, CaseSensitivity::Auto, 10, true)
                .0
                .is_empty(),
            "Disconnected scan fallback to Offline must keep newly excluded entries pruned"
        );
        assert_eq!(
            coord2
                .search(&primary, "keep.txt", SearchMode::Auto, CaseSensitivity::Auto, 10, true)
                .0
                .len(),
            1
        );
    }

    #[test]
    fn orphan_unlink_skips_volume_reclaimed_by_slot_or_manifest() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        std::fs::create_dir_all(&volumes_dir).unwrap();
        let rec = make_network_record(
            "/Volumes/Reclaimed",
            "/Volumes/Reclaimed",
            None,
            "",
            "Reclaimed",
            "//nas.local/Reclaimed",
            1,
            false,
        );
        let vid = rec.volume_id.clone();
        let cache_path = volumes_dir.join(format!("{vid}.cache"));
        std::fs::write(&cache_path, b"LOKI-NEW").unwrap();

        let coord = VolumeCoordinator::with_mount_and_fda_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(base_mounts),
            Arc::new(|| true),
        );
        coord.inner.write().manifest.upsert(rec);

        // Must preserve cache file when manifest already contains the re-added volume.
        unlink_orphan_cache_if_still_absent(&coord.inner, &vid, &cache_path);
        assert!(cache_path.exists(), "reclaimed volume cache must not be unlinked");

        // Once absent from both slots and manifest, orphan file is unlinked cleanly.
        coord.inner.write().manifest.remove_by_path_or_id(&vid);
        unlink_orphan_cache_if_still_absent(&coord.inner, &vid, &cache_path);
        assert!(!cache_path.exists(), "true orphan cache file must be unlinked");
    }

    #[test]
    fn transition_effects_delete_skips_reclaimed_volume_cache() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        std::fs::create_dir_all(&volumes_dir).unwrap();
        let rec = make_network_record(
            "/Volumes/ReclaimedEffects",
            "/Volumes/ReclaimedEffects",
            None,
            "",
            "ReclaimedEffects",
            "//nas.local/ReclaimedEffects",
            1,
            false,
        );
        let vid = rec.volume_id.clone();
        let cache_path = volumes_dir.join(format!("{vid}.cache"));
        std::fs::write(&cache_path, b"LOKI-FRESH").unwrap();

        let coord = VolumeCoordinator::with_mount_and_fda_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(base_mounts),
            Arc::new(|| true),
        );
        coord.inner.write().manifest.upsert(rec);

        let mut stale_effects = TransitionEffects::default();
        stale_effects.push_cache_delete(vid.clone(), cache_path.clone());
        stale_effects.push_cache_delete(vid.clone(), cache_path.clone());
        assert_eq!(stale_effects.files_to_delete.len(), 1);
        coord.execute_effects(stale_effects);
        assert!(
            cache_path.exists(),
            "TransitionEffects::execute must not unlink cache reclaimed by manifest/slots"
        );

        coord.inner.write().manifest.remove_by_path_or_id(&vid);
        let mut final_effects = TransitionEffects::default();
        final_effects.push_cache_delete(vid, cache_path.clone());
        coord.execute_effects(final_effects);
        assert!(
            !cache_path.exists(),
            "TransitionEffects::execute must unlink cache once absent from slots and manifest"
        );
    }

    #[test]
    fn drifted_mount_rescan_keeps_configured_exclude_paths_excluded() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let mounts_parent = TempDir::new().unwrap();
        let parent_canon = mounts_parent.path().canonicalize().unwrap();
        let orig_root = parent_canon.join("Drive");
        let drifted_root = parent_canon.join("Drive-1");
        std::fs::create_dir_all(drifted_root.join("private")).unwrap();
        std::fs::write(drifted_root.join("public.txt"), "ok").unwrap();
        std::fs::write(drifted_root.join("fresh.txt"), "new").unwrap();
        std::fs::write(drifted_root.join("private/tax.pdf"), "secret").unwrap();

        let orig_str = orig_root.to_string_lossy().to_string();
        let drifted_str = drifted_root.to_string_lossy().to_string();
        let mut config = AppConfig::default();
        config.index.index_paths = vec![orig_str.clone()];
        config.index.include_hidden = true;
        let initial_snap = VolumeScopeSnapshot::from_config(&config, &orig_root, &drifted_root);

        let identity = VolumeIdentity::LocalUuid {
            uuid: "EXT-DRIFT-SCAN-EXCLUDE".to_string(),
            subpath: String::new(),
        };
        let record = VolumeRecord {
            volume_id: identity.volume_id(),
            configured_path: orig_str.clone(),
            active_configured_path: drifted_str.clone(),
            active_mount_root: Some(drifted_str.clone()),
            subpath: String::new(),
            display_name: "Drive".to_string(),
            kind: VolumeKind::LocalExternal,
            fstype: "apfs".to_string(),
            identity,
            last_sync_at: Some(1_700_000_000),
            last_event_id: Some(fsevents::current_event_id()),
            journal_uuid: fsevents::journal_uuid_for_path(&drifted_root),
            entry_count: 2,
            scope_snapshot: Some(initial_snap),
            dirty_upper_bound: false,
            pending_confirm_reason: None,
            dirty_filter: false,
            filter_fingerprint: None,
        };
        seed_volume_cache(
            &volumes_dir,
            &record,
            vec![
                make_entry("public.txt", &format!("{drifted_str}/public.txt"), false),
                make_entry("tax.pdf", &format!("{drifted_str}/private/tax.pdf"), false),
            ],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(record);
        manifest.save_atomic(&volumes_dir).unwrap();

        let mounts_state = Arc::new(Mutex::new(base_mounts()));
        let mounts_read = Arc::clone(&mounts_state);
        let coord = VolumeCoordinator::with_mount_and_fda_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
            Arc::new(|| true),
        );
        let mut excluded_cfg = config.clone();
        excluded_cfg
            .index
            .exclude_paths
            .push(format!("{orig_str}/private"));
        coord.initialize(&excluded_cfg, Arc::new(|| {}));
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));

        let primary = IndexStore::new();
        assert!(
            coord
                .search(&primary, "tax.pdf", SearchMode::Auto, CaseSensitivity::Auto, 10, true)
                .0
                .is_empty(),
            "offline prune must remove configured exclude_paths under drifted mount"
        );

        // Replug at the drifted mount root (`Drive-1`) then run explicit rescan: must keep `private/tax.pdf` excluded.
        let mut replugged = base_mounts();
        replugged.push(MountedFsInfo {
            mount_point: drifted_str.clone(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk72s1".to_string(),
            is_local: true,
            uuid: Some("EXT-DRIFT-SCAN-EXCLUDE".to_string()),
            total_bytes: 500_000,
            volume_name: "Drive-1".to_string(),
        });
        *mounts_state.lock() = replugged;
        coord.notify_volume_mounted(&drifted_root);
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord.statuses()[0].state, VolumeState::Online);
        assert_eq!(coord.scan_run_count(), 0);

        assert_eq!(coord.rescan_volume(&orig_str), Ok(true));
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && coord.statuses()[0].state != VolumeState::Online {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(coord.statuses()[0].state, VolumeState::Online);
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));

        assert!(
            coord
                .search(&primary, "tax.pdf", SearchMode::Auto, CaseSensitivity::Auto, 10, true)
                .0
                .is_empty(),
            "drifted mount rescan must not resurrect entries excluded under configured_path"
        );
        assert_eq!(
            coord
                .search(&primary, "fresh.txt", SearchMode::Auto, CaseSensitivity::Auto, 10, true)
                .0
                .len(),
            1
        );
    }

    #[test]
    fn will_unmount_during_narrowing_rescan_prunes_offline_cache_immediately() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let root_str = "/Volumes/WillUnmountPrune";
        let mut old_cfg = AppConfig::default();
        old_cfg.index.index_paths = vec![root_str.to_string()];
        old_cfg.index.include_hidden = true;
        let old_snap =
            VolumeScopeSnapshot::from_config(&old_cfg, Path::new(root_str), Path::new(root_str));

        let mut cfg = old_cfg.clone();
        cfg.index.exclude.push("blocked".to_string());

        let identity = VolumeIdentity::LocalUuid {
            uuid: "EXT-WILL-UNMOUNT-PRUNE".to_string(),
            subpath: String::new(),
        };
        let vid = identity.volume_id();
        let store = IndexStore::new();
        store.replace_compact(
            CompactIndex::from_entries(vec![
                make_entry("keep.txt", &format!("{root_str}/keep.txt"), false),
                make_entry("secret.txt", &format!("{root_str}/blocked/secret.txt"), false),
            ])
            .unwrap(),
        );
        let cache_path = volumes_dir.join(format!("{vid}.cache"));
        cache::save_compact(&store.read(), None, &cache_path).unwrap();

        let coord = VolumeCoordinator::with_mount_and_fda_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(base_mounts),
            Arc::new(|| true),
        );
        {
            let mut inner = coord.inner.write();
            inner.config = cfg;
            inner.slots.push(VolumeSlot {
                volume_id: vid,
                configured_path: root_str.to_string(),
                active_configured_path: root_str.to_string(),
                active_mount_root: Some(root_str.to_string()),
                subpath: String::new(),
                display_name: "WillUnmountPrune".to_string(),
                kind: VolumeKind::LocalExternal,
                fstype: "apfs".to_string(),
                from_name: "/dev/disk73s1".to_string(),
                identity,
                state: VolumeState::Syncing,
                entry_count: 2,
                last_sync_at: Some(1_700_000_000),
                last_event_id: None,
                journal_uuid: None,
                scope_snapshot: Some(old_snap),
                dirty_upper_bound: false,
                pending_confirm_reason: None,
                in_flight_scan_snapshot: None,
                in_flight_prune_snapshot: None,
                stream_snapshot: None,
                scanned_count: Arc::new(AtomicU64::new(0)),
                is_remounted: false,
                has_cache: true,
                dirty_cache: false,
                cache_seq: 0,
                saved_cache_seq: 0,
                cache_persistence: CachePersistence::default(),
                generation: 1,
                cancel_token: Arc::new(AtomicBool::new(false)),
                store,
                stream_handle: None,
                last_rescan_triggered: None,
                last_scan_duration: None,
                next_reconnect_retry: None,
                unmounting_until: None,
                skip_missing_rescan: false,
            });
        }

        // Calling willUnmount alone (without didUnmount) must cancel the scan and prune immediately.
        coord.notify_volume_will_unmount(Path::new(root_str));
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord.statuses()[0].state, VolumeState::Offline);

        let primary = IndexStore::new();
        assert!(
            coord
                .search(&primary, "secret.txt", SearchMode::Auto, CaseSensitivity::Auto, 10, true)
                .0
                .is_empty(),
            "willUnmount interrupting a scan must prune excluded entries immediately"
        );
        assert_eq!(
            coord
                .search(&primary, "keep.txt", SearchMode::Auto, CaseSensitivity::Auto, 10, true)
                .0
                .len(),
            1
        );
    }

    #[test]
    fn cross_volume_isolation_and_online_drifted_prune() {
        let cache_dir = TempDir::new().unwrap();
        let mounts_parent = TempDir::new().unwrap();
        let parent_canon = mounts_parent.path().canonicalize().unwrap();
        let orig_root = parent_canon.join("Work22T");
        let drifted_root = parent_canon.join("Work22T-1");
        let local_dir = TempDir::new().unwrap();
        std::fs::create_dir_all(drifted_root.join("Cache")).unwrap();
        std::fs::write(drifted_root.join("keep.txt"), "ok").unwrap();
        std::fs::write(drifted_root.join("Cache/junk.tmp"), "drop").unwrap();

        let orig_str = orig_root.to_string_lossy().to_string();
        let drifted_str = drifted_root.to_string_lossy().to_string();
        let local_str = local_dir.path().canonicalize().unwrap().to_string_lossy().to_string();

        let mut config = AppConfig::default();
        config.index.index_paths = vec![local_str.clone(), orig_str.clone()];
        config.index.index_apps = false;
        config.index.include_hidden = false;

        let mut mounts = base_mounts();
        mounts.push(MountedFsInfo {
            mount_point: drifted_str.clone(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk80s1".to_string(),
            is_local: true,
            uuid: Some("EXT-ISOLATION-UUID".to_string()),
            total_bytes: 22_000_000_000_000,
            volume_name: "Work22T-1".to_string(),
        });
        let mounts_arc = Arc::new(Mutex::new(mounts));
        let mounts_read = Arc::clone(&mounts_arc);
        let fda = Arc::new(AtomicBool::new(false));
        let fda_read = Arc::clone(&fda);

        let coord = VolumeCoordinator::with_mount_and_fda_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(move || mounts_read.lock().clone()),
            Arc::new(move || fda_read.load(Ordering::SeqCst)),
        );

        // Seed cached slot on drifted mount and initialize -> Online with live stream
        let identity = VolumeIdentity::LocalUuid {
            uuid: "EXT-ISOLATION-UUID".to_string(),
            subpath: String::new(),
        };
        let volumes_dir = cache_dir.path().join("volumes");
        let init_snap = VolumeScopeSnapshot::from_config(&config, &orig_root, &drifted_root);
        let rec = VolumeRecord {
            volume_id: identity.volume_id(),
            configured_path: orig_str.clone(),
            active_configured_path: drifted_str.clone(),
            active_mount_root: Some(drifted_str.clone()),
            subpath: String::new(),
            display_name: "Work22T".to_string(),
            kind: VolumeKind::LocalExternal,
            fstype: "apfs".to_string(),
            identity,
            last_sync_at: Some(1_700_000_000),
            last_event_id: Some(fsevents::current_event_id()),
            journal_uuid: fsevents::journal_uuid_for_path(&drifted_root),
            entry_count: 2,
            scope_snapshot: Some(init_snap),
            dirty_upper_bound: false,
            pending_confirm_reason: None,
            dirty_filter: false,
            filter_fingerprint: None,
        };
        seed_volume_cache(
            &volumes_dir,
            &rec,
            vec![
                make_entry("keep.txt", &format!("{drifted_str}/keep.txt"), false),
                make_entry("junk.tmp", &format!("{drifted_str}/Cache/junk.tmp"), false),
            ],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(rec);
        manifest.save_atomic(&volumes_dir).unwrap();

        coord.initialize(&config, Arc::new(|| {}));
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord.statuses()[0].state, VolumeState::Online);
        assert_eq!(coord.scan_run_count(), 0);
        let gen_before = coord.slot_generation(&orig_str).unwrap();
        assert!(coord.inner.read().slots[0].stream_handle.is_some());

        // 1. Mutate local index_paths, toggle index_apps, exclude a local path, flip has_fda:
        //    external volume MUST stay Online with scan_runs == 0 and untouched generation/stream!
        let mut cfg_local_edits = config.clone();
        cfg_local_edits.index.index_paths.push("/tmp".to_string());
        cfg_local_edits.index.index_apps = true;
        cfg_local_edits.index.exclude_paths.push(format!("{local_str}/local_skip"));
        fda.store(true, Ordering::SeqCst);
        coord.on_config_updated(&cfg_local_edits);
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord.scan_run_count(), 0);
        assert_eq!(coord.prune_run_count(), 0);
        assert_eq!(coord.statuses()[0].state, VolumeState::Online);
        assert_eq!(coord.slot_generation(&orig_str), Some(gen_before));
        assert!(coord.inner.read().slots[0].stream_handle.is_some());

        // 2. Add configured exclude path `/Volumes/Work22T/Cache` while online in `-1` drifted state:
        //    PruneOnly evicts Cache/junk.tmp, advances scope_snapshot, keeps Online, reattaches stream, scan_runs == 0!
        let mut cfg_vol_exclude = cfg_local_edits.clone();
        cfg_vol_exclude.index.exclude_paths.push(format!("{orig_str}/Cache"));
        coord.on_config_updated(&cfg_vol_exclude);
        assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord.scan_run_count(), 0);
        assert_eq!(coord.prune_run_count(), 1);
        assert_eq!(coord.statuses()[0].state, VolumeState::Online);
        assert!(coord.inner.read().slots[0].stream_handle.is_some());

        let primary = IndexStore::new();
        assert!(coord
            .search(&primary, "junk.tmp", SearchMode::Auto, CaseSensitivity::Auto, 10, true)
            .0
            .is_empty());
        assert_eq!(
            coord
                .search(&primary, "keep.txt", SearchMode::Auto, CaseSensitivity::Auto, 10, true)
                .0
                .len(),
            1
        );
        let saved = VolumeManifest::load_from_dir(&volumes_dir);
        assert!(saved.records[0]
            .scope_snapshot
            .as_ref()
            .unwrap()
            .exclude_subpaths
            .contains(&"Cache".to_string()));
    }

    #[test]
    fn stream_loop_ignores_mount_and_root_events_replays_user_dropped_and_invalidates_journal() {
        // Test 1: MOUNT_CHANGED + root ITEM_CREATED + 2x USER_DROPPED replay cleanly; 4th USER_DROPPED -> JournalInvalidated
        for fstype in ["apfs", "exfat"] {
            let cache_dir = TempDir::new().unwrap();
            let volumes_dir = cache_dir.path().join("volumes");
            let disk_dir = TempDir::new().unwrap();
            let disk_root = disk_dir.path().canonicalize().unwrap();
            let disk_str = disk_root.to_string_lossy().to_string();
            // Create a disk file that is NOT in the store: if root ITEM_CREATED triggered dir_creates scan, it would appear!
            std::fs::write(disk_root.join("unindexed_canary.txt"), "canary").unwrap();

            let identity = VolumeIdentity::LocalUuid {
                uuid: format!("STREAM-GUARD-{fstype}"),
                subpath: String::new(),
            };
            let vid = identity.volume_id();
            let store = IndexStore::new();
            store.replace_compact(
                CompactIndex::from_entries(vec![make_entry(
                    "kept.txt",
                    &format!("{disk_str}/kept.txt"),
                    false,
                )])
                .unwrap(),
            );
            let cfg = AppConfig::default();
            let snap = VolumeScopeSnapshot::from_config(&cfg, &disk_root, &disk_root);

            let coord = VolumeCoordinator::with_mount_lister(
                cache_dir.path().to_path_buf(),
                Arc::new(base_mounts),
            );
            {
                let mut inner = coord.inner.write();
                inner.slots.push(VolumeSlot {
                    volume_id: vid.clone(),
                    configured_path: disk_str.clone(),
                    active_configured_path: disk_str.clone(),
                    active_mount_root: Some(disk_str.clone()),
                    subpath: String::new(),
                    display_name: "StreamGuard".to_string(),
                    kind: VolumeKind::LocalExternal,
                    fstype: fstype.to_string(),
                    from_name: "/dev/disk81s1".to_string(),
                    identity,
                    state: VolumeState::Online,
                    entry_count: 1,
                    last_sync_at: Some(1_700_000_000),
                    last_event_id: Some(500),
                    journal_uuid: Some("UUID-1".to_string()),
                    scope_snapshot: Some(snap.clone()),
                    dirty_upper_bound: false,
                    pending_confirm_reason: None,
                    in_flight_scan_snapshot: None,
                    in_flight_prune_snapshot: None,
                    stream_snapshot: Some(snap.clone()),
                    scanned_count: Arc::new(AtomicU64::new(0)),
                    is_remounted: false,
                    has_cache: true,
                    dirty_cache: false,
                    cache_seq: 0,
                    saved_cache_seq: 0,
                    cache_persistence: CachePersistence::default(),
                    generation: 1,
                    cancel_token: Arc::new(AtomicBool::new(false)),
                    store: store.clone(),
                    stream_handle: None,
                    last_rescan_triggered: None,
                    last_scan_duration: None,
                    next_reconnect_retry: None,
                    unmounting_until: None,
                    skip_missing_rescan: false,
                });
            }

            let (tx1, rx1) = mpsc::channel();
            let reopen_txs: Arc<Mutex<Vec<mpsc::Sender<Vec<fsevents::Event>>>>> =
                Arc::new(Mutex::new(Vec::new()));
            let reopen_ids: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
            let reopen_txs_cb = Arc::clone(&reopen_txs);
            let reopen_ids_cb = Arc::clone(&reopen_ids);
            let dummy_dir = TempDir::new().unwrap();
            let dummy_root = dummy_dir.path().canonicalize().unwrap();

            let stop = Arc::new(AtomicBool::new(false));
            let stop_loop = Arc::clone(&stop);
            let stream_slot = Arc::new(Mutex::new(None));
            let weak_inner = Arc::downgrade(&coord.inner);
            let manifest_lock = Arc::clone(&coord.manifest_save_lock);
            let vdir = volumes_dir.clone();
            let vid_loop = vid.clone();
            let active_loop = disk_root.clone();
            let mount_loop = disk_root.clone();
            let store_loop = store.clone();
            let filter = CompiledFilter::for_volume_snapshot(&snap, &disk_root);

            let handle = std::thread::spawn(move || {
                run_volume_stream_loop(
                    rx1,
                    stream_slot,
                    stop_loop,
                    weak_inner,
                    manifest_lock,
                    vdir,
                    vid_loop,
                    1,
                    mount_loop,
                    active_loop,
                    store_loop,
                    filter,
                    false,
                    500,
                    Duration::from_millis(25),
                    None,
                    move |since| {
                        reopen_ids_cb.lock().push(since);
                        let (ntx, nrx) = mpsc::channel();
                        reopen_txs_cb.lock().push(ntx);
                        let (sink_tx, _sink_rx) = mpsc::channel();
                        let s = fsevents::watch(&[dummy_root.clone()], fsevents::SINCE_NOW, sink_tx)?;
                        Some((s, nrx))
                    },
                );
            });

            // 1. Inject MOUNT_CHANGED and root ITEM_CREATED | ITEM_IS_DIR -> must be ignored (no full scan!)
            tx1.send(vec![
                fsevents::Event {
                    path: disk_root.clone(),
                    flags: fsevents::MOUNT_CHANGED,
                    id: 501,
                    inode: None,
                },
                fsevents::Event {
                    path: disk_root.clone(),
                    flags: fsevents::ITEM_CREATED | 0x0002_0000, // ItemIsDir
                    id: 502,
                    inode: None,
                },
            ])
            .unwrap();
            std::thread::sleep(Duration::from_millis(250));
            assert_eq!(coord.statuses()[0].state, VolumeState::Online);
            assert_eq!(coord.scan_run_count(), 0);
            assert_eq!(
                store.len(),
                1,
                "root ITEM_CREATED must never trigger dir_creates scan of unindexed_canary.txt"
            );

            // 2. Inject 1st & 2nd USER_DROPPED | MUST_SCAN_SUBDIRS -> silent replay from applied_id (500)
            tx1.send(vec![fsevents::Event {
                path: disk_root.clone(),
                flags: fsevents::USER_DROPPED | fsevents::MUST_SCAN_SUBDIRS,
                id: 503,
                inode: None,
            }])
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline && reopen_txs.lock().is_empty() {
                std::thread::sleep(Duration::from_millis(15));
            }
            assert_eq!(reopen_ids.lock().as_slice(), &[500]);
            assert_eq!(coord.statuses()[0].state, VolumeState::Online);

            // Drop 2, 3, 4 without HISTORY_DONE -> 3 replays allowed, 4th drop -> JournalInvalidated
            for attempt in 1..=3 {
                let latest_tx = reopen_txs.lock().last().unwrap().clone();
                latest_tx
                    .send(vec![fsevents::Event {
                        path: disk_root.clone(),
                        flags: fsevents::USER_DROPPED | fsevents::MUST_SCAN_SUBDIRS,
                        id: 503 + attempt as u64,
                        inode: None,
                    }])
                    .unwrap();
                if attempt < 3 {
                    let dl = Instant::now() + Duration::from_secs(3);
                    while Instant::now() < dl && reopen_txs.lock().len() <= attempt {
                        std::thread::sleep(Duration::from_millis(15));
                    }
                    assert_eq!(coord.statuses()[0].state, VolumeState::Online);
                }
            }

            handle.join().unwrap();
            assert_eq!(coord.scan_run_count(), 0);
            assert_eq!(coord.statuses()[0].state, VolumeState::NeedsConfirmation);
            assert_eq!(
                coord.statuses()[0].confirm_reason,
                Some(VolumeConfirmReason::JournalInvalidated)
            );
            assert_eq!(store.len(), 1, "old cache must remain intact");
        }

        // Test 2: Root MUST_SCAN_SUBDIRS, KERNEL_DROPPED, IDS_WRAPPED on both apfs and exfat -> JournalInvalidated
        for (fstype, flags) in [
            ("apfs", fsevents::MUST_SCAN_SUBDIRS),
            ("exfat", fsevents::MUST_SCAN_SUBDIRS),
            ("apfs", fsevents::KERNEL_DROPPED),
            ("exfat", fsevents::IDS_WRAPPED),
        ] {
            let cache_dir = TempDir::new().unwrap();
            let volumes_dir = cache_dir.path().join("volumes");
            let disk_root = PathBuf::from("/Volumes/DropMatrix");
            let identity = VolumeIdentity::LocalUuid {
                uuid: format!("DROP-{fstype}-{flags}"),
                subpath: String::new(),
            };
            let vid = identity.volume_id();
            let store = IndexStore::new();
            store.replace_compact(
                CompactIndex::from_entries(vec![make_entry(
                    "kept.txt",
                    "/Volumes/DropMatrix/kept.txt",
                    false,
                )])
                .unwrap(),
            );
            let snap = VolumeScopeSnapshot::from_config(&AppConfig::default(), &disk_root, &disk_root);
            let coord = VolumeCoordinator::with_mount_lister(
                cache_dir.path().to_path_buf(),
                Arc::new(base_mounts),
            );
            {
                let mut inner = coord.inner.write();
                inner.slots.push(VolumeSlot {
                    volume_id: vid.clone(),
                    configured_path: "/Volumes/DropMatrix".to_string(),
                    active_configured_path: "/Volumes/DropMatrix".to_string(),
                    active_mount_root: Some("/Volumes/DropMatrix".to_string()),
                    subpath: String::new(),
                    display_name: "DropMatrix".to_string(),
                    kind: VolumeKind::LocalExternal,
                    fstype: fstype.to_string(),
                    from_name: "/dev/disk82s1".to_string(),
                    identity,
                    state: VolumeState::Online,
                    entry_count: 1,
                    last_sync_at: Some(1_700_000_000),
                    last_event_id: Some(700),
                    journal_uuid: Some("UUID-2".to_string()),
                    scope_snapshot: Some(snap.clone()),
                    dirty_upper_bound: false,
                    pending_confirm_reason: None,
                    in_flight_scan_snapshot: None,
                    in_flight_prune_snapshot: None,
                    stream_snapshot: Some(snap.clone()),
                    scanned_count: Arc::new(AtomicU64::new(0)),
                    is_remounted: false,
                    has_cache: true,
                    dirty_cache: false,
                    cache_seq: 0,
                    saved_cache_seq: 0,
                    cache_persistence: CachePersistence::default(),
                    generation: 1,
                    cancel_token: Arc::new(AtomicBool::new(false)),
                    store: store.clone(),
                    stream_handle: None,
                    last_rescan_triggered: None,
                    last_scan_duration: None,
                    next_reconnect_retry: None,
                    unmounting_until: None,
                    skip_missing_rescan: false,
                });
            }

            let (tx, rx) = mpsc::channel();
            tx.send(vec![fsevents::Event {
                path: disk_root.clone(),
                flags,
                id: 701,
                inode: None,
            }])
            .unwrap();
            let filter = CompiledFilter::for_volume_snapshot(&snap, &disk_root);
            run_volume_stream_loop(
                rx,
                Arc::new(Mutex::new(None)),
                Arc::new(AtomicBool::new(false)),
                Arc::downgrade(&coord.inner),
                Arc::clone(&coord.manifest_save_lock),
                volumes_dir,
                vid,
                1,
                disk_root.clone(),
                disk_root,
                store.clone(),
                filter,
                false,
                700,
                Duration::from_millis(20),
                None,
                |_| None,
            );
            assert_eq!(coord.scan_run_count(), 0);
            assert_eq!(coord.statuses()[0].state, VolumeState::NeedsConfirmation);
            assert_eq!(
                coord.statuses()[0].confirm_reason,
                Some(VolumeConfirmReason::JournalInvalidated)
            );
            assert_eq!(store.len(), 1);
        }
    }

    #[test]
    fn exfat_ntfs_apfs_journal_split_matrix() {
        let cases: &[(&str, Option<&str>, Option<&str>, VolumeState, Option<VolumeConfirmReason>, bool)] = &[
            // ExFAT 同挂载会话重启应用（UUID 匹配）-> 历史追赶并保持 Online
            ("exfat", Some("EXFAT-UUID-1"), Some("EXFAT-UUID-1"), VolumeState::Online, None, false),
            // ExFAT 跨关机重启 UUID 轮换 -> 静默更新 UUID、复用旧缓存保持 Online
            ("exfat", Some("EXFAT-UUID-OLD"), Some("EXFAT-UUID-NEW"), VolumeState::Online, None, false),
            // NTFS live_uuid == None -> 复用旧缓存保持 Online
            ("ntfs", None, None, VolumeState::Online, None, false),
            // APFS UUID 不匹配 -> NeedsConfirmation(JournalInvalidated) 且保留旧缓存
            ("apfs", Some("APFS-UUID-OLD"), Some("APFS-UUID-NEW"), VolumeState::NeedsConfirmation, Some(VolumeConfirmReason::JournalInvalidated), true),
        ];

        for (idx, (fstype, saved_uuid, live_uuid, expected_state, expected_reason, expected_persistent)) in
            cases.iter().enumerate()
        {
            let cache_dir = TempDir::new().unwrap();
            let volumes_dir = cache_dir.path().join("volumes");
            let disk_dir = TempDir::new().unwrap();
            let disk_root = disk_dir.path().canonicalize().unwrap();
            let disk_str = disk_root.to_string_lossy().to_string();

            let mut cfg = AppConfig::default();
            cfg.index.index_paths = vec![disk_str.clone()];
            let snap = VolumeScopeSnapshot::from_config(&cfg, &disk_root, &disk_root);

            let identity = VolumeIdentity::LocalUuid {
                uuid: format!("MATRIX-{fstype}-{idx}"),
                subpath: String::new(),
            };
            let rec = VolumeRecord {
                volume_id: identity.volume_id(),
                configured_path: disk_str.clone(),
                active_configured_path: disk_str.clone(),
                active_mount_root: Some(disk_str.clone()),
                subpath: String::new(),
                display_name: format!("Vol{idx}"),
                kind: VolumeKind::LocalExternal,
                fstype: (*fstype).to_string(),
                identity,
                last_sync_at: Some(1_700_000_000),
                last_event_id: Some(fsevents::current_event_id()),
                journal_uuid: saved_uuid.map(str::to_string),
                entry_count: 1,
                scope_snapshot: Some(snap),
                dirty_upper_bound: false,
                pending_confirm_reason: None,
                dirty_filter: false,
                filter_fingerprint: None,
            };
            seed_volume_cache(
                &volumes_dir,
                &rec,
                vec![make_entry("cached.txt", &format!("{disk_str}/cached.txt"), false)],
            );
            let mut manifest = VolumeManifest::default();
            manifest.upsert(rec);
            manifest.save_atomic(&volumes_dir).unwrap();

            let mut mounts = base_mounts();
            mounts.push(MountedFsInfo {
                mount_point: disk_str.clone(),
                fstype: (*fstype).to_string(),
                from_name: format!("/dev/disk9{idx}s1"),
                is_local: true,
                uuid: Some(format!("MATRIX-{fstype}-{idx}")),
                total_bytes: 1_000_000,
                volume_name: format!("Vol{idx}"),
            });
            let live_owned = live_uuid.map(str::to_string);
            let coord = VolumeCoordinator::with_mount_and_journal_provider(
                cache_dir.path().to_path_buf(),
                Arc::new(move || mounts.clone()),
                Arc::new(move |_| live_owned.clone()),
            );

            coord.initialize(&cfg, Arc::new(|| {}));
            assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));

            let st = &coord.statuses()[0];
            assert_eq!(st.state, *expected_state, "case {idx} ({fstype})");
            assert_eq!(st.confirm_reason, *expected_reason, "case {idx} ({fstype})");
            assert_eq!(st.supports_persistent_journal, *expected_persistent);
            assert_eq!(coord.scan_run_count(), 0);
            assert_eq!(st.entry_count, 1, "old cache must remain intact");

            if *fstype == "exfat" && saved_uuid != live_uuid {
                let updated_manifest = VolumeManifest::load_from_dir(&volumes_dir);
                assert_eq!(
                    updated_manifest.records[0].journal_uuid.as_deref(),
                    *live_uuid,
                    "ExFAT rotated UUID must be silently persisted"
                );
            }
        }
    }

    #[test]
    fn needs_confirmation_priority_aba_rollback_and_lossless_cancel() {
        let cache_dir = TempDir::new().unwrap();
        let volumes_dir = cache_dir.path().join("volumes");
        let disk_dir = TempDir::new().unwrap();
        let disk_root = disk_dir.path().canonicalize().unwrap();
        let disk_str = disk_root.to_string_lossy().to_string();
        std::fs::write(disk_root.join("base.txt"), "ok").unwrap();

        let mut base_cfg = AppConfig::default();
        base_cfg.index.index_paths = vec![disk_str.clone()];
        base_cfg.index.include_hidden = false;

        let mut mounts = base_mounts();
        mounts.push(MountedFsInfo {
            mount_point: disk_str.clone(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk85s1".to_string(),
            is_local: true,
            uuid: Some("EXT-ABA-UUID".to_string()),
            total_bytes: 500_000,
            volume_name: "AbaDisk".to_string(),
        });
        let mounts_state = Arc::new(Mutex::new(mounts.clone()));
        let live_journal = Arc::new(Mutex::new(Some("JUUID-A".to_string())));

        // 1. Seed online APFS volume -> widen filter -> NeedsConfirmation(FilterWidened) + live stream + dirty_upper_bound
        let identity = VolumeIdentity::LocalUuid {
            uuid: "EXT-ABA-UUID".to_string(),
            subpath: String::new(),
        };
        let base_snap = VolumeScopeSnapshot::from_config(&base_cfg, &disk_root, &disk_root);
        let init_rec = VolumeRecord {
            volume_id: identity.volume_id(),
            configured_path: disk_str.clone(),
            active_configured_path: disk_str.clone(),
            active_mount_root: Some(disk_str.clone()),
            subpath: String::new(),
            display_name: "AbaDisk".to_string(),
            kind: VolumeKind::LocalExternal,
            fstype: "apfs".to_string(),
            identity,
            last_sync_at: Some(1_700_000_000),
            last_event_id: Some(fsevents::current_event_id()),
            journal_uuid: Some("JUUID-A".to_string()),
            entry_count: 1,
            scope_snapshot: Some(base_snap.clone()),
            dirty_upper_bound: false,
            pending_confirm_reason: None,
            dirty_filter: false,
            filter_fingerprint: None,
        };
        seed_volume_cache(
            &volumes_dir,
            &init_rec,
            vec![make_entry("base.txt", &format!("{disk_str}/base.txt"), false)],
        );
        let mut manifest = VolumeManifest::default();
        manifest.upsert(init_rec.clone());
        manifest.save_atomic(&volumes_dir).unwrap();

        let mut widened_cfg = base_cfg.clone();
        widened_cfg.index.include_hidden = true;

        let primary = IndexStore::new();
        {
            let m_read = Arc::clone(&mounts_state);
            let j_read = Arc::clone(&live_journal);
            let coord = VolumeCoordinator::with_mount_and_journal_provider(
                cache_dir.path().to_path_buf(),
                Arc::new(move || m_read.lock().clone()),
                Arc::new(move |_| j_read.lock().clone()),
            );
            coord.initialize(&base_cfg, Arc::new(|| {}));
            assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
            assert_eq!(coord.statuses()[0].state, VolumeState::Online);

            coord.on_config_updated(&widened_cfg);
            assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
            assert_eq!(coord.scan_run_count(), 0);
            assert_eq!(coord.statuses()[0].state, VolumeState::NeedsConfirmation);
            assert_eq!(
                coord.statuses()[0].confirm_reason,
                Some(VolumeConfirmReason::FilterWidened)
            );
            assert!(coord.inner.read().slots[0].dirty_upper_bound);
            assert!(coord.inner.read().slots[0].stream_handle.is_some());

            // Write a hidden file during deferral -> ingested via widened live stream!
            std::thread::sleep(Duration::from_millis(250));
            std::fs::write(disk_root.join(".deferred_hidden.txt"), "h").unwrap();
            let dl = Instant::now() + Duration::from_secs(8);
            let mut saw_hidden = false;
            while Instant::now() < dl {
                if !coord
                    .search(&primary, ".deferred_hidden.txt", SearchMode::Auto, CaseSensitivity::Auto, 10, false)
                    .0
                    .is_empty()
                {
                    saw_hidden = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            assert!(saw_hidden, "live stream must ingest hidden file during FilterWidened deferral");
            coord.persist_all();
        }

        // 2. Restart with widened_cfg -> stays NeedsConfirmation(FilterWidened) with live stream
        {
            let m_read = Arc::clone(&mounts_state);
            let j_read = Arc::clone(&live_journal);
            let coord = VolumeCoordinator::with_mount_and_journal_provider(
                cache_dir.path().to_path_buf(),
                Arc::new(move || m_read.lock().clone()),
                Arc::new(move |_| j_read.lock().clone()),
            );
            coord.initialize(&widened_cfg, Arc::new(|| {}));
            assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
            assert_eq!(coord.scan_run_count(), 0);
            assert_eq!(coord.statuses()[0].state, VolumeState::NeedsConfirmation);
            assert_eq!(
                coord.statuses()[0].confirm_reason,
                Some(VolumeConfirmReason::FilterWidened)
            );
            assert!(coord.inner.read().slots[0].stream_handle.is_some());

            // Online ABA rollback back to base_cfg -> auto PruneJob evicts .deferred_hidden.txt & restores Online!
            coord.on_config_updated(&base_cfg);
            assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
            assert_eq!(coord.scan_run_count(), 0);
            assert_eq!(coord.prune_run_count(), 1);
            assert_eq!(coord.statuses()[0].state, VolumeState::Online);
            assert!(!coord.inner.read().slots[0].dirty_upper_bound);
            assert!(coord
                .search(&primary, ".deferred_hidden.txt", SearchMode::Auto, CaseSensitivity::Auto, 10, true)
                .0
                .is_empty());

            // Widen again, go offline, rotate journal UUID -> replug: JournalInvalidated overrides FilterWidened & no stream!
            coord.on_config_updated(&widened_cfg);
            assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
            *mounts_state.lock() = base_mounts();
            coord.notify_volume_will_unmount(&disk_root);
            let saved_ev_id = coord.inner.read().slots[0].last_event_id;
            *live_journal.lock() = Some("JUUID-ROTATED".to_string());
            *mounts_state.lock() = mounts.clone();
            coord.notify_volume_mounted(&disk_root);
            assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
            assert_eq!(coord.statuses()[0].state, VolumeState::NeedsConfirmation);
            assert_eq!(
                coord.statuses()[0].confirm_reason,
                Some(VolumeConfirmReason::JournalInvalidated)
            );
            assert!(
                coord.inner.read().slots[0].stream_handle.is_none(),
                "JournalInvalidated must NOT attach live stream"
            );
            assert_eq!(coord.inner.read().slots[0].last_event_id, saved_ev_id);

            // Cancel flow 1: confirm scan on NeedsConfirmation then cancel immediately -> preserves old cache & returns to NeedsConfirmation
            {
                let mut inner = coord.inner.write();
                inner.slots[0].state = VolumeState::Syncing;
            }
            assert_eq!(coord.cancel_volume_scan(&init_rec.volume_id), Ok(true));
            assert_eq!(coord.statuses()[0].state, VolumeState::NeedsConfirmation);
            assert_eq!(
                coord.statuses()[0].confirm_reason,
                Some(VolumeConfirmReason::JournalInvalidated)
            );
            assert_eq!(
                coord
                    .search(&primary, "base.txt", SearchMode::Auto, CaseSensitivity::Auto, 10, false)
                    .0
                    .len(),
                1
            );
        }

        // 3. Cancel flow 2: actively adding an uncached mounted volume enters Syncing; cancel restores NeedsConfirmation(InitialScan)
        let cache_dir2 = TempDir::new().unwrap();
        let disk2 = TempDir::new().unwrap();
        let disk2_root = disk2.path().canonicalize().unwrap();
        let disk2_str = disk2_root.to_string_lossy().to_string();
        let mut mounts2 = base_mounts();
        mounts2.push(MountedFsInfo {
            mount_point: disk2_str.clone(),
            fstype: "apfs".to_string(),
            from_name: "/dev/disk86s1".to_string(),
            is_local: true,
            uuid: Some("EXT-ACTIVE-ADD-CANCEL".to_string()),
            total_bytes: 500_000,
            volume_name: "NewDisk".to_string(),
        });
        let gate = Arc::new((std::sync::Mutex::new(true), std::sync::Condvar::new()));
        let gate_cb = Arc::clone(&gate);
        let main_tid = std::thread::current().id();
        let coord2 = VolumeCoordinator::with_mount_lister(
            cache_dir2.path().to_path_buf(),
            Arc::new(move || {
                if std::thread::current().id() != main_tid {
                    let (lock, cvar) = &*gate_cb;
                    let mut hold = lock.lock().unwrap();
                    while *hold {
                        hold = cvar.wait(hold).unwrap();
                    }
                }
                mounts2.clone()
            }),
        );
        let mut cfg_before = AppConfig::default();
        cfg_before.index.index_paths = vec![];
        coord2.initialize(&cfg_before, Arc::new(|| {}));

        let mut cfg_after = cfg_before.clone();
        cfg_after.index.index_paths = vec![disk2_str.clone()];
        coord2.on_config_updated(&cfg_after);
        assert_eq!(coord2.statuses()[0].state, VolumeState::Syncing);
        assert_eq!(coord2.cancel_volume_scan(&disk2_str), Ok(true));
        {
            let (lock, cvar) = &*gate;
            *lock.lock().unwrap() = false;
            cvar.notify_all();
        }
        assert!(coord2.wait_for_workers_idle(Duration::from_secs(5)));
        assert_eq!(coord2.statuses()[0].state, VolumeState::NeedsConfirmation);
        assert_eq!(
            coord2.statuses()[0].confirm_reason,
            Some(VolumeConfirmReason::InitialScan)
        );
        assert!(!coord2.statuses()[0].has_cache);
    }

    #[test]
    fn legacy_manifest_upgrade_and_corrupt_manifest_boundary_cases() {
        // Case 1: d410c85 manifest with legacy fp(has_fda=false) claimed cleanly even when has_fda=true
        {
            let cache_dir = TempDir::new().unwrap();
            let volumes_dir = cache_dir.path().join("volumes");
            let root_str = "/Volumes/LegacyClaim";
            let mut cfg = AppConfig::default();
            cfg.index.index_paths = vec![root_str.to_string()];
            let legacy_fp_no_fda = legacy_volume_filter_fingerprint(&cfg, false);

            let mut rec = make_network_record(
                root_str,
                root_str,
                None,
                "",
                "LegacyClaim",
                "//nas/LegacyClaim",
                1,
                false,
            );
            rec.filter_fingerprint = Some(legacy_fp_no_fda);
            seed_volume_cache(
                &volumes_dir,
                &rec,
                vec![make_entry("a.txt", "/Volumes/LegacyClaim/a.txt", false)],
            );
            // Serialize legacy JSON explicitly with dirty_filter and filter_fingerprint
            let legacy_json = serde_json::json!({
                "records": [{
                    "volume_id": rec.volume_id,
                    "configured_path": rec.configured_path,
                    "active_configured_path": rec.active_configured_path,
                    "active_mount_root": rec.active_mount_root,
                    "subpath": rec.subpath,
                    "display_name": rec.display_name,
                    "kind": rec.kind,
                    "fstype": rec.fstype,
                    "identity": rec.identity,
                    "last_sync_at": rec.last_sync_at,
                    "last_event_id": rec.last_event_id,
                    "journal_uuid": rec.journal_uuid,
                    "entry_count": rec.entry_count,
                    "dirty_filter": false,
                    "filter_fingerprint": legacy_fp_no_fda,
                    "pruned_for_fp": null
                }]
            });
            std::fs::write(
                volumes_dir.join("manifest.json"),
                serde_json::to_string_pretty(&legacy_json).unwrap(),
            )
            .unwrap();

            let coord = VolumeCoordinator::with_mount_and_fda_lister(
                cache_dir.path().to_path_buf(),
                Arc::new(base_mounts),
                Arc::new(|| true),
            );
            coord.initialize(&cfg, Arc::new(|| {}));
            assert_eq!(coord.scan_run_count(), 0);
            assert_eq!(coord.prune_run_count(), 0);
            let saved = VolumeManifest::load_from_dir(&volumes_dir);
            assert!(saved.records[0].scope_snapshot.is_some());
            assert_eq!(saved.records[0].pending_confirm_reason, None);
            let raw = std::fs::read_to_string(volumes_dir.join("manifest.json")).unwrap();
            assert!(!raw.contains("\"dirty_filter\""));
            assert!(!raw.contains("\"filter_fingerprint\""));
        }

        // Case 2: dirty_filter == false but legacy filter_fingerprint mismatches (offline edit) ->
        // scope_snapshot == None, NeedsConfirmation(FilterWidened), modifying config does NOT trigger ABA rollback!
        {
            let cache_dir = TempDir::new().unwrap();
            let volumes_dir = cache_dir.path().join("volumes");
            let disk_dir = TempDir::new().unwrap();
            let disk_root = disk_dir.path().canonicalize().unwrap();
            let disk_str = disk_root.to_string_lossy().to_string();

            let mut cfg = AppConfig::default();
            cfg.index.index_paths = vec![disk_str.clone()];
            let identity = VolumeIdentity::LocalUuid {
                uuid: "LEGACY-MISMATCH-UUID".to_string(),
                subpath: String::new(),
            };
            let vid = identity.volume_id();
            let rec = VolumeRecord {
                volume_id: vid.clone(),
                configured_path: disk_str.clone(),
                active_configured_path: disk_str.clone(),
                active_mount_root: Some(disk_str.clone()),
                subpath: String::new(),
                display_name: "LegacyMismatch".to_string(),
                kind: VolumeKind::LocalExternal,
                fstype: "exfat".to_string(),
                identity: identity.clone(),
                last_sync_at: Some(1_700_000_000),
                last_event_id: Some(10),
                journal_uuid: Some("EXFAT-J".to_string()),
                entry_count: 1,
                scope_snapshot: None,
                dirty_upper_bound: false,
                pending_confirm_reason: None,
                dirty_filter: false,
                filter_fingerprint: Some(0xDEAD_BEEF_1234_5678),
            };
            seed_volume_cache(
                &volumes_dir,
                &rec,
                vec![make_entry("doc.txt", &format!("{disk_str}/doc.txt"), false)],
            );
            let legacy_json = serde_json::json!({
                "records": [{
                    "volume_id": rec.volume_id,
                    "configured_path": rec.configured_path,
                    "active_configured_path": rec.active_configured_path,
                    "active_mount_root": rec.active_mount_root,
                    "subpath": rec.subpath,
                    "display_name": rec.display_name,
                    "kind": rec.kind,
                    "fstype": rec.fstype,
                    "identity": rec.identity,
                    "last_sync_at": rec.last_sync_at,
                    "last_event_id": rec.last_event_id,
                    "journal_uuid": rec.journal_uuid,
                    "entry_count": rec.entry_count,
                    "dirty_filter": false,
                    "filter_fingerprint": 0xDEAD_BEEF_1234_5678u64
                }]
            });
            std::fs::write(
                volumes_dir.join("manifest.json"),
                serde_json::to_string_pretty(&legacy_json).unwrap(),
            )
            .unwrap();

            let mut mounts = base_mounts();
            mounts.push(MountedFsInfo {
                mount_point: disk_str.clone(),
                fstype: "exfat".to_string(),
                from_name: "/dev/disk88s1".to_string(),
                is_local: true,
                uuid: Some("LEGACY-MISMATCH-UUID".to_string()),
                total_bytes: 500_000,
                volume_name: "LegacyMismatch".to_string(),
            });
            let coord = VolumeCoordinator::with_mount_lister(
                cache_dir.path().to_path_buf(),
                Arc::new(move || mounts.clone()),
            );
            coord.initialize(&cfg, Arc::new(|| {}));
            assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
            assert_eq!(coord.scan_run_count(), 0);
            assert_eq!(coord.statuses()[0].state, VolumeState::NeedsConfirmation);
            assert_eq!(
                coord.statuses()[0].confirm_reason,
                Some(VolumeConfirmReason::FilterWidened)
            );
            assert!(coord.inner.read().slots[0].scope_snapshot.is_none());

            // Modifying config while scope_snapshot == None must NOT trigger ABA rollback
            let mut next_cfg = cfg.clone();
            next_cfg.index.exclude.push("unrelated".to_string());
            coord.on_config_updated(&next_cfg);
            assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
            assert_eq!(coord.prune_run_count(), 0);
            assert_eq!(coord.statuses()[0].state, VolumeState::NeedsConfirmation);
        }

        // Case 3: Corrupt manifest.json with surviving .cache on both APFS and ExFAT ->
        // directly NeedsConfirmation(JournalInvalidated), no live stream, old cache searchable
        for fstype in ["apfs", "exfat"] {
            let cache_dir = TempDir::new().unwrap();
            let volumes_dir = cache_dir.path().join("volumes");
            let disk_dir = TempDir::new().unwrap();
            let disk_root = disk_dir.path().canonicalize().unwrap();
            let disk_str = disk_root.to_string_lossy().to_string();

            let identity = VolumeIdentity::LocalUuid {
                uuid: format!("CORRUPT-MANIFEST-{fstype}"),
                subpath: String::new(),
            };
            let vid = identity.volume_id();
            std::fs::create_dir_all(&volumes_dir).unwrap();
            let compact = CompactIndex::from_entries(vec![make_entry(
                "survivor.txt",
                &format!("{disk_str}/survivor.txt"),
                false,
            )])
            .unwrap();
            cache::save_compact(
                &compact,
                Some(cache::Checkpoint {
                    event_id: 99,
                    fingerprint: 0,
                }),
                &volumes_dir.join(format!("{vid}.cache")),
            )
            .unwrap();
            // Write corrupted JSON as manifest.json
            std::fs::write(volumes_dir.join("manifest.json"), b"{corrupt json").unwrap();

            let mut cfg = AppConfig::default();
            cfg.index.index_paths = vec![disk_str.clone()];
            let mut mounts = base_mounts();
            mounts.push(MountedFsInfo {
                mount_point: disk_str.clone(),
                fstype: fstype.to_string(),
                from_name: "/dev/disk89s1".to_string(),
                is_local: true,
                uuid: Some(format!("CORRUPT-MANIFEST-{fstype}")),
                total_bytes: 500_000,
                volume_name: "CorruptVol".to_string(),
            });
            let coord = VolumeCoordinator::with_mount_lister(
                cache_dir.path().to_path_buf(),
                Arc::new(move || mounts.clone()),
            );
            coord.initialize(&cfg, Arc::new(|| {}));
            assert!(coord.wait_for_workers_idle(Duration::from_secs(5)));
            assert_eq!(coord.scan_run_count(), 0);
            assert_eq!(coord.statuses()[0].state, VolumeState::NeedsConfirmation);
            assert_eq!(
                coord.statuses()[0].confirm_reason,
                Some(VolumeConfirmReason::JournalInvalidated)
            );
            assert!(
                coord.inner.read().slots[0].stream_handle.is_none(),
                "corrupt manifest must not attach live stream even on {fstype}"
            );
            assert_eq!(coord.inner.read().slots[0].last_event_id, Some(99));
            let primary = IndexStore::new();
            assert_eq!(
                coord
                    .search(&primary, "survivor.txt", SearchMode::Auto, CaseSensitivity::Auto, 10, false)
                    .0
                    .len(),
                1
            );
        }
    }

    #[test]
    fn poll_live_journal_uuid_waits_for_delayed_fseventsd_and_honors_cancel() {
        let attempts = Arc::new(AtomicU64::new(0));
        let attempts_cb = Arc::clone(&attempts);
        let provider: JournalUuidProvider = Arc::new(move |_| {
            let n = attempts_cb.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                None
            } else {
                Some("DELAYED-APFS-UUID".to_string())
            }
        });
        let cancel = AtomicBool::new(false);
        let got = poll_live_journal_uuid(
            &provider,
            Path::new("/Volumes/DelayedAPFS"),
            &cancel,
            Duration::from_secs(2),
        );
        assert_eq!(got.as_deref(), Some("DELAYED-APFS-UUID"));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);

        // Cancelled token must abort retry immediately without waiting full timeout.
        let never: JournalUuidProvider = Arc::new(|_| None);
        let cancelled = AtomicBool::new(true);
        let start = Instant::now();
        let none = poll_live_journal_uuid(
            &never,
            Path::new("/Volumes/CancelledAPFS"),
            &cancelled,
            Duration::from_secs(5),
        );
        assert_eq!(none, None);
        assert!(start.elapsed() < Duration::from_millis(200));
    }

    #[test]
    fn aborted_prune_job_clears_in_flight_prune_snapshot_for_re_enqueue() {
        let cache_dir = TempDir::new().unwrap();
        let disk_str = "/Volumes/OfflinePruneAbort".to_string();
        let mut cfg_s0 = AppConfig::default();
        cfg_s0.index.index_paths = vec![disk_str.clone()];
        cfg_s0.index.exclude = vec![];
        cfg_s0.index.include_hidden = true;

        let snap_s0 =
            VolumeScopeSnapshot::from_config(&cfg_s0, Path::new(&disk_str), Path::new(&disk_str));
        let mut cfg_s1 = cfg_s0.clone();
        cfg_s1.index.exclude = vec!["dropme".to_string()];
        let snap_s1 =
            VolumeScopeSnapshot::from_config(&cfg_s1, Path::new(&disk_str), Path::new(&disk_str));
        let mut cfg_s2 = cfg_s0.clone();
        cfg_s2.index.exclude = vec!["other".to_string()];

        let store = IndexStore::new();
        let compact = crate::compact_index::CompactIndex::from_entries(vec![
            make_entry("keep.txt", &format!("{disk_str}/keep.txt"), false),
            make_entry("drop.txt", &format!("{disk_str}/dropme/drop.txt"), false),
        ])
        .unwrap();
        store.replace_compact(compact);

        let identity = VolumeIdentity::LocalUuid {
            uuid: "ABORT-PRUNE-UUID".to_string(),
            subpath: String::new(),
        };
        let vid = identity.volume_id();
        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(base_mounts),
        );
        {
            let mut guard = coord.inner.write();
            guard.config = cfg_s1.clone();
            guard.slots.push(VolumeSlot {
                volume_id: vid.clone(),
                configured_path: disk_str.clone(),
                active_configured_path: disk_str.clone(),
                active_mount_root: None,
                subpath: String::new(),
                display_name: "OfflinePruneAbort".to_string(),
                kind: VolumeKind::LocalExternal,
                fstype: "apfs".to_string(),
                from_name: "/dev/disk91s1".to_string(),
                identity,
                state: VolumeState::Offline,
                entry_count: 2,
                last_sync_at: Some(1_700_000_000),
                last_event_id: Some(10),
                journal_uuid: Some("ABORT-JUUID".to_string()),
                scope_snapshot: Some(snap_s0),
                dirty_upper_bound: false,
                pending_confirm_reason: None,
                in_flight_scan_snapshot: None,
                in_flight_prune_snapshot: None,
                stream_snapshot: None,
                scanned_count: Arc::new(AtomicU64::new(0)),
                is_remounted: false,
                has_cache: true,
                dirty_cache: false,
                cache_seq: 0,
                saved_cache_seq: 0,
                cache_persistence: CachePersistence::default(),
                generation: 1,
                cancel_token: Arc::new(AtomicBool::new(false)),
                store: store.clone(),
                stream_handle: None,
                last_rescan_triggered: None,
                last_scan_duration: None,
                next_reconnect_retry: None,
                unmounting_until: None,
                skip_missing_rescan: false,
            });
        }

        // 1. 入队 S1 裁剪任务
        let mut eff1 = TransitionEffects::default();
        {
            let mut guard = coord.inner.write();
            reconcile_offline_slot_scope(&mut guard.slots[0], &cfg_s1, &mut eff1);
            assert!(guard.slots[0].in_flight_prune_snapshot.is_some());
        }
        let job_s1 = match eff1.tasks.pop() {
            Some(WorkerTask::Prune(job)) => job,
            other => panic!("expected PruneJob, got {:?}", other.is_some()),
        };

        // 2. 在 job_s1 执行前配置改为 S2（live_target != S1），令其命中 !should_prune 早退
        coord.inner.write().config = cfg_s2;
        execute_prune_job(
            &coord.inner,
            &coord.manifest_save_lock,
            &coord.worker_queue,
            &coord.journal_uuid_provider,
            Duration::ZERO,
            &coord.volumes_dir(),
            job_s1,
        );
        assert!(
            coord.inner.read().slots[0]
                .in_flight_prune_snapshot
                .is_none(),
            "aborted prune job must clear in_flight_prune_snapshot so future identical prunes are not deadlocked"
        );

        // 3. 切回 S1 后必须允许重新入队并成功完成裁剪
        let mut eff2 = TransitionEffects::default();
        {
            let mut guard = coord.inner.write();
            guard.config = cfg_s1.clone();
            reconcile_offline_slot_scope(&mut guard.slots[0], &cfg_s1, &mut eff2);
        }
        let job_s1_retry = match eff2.tasks.pop() {
            Some(WorkerTask::Prune(job)) => job,
            _ => panic!("re-enqueue after aborted prune must produce a fresh PruneJob"),
        };
        execute_prune_job(
            &coord.inner,
            &coord.manifest_save_lock,
            &coord.worker_queue,
            &coord.journal_uuid_provider,
            Duration::ZERO,
            &coord.volumes_dir(),
            job_s1_retry,
        );
        let guard = coord.inner.read();
        assert_eq!(guard.slots[0].scope_snapshot.as_ref(), Some(&snap_s1));
        assert!(guard.slots[0].in_flight_prune_snapshot.is_none());
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn subpath_root_move_evicts_subtree_while_whole_volume_root_keeps_cache() {
        let cache_dir = TempDir::new().unwrap();
        let disk_dir = TempDir::new().unwrap();
        let mount_root = disk_dir.path().canonicalize().unwrap();
        let subpath_root = mount_root.join("Work");
        let moved_dest = mount_root.join("WorkMoved");
        std::fs::create_dir_all(&subpath_root).unwrap();
        let file = subpath_root.join("doc.txt");
        std::fs::write(&file, "x").unwrap();

        let cfg = AppConfig::default();
        let snap = VolumeScopeSnapshot::from_config(&cfg, &subpath_root, &subpath_root);
        let store = IndexStore::new();
        let compact = crate::compact_index::CompactIndex::from_entries(vec![
            make_entry("Work", &subpath_root.to_string_lossy(), true),
            make_entry("doc.txt", &file.to_string_lossy(), false),
        ])
        .unwrap();
        store.replace_compact(compact);

        let coord = VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(base_mounts),
        );
        // 移走卷内子路径，卷挂载点仍存在 -> 收到根路径 ITEM_RENAMED 后清理子树
        std::fs::rename(&subpath_root, &moved_dest).unwrap();
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_loop = Arc::clone(&stop);
        let store_loop = store.clone();
        let filter = CompiledFilter::for_volume_snapshot(&snap, &subpath_root);
        let volumes_dir = cache_dir.path().join("volumes");
        let handle = std::thread::spawn(move || {
            run_volume_stream_loop(
                rx,
                Arc::new(Mutex::new(None)),
                stop_loop,
                Arc::downgrade(&coord.inner),
                Arc::clone(&coord.manifest_save_lock),
                volumes_dir,
                "subpath-vid".to_string(),
                1,
                mount_root,
                subpath_root,
                store_loop,
                filter,
                true,
                100,
                Duration::from_millis(20),
                None,
                |_| None,
            );
        });
        tx.send(vec![fsevents::Event {
            path: moved_dest.with_file_name("Work"),
            flags: fsevents::ITEM_RENAMED,
            id: 101,
            inode: None,
        }])
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && !store.is_empty() {
            std::thread::sleep(Duration::from_millis(20));
        }
        stop.store(true, Ordering::SeqCst);
        drop(tx);
        let _ = handle.join();
        assert!(
            store.is_empty(),
            "moved volume subpath root must evict stale subtree entries"
        );
    }

    #[test]
    fn prune_reverted_mid_flight_after_removing_entries_marks_filter_widened_instead_of_losing_files()
    {
        let cache_dir = TempDir::new().unwrap();
        let disk_str = "/Volumes/MidFlightPruneRevert".to_string();
        let mut cfg_o = AppConfig::default();
        cfg_o.index.index_paths = vec![disk_str.clone()];
        cfg_o.index.exclude = vec![];
        cfg_o.index.include_hidden = true;
        let snap_o =
            VolumeScopeSnapshot::from_config(&cfg_o, Path::new(&disk_str), Path::new(&disk_str));

        let mut cfg_a = cfg_o.clone();
        cfg_a.index.exclude = vec!["dropme".to_string()];
        let snap_a =
            VolumeScopeSnapshot::from_config(&cfg_a, Path::new(&disk_str), Path::new(&disk_str));

        let store = IndexStore::new();
        let compact = crate::compact_index::CompactIndex::from_entries(vec![
            make_entry("keep.txt", &format!("{disk_str}/keep.txt"), false),
            make_entry("drop.txt", &format!("{disk_str}/dropme/drop.txt"), false),
        ])
        .unwrap();
        store.replace_compact(compact);

        let identity = VolumeIdentity::LocalUuid {
            uuid: "MID-FLIGHT-PRUNE-UUID".to_string(),
            subpath: String::new(),
        };
        let vid = identity.volume_id();
        let coord = Arc::new(VolumeCoordinator::with_mount_lister(
            cache_dir.path().to_path_buf(),
            Arc::new(base_mounts),
        ));
        {
            let mut guard = coord.inner.write();
            guard.config = cfg_a.clone();
            guard.slots.push(VolumeSlot {
                volume_id: vid.clone(),
                configured_path: disk_str.clone(),
                active_configured_path: disk_str.clone(),
                active_mount_root: None,
                subpath: String::new(),
                display_name: "MidFlightPruneRevert".to_string(),
                kind: VolumeKind::LocalExternal,
                fstype: "apfs".to_string(),
                from_name: "/dev/disk92s1".to_string(),
                identity,
                state: VolumeState::Offline,
                entry_count: 2,
                last_sync_at: Some(1_700_000_000),
                last_event_id: Some(10),
                journal_uuid: Some("MID-FLIGHT-JUUID".to_string()),
                scope_snapshot: Some(snap_o),
                dirty_upper_bound: false,
                pending_confirm_reason: None,
                in_flight_scan_snapshot: None,
                in_flight_prune_snapshot: None,
                stream_snapshot: None,
                scanned_count: Arc::new(AtomicU64::new(0)),
                is_remounted: false,
                has_cache: true,
                dirty_cache: false,
                cache_seq: 0,
                saved_cache_seq: 0,
                cache_persistence: CachePersistence::default(),
                generation: 1,
                cancel_token: Arc::new(AtomicBool::new(false)),
                store: store.clone(),
                stream_handle: None,
                last_rescan_triggered: None,
                last_scan_duration: None,
                next_reconnect_retry: None,
                unmounting_until: None,
                skip_missing_rescan: false,
            });
        }

        let mut eff = TransitionEffects::default();
        {
            let mut guard = coord.inner.write();
            reconcile_offline_slot_scope(&mut guard.slots[0], &cfg_a, &mut eff);
        }
        let job_a = match eff.tasks.pop() {
            Some(WorkerTask::Prune(job)) => job,
            _ => panic!("expected PruneJob"),
        };

        // 持住 store 读锁让 execute_prune_job 通过 should_prune 后停在 retain_paths 写锁前，
        // 期间并发把配置改回原值 O，再释放读锁令其执行完裁剪并进入复核阶段。
        let hold_read = store.read();
        let coord_worker = Arc::clone(&coord);
        let worker = std::thread::spawn(move || {
            execute_prune_job(
                &coord_worker.inner,
                &coord_worker.manifest_save_lock,
                &coord_worker.worker_queue,
                &coord_worker.journal_uuid_provider,
                Duration::ZERO,
                &coord_worker.volumes_dir(),
                job_a,
            );
        });
        std::thread::sleep(Duration::from_millis(60));
        coord.inner.write().config = cfg_o;
        drop(hold_read);
        worker.join().unwrap();

        let guard = coord.inner.read();
        assert_eq!(store.len(), 1);
        assert_eq!(guard.slots[0].entry_count, 1);
        assert_eq!(
            guard.slots[0].scope_snapshot.as_ref(),
            Some(&snap_a),
            "pruned cache must advance scope_snapshot to match RAM and disk"
        );
        assert_eq!(
            guard.slots[0].pending_confirm_reason,
            Some(VolumeConfirmReason::FilterWidened),
            "reverting config back to wider O mid-prune must raise FilterWidened instead of silently losing entries"
        );
        assert!(guard.slots[0].in_flight_prune_snapshot.is_none());
    }
}
