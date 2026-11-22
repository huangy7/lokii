use crate::cache;
use crate::compact_index::CompactIndex;
use crate::config::{AppConfig, ConfigStore, IndexConfig};
use crate::filter::CompiledFilter;
use crate::fsevents;
use crate::indexer::IndexStore;
use crate::path_utils::{path_starts_with_ci, paths_equal_ci};
use crate::scanner::{self, VolumeScanError};
use crate::search::{
    self, CaseSensitivity, MultiSearchResult, QueryFilter, SearchMode, SearchShard,
};
use crate::volumes::{
    list_mounted_volumes, list_mounted_volumes_fast, normalize_network_source,
    partition_index_paths, MountedFsInfo, MountedPathTarget, ResolvedMountMatch, VolumeIdentity,
    VolumeKind, VolumeManifest, VolumeRecord,
};
use crate::watcher::{
    self, apply_batch, resolve_changes, Change, DEFAULT_DEBOUNCE_MS, MAX_BATCH_SIZE, MAX_DEBOUNCE,
    PERSIST_EVENTS, PERSIST_EVERY,
};
use parking_lot::{Condvar, Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolumeState {
    Online,
    Syncing,
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
}

struct VolumeStreamHandle {
    stream: Mutex<Option<fsevents::Stream>>,
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
    identity: VolumeIdentity,
    state: VolumeState,
    entry_count: u64,
    last_sync_at: Option<u64>,
    last_event_id: Option<u64>,
    journal_uuid: Option<String>,
    dirty_filter: bool,
    scan_pending_filter_update: bool,
    is_remounted: bool,
    has_cache: bool,
    dirty_cache: bool,
    cache_seq: u64,
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
            dirty_filter: self.dirty_filter,
        }
    }

    fn status_info(&self) -> VolumeStatusInfo {
        let active_path = match self.state {
            VolumeState::Online | VolumeState::Syncing | VolumeState::Missing => {
                Some(self.active_configured_path.clone())
            }
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
        }
    }

    fn checkpoint(&self) -> Option<cache::Checkpoint> {
        self.last_event_id.map(|event_id| cache::Checkpoint {
            event_id,
            fingerprint: 0,
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
}

struct CoordinatorInner {
    config: AppConfig,
    manifest: VolumeManifest,
    slots: Vec<VolumeSlot>,
    on_status_change: Option<Arc<dyn Fn() + Send + Sync>>,
}

struct ScanJob {
    volume_id: String,
    generation: u64,
    kind: VolumeKind,
    root: PathBuf,
    config: AppConfig,
    cancel: Arc<AtomicBool>,
}

struct AttachStreamJob {
    volume_id: String,
    generation: u64,
    active_path: PathBuf,
    since_event_id: Option<u64>,
    stored_journal_uuid: Option<String>,
    require_journal_verify: bool,
    config: AppConfig,
    cancel: Arc<AtomicBool>,
    store: IndexStore,
}

enum WorkerTask {
    Scan(ScanJob),
    AttachStream(AttachStreamJob),
}

impl WorkerTask {
    fn volume_id(&self) -> &str {
        match self {
            WorkerTask::Scan(job) => &job.volume_id,
            WorkerTask::AttachStream(job) => &job.volume_id,
        }
    }

    fn generation(&self) -> u64 {
        match self {
            WorkerTask::Scan(job) => job.generation,
            WorkerTask::AttachStream(job) => job.generation,
        }
    }

    fn is_cancelled(&self) -> bool {
        match self {
            WorkerTask::Scan(job) => job.cancel.load(Ordering::SeqCst),
            WorkerTask::AttachStream(job) => job.cancel.load(Ordering::SeqCst),
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

#[derive(Default)]
struct TransitionEffects {
    handles_to_drop: Vec<VolumeStreamHandle>,
    files_to_delete: Vec<PathBuf>,
    cache_saves: Vec<CacheSave>,
    tasks: Vec<WorkerTask>,
    save_manifest: bool,
    notify: Option<Arc<dyn Fn() + Send + Sync>>,
}

fn save_dirty_caches_and_mark(
    inner: &Arc<RwLock<CoordinatorInner>>,
    saves: Vec<CacheSave>,
) -> HashSet<String> {
    let mut saved_ids = HashSet::new();
    for save in saves {
        if cache::save_compact(&save.store.read(), save.checkpoint, &save.cache_path).is_ok() {
            saved_ids.insert(save.volume_id.clone());
            let mut guard = inner.write();
            if let Some(slot) = guard.slots.iter_mut().find(|s| {
                s.volume_id == save.volume_id && s.generation == save.generation
            }) {
                slot.cache_persistence.saved_events =
                    slot.cache_persistence.saved_events.max(save.applied_events);
                slot.cache_persistence.last_save = Instant::now();
                if slot.cache_seq == save.cache_seq {
                    slot.dirty_cache = false;
                }
            }
        }
    }
    saved_ids
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
        for path in self.files_to_delete {
            let _ = std::fs::remove_file(path);
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
    worker_queue: Arc<WorkerQueue>,
    worker_threads: Mutex<Vec<JoinHandle<()>>>,
    monitor_stop: Arc<(Mutex<bool>, Condvar)>,
    monitor_thread: Mutex<Option<JoinHandle<()>>>,
}

impl VolumeCoordinator {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self::with_mount_lister_impl(
            cache_dir,
            Arc::new(list_mounted_volumes),
            Arc::new(list_mounted_volumes_fast),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_mount_lister(
        cache_dir: PathBuf,
        mount_lister: Arc<dyn Fn() -> Vec<MountedFsInfo> + Send + Sync>,
    ) -> Self {
        let fast_mount_lister = Arc::clone(&mount_lister);
        Self::with_mount_lister_impl(cache_dir, mount_lister, fast_mount_lister)
    }

    fn with_mount_lister_impl(
        cache_dir: PathBuf,
        mount_lister: Arc<dyn Fn() -> Vec<MountedFsInfo> + Send + Sync>,
        fast_mount_lister: Arc<dyn Fn() -> Vec<MountedFsInfo> + Send + Sync>,
    ) -> Self {
        let inner = Arc::new(RwLock::new(CoordinatorInner {
            config: AppConfig::default(),
            manifest: VolumeManifest::default(),
            slots: Vec::new(),
            on_status_change: None,
        }));
        let worker_queue = Arc::new(WorkerQueue::new());
        Self {
            cache_dir,
            inner,
            manifest_save_lock: Arc::new(Mutex::new(())),
            mount_lister,
            fast_mount_lister,
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
            let volumes_dir = self.volumes_dir();
            let handle = std::thread::Builder::new()
                .name(format!("volume-worker-{index}"))
                .spawn(move || {
                    run_worker_loop(inner, manifest_save_lock, queue, mount_lister, volumes_dir)
                })
                .expect("spawn volume-worker");
            guard.push(handle);
        }
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

            let loaded = reconcile_partition_locked(
                &mut inner,
                &partition.mounted_targets,
                &mounts,
                &volumes_dir,
                &mut preloaded,
                false,
                &mut effects,
            );
            (loaded, effects)
        };

        if !effects.tasks.is_empty() {
            self.ensure_worker_started();
        }
        effects.execute(
            &self.inner,
            &self.manifest_save_lock,
            &volumes_dir,
            &self.worker_queue,
        );
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
        let worker_queue = Arc::clone(&self.worker_queue);
        let volumes_dir = self.volumes_dir();
        let stop = Arc::clone(&self.monitor_stop);
        // Start worker thread unconditionally when monitor is active so monitor-detected mounts can enqueue scans.
        self.ensure_worker_started();

        let handle = std::thread::Builder::new()
            .name("volume-monitor".to_string())
            .spawn(move || {
                let (lock, cvar) = &*stop;
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

                    let (cfg_changed, rescan_minutes, cfg_opt) = {
                        let guard = config_store.read();
                        let changed = last_index_cfg.as_ref() != Some(&guard.index);
                        let rescan_mins = guard.index.network_rescan_minutes;
                        let cfg = if changed { Some(guard.clone()) } else { None };
                        (changed, rescan_mins, cfg)
                    };

                    let mounts = (mount_lister)();
                    let mounts_changed = last_mounts.as_ref() != Some(&mounts);

                    if cfg_changed {
                        let cfg = cfg_opt.unwrap_or_else(|| config_store.read().clone());
                        last_index_cfg = Some(cfg.index.clone());
                        last_mounts = Some(mounts.clone());
                        // 配置变更必须走完整 on_config_updated 对账（含过滤器 diff、移除槽位回收），
                        // 严禁走 refresh_mounts_locked 直接覆写 guard.config 吞掉 dirty_filter。
                        let effects = apply_config_update(
                            &inner,
                            &mounts,
                            &volumes_dir,
                            &cfg,
                        );
                        effects.execute(&inner, &manifest_save_lock, &volumes_dir, &worker_queue);
                    } else {
                        let now = Instant::now();
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

                        // 挂载表未变化且无卸载宽限期或可恢复离线卷时直接跳过全量对账，零克隆、零写锁
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

                    // Periodic rescan for online network volumes when configured.
                    if rescan_minutes > 0 {
                        let base_interval = Duration::from_secs(rescan_minutes * 60);
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
                                for slot in &mut guard.slots {
                                    if network_slot_rescan_due(slot, base_interval, now_unix) {
                                        prepare_slot_for_scan(slot, &cfg_snap, &mut eff);
                                        changed = true;
                                    }
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
        if !effects.tasks.is_empty() {
            self.ensure_worker_started();
        }
        effects.execute(
            &self.inner,
            &self.manifest_save_lock,
            &volumes_dir,
            &self.worker_queue,
        );
    }

    pub fn notify_volume_will_unmount(&self, mount_path: &Path) {
        let volumes_dir = self.volumes_dir();
        let unmount_deadline = Instant::now() + UNMOUNT_GRACE_PERIOD;
        let effects = {
            let mut inner = self.inner.write();
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
        effects.execute(
            &self.inner,
            &self.manifest_save_lock,
            &volumes_dir,
            &self.worker_queue,
        );
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
        if !effects.tasks.is_empty() {
            self.ensure_worker_started();
        }
        effects.execute(
            &self.inner,
            &self.manifest_save_lock,
            &volumes_dir,
            &self.worker_queue,
        );
    }

    pub fn on_config_updated(&self, new_config: &AppConfig) {
        let volumes_dir = self.volumes_dir();
        let mounts = (self.mount_lister)();
        let effects = apply_config_update(&self.inner, &mounts, &volumes_dir, new_config);
        if !effects.tasks.is_empty() {
            self.ensure_worker_started();
        }
        effects.execute(
            &self.inner,
            &self.manifest_save_lock,
            &volumes_dir,
            &self.worker_queue,
        );
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
                effects
                    .files_to_delete
                    .push(volumes_dir.join(format!("{}.cache", removed_rec.volume_id)));
            }
            for vid in &matched_ids {
                effects
                    .files_to_delete
                    .push(volumes_dir.join(format!("{vid}.cache")));
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

        effects.execute(
            &self.inner,
            &self.manifest_save_lock,
            &volumes_dir,
            &self.worker_queue,
        );
        Ok(removed)
    }

    pub fn rescan_volume(&self, path_or_id: &str) -> Result<bool, String> {
        let volumes_dir = self.volumes_dir();
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

            let matched_mount = resolve_slot_mount(slot, &mounts);
            let Some(resolved) = matched_mount else {
                return Ok(false);
            };

            let new_vid = resolved.identity.volume_id();
            if slot.volume_id != new_vid {
                reset_slot_for_volume_swap(slot, &resolved, &manifest_snap, None, &mut effects);
            } else {
                slot.identity = resolved.identity;
                slot.kind = resolved.kind;
                slot.fstype = resolved.fstype;
                slot.subpath = resolved.subpath;
                slot.display_name = resolved.display_name;
            }

            let mount_root_str = resolved.mount_root.to_string_lossy().to_string();
            let active_path_str = join_mount_subpath(&resolved.mount_root, &slot.subpath);
            let is_remounted = !str_paths_equal_ci(&active_path_str, &slot.configured_path);

            slot.unmounting_until = None;
            slot.next_reconnect_retry = None;
            slot.active_mount_root = Some(mount_root_str);
            slot.active_configured_path = active_path_str;
            slot.is_remounted = is_remounted;
            prepare_slot_for_scan(slot, &cfg_snap, &mut effects);
            effects.notify = inner.on_status_change.clone();
            (true, effects)
        };

        if !effects.tasks.is_empty() {
            self.ensure_worker_started();
        }
        effects.execute(
            &self.inner,
            &self.manifest_save_lock,
            &volumes_dir,
            &self.worker_queue,
        );
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
                    dirty_records.push(slot.to_record());
                } else {
                    clean_records.push(slot.to_record());
                }
            }
            (saves, clean_records, dirty_records)
        };

        // 先落盘 .cache，仅将未脏或 .cache 确实落盘成功的槽位记录写入 manifest，
        // 避免 manifest.last_event_id 超前于磁盘 .cache 快照导致下次启动漏重放 FSEvents 事件。
        let saved_ids = save_dirty_caches_and_mark(&self.inner, to_save);
        {
            let mut inner = self.inner.write();
            for rec in clean_records {
                inner.manifest.upsert(rec);
            }
            for rec in dirty_records {
                if saved_ids.contains(&rec.volume_id) {
                    inner.manifest.upsert(rec);
                }
            }
        }
        save_latest_manifest_locked(&self.inner, &self.manifest_save_lock, &volumes_dir);
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
            },
        },
    };
    (kind, identity)
}

fn match_mount_for_identity<'a>(
    identity: &VolumeIdentity,
    mounts: &'a [MountedFsInfo],
) -> Option<&'a MountedFsInfo> {
    mounts.iter().find(|m| {
        if crate::volumes::is_system_mount_point(&m.mount_point) {
            return false;
        }
        match identity {
            VolumeIdentity::LocalUuid { uuid, .. } => {
                m.is_local && m.uuid.as_deref().is_some_and(|u| u.eq_ignore_ascii_case(uuid))
            }
            VolumeIdentity::LocalFallback {
                fstype,
                source,
                total_bytes,
                volume_name,
                ..
            } => {
                m.is_local
                    && m.fstype.eq_ignore_ascii_case(fstype)
                    && m.from_name == *source
                    && m.total_bytes == *total_bytes
                    && m.volume_name == *volume_name
            }
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
    let (kind, matched_identity) = build_identity_for_mount(m, subpath);
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
        identity: matched_identity,
    })
}

fn cancel_slot_work(slot: &mut VolumeSlot, effects: &mut TransitionEffects) {
    slot.cancel_token.store(true, Ordering::SeqCst);
    slot.generation = slot.generation.wrapping_add(1);
    if let Some(handle) = slot.stream_handle.take() {
        effects.handles_to_drop.push(handle);
    }
}

fn prepare_slot_for_scan(
    slot: &mut VolumeSlot,
    config: &AppConfig,
    effects: &mut TransitionEffects,
) {
    cancel_slot_work(slot, effects);
    slot.cancel_token = Arc::new(AtomicBool::new(false));
    slot.state = VolumeState::Syncing;
    slot.scan_pending_filter_update = false;
    slot.skip_missing_rescan = false;
    slot.last_rescan_triggered = Some(Instant::now());
    effects.tasks.push(WorkerTask::Scan(ScanJob {
        volume_id: slot.volume_id.clone(),
        generation: slot.generation,
        kind: slot.kind,
        root: PathBuf::from(&slot.active_configured_path),
        config: config.clone(),
        cancel: Arc::clone(&slot.cancel_token),
    }));
}

fn reset_slot_for_volume_swap(
    slot: &mut VolumeSlot,
    res: &ResolvedMountMatch,
    manifest: &VolumeManifest,
    preloaded: Option<PreloadedCache>,
    effects: &mut TransitionEffects,
) -> u64 {
    cancel_slot_work(slot, effects);
    slot.cancel_token = Arc::new(AtomicBool::new(false));

    let new_vid = res.identity.volume_id();
    let active_str = join_mount_subpath(&res.mount_root, &res.subpath);
    slot.volume_id = new_vid.clone();
    slot.identity = res.identity.clone();
    slot.kind = res.kind;
    slot.fstype = res.fstype.clone();
    slot.subpath = res.subpath.clone();
    slot.display_name = res.display_name.clone();
    slot.active_configured_path = active_str;
    slot.active_mount_root = None;
    slot.store = IndexStore::new();
    slot.entry_count = 0;
    slot.has_cache = false;
    slot.dirty_cache = false;
    slot.cache_seq = slot.cache_seq.wrapping_add(1);
    slot.cache_persistence = CachePersistence::default();
    slot.dirty_filter = false;
    slot.scan_pending_filter_update = false;
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
        slot.last_event_id = loaded.checkpoint.map(|cp| cp.event_id);
        if let Some(rec) = manifest.records.iter().find(|r| r.volume_id == new_vid) {
            slot.active_configured_path = rec.active_configured_path.clone();
            slot.last_sync_at = rec.last_sync_at;
            slot.journal_uuid = rec.journal_uuid.clone();
            slot.dirty_filter = rec.dirty_filter;
        }
        return count;
    }
    0
}

fn resolve_slot_mount(slot: &VolumeSlot, mounts: &[MountedFsInfo]) -> Option<ResolvedMountMatch> {
    if let Some(m) = match_mount_for_identity(&slot.identity, mounts) {
        let (kind, identity) = build_identity_for_mount(m, &slot.subpath);
        return Some(ResolvedMountMatch {
            mount_root: PathBuf::from(&m.mount_point),
            subpath: slot.subpath.clone(),
            display_name: slot.display_name.clone(),
            kind,
            fstype: m.fstype.clone(),
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
        identity,
    })
}

fn reconcile_partition_locked(
    inner: &mut CoordinatorInner,
    targets: &[MountedPathTarget],
    mounts: &[MountedFsInfo],
    volumes_dir: &Path,
    preloaded: &mut HashMap<String, PreloadedCache>,
    force_retry_missing: bool,
    effects: &mut TransitionEffects,
) -> u64 {
    let mut loaded_total = 0u64;
    let mut manifest_changed = false;
    let mut status_changed = false;

    for target in targets {
        let configured_str = target.configured_path.to_string_lossy().to_string();
        let manifest_record = inner.manifest.find_by_path(&configured_str).cloned();

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
                }
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
                identity: rec.identity,
                state: VolumeState::Offline,
                entry_count: store.len() as u64,
                last_sync_at: rec.last_sync_at,
                // 以 .cache 内嵌检查点为准，防止 manifest 超前导致漏重放 FSEvents 事件
                last_event_id: if has_cache { loaded_event_id.or(rec.last_event_id) } else { rec.last_event_id },
                journal_uuid: rec.journal_uuid,
                dirty_filter: rec.dirty_filter,
                scan_pending_filter_update: false,
                is_remounted: false,
                has_cache,
                dirty_cache: false,
                cache_seq: 0,
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
            if let Some(loaded) = preloaded.remove(&vid) {
                let count = loaded.compact.len() as u64;
                store.replace_compact(loaded.compact);
                loaded_total += count;
                has_cache = true;
                last_event_id = loaded.checkpoint.map(|cp| cp.event_id);
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
                identity: res.identity.clone(),
                state: VolumeState::Offline,
                entry_count: store.len() as u64,
                last_sync_at: None,
                last_event_id,
                journal_uuid: None,
                dirty_filter: false,
                scan_pending_filter_update: false,
                is_remounted: false,
                has_cache,
                dirty_cache: false,
                cache_seq: 0,
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
                identity: pending_id,
                state: VolumeState::Offline,
                entry_count: 0,
                last_sync_at: None,
                last_event_id: None,
                journal_uuid: None,
                dirty_filter: false,
                scan_pending_filter_update: false,
                is_remounted: false,
                has_cache: false,
                dirty_cache: false,
                cache_seq: 0,
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
                } else {
                    slot.active_configured_path = new_active_path.clone();
                    slot.is_remounted = is_remounted;
                    if !slot.dirty_filter {
                        slot.dirty_cache = true;
                        slot.cache_seq = slot.cache_seq.wrapping_add(1);
                        effects.cache_saves.push(slot.cache_save(volumes_dir));
                    }
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

            let already_syncing_same = !swapped
                && prev_state == VolumeState::Syncing
                && prev_active.as_deref() == Some(mount_root_str.as_str())
                && !slot.scan_pending_filter_update;

            if same_missing_mount {
                // Keep Missing until explicit rescan, mount, or filter update rather than spinning every poll.
                slot.state = VolumeState::Missing;
            } else if already_syncing_same {
                // 同一挂载根上仍有在途扫描任务（含手动重扫与网络卷周期重扫）时保持 Syncing，
                // 避免监控轮询因为已有旧缓存而把扫描中的槽位提前覆写回 Online。
                slot.state = VolumeState::Syncing;
            } else if slot.dirty_filter || !slot.has_cache {
                prepare_slot_for_scan(&mut slot, &inner.config, effects);
            } else {
                slot.state = VolumeState::Online;
                if !matches!(slot.identity, VolumeIdentity::PendingPath { .. }) {
                    inner.manifest.upsert(slot.to_record());
                    manifest_changed = manifest_changed
                        || swapped
                        || path_shifted
                        || meta_changed
                        || prev_state != VolumeState::Online;
                }
                if slot.kind == VolumeKind::LocalExternal && slot.stream_handle.is_none() {
                    effects.tasks.push(WorkerTask::AttachStream(AttachStreamJob {
                        volume_id: slot.volume_id.clone(),
                        generation: slot.generation,
                        active_path: PathBuf::from(&new_active_path),
                        since_event_id: slot.last_event_id,
                        stored_journal_uuid: slot.journal_uuid.clone(),
                        require_journal_verify: true,
                        config: inner.config.clone(),
                        cancel: Arc::clone(&slot.cancel_token),
                        store: slot.store.clone(),
                    }));
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
            slot.entry_count = slot.store.len() as u64;
            if prev_state != VolumeState::Offline && !matches!(slot.identity, VolumeIdentity::PendingPath { .. }) {
                if slot.dirty_cache {
                    effects.cache_saves.push(slot.cache_save(volumes_dir));
                }
                inner.manifest.upsert(slot.to_record());
                manifest_changed = true;
            }
        }

        if slot.state != prev_state || slot.active_mount_root != prev_active {
            status_changed = true;
        }
        inner.slots.push(slot);
    }

    if manifest_changed {
        effects.save_manifest = true;
    }
    if status_changed {
        effects.notify = inner.on_status_change.clone();
    }
    loaded_total
}

/// 统一处理索引配置变更对卷协调器的影响：比对过滤器规则变更、回收已移除卷槽位与缓存文件、更新 `inner.config` 并对账分片。
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
    let filter_changed = filter_rules_differ(&guard.config.index, &new_config.index);

    let mut manifest_dirty = false;
    if filter_changed {
        for slot in &mut guard.slots {
            slot.dirty_filter = true;
            slot.scan_pending_filter_update = true;
            slot.skip_missing_rescan = false;
        }
        for rec in &mut guard.manifest.records {
            rec.dirty_filter = true;
            manifest_dirty = true;
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
        effects
            .files_to_delete
            .push(volumes_dir.join(format!("{vid}.cache")));
        if guard.manifest.remove_by_path_or_id(vid).is_some() {
            manifest_dirty = true;
        }
    }
    for p in &removed_paths {
        if let Some(removed_rec) = guard.manifest.remove_by_path_or_id(p) {
            effects
                .files_to_delete
                .push(volumes_dir.join(format!("{}.cache", removed_rec.volume_id)));
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
        &mut effects,
    );

    if manifest_dirty {
        effects.save_manifest = true;
    }
    if !removed_ids.is_empty() || filter_changed {
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
    reconcile_partition_locked(
        &mut guard,
        &partition.mounted_targets,
        mounts,
        volumes_dir,
        &mut preloaded,
        force_retry_missing,
        &mut effects,
    );
    effects
}

fn run_worker_loop(
    inner: Arc<RwLock<CoordinatorInner>>,
    manifest_save_lock: Arc<Mutex<()>>,
    queue: Arc<WorkerQueue>,
    mount_lister: Arc<dyn Fn() -> Vec<MountedFsInfo> + Send + Sync>,
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
                    &volumes_dir,
                    job,
                );
            }
            WorkerTask::AttachStream(job) => {
                attach_stream_for_job(&inner, &queue, job);
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
            let _ = std::fs::remove_file(cache_path);
            ScanFinalizeResult::Aborted
        }
        Phase3State::Aborted => ScanFinalizeResult::Aborted,
    }
}

fn execute_scan_job(
    inner: &Arc<RwLock<CoordinatorInner>>,
    manifest_save_lock: &Mutex<()>,
    queue: &Arc<WorkerQueue>,
    mount_lister: &Arc<dyn Fn() -> Vec<MountedFsInfo> + Send + Sync>,
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
        fsevents::journal_uuid_for_path(&job.root)
    } else {
        None
    };

    let filter = CompiledFilter::for_volume(&job.config, &job.root);
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
    let mut progress = |_count: u64| {};
    let scan_result = scanner::scan_volume_compact_atomic(
        &job.root,
        &filter,
        &job.cancel,
        &is_mounted,
        &mut progress,
    );

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
                slot.store.replace_compact(compact);
                slot.dirty_filter = false;
                slot.scan_pending_filter_update = false;
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
                    queue,
                    AttachStreamJob {
                        volume_id: job.volume_id,
                        generation: job.generation,
                        active_path: job.root,
                        since_event_id: checkpoint.map(|c| c.event_id),
                        stored_journal_uuid: journal_uuid,
                        require_journal_verify: false,
                        config: job.config,
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
            let (dropped_handle, pending_save, notify) = {
                let mut guard = inner.write();
                let Some(slot) = guard.slots.iter_mut().find(|s| {
                    s.volume_id == job.volume_id
                        && s.generation == job.generation
                        && !s.cancel_token.load(Ordering::SeqCst)
                }) else {
                    return;
                };
                let h = slot.stream_handle.take();
                slot.generation = slot.generation.wrapping_add(1);
                slot.state = VolumeState::Offline;
                slot.active_mount_root = None;
                slot.is_remounted = false;
                slot.skip_missing_rescan = false;
                // 扫描期间遇到临时断连时设置 30 秒退避重试，避免网络恢复后永远停留在 Offline，也避免死挂载高频打扰
                slot.next_reconnect_retry = Some(Instant::now() + Duration::from_secs(30));
                slot.entry_count = slot.store.len() as u64;
                let save = (slot.dirty_cache && !matches!(slot.identity, VolumeIdentity::PendingPath { .. }))
                    .then(|| slot.cache_save(volumes_dir));
                let rec = (!matches!(slot.identity, VolumeIdentity::PendingPath { .. }))
                    .then(|| slot.to_record());
                if let Some(rec) = rec {
                    guard.manifest.upsert(rec);
                }
                (h, save, guard.on_status_change.clone())
            };
            drop(dropped_handle);
            if let Some(save) = pending_save {
                save_dirty_caches_and_mark(inner, vec![save]);
            }
            save_latest_manifest_locked(inner, manifest_save_lock, volumes_dir);
            if let Some(cb) = notify {
                cb();
            }
        }
        Err(VolumeScanError::RootUnavailable(_)) | Err(VolumeScanError::IndexLimit(_)) => {
            if job.cancel.load(Ordering::SeqCst) {
                return;
            }
            let (stale_handle, notify) = {
                let mut guard = inner.write();
                let Some(slot) = guard.slots.iter_mut().find(|s| {
                    s.volume_id == job.volume_id
                        && s.generation == job.generation
                        && !s.cancel_token.load(Ordering::SeqCst)
                }) else {
                    return;
                };
                let h = slot.stream_handle.take();
                slot.state = VolumeState::Missing;
                slot.skip_missing_rescan = true;
                (h, guard.on_status_change.clone())
            };
            drop(stale_handle);
            if let Some(cb) = notify {
                cb();
            }
        }
    }
}

fn attach_stream_for_job(
    inner: &Arc<RwLock<CoordinatorInner>>,
    queue: &Arc<WorkerQueue>,
    job: AttachStreamJob,
) {
    if job.cancel.load(Ordering::SeqCst) {
        return;
    }
    let Some(live_uuid) = fsevents::journal_uuid_for_path(&job.active_path) else {
        if job.require_journal_verify {
            enqueue_rescan_from_stream(inner, queue, &job.volume_id, job.generation);
        }
        return;
    };

    // When attaching to a pre-existing cache, prove the FSEvents journal UUID matches
    // and our checkpoint event ID is valid; otherwise fall back to a clean rescan.
    if let Some(ref saved_uuid) = job.stored_journal_uuid {
        if *saved_uuid != live_uuid {
            enqueue_rescan_from_stream(inner, queue, &job.volume_id, job.generation);
            return;
        }
    } else if job.require_journal_verify {
        enqueue_rescan_from_stream(inner, queue, &job.volume_id, job.generation);
        return;
    }

    let now_id = fsevents::current_event_id();
    let since = match (
        job.stored_journal_uuid.as_ref(),
        job.since_event_id,
        job.require_journal_verify,
    ) {
        (Some(_), Some(id), _) if id > 0 && id <= now_id => id,
        (Some(_), Some(_), _) => {
            enqueue_rescan_from_stream(inner, queue, &job.volume_id, job.generation);
            return;
        }
        (_, _, true) => {
            enqueue_rescan_from_stream(inner, queue, &job.volume_id, job.generation);
            return;
        }
        (_, Some(id), false) if id > 0 && id <= now_id => id,
        (_, _, false) => now_id,
    };

    let (tx, rx) = mpsc::channel();
    let Some(stream) = fsevents::watch(&[job.active_path.clone()], since, tx) else {
        if job.require_journal_verify {
            enqueue_rescan_from_stream(inner, queue, &job.volume_id, job.generation);
        }
        return;
    };

    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    let weak_inner = Arc::downgrade(inner);
    let queue_clone = Arc::clone(queue);
    let volume_id = job.volume_id.clone();
    let generation = job.generation;
    let active_root = job.active_path.clone();
    let filter = CompiledFilter::for_volume(&job.config, &job.active_path);
    let include_hidden = job.config.index.include_hidden;
    let store = job.store.clone();
    let notify = inner.read().on_status_change.clone();

    let thread = std::thread::Builder::new()
        .name(format!("vol-fs-{}", &volume_id[..volume_id.len().min(8)]))
        .spawn(move || {
            run_volume_stream_loop(
                rx,
                stop_flag,
                weak_inner,
                queue_clone,
                volume_id,
                generation,
                active_root,
                store,
                filter,
                include_hidden,
                since,
                notify,
            );
        })
        .ok();

    let handle = VolumeStreamHandle {
        stream: Mutex::new(Some(stream)),
        stop,
        thread,
    };

    let stale = {
        let mut guard = inner.write();
        if let Some(slot) = guard.slots.iter_mut().find(|s| {
            s.volume_id == job.volume_id
                && s.generation == job.generation
                && s.state == VolumeState::Online
                && !s.cancel_token.load(Ordering::SeqCst)
        }) {
            slot.journal_uuid = Some(live_uuid);
            slot.stream_handle.replace(handle)
        } else {
            Some(handle)
        }
    };
    drop(stale);
}

#[allow(clippy::too_many_arguments)]
fn run_volume_stream_loop(
    rx: mpsc::Receiver<Vec<fsevents::Event>>,
    stop: Arc<AtomicBool>,
    weak_inner: Weak<RwLock<CoordinatorInner>>,
    queue: Arc<WorkerQueue>,
    volume_id: String,
    generation: u64,
    active_root: PathBuf,
    store: IndexStore,
    filter: CompiledFilter,
    include_hidden: bool,
    initial_seen: u64,
    notify: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    let mut pending: HashMap<PathBuf, Change> = HashMap::new();
    let mut pending_events = 0usize;
    let mut first_pending: Option<Instant> = None;
    let mut seen = initial_seen;
    let mut need_rescan = false;

    while !stop.load(Ordering::Acquire) {
        let timeout = match first_pending {
            Some(first) => Duration::from_millis(DEFAULT_DEBOUNCE_MS)
                .min(MAX_DEBOUNCE.saturating_sub(first.elapsed())),
            None => Duration::from_millis(DEFAULT_DEBOUNCE_MS),
        };
        let quiet = match rx.recv_timeout(timeout) {
            Ok(batch) => {
                for ev in batch {
                    if ev.flags & fsevents::HISTORY_DONE != 0 {
                        continue;
                    }
                    seen = seen.max(ev.id);
                    if ev.flags
                        & (fsevents::IDS_WRAPPED | fsevents::KERNEL_DROPPED | fsevents::USER_DROPPED)
                        != 0
                    {
                        need_rescan = true;
                        break;
                    }
                    if ev.flags & (fsevents::MUST_SCAN_SUBDIRS | fsevents::MOUNT_CHANGED) != 0 {
                        if path_starts_with_ci(&active_root, &ev.path) {
                            need_rescan = true;
                            break;
                        }
                        if path_starts_with_ci(&ev.path, &active_root)
                            && !filter.is_excluded(&ev.path)
                            && (include_hidden || !is_under_hidden_dir(&active_root, &ev.path))
                        {
                            // Directory subtree must be reconciled from disk cleanly.
                            let entry = pending.entry(ev.path).or_default();
                            entry.flags |= fsevents::ITEM_CREATED;
                            pending_events += 1;
                            first_pending.get_or_insert_with(Instant::now);
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

        if need_rescan || stop.load(Ordering::Acquire) {
            break;
        }

        let overdue = first_pending.is_some_and(|t| t.elapsed() >= MAX_DEBOUNCE);
        if !pending.is_empty() && (quiet || overdue || pending.len() >= MAX_BATCH_SIZE) {
            let drained = std::mem::take(&mut pending);
            let applied_events = std::mem::take(&mut pending_events);
            first_pending = None;
            let (batch, renames) = resolve_changes(drained, &store);
            match apply_batch(batch, renames, &store, &filter, include_hidden) {
                Ok((added, removed)) => {
                    if let Some(arc) = weak_inner.upgrade() {
                        let mut guard = arc.write();
                        if let Some(slot) = guard.slots.iter_mut().find(|s| {
                            s.volume_id == volume_id
                                && s.generation == generation
                                && s.state == VolumeState::Online
                        }) {
                            slot.last_event_id = Some(seen);
                            slot.entry_count = slot.store.len() as u64;
                            slot.dirty_cache = true;
                            slot.cache_seq = slot.cache_seq.wrapping_add(1);
                            slot.cache_persistence.applied_events = slot
                                .cache_persistence
                                .applied_events
                                .saturating_add(applied_events);
                        }
                    }
                    if added + removed > 0 {
                        if let Some(ref cb) = notify {
                            cb();
                        }
                    }
                }
                Err(_) => {
                    need_rescan = true;
                    break;
                }
            }
        }
    }

    if need_rescan && !stop.load(Ordering::Acquire) {
        if let Some(arc) = weak_inner.upgrade() {
            enqueue_rescan_from_stream(&arc, &queue, &volume_id, generation);
        }
    }
}

fn enqueue_rescan_from_stream(
    inner: &Arc<RwLock<CoordinatorInner>>,
    queue: &Arc<WorkerQueue>,
    volume_id: &str,
    generation: u64,
) {
    let mut guard = inner.write();
    let cfg = guard.config.clone();
    let Some(slot) = guard.slots.iter_mut().find(|s| {
        s.volume_id == volume_id
            && s.generation == generation
            && s.state == VolumeState::Online
            && !s.cancel_token.load(Ordering::SeqCst)
    }) else {
        return;
    };
    // Do not drop slot.stream_handle here because this may execute on vol-fs-*;
    // exiting the loop finishes the thread cleanly and execute_scan_job drops the handle later.
    slot.cancel_token.store(true, Ordering::SeqCst);
    slot.generation = slot.generation.wrapping_add(1);
    slot.cancel_token = Arc::new(AtomicBool::new(false));
    slot.state = VolumeState::Syncing;
    slot.skip_missing_rescan = false;
    slot.last_rescan_triggered = Some(Instant::now());
    let job = ScanJob {
        volume_id: slot.volume_id.clone(),
        generation: slot.generation,
        kind: slot.kind,
        root: PathBuf::from(&slot.active_configured_path),
        config: cfg,
        cancel: Arc::clone(&slot.cancel_token),
    };
    let notify = guard.on_status_change.clone();
    drop(guard);
    queue.enqueue(WorkerTask::Scan(job));
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

/// 判断在线网络卷是否到达周期重扫时机：在用户配置间隔基础上引入上次扫描耗时 5 倍的自适应冷却（上限 6 小时），
/// 防止大容量慢速 NAS 扫描完成后短时间内连续重扫占满网络与 CPU，也防止偶发慢扫长期压制后续自动同步。
fn network_slot_rescan_due(slot: &VolumeSlot, base_interval: Duration, now_unix: u64) -> bool {
    if slot.kind != VolumeKind::Network || slot.state != VolumeState::Online {
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
            slot.dirty_cache
                && slot.state == VolumeState::Online
                && slot.cache_persistence.is_due(now)
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
            if slot.dirty_cache
                && slot.state == VolumeState::Online
                && slot.cache_persistence.is_due(now)
            {
                saves.push(slot.cache_save(volumes_dir));
                records.push(slot.to_record());
            }
        }
        (saves, records)
    };

    // 仅在对应 .cache 实际写盘成功后再更新 manifest，确保 manifest 检查点不超前于磁盘快照
    let saved_ids = save_dirty_caches_and_mark(inner, saves);
    if saved_ids.is_empty() {
        return;
    }
    {
        let mut guard = inner.write();
        for rec in pending_records {
            if saved_ids.contains(&rec.volume_id) {
                guard.manifest.upsert(rec);
            }
        }
    }
    save_latest_manifest_locked(inner, manifest_save_lock, volumes_dir);
}

fn filter_rules_differ(old: &IndexConfig, new: &IndexConfig) -> bool {
    old.exclude != new.exclude
        || old.exclude_paths != new.exclude_paths
        || old.include_hidden != new.include_hidden
        || old.index_apps != new.index_apps
        || old.exclude_sensitive != new.exclude_sensitive
        || old.sensitive_names != new.sensitive_names
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
            dirty_filter,
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
            dirty_filter: true, // force background rescan on init
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

        // 1. Change filter rule while offline -> dirty_filter = true in manifest
        let mut updated_filter_cfg = config.clone();
        updated_filter_cfg.index.exclude.push("secret_dir".to_string());
        coord.on_config_updated(&updated_filter_cfg);

        let m_after_filter = VolumeManifest::load_from_dir(&volumes_dir);
        assert!(m_after_filter.records.iter().all(|r| r.dirty_filter));

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
            last_event_id: None,
            journal_uuid: None,
            entry_count: 2,
            dirty_filter: false,
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

        // Add exclusion while offline
        let mut new_cfg = config.clone();
        new_cfg.index.exclude_paths.push(format!("{disk_str}/secret"));
        coord.on_config_updated(&new_cfg);
        assert_eq!(coord.statuses()[0].state, VolumeState::Offline);

        // Now plug disk in and notify
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

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if coord.statuses()[0].state == VolumeState::Online {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(coord.statuses()[0].state, VolumeState::Online);

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
        assert!(!m_final.records[0].dirty_filter);
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
            dirty_filter: false,
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
    fn reconnecting_cached_local_external_with_missing_or_mismatched_journal_triggers_rescan() {
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
                dirty_filter: false,
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
            let primary = IndexStore::new();
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
            assert!(
                found,
                "cached LocalExternal without matching journal UUID must trigger fallback rescan"
            );
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

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if coord.statuses()[0].state == VolumeState::Missing {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(coord.statuses()[0].state, VolumeState::Missing);

        // Trigger filter change (dirty_filter = true) while subpath is Missing;
        // it runs once to settle on Missing, then must not spin every 3s monitor tick.
        let mut updated = config.clone();
        updated.index.exclude.push("tmp_ignored".to_string());
        config_store.update(updated.clone());
        coord.on_config_updated(&updated);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if coord.statuses()[0].state == VolumeState::Missing {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(coord.statuses()[0].state, VolumeState::Missing);

        let gen_after_settle = coord.slot_generation(&configured_str).unwrap();
        coord.start_monitor(config_store, Arc::new(|| {}));
        std::thread::sleep(Duration::from_millis(3500));

        assert_eq!(coord.statuses()[0].state, VolumeState::Missing);
        assert_eq!(
            coord.slot_generation(&configured_str),
            Some(gen_after_settle),
            "monitor poll must not repeatedly re-trigger Syncing on Missing + dirty_filter"
        );
    }

    #[test]
    fn worker_queue_enqueue_enforces_generation_and_cancellation_monotonicity() {
        let queue = WorkerQueue::new();
        let active_cancel_2 = Arc::new(AtomicBool::new(false));
        let active_cancel_1 = Arc::new(AtomicBool::new(false));
        let cancelled_3 = Arc::new(AtomicBool::new(true));

        queue.enqueue(WorkerTask::Scan(ScanJob {
            volume_id: "vid-a".to_string(),
            generation: 2,
            kind: VolumeKind::LocalExternal,
            root: PathBuf::from("/Volumes/A"),
            config: AppConfig::default(),
            cancel: Arc::clone(&active_cancel_2),
        }));

        // Stale lower-generation task must not evict generation 2
        queue.enqueue(WorkerTask::AttachStream(AttachStreamJob {
            volume_id: "vid-a".to_string(),
            generation: 1,
            active_path: PathBuf::from("/Volumes/A"),
            since_event_id: Some(1),
            stored_journal_uuid: None,
            require_journal_verify: false,
            config: AppConfig::default(),
            cancel: active_cancel_1,
            store: IndexStore::new(),
        }));

        // Cancelled task (even with higher generation) must not enqueue or evict
        queue.enqueue(WorkerTask::AttachStream(AttachStreamJob {
            volume_id: "vid-a".to_string(),
            generation: 3,
            active_path: PathBuf::from("/Volumes/A"),
            since_event_id: Some(1),
            stored_journal_uuid: None,
            require_journal_verify: false,
            config: AppConfig::default(),
            cancel: cancelled_3,
            store: IndexStore::new(),
        }));

        // Unrelated volume coexists independently
        queue.enqueue(WorkerTask::Scan(ScanJob {
            volume_id: "vid-b".to_string(),
            generation: 1,
            kind: VolumeKind::Network,
            root: PathBuf::from("/Volumes/B"),
            config: AppConfig::default(),
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
            root: PathBuf::from("/Volumes/A"),
            config: AppConfig::default(),
            cancel: Arc::new(AtomicBool::new(false)),
        }));
        queue.enqueue(WorkerTask::Scan(ScanJob {
            volume_id: "vid-a".to_string(),
            generation: 4,
            kind: VolumeKind::LocalExternal,
            root: PathBuf::from("/Volumes/A"),
            config: AppConfig::default(),
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
            dirty_filter: false,
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
                dirty_filter: false,
                scan_pending_filter_update: false,
                is_remounted: false,
                has_cache: true,
                dirty_cache: true,
                cache_seq: 1,
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
                dirty_filter: false,
                scan_pending_filter_update: false,
                is_remounted: false,
                has_cache: true,
                dirty_cache: true,
                cache_seq: 1,
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
            active_path: PathBuf::from("/tmp"),
            since_event_id: None,
            stored_journal_uuid: None,
            require_journal_verify: false,
            config: AppConfig::default(),
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
            inner.slots.push(VolumeSlot {
                volume_id: vid,
                configured_path: "/Volumes/NasSync".to_string(),
                active_configured_path: "/Volumes/NasSync".to_string(),
                active_mount_root: Some("/Volumes/NasSync".to_string()),
                subpath: String::new(),
                display_name: "NasSync".to_string(),
                kind: VolumeKind::Network,
                fstype: "smbfs".to_string(),
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
                dirty_filter: false,
                scan_pending_filter_update: false,
                is_remounted: false,
                has_cache: true,
                dirty_cache: false,
                cache_seq: 0,
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
            dirty_filter: false,
            scan_pending_filter_update: false,
            is_remounted: false,
            has_cache: true,
            dirty_cache: false,
            cache_seq: 0,
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
    }
}
