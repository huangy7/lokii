use crate::config::{AppConfig, ConfigStore, IndexConfig};
use crate::filter::{CompiledFilter, ScopeDecision};
use crate::indexer::{FileEntry, IndexStore};
use crate::metrics::WatcherMetrics;
use crate::path_utils::paths_equal_ci;
use crate::permissions::has_full_disk_access;
use crate::scanner;
use crate::scope_diff::{diff_primary_scope_runtime, PrimaryScopeAction, PrimaryScopeSnapshot};
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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

pub(crate) fn apply_batch(
    mut batch: EventBatch,
    renames: Vec<(String, String)>,
    store: &IndexStore,
    filter: &CompiledFilter,
    include_hidden: bool,
) -> Result<(usize, usize), crate::compact_index::IndexError> {
    let mut excluded_renames = Vec::new();
    let mut shallow_rename_entries = Vec::new();
    let mut shallow_remove_roots = Vec::new();
    for (_, new) in &renames {
        let new_path = Path::new(new);
        let new_is_dir = std::fs::metadata(new_path).map(|m| m.is_dir()).unwrap_or(true);
        match filter.evaluate_with_hidden(new_path, new_is_dir, include_hidden) {
            ScopeDecision::HardDrop => excluded_renames.push(new.clone()),
            ScopeDecision::IndexShallowContainer => {
                // 改名落入浅层容器时保留容器目录自身并清退原目录下所有子孙条目
                if let Some(entry) = file_entry_from_path(new_path) {
                    shallow_remove_roots.push(new.clone());
                    shallow_rename_entries.push(entry);
                } else {
                    excluded_renames.push(new.clone());
                }
            }
            ScopeDecision::IndexNormally => {}
        }
    }
    for (old, new) in &renames {
        batch.removes.retain(|path| path.to_string_lossy() != *old);
        batch.creates.retain(|path| path.to_string_lossy() != *new);
        batch
            .dir_creates
            .retain(|path| path.to_string_lossy() != *new);
    }
    // DirtyQueue 会把新建目录的子项事件合并掉，普通目录需回扫补齐；
    // 浅层容器目录仅收录自身条目并清除残留子项，不触发子目录扫描。
    let mut dir_roots = Vec::new();
    let mut shallow_dir_paths = Vec::new();
    for path in &batch.dir_creates {
        match filter.evaluate_with_hidden(path, true, include_hidden) {
            ScopeDecision::HardDrop => {}
            ScopeDecision::IndexShallowContainer => shallow_dir_paths.push(path.clone()),
            ScopeDecision::IndexNormally => dir_roots.push(path.clone()),
        }
    }
    let mut creates: Vec<_> = batch
        .creates
        .iter()
        .filter(|path| !dir_roots.iter().any(|root| root == *path))
        .filter(|path| !shallow_dir_paths.iter().any(|root| root == *path))
        .filter_map(|path| {
            let entry = file_entry_from_path(path)?;
            match filter.evaluate_with_hidden(path, entry.is_dir, include_hidden) {
                ScopeDecision::HardDrop => None,
                ScopeDecision::IndexShallowContainer => {
                    shallow_remove_roots.push(path.to_string_lossy().into_owned());
                    Some(entry)
                }
                ScopeDecision::IndexNormally => Some(entry),
            }
        })
        .collect();
    creates.extend(shallow_rename_entries);
    for path in &shallow_dir_paths {
        if let Some(entry) = file_entry_from_path(path) {
            shallow_remove_roots.push(path.to_string_lossy().into_owned());
            creates.push(entry);
        }
    }
    if !dir_roots.is_empty() {
        creates.extend(scanner::scan_directories(
            &dir_roots,
            Arc::new(filter.clone()),
            include_hidden,
            Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        ));
    }
    // 回扫目录与浅层容器均先删旧子树再追加，避免改名重建或容器切入后遗留旧子项。
    let remove_roots: Vec<String> = batch
        .removes
        .iter()
        .chain(&dir_roots)
        .map(|path| path.to_string_lossy().into_owned())
        .chain(shallow_remove_roots)
        .collect();
    store
        .write()
        .apply_event_batch(&renames, &excluded_renames, &remove_roots, creates)
}

/// 无新事件多久后落一批。
pub(crate) const DEFAULT_DEBOUNCE_MS: u64 = 500;

// ── Safe Watch Paths ───────────────────────────────────────────────────────

fn collect_safe_watch_paths(
    path: &Path,
    filter: &CompiledFilter,
    protected: &[PathBuf],
    out: &mut Vec<PathBuf>,
) {
    // Evaluate pure in-memory exclusion and TCC checks before statting the path
    // so excluded mount subtrees or ungranted directories are never touched on disk.
    if filter.is_excluded(path) {
        return;
    }

    if scanner::is_protected_or_descendant(path, protected) {
        return;
    }

    if !path.exists() {
        return;
    }

    match filter.evaluate(path, path.is_dir()) {
        ScopeDecision::HardDrop | ScopeDecision::IndexShallowContainer => return,
        ScopeDecision::IndexNormally => {}
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

fn watch_scope_for_paths_with_fda(
    paths: Vec<PathBuf>,
    filter: &CompiledFilter,
    has_fda: bool,
) -> (bool, Vec<PathBuf>, Vec<PathBuf>) {
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
pub(crate) fn resolve_changes(
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
pub(crate) const MAX_DEBOUNCE: Duration = Duration::from_millis(2000);
pub(crate) const MAX_BATCH_SIZE: usize = 10_000;
/// 仅用户态丢事件时，静默这么久后从已应用位置重开流重放；连续这么多次仍丢则改为全量对账。
const GAP_QUIET: Duration = Duration::from_secs(2);
const GAP_REPLAYS: u32 = 3;
/// 兜底：变更再少也最多隔这么久落一次盘，避免检查点比 FSEvents 历史还旧。
pub(crate) const PERSIST_EVERY: Duration = Duration::from_secs(12 * 3600);
/// 未落盘的索引事件达到这么多条就先落一次盘：与启动回放要处理的事件数一一对应；取 REPLAY_PATH_LIMIT 的 1/4 留余量。
pub(crate) const PERSIST_EVENTS: usize = 50_000;
/// 全量对账拿不到扫描锁（重建进行中）时的重试间隔，与扫描出错后的退避。
const RECONCILE_RETRY: Duration = Duration::from_secs(2);
const RECONCILE_BACKOFF: Duration = Duration::from_secs(60);
/// 15 秒周期自检中本地主根暂时消失的抗抖动宽限时长：超过该时长仍不存在则退出 scope 并清理，
/// 既防止构建脚本瞬时替换目录引发双扫，又避免真实删移后的根目录被永久钉在 scope 中。
const MISSING_ROOT_GRACE: Duration = Duration::from_secs(60);

/// 索引服务从哪里接上。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resume {
    /// 从该事件 ID 起流：先执行可选启动期范围 Diff/补扫与历史回放，再转入实时监听。
    From {
        event_id: u64,
        saved_snapshot: Option<PrimaryScopeSnapshot>,
        upgrade_rescans: Vec<PathBuf>,
        needs_v6_persist: bool,
    },
    /// 没有可信检查点：先全量对账，再从扫描前的事件 ID 起流。
    Verify,
}

impl Resume {
    /// 从系统当前事件 ID 起，只看此后的变更。
    pub fn now() -> Self {
        Self::from_id(crate::fsevents::current_event_id())
    }

    pub fn from_id(event_id: u64) -> Self {
        Resume::From {
            event_id,
            saved_snapshot: None,
            upgrade_rescans: Vec::new(),
            needs_v6_persist: false,
        }
    }

    pub fn with_snapshot(
        event_id: u64,
        saved_snapshot: Option<PrimaryScopeSnapshot>,
        upgrade_rescans: Vec<PathBuf>,
        needs_v6_persist: bool,
    ) -> Self {
        Resume::From {
            event_id,
            saved_snapshot,
            upgrade_rescans,
            needs_v6_persist,
        }
    }
}

enum Control {
    Replace {
        index: crate::compact_index::CompactIndex,
        checkpoint: crate::cache::Checkpoint,
        snapshot: PrimaryScopeSnapshot,
        done: mpsc::Sender<()>,
    },
    /// 非阻塞配置刷新唤醒：消除 500ms 轮询延迟。
    RefreshScope,
}

/// 运行中的索引服务。
#[derive(Clone)]
pub struct WatcherHandle {
    control: mpsc::Sender<Control>,
    /// 指向当前活跃事件通道的唤醒发送端：投递空批次可立刻打断 `recv_timeout(500ms)` 轮询等待。
    wake_tx: Arc<std::sync::Mutex<Option<mpsc::Sender<Vec<crate::fsevents::Event>>>>>,
    /// 已完整写入索引的最大事件 ID（0 表示尚无可信检查点）。
    pub applied: Arc<AtomicU64>,
    /// 索引内容所对应的索引范围指纹（不一定是当前配置的）。
    fingerprint: Arc<AtomicU64>,
    /// 索引当前已完整对齐的结构化范围快照。
    applied_snapshot: Arc<RwLock<PrimaryScopeSnapshot>>,
    /// 存在待补扫快照（未 flush 的增量更新）时置 true，禁止半途存盘写出过期快照。
    pending_snapshot_active: Arc<AtomicBool>,
    /// 外部配置更新前递增的脏序号；与 `applied_scope_seq` 不等期间禁止存盘写出过期快照。
    scope_dirty_seq: Arc<AtomicU64>,
    applied_scope_seq: Arc<AtomicU64>,
}

impl WatcherHandle {
    /// 当前是否允许安全落盘（无未完成补扫且无未对账的配置更新）。
    pub fn is_persist_safe(&self) -> bool {
        !self.pending_snapshot_active.load(Ordering::Acquire)
            && self.scope_dirty_seq.load(Ordering::Acquire)
                == self.applied_scope_seq.load(Ordering::Acquire)
    }

    /// 在配置写入前递增脏序号，防止并发 `persist_now()` 或退出 `persist_index()` 写出旧快照。
    pub fn mark_scope_dirty(&self) -> u64 {
        self.scope_dirty_seq.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// 非阻塞唤醒 watcher 立即执行范围 Diff 对账（同步发送空批次打断 500ms 等待，亚毫秒级生效）。
    pub fn request_scope_refresh(&self) {
        let _ = self.control.send(Control::RefreshScope);
        if let Some(tx) = self.wake_tx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = tx.send(Vec::new());
        }
    }

    /// 读取当前索引已完整对齐的结构化范围快照，供 `preview_primary_scope_action` 比对。
    pub fn current_applied_snapshot(&self) -> PrimaryScopeSnapshot {
        self.applied_snapshot.read().clone()
    }

    /// 当前索引的回放检查点。存在半途补扫或未对账预裁时返回 `None`，阻止退出存盘破坏一致性。
    pub fn checkpoint(&self) -> Option<crate::cache::Checkpoint> {
        if !self.is_persist_safe() {
            return None;
        }
        let event_id = self.applied.load(Ordering::Acquire);
        (event_id != 0).then(|| crate::cache::Checkpoint {
            event_id,
            fingerprint: self.fingerprint.load(Ordering::Acquire),
        })
    }

    /// 连同结构化快照一起读取当前可安全落盘的 v6 检查点。
    pub fn checkpoint_with_snapshot(&self) -> Option<(crate::cache::Checkpoint, PrimaryScopeSnapshot)> {
        if !self.is_persist_safe() {
            return None;
        }
        let event_id = self.applied.load(Ordering::Acquire);
        if event_id == 0 {
            return None;
        }
        let snapshot = self.applied_snapshot.read().clone();
        let checkpoint = crate::cache::Checkpoint {
            event_id,
            fingerprint: self.fingerprint.load(Ordering::Acquire),
        };
        if !self.is_persist_safe() {
            return None;
        }
        Some((checkpoint, snapshot))
    }

    /// 由服务线程换入整体重建的索引；`checkpoint` 是重建扫描开始前取的，服务会从其事件 ID 起重放补齐。
    /// 服务已退出时原样交还索引，由调用方自行替换。
    pub fn replace(
        &self,
        index: crate::compact_index::CompactIndex,
        checkpoint: crate::cache::Checkpoint,
        snapshot: PrimaryScopeSnapshot,
    ) -> Result<(), crate::compact_index::CompactIndex> {
        let (done, replaced) = mpsc::channel();
        match self.control.send(Control::Replace {
            index,
            checkpoint,
            snapshot,
            done,
        }) {
            Ok(()) => {
                let _ = replaced.recv();
                Ok(())
            }
            Err(mpsc::SendError(Control::Replace { index, .. })) => Err(index),
            Err(_) => unreachable!(),
        }
    }
}

/// 当前配置决定的监听范围。
struct Scope {
    snapshot: PrimaryScopeSnapshot,
    fingerprint: u64,
    filter: CompiledFilter,
    has_fda: bool,
    protected: Vec<PathBuf>,
    /// 挂流的根（无 FDA 时绕开受保护目录）。
    watch_paths: Vec<PathBuf>,
    /// 全量对账扫描的根。
    scan_paths: Vec<PathBuf>,
    include_hidden: bool,
}

impl Scope {
    fn new(config: &AppConfig) -> Self {
        Self::new_with_fda(config, has_full_disk_access())
    }

    fn new_with_fda(config: &AppConfig, has_fda: bool) -> Self {
        let scan_paths = config.resolve_paths();
        let filter = CompiledFilter::from_config(config);
        let (has_fda, protected, watch_paths) =
            watch_scope_for_paths_with_fda(scan_paths.clone(), &filter, has_fda);
        let snapshot = PrimaryScopeSnapshot::from_inputs(config, &scan_paths, &[], has_fda);
        let fingerprint = snapshot.fingerprint_fnv1a();
        Self {
            snapshot,
            fingerprint,
            filter,
            has_fda,
            protected,
            watch_paths,
            scan_paths,
            include_hidden: config.index.include_hidden,
        }
    }

    fn new_primary(config: &AppConfig, volumes_dir: &Path) -> Self {
        let (primary_paths, excluded_mount_roots) = primary_scope_inputs(config, volumes_dir);
        Self::new_primary_with_inputs(
            config,
            primary_paths,
            excluded_mount_roots,
            has_full_disk_access(),
        )
    }

    fn new_primary_with_inputs(
        config: &AppConfig,
        primary_paths: Vec<PathBuf>,
        excluded_mount_roots: Vec<PathBuf>,
        has_fda: bool,
    ) -> Self {
        let filter =
            CompiledFilter::from_primary_config(config, &primary_paths, &excluded_mount_roots);
        let (has_fda, protected, watch_paths) =
            watch_scope_for_paths_with_fda(primary_paths.clone(), &filter, has_fda);
        let snapshot = PrimaryScopeSnapshot::from_inputs(
            config,
            &primary_paths,
            &excluded_mount_roots,
            has_fda,
        );
        let fingerprint = snapshot.fingerprint_fnv1a();
        Self {
            snapshot,
            fingerprint,
            filter,
            has_fda,
            protected,
            watch_paths,
            scan_paths: primary_paths,
            include_hidden: config.index.include_hidden,
        }
    }

    /// 内存裁剪统一谓词：同时涵盖 FDA 受保护目录、大小写无关排除前缀、穿透子根保护、敏感名与按根 `.app` 浅层容器。
    fn retains_entry(&self, path: &Path, is_dir: bool) -> bool {
        (self.has_fda || !scanner::is_protected_or_descendant(path, &self.protected))
            && self
                .filter
                .evaluate_with_hidden(path, is_dir, self.include_hidden)
                != ScopeDecision::HardDrop
    }

    /// 事件是否该处理：先剔除受保护路径与策略硬丢弃项，免得逐条 stat（受保护路径还会触发 TCC 弹窗）。
    fn admits(&self, path: &Path) -> bool {
        self.retains_entry(path, true)
    }
}

/// 纯周期自检抗抖动：既有本地主根从 `current_roots` 消失但不在 `excluded_mount_roots` 中时，
/// 仅在 `grace` 宽限期内保留以防外部构建瞬时替换目录引发双扫；超过宽限期仍缺失则允许其退出 scope。
fn apply_missing_root_grace(
    prev_roots: &[PathBuf],
    current_roots: &mut Vec<PathBuf>,
    excluded_mount_roots: &[PathBuf],
    missing_since: &mut Vec<(PathBuf, Instant)>,
    now: Instant,
    grace: Duration,
) {
    // 清理已恢复出现、已迁移为挂载排除根或已不再属于旧根集合的计时项
    missing_since.retain(|(path, _)| {
        prev_roots.iter().any(|prev| paths_equal_ci(prev, path))
            && !current_roots.iter().any(|cur| paths_equal_ci(cur, path))
            && !excluded_mount_roots.iter().any(|m| paths_equal_ci(m, path))
    });

    for prev_root in prev_roots {
        if current_roots.iter().any(|p| paths_equal_ci(p, prev_root))
            || excluded_mount_roots
                .iter()
                .any(|m| paths_equal_ci(m, prev_root))
        {
            continue;
        }
        if let Some(pos) = missing_since
            .iter()
            .position(|(p, _)| paths_equal_ci(p, prev_root))
        {
            let first_missing = missing_since[pos].1;
            if now.saturating_duration_since(first_missing) < grace {
                current_roots.push(prev_root.clone());
            } else {
                missing_since.remove(pos);
            }
        } else {
            missing_since.push((prev_root.clone(), now));
            current_roots.push(prev_root.clone());
        }
    }
}

pub(crate) fn primary_scope_inputs(
    config: &AppConfig,
    volumes_dir: &Path,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let manifest = crate::volumes::VolumeManifest::load_from_dir(volumes_dir);
    // 仅需挂载点路径做域划分，采用免 getattrlist 的轻量内核快照，绝不唤醒休眠外置机械硬盘。
    let mounts = crate::volumes::list_mounted_volumes_fast();
    primary_scope_inputs_with(config, &manifest, &mounts)
}

pub(crate) fn primary_scope_inputs_with(
    config: &AppConfig,
    manifest: &crate::volumes::VolumeManifest,
    mounts: &[crate::volumes::MountedFsInfo],
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let partition = crate::volumes::partition_index_paths(config, manifest, mounts);
    // Excluded mount roots track stable configured mount targets; active mount points are added
    // only when nested under a primary root so unmounts or -1 mount drift do not churn the primary
    // domain fingerprint.
    let mut excluded_mount_roots: Vec<PathBuf> = Vec::new();
    for target in &partition.mounted_targets {
        if !excluded_mount_roots
            .iter()
            .any(|p| paths_equal_ci(p, &target.configured_path))
        {
            excluded_mount_roots.push(target.configured_path.clone());
        }
        if let Some(ref resolved) = target.mounted {
            if partition
                .primary_paths
                .iter()
                .any(|root| crate::path_utils::path_starts_with_ci(&resolved.mount_root, root))
                && !excluded_mount_roots
                    .iter()
                    .any(|p| paths_equal_ci(p, &resolved.mount_root))
            {
                excluded_mount_roots.push(resolved.mount_root.clone());
            }
        }
    }
    excluded_mount_roots.sort();
    excluded_mount_roots.dedup();
    (partition.primary_paths, excluded_mount_roots)
}

/// 刚获得完全磁盘访问时，原先受保护、现在可读且在索引范围内的目录。
#[cfg(test)]
fn newly_readable(protected: &[PathBuf], scope: &Scope) -> Vec<PathBuf> {
    protected
        .iter()
        .filter(|path| scope.scan_paths.iter().any(|root| path.starts_with(root)))
        .filter(|path| path.exists() && scope.admits(path))
        .cloned()
        .collect()
}

/// 全量对账扫描期间每 50ms 推送一次实时扫描计数的 RAII 轮询器。
struct ReconcileProgressPoller {
    done: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ReconcileProgressPoller {
    fn start(
        progress: Arc<AtomicUsize>,
        callback: Arc<dyn Fn(&str, u64) + Send + Sync>,
    ) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let poll_done = Arc::clone(&done);
        let handle = std::thread::spawn(move || {
            while !poll_done.load(Ordering::Relaxed) {
                let count = progress.load(Ordering::Relaxed);
                callback("rebuilding", count as u64);
                std::thread::park_timeout(Duration::from_millis(50));
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

impl Drop for ReconcileProgressPoller {
    fn drop(&mut self) {
        self.finish();
    }
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
    /// 上次低频复核本地根目录存在性与嵌套挂载点排除集合的时刻
    scope_refreshed: Instant,
    /// 上次探测完全磁盘访问权限的时刻，避免每 500ms 高频打开系统 TCC.db
    fda_checked: Instant,
    stream: Option<crate::fsevents::Stream>,
    events: mpsc::Receiver<Vec<crate::fsevents::Event>>,
    wake_tx: Arc<std::sync::Mutex<Option<mpsc::Sender<Vec<crate::fsevents::Event>>>>>,
    control: mpsc::Receiver<Control>,
    applied: Arc<AtomicU64>,
    /// 索引内容对应的范围指纹；与 `scope.fingerprint` 不同即索引落后于配置，需全量对账。
    fingerprint: Arc<AtomicU64>,
    /// 索引当前已完整对齐的结构化快照。
    applied_snapshot: Arc<RwLock<PrimaryScopeSnapshot>>,
    /// 补扫完成后才生效的新快照（新增根、移除嵌套卷、获得 FDA 等增量补扫期间）。
    pending_snapshot: Option<PrimaryScopeSnapshot>,
    /// 延后至 `flush()` 执行的增量前裁剪标记：未 flush 前保持 `store` 与 `applied_snapshot` 严格一致，
    /// 防止补扫未完成时配置来回切换（如 `[A,B]→[A,C]→[A,B]` 或 `[A,B,D]`）造成提前裁掉的条目永久丢失。
    pending_prune: bool,
    pending_snapshot_active: Arc<AtomicBool>,
    scope_dirty_seq: Arc<AtomicU64>,
    applied_scope_seq: Arc<AtomicU64>,
    pending_scope_seq: Option<u64>,
    /// 纯 15 秒周期自检中本地主根首次缺失的时刻表，超 `MISSING_ROOT_GRACE` 后停止补回。
    missing_root_since: Vec<(PathBuf, Instant)>,
    pending: HashMap<PathBuf, Change>,
    /// 已收下（含已应用）事件的最大 ID；一批应用完后 `applied` 推进到这里。
    seen: u64,
    first_pending: Option<Instant>,
    /// 与范围 diff 无关的回扫队列（v5->v6 `.app` 升级补扫、MUST_SCAN_SUBDIRS、ROOT_CHANGED 原位替换），
    /// 配置热切换走 NoOp/PruneOnly/IncrementalUpdate 时必须保留，直至 flush 扫完。
    rescans: Vec<PathBuf>,
    /// 仅由 `diff_primary_scope` 产出的范围增量补扫根，随下一次范围 diff 覆盖或清空。
    scope_rescans: Vec<PathBuf>,
    /// 监听根收到 `ROOT_CHANGED` 原位替换后，在本次 flush 补扫完重开流以绑定新目录 inode。
    root_changed_restart: bool,
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
    /// 上次落盘后收下的索引事件数，对应下次启动要回放的量。
    unpersisted_events: usize,
    primary_volumes_dir: Option<PathBuf>,
    last_index_config: IndexConfig,
    on_config_updated: Option<Arc<dyn Fn(&AppConfig) + Send + Sync>>,
    on_reconcile_progress: Option<Arc<dyn Fn(&str, u64) + Send + Sync>>,
    on_reconcile_ready: Option<Arc<dyn Fn() + Send + Sync>>,
    on_reconcile_error: Option<Arc<dyn Fn(String) + Send + Sync>>,
    on_scope_applied: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl<F, P> Service<F, P>
where
    F: Fn(usize),
    P: Fn(&[u8], crate::cache::Checkpoint, &PrimaryScopeSnapshot),
{
    fn can_persist(&self) -> bool {
        self.pending_snapshot.is_none()
            && self.scope_dirty_seq.load(Ordering::Acquire)
                == self.applied_scope_seq.load(Ordering::Acquire)
    }

    fn set_pending_snapshot(&mut self, snapshot: Option<PrimaryScopeSnapshot>, seq: Option<u64>) {
        self.pending_snapshot_active
            .store(snapshot.is_some(), Ordering::Release);
        self.pending_snapshot = snapshot;
        self.pending_scope_seq = seq;
    }

    fn run(mut self, resume: Resume) {
        match resume {
            Resume::From {
                event_id,
                saved_snapshot,
                upgrade_rescans,
                needs_v6_persist,
            } if event_id <= crate::fsevents::current_event_id() => {
                if let Some(saved) = saved_snapshot {
                    let saved_fp = saved.fingerprint_fnv1a();
                    *self.applied_snapshot.write() = saved.clone();
                    self.fingerprint.store(saved_fp, Ordering::Release);
                    match diff_primary_scope_runtime(&saved, &self.scope.snapshot) {
                        PrimaryScopeAction::NoOp => {
                            self.catching_up = true;
                            self.restart(event_id);
                            if !upgrade_rescans.is_empty() {
                                self.rescans.extend(upgrade_rescans);
                                self.set_pending_snapshot(Some(self.scope.snapshot.clone()), None);
                                self.first_pending = Some(Instant::now());
                            } else {
                                *self.applied_snapshot.write() = self.scope.snapshot.clone();
                                self.fingerprint
                                    .store(self.scope.fingerprint, Ordering::Release);
                                if needs_v6_persist {
                                    self.dirty = true;
                                }
                            }
                        }
                        PrimaryScopeAction::PruneOnly => {
                            // 裁剪前先置位 pending_snapshot_active，阻断并发退出 persist_index 写出“已裁索引 + 旧快照”
                            self.pending_snapshot_active.store(true, Ordering::Release);
                            self.store.retain_paths(|path, is_dir| {
                                self.scope.retains_entry(Path::new(path), is_dir)
                            });
                            (self.on_batch)(self.store.len());
                            self.catching_up = true;
                            self.restart(event_id);
                            if !upgrade_rescans.is_empty() {
                                self.rescans.extend(upgrade_rescans);
                                self.set_pending_snapshot(Some(self.scope.snapshot.clone()), None);
                                self.first_pending = Some(Instant::now());
                            } else {
                                *self.applied_snapshot.write() = self.scope.snapshot.clone();
                                self.fingerprint
                                    .store(self.scope.fingerprint, Ordering::Release);
                                self.pending_snapshot_active.store(false, Ordering::Release);
                                self.dirty = true;
                            }
                        }
                        PrimaryScopeAction::IncrementalUpdate {
                            prune_first,
                            scan_roots,
                        } => {
                            self.catching_up = true;
                            self.restart(event_id);
                            self.scope_rescans = scan_roots;
                            for dir in upgrade_rescans {
                                if !self.scope_rescans.iter().any(|r| paths_equal_ci(r, &dir)) {
                                    self.rescans.push(dir);
                                }
                            }
                            self.pending_prune = prune_first;
                            self.set_pending_snapshot(Some(self.scope.snapshot.clone()), None);
                            self.first_pending = Some(Instant::now());
                        }
                        PrimaryScopeAction::FullRebuild => {
                            self.applied.store(0, Ordering::Release);
                            self.reconcile_all();
                        }
                    }
                } else {
                    self.catching_up = true;
                    self.restart(event_id);
                    if !upgrade_rescans.is_empty() {
                        self.rescans.extend(upgrade_rescans);
                        self.set_pending_snapshot(Some(self.scope.snapshot.clone()), None);
                        self.first_pending = Some(Instant::now());
                    } else if needs_v6_persist {
                        self.dirty = true;
                    }
                }
            }
            // 事件 ID 比系统当前还新或无检查点：先全量对账。
            _ => {
                self.applied.store(0, Ordering::Release);
                self.reconcile_all();
            }
        }
        loop {
            self.poll_control();
            self.poll_scope(false);
            self.poll_gap();
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
        if !self.can_persist() {
            return;
        }
        if self.dirty
            && (self.unpersisted_events >= PERSIST_EVENTS
                || self.last_persist.elapsed() >= PERSIST_EVERY)
        {
            self.persist_now();
        }
    }

    fn persist_now(&mut self) {
        // 处于先删后补中间态或外部预裁尚未对账时跳过存盘，绝不写出“已裁索引 + 旧快照”
        if !self.can_persist() {
            return;
        }
        // 仅在短读锁内提取检查点并完成内存编码，立即释放读锁后再把字节流交给回调落盘与 sync_data()，
        // 防止在 parking_lot::RwLock 写优先策略下因慢磁盘 fsync 引发写锁与 UI 搜索读锁的级联阻塞。
        let payload = {
            let index = self.store.read();
            let event_id = self.applied.load(Ordering::Acquire);
            (event_id != 0).then(|| {
                let snapshot = self.applied_snapshot.read().clone();
                let checkpoint = crate::cache::Checkpoint {
                    event_id,
                    fingerprint: self.fingerprint.load(Ordering::Acquire),
                };
                (crate::cache::encode_compact(&index), checkpoint, snapshot)
            })
        };
        // 编码后再次校验脏序号，防止编码瞬间并发配置预裁混入已裁剪内存索引
        if !self.can_persist() {
            self.dirty = true;
            return;
        }
        self.dirty = false;
        self.unpersisted_events = 0;
        self.last_persist = Instant::now();
        if let Some((encoded, checkpoint, snapshot)) = payload {
            (self.persist)(&encoded, checkpoint, &snapshot);
        }
    }

    /// 是否在等全量对账（期间的事件都会被对账扫描与其后的重放覆盖，不必再攒）。
    fn reconcile_pending(&self) -> bool {
        let pending_matches_scope = self
            .pending_snapshot
            .as_ref()
            .is_some_and(|snap| snap.fingerprint_fnv1a() == self.scope.fingerprint);
        let stale_scope = self.fingerprint.load(Ordering::Acquire) != self.scope.fingerprint
            && !pending_matches_scope;
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
            HISTORY_DONE, IDS_WRAPPED, KERNEL_DROPPED, MUST_SCAN_SUBDIRS, USER_DROPPED,
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
            } else if event.flags & MUST_SCAN_SUBDIRS != 0 {
                self.request_rescan(event.path, event.id);
            } else if self
                .scope
                .scan_paths
                .iter()
                .any(|root| paths_equal_ci(&event.path, root))
            {
                // 监听根自身的普通/创建事件绝不送入 pending 触发 dir_creates 同步全扫；
                // 但非系统根 `/` 的删除、移走或原位目录替换（RootChanged）必须精准收敛：
                // 路径已消失时仅投递删除批次清掉该子树，原位替换时仅回扫该根，绝不连坐其它根。
                if event.path != Path::new("/")
                    && event.flags
                        & (crate::fsevents::ITEM_REMOVED
                            | crate::fsevents::ITEM_RENAMED
                            | crate::fsevents::ROOT_CHANGED)
                        != 0
                {
                    if std::fs::symlink_metadata(&event.path).is_err() {
                        self.metrics.events_enqueued.fetch_add(1, Ordering::Relaxed);
                        self.unpersisted_events += 1;
                        let change = self.pending.entry(event.path).or_default();
                        change.flags |= crate::fsevents::ITEM_REMOVED;
                        self.first_pending.get_or_insert_with(Instant::now);
                        self.history_paths += usize::from(self.catching_up);
                    } else if event.flags & crate::fsevents::ROOT_CHANGED != 0 {
                        self.rescans.push(event.path);
                        self.root_changed_restart = true;
                        self.first_pending.get_or_insert_with(Instant::now);
                    }
                }
                continue;
            } else if self.scope.admits(&event.path) {
                self.metrics.events_enqueued.fetch_add(1, Ordering::Relaxed);
                self.unpersisted_events += 1;
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
            self.scope_rescans.clear();
            self.pending_prune = false;
            self.root_changed_restart = false;
            self.first_pending = None;
            if self.reconcile_retry.is_none_or(|at| Instant::now() >= at) {
                self.reconcile_all();
            }
            return;
        }
        let mut changes = std::mem::take(&mut self.pending);
        // 子树对账即"把该目录当作新建目录回扫"，并入同一批以便与其下的事件去重。
        for dir in self.scope_rescans.drain(..) {
            changes.entry(dir).or_default().flags |= crate::fsevents::ITEM_CREATED;
        }
        for dir in self.rescans.drain(..) {
            if self.scope.admits(&dir)
                || self.scope.scan_paths.iter().any(|r| paths_equal_ci(r, &dir))
            {
                changes.entry(dir).or_default().flags |= crate::fsevents::ITEM_CREATED;
            }
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
        // 在增量补扫批次应用成功后再执行延后裁剪并原子推进 applied_snapshot，
        // 保证任意未 flush 期间的配置反复切换都不会破坏 store 与 applied_snapshot 的一致基线。
        let pruned = if std::mem::take(&mut self.pending_prune) {
            self.store
                .retain_paths(|path, is_dir| self.scope.retains_entry(Path::new(path), is_dir))
                > 0
        } else {
            false
        };
        if pruned {
            self.dirty = true;
        }
        let mut completed_incremental_snapshot = false;
        if let Some(snapshot) = self.pending_snapshot.take() {
            let fp = snapshot.fingerprint_fnv1a();
            *self.applied_snapshot.write() = snapshot;
            self.fingerprint.store(fp, Ordering::Release);
            self.pending_snapshot_active.store(false, Ordering::Release);
            if let Some(seq) = self.pending_scope_seq.take() {
                self.applied_scope_seq.fetch_max(seq, Ordering::Release);
            }
            self.dirty = true;
            completed_incremental_snapshot = true;
        }
        if self.gap.is_none() {
            self.applied.fetch_max(self.seen, Ordering::Release);
        }
        if std::mem::take(&mut self.root_changed_restart) {
            // 原位替换监听根后重开流，确保 FSEventStream 绑定到新目录 inode 持续接后续实时事件
            self.restart(self.applied.load(Ordering::Acquire));
        }
        if changed || pruned || completed_incremental_snapshot {
            (self.on_batch)(self.store.len());
        }
        if completed_incremental_snapshot {
            if let Some(ref scope_cb) = self.on_scope_applied {
                scope_cb();
            }
        }
        let history_finished = std::mem::take(&mut self.history_done);
        if history_finished {
            if self.gap.is_none() {
                self.gap_replays = 0;
            }
            if std::mem::take(&mut self.catching_up) {
                self.history_paths = 0;
                self.persist_now();
                return;
            }
        }
        if completed_incremental_snapshot {
            self.persist_now();
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
        let target_seq = self.scope_dirty_seq.load(Ordering::Acquire);
        let snapshot = self.scope.snapshot.clone();
        let checkpoint = crate::cache::Checkpoint {
            event_id: crate::fsevents::current_event_id(),
            fingerprint: snapshot.fingerprint_fnv1a(),
        };
        let progress = Arc::new(AtomicUsize::new(0));
        let poller = self
            .on_reconcile_progress
            .as_ref()
            .map(|cb| ReconcileProgressPoller::start(Arc::clone(&progress), Arc::clone(cb)));
        let scan_res = scanner::scan_compact(
            &self.scope.scan_paths,
            Arc::new(self.scope.filter.clone()),
            self.scope.include_hidden,
            progress,
        );
        drop(poller);
        match scan_res {
            Ok(fresh) => {
                self.swap_in(fresh, checkpoint, snapshot);
                self.applied_scope_seq
                    .fetch_max(target_seq, Ordering::Release);
            }
            Err(error) => {
                let message = format!("full reconciliation failed: {error}");
                eprintln!("[Lokii] {message}");
                if let Some(ref error_cb) = self.on_reconcile_error {
                    error_cb(message);
                }
                self.demand_reconcile(0);
                self.reconcile_retry = Some(Instant::now() + RECONCILE_BACKOFF);
                return;
            }
        }
        self.store.warm_query_index();
        (self.on_batch)(self.store.len());
        if let Some(ref ready_cb) = self.on_reconcile_ready {
            ready_cb();
        }
        self.persist_now();
    }

    /// 换入一份扫描于 `checkpoint.event_id` 之后、按 `snapshot` 范围扫描的完整索引。
    /// 检查点先降再替换、指纹与快照替换后再升：任何时刻持锁读到的组合都不会比索引内容新。
    fn swap_in(
        &mut self,
        index: crate::compact_index::CompactIndex,
        checkpoint: crate::cache::Checkpoint,
        snapshot: PrimaryScopeSnapshot,
    ) {
        let from = checkpoint.event_id;
        self.applied.store(from, Ordering::Release);
        self.store.replace_compact(index);
        *self.applied_snapshot.write() = snapshot;
        self.fingerprint.store(checkpoint.fingerprint, Ordering::Release);
        self.pending_prune = false;
        self.set_pending_snapshot(None, None);
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
        self.scope_rescans.clear();
        self.root_changed_restart = false;
        self.applied.store(from, Ordering::Release);
        self.restart(from);
    }

    /// 从 `from` 重开流；未应用的事件丢弃，新流会重新送达（应用按磁盘现状收敛，可重复）。
    fn restart(&mut self, from: u64) {
        self.stream = None;
        let (tx, rx) = mpsc::channel();
        *self.wake_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx.clone());
        self.events = rx;
        self.pending.clear();
        self.first_pending = None;
        self.history_done = false;
        self.seen = from;
        self.stream = crate::fsevents::watch(&self.scope.watch_paths, from, tx);
    }

    fn poll_control(&mut self) {
        let mut refresh_requested = false;
        while let Ok(control) = self.control.try_recv() {
            match control {
                Control::Replace {
                    index,
                    checkpoint,
                    snapshot,
                    done,
                } => {
                    let target_seq = self.scope_dirty_seq.load(Ordering::Acquire);
                    self.swap_in(index, checkpoint, snapshot);
                    if self.applied_snapshot.read().fingerprint_fnv1a() == self.scope.fingerprint {
                        self.applied_scope_seq
                            .fetch_max(target_seq, Ordering::Release);
                    }
                    // 先应答再预热：重建线程（及其后等着存盘退出的主线程）不必等预热。
                    let _ = done.send(());
                    self.store.warm_query_index();
                }
                Control::RefreshScope => {
                    refresh_requested = true;
                }
            }
        }
        if refresh_requested {
            self.poll_scope(true);
        }
    }

    /// 索引根、排除规则、隐藏文件或 FDA 变化后，执行结构化三分法增量更新并按新范围重开流。
    fn poll_scope(&mut self, force: bool) {
        if !force && self.scope_checked.elapsed() < Duration::from_millis(DEFAULT_DEBOUNCE_MS) {
            return;
        }
        self.scope_checked = Instant::now();

        let target_seq = self.scope_dirty_seq.load(Ordering::Acquire);
        let dirty_seq_pending = target_seq != self.applied_scope_seq.load(Ordering::Acquire);

        let changed_config = {
            let guard = self.config_store.read();
            if guard.index != self.last_index_config {
                Some(guard.clone())
            } else {
                None
            }
        };
        let index_changed = changed_config.is_some();
        if let Some(ref config) = changed_config {
            self.last_index_config = config.index.clone();
            if let Some(ref callback) = self.on_config_updated {
                callback(config);
            }
        }

        // 已获 FDA 时放宽至 30s 探测一次，未获权时保持 2s 轮询以兼顾授权即时生效与低系统调用开销
        let fda_poll_interval = if self.scope.has_fda {
            Duration::from_secs(30)
        } else {
            Duration::from_secs(2)
        };
        let current_has_fda = if self.fda_checked.elapsed() >= fda_poll_interval {
            self.fda_checked = Instant::now();
            has_full_disk_access()
        } else {
            self.scope.has_fda
        };
        let fda_changed = current_has_fda != self.scope.has_fda;

        // 每 15 秒低频复核一次本地目录存在性与嵌套挂载排除集合，兼顾新建索引根/后挂载嵌套卷的自动感知与低空转开销
        let periodic_scope_refresh = self.scope_refreshed.elapsed() >= Duration::from_secs(15);
        if periodic_scope_refresh {
            self.scope_refreshed = Instant::now();
        }

        let next_scope = if !(force || dirty_seq_pending || index_changed || fda_changed || periodic_scope_refresh) {
            None
        } else {
            let config = changed_config
                .as_ref()
                .cloned()
                .unwrap_or_else(|| self.config_store.read().clone());
            let pure_periodic =
                periodic_scope_refresh && !force && !dirty_seq_pending && !index_changed && !fda_changed;
            if !pure_periodic {
                self.missing_root_since.clear();
            }
            match &self.primary_volumes_dir {
                Some(volumes_dir) => {
                    let (mut primary_paths, excluded_mount_roots) =
                        primary_scope_inputs(&config, volumes_dir);
                    // 抗抖动：纯 15 秒轮询且配置未变时，若既有本地主根暂时不可达，
                    // 仅在 MISSING_ROOT_GRACE 宽限期内保留，超时仍不存在则退出 scope 并清理。
                    if pure_periodic {
                        apply_missing_root_grace(
                            &self.scope.scan_paths,
                            &mut primary_paths,
                            &excluded_mount_roots,
                            &mut self.missing_root_since,
                            Instant::now(),
                            MISSING_ROOT_GRACE,
                        );
                    }
                    let next_snap = PrimaryScopeSnapshot::from_inputs(
                        &config,
                        &primary_paths,
                        &excluded_mount_roots,
                        current_has_fda,
                    );
                    if next_snap != self.scope.snapshot {
                        Some(Scope::new_primary_with_inputs(
                            &config,
                            primary_paths,
                            excluded_mount_roots,
                            current_has_fda,
                        ))
                    } else {
                        None
                    }
                }
                None => {
                    let mut paths = config.resolve_paths();
                    if pure_periodic {
                        apply_missing_root_grace(
                            &self.scope.scan_paths,
                            &mut paths,
                            &[],
                            &mut self.missing_root_since,
                            Instant::now(),
                            MISSING_ROOT_GRACE,
                        );
                    }
                    let next_snap =
                        PrimaryScopeSnapshot::from_inputs(&config, &paths, &[], current_has_fda);
                    if next_snap != self.scope.snapshot {
                        let filter = CompiledFilter::from_primary_config(&config, &paths, &[]);
                        let (has_fda, protected, watch_paths) =
                            watch_scope_for_paths_with_fda(paths.clone(), &filter, current_has_fda);
                        let fingerprint = next_snap.fingerprint_fnv1a();
                        Some(Scope {
                            snapshot: next_snap,
                            fingerprint,
                            filter,
                            has_fda,
                            protected,
                            watch_paths,
                            scan_paths: paths,
                            include_hidden: config.index.include_hidden,
                        })
                    } else {
                        None
                    }
                }
            }
        };

        if let Some(next_scope) = next_scope {
            let previous = std::mem::replace(&mut self.scope, next_scope);
            let old_applied = self.applied_snapshot.read().clone();
            let index_matched_previous = old_applied.fingerprint_fnv1a() == previous.fingerprint
                || self.pending_snapshot.is_some();

            if !index_matched_previous {
                // 索引原本就落后于旧范围，直接交由全量对账收敛
                self.scope_rescans.clear();
                self.pending_prune = false;
                self.set_pending_snapshot(None, None);
                self.restart(self.applied.load(Ordering::Acquire));
                if force {
                    self.flush();
                }
                return;
            }

            match diff_primary_scope_runtime(&old_applied, &self.scope.snapshot) {
                PrimaryScopeAction::NoOp => {
                    self.scope_rescans.clear();
                    self.pending_prune = false;
                    self.rescans.retain(|dir| {
                        self.scope.admits(dir)
                            || self.scope.scan_paths.iter().any(|r| paths_equal_ci(r, dir))
                    });
                    if !self.rescans.is_empty() {
                        // 仍有未 flush 的升级补扫或事件回扫：保留 pending_snapshot 直到 flush 完成，绝不丢弃回扫
                        self.restart(self.applied.load(Ordering::Acquire));
                        self.set_pending_snapshot(Some(self.scope.snapshot.clone()), Some(target_seq));
                        self.first_pending = Some(Instant::now());
                        if force {
                            self.flush();
                        }
                    } else {
                        self.set_pending_snapshot(None, None);
                        *self.applied_snapshot.write() = self.scope.snapshot.clone();
                        self.fingerprint
                            .store(self.scope.fingerprint, Ordering::Release);
                        self.applied_scope_seq
                            .fetch_max(target_seq, Ordering::Release);
                        self.restart(self.applied.load(Ordering::Acquire));
                        if force || dirty_seq_pending {
                            if let Some(ref scope_cb) = self.on_scope_applied {
                                scope_cb();
                            }
                        }
                    }
                }
                PrimaryScopeAction::PruneOnly => {
                    self.scope_rescans.clear();
                    self.pending_prune = false;
                    // 裁剪前置位 pending_snapshot_active，确保周期自检/FDA 撤销/宽限到期路径下
                    // 并发退出 persist_index() 绝不会写出“已裁索引 + 旧快照”
                    self.pending_snapshot_active.store(true, Ordering::Release);
                    self.store.retain_paths(|path, is_dir| {
                        self.scope.retains_entry(Path::new(path), is_dir)
                    });
                    self.rescans.retain(|dir| {
                        self.scope.admits(dir)
                            || self.scope.scan_paths.iter().any(|r| paths_equal_ci(r, dir))
                    });
                    self.dirty = true;
                    self.restart(self.applied.load(Ordering::Acquire));
                    if !self.rescans.is_empty() {
                        self.set_pending_snapshot(Some(self.scope.snapshot.clone()), Some(target_seq));
                        self.first_pending = Some(Instant::now());
                        (self.on_batch)(self.store.len());
                        if force {
                            self.flush();
                        }
                    } else {
                        *self.applied_snapshot.write() = self.scope.snapshot.clone();
                        self.fingerprint
                            .store(self.scope.fingerprint, Ordering::Release);
                        self.applied_scope_seq
                            .fetch_max(target_seq, Ordering::Release);
                        self.set_pending_snapshot(None, None);
                        (self.on_batch)(self.store.len());
                        if let Some(ref scope_cb) = self.on_scope_applied {
                            scope_cb();
                        }
                        if force {
                            self.persist_now();
                        }
                    }
                }
                PrimaryScopeAction::IncrementalUpdate {
                    prune_first,
                    scan_roots,
                } => {
                    self.restart(self.applied.load(Ordering::Acquire));
                    self.scope_rescans = scan_roots;
                    self.pending_prune = prune_first;
                    self.set_pending_snapshot(Some(self.scope.snapshot.clone()), Some(target_seq));
                    self.first_pending = Some(Instant::now());
                    if force {
                        self.flush();
                    }
                }
                PrimaryScopeAction::FullRebuild => {
                    self.rescans.clear();
                    self.scope_rescans.clear();
                    self.pending_prune = false;
                    self.root_changed_restart = false;
                    self.set_pending_snapshot(None, None);
                    self.restart(self.applied.load(Ordering::Acquire));
                    if force {
                        self.flush();
                    }
                }
            }
        } else if dirty_seq_pending && self.pending_snapshot.is_none() && !self.reconcile_pending() {
            // 配置改动不影响主索引范围（如仅改外置卷或 network_rescan_minutes），立即放行存盘序号
            self.applied_scope_seq
                .fetch_max(target_seq, Ordering::Release);
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

/// 启动索引服务线程（无卷隔离的旧全量入口，仅保留给单元测试）。
#[cfg(test)]
fn start_watcher<F, P>(
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
    P: Fn(&[u8], crate::cache::Checkpoint, &PrimaryScopeSnapshot) + Send + 'static,
{
    start_watcher_with_options(
        store,
        config_store,
        metrics,
        resume,
        scan_lock,
        None,
        on_batch,
        persist,
    )
}

/// 主卷监听的隔离与回调选项。
pub struct WatcherOptions {
    pub cache_dir: PathBuf,
    pub on_config_updated: Option<Arc<dyn Fn(&AppConfig) + Send + Sync>>,
    pub on_reconcile_progress: Option<Arc<dyn Fn(&str, u64) + Send + Sync>>,
    pub on_reconcile_ready: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_reconcile_error: Option<Arc<dyn Fn(String) + Send + Sync>>,
    pub on_scope_applied: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// 仅监听并维护本地主索引根；挂载卷目录被隔离交由卷协调器接管，并在配置变更时触发回调。
pub fn start_primary_watcher<F, P>(
    store: IndexStore,
    config_store: ConfigStore,
    metrics: Arc<WatcherMetrics>,
    resume: Resume,
    scan_lock: Arc<std::sync::Mutex<()>>,
    options: WatcherOptions,
    on_batch: F,
    persist: P,
) -> WatcherHandle
where
    F: Fn(usize) + Send + 'static,
    P: Fn(&[u8], crate::cache::Checkpoint, &PrimaryScopeSnapshot) + Send + 'static,
{
    start_watcher_with_options(
        store,
        config_store,
        metrics,
        resume,
        scan_lock,
        Some(options),
        on_batch,
        persist,
    )
}

fn start_watcher_with_options<F, P>(
    store: IndexStore,
    config_store: ConfigStore,
    metrics: Arc<WatcherMetrics>,
    resume: Resume,
    scan_lock: Arc<std::sync::Mutex<()>>,
    options: Option<WatcherOptions>,
    on_batch: F,
    persist: P,
) -> WatcherHandle
where
    F: Fn(usize) + Send + 'static,
    P: Fn(&[u8], crate::cache::Checkpoint, &PrimaryScopeSnapshot) + Send + 'static,
{
    let (control_tx, control) = mpsc::channel();
    let applied = Arc::new(AtomicU64::new(match &resume {
        Resume::From { event_id, .. } => *event_id,
        Resume::Verify => 0,
    }));
    let initial_config = config_store.read().clone();
    let (
        primary_volumes_dir,
        on_config_updated,
        on_reconcile_progress,
        on_reconcile_ready,
        on_reconcile_error,
        on_scope_applied,
    ) = match options {
        Some(opts) => (
            Some(opts.cache_dir.join("volumes")),
            opts.on_config_updated,
            opts.on_reconcile_progress,
            opts.on_reconcile_ready,
            opts.on_reconcile_error,
            opts.on_scope_applied,
        ),
        None => (None, None, None, None, None, None),
    };
    let scope = match &primary_volumes_dir {
        Some(volumes_dir) => Scope::new_primary(&initial_config, volumes_dir),
        None => Scope::new(&initial_config),
    };
    let initial_applied_snapshot = match &resume {
        Resume::From {
            saved_snapshot: Some(saved),
            ..
        } => saved.clone(),
        _ => scope.snapshot.clone(),
    };
    let fingerprint = Arc::new(AtomicU64::new(initial_applied_snapshot.fingerprint_fnv1a()));
    let applied_snapshot = Arc::new(RwLock::new(initial_applied_snapshot));
    let pending_snapshot_active = Arc::new(AtomicBool::new(false));
    let scope_dirty_seq = Arc::new(AtomicU64::new(0));
    let applied_scope_seq = Arc::new(AtomicU64::new(0));
    let (init_event_tx, init_event_rx) = mpsc::channel();
    let wake_tx = Arc::new(std::sync::Mutex::new(Some(init_event_tx)));
    let handle = WatcherHandle {
        control: control_tx,
        wake_tx: Arc::clone(&wake_tx),
        applied: Arc::clone(&applied),
        fingerprint: Arc::clone(&fingerprint),
        applied_snapshot: Arc::clone(&applied_snapshot),
        pending_snapshot_active: Arc::clone(&pending_snapshot_active),
        scope_dirty_seq: Arc::clone(&scope_dirty_seq),
        applied_scope_seq: Arc::clone(&applied_scope_seq),
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
            scope_refreshed: Instant::now(),
            fda_checked: Instant::now(),
            stream: None,
            events: init_event_rx,
            wake_tx,
            control,
            applied,
            fingerprint,
            applied_snapshot,
            pending_snapshot: None,
            pending_prune: false,
            pending_snapshot_active,
            scope_dirty_seq,
            applied_scope_seq,
            pending_scope_seq: None,
            missing_root_since: Vec::new(),
            pending: HashMap::new(),
            seen: 0,
            first_pending: None,
            rescans: Vec::new(),
            scope_rescans: Vec::new(),
            root_changed_restart: false,
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
            unpersisted_events: 0,
            primary_volumes_dir,
            last_index_config: initial_config.index,
            on_config_updated,
            on_reconcile_progress,
            on_reconcile_ready,
            on_reconcile_error,
            on_scope_applied,
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
            move |_, checkpoint: crate::cache::Checkpoint, _| {
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
        // 给系统 fseventsd 留出提交时间，确保初态文件的创建事件已落库，不会漏入 since 之后
        std::thread::sleep(Duration::from_millis(150));
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
        // 给系统 fseventsd 留出提交时间，确保离线变更完整进入日志，避免虚拟化 CI (Rosetta 2) 下合并目录事件
        std::thread::sleep(Duration::from_millis(400));

        let (handle, persisted) = serve(&store, &root, Resume::from_id(since));
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
    ) -> Service<impl Fn(usize), impl Fn(&[u8], crate::cache::Checkpoint, &PrimaryScopeSnapshot)> {
        idle_service_persisting(store, root, |_: &[u8], _, _| {})
    }

    fn idle_service_persisting<P: Fn(&[u8], crate::cache::Checkpoint, &PrimaryScopeSnapshot)>(
        store: &IndexStore,
        root: &Path,
        persist: P,
    ) -> Service<impl Fn(usize), P> {
        let config = scoped_config(root);
        let (_, control) = mpsc::channel();
        let scope = Scope::new(&config);
        let fp = scope.fingerprint;
        let snap = scope.snapshot.clone();
        Service {
            store: store.clone(),
            config_store: ConfigStore::new(config.clone()),
            metrics: Arc::new(WatcherMetrics::default()),
            scan_lock: Arc::new(std::sync::Mutex::new(())),
            on_batch: |_| {},
            persist,
            scope,
            scope_checked: Instant::now(),
            scope_refreshed: Instant::now(),
            fda_checked: Instant::now(),
            stream: None,
            events: mpsc::channel().1,
            wake_tx: Arc::new(std::sync::Mutex::new(None)),
            control,
            applied: Arc::new(AtomicU64::new(5)),
            fingerprint: Arc::new(AtomicU64::new(fp)),
            applied_snapshot: Arc::new(RwLock::new(snap)),
            pending_snapshot: None,
            pending_prune: false,
            pending_snapshot_active: Arc::new(AtomicBool::new(false)),
            scope_dirty_seq: Arc::new(AtomicU64::new(0)),
            applied_scope_seq: Arc::new(AtomicU64::new(0)),
            pending_scope_seq: None,
            missing_root_since: Vec::new(),
            pending: HashMap::new(),
            seen: 5,
            first_pending: None,
            rescans: Vec::new(),
            scope_rescans: Vec::new(),
            root_changed_restart: false,
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
            unpersisted_events: 0,
            primary_volumes_dir: None,
            last_index_config: config.index,
            on_config_updated: None,
            on_reconcile_progress: None,
            on_reconcile_ready: None,
            on_reconcile_error: None,
            on_scope_applied: None,
        }
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
            |_: &[u8], checkpoint: crate::cache::Checkpoint, _| {
                saved.borrow_mut().push(checkpoint.event_id)
            },
        );

        service.last_persist = Instant::now() - PERSIST_EVERY;
        service.persist_if_due();
        assert!(saved.borrow().is_empty(), "nothing changed, nothing to save");

        service.intake(vec![event(file, crate::fsevents::ITEM_CREATED, 60)]);
        assert_eq!(service.unpersisted_events, 1);
        service.flush();
        service.last_persist = Instant::now() - Duration::from_secs(11 * 3600);
        service.persist_if_due();
        assert!(saved.borrow().is_empty(), "fallback interval has not elapsed");

        service.last_persist = Instant::now() - Duration::from_secs(12 * 3600);
        service.persist_if_due();
        assert_eq!(*saved.borrow(), [60]);
        assert_eq!(service.unpersisted_events, 0);
        service.persist_if_due();
        assert_eq!(*saved.borrow(), [60], "saved once per interval");
    }

    #[test]
    fn changes_are_persisted_when_the_event_threshold_is_reached() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("a.txt");
        fs::write(&file, "x").unwrap();
        let store = IndexStore::new();
        let saved = std::cell::RefCell::new(Vec::new());
        let mut service = idle_service_persisting(
            &store,
            &root,
            |_: &[u8], checkpoint: crate::cache::Checkpoint, _| {
                saved.borrow_mut().push(checkpoint.event_id)
            },
        );

        service.intake(vec![event(file, crate::fsevents::ITEM_CREATED, 60)]);
        service.flush();
        service.unpersisted_events = PERSIST_EVENTS - 1;
        service.persist_if_due();
        assert!(saved.borrow().is_empty(), "neither persistence trigger is due");

        service.unpersisted_events = PERSIST_EVENTS;
        service.persist_if_due();
        assert_eq!(*saved.borrow(), [60]);
        assert_eq!(service.unpersisted_events, 0);
        assert!(!service.dirty);
        service.persist_if_due();
        assert_eq!(*saved.borrow(), [60], "saved once per threshold crossing");
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
        let scope = Scope::new(&scoped_config(&root));
        let checkpoint = crate::cache::Checkpoint {
            event_id: from,
            fingerprint: scope.fingerprint,
        };

        assert!(handle.replace(fresh, checkpoint, scope.snapshot).is_ok());
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

    fn rescope(
        service: &mut Service<
            impl Fn(usize),
            impl Fn(&[u8], crate::cache::Checkpoint, &PrimaryScopeSnapshot),
        >,
        edit: impl FnOnce(&mut AppConfig),
    ) {
        let mut config = service.config_store.read().clone();
        edit(&mut config);
        service.config_store.update(config);
        service.scope_checked = Instant::now() - Duration::from_secs(1);
        service.poll_scope(false);
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
    fn added_root_uses_incremental_rescan_and_widened_filter_triggers_reconcile() {
        let dir = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let extra_root = other.path().canonicalize().unwrap();
        fs::write(extra_root.join("extra.txt"), "hi").unwrap();
        let store = IndexStore::new();
        let mut service = idle_service(&store, &root);
        let before = service.fingerprint.load(Ordering::Relaxed);
        let extra = extra_root.to_string_lossy().into_owned();

        // 新增本地根：走 IncrementalUpdate（rescans + pending_snapshot），不触发全量重建；
        // flush 前保留旧指纹并暂停落盘，flush 后自动补齐并推进指纹。
        rescope(&mut service, |config| config.index.index_paths.push(extra));
        assert!(!service.reconcile_pending());
        assert!(service.pending_snapshot.is_some());
        assert!(!service.can_persist(), "must skip persist while pending_snapshot is active");
        assert_eq!(
            service.fingerprint.load(Ordering::Relaxed),
            before,
            "pending incremental update must not advance fingerprint prematurely"
        );
        service.flush();
        assert!(indexed(&store, &extra_root.join("extra.txt")));
        assert!(service.pending_snapshot.is_none());
        assert!(service.can_persist());
        assert_eq!(
            service.fingerprint.load(Ordering::Relaxed),
            service.scope.fingerprint
        );

        // 真正过滤宽化（先关闭隐藏文件、再重新开启）：走 FullRebuild，对账前保留旧指纹
        rescope(&mut service, |config| config.index.include_hidden = false);
        let narrowed_fp = service.fingerprint.load(Ordering::Relaxed);
        rescope(&mut service, |config| config.index.include_hidden = true);
        assert!(service.reconcile_pending());
        assert_eq!(
            service.fingerprint.load(Ordering::Relaxed),
            narrowed_fp,
            "saves must not claim the widened scope before reconcile"
        );
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
        let snapshot = service.scope.snapshot.clone();
        let index = || crate::compact_index::CompactIndex::from_entries(Vec::new()).unwrap();
        service.demand_reconcile(100);

        service.swap_in(
            index(),
            crate::cache::Checkpoint {
                event_id: 50,
                fingerprint,
            },
            snapshot.clone(),
        );
        assert!(service.full_reconcile, "drop at 100 happened after the scan started at 50");
        service.swap_in(
            index(),
            crate::cache::Checkpoint {
                event_id: 150,
                fingerprint,
            },
            snapshot,
        );
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
        assert!(!scope.admits(&root.join(".secret")));
        assert!(!scope.admits(&root.join(".DS_Store")));
        assert!(scope.admits(&root.join("src/x.txt")));

        config.index.include_hidden = true;
        config.index.exclude = crate::config::IndexConfig::default().exclude;
        let scope_hidden_on = Scope::new(&config);
        assert!(!scope_hidden_on.admits(&root.join(".DS_Store")));
        assert!(scope_hidden_on.admits(&root.join("._note.txt")));
        assert!(scope_hidden_on.admits(&root.join(".zshrc")));
        assert!(!scope_hidden_on.admits(&root.join("node_modules")));
        assert!(!scope_hidden_on.admits(&root.join("node_modules/pkg/a.js")));
    }

    #[test]
    fn checkpoint_is_absent_until_something_is_known_or_while_pending_or_dirty() {
        let (control, _) = mpsc::channel();
        let snap = PrimaryScopeSnapshot::from_inputs(&AppConfig::default(), &[], &[], true);
        let handle = WatcherHandle {
            control,
            wake_tx: Arc::new(std::sync::Mutex::new(None)),
            applied: Arc::new(AtomicU64::new(0)),
            fingerprint: Arc::new(AtomicU64::new(9)),
            applied_snapshot: Arc::new(RwLock::new(snap)),
            pending_snapshot_active: Arc::new(AtomicBool::new(false)),
            scope_dirty_seq: Arc::new(AtomicU64::new(0)),
            applied_scope_seq: Arc::new(AtomicU64::new(0)),
        };
        assert!(handle.checkpoint().is_none());
        handle.applied.store(42, Ordering::Relaxed);
        assert_eq!(
            handle.checkpoint(),
            Some(crate::cache::Checkpoint { event_id: 42, fingerprint: 9 })
        );
        // 半途补扫期间禁止退出存盘
        handle.pending_snapshot_active.store(true, Ordering::Release);
        assert!(handle.checkpoint().is_none());
        handle.pending_snapshot_active.store(false, Ordering::Release);
        assert!(handle.checkpoint().is_some());
        // 并发配置预裁尚未对账期间同样禁止退出存盘
        let seq = handle.mark_scope_dirty();
        assert!(handle.checkpoint().is_none());
        handle.applied_scope_seq.store(seq, Ordering::Release);
        assert!(handle.checkpoint().is_some());
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
            |_, _, _| {},
        );

        config.index.index_paths = vec![new_root.to_string_lossy().into_owned()];
        config_store.update(config);
        // 等待服务线程在轮询窗口内感知配置变化并重启流
        std::thread::sleep(Duration::from_millis(2000));
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
        // Rename dir to "node_modules" (excluded name) — old entries are removed and the new path is not indexed
        let tmp = TempDir::new().unwrap();
        let old_dir = tmp.path().join("mydir");
        let new_dir = tmp.path().join("node_modules");
        fs::create_dir(&new_dir).unwrap();

        let old_prefix = old_dir.to_string_lossy().to_string();
        let new_prefix = new_dir.to_string_lossy().to_string();

        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("mydir", &old_prefix, true),
            make_entry("a.txt", &format!("{}/a.txt", old_prefix), false),
            make_entry("b.txt", &format!("{}/b.txt", old_prefix), false),
        ]);

        let renames = vec![(old_prefix.clone(), new_prefix.clone())];
        apply_batch(EventBatch::default(), renames, &store, &dummy_filter(), true).unwrap();

        assert!(store.entries().is_empty());
    }

    #[test]
    fn test_apply_batch_excluded_dir_create_is_ignored() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let nm = root.join("node_modules");
        let nm_child = nm.join("pkg");
        fs::create_dir_all(&nm_child).unwrap();
        fs::write(nm_child.join("index.js"), "export {}").unwrap();
        let noise = root.join(".DS_Store");
        fs::write(&noise, "junk").unwrap();

        let store = IndexStore::new();

        let mut cfg = scoped_config(&root);
        cfg.index.include_hidden = true;
        cfg.index.exclude = crate::config::IndexConfig::default().exclude;
        let filter = CompiledFilter::from_config(&cfg);

        let batch = EventBatch {
            creates: vec![noise],
            removes: vec![],
            dir_creates: vec![nm.clone()],
        };
        apply_batch(batch, Vec::new(), &store, &filter, true).unwrap();

        assert!(store.entries().is_empty());

        // collect_safe_watch_paths stops recursion on excluded directories when descending due to protected sibling
        let protected_sibling = root.join("Documents");
        fs::create_dir_all(&protected_sibling).unwrap();
        let mut watch_out = Vec::new();
        collect_safe_watch_paths(&root, &filter, std::slice::from_ref(&protected_sibling), &mut watch_out);
        assert!(!watch_out.contains(&nm));
        assert!(!watch_out.contains(&nm_child));
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

    #[test]
    fn primary_watcher_isolates_volumes_and_forwards_config_updates() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let primary_dir = root.join("local");
        std::fs::create_dir_all(&primary_dir).unwrap();
        let cache_dir = root.join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![
            primary_dir.to_string_lossy().to_string(),
            "/Volumes/ExternalUSB".to_string(),
        ];
        cfg.index.include_hidden = true;
        let config_store = ConfigStore::new(cfg.clone());
        let store = IndexStore::new();
        let metrics = Arc::new(WatcherMetrics::default());
        let scan_lock = Arc::new(std::sync::Mutex::new(()));
        let callback_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let callback_count_clone = Arc::clone(&callback_count);

        let _handle = start_primary_watcher(
            store.clone(),
            config_store.clone(),
            metrics,
            Resume::from_id(crate::fsevents::current_event_id()),
            scan_lock,
            WatcherOptions {
                cache_dir,
                on_config_updated: Some(Arc::new(move |_| {
                    callback_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                })),
                on_reconcile_progress: None,
                on_reconcile_ready: None,
                on_reconcile_error: None,
            on_scope_applied: None,
            },
            |_| {},
            |_, _, _| {},
        );

        let local_file = primary_dir.join("hello.txt");
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(&local_file, "hi").unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if store
                .entries()
                .iter()
                .any(|e| e.path == local_file.to_string_lossy())
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            store
                .entries()
                .iter()
                .any(|e| e.path == local_file.to_string_lossy()),
            "primary watcher should index local primary dir"
        );

        // Mutate config and wait for poll_scope to forward the update
        let mut next = cfg.clone();
        next.index.exclude.push("*.tmp".to_string());
        config_store.update(next);

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && callback_count.load(std::sync::atomic::Ordering::SeqCst) == 0
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            callback_count.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "primary watcher should forward config updates via callback"
        );
    }

    #[test]
    fn persist_now_releases_store_read_lock_before_invoking_persist_callback() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("hello.txt");
        fs::write(&file, "data").unwrap();
        let store = IndexStore::new();
        let store_for_cb = store.clone();
        let write_lock_acquired = std::cell::Cell::new(false);
        let saved_bytes_len = std::cell::Cell::new(0usize);

        let mut service = idle_service_persisting(
            &store,
            &root,
            |encoded: &[u8], checkpoint: crate::cache::Checkpoint, _| {
                // 若 persist_now 仍持有 store.read()，此处 try_write 必返回 None
                assert!(
                    store_for_cb.try_write().is_some(),
                    "store read lock must be released before disk persistence callback runs"
                );
                assert_eq!(checkpoint.event_id, 88);
                assert!(!encoded.is_empty());
                write_lock_acquired.set(true);
                saved_bytes_len.set(encoded.len());
            },
        );

        service.intake(vec![event(file, crate::fsevents::ITEM_CREATED, 88)]);
        service.flush();
        service.persist_now();

        assert!(write_lock_acquired.get());
        assert!(saved_bytes_len.get() > 0);
    }

    #[test]
    fn incremental_oracle_matches_fresh_scan_across_mutations_and_guards_halfway_persist() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root_a = base.join("root_a");
        let root_b = base.join("root_b");
        let lib_keep = root_a.join("Library/WeWork");
        let lib_drop = root_a.join("Library/Caches");
        let build_out = root_a.join("proj/build/out");
        let app_pkg = root_a.join("Tool.app/Wrapper");
        let hidden_dir = root_a.join(".hidden_dir");

        fs::create_dir_all(&lib_keep).unwrap();
        fs::create_dir_all(&lib_drop).unwrap();
        fs::create_dir_all(&build_out).unwrap();
        fs::create_dir_all(&app_pkg).unwrap();
        fs::create_dir_all(&hidden_dir).unwrap();
        fs::create_dir_all(&root_b).unwrap();

        fs::write(root_a.join("keep.txt"), "a").unwrap();
        fs::write(root_a.join("café.txt"), "nfd").unwrap();
        fs::write(lib_keep.join("chat.db"), "keep").unwrap();
        fs::write(lib_drop.join("tmp.bin"), "drop").unwrap();
        fs::write(build_out.join("artifact.o"), "out").unwrap();
        fs::write(app_pkg.join("inner.bin"), "app").unwrap();
        fs::write(hidden_dir.join("dot.txt"), "dot").unwrap();
        fs::write(root_b.join("b_file.txt"), "b").unwrap();

        let paths_of = |store: &IndexStore| -> std::collections::BTreeSet<String> {
            store
                .read()
                .iter()
                .map(|(_, view)| view.materialize().path)
                .collect()
        };
        let oracle_scan = |cfg: &AppConfig| -> std::collections::BTreeSet<String> {
            let roots = cfg.resolve_paths();
            let filter = Arc::new(CompiledFilter::from_config(cfg));
            let compact = scanner::scan_compact(
                &roots,
                filter,
                cfg.index.include_hidden,
                Arc::new(AtomicUsize::new(0)),
            )
            .unwrap();
            compact
                .iter()
                .map(|(_, view)| view.materialize().path)
                .collect()
        };

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![
            root_a.to_string_lossy().into_owned(),
            lib_keep.to_string_lossy().into_owned(),
        ];
        cfg.index.exclude_paths = vec![root_a.join("Library").to_string_lossy().into_owned()];
        cfg.index.exclude = vec!["build".into()];
        cfg.index.include_hidden = true;
        cfg.index.index_apps = false;

        let store = IndexStore::new();
        let initial_compact = scanner::scan_compact(
            &cfg.resolve_paths(),
            Arc::new(CompiledFilter::from_config(&cfg)),
            cfg.index.include_hidden,
            Arc::new(AtomicUsize::new(0)),
        )
        .unwrap();
        store.replace_compact(initial_compact);

        let persist_calls = std::cell::Cell::new(0usize);
        let (_, control) = mpsc::channel();
        let scope = Scope::new(&cfg);
        let fp = scope.fingerprint;
        let snap = scope.snapshot.clone();
        let mut service = Service {
            store: store.clone(),
            config_store: ConfigStore::new(cfg.clone()),
            metrics: Arc::new(WatcherMetrics::default()),
            scan_lock: Arc::new(std::sync::Mutex::new(())),
            on_batch: |_: usize| {},
            persist: |_: &[u8], _: crate::cache::Checkpoint, _: &PrimaryScopeSnapshot| {
                persist_calls.set(persist_calls.get() + 1);
            },
            scope,
            scope_checked: Instant::now(),
            scope_refreshed: Instant::now(),
            fda_checked: Instant::now(),
            stream: None,
            events: mpsc::channel().1,
            wake_tx: Arc::new(std::sync::Mutex::new(None)),
            control,
            applied: Arc::new(AtomicU64::new(10)),
            fingerprint: Arc::new(AtomicU64::new(fp)),
            applied_snapshot: Arc::new(RwLock::new(snap)),
            pending_snapshot: None,
            pending_prune: false,
            pending_snapshot_active: Arc::new(AtomicBool::new(false)),
            scope_dirty_seq: Arc::new(AtomicU64::new(0)),
            applied_scope_seq: Arc::new(AtomicU64::new(0)),
            pending_scope_seq: None,
            missing_root_since: Vec::new(),
            pending: HashMap::new(),
            seen: 10,
            first_pending: None,
            rescans: Vec::new(),
            scope_rescans: Vec::new(),
            root_changed_restart: false,
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
            unpersisted_events: 0,
            primary_volumes_dir: None,
            last_index_config: cfg.index.clone(),
            on_config_updated: None,
            on_reconcile_progress: None,
            on_reconcile_ready: None,
            on_reconcile_error: None,
            on_scope_applied: None,
        };

        // 1. 新增穿透 build 黑名单的显式子根
        cfg.index.index_paths.push(build_out.to_string_lossy().into_owned());
        rescope(&mut service, |c| *c = cfg.clone());
        assert!(!service.reconcile_pending());
        service.flush();
        assert_eq!(paths_of(&store), oracle_scan(&cfg));

        // 2. 新增排除路径（大小写混写）不误杀穿透子根 lib_keep
        cfg.index.exclude_paths.push(root_a.join("LIBRARY").to_string_lossy().into_owned());
        cfg.index.exclude_paths.push(root_a.join("café.txt").to_string_lossy().into_owned());
        rescope(&mut service, |c| *c = cfg.clone());
        assert!(!service.reconcile_pending());
        assert_eq!(paths_of(&store), oracle_scan(&cfg));

        // 3. 关闭隐藏文件（PruneOnly，零全扫）
        cfg.index.include_hidden = false;
        rescope(&mut service, |c| *c = cfg.clone());
        assert!(!service.reconcile_pending());
        assert_eq!(paths_of(&store), oracle_scan(&cfg));

        // 4. 同时删根（删 build_out）+ 增根（加 root_b）：验证半途存盘被跳过，flush 后 100% 等价
        cfg.index.index_paths.retain(|p| p != &build_out.to_string_lossy());
        cfg.index.index_paths.push(root_b.to_string_lossy().into_owned());
        let persists_before = persist_calls.get();
        rescope(&mut service, |c| *c = cfg.clone());
        assert!(!service.reconcile_pending());
        assert!(service.pending_snapshot.is_some());
        service.dirty = true;
        service.persist_now();
        assert_eq!(
            persist_calls.get(),
            persists_before,
            "persist_now must skip during prune_first pending_snapshot window"
        );
        service.flush();
        assert_eq!(paths_of(&store), oracle_scan(&cfg));
        assert!(persist_calls.get() > persists_before);

        // 5. 监听根收到普通 ITEM_CREATED 与 MOUNT_CHANGED 绝不入 pending 或触发全扫
        service.intake(vec![
            event(root_a.clone(), crate::fsevents::ITEM_CREATED, 11),
            event(root_a.clone(), crate::fsevents::MOUNT_CHANGED, 12),
        ]);
        assert!(service.pending.is_empty());
        assert!(!service.full_reconcile);
    }

    #[test]
    fn poll_scope_aba_revert_and_redirect_before_flush_preserves_pruned_root() {
        let dir = TempDir::new().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let root_a = base.join("A");
        let root_b = base.join("B");
        let root_c = base.join("C");
        let root_d = base.join("D");
        for r in [&root_a, &root_b, &root_c, &root_d] {
            fs::create_dir_all(r).unwrap();
        }
        let file_a = root_a.join("a.txt");
        let file_b = root_b.join("b.txt");
        let file_c = root_c.join("c.txt");
        let file_d = root_d.join("d.txt");
        fs::write(&file_a, "a").unwrap();
        fs::write(&file_b, "b").unwrap();
        fs::write(&file_c, "c").unwrap();
        fs::write(&file_d, "d").unwrap();

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![
            root_a.to_string_lossy().into_owned(),
            root_b.to_string_lossy().into_owned(),
        ];
        cfg.index.exclude_paths = vec![];
        cfg.index.exclude = vec![];
        cfg.index.include_hidden = true;
        cfg.index.index_apps = false;

        let store = IndexStore::new();
        let initial = scanner::scan_compact(
            &cfg.resolve_paths(),
            Arc::new(CompiledFilter::from_config(&cfg)),
            cfg.index.include_hidden,
            Arc::new(AtomicUsize::new(0)),
        )
        .unwrap();
        store.replace_compact(initial);

        let (_, control) = mpsc::channel();
        let scope = Scope::new(&cfg);
        let fp = scope.fingerprint;
        let snap = scope.snapshot.clone();
        let mut service = Service {
            store: store.clone(),
            config_store: ConfigStore::new(cfg.clone()),
            metrics: Arc::new(WatcherMetrics::default()),
            scan_lock: Arc::new(std::sync::Mutex::new(())),
            on_batch: |_: usize| {},
            persist: |_: &[u8], _: crate::cache::Checkpoint, _: &PrimaryScopeSnapshot| {},
            scope,
            scope_checked: Instant::now(),
            scope_refreshed: Instant::now(),
            fda_checked: Instant::now(),
            stream: None,
            events: mpsc::channel().1,
            wake_tx: Arc::new(std::sync::Mutex::new(None)),
            control,
            applied: Arc::new(AtomicU64::new(10)),
            fingerprint: Arc::new(AtomicU64::new(fp)),
            applied_snapshot: Arc::new(RwLock::new(snap)),
            pending_snapshot: None,
            pending_prune: false,
            pending_snapshot_active: Arc::new(AtomicBool::new(false)),
            scope_dirty_seq: Arc::new(AtomicU64::new(0)),
            applied_scope_seq: Arc::new(AtomicU64::new(0)),
            pending_scope_seq: None,
            missing_root_since: Vec::new(),
            pending: HashMap::new(),
            seen: 10,
            first_pending: None,
            rescans: Vec::new(),
            scope_rescans: Vec::new(),
            root_changed_restart: false,
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
            unpersisted_events: 0,
            primary_volumes_dir: None,
            last_index_config: cfg.index.clone(),
            on_config_updated: None,
            on_reconcile_progress: None,
            on_reconcile_ready: None,
            on_reconcile_error: None,
            on_scope_applied: None,
        };

        // 1. [A, B] -> [A, C] (IncrementalUpdate { prune_first: true }): 未 flush 前 B 尚未从 store 误删
        let mut cfg_ac = cfg.clone();
        cfg_ac.index.index_paths = vec![
            root_a.to_string_lossy().into_owned(),
            root_c.to_string_lossy().into_owned(),
        ];
        rescope(&mut service, |c| *c = cfg_ac.clone());
        assert!(service.pending_snapshot.is_some());
        assert!(service.pending_prune);
        assert!(indexed(&store, &file_b));

        // 2. flush 之前立刻改回 [A, B] -> 走 NoOp，B 完整保留且无须重扫
        rescope(&mut service, |c| *c = cfg.clone());
        assert!(service.pending_snapshot.is_none());
        assert!(!service.pending_prune);
        assert!(indexed(&store, &file_a));
        assert!(indexed(&store, &file_b));
        assert!(!indexed(&store, &file_c));

        // 3. [A, B] -> [A, C] 未 flush 时直接切向 [A, B, D]（不走 NoOp 分支）-> flush 后 A、B、D 均完整存在
        rescope(&mut service, |c| *c = cfg_ac.clone());
        assert!(service.pending_prune);
        let mut cfg_abd = cfg.clone();
        cfg_abd.index.index_paths = vec![
            root_a.to_string_lossy().into_owned(),
            root_b.to_string_lossy().into_owned(),
            root_d.to_string_lossy().into_owned(),
        ];
        rescope(&mut service, |c| *c = cfg_abd.clone());
        assert!(!service.pending_prune);
        service.flush();
        assert!(indexed(&store, &file_a));
        assert!(indexed(&store, &file_b), "B must survive un-flushed [A,B]->[A,C]->[A,B,D]");
        assert!(!indexed(&store, &file_c));
        assert!(indexed(&store, &file_d));
    }

    #[test]
    fn root_move_and_in_place_replace_events_clean_or_rescan_root() {
        let dir = TempDir::new().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let root_a = base.join("A");
        let root_b = base.join("B");
        let root_b_moved = base.join("B_moved");
        fs::create_dir_all(&root_a).unwrap();
        fs::create_dir_all(&root_b).unwrap();
        let file_a = root_a.join("a.txt");
        let file_b = root_b.join("b.txt");
        fs::write(&file_a, "a").unwrap();
        fs::write(&file_b, "b").unwrap();

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![
            root_a.to_string_lossy().into_owned(),
            root_b.to_string_lossy().into_owned(),
        ];
        cfg.index.exclude_paths = vec![];
        cfg.index.exclude = vec![];
        cfg.index.include_hidden = true;
        cfg.index.index_apps = false;

        let store = IndexStore::new();
        let initial = scanner::scan_compact(
            &cfg.resolve_paths(),
            Arc::new(CompiledFilter::from_config(&cfg)),
            cfg.index.include_hidden,
            Arc::new(AtomicUsize::new(0)),
        )
        .unwrap();
        store.replace_compact(initial);

        let (_, control) = mpsc::channel();
        let scope = Scope::new(&cfg);
        let fp = scope.fingerprint;
        let snap = scope.snapshot.clone();
        let mut service = Service {
            store: store.clone(),
            config_store: ConfigStore::new(cfg.clone()),
            metrics: Arc::new(WatcherMetrics::default()),
            scan_lock: Arc::new(std::sync::Mutex::new(())),
            on_batch: |_: usize| {},
            persist: |_: &[u8], _: crate::cache::Checkpoint, _: &PrimaryScopeSnapshot| {},
            scope,
            scope_checked: Instant::now(),
            scope_refreshed: Instant::now(),
            fda_checked: Instant::now(),
            stream: None,
            events: mpsc::channel().1,
            wake_tx: Arc::new(std::sync::Mutex::new(None)),
            control,
            applied: Arc::new(AtomicU64::new(10)),
            fingerprint: Arc::new(AtomicU64::new(fp)),
            applied_snapshot: Arc::new(RwLock::new(snap)),
            pending_snapshot: None,
            pending_prune: false,
            pending_snapshot_active: Arc::new(AtomicBool::new(false)),
            scope_dirty_seq: Arc::new(AtomicU64::new(0)),
            applied_scope_seq: Arc::new(AtomicU64::new(0)),
            pending_scope_seq: None,
            missing_root_since: Vec::new(),
            pending: HashMap::new(),
            seen: 10,
            first_pending: None,
            rescans: Vec::new(),
            scope_rescans: Vec::new(),
            root_changed_restart: false,
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
            unpersisted_events: 0,
            primary_volumes_dir: None,
            last_index_config: cfg.index.clone(),
            on_config_updated: None,
            on_reconcile_progress: None,
            on_reconcile_ready: None,
            on_reconcile_error: None,
            on_scope_applied: None,
        };

        // 1. 移走根目录 B，FSEvents 仅上报根路径一条 ITEM_RENAMED
        fs::rename(&root_b, &root_b_moved).unwrap();
        service.intake(vec![event(root_b.clone(), crate::fsevents::ITEM_RENAMED, 11)]);
        service.flush();
        assert!(!service.full_reconcile);
        assert!(indexed(&store, &file_a), "sibling root A must remain untouched");
        assert!(!indexed(&store, &file_b), "moved root B subtree must be evicted");

        // 2. 原位重建新目录 B 并上报 ROOT_CHANGED -> 局部回扫 B 收录新文件，不触发全量重建
        fs::create_dir_all(&root_b).unwrap();
        let replaced = root_b.join("replaced.txt");
        fs::write(&replaced, "new").unwrap();
        service.intake(vec![event(root_b.clone(), crate::fsevents::ROOT_CHANGED, 12)]);
        service.flush();
        assert!(!service.full_reconcile);
        assert!(indexed(&store, &replaced));

        // 3. 根仍在磁盘上时的普通 ITEM_CREATED / ITEM_RENAMED 绝不触发同步全扫
        service.intake(vec![
            event(root_a.clone(), crate::fsevents::ITEM_CREATED, 13),
            event(root_a.clone(), crate::fsevents::ITEM_RENAMED, 14),
        ]);
        assert!(service.pending.is_empty());
        assert!(service.rescans.is_empty());
        assert!(!service.full_reconcile);
    }

    #[test]
    fn apply_missing_root_grace_expires_after_grace_and_resets_on_recovery() {
        let a = PathBuf::from("/tmp/root_a");
        let b = PathBuf::from("/tmp/root_b");
        let prev = vec![a.clone(), b.clone()];
        let mut missing_since = Vec::new();
        let t0 = Instant::now();
        let grace = Duration::from_secs(60);

        // 首次缺失：在宽限期内保留 B
        let mut cur = vec![a.clone()];
        apply_missing_root_grace(&prev, &mut cur, &[], &mut missing_since, t0, grace);
        assert_eq!(cur, vec![a.clone(), b.clone()]);
        assert_eq!(missing_since.len(), 1);

        // 45s 仍未超时：继续保留 B
        let mut cur = vec![a.clone()];
        apply_missing_root_grace(
            &prev,
            &mut cur,
            &[],
            &mut missing_since,
            t0 + Duration::from_secs(45),
            grace,
        );
        assert_eq!(cur, vec![a.clone(), b.clone()]);

        // 60s 到期：不再补回 B，且移除计时项允许退出 scope
        let mut cur = vec![a.clone()];
        apply_missing_root_grace(
            &prev,
            &mut cur,
            &[],
            &mut missing_since,
            t0 + Duration::from_secs(60),
            grace,
        );
        assert_eq!(cur, vec![a.clone()]);
        assert!(missing_since.is_empty());

        // 若在宽限期内重新出现则清零计时；若转入 excluded_mount_roots 则立即放行不补回
        let mut cur = vec![a.clone()];
        apply_missing_root_grace(&prev, &mut cur, &[], &mut missing_since, t0, grace);
        assert_eq!(missing_since.len(), 1);
        let mut cur_back = vec![a.clone(), b.clone()];
        apply_missing_root_grace(
            &prev,
            &mut cur_back,
            &[],
            &mut missing_since,
            t0 + Duration::from_secs(20),
            grace,
        );
        assert!(missing_since.is_empty());

        let mut cur_excl = vec![a.clone()];
        apply_missing_root_grace(&prev, &mut cur_excl, &[b.clone()], &mut missing_since, t0, grace);
        assert_eq!(cur_excl, vec![a.clone()]);
        assert!(missing_since.is_empty());
    }

    #[test]
    fn poll_scope_preserves_upgrade_and_event_rescans_across_noop_prune_and_incremental() {
        let dir = TempDir::new().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let root_a = base.join("A");
        let root_b = base.join("B");
        let app_dir = root_a.join("MyTool.app");
        let wrapper_dir = app_dir.join("Wrapper");
        fs::create_dir_all(&wrapper_dir).unwrap();
        fs::create_dir_all(&root_b).unwrap();
        let wrapper_file = wrapper_dir.join("payload.bin");
        let file_a = root_a.join("a.txt");
        let file_b = root_b.join("b.txt");
        fs::write(&wrapper_file, "bin").unwrap();
        fs::write(&file_a, "a").unwrap();
        fs::write(&file_b, "b").unwrap();

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![root_a.to_string_lossy().into_owned()];
        cfg.index.exclude_paths = vec![];
        cfg.index.exclude = vec![];
        cfg.index.include_hidden = true;
        cfg.index.index_apps = false;

        // 模拟 v5 升级认领后的初始索引：仅有 a.txt 与 MyTool.app 自身，缺失 Wrapper/payload.bin，
        // 且已把 MyTool.app 排入 self.rescans 等待首次 flush 补扫。
        let store = IndexStore::new();
        let seed = crate::compact_index::CompactIndex::from_entries(vec![
            file_entry_from_path(&root_a).unwrap(),
            file_entry_from_path(&file_a).unwrap(),
            file_entry_from_path(&app_dir).unwrap(),
        ])
        .unwrap();
        store.replace_compact(seed);

        let (_, control) = mpsc::channel();
        let scope = Scope::new(&cfg);
        let fp = scope.fingerprint;
        let snap = scope.snapshot.clone();
        let mut service = Service {
            store: store.clone(),
            config_store: ConfigStore::new(cfg.clone()),
            metrics: Arc::new(WatcherMetrics::default()),
            scan_lock: Arc::new(std::sync::Mutex::new(())),
            on_batch: |_: usize| {},
            persist: |_: &[u8], _: crate::cache::Checkpoint, _: &PrimaryScopeSnapshot| {},
            scope,
            scope_checked: Instant::now(),
            scope_refreshed: Instant::now(),
            fda_checked: Instant::now(),
            stream: None,
            events: mpsc::channel().1,
            wake_tx: Arc::new(std::sync::Mutex::new(None)),
            control,
            applied: Arc::new(AtomicU64::new(10)),
            fingerprint: Arc::new(AtomicU64::new(fp)),
            applied_snapshot: Arc::new(RwLock::new(snap.clone())),
            pending_snapshot: Some(snap),
            pending_prune: false,
            pending_snapshot_active: Arc::new(AtomicBool::new(true)),
            scope_dirty_seq: Arc::new(AtomicU64::new(0)),
            applied_scope_seq: Arc::new(AtomicU64::new(0)),
            pending_scope_seq: None,
            missing_root_since: Vec::new(),
            pending: HashMap::new(),
            seen: 10,
            first_pending: Some(Instant::now()),
            rescans: vec![app_dir.clone()],
            scope_rescans: Vec::new(),
            root_changed_restart: false,
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
            unpersisted_events: 0,
            primary_volumes_dir: None,
            last_index_config: cfg.index.clone(),
            on_config_updated: None,
            on_reconcile_progress: None,
            on_reconcile_ready: None,
            on_reconcile_error: None,
            on_scope_applied: None,
        };

        // 1. 首次 flush 前发生 IncrementalUpdate（新增 B）：不得覆盖 self.rescans 中的 MyTool.app
        let mut cfg_ab = cfg.clone();
        cfg_ab.index.index_paths.push(root_b.to_string_lossy().into_owned());
        rescope(&mut service, |c| *c = cfg_ab.clone());
        assert_eq!(service.rescans, vec![app_dir.clone()]);
        assert_eq!(service.scope_rescans, vec![root_b.clone()]);
        assert!(service.pending_snapshot_active.load(Ordering::Acquire));

        // 2. 尚未 flush 前又改回仅 A（命中 NoOp）：不得清空 self.rescans，且保持 pending_snapshot_active = true
        rescope(&mut service, |c| *c = cfg.clone());
        assert_eq!(service.rescans, vec![app_dir.clone()]);
        assert!(service.scope_rescans.is_empty());
        assert!(service.pending_snapshot_active.load(Ordering::Acquire));

        // 3. 尚未 flush 前又收窄排除一个无关目录（命中 PruneOnly）：仍保留 self.rescans 与 pending_snapshot_active = true
        let mut cfg_prune = cfg.clone();
        cfg_prune.index.exclude.push("unused_dir".into());
        rescope(&mut service, |c| *c = cfg_prune);
        assert_eq!(service.rescans, vec![app_dir.clone()]);
        assert!(service.pending_snapshot_active.load(Ordering::Acquire));

        // 4. flush 执行后，MyTool.app/Wrapper/payload.bin 被成功补扫且复位 pending_snapshot_active
        service.flush();
        assert!(indexed(&store, &wrapper_file));
        assert!(!service.pending_snapshot_active.load(Ordering::Acquire));
    }
}
