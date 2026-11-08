use crate::config::{AppConfig, ConfigStore};
use crate::filter::CompiledFilter;
use crate::indexer::{FileEntry, IndexStore};
use crate::metrics::WatcherMetrics;
use crate::permissions::has_full_disk_access;
use crate::scanner;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Check if a path's filename starts with '.' (hidden file).
pub fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().starts_with('.'))
        .unwrap_or(false)
}

/// Create a FileEntry from a filesystem path by reading metadata.
/// Returns None if the path doesn't exist or metadata can't be read.
pub fn file_entry_from_path(path: &Path) -> Option<FileEntry> {
    let metadata = std::fs::metadata(path).ok()?;
    let name_str = path.file_name()?.to_string_lossy();
    let extension = if metadata.is_dir() {
        String::new()
    } else {
        std::path::Path::new(name_str.as_ref())
            .extension()
            .map(|ext| ext.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    Some(FileEntry {
        name: name_str.into_owned(),
        path: path.to_string_lossy().into_owned(),
        is_dir: metadata.is_dir(),
        size: metadata.len(),
        modified: metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0),
        created_at: metadata
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0),
        extension,
    })
}

/// 按磁盘现状归类后的一批变更，每个路径至多出现一次。
#[derive(Debug, Default)]
pub struct EventBatch {
    pub creates: Vec<PathBuf>,
    pub removes: Vec<PathBuf>,
    /// 本批次中带显式创建语义的新目录，处理时需要回扫其内容。
    pub dir_creates: Vec<PathBuf>,
}

/// 直接应用一批已归类的变更（不做目录重命名推断）。
pub fn process_event_batch(
    batch: EventBatch,
    store: &IndexStore,
    filter: &CompiledFilter,
    include_hidden: bool,
) -> (usize, usize) {
    apply_batch(batch, Vec::new(), store, filter, include_hidden).unwrap_or_else(|error| {
        eprintln!("[Lokii] index update failed: {error}");
        (0, 0)
    })
}

fn apply_batch(
    mut batch: EventBatch,
    renames: Vec<(String, String)>,
    store: &IndexStore,
    filter: &CompiledFilter,
    include_hidden: bool,
) -> Result<(usize, usize), crate::compact_index::IndexError> {
    let excluded_renames: Vec<_> = renames
        .iter()
        .filter(|(_, new)| {
            filter.is_excluded(Path::new(new)) || (!include_hidden && is_hidden(Path::new(new)))
        })
        .map(|(_, new)| new.clone())
        .collect();
    for (old, new) in &renames {
        batch.removes.retain(|path| path.to_string_lossy() != *old);
        batch.creates.retain(|path| path.to_string_lossy() != *new);
        batch
            .dir_creates
            .retain(|path| path.to_string_lossy() != *new);
    }
    // DirtyQueue 会把新建目录的子项事件合并掉，目录内容只能在此处回扫补齐，
    // 否则新建目录下的文件要等下一次全量对账才进入索引。
    let dir_roots: Vec<PathBuf> = batch
        .dir_creates
        .iter()
        .filter(|path| !filter.is_excluded(path))
        .filter(|path| include_hidden || !is_hidden(path))
        .cloned()
        .collect();
    let mut creates: Vec<_> = batch
        .creates
        .iter()
        .filter(|path| !filter.is_excluded(path))
        .filter(|path| include_hidden || !is_hidden(path))
        .filter(|path| !dir_roots.iter().any(|root| root == *path))
        .filter_map(|path| file_entry_from_path(path))
        .collect();
    if !dir_roots.is_empty() {
        creates.extend(scanner::scan_directories(
            &dir_roots,
            Arc::new(filter.clone()),
            include_hidden,
            Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        ));
    }
    // 回扫的目录整棵以扫描结果为准：先删旧子树再追加，目录改名后又被重建时不留旧子项。
    let remove_roots: Vec<String> = batch
        .removes
        .iter()
        .chain(&dir_roots)
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    store
        .write()
        .apply_event_batch(&renames, &excluded_renames, &remove_roots, creates)
}

/// 无新事件多久后落一批。
const DEFAULT_DEBOUNCE_MS: u64 = 500;

// ── Safe Watch Paths ───────────────────────────────────────────────────────

fn collect_safe_watch_paths(
    path: &Path,
    filter: &CompiledFilter,
    protected: &[PathBuf],
    out: &mut Vec<PathBuf>,
) {
    if !path.exists() {
        return;
    }

    if scanner::is_protected_or_descendant(path, protected) {
        return;
    }

    if filter.is_excluded(path) {
        return;
    }

    // Check if this directory contains any protected or excluded descendants
    let has_protected = scanner::contains_protected_descendant(path, protected);
    let has_excluded = filter.has_excluded_descendant(path);

    if has_protected || has_excluded {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                collect_safe_watch_paths(&entry.path(), filter, protected, out);
            }
        }
        return;
    }

    out.push(path.to_path_buf());
}

fn safe_watch_paths(
    paths: &[PathBuf],
    filter: &CompiledFilter,
    protected: &[PathBuf],
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        collect_safe_watch_paths(path, filter, protected, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

/// FDA 状态、需丢弃事件的受保护路径，以及可安全挂监听的根目录。
fn watch_scope(config: &AppConfig, filter: &CompiledFilter) -> (bool, Vec<PathBuf>, Vec<PathBuf>) {
    let paths = config.resolve_paths();
    let has_fda = has_full_disk_access();
    let protected = if has_fda {
        vec![]
    } else {
        scanner::protected_paths()
    };
    let watch_paths = if has_fda {
        paths
    } else {
        safe_watch_paths(&paths, filter, &protected)
    };
    (has_fda, protected, watch_paths)
}

// ── Resolving Changes ──────────────────────────────────────────────────────

/// shortcut: 追赶期间变更路径超过此数直接全量对账，逐条 stat 已不比扫描便宜；按实测再调。
const REPLAY_PATH_LIMIT: usize = 200_000;

/// 把变更路径按当前磁盘状态归类：仍存在的更新，不存在的删除；
/// 新建或移入的目录整棵回扫，其下的路径不再单独处理。
fn replay_batch(changed: HashMap<PathBuf, u32>) -> EventBatch {
    let mut batch = EventBatch::default();
    let mut rescans = HashSet::new();
    let mut existing = Vec::new();
    for (path, flags) in changed {
        match std::fs::metadata(&path) {
            Ok(meta)
                if meta.is_dir()
                    && flags & (crate::fsevents::ITEM_CREATED | crate::fsevents::ITEM_RENAMED)
                        != 0 =>
            {
                rescans.insert(path);
            }
            Ok(_) => existing.push(path),
            Err(_) => batch.removes.push(path),
        }
    }
    let covered = |path: &Path| path.ancestors().skip(1).any(|a| rescans.contains(a));
    batch
        .creates
        .extend(existing.into_iter().filter(|path| !covered(path)));
    for dir in rescans.iter().filter(|dir| !covered(dir)) {
        batch.dir_creates.push(dir.clone());
        batch.creates.push(dir.clone());
    }
    batch
}

/// 一个路径在本批内累积的事件标志，以及最近一次事件带来的 inode。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Change {
    pub flags: u32,
    pub inode: Option<u64>,
}

/// 按 inode 精确配对目录重命名：同一 inode 的 Renamed 事件中，当前仍是该 inode 的目录为新侧，
/// 已不存在或已换了 inode 的路径为旧侧；只有一新一旧时才配对，其余交给回扫。
fn pair_dir_renames(changes: &HashMap<PathBuf, Change>) -> Vec<(PathBuf, PathBuf)> {
    use std::os::unix::fs::MetadataExt;
    let mut groups: HashMap<u64, Vec<&PathBuf>> = HashMap::new();
    for (path, change) in changes {
        if let (true, Some(inode)) = (change.flags & crate::fsevents::ITEM_RENAMED != 0, change.inode) {
            groups.entry(inode).or_default().push(path);
        }
    }
    let mut pairs = Vec::new();
    for (inode, paths) in groups {
        let (new, old): (Vec<&PathBuf>, Vec<&PathBuf>) = paths.into_iter().partition(|path| {
            std::fs::metadata(path).is_ok_and(|meta| meta.is_dir() && meta.ino() == inode)
        });
        if let ([new], [old]) = (new.as_slice(), old.as_slice()) {
            pairs.push(((*old).clone(), (*new).clone()));
        }
    }
    pairs
}

/// 把一批变更解析成按磁盘现状收敛的批次与精确配对的目录重命名。
///
/// 原地改写只搬得动索引里已有的子项：旧目录在索引中没有子项（如未被索引的隐藏临时目录），
/// 或本批里旧目录下还有别的变更（改名前刚写入、已无法按旧路径 stat）时，改为回扫新目录。
fn resolve_changes(
    mut changes: HashMap<PathBuf, Change>,
    store: &IndexStore,
) -> (EventBatch, Vec<(String, String)>) {
    let pairs: Vec<_> = {
        let index = store.read();
        pair_dir_renames(&changes)
            .into_iter()
            .filter(|(old, _)| {
                index.has_children(&old.to_string_lossy())
                    && !changes.keys().any(|path| path != old && path.starts_with(old))
            })
            .collect()
    };
    for (old, new) in &pairs {
        changes.remove(old);
        changes.remove(new);
    }
    let batch = replay_batch(changes.into_iter().map(|(path, change)| (path, change.flags)).collect());
    let renames = pairs
        .into_iter()
        .map(|(old, new)| {
            (
                old.to_string_lossy().into_owned(),
                new.to_string_lossy().into_owned(),
            )
        })
        .collect();
    (batch, renames)
}

// ── Index Service ──────────────────────────────────────────────────────────

/// 连续事件最长合并窗口，与一批最多容纳的路径数。
const MAX_DEBOUNCE: Duration = Duration::from_millis(2000);
const MAX_BATCH_SIZE: usize = 10_000;
/// 仅用户态丢事件时，静默这么久后从已应用位置重开流重放；连续这么多次仍丢则改为全量对账。
const GAP_QUIET: Duration = Duration::from_secs(2);
const GAP_REPLAYS: u32 = 3;
/// 有变更时最多隔这么久落一次盘：崩溃后只需回放这段时间。
const PERSIST_EVERY: Duration = Duration::from_secs(10 * 60);
/// 全量对账拿不到扫描锁（重建进行中）时的重试间隔，与扫描出错后的退避。
const RECONCILE_RETRY: Duration = Duration::from_secs(2);
const RECONCILE_BACKOFF: Duration = Duration::from_secs(60);

/// 索引服务从哪里接上。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resume {
    /// 从该事件 ID 起流：先回放历史（即启动追赶），再转入实时。
    From(u64),
    /// 没有可信检查点：先全量对账，再从扫描前的事件 ID 起流。
    Verify,
}

impl Resume {
    /// 从系统当前事件 ID 起，只看此后的变更。
    pub fn now() -> Self {
        Resume::From(crate::fsevents::current_event_id())
    }
}

enum Control {
    Replace {
        index: crate::compact_index::CompactIndex,
        checkpoint: crate::cache::Checkpoint,
        done: mpsc::Sender<()>,
    },
    /// 后台重扫得到的整棵子树，由服务线程换入。
    Subtree { root: PathBuf, entries: Vec<FileEntry> },
}

/// 运行中的索引服务。
#[derive(Clone)]
pub struct WatcherHandle {
    control: mpsc::Sender<Control>,
    /// 已完整写入索引的最大事件 ID（0 表示尚无可信检查点）。
    pub applied: Arc<AtomicU64>,
    /// 索引内容所对应的索引范围指纹（不一定是当前配置的）。
    fingerprint: Arc<AtomicU64>,
}

impl WatcherHandle {
    /// 当前索引的回放检查点。调用方须先持有索引读锁再调用：换入新索引时检查点先降、
    /// 指纹后升，持锁读到的组合不会比锁住的索引内容新。
    pub fn checkpoint(&self) -> Option<crate::cache::Checkpoint> {
        let event_id = self.applied.load(Ordering::Acquire);
        (event_id != 0).then(|| crate::cache::Checkpoint {
            event_id,
            fingerprint: self.fingerprint.load(Ordering::Acquire),
        })
    }

    /// 由服务线程换入整体重建的索引；`checkpoint` 是重建扫描开始前取的，服务会从其事件 ID 起重放补齐。
    /// 服务已退出时原样交还索引，由调用方自行替换。
    pub fn replace(
        &self,
        index: crate::compact_index::CompactIndex,
        checkpoint: crate::cache::Checkpoint,
    ) -> Result<(), crate::compact_index::CompactIndex> {
        let (done, replaced) = mpsc::channel();
        match self.control.send(Control::Replace {
            index,
            checkpoint,
            done,
        }) {
            Ok(()) => {
                let _ = replaced.recv();
                Ok(())
            }
            Err(mpsc::SendError(Control::Replace { index, .. })) => Err(index),
            Err(mpsc::SendError(Control::Subtree { .. })) => unreachable!("sent a Replace"),
        }
    }
}

/// 当前配置决定的监听范围。
struct Scope {
    fingerprint: u64,
    exclude: Vec<String>,
    exclude_paths: Vec<PathBuf>,
    filter: CompiledFilter,
    has_fda: bool,
    protected: Vec<PathBuf>,
    /// 挂流的根（无 FDA 时绕开受保护目录）。
    watch_paths: Vec<PathBuf>,
    /// 全量对账扫描的根。
    scan_paths: Vec<PathBuf>,
    include_hidden: bool,
    /// 非本地卷上的根多久整棵重扫一次（`None` 表示不重扫）。
    network_rescan: Option<Duration>,
}

impl Scope {
    fn new(config: &AppConfig) -> Self {
        let filter = CompiledFilter::from_config(config);
        let (has_fda, protected, watch_paths) = watch_scope(config, &filter);
        Self {
            fingerprint: crate::startup::scope_fingerprint(config),
            exclude: config.index.exclude.clone(),
            exclude_paths: config.resolve_exclude_paths(),
            filter,
            has_fda,
            protected,
            watch_paths,
            scan_paths: config.resolve_paths(),
            include_hidden: config.index.include_hidden,
            network_rescan: network_rescan(config),
        }
    }

    /// 事件是否该处理：先剔除排除与受保护路径，免得逐条 stat（受保护路径还会触发 TCC 弹窗）。
    fn admits(&self, path: &Path) -> bool {
        !self.filter.is_excluded(path)
            && (self.has_fda || !scanner::is_protected_or_descendant(path, &self.protected))
            && (self.include_hidden || !self.under_hidden_dir(path))
    }

    /// 路径在其索引根之下是否经过隐藏目录（扫描不会进入这些目录，事件也不该把其中文件加进来）。
    fn under_hidden_dir(&self, path: &Path) -> bool {
        let Some(root) = self
            .scan_paths
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.as_os_str().len())
        else {
            return false;
        };
        let relative = path.strip_prefix(root).unwrap_or(path);
        relative
            .parent()
            .is_some_and(|dirs| dirs.components().any(|part| is_hidden(Path::new(part.as_os_str()))))
    }

    /// 新范围是否只是在旧范围上追加了排除路径（或失去 FDA）：索引就地剔除即可，无需重扫。
    fn narrows(&self, previous: &Scope) -> bool {
        self.scan_paths == previous.scan_paths
            && self.include_hidden == previous.include_hidden
            && self.exclude == previous.exclude
            && previous.exclude_paths.iter().all(|path| self.exclude_paths.contains(path))
            && (self.has_fda == previous.has_fda || !self.has_fda)
    }

    /// 只多了 FDA：补扫原受保护目录即可。
    fn only_gains_fda(&self, previous: &Scope) -> bool {
        self.scan_paths == previous.scan_paths
            && self.include_hidden == previous.include_hidden
            && self.exclude == previous.exclude
            && self.exclude_paths == previous.exclude_paths
            && self.has_fda
            && !previous.has_fda
    }
}

fn network_rescan(config: &AppConfig) -> Option<Duration> {
    let minutes = config.index.network_rescan_minutes;
    (minutes > 0).then(|| Duration::from_secs(minutes * 60))
}

/// 路径所在卷是否本地卷；查不到（不存在、未挂载）按本地处理，不触发重扫。
fn is_local_volume(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return true;
    };
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut stat) } != 0 {
        return true;
    }
    stat.f_flags & libc::MNT_LOCAL as u32 != 0
}

/// 在后台线程逐个重扫远端卷上的根，结果交回服务线程换入（网络卷可能很慢，不能挡住本地事件）。
fn rescan_network_roots(
    roots: Vec<PathBuf>,
    filter: Arc<CompiledFilter>,
    include_hidden: bool,
    is_remote: impl Fn(&Path) -> bool + Send + 'static,
    control: mpsc::Sender<Control>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        // 卸载中的共享 exists/statfs 也可能卡住，只卡这个线程。
        for root in roots.into_iter().filter(|root| root.exists() && is_remote(root)) {
            let entries = scanner::scan_directories(
                &[root.clone()],
                Arc::clone(&filter),
                include_hidden,
                Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            );
            if control.send(Control::Subtree { root, entries }).is_err() {
                return;
            }
        }
    })
}

/// 刚获得完全磁盘访问时，原先受保护、现在可读且在索引范围内的目录。
fn newly_readable(protected: &[PathBuf], scope: &Scope) -> Vec<PathBuf> {
    protected
        .iter()
        .filter(|path| scope.scan_paths.iter().any(|root| path.starts_with(root)))
        .filter(|path| path.exists() && scope.admits(path))
        .cloned()
        .collect()
}

struct Service<F, P> {
    store: IndexStore,
    config_store: ConfigStore,
    metrics: Arc<WatcherMetrics>,
    scan_lock: Arc<std::sync::Mutex<()>>,
    on_batch: F,
    persist: P,
    scope: Scope,
    scope_checked: Instant,
    stream: Option<crate::fsevents::Stream>,
    events: mpsc::Receiver<Vec<crate::fsevents::Event>>,
    control: mpsc::Receiver<Control>,
    /// 交给后台重扫线程，用来把结果送回本线程。
    control_tx: mpsc::Sender<Control>,
    applied: Arc<AtomicU64>,
    /// 索引内容对应的范围指纹；与 `scope.fingerprint` 不同即索引落后于配置，需全量对账。
    fingerprint: Arc<AtomicU64>,
    /// 补扫完成后才生效的新指纹（获得 FDA 后补扫受保护目录期间）。
    pending_fingerprint: Option<u64>,
    pending: HashMap<PathBuf, Change>,
    /// 已收下（含已应用）事件的最大 ID；一批应用完后 `applied` 推进到这里。
    seen: u64,
    first_pending: Option<Instant>,
    rescans: Vec<PathBuf>,
    /// 事件要求的全量对账（丢事件、ID 回绕、应用失败等）。
    full_reconcile: bool,
    /// 要求全量对账的最新事件 ID；只有扫描开始于其后的换入才能清掉该要求。
    reconcile_after: u64,
    /// 全量对账失败（重建占着扫描锁、扫描出错）后，下次重试的最早时刻。
    reconcile_retry: Option<Instant>,
    /// 用户态丢事件的最近时刻；存在期间 `applied` 冻结。
    gap: Option<Instant>,
    gap_replays: u32,
    catching_up: bool,
    history_done: bool,
    history_paths: usize,
    /// 上次落盘后是否写入过变更。
    dirty: bool,
    last_persist: Instant,
    network_scanned: Option<Instant>,
    network_scanning: Arc<std::sync::atomic::AtomicBool>,
}

impl<F, P> Service<F, P>
where
    F: Fn(usize),
    P: Fn(&crate::compact_index::CompactIndex, crate::cache::Checkpoint),
{
    fn run(mut self, resume: Resume) {
        match resume {
            Resume::From(id) if id <= crate::fsevents::current_event_id() => {
                self.catching_up = true;
                self.restart(id);
            }
            // 事件 ID 比系统当前还新：事件库被重置过，历史不可信。
            _ => {
                self.applied.store(0, Ordering::Release);
                self.reconcile_all();
            }
        }
        loop {
            self.poll_control();
            self.poll_scope();
            self.poll_gap();
            self.poll_network();
            let timeout = match self.first_pending {
                Some(first) => Duration::from_millis(DEFAULT_DEBOUNCE_MS)
                    .min(MAX_DEBOUNCE.saturating_sub(first.elapsed())),
                None => Duration::from_millis(DEFAULT_DEBOUNCE_MS),
            };
            let quiet = match self.events.recv_timeout(timeout) {
                Ok(batch) => {
                    self.intake(batch);
                    false
                }
                Err(mpsc::RecvTimeoutError::Timeout) => true,
                // 没有流（无可监听的根）：等配置变化。
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(timeout);
                    true
                }
            };
            let overdue = self.first_pending.is_some_and(|t| t.elapsed() >= MAX_DEBOUNCE);
            if quiet || overdue || self.pending.len() >= MAX_BATCH_SIZE {
                self.flush();
            }
            self.persist_if_due();
        }
    }

    fn persist_if_due(&mut self) {
        if self.dirty && self.last_persist.elapsed() >= PERSIST_EVERY {
            self.persist_now();
        }
    }

    fn persist_now(&mut self) {
        self.dirty = false;
        self.last_persist = Instant::now();
        let handle_view = WatcherHandle {
            control: self.control_tx.clone(),
            applied: Arc::clone(&self.applied),
            fingerprint: Arc::clone(&self.fingerprint),
        };
        let index = self.store.read();
        if let Some(checkpoint) = handle_view.checkpoint() {
            (self.persist)(&index, checkpoint);
        }
    }

    /// 是否在等全量对账（期间的事件都会被对账扫描与其后的重放覆盖，不必再攒）。
    fn reconcile_pending(&self) -> bool {
        let stale_scope = self.fingerprint.load(Ordering::Acquire) != self.scope.fingerprint
            && self.pending_fingerprint != Some(self.scope.fingerprint);
        self.full_reconcile
            || stale_scope
            || (self.catching_up && self.history_paths > REPLAY_PATH_LIMIT)
    }

    fn demand_reconcile(&mut self, after: u64) {
        self.full_reconcile = true;
        self.reconcile_after = self.reconcile_after.max(after);
    }

    fn intake(&mut self, batch: Vec<crate::fsevents::Event>) {
        use crate::fsevents::{
            HISTORY_DONE, IDS_WRAPPED, KERNEL_DROPPED, MOUNT_CHANGED, MUST_SCAN_SUBDIRS,
            USER_DROPPED,
        };
        for event in batch {
            self.metrics.events_received.fetch_add(1, Ordering::Relaxed);
            if event.flags & HISTORY_DONE != 0 {
                self.history_done = true;
                continue;
            }
            self.seen = self.seen.max(event.id);
            if self.reconcile_pending() {
                continue;
            }
            if event.flags & IDS_WRAPPED != 0 {
                self.demand_reconcile(event.id);
            } else if event.flags & MUST_SCAN_SUBDIRS != 0
                && event.flags & (USER_DROPPED | KERNEL_DROPPED) == USER_DROPPED
            {
                // fseventsd 仍记着它没送达的事件：静默后从已应用位置重放即可补齐。
                self.gap = Some(Instant::now());
            } else if event.flags & (MUST_SCAN_SUBDIRS | MOUNT_CHANGED) != 0 {
                self.request_rescan(event.path, event.id);
            } else if self.scope.admits(&event.path) {
                self.metrics.events_enqueued.fetch_add(1, Ordering::Relaxed);
                let change = self.pending.entry(event.path).or_default();
                change.flags |= event.flags;
                change.inode = event.inode.or(change.inode);
                self.first_pending.get_or_insert_with(Instant::now);
                self.history_paths += usize::from(self.catching_up);
            } else {
                self.metrics
                    .events_dropped_excluded
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// 子树需整体对账；若覆盖了某个监听根，等同全量对账。
    fn request_rescan(&mut self, path: PathBuf, id: u64) {
        if self.scope.scan_paths.iter().any(|root| root.starts_with(&path)) {
            self.demand_reconcile(id);
        } else if self.scope.admits(&path) {
            self.rescans.push(path);
        }
        self.first_pending.get_or_insert_with(Instant::now);
    }

    fn flush(&mut self) {
        if self.reconcile_pending() {
            self.pending.clear();
            self.rescans.clear();
            self.first_pending = None;
            if self.reconcile_retry.is_none_or(|at| Instant::now() >= at) {
                self.reconcile_all();
            }
            return;
        }
        let mut changes = std::mem::take(&mut self.pending);
        // 子树对账即"把该目录当作新建目录回扫"，并入同一批以便与其下的事件去重。
        for dir in self.rescans.drain(..) {
            changes.entry(dir).or_default().flags |= crate::fsevents::ITEM_CREATED;
        }
        self.first_pending = None;
        let changed = !changes.is_empty();
        if changed {
            let (batch, renames) = resolve_changes(changes, &self.store);
            if let Err(error) = apply_batch(
                batch,
                renames,
                &self.store,
                &self.scope.filter,
                self.scope.include_hidden,
            ) {
                eprintln!("[Lokii] index event batch failed: {error}");
                self.demand_reconcile(self.seen);
                return;
            }
            self.metrics.batches_processed.fetch_add(1, Ordering::Relaxed);
            self.dirty = true;
        }
        if let Some(fingerprint) = self.pending_fingerprint.take() {
            self.fingerprint.store(fingerprint, Ordering::Release);
        }
        if self.gap.is_none() {
            self.applied.fetch_max(self.seen, Ordering::Release);
        }
        if changed {
            (self.on_batch)(self.store.len());
        }
        if std::mem::take(&mut self.history_done) {
            if self.gap.is_none() {
                self.gap_replays = 0;
            }
            if std::mem::take(&mut self.catching_up) {
                self.history_paths = 0;
                self.persist_now();
            }
        }
    }

    /// 全量扫描替换索引，再从扫描前的事件 ID 起流，补上扫描期间的变更。
    fn reconcile_all(&mut self) {
        // 重建正在进行：它完成后会 resync，这里不必重复扫描。
        let scan_lock = Arc::clone(&self.scan_lock);
        let Ok(_guard) = scan_lock.try_lock() else {
            if !self.reconcile_pending() {
                self.demand_reconcile(0);
            }
            self.reconcile_retry = Some(Instant::now() + RECONCILE_RETRY);
            return;
        };
        let checkpoint = crate::cache::Checkpoint {
            event_id: crate::fsevents::current_event_id(),
            fingerprint: self.scope.fingerprint,
        };
        match scanner::scan_compact(
            &self.scope.scan_paths,
            Arc::new(self.scope.filter.clone()),
            self.scope.include_hidden,
            Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        ) {
            Ok(fresh) => self.swap_in(fresh, checkpoint),
            Err(error) => {
                eprintln!("[Lokii] full reconciliation failed: {error}");
                self.demand_reconcile(0);
                self.reconcile_retry = Some(Instant::now() + RECONCILE_BACKOFF);
                return;
            }
        }
        self.store.warm_query_index();
        (self.on_batch)(self.store.len());
        self.persist_now();
    }

    /// 换入一份扫描于 `checkpoint.event_id` 之后、按 `checkpoint.fingerprint` 范围扫描的完整索引。
    /// 检查点先降再替换、指纹替换后再升：任何时刻持锁读到的组合都不会比索引内容新。
    fn swap_in(
        &mut self,
        index: crate::compact_index::CompactIndex,
        checkpoint: crate::cache::Checkpoint,
    ) {
        let from = checkpoint.event_id;
        self.applied.store(from, Ordering::Release);
        self.store.replace_compact(index);
        self.fingerprint.store(checkpoint.fingerprint, Ordering::Release);
        self.pending_fingerprint = None;
        self.resync(from);
    }

    /// 索引已对齐到 `from` 之前的全部事件：从这里重开流。
    fn resync(&mut self, from: u64) {
        // 扫描开始之后才发生的丢事件，这次扫描覆盖不到。
        self.full_reconcile &= from < self.reconcile_after;
        self.reconcile_retry = None;
        self.gap = None;
        self.gap_replays = 0;
        self.catching_up = false;
        self.history_paths = 0;
        self.rescans.clear();
        self.applied.store(from, Ordering::Release);
        self.restart(from);
    }

    /// 从 `from` 重开流；未应用的事件丢弃，新流会重新送达（应用按磁盘现状收敛，可重复）。
    fn restart(&mut self, from: u64) {
        self.stream = None;
        let (tx, rx) = mpsc::channel();
        self.events = rx;
        self.pending.clear();
        self.first_pending = None;
        self.history_done = false;
        self.seen = from;
        self.stream = crate::fsevents::watch(&self.scope.watch_paths, from, tx);
    }

    fn poll_control(&mut self) {
        while let Ok(control) = self.control.try_recv() {
            match control {
                Control::Replace {
                    index,
                    checkpoint,
                    done,
                } => {
                    self.swap_in(index, checkpoint);
                    // 先应答再预热：重建线程（及其后等着存盘退出的主线程）不必等预热。
                    let _ = done.send(());
                    self.store.warm_query_index();
                }
                Control::Subtree { root, entries } => {
                    // 扫描期间该根可能已移出索引范围。
                    if !self.scope.scan_paths.contains(&root) || !self.scope.admits(&root) {
                        continue;
                    }
                    let root = root.to_string_lossy().into_owned();
                    let applied = self
                        .store
                        .write()
                        .apply_event_batch(&[], &[], &[root], entries);
                    match applied {
                        Ok(_) => {
                            self.dirty = true;
                            (self.on_batch)(self.store.len());
                        }
                        Err(error) => eprintln!("[Lokii] network rescan failed: {error}"),
                    }
                }
            }
        }
    }

    /// 非本地卷上的根按配置间隔整棵重扫；启动后先扫一次，补上关闭期间的远端改动。
    fn poll_network(&mut self) {
        let Some(every) = self.scope.network_rescan else {
            return;
        };
        if self.network_scanned.is_some_and(|at| at.elapsed() < every)
            || self.network_scanning.swap(true, Ordering::Relaxed)
        {
            return;
        }
        self.network_scanned = Some(Instant::now());
        let scanning = Arc::clone(&self.network_scanning);
        let worker = rescan_network_roots(
            self.scope.scan_paths.clone(),
            Arc::new(self.scope.filter.clone()),
            self.scope.include_hidden,
            |root| !is_local_volume(root),
            self.control_tx.clone(),
        );
        std::thread::spawn(move || {
            let _ = worker.join();
            scanning.store(false, Ordering::Relaxed);
        });
    }

    /// 索引根、排除规则、隐藏文件或 FDA 变化后，按新范围从已应用位置重开流。
    fn poll_scope(&mut self) {
        if self.scope_checked.elapsed() < Duration::from_millis(DEFAULT_DEBOUNCE_MS) {
            return;
        }
        self.scope_checked = Instant::now();
        let config = self.config_store.read().clone();
        if crate::startup::scope_fingerprint(&config) != self.scope.fingerprint {
            let previous = std::mem::replace(&mut self.scope, Scope::new(&config));
            let index_matched_previous =
                self.fingerprint.load(Ordering::Acquire) == previous.fingerprint;
            self.restart(self.applied.load(Ordering::Acquire));
            // 索引原本就落后于旧范围时，交给全量对账（reconcile_pending 会发现指纹不符）。
            if index_matched_previous && self.scope.narrows(&previous) {
                let added: Vec<String> = self
                    .scope
                    .exclude_paths
                    .iter()
                    .filter(|path| !previous.exclude_paths.contains(path))
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect();
                self.store.exclude_paths(&added);
                self.fingerprint.store(self.scope.fingerprint, Ordering::Release);
                self.dirty = true;
            } else if index_matched_previous && self.scope.only_gains_fda(&previous) {
                self.rescans
                    .extend(newly_readable(&previous.protected, &self.scope));
                self.pending_fingerprint = Some(self.scope.fingerprint);
                self.first_pending.get_or_insert_with(Instant::now);
            }
        } else {
            self.scope.network_rescan = network_rescan(&config);
        }
    }

    fn poll_gap(&mut self) {
        let Some(at) = self.gap else {
            return;
        };
        if at.elapsed() < GAP_QUIET {
            return;
        }
        self.gap = None;
        self.gap_replays += 1;
        if self.gap_replays > GAP_REPLAYS {
            self.demand_reconcile(self.seen);
            self.flush();
        } else {
            self.restart(self.applied.load(Ordering::Acquire));
        }
    }
}

/// 启动索引服务线程：按 `resume` 接上（追赶或全量对账），之后持续把 FSEvents 变更写入索引。
///
/// `on_batch(总数)` 在每批变更后调用；`persist(索引, 检查点)` 在追赶完成、全量对账后与
/// 有变更时定期调用，由调用方落盘。
pub fn start_watcher<F, P>(
    store: IndexStore,
    config_store: ConfigStore,
    metrics: Arc<WatcherMetrics>,
    resume: Resume,
    scan_lock: Arc<std::sync::Mutex<()>>,
    on_batch: F,
    persist: P,
) -> WatcherHandle
where
    F: Fn(usize) + Send + 'static,
    P: Fn(&crate::compact_index::CompactIndex, crate::cache::Checkpoint) + Send + 'static,
{
    let (control_tx, control) = mpsc::channel();
    let service_tx = control_tx.clone();
    let applied = Arc::new(AtomicU64::new(match resume {
        Resume::From(id) => id,
        Resume::Verify => 0,
    }));
    let scope = Scope::new(&config_store.read());
    // 追赶或冷启动的索引都对应当前范围；无检查点时 applied 为 0，指纹不会被用到。
    let fingerprint = Arc::new(AtomicU64::new(scope.fingerprint));
    let handle = WatcherHandle {
        control: control_tx,
        applied: Arc::clone(&applied),
        fingerprint: Arc::clone(&fingerprint),
    };
    std::thread::spawn(move || {
        let service = Service {
            store,
            config_store,
            metrics,
            scan_lock,
            on_batch,
            persist,
            scope,
            scope_checked: Instant::now(),
            stream: None,
            events: mpsc::channel().1,
            control,
            control_tx: service_tx,
            applied,
            fingerprint,
            pending_fingerprint: None,
            pending: HashMap::new(),
            seen: 0,
            first_pending: None,
            rescans: Vec::new(),
            full_reconcile: false,
            reconcile_after: 0,
            reconcile_retry: None,
            gap: None,
            gap_replays: 0,
            catching_up: false,
            history_done: false,
            history_paths: 0,
            dirty: false,
            last_persist: Instant::now(),
            network_scanned: None,
            network_scanning: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        service.run(resume);
    });
    handle
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn indexed(store: &IndexStore, path: &Path) -> bool {
        let path = path.to_string_lossy();
        store
            .read()
            .iter()
            .any(|(_, view)| view.materialize().path == path)
    }

    fn ino(path: &Path) -> Option<u64> {
        use std::os::unix::fs::MetadataExt;
        Some(fs::metadata(path).unwrap().ino())
    }

    fn renamed(inode: Option<u64>) -> Change {
        Change {
            flags: crate::fsevents::ITEM_RENAMED,
            inode,
        }
    }

    #[test]
    fn directory_renames_pair_by_inode() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (a, b, c) = (root.join("a"), root.join("b"), root.join("c"));
        fs::create_dir(&a).unwrap();
        let inode = ino(&a);
        fs::rename(&a, &b).unwrap();
        let changes = HashMap::from([(a.clone(), renamed(inode)), (b.clone(), renamed(inode))]);
        assert_eq!(pair_dir_renames(&changes), vec![(a.clone(), b.clone())]);

        // The old path was recreated: still the old side, it is another inode now.
        fs::create_dir(&a).unwrap();
        assert_eq!(pair_dir_renames(&changes), vec![(a.clone(), b.clone())]);

        // Moved in from outside the watched roots: nothing to pair with.
        fs::create_dir(&c).unwrap();
        let changes = HashMap::from([(c.clone(), renamed(ino(&c)))]);
        assert!(pair_dir_renames(&changes).is_empty());
    }

    #[test]
    fn paired_rename_rewrites_children_without_rescanning() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir(root.join("a")).unwrap();
        let inode = ino(&root.join("a"));
        let config = scoped_config(&root);
        let filter = CompiledFilter::from_config(&config);
        let store = IndexStore::new();
        // ghost.txt exists only in the index: a rescan would drop it, a rewrite moves it.
        store.replace(vec![
            make_entry("a", &root.join("a").to_string_lossy(), true),
            make_entry("ghost.txt", &root.join("a/ghost.txt").to_string_lossy(), false),
        ]);
        fs::rename(root.join("a"), root.join("b")).unwrap();

        let changes = HashMap::from([
            (root.join("a"), renamed(inode)),
            (root.join("b"), renamed(inode)),
        ]);
        let (batch, renames) = resolve_changes(changes, &store);
        apply_batch(batch, renames, &store, &filter, true).unwrap();

        assert!(indexed(&store, &root.join("b/ghost.txt")));
        assert!(!indexed(&store, &root.join("a/ghost.txt")));
        assert!(!indexed(&store, &root.join("a")));
    }

    fn scoped_config(root: &Path) -> AppConfig {
        let mut config = AppConfig::default();
        config.index.index_paths = vec![root.to_string_lossy().into_owned()];
        config.index.exclude = vec![];
        config.index.exclude_paths = vec![];
        // TempDir names start with ".tmp".
        config.index.include_hidden = true;
        config
    }

    #[test]
    fn rescanned_directory_drops_children_that_moved_away() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir(root.join("a")).unwrap();
        fs::write(root.join("a/child.txt"), "x").unwrap();
        let mut config = AppConfig::default();
        config.index.index_paths = vec![root.to_string_lossy().into_owned()];
        config.index.exclude = vec![];
        config.index.exclude_paths = vec![];
        let filter = CompiledFilter::from_config(&config);
        let store = IndexStore::new();
        store.replace(scanner::scan_directories(
            &[root.clone()],
            Arc::new(filter.clone()),
            true,
            Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        ));

        // a -> b, then a fresh empty a: no event mentions a/child.txt.
        fs::rename(root.join("a"), root.join("b")).unwrap();
        fs::create_dir(root.join("a")).unwrap();
        let renamed_in = crate::fsevents::ITEM_CREATED | crate::fsevents::ITEM_RENAMED;
        let changed = HashMap::from([(root.join("a"), renamed_in), (root.join("b"), renamed_in)]);
        apply_batch(replay_batch(changed), Vec::new(), &store, &filter, true).unwrap();

        assert!(indexed(&store, &root.join("b/child.txt")));
        assert!(indexed(&store, &root.join("a")));
        assert!(!indexed(&store, &root.join("a/child.txt")), "stale child kept");
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn scan(root: &Path) -> Vec<FileEntry> {
        let filter = CompiledFilter::from_config(&scoped_config(root));
        scanner::scan_directories(
            &[root.to_path_buf()],
            Arc::new(filter),
            true,
            Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        )
    }

    /// 启动服务，返回句柄与"已落盘的检查点"接收端。
    fn serve(
        store: &IndexStore,
        root: &Path,
        resume: Resume,
    ) -> (WatcherHandle, mpsc::Receiver<u64>) {
        let (persisted, rx) = mpsc::channel();
        let handle = start_watcher(
            store.clone(),
            ConfigStore::new(scoped_config(root)),
            Arc::new(WatcherMetrics::default()),
            resume,
            Arc::new(std::sync::Mutex::new(())),
            |_| {},
            move |_, checkpoint: crate::cache::Checkpoint| {
                let _ = persisted.send(checkpoint.event_id);
            },
        );
        (handle, rx)
    }

    #[test]
    fn service_catches_up_from_checkpoint_then_follows_live_changes() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("kept.txt"), "a").unwrap();
        fs::write(root.join("deleted.txt"), "b").unwrap();
        fs::create_dir(root.join("old_dir")).unwrap();
        fs::write(root.join("old_dir/child.txt"), "c").unwrap();
        let mut entries = scan(&root);
        // Only in the index and never mentioned by an event: replay keeps it, a rescan would not.
        let ghost = root.join("ghost.txt");
        entries.push(make_entry("ghost.txt", &ghost.to_string_lossy(), false));
        let store = IndexStore::new();
        store.replace(entries);
        let since = crate::fsevents::current_event_id();

        fs::remove_file(root.join("deleted.txt")).unwrap();
        fs::write(root.join("added.txt"), "d").unwrap();
        fs::rename(root.join("old_dir"), root.join("new_dir")).unwrap();
        fs::create_dir_all(root.join("made/deep")).unwrap();
        fs::write(root.join("made/deep/file.txt"), "e").unwrap();

        let (handle, persisted) = serve(&store, &root, Resume::From(since));
        let checkpoint = persisted.recv_timeout(Duration::from_secs(10)).expect("caught up");
        assert!(checkpoint > since);
        for (path, present) in [
            ("added.txt", true),
            ("kept.txt", true),
            ("made/deep/file.txt", true),
            ("new_dir/child.txt", true),
            ("ghost.txt", true),
            ("deleted.txt", false),
            ("old_dir", false),
            ("old_dir/child.txt", false),
        ] {
            assert_eq!(indexed(&store, &root.join(path)), present, "{path}");
        }

        let live = root.join("live.txt");
        fs::write(&live, "f").unwrap();
        wait_until("live change", || indexed(&store, &live));
        assert!(handle.applied.load(Ordering::Relaxed) > checkpoint);
    }

    #[test]
    fn service_without_checkpoint_rescans_everything() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("real.txt"), "a").unwrap();
        let ghost = root.join("ghost.txt");
        let store = IndexStore::new();
        store.replace(vec![make_entry("ghost.txt", &ghost.to_string_lossy(), false)]);

        let (_handle, persisted) = serve(&store, &root, Resume::Verify);
        persisted.recv_timeout(Duration::from_secs(10)).expect("verified");
        assert!(indexed(&store, &root.join("real.txt")));
        assert!(!indexed(&store, &ghost));
    }

    fn idle_service(
        store: &IndexStore,
        root: &Path,
    ) -> Service<impl Fn(usize), impl Fn(&crate::compact_index::CompactIndex, crate::cache::Checkpoint)>
    {
        idle_service_persisting(store, root, |_: &crate::compact_index::CompactIndex, _| {})
    }

    fn idle_service_persisting<
        P: Fn(&crate::compact_index::CompactIndex, crate::cache::Checkpoint),
    >(
        store: &IndexStore,
        root: &Path,
        persist: P,
    ) -> Service<impl Fn(usize), P> {
        let config = scoped_config(root);
        let (control_tx, control) = mpsc::channel();
        Service {
            store: store.clone(),
            config_store: ConfigStore::new(config.clone()),
            metrics: Arc::new(WatcherMetrics::default()),
            scan_lock: Arc::new(std::sync::Mutex::new(())),
            on_batch: |_| {},
            persist,
            scope: Scope::new(&config),
            scope_checked: Instant::now(),
            stream: None,
            events: mpsc::channel().1,
            control,
            applied: Arc::new(AtomicU64::new(5)),
            control_tx,
            fingerprint: Arc::new(AtomicU64::new(Scope::new(&config).fingerprint)),
            pending_fingerprint: None,
            pending: HashMap::new(),
            seen: 5,
            first_pending: None,
            rescans: Vec::new(),
            full_reconcile: false,
            reconcile_after: 0,
            reconcile_retry: None,
            gap: None,
            gap_replays: 0,
            catching_up: false,
            history_done: false,
            history_paths: 0,
            dirty: false,
            last_persist: Instant::now(),
            network_scanned: None,
            network_scanning: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    #[test]
    fn local_volumes_are_detected() {
        let dir = TempDir::new().unwrap();
        assert!(is_local_volume(dir.path()));
        assert!(is_local_volume(Path::new("/definitely/not/mounted")), "unknown counts as local");
    }

    #[test]
    fn network_roots_are_rescanned_off_thread_and_applied_by_the_service() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("real.txt"), "x").unwrap();
        let ghost = root.join("ghost.txt");
        let store = IndexStore::new();
        store.replace(vec![make_entry("ghost.txt", &ghost.to_string_lossy(), false)]);
        let mut service = idle_service(&store, &root);

        rescan_network_roots(
            vec![root.clone()],
            Arc::new(service.scope.filter.clone()),
            true,
            |_| true,
            service.control_tx.clone(),
        );
        wait_until("network rescan applied", || {
            service.poll_control();
            indexed(&store, &root.join("real.txt"))
        });
        assert!(!indexed(&store, &ghost));
        assert!(service.dirty);
    }

    #[test]
    fn network_rescans_follow_the_configured_interval() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = IndexStore::new();
        let mut service = idle_service(&store, &root);
        service.scope.network_rescan = None;
        service.poll_network();
        assert!(service.network_scanned.is_none(), "0 minutes disables rescans");

        service.scope.network_rescan = Some(Duration::from_secs(300));
        service.poll_network();
        let first = service.network_scanned.expect("due right away");
        service.network_scanning.store(false, Ordering::Relaxed);
        service.poll_network();
        assert_eq!(service.network_scanned, Some(first), "not due again yet");
    }

    #[test]
    fn granting_full_disk_access_rescans_newly_readable_protected_dirs() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir(root.join("Desktop")).unwrap();
        let scope = Scope::new(&scoped_config(&root));
        let protected = vec![
            root.join("Desktop"),
            root.join("Missing"),
            PathBuf::from("/elsewhere/Desktop"),
        ];
        assert_eq!(newly_readable(&protected, &scope), vec![root.join("Desktop")]);
    }

    #[test]
    fn changes_are_persisted_periodically_with_the_applied_checkpoint() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("a.txt");
        fs::write(&file, "x").unwrap();
        let store = IndexStore::new();
        let saved = std::cell::RefCell::new(Vec::new());
        let mut service = idle_service_persisting(
            &store,
            &root,
            |_: &crate::compact_index::CompactIndex, checkpoint: crate::cache::Checkpoint| {
                saved.borrow_mut().push(checkpoint.event_id)
            },
        );

        service.last_persist = Instant::now() - PERSIST_EVERY;
        service.persist_if_due();
        assert!(saved.borrow().is_empty(), "nothing changed, nothing to save");

        service.intake(vec![event(file, crate::fsevents::ITEM_CREATED, 60)]);
        service.flush();
        service.persist_if_due();
        assert_eq!(*saved.borrow(), [60]);
        service.persist_if_due();
        assert_eq!(*saved.borrow(), [60], "saved once per interval");
    }

    #[test]
    fn rebuilt_index_is_swapped_in_by_the_service() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = IndexStore::new();
        store.replace(vec![make_entry("old.txt", &root.join("old.txt").to_string_lossy(), false)]);
        let (handle, _) = serve(&store, &root, Resume::now());
        let fresh = crate::compact_index::CompactIndex::from_entries(vec![make_entry(
            "new.txt",
            &root.join("new.txt").to_string_lossy(),
            false,
        )])
        .unwrap();
        let from = crate::fsevents::current_event_id();
        let checkpoint = crate::cache::Checkpoint {
            event_id: from,
            fingerprint: Scope::new(&scoped_config(&root)).fingerprint,
        };

        assert!(handle.replace(fresh, checkpoint).is_ok());
        assert!(indexed(&store, &root.join("new.txt")));
        assert!(!indexed(&store, &root.join("old.txt")));
        assert_eq!(handle.applied.load(Ordering::Relaxed), from);
    }

    fn event(path: PathBuf, flags: u32, id: u64) -> crate::fsevents::Event {
        crate::fsevents::Event {
            path,
            flags,
            id,
            inode: None,
        }
    }

    #[test]
    fn applied_id_advances_only_after_the_batch_is_written() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("a.txt");
        fs::write(&file, "x").unwrap();
        let store = IndexStore::new();
        let mut service = idle_service(&store, &root);

        service.intake(vec![event(file.clone(), crate::fsevents::ITEM_CREATED, 30)]);
        assert_eq!(service.applied.load(Ordering::Relaxed), 5);
        service.flush();
        assert_eq!(service.applied.load(Ordering::Relaxed), 30);
        assert!(indexed(&store, &file));
    }

    #[test]
    fn must_scan_subdirs_reconciles_that_subtree() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let sub = root.join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("real.txt"), "x").unwrap();
        let ghost = sub.join("ghost.txt");
        let store = IndexStore::new();
        store.replace(vec![make_entry("ghost.txt", &ghost.to_string_lossy(), false)]);
        let mut service = idle_service(&store, &root);

        service.intake(vec![event(sub.clone(), crate::fsevents::MUST_SCAN_SUBDIRS, 40)]);
        service.flush();
        assert!(indexed(&store, &sub.join("real.txt")));
        assert!(!indexed(&store, &ghost));
        assert_eq!(service.applied.load(Ordering::Relaxed), 40);
    }

    #[test]
    fn user_dropped_events_freeze_checkpoint_then_replay() {
        use crate::fsevents::{MUST_SCAN_SUBDIRS, USER_DROPPED};
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = IndexStore::new();
        let mut service = idle_service(&store, &root);

        service.intake(vec![event(root.clone(), MUST_SCAN_SUBDIRS | USER_DROPPED, 50)]);
        service.flush();
        assert_eq!(service.applied.load(Ordering::Relaxed), 5, "gap must hold the checkpoint");
        assert!(!service.full_reconcile);

        service.gap = Some(Instant::now() - GAP_QUIET);
        service.poll_gap();
        assert_eq!(service.gap_replays, 1);
        assert!(service.stream.is_some(), "replays from the checkpoint");
        assert_eq!(service.seen, 5);
    }

    #[test]
    fn resolve_follows_disk_state() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (kept, gone, made) = (root.join("kept.txt"), root.join("gone.txt"), root.join("made"));
        fs::write(&kept, "x").unwrap();
        fs::create_dir(&made).unwrap();
        fs::write(made.join("child.txt"), "x").unwrap();
        let compound = crate::fsevents::ITEM_CREATED | fs_removed();
        let changes = HashMap::from([
            // Saved atomically (removed + created): still on disk, so upsert.
            (kept.clone(), Change { flags: compound, inode: None }),
            // Created then trashed within the window: gone.
            (gone.clone(), Change { flags: compound, inode: None }),
            (made.clone(), Change { flags: crate::fsevents::ITEM_CREATED, inode: None }),
            (made.join("child.txt"), Change { flags: crate::fsevents::ITEM_CREATED, inode: None }),
        ]);
        let (batch, renames) = resolve_changes(changes, &IndexStore::new());
        assert!(renames.is_empty());
        assert_eq!(batch.creates.len(), 2, "{batch:?}");
        assert!(batch.creates.contains(&kept) && batch.creates.contains(&made));
        assert_eq!(batch.dir_creates, vec![made.clone()]);
        assert_eq!(batch.removes, vec![gone]);

        let store = IndexStore::new();
        let filter = CompiledFilter::from_config(&scoped_config(&root));
        apply_batch(batch, Vec::new(), &store, &filter, true).unwrap();
        assert!(indexed(&store, &made.join("child.txt")), "new directory scanned");
    }

    fn fs_removed() -> u32 {
        fsevent_sys::kFSEventStreamEventFlagItemRemoved
    }

    #[test]
    fn directory_moved_away_drops_its_children() {
        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("proj", "/tmp/lokii-missing/proj", true),
            make_entry("main.rs", "/tmp/lokii-missing/proj/main.rs", false),
        ]);
        let changes = HashMap::from([(
            PathBuf::from("/tmp/lokii-missing/proj"),
            Change { flags: crate::fsevents::ITEM_RENAMED, inode: Some(1) },
        )]);
        let (batch, renames) = resolve_changes(changes, &store);
        assert!(renames.is_empty());
        let (_added, removed) = apply_batch(batch, renames, &store, &dummy_filter(), true).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(store.len(), 0);
    }

    // ── 评审回归 ──

    #[test]
    fn rename_from_an_unindexed_directory_rescans_the_new_one() {
        // mkdir .tmpX && fill && mv .tmpX pkg: the hidden source never entered the index.
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir(root.join(".tmpx")).unwrap();
        fs::write(root.join(".tmpx/lib.rs"), "x").unwrap();
        let inode = ino(&root.join(".tmpx"));
        fs::rename(root.join(".tmpx"), root.join("pkg")).unwrap();
        let store = IndexStore::new();
        let changes = HashMap::from([
            (root.join(".tmpx"), renamed(inode)),
            (root.join("pkg"), renamed(inode)),
        ]);
        let (batch, renames) = resolve_changes(changes, &store);
        assert!(renames.is_empty());
        let filter = CompiledFilter::from_config(&scoped_config(&root));
        apply_batch(batch, renames, &store, &filter, true).unwrap();
        assert!(indexed(&store, &root.join("pkg/lib.rs")));
    }

    #[test]
    fn rename_after_changes_inside_the_old_directory_rescans_the_new_one() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir(root.join("a")).unwrap();
        fs::write(root.join("a/old.txt"), "x").unwrap();
        let store = IndexStore::new();
        store.replace(scan(&root));
        let inode = ino(&root.join("a"));
        fs::write(root.join("a/new.txt"), "y").unwrap();
        fs::rename(root.join("a"), root.join("b")).unwrap();
        let created = Change {
            flags: crate::fsevents::ITEM_CREATED,
            inode: None,
        };
        let changes = HashMap::from([
            (root.join("a/new.txt"), created),
            (root.join("a"), renamed(inode)),
            (root.join("b"), renamed(inode)),
        ]);
        let (batch, renames) = resolve_changes(changes, &store);
        assert!(renames.is_empty());
        let filter = CompiledFilter::from_config(&scoped_config(&root));
        apply_batch(batch, renames, &store, &filter, true).unwrap();
        assert!(indexed(&store, &root.join("b/new.txt")));
        assert!(indexed(&store, &root.join("b/old.txt")));
        assert!(!indexed(&store, &root.join("a/old.txt")));
    }

    #[test]
    fn renaming_into_a_hidden_name_drops_the_subtree_when_hidden_is_off() {
        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("a", "/srv/lokii-h/a", true),
            make_entry("f.txt", "/srv/lokii-h/a/f.txt", false),
        ]);
        let renames = vec![("/srv/lokii-h/a".to_string(), "/srv/lokii-h/.a".to_string())];
        apply_batch(EventBatch::default(), renames, &store, &dummy_filter(), false).unwrap();
        assert_eq!(store.len(), 0);
    }

    fn rescope(service: &mut Service<impl Fn(usize), impl Fn(&crate::compact_index::CompactIndex, crate::cache::Checkpoint)>, edit: impl FnOnce(&mut AppConfig)) {
        let mut config = service.config_store.read().clone();
        edit(&mut config);
        service.config_store.update(config);
        service.scope_checked = Instant::now() - Duration::from_secs(1);
        service.poll_scope();
    }

    #[test]
    fn added_exclusion_prunes_in_place_and_keeps_the_checkpoint_valid() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("x.txt", &root.join("sub/x.txt").to_string_lossy(), false),
            make_entry("y.txt", &root.join("y.txt").to_string_lossy(), false),
        ]);
        let mut service = idle_service(&store, &root);
        let excluded = root.join("sub").to_string_lossy().into_owned();
        rescope(&mut service, |config| config.index.exclude_paths = vec![excluded]);

        assert!(!indexed(&store, &root.join("sub/x.txt")));
        assert!(indexed(&store, &root.join("y.txt")));
        assert!(!service.reconcile_pending());
        assert_eq!(service.fingerprint.load(Ordering::Relaxed), service.scope.fingerprint);
    }

    #[test]
    fn widened_scope_keeps_the_old_fingerprint_until_reconciled() {
        let dir = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = IndexStore::new();
        let mut service = idle_service(&store, &root);
        let before = service.fingerprint.load(Ordering::Relaxed);
        let extra = other.path().canonicalize().unwrap().to_string_lossy().into_owned();
        rescope(&mut service, |config| config.index.index_paths.push(extra));

        assert!(service.reconcile_pending());
        assert_eq!(service.fingerprint.load(Ordering::Relaxed), before, "saves must not claim the new scope");
    }

    #[test]
    fn blocked_reconciliation_backs_off_instead_of_spinning() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = IndexStore::new();
        let mut service = idle_service(&store, &root);
        let scan_lock = Arc::clone(&service.scan_lock);
        let _rebuilding = scan_lock.lock().unwrap();
        service.demand_reconcile(0);
        service.first_pending = Some(Instant::now() - MAX_DEBOUNCE);

        service.flush();
        assert!(service.first_pending.is_none(), "no zero-timeout spin");
        let retry = service.reconcile_retry.expect("retry scheduled");
        service.flush();
        assert_eq!(service.reconcile_retry, Some(retry), "not retried before it is due");

        let file = root.join("a.txt");
        fs::write(&file, "x").unwrap();
        service.intake(vec![event(file, crate::fsevents::ITEM_CREATED, 70)]);
        assert!(service.pending.is_empty(), "events wait for the reconciliation");
    }

    #[test]
    fn swap_in_only_clears_reconcile_demands_its_scan_covers() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = IndexStore::new();
        let mut service = idle_service(&store, &root);
        let fingerprint = service.scope.fingerprint;
        let index = || crate::compact_index::CompactIndex::from_entries(Vec::new()).unwrap();
        service.demand_reconcile(100);

        service.swap_in(index(), crate::cache::Checkpoint { event_id: 50, fingerprint });
        assert!(service.full_reconcile, "drop at 100 happened after the scan started at 50");
        service.swap_in(index(), crate::cache::Checkpoint { event_id: 150, fingerprint });
        assert!(!service.full_reconcile);
    }

    #[test]
    fn files_under_hidden_directories_are_not_admitted_when_hidden_is_off() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut config = scoped_config(&root);
        config.index.include_hidden = false;
        let scope = Scope::new(&config);
        assert!(!scope.admits(&root.join(".cache/x.txt")));
        assert!(scope.admits(&root.join("src/x.txt")));
    }

    #[test]
    fn network_results_for_roots_no_longer_indexed_are_dropped() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let store = IndexStore::new();
        let mut service = idle_service(&store, &root);
        let gone = PathBuf::from("/Volumes/lokii-removed-share");
        service
            .control_tx
            .send(Control::Subtree {
                root: gone.clone(),
                entries: vec![make_entry("a.txt", "/Volumes/lokii-removed-share/a.txt", false)],
            })
            .unwrap();
        service.poll_control();
        assert!(store.is_empty());
    }

    #[test]
    fn checkpoint_is_absent_until_something_is_known() {
        let (control, _) = mpsc::channel();
        let handle = WatcherHandle {
            control,
            applied: Arc::new(AtomicU64::new(0)),
            fingerprint: Arc::new(AtomicU64::new(9)),
        };
        assert!(handle.checkpoint().is_none());
        handle.applied.store(42, Ordering::Relaxed);
        assert_eq!(
            handle.checkpoint(),
            Some(crate::cache::Checkpoint { event_id: 42, fingerprint: 9 })
        );
    }

    #[test]
    fn watcher_follows_index_root_changes_at_runtime() {
        let old_dir = TempDir::new().unwrap();
        let new_dir = TempDir::new().unwrap();
        // FSEvents reports resolved paths (/private/var/...), so index those.
        let old_root = old_dir.path().canonicalize().unwrap();
        let new_root = new_dir.path().canonicalize().unwrap();
        let mut config = AppConfig::default();
        config.index.index_paths = vec![old_root.to_string_lossy().into_owned()];
        config.index.exclude = vec![];
        config.index.exclude_paths = vec![];
        let config_store = crate::config::ConfigStore::new(config.clone());
        let store = IndexStore::new();
        let _handle = start_watcher(
            store.clone(),
            config_store.clone(),
            Arc::new(WatcherMetrics::default()),
            Resume::now(),
            Arc::new(std::sync::Mutex::new(())),
            |_| {},
            |_, _| {},
        );

        config.index.index_paths = vec![new_root.to_string_lossy().into_owned()];
        config_store.update(config);
        std::thread::sleep(Duration::from_millis(1500));
        let fresh = new_root.join("fresh.txt");
        let stale = old_root.join("stale.txt");
        fs::write(&fresh, "new").unwrap();
        fs::write(&stale, "old").unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        while !indexed(&store, &fresh) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(indexed(&store, &fresh), "new root is not watched");
        assert!(!indexed(&store, &stale), "removed root is still watched");
    }

    // --- is_hidden tests ---

    #[test]
    fn test_is_hidden_true() {
        assert!(is_hidden(Path::new("/Users/test/.hidden")));
    }

    #[test]
    fn test_is_hidden_false() {
        assert!(!is_hidden(Path::new("/Users/test/visible")));
    }

    #[test]
    fn test_is_hidden_dot_dir() {
        assert!(is_hidden(Path::new("/Users/test/.config")));
    }

    // --- file_entry_from_path tests ---

    #[test]
    fn test_file_entry_from_path_existing_file() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.txt");
        fs::write(&file_path, "hello world").unwrap();

        let entry = file_entry_from_path(&file_path);
        assert!(entry.is_some());
        let entry = entry.unwrap();
        assert_eq!(entry.name, "test.txt");
        assert!(!entry.is_dir);
        assert_eq!(entry.size, 11); // "hello world" is 11 bytes
        assert!(entry.modified > 0);
        assert!(entry.path.ends_with("test.txt"));
    }

    #[test]
    fn test_file_entry_from_path_nonexistent() {
        let entry = file_entry_from_path(Path::new("/nonexistent/path/abc.txt"));
        assert!(entry.is_none());
    }

    #[test]
    fn test_file_entry_from_path_directory() {
        let tmp = TempDir::new().unwrap();
        let dir_path = tmp.path().join("subdir");
        fs::create_dir(&dir_path).unwrap();

        let entry = file_entry_from_path(&dir_path);
        assert!(entry.is_some());
        let entry = entry.unwrap();
        assert_eq!(entry.name, "subdir");
        assert!(entry.is_dir);
    }

    #[test]
    fn test_process_event_batch_upsert_no_duplicates() {
        // When a file already exists in the index and a create event arrives,
        // upsert semantics should update (not duplicate) the entry.
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("existing.txt");
        fs::write(&file_path, "original").unwrap();

        let store = IndexStore::new();
        store.add_entries(vec![FileEntry {
            name: "existing.txt".to_string(),
            path: file_path.to_string_lossy().into_owned(),
            is_dir: false,
            size: 8,
            modified: 1700000000,
            created_at: 0,
            extension: "txt".to_string(),
        }]);
        assert_eq!(store.len(), 1);

        // Simulate a modify event that triggers upsert
        let batch = EventBatch {
            creates: vec![file_path],
            removes: vec![],
            dir_creates: vec![],
        };
        let (added, removed) = process_event_batch(batch, &store, &dummy_filter(), true);
        assert_eq!(added, 1);
        assert_eq!(removed, 1); // old entry removed via upsert
        assert_eq!(store.len(), 1); // no duplicate
    }

    // --- process_event_batch tests ---

    #[test]
    fn test_process_event_batch_creates() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("new_file.txt");
        fs::write(&file_path, "data").unwrap();

        let store = IndexStore::new();
        let batch = EventBatch {
            creates: vec![file_path],
            removes: vec![],
            dir_creates: vec![],
        };

        let (added, removed) = process_event_batch(batch, &store, &dummy_filter(), true);
        assert_eq!(added, 1);
        assert_eq!(removed, 0);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn test_process_event_batch_removes() {
        let store = IndexStore::new();
        store.add_entries(vec![FileEntry {
            name: "old.txt".to_string(),
            path: "/tmp/old.txt".to_string(),
            is_dir: false,
            size: 100,
            modified: 1700000000,
            created_at: 0,
            extension: "txt".to_string(),
        }]);
        assert_eq!(store.len(), 1);

        let batch = EventBatch {
            creates: vec![],
            removes: vec![PathBuf::from("/tmp/old.txt")],
            dir_creates: vec![],
        };

        let (added, removed) = process_event_batch(batch, &store, &dummy_filter(), true);
        assert_eq!(added, 0);
        assert_eq!(removed, 1);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_process_event_batch_filters_excluded() {
        let tmp = TempDir::new().unwrap();
        let nm_dir = tmp.path().join("node_modules");
        fs::create_dir(&nm_dir).unwrap();
        let file_path = nm_dir.join("pkg.js");
        fs::write(&file_path, "module.exports = {}").unwrap();

        let store = IndexStore::new();
        let batch = EventBatch {
            creates: vec![file_path],
            removes: vec![],
            dir_creates: vec![],
        };

        let (added, _removed) = process_event_batch(batch, &store, &dummy_filter(), true);
        assert_eq!(added, 0);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_process_event_batch_filters_hidden() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join(".hidden_file");
        fs::write(&file_path, "secret").unwrap();

        let store = IndexStore::new();
        let batch = EventBatch {
            creates: vec![file_path],
            removes: vec![],
            dir_creates: vec![],
        };

        // include_hidden = false should filter out hidden files
        let (added, _removed) = process_event_batch(batch, &store, &dummy_filter(), false);
        assert_eq!(added, 0);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_process_event_batch_includes_hidden_when_configured() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join(".hidden_file");
        fs::write(&file_path, "secret").unwrap();

        let store = IndexStore::new();
        let batch = EventBatch {
            creates: vec![file_path],
            removes: vec![],
            dir_creates: vec![],
        };

        // include_hidden = true should keep hidden files
        let (added, _removed) = process_event_batch(batch, &store, &dummy_filter(), true);
        assert_eq!(added, 1);
        assert_eq!(store.len(), 1);
    }

    // --- directory rename tests ---

    /// Helper to create a FileEntry for tests (mirrors indexer::tests::make_entry pattern)
    fn dummy_filter() -> crate::filter::CompiledFilter {
        let mut cfg = crate::config::AppConfig::default();
        cfg.index.index_paths = vec![];
        crate::filter::CompiledFilter::from_config(&cfg)
    }

    fn make_entry(name: &str, path: &str, is_dir: bool) -> FileEntry {
        let extension = if is_dir {
            String::new()
        } else {
            std::path::Path::new(name)
                .extension()
                .map(|ext| ext.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        };
        FileEntry {
            name: name.to_string(),
            path: path.to_string(),
            is_dir,
            size: 1024,
            modified: 1700000000,
            created_at: 0,
            extension,
        }
    }

    #[test]
    fn test_dir_rename_updates_child_paths() {
        // Use TempDir as parent so old and new dirs share the same parent (like real renames)
        let tmp = TempDir::new().unwrap();
        let old_dir = tmp.path().join("mydir");
        let new_dir = tmp.path().join("newdir");
        // Create new dir on disk (file_entry_from_path needs it); old dir doesn't need to exist
        fs::create_dir(&new_dir).unwrap();

        let old_prefix = old_dir.to_string_lossy().to_string();
        let new_prefix = new_dir.to_string_lossy().to_string();

        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("mydir", &old_prefix, true),
            make_entry("a.txt", &format!("{}/a.txt", old_prefix), false),
            make_entry("b.txt", &format!("{}/sub/b.txt", old_prefix), false),
        ]);

        let renames = vec![(old_prefix.clone(), new_dir.to_string_lossy().into_owned())];
        apply_batch(EventBatch::default(), renames, &store, &dummy_filter(), true).unwrap();

        let entries = store.entries();
        assert!(
            entries.iter().any(|e| e.path == new_prefix),
            "Directory entry should have new path"
        );
        assert!(
            entries
                .iter()
                .any(|e| e.path == format!("{}/a.txt", new_prefix)),
            "Child a.txt should have new prefix"
        );
        assert!(
            entries
                .iter()
                .any(|e| e.path == format!("{}/sub/b.txt", new_prefix)),
            "Child b.txt should have new prefix"
        );
        assert!(
            !entries.iter().any(|e| e.path.contains("mydir")),
            "No entries should contain old 'mydir' prefix"
        );
    }

    #[test]
    fn test_dir_rename_no_stale_entries() {
        let tmp = TempDir::new().unwrap();
        let old_dir = tmp.path().join("mydir");
        let new_dir = tmp.path().join("newdir");
        fs::create_dir(&new_dir).unwrap();

        let old_prefix = old_dir.to_string_lossy().to_string();

        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("mydir", &old_prefix, true),
            make_entry("a.txt", &format!("{}/a.txt", old_prefix), false),
            make_entry("b.txt", &format!("{}/sub/b.txt", old_prefix), false),
        ]);

        let renames = vec![(old_prefix.clone(), new_dir.to_string_lossy().into_owned())];
        apply_batch(EventBatch::default(), renames, &store, &dummy_filter(), true).unwrap();

        // 3 entries in, 3 entries out (rewritten in-place, not removed+re-added)
        assert_eq!(store.len(), 3, "Entry count should remain 3 after rename");

        let entries = store.entries();
        assert!(
            !entries.iter().any(|e| e.path.contains("mydir")),
            "No stale entries with old prefix"
        );
    }

    #[test]
    fn test_dir_rename_nested() {
        // Deep nesting: parent/a/b/c/file.txt — rename parent/a to parent/z
        let tmp = TempDir::new().unwrap();
        let old_dir = tmp.path().join("a");
        let new_dir = tmp.path().join("z");
        fs::create_dir(&new_dir).unwrap();

        let old_prefix = old_dir.to_string_lossy().to_string();
        let new_prefix = new_dir.to_string_lossy().to_string();

        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("a", &old_prefix, true),
            make_entry("b", &format!("{}/b", old_prefix), true),
            make_entry("c", &format!("{}/b/c", old_prefix), true),
            make_entry("file.txt", &format!("{}/b/c/file.txt", old_prefix), false),
        ]);

        let renames = vec![(old_prefix.clone(), new_dir.to_string_lossy().into_owned())];
        apply_batch(EventBatch::default(), renames, &store, &dummy_filter(), true).unwrap();

        let entries = store.entries();
        assert!(
            entries
                .iter()
                .any(|e| e.path == format!("{}/b/c/file.txt", new_prefix)),
            "Deeply nested file should have updated path"
        );
        assert!(
            !entries.iter().any(|e| e.path.contains("/a/")),
            "No entries should contain old /a/ prefix"
        );
    }

    #[test]
    fn test_dir_rename_to_excluded_removes_children() {
        // Rename dir to "node_modules" (excluded) — all children should be removed
        let tmp = TempDir::new().unwrap();
        let old_dir = tmp.path().join("mydir");
        let new_dir = tmp.path().join("node_modules");
        fs::create_dir(&new_dir).unwrap();

        let old_prefix = old_dir.to_string_lossy().to_string();

        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("mydir", &old_prefix, true),
            make_entry("a.txt", &format!("{}/a.txt", old_prefix), false),
            make_entry("b.txt", &format!("{}/b.txt", old_prefix), false),
        ]);

        let renames = vec![(old_prefix.clone(), new_dir.to_string_lossy().into_owned())];
        apply_batch(EventBatch::default(), renames, &store, &dummy_filter(), true).unwrap();

        assert_eq!(
            store.len(),
            0,
            "All entries should be removed when renamed to excluded directory"
        );
    }

    #[test]
    fn test_prefix_boundary_no_false_match() {
        // parent/bar + child, AND parent/barista/y.txt — rename parent/bar to parent/baz
        // barista entry must NOT be affected
        let tmp = TempDir::new().unwrap();
        let old_dir = tmp.path().join("bar");
        let new_dir = tmp.path().join("baz");
        fs::create_dir(&new_dir).unwrap();

        let old_prefix = old_dir.to_string_lossy().to_string();
        let new_prefix = new_dir.to_string_lossy().to_string();
        let barista_path = format!("{}/y.txt", tmp.path().join("barista").to_string_lossy());

        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("bar", &old_prefix, true),
            make_entry("x.txt", &format!("{}/x.txt", old_prefix), false),
            make_entry("y.txt", &barista_path, false),
        ]);

        let renames = vec![(old_prefix.clone(), new_dir.to_string_lossy().into_owned())];
        apply_batch(EventBatch::default(), renames, &store, &dummy_filter(), true).unwrap();

        let entries = store.entries();

        // bar entries updated
        assert!(
            entries.iter().any(|e| e.path == new_prefix),
            "bar dir should be renamed to baz"
        );
        assert!(
            entries
                .iter()
                .any(|e| e.path == format!("{}/x.txt", new_prefix)),
            "bar/x.txt should be renamed"
        );

        // barista entry unchanged
        assert!(
            entries.iter().any(|e| e.path == barista_path),
            "barista/y.txt should be unaffected"
        );
    }

    #[test]
    fn test_file_rename_still_works() {
        // Simple file rename — existing behavior must be preserved
        let tmp = TempDir::new().unwrap();
        let new_file = tmp.path().join("new.txt");
        fs::write(&new_file, "content").unwrap();

        let store = IndexStore::new();
        store.add_entries(vec![make_entry("old.txt", "/tmp/old.txt", false)]);

        let batch = EventBatch {
            removes: vec![PathBuf::from("/tmp/old.txt")],
            creates: vec![new_file.clone()],
            dir_creates: vec![],
        };

        process_event_batch(batch, &store, &dummy_filter(), true);

        let entries = store.entries();
        let new_path = new_file.to_string_lossy().to_string();
        assert!(
            entries.iter().any(|e| e.path == new_path),
            "New file should be in index"
        );
        assert!(
            !entries.iter().any(|e| e.path == "/tmp/old.txt"),
            "Old file should be removed from index"
        );
    }

    // --- New tests for FSEvents rename fix ---

    #[test]
    fn test_cascade_remove_directory_children() {
        // When a directory is removed, all children in the index should also be removed.
        // This handles the case where detect_dir_renames() pairing fails.
        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("mydir", "/tmp/mydir", true),
            make_entry("a.txt", "/tmp/mydir/a.txt", false),
            make_entry("sub", "/tmp/mydir/sub", true),
            make_entry("b.txt", "/tmp/mydir/sub/b.txt", false),
            make_entry("other.txt", "/tmp/other.txt", false),
        ]);
        assert_eq!(store.len(), 5);

        let batch = EventBatch {
            creates: vec![],
            removes: vec![PathBuf::from("/tmp/mydir")],
            dir_creates: vec![],
        };

        let (_added, removed) = process_event_batch(batch, &store, &dummy_filter(), true);
        // Should remove mydir + 3 children = 4 total
        assert_eq!(removed, 4);
        assert_eq!(store.len(), 1);
        let entries = store.entries();
        assert_eq!(entries[0].path, "/tmp/other.txt");
    }

    #[test]
    fn test_cascade_remove_respects_prefix_boundary() {
        // /tmp/bar removal should NOT cascade to /tmp/barista/file.txt
        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("bar", "/tmp/bar", true),
            make_entry("x.txt", "/tmp/bar/x.txt", false),
            make_entry("y.txt", "/tmp/barista/y.txt", false),
        ]);

        let batch = EventBatch {
            creates: vec![],
            removes: vec![PathBuf::from("/tmp/bar")],
            dir_creates: vec![],
        };

        process_event_batch(batch, &store, &dummy_filter(), true);
        assert_eq!(store.len(), 1);
        let entries = store.entries();
        assert_eq!(entries[0].path, "/tmp/barista/y.txt");
    }

}
