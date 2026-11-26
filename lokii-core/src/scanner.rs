use crate::indexer::FileEntry;
use crate::path_utils::{path_starts_with_ci, paths_equal_ci};
use crate::permissions::has_full_disk_access;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// 没有完全磁盘访问权限时，macOS 会弹窗询问的受保护路径。
/// 直接跳过这些路径，避免触发系统权限对话框。
pub(crate) fn protected_paths() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return vec![];
    };
    let lib = home.join("Library");
    let base_paths = vec![
        // ~/Library 下的受保护目录
        lib.join("Application Support"),
        lib.join("Caches"),
        lib.join("Containers"),
        lib.join("Group Containers"),
        lib.join("HTTPStorages"),
        lib.join("Application Scripts"),
        lib.join("Metadata"),
        lib.join("Cookies"),
        lib.join("IdentityServices"),
        lib.join("Sharing"),
        lib.join("Mail"),
        lib.join("Messages"),
        lib.join("Safari"),
        lib.join("Calendars"),
        lib.join("Reminders"),
        lib.join("HomeKit"),
        lib.join("Photos"),
        lib.join("Music"),
        lib.join("Media"),
        lib.join("MediaLibrary"),
        lib.join("PersonalizationPortrait"),
        lib.join("Suggestions"),
        // 用户主目录下的受保护目录
        home.join("Desktop"),
        home.join("Downloads"),
        home.join("Documents"),
        home.join("Pictures"),
        home.join("Movies"),
        home.join("Music"),
        home.join("iTunes"),
        // 系统目录（有些可能触发弹窗，有些是为防止误扫）
        PathBuf::from("/System/Library/Assets"),
        PathBuf::from("/Library/Application Support/com.apple.TCC"),
    ];

    // macOS 可能返回 Data Volume 真实路径（/System/Volumes/Data/...），
    // 为避免 /Users/... 过滤失效，这里补充等价别名路径。
    with_macos_data_volume_aliases(base_paths)
}

fn with_macos_data_volume_aliases(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let users_prefix = Path::new("/Users");
    let lib_prefix = Path::new("/Library");
    let users_data_prefix = Path::new("/System/Volumes/Data/Users");
    let lib_data_prefix = Path::new("/System/Volumes/Data/Library");

    let mut out = Vec::with_capacity(paths.len() * 2);
    for p in paths {
        out.push(p.clone());

        if let Ok(rest) = p.strip_prefix(users_prefix) {
            out.push(users_data_prefix.join(rest));
            continue;
        }

        if let Ok(rest) = p.strip_prefix(lib_prefix) {
            out.push(lib_data_prefix.join(rest));
        }
    }

    out.sort();
    out.dedup();
    out
}

pub(crate) fn is_protected_or_descendant(path: &Path, protected: &[PathBuf]) -> bool {
    protected
        .iter()
        .any(|p| paths_equal_ci(path, p) || path_starts_with_ci(path, p))
}

#[cfg(test)]
fn dummy_filter() -> std::sync::Arc<crate::filter::CompiledFilter> {
    let mut cfg = crate::config::AppConfig::default();
    cfg.index.index_paths = vec![];
    std::sync::Arc::new(crate::filter::CompiledFilter::from_config(&cfg))
}

pub(crate) fn contains_protected_descendant(path: &Path, protected: &[PathBuf]) -> bool {
    protected.iter().any(|p| path_starts_with_ci(p, path))
}

/// 并行扫描目录并在遍历期实时应用排除过滤器，避免进入已排除子树。
///
/// 在 macOS 上采用多线程目录队列 + `getattrlistbulk(2)` 批量拉取属性，将原本逐文件的 `lstat`
/// 合并为批量系统调用，并在 `scan_compact` 路径下通过借用缓冲与 `InternHints` 直写 `CompactIndex`。
pub fn scan_directories(
    paths: &[PathBuf],
    filter: Arc<crate::filter::CompiledFilter>,
    include_hidden: bool,
    progress: Arc<AtomicUsize>,
) -> Vec<FileEntry> {
    let mut entries = Vec::new();
    scan_borrowed_with_sink(
        paths,
        filter,
        include_hidden,
        progress,
        |name, path, is_dir, size, modified, created_at, extension| {
            entries.push(FileEntry {
                name: name.to_owned(),
                path: path.to_owned(),
                is_dir,
                size,
                modified,
                created_at,
                extension: extension.to_owned(),
            });
            Ok(())
        },
    )
    .expect("infallible vector sink");
    entries
}

pub fn scan_compact(
    paths: &[PathBuf],
    filter: Arc<crate::filter::CompiledFilter>,
    include_hidden: bool,
    progress: Arc<AtomicUsize>,
) -> Result<crate::compact_index::CompactIndex, crate::compact_index::IndexError> {
    let mut index = crate::compact_index::CompactIndex::new();
    let mut hints = crate::compact_index::InternHints::default();
    scan_borrowed_with_sink(
        paths,
        filter,
        include_hidden,
        progress,
        |name, path, is_dir, size, modified, created_at, extension| {
            index.push_borrowed_with_hints(
                name,
                path,
                is_dir,
                size,
                modified,
                created_at,
                extension,
                &mut hints,
            )
        },
    )?;
    Ok(index)
}

/// 剔除被更上层索引根完全覆盖、且沿途无排除或隐藏剪枝阻断的冗余子扫描根。
pub(crate) fn deduplicate_scan_roots(
    paths: &[PathBuf],
    filter: &crate::filter::CompiledFilter,
    include_hidden: bool,
) -> Vec<PathBuf> {
    let mut unique_paths: Vec<&Path> = Vec::new();
    for p in paths {
        if !unique_paths.iter().any(|existing| paths_equal_ci(existing, p)) {
            unique_paths.push(p.as_path());
        }
    }

    let mut clean_roots: Vec<PathBuf> = Vec::new();
    for (i, candidate) in unique_paths.iter().enumerate() {
        // 存在更上层的根覆盖 candidate，且两者之间没有任何一处被排除或隐藏剪枝阻断时，
        // candidate 才能被视为已包含在该根的遍历中。
        let is_redundant = unique_paths.iter().enumerate().any(|(j, other)| {
            j != i
                && path_starts_with_ci(candidate, other)
                && candidate
                    .ancestors()
                    .take_while(|ancestor| !paths_equal_ci(ancestor, other))
                    .all(|ancestor| {
                        // 上层扫描器会在 HardDrop 或 IndexShallowContainer 节点剪枝阻断，无法继续深入子项
                        filter.evaluate_with_hidden(ancestor, true, include_hidden)
                            == crate::filter::ScopeDecision::IndexNormally
                    })
        });
        if !is_redundant {
            clean_roots.push(candidate.to_path_buf());
        }
    }
    clean_roots
}

struct PrimaryScannedItem {
    name_start: u32,
    name_len: u32,
    is_dir: bool,
    size: u64,
    modified: u64,
    created_at: u64,
}

/// 单目录扫描批次：整个目录共享一份 `parent` 前缀与连续 `names` 缓冲，
/// 消除工作线程上按文件分配 `String` 的开销，并让接收侧 `InternHints.parent` 100% 命中。
struct PrimaryDirBatch {
    parent: String,
    names: String,
    items: Vec<PrimaryScannedItem>,
}

struct PrimaryQueueState {
    dirs: std::collections::VecDeque<PathBuf>,
    active_workers: usize,
    aborted: bool,
}

struct PrimaryWorkQueue {
    state: parking_lot::Mutex<PrimaryQueueState>,
    cvar: parking_lot::Condvar,
}

impl PrimaryWorkQueue {
    fn new(initial: Vec<PathBuf>) -> Self {
        Self {
            state: parking_lot::Mutex::new(PrimaryQueueState {
                dirs: initial.into_iter().collect(),
                active_workers: 0,
                aborted: false,
            }),
            cvar: parking_lot::Condvar::new(),
        }
    }

    fn pop_blocking(&self) -> Option<PathBuf> {
        let mut guard = self.state.lock();
        loop {
            if guard.aborted {
                return None;
            }
            // 采用 LIFO 深度优先出队：显著压缩峰值目录队列内存占用，并提升内核 VFS dcache 局部性
            if let Some(dir) = guard.dirs.pop_back() {
                guard.active_workers += 1;
                return Some(dir);
            }
            if guard.active_workers == 0 {
                return None;
            }
            self.cvar.wait(&mut guard);
        }
    }

    fn finish_dir(&self, new_subdirs: &mut Vec<PathBuf>) {
        let mut guard = self.state.lock();
        guard.active_workers = guard.active_workers.saturating_sub(1);
        let added = if !guard.aborted && !new_subdirs.is_empty() {
            let count = new_subdirs.len();
            guard.dirs.extend(new_subdirs.drain(..));
            count
        } else {
            new_subdirs.clear();
            0
        };
        if guard.dirs.is_empty() && guard.active_workers == 0 {
            self.cvar.notify_all();
        } else if added == 1 {
            // 单子目录入队时仅唤醒一个等待线程，规避多核条件变量惊群
            self.cvar.notify_one();
        } else if added > 1 {
            self.cvar.notify_all();
        }
    }

    fn abort(&self) {
        let mut guard = self.state.lock();
        guard.aborted = true;
        guard.dirs.clear();
        self.cvar.notify_all();
    }
}

/// 确保工作线程即使提前退出或 panic 也会递减 `active_workers`，防止其他线程在条件变量上永久阻塞。
struct ActiveDirGuard<'a> {
    queue: &'a PrimaryWorkQueue,
    subdirs: Vec<PathBuf>,
    finished: bool,
}

impl<'a> ActiveDirGuard<'a> {
    fn new(queue: &'a PrimaryWorkQueue) -> Self {
        Self {
            queue,
            subdirs: Vec::new(),
            finished: false,
        }
    }

    fn finish(mut self) {
        self.finished = true;
        self.queue.finish_dir(&mut self.subdirs);
    }
}

impl Drop for ActiveDirGuard<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.subdirs.clear();
            self.queue.finish_dir(&mut self.subdirs);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_borrowed_with_sink(
    paths: &[PathBuf],
    filter: Arc<crate::filter::CompiledFilter>,
    include_hidden: bool,
    progress: Arc<AtomicUsize>,
    mut sink: impl FnMut(&str, &str, bool, u64, u64, u64, &str) -> Result<(), crate::compact_index::IndexError>,
) -> Result<(), crate::compact_index::IndexError> {
    // 没有 FDA 时跳过受保护路径，避免触发系统权限弹窗
    let skip_paths: Vec<PathBuf> = if has_full_disk_access() {
        vec![]
    } else {
        protected_paths()
    };

    let scan_roots = deduplicate_scan_roots(paths, &filter, include_hidden);
    let mut initial_dirs = Vec::new();
    let mut ext_buf = String::with_capacity(32);

    for root in &scan_roots {
        if is_protected_or_descendant(root, &skip_paths) {
            continue;
        }
        let Ok(root_meta) = std::fs::metadata(root) else {
            continue;
        };
        let root_is_dir = root_meta.is_dir();

        // 显式索引根（depth == None）在未配置全局 include_roots 的测试桩中仍需放行，仅检查系统噪声与显式排除前缀
        let should_descend = if filter.has_empty_include_roots() {
            if crate::filter::is_system_noise_path(root) || filter.matches_excluded_prefix(root) {
                continue;
            }
            root_is_dir
        } else {
            match filter.evaluate_with_hidden(root, root_is_dir, include_hidden) {
                crate::filter::ScopeDecision::HardDrop => continue,
                crate::filter::ScopeDecision::IndexShallowContainer => false,
                crate::filter::ScopeDecision::IndexNormally => root_is_dir,
            }
        };

        let root_str = root.to_string_lossy();
        let root_name = root
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_else(|| root_str.clone());
        let root_size = if root_is_dir { 0 } else { root_meta.len() };
        let root_modified = system_time_to_unix_secs(root_meta.modified());
        let root_created = system_time_to_unix_secs(root_meta.created());
        write_extension_lowercase(&root_name, root_is_dir, &mut ext_buf);
        sink(
            &root_name,
            &root_str,
            root_is_dir,
            root_size,
            root_modified,
            root_created,
            &ext_buf,
        )?;
        progress.fetch_add(1, Ordering::Relaxed);

        if should_descend {
            initial_dirs.push(root.clone());
        }
    }

    if initial_dirs.is_empty() {
        return Ok(());
    }

    let worker_count = std::thread::available_parallelism()
        .map(|n| n.get().clamp(2, 8))
        .unwrap_or(4);
    let queue = PrimaryWorkQueue::new(initial_dirs);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let (tx, rx) = std::sync::mpsc::sync_channel::<PrimaryDirBatch>(64);

    let filter_ref = &filter;
    let skip_paths_ref = &skip_paths;
    let queue_ref = &queue;
    let cancel_ref = &cancel;

    /// 确保接收侧即使在 sink 中发生 panic，也会立即置位 cancel 与 abort 唤醒并终止所有后台扫描 worker。
    struct ScopeAbortGuard<'a> {
        cancel: &'a std::sync::atomic::AtomicBool,
        queue: &'a PrimaryWorkQueue,
    }
    impl Drop for ScopeAbortGuard<'_> {
        fn drop(&mut self) {
            self.cancel.store(true, Ordering::Relaxed);
            self.queue.abort();
        }
    }

    std::thread::scope(|scope| {
        let _abort_on_exit = ScopeAbortGuard {
            cancel: cancel_ref,
            queue: queue_ref,
        };
        for _ in 0..worker_count {
            let worker_tx = tx.clone();
            scope.spawn(move || {
                #[cfg(target_os = "macos")]
                let mut bulk_buf = vec![0u64; 8192];
                let mut path_buf = String::with_capacity(512);
                let mut direct_protected: Vec<&PathBuf> = Vec::new();
                let always_mounted = || true;

                while let Some(dir) = queue_ref.pop_blocking() {
                    let mut dir_guard = ActiveDirGuard::new(queue_ref);
                    if cancel_ref.load(Ordering::Relaxed) {
                        dir_guard.finish();
                        break;
                    }

                    let dir_str = dir.to_string_lossy();
                    path_buf.clear();
                    if dir_str == "/" {
                        path_buf.push('/');
                    } else {
                        path_buf.push_str(dir_str.trim_end_matches('/'));
                        path_buf.push('/');
                    }
                    let prefix_len = path_buf.len();

                    // 目录级预判：仅提取直接挂在当前目录之下的受保护子路径与子项过滤上下文，
                    // 避免在枚举海量文件时重复扫描 70+ 条 TCC 规则与 25+ 条排除前缀。
                    direct_protected.clear();
                    if !skip_paths_ref.is_empty() {
                        let child_depth = dir.components().count() + 1;
                        for protected in skip_paths_ref {
                            if protected.components().count() == child_depth
                                && path_starts_with_ci(protected, &dir)
                            {
                                direct_protected.push(protected);
                            }
                        }
                    }
                    let dir_ctx = filter_ref.dir_child_context(&dir, include_hidden);

                    let mut names = String::new();
                    let mut items = Vec::new();
                    let subdirs = &mut dir_guard.subdirs;

                    let mut handle_entry = |raw_name: &std::ffi::OsStr,
                                            name: &str,
                                            is_dir: bool,
                                            size: u64,
                                            modified: u64,
                                            created_at: u64|
                     -> Result<(), VolumeScanError> {
                        path_buf.truncate(prefix_len);
                        path_buf.push_str(name);
                        let full_path = Path::new(&path_buf);

                        // 没有 FDA 时仅在当前目录存在直接受保护子项时才比对 TCC 列表，避免触发系统权限弹窗
                        if !direct_protected.is_empty()
                            && direct_protected
                                .iter()
                                .any(|protected| paths_equal_ci(full_path, protected))
                        {
                            return Ok(());
                        }

                        match filter_ref.evaluate_child(&dir_ctx, full_path, name, is_dir) {
                            crate::filter::ScopeDecision::HardDrop => return Ok(()),
                            crate::filter::ScopeDecision::IndexShallowContainer => {}
                            crate::filter::ScopeDecision::IndexNormally => {
                                if is_dir {
                                    // 用原始 OsStr 拼接子目录路径，避免非 UTF-8 目录名经 lossy 解码后 open 返回 ENOENT 丢子树
                                    subdirs.push(dir.join(raw_name));
                                }
                            }
                        }

                        let name_start = names.len() as u32;
                        names.push_str(name);
                        items.push(PrimaryScannedItem {
                            name_start,
                            name_len: name.len() as u32,
                            is_dir,
                            size,
                            modified,
                            created_at,
                        });
                        Ok(())
                    };

                    #[cfg(target_os = "macos")]
                    {
                        if let Ok(macos_bulk::BulkOutcome::Unsupported) =
                            macos_bulk::enumerate_dir_bulk(
                                &dir,
                                false,
                                false,
                                &mut bulk_buf,
                                cancel_ref,
                                &always_mounted,
                                &mut handle_entry,
                            )
                        {
                            let _ = enumerate_dir_fallback(
                                &dir,
                                false,
                                false,
                                cancel_ref,
                                &always_mounted,
                                &mut handle_entry,
                            );
                        }
                    }

                    #[cfg(not(target_os = "macos"))]
                    {
                        let _ = enumerate_dir_fallback(
                            &dir,
                            false,
                            false,
                            cancel_ref,
                            &always_mounted,
                            &mut handle_entry,
                        );
                    }

                    // 先把新发现的子目录放回工作队列，再发送当前目录条目批次，避免通道背压时饿死空闲 worker
                    dir_guard.finish();

                    if !items.is_empty() {
                        let parent = path_buf[..prefix_len].to_owned();
                        if worker_tx
                            .send(PrimaryDirBatch {
                                parent,
                                names,
                                items,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            });
        }
        // 关闭主线程持有的发送端，确保所有 worker 结束后 rx 正常结束迭代
        drop(tx);

        let mut sink_path_buf = String::with_capacity(512);
        let mut sink_result: Result<(), crate::compact_index::IndexError> = Ok(());

        for batch in rx {
            sink_path_buf.clear();
            sink_path_buf.push_str(&batch.parent);
            let prefix_len = sink_path_buf.len();
            let mut batch_count = 0usize;

            for item in &batch.items {
                let start = item.name_start as usize;
                let end = start + item.name_len as usize;
                let name = &batch.names[start..end];
                sink_path_buf.truncate(prefix_len);
                sink_path_buf.push_str(name);
                write_extension_lowercase(name, item.is_dir, &mut ext_buf);
                if let Err(err) = sink(
                    name,
                    &sink_path_buf,
                    item.is_dir,
                    item.size,
                    item.modified,
                    item.created_at,
                    &ext_buf,
                ) {
                    sink_result = Err(err);
                    cancel_ref.store(true, Ordering::Relaxed);
                    queue_ref.abort();
                    break;
                }
                batch_count += 1;
            }
            if batch_count > 0 {
                progress.fetch_add(batch_count, Ordering::Relaxed);
            }
            if sink_result.is_err() {
                break;
            }
        }

        sink_result
    })
}

/// 扫描卷时可能出现的异常中止错误。
#[derive(Debug, PartialEq, Eq)]
pub enum VolumeScanError {
    Cancelled,
    Disconnected(String),
    RootUnavailable(String),
    IndexLimit(String),
}

impl std::fmt::Display for VolumeScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => write!(f, "volume scan cancelled"),
            Self::Disconnected(msg) => write!(f, "volume disconnected: {msg}"),
            Self::RootUnavailable(msg) => write!(f, "volume root unavailable: {msg}"),
            Self::IndexLimit(msg) => write!(f, "volume index limit reached: {msg}"),
        }
    }
}

impl std::error::Error for VolumeScanError {}

/// 判断底层 I/O 错误是否代表无需挂载表核对即可确定的卷级传输断开或硬件离线。
#[inline]
pub(crate) fn is_deterministic_disconnect_error(err: &std::io::Error) -> bool {
    #[cfg(unix)]
    if let Some(code) = err.raw_os_error() {
        match code {
            libc::ENOTCONN
            | libc::ETIMEDOUT
            | libc::ESTALE
            | libc::EIO
            | libc::EHOSTDOWN
            | libc::ENETDOWN
            | libc::ENXIO => return true,
            _ => {}
        }
    }
    matches!(
        err.kind(),
        std::io::ErrorKind::NotConnected | std::io::ErrorKind::TimedOut
    )
}

/// 判断底层 I/O 错误是否代表卷级连接断开或硬件离线。
///
/// 错误分级原则：
/// - `ENOTCONN`、`ETIMEDOUT`、`ESTALE`、`EIO`、`EHOSTDOWN`、`ENETDOWN`、`ENXIO` 恒为断开错误；
/// - `ENOENT` 在目标卷仍挂载时为正常瞬态缺失，若核对 `is_mounted() == false` 则代表整个挂载点已脱落；
/// - `EACCES`、`EPERM`、`ELOOP` 为单文件或局部子目录权限/循环问题，不应中断全卷扫描。
pub fn is_disconnect_error(err: &std::io::Error, is_mounted: &dyn Fn() -> bool) -> bool {
    if is_deterministic_disconnect_error(err) {
        return true;
    }
    #[cfg(unix)]
    if let Some(code) = err.raw_os_error() {
        match code {
            libc::ENOENT => return !is_mounted(),
            libc::EACCES | libc::EPERM | libc::ELOOP => return false,
            _ => {}
        }
    }
    match err.kind() {
        std::io::ErrorKind::NotFound => !is_mounted(),
        _ => false,
    }
}

#[inline]
fn write_extension_lowercase(name: &str, is_dir: bool, out: &mut String) {
    out.clear();
    if is_dir {
        return;
    }
    if let Some(ext) = Path::new(name).extension().and_then(|e| e.to_str()) {
        if ext.is_ascii() {
            out.reserve(ext.len());
            for b in ext.bytes() {
                out.push(b.to_ascii_lowercase() as char);
            }
        } else {
            out.push_str(&ext.to_lowercase());
        }
    }
}

#[inline]
fn system_time_to_unix_secs(res: std::io::Result<std::time::SystemTime>) -> u64 {
    res.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(target_os = "macos")]
mod macos_bulk {
    use super::{is_disconnect_error, VolumeScanError};
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};

    const ATTR_CMN_NAME: u32 = 0x0000_0001;
    const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
    const ATTR_CMN_CRTIME: u32 = 0x0000_0200;
    const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
    const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;

    /// macOS `enum vtype`: `VNON = 0, VREG = 1, VDIR = 2`.
    const VDIR: u32 = 2;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct AttrSet {
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct AttrReference {
        attr_dataoffset: i32,
        attr_length: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct RawTimespec {
        tv_sec: i64,
        tv_nsec: i64,
    }

    extern "C" {
        fn getattrlistbulk(
            dirfd: libc::c_int,
            alist: *mut libc::attrlist,
            attribute_buffer: *mut libc::c_void,
            buffer_size: libc::size_t,
            options: u64,
        ) -> libc::c_int;
    }

    /// 持有目录文件描述符并在退出作用域时立即关闭，保证取消或断开后不遗留占用句柄（避免 EBUSY）。
    struct OwnedDirFd(libc::c_int);

    impl Drop for OwnedDirFd {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.0);
            }
        }
    }

    pub(super) enum BulkOutcome {
        Done,
        Unsupported,
    }

    /// 通过 macOS `getattrlistbulk(2)` 批量拉取目录下条目的名称与核心元数据，
    /// 对 SMB/网络卷可将原本的 `readdir + N 次 lstat` 合并为少量批量系统调用，显著降低网络往返与 CPU 占用。
    pub(super) fn enumerate_dir_bulk(
        dir: &Path,
        is_root: bool,
        abort_on_disconnect: bool,
        buf: &mut [u64],
        cancel: &AtomicBool,
        is_mounted: &dyn Fn() -> bool,
        mut on_entry: impl FnMut(&std::ffi::OsStr, &str, bool, u64, u64, u64) -> Result<(), VolumeScanError>,
    ) -> Result<BulkOutcome, VolumeScanError> {
        let Ok(c_path) = CString::new(dir.as_os_str().as_bytes()) else {
            return Ok(BulkOutcome::Unsupported);
        };
        // 非根目录启用 O_NOFOLLOW，防止父目录枚举与打开子目录之间发生符号链接替换（TOCTOU）
        let mut open_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
        if !is_root {
            open_flags |= libc::O_NOFOLLOW;
        }
        let fd = unsafe { libc::open(c_path.as_ptr(), open_flags) };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            if abort_on_disconnect && is_disconnect_error(&err, is_mounted) {
                return Err(VolumeScanError::Disconnected(err.to_string()));
            }
            if is_root {
                return Err(VolumeScanError::RootUnavailable(err.to_string()));
            }
            return Ok(BulkOutcome::Done);
        }
        let _fd_guard = OwnedDirFd(fd);

        let mut attrlist: libc::attrlist = unsafe { std::mem::zeroed() };
        attrlist.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        attrlist.commonattr = ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_ERROR
            | ATTR_CMN_NAME
            | ATTR_CMN_OBJTYPE
            | ATTR_CMN_CRTIME
            | ATTR_CMN_MODTIME;
        attrlist.fileattr = ATTR_FILE_DATALENGTH;

        let buf_bytes = buf.len() * std::mem::size_of::<u64>();
        let mut saw_any_batch = false;

        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(VolumeScanError::Cancelled);
            }

            let count = unsafe {
                getattrlistbulk(
                    fd,
                    &mut attrlist,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf_bytes,
                    0,
                )
            };
            if count == 0 {
                return Ok(BulkOutcome::Done);
            }
            if count < 0 {
                let err = std::io::Error::last_os_error();
                if let Some(code) = err.raw_os_error() {
                    if !saw_any_batch
                        && matches!(code, libc::ENOTSUP | libc::EOPNOTSUPP | libc::EINVAL)
                    {
                        return Ok(BulkOutcome::Unsupported);
                    }
                }
                if abort_on_disconnect && is_disconnect_error(&err, is_mounted) {
                    return Err(VolumeScanError::Disconnected(err.to_string()));
                }
                if is_root && !saw_any_batch {
                    return Err(VolumeScanError::RootUnavailable(err.to_string()));
                }
                return Ok(BulkOutcome::Done);
            }
            saw_any_batch = true;

            let raw = unsafe {
                std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf_bytes)
            };
            let mut pos = 0usize;
            for entry_idx in 0..(count as usize) {
                if (entry_idx & 0xff) == 0 && cancel.load(Ordering::Relaxed) {
                    return Err(VolumeScanError::Cancelled);
                }
                if pos + 4 > raw.len() {
                    break;
                }
                let entry_len = unsafe {
                    std::ptr::read_unaligned(raw.as_ptr().add(pos) as *const u32)
                } as usize;
                let entry_end = pos.saturating_add(entry_len);
                if entry_len < 4 + std::mem::size_of::<AttrSet>() || entry_end > raw.len() {
                    break;
                }

                let mut cursor = pos + 4;
                let returned = unsafe {
                    std::ptr::read_unaligned(raw.as_ptr().add(cursor) as *const AttrSet)
                };
                cursor += std::mem::size_of::<AttrSet>();

                // 按 getattrlistbulk(2) 约定，ATTR_CMN_ERROR 紧跟在 ATTR_CMN_RETURNED_ATTRS 之后
                if (returned.commonattr & ATTR_CMN_ERROR) != 0 {
                    if cursor + 4 > entry_end {
                        pos = entry_end;
                        continue;
                    }
                    let err_code = unsafe {
                        std::ptr::read_unaligned(raw.as_ptr().add(cursor) as *const u32)
                    };
                    cursor += 4;
                    if err_code != 0 {
                        if abort_on_disconnect {
                            let io_err = std::io::Error::from_raw_os_error(err_code as i32);
                            if is_disconnect_error(&io_err, is_mounted) {
                                return Err(VolumeScanError::Disconnected(io_err.to_string()));
                            }
                        }
                        pos = entry_end;
                        continue;
                    }
                }

                let mut name_slice: Option<&[u8]> = None;
                if (returned.commonattr & ATTR_CMN_NAME) != 0 {
                    if cursor + std::mem::size_of::<AttrReference>() > entry_end {
                        pos = entry_end;
                        continue;
                    }
                    let field_pos = cursor;
                    let attr_ref = unsafe {
                        std::ptr::read_unaligned(raw.as_ptr().add(cursor) as *const AttrReference)
                    };
                    cursor += std::mem::size_of::<AttrReference>();
                    if attr_ref.attr_dataoffset >= 0 && attr_ref.attr_length > 0 {
                        let start = field_pos.saturating_add(attr_ref.attr_dataoffset as usize);
                        let end = start.saturating_add(attr_ref.attr_length as usize);
                        if end <= entry_end {
                            let bytes = &raw[start..end];
                            let trimmed = match bytes.iter().position(|&b| b == 0) {
                                Some(nul) => &bytes[..nul],
                                None => bytes,
                            };
                            name_slice = Some(trimmed);
                        }
                    }
                }

                let mut is_dir = false;
                if (returned.commonattr & ATTR_CMN_OBJTYPE) != 0 {
                    if cursor + 4 > entry_end {
                        pos = entry_end;
                        continue;
                    }
                    let obj_type = unsafe {
                        std::ptr::read_unaligned(raw.as_ptr().add(cursor) as *const u32)
                    };
                    cursor += 4;
                    is_dir = obj_type == VDIR;
                }

                let mut created_at = 0u64;
                if (returned.commonattr & ATTR_CMN_CRTIME) != 0 {
                    if cursor + std::mem::size_of::<RawTimespec>() > entry_end {
                        pos = entry_end;
                        continue;
                    }
                    let ts = unsafe {
                        std::ptr::read_unaligned(raw.as_ptr().add(cursor) as *const RawTimespec)
                    };
                    cursor += std::mem::size_of::<RawTimespec>();
                    created_at = ts.tv_sec.max(0) as u64;
                }

                let mut modified = 0u64;
                if (returned.commonattr & ATTR_CMN_MODTIME) != 0 {
                    if cursor + std::mem::size_of::<RawTimespec>() > entry_end {
                        pos = entry_end;
                        continue;
                    }
                    let ts = unsafe {
                        std::ptr::read_unaligned(raw.as_ptr().add(cursor) as *const RawTimespec)
                    };
                    cursor += std::mem::size_of::<RawTimespec>();
                    modified = ts.tv_sec.max(0) as u64;
                }

                let mut size = 0u64;
                if (returned.fileattr & ATTR_FILE_DATALENGTH) != 0 {
                    if cursor + 8 <= entry_end {
                        let data_len = unsafe {
                            std::ptr::read_unaligned(raw.as_ptr().add(cursor) as *const i64)
                        };
                        if !is_dir {
                            size = data_len.max(0) as u64;
                        }
                    }
                }

                if let Some(bytes) = name_slice {
                    if !bytes.is_empty() && bytes != b"." && bytes != b".." {
                        let raw_os = std::ffi::OsStr::from_bytes(bytes);
                        match std::str::from_utf8(bytes) {
                            Ok(name) => {
                                on_entry(raw_os, name, is_dir, size, modified, created_at)?;
                            }
                            Err(_) => {
                                let owned = String::from_utf8_lossy(bytes);
                                on_entry(raw_os, &owned, is_dir, size, modified, created_at)?;
                            }
                        }
                    }
                }

                pos = entry_end;
            }
        }
    }
}

fn enumerate_dir_fallback(
    dir: &Path,
    is_root: bool,
    abort_on_disconnect: bool,
    cancel: &std::sync::atomic::AtomicBool,
    is_mounted: &dyn Fn() -> bool,
    mut on_entry: impl FnMut(&std::ffi::OsStr, &str, bool, u64, u64, u64) -> Result<(), VolumeScanError>,
) -> Result<(), VolumeScanError> {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(err) => {
            if abort_on_disconnect && is_disconnect_error(&err, is_mounted) {
                return Err(VolumeScanError::Disconnected(err.to_string()));
            }
            if is_root {
                return Err(VolumeScanError::RootUnavailable(err.to_string()));
            }
            return Ok(());
        }
    };

    for entry_res in read_dir {
        if cancel.load(Ordering::Relaxed) {
            return Err(VolumeScanError::Cancelled);
        }
        let entry = match entry_res {
            Ok(e) => e,
            Err(err) => {
                if abort_on_disconnect && is_disconnect_error(&err, is_mounted) {
                    return Err(VolumeScanError::Disconnected(err.to_string()));
                }
                continue;
            }
        };
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(err) => {
                if abort_on_disconnect && is_disconnect_error(&err, is_mounted) {
                    return Err(VolumeScanError::Disconnected(err.to_string()));
                }
                continue;
            }
        };
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let is_dir = meta.is_dir();
        let size = if is_dir { 0 } else { meta.len() };
        let modified = system_time_to_unix_secs(meta.modified());
        let created_at = system_time_to_unix_secs(meta.created());
        on_entry(&name, &name_str, is_dir, size, modified, created_at)?;
    }
    Ok(())
}

/// 原子扫描独立挂载卷并直接流式写入 `CompactIndex`。
///
/// 性能与安全设计：
/// 1. 在 macOS 上优先使用 `getattrlistbulk(2)` 单线程批量读取目录与元数据，规避多线程并发打 SMB 导致的内核锁竞争
///    与 Rayon 线程忙等，同时把原本每文件一次的 `lstat` 合并为批量系统调用；不支持的文件系统自动回退 `read_dir`；
/// 2. 直接借用复用缓冲区并配合 `InternHints` 流式写入 `CompactIndex`，省去中间 `Vec<FileEntry>` 峰值内存与海量堆分配；
/// 3. 每个批次与目录切换均检查 `cancel` 和卷断开错误，RAII 立即关闭目录句柄，确保卸载和取消零残留。
pub fn scan_volume_compact_atomic(
    root: &Path,
    filter: &crate::filter::CompiledFilter,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
    is_mounted: &dyn Fn() -> bool,
    on_progress: &mut dyn FnMut(u64),
) -> Result<crate::compact_index::CompactIndex, VolumeScanError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(VolumeScanError::Cancelled);
    }

    if !is_mounted() {
        return Err(VolumeScanError::Disconnected(format!(
            "volume at {:?} is not mounted",
            root
        )));
    }

    let root_meta = match std::fs::metadata(root) {
        Ok(m) => m,
        Err(err) => {
            if is_disconnect_error(&err, is_mounted) {
                return Err(VolumeScanError::Disconnected(err.to_string()));
            } else {
                return Err(VolumeScanError::RootUnavailable(err.to_string()));
            }
        }
    };

    if !root_meta.is_dir() {
        return Err(VolumeScanError::RootUnavailable(format!(
            "{:?} is not a directory",
            root
        )));
    }

    let mut index = crate::compact_index::CompactIndex::new();
    let mut hints = crate::compact_index::InternHints::default();
    let mut path_buf = String::with_capacity(512);
    let mut ext_buf = String::with_capacity(32);
    #[cfg(target_os = "macos")]
    let mut bulk_buf = vec![0u64; 8192]; // 64 KiB 8 字节对齐批量属性缓冲区

    let root_str = root.to_string_lossy();
    let root_name = root
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_else(|| root_str.clone());
    let root_modified = system_time_to_unix_secs(root_meta.modified());
    let root_created = system_time_to_unix_secs(root_meta.created());
    index
        .push_borrowed_with_hints(
            &root_name,
            &root_str,
            true,
            0,
            root_modified,
            root_created,
            "",
            &mut hints,
        )
        .map_err(|e| VolumeScanError::IndexLimit(e.to_string()))?;

    let mut progress_count = 1u64;
    on_progress(progress_count);

    let mut dirs = std::collections::VecDeque::new();
    dirs.push_back(root.to_path_buf());
    let mut is_first_dir = true;
    let include_hidden = filter.include_hidden();

    while let Some(dir) = dirs.pop_back() {
        if cancel.load(Ordering::Relaxed) {
            return Err(VolumeScanError::Cancelled);
        }

        let dir_str = dir.to_string_lossy();
        path_buf.clear();
        if dir_str == "/" {
            path_buf.push('/');
        } else {
            path_buf.push_str(dir_str.trim_end_matches('/'));
            path_buf.push('/');
        }
        let prefix_len = path_buf.len();
        let is_root = is_first_dir;
        is_first_dir = false;
        let dir_ctx = filter.dir_child_context(&dir, include_hidden);

        let mut handle_entry = |raw_name: &std::ffi::OsStr,
                                name: &str,
                                is_dir: bool,
                                size: u64,
                                modified: u64,
                                created_at: u64|
         -> Result<(), VolumeScanError> {
            path_buf.truncate(prefix_len);
            path_buf.push_str(name);
            let decision = filter.evaluate_child(&dir_ctx, Path::new(&path_buf), name, is_dir);
            match decision {
                crate::filter::ScopeDecision::HardDrop => return Ok(()),
                crate::filter::ScopeDecision::IndexShallowContainer => {}
                crate::filter::ScopeDecision::IndexNormally => {
                    if is_dir {
                        dirs.push_back(dir.join(raw_name));
                    }
                }
            }

            write_extension_lowercase(name, is_dir, &mut ext_buf);
            index
                .push_borrowed_with_hints(
                    name,
                    &path_buf,
                    is_dir,
                    size,
                    modified,
                    created_at,
                    &ext_buf,
                    &mut hints,
                )
                .map_err(|e| VolumeScanError::IndexLimit(e.to_string()))?;

            progress_count += 1;
            on_progress(progress_count);
            Ok(())
        };

        #[cfg(target_os = "macos")]
        {
            match macos_bulk::enumerate_dir_bulk(
                &dir,
                is_root,
                true,
                &mut bulk_buf,
                cancel,
                is_mounted,
                &mut handle_entry,
            )? {
                macos_bulk::BulkOutcome::Done => {}
                macos_bulk::BulkOutcome::Unsupported => {
                    enumerate_dir_fallback(&dir, is_root, true, cancel, is_mounted, &mut handle_entry)?;
                }
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            enumerate_dir_fallback(&dir, is_root, true, cancel, is_mounted, &mut handle_entry)?;
        }
    }

    if cancel.load(Ordering::Relaxed) {
        return Err(VolumeScanError::Cancelled);
    }

    Ok(index)
}

/// 并行原子扫描本地外置高速卷（如 Thunderbolt / USB-C 外置 SSD）并流式写入 `CompactIndex`。
///
/// 采用 DFS 工作队列（上限 8 线程）+ 目录级 O(1) 过滤 + `getattrlistbulk(2)` 批量属性读取；
/// 任何工作线程检测到取消或卷卸载断开错误时立即中止全队列并释放句柄，避免卸载残留。
pub fn scan_volume_compact_atomic_parallel(
    root: &Path,
    filter: &crate::filter::CompiledFilter,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
    is_mounted: &(dyn Fn() -> bool + Sync),
    on_progress: &mut dyn FnMut(u64),
) -> Result<crate::compact_index::CompactIndex, VolumeScanError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(VolumeScanError::Cancelled);
    }

    if !is_mounted() {
        return Err(VolumeScanError::Disconnected(format!(
            "volume at {:?} is not mounted",
            root
        )));
    }

    let root_meta = match std::fs::metadata(root) {
        Ok(m) => m,
        Err(err) => {
            if is_disconnect_error(&err, is_mounted) {
                return Err(VolumeScanError::Disconnected(err.to_string()));
            } else {
                return Err(VolumeScanError::RootUnavailable(err.to_string()));
            }
        }
    };

    if !root_meta.is_dir() {
        return Err(VolumeScanError::RootUnavailable(format!(
            "{:?} is not a directory",
            root
        )));
    }

    let mut index = crate::compact_index::CompactIndex::new();
    let mut hints = crate::compact_index::InternHints::default();
    let mut ext_buf = String::with_capacity(32);

    let root_str = root.to_string_lossy();
    let root_name = root
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_else(|| root_str.clone());
    let root_modified = system_time_to_unix_secs(root_meta.modified());
    let root_created = system_time_to_unix_secs(root_meta.created());
    index
        .push_borrowed_with_hints(
            &root_name,
            &root_str,
            true,
            0,
            root_modified,
            root_created,
            "",
            &mut hints,
        )
        .map_err(|e| VolumeScanError::IndexLimit(e.to_string()))?;

    let mut progress_count = 1u64;
    on_progress(progress_count);

    let worker_count = std::thread::available_parallelism()
        .map(|n| n.get().clamp(2, 8))
        .unwrap_or(4);
    let queue = PrimaryWorkQueue::new(vec![root.to_path_buf()]);
    let local_abort = std::sync::atomic::AtomicBool::new(false);
    let first_error: parking_lot::Mutex<Option<VolumeScanError>> = parking_lot::Mutex::new(None);
    let (tx, rx) = std::sync::mpsc::sync_channel::<PrimaryDirBatch>(64);
    let include_hidden = filter.include_hidden();

    let queue_ref = &queue;
    let cancel_ref = cancel.as_ref();
    let local_abort_ref = &local_abort;
    let first_error_ref = &first_error;

    struct VolumeAbortGuard<'a> {
        local_abort: &'a std::sync::atomic::AtomicBool,
        queue: &'a PrimaryWorkQueue,
    }
    impl Drop for VolumeAbortGuard<'_> {
        fn drop(&mut self) {
            self.local_abort.store(true, Ordering::Relaxed);
            self.queue.abort();
        }
    }

    let sink_result: Result<(), VolumeScanError> = std::thread::scope(|scope| {
        let _abort_on_exit = VolumeAbortGuard {
            local_abort: local_abort_ref,
            queue: queue_ref,
        };

        for _ in 0..worker_count {
            let worker_tx = tx.clone();
            scope.spawn(move || {
                #[cfg(target_os = "macos")]
                let mut bulk_buf = vec![0u64; 8192];
                let mut path_buf = String::with_capacity(512);

                while let Some(dir) = queue_ref.pop_blocking() {
                    let mut dir_guard = ActiveDirGuard::new(queue_ref);
                    if cancel_ref.load(Ordering::Relaxed) || local_abort_ref.load(Ordering::Relaxed)
                    {
                        dir_guard.finish();
                        break;
                    }

                    let dir_str = dir.to_string_lossy();
                    path_buf.clear();
                    if dir_str == "/" {
                        path_buf.push('/');
                    } else {
                        path_buf.push_str(dir_str.trim_end_matches('/'));
                        path_buf.push('/');
                    }
                    let prefix_len = path_buf.len();
                    let is_root = dir.as_path() == root;
                    let dir_ctx = filter.dir_child_context(&dir, include_hidden);

                    let mut names = String::new();
                    let mut items = Vec::new();
                    let subdirs = &mut dir_guard.subdirs;

                    let mut handle_entry = |raw_name: &std::ffi::OsStr,
                                            name: &str,
                                            is_dir: bool,
                                            size: u64,
                                            modified: u64,
                                            created_at: u64|
                     -> Result<(), VolumeScanError> {
                        if local_abort_ref.load(Ordering::Relaxed) {
                            return Err(VolumeScanError::Cancelled);
                        }
                        path_buf.truncate(prefix_len);
                        path_buf.push_str(name);
                        let decision =
                            filter.evaluate_child(&dir_ctx, Path::new(&path_buf), name, is_dir);
                        match decision {
                            crate::filter::ScopeDecision::HardDrop => return Ok(()),
                            crate::filter::ScopeDecision::IndexShallowContainer => {}
                            crate::filter::ScopeDecision::IndexNormally => {
                                if is_dir {
                                    subdirs.push(dir.join(raw_name));
                                }
                            }
                        }

                        let name_start = names.len() as u32;
                        names.push_str(name);
                        items.push(PrimaryScannedItem {
                            name_start,
                            name_len: name.len() as u32,
                            is_dir,
                            size,
                            modified,
                            created_at,
                        });
                        Ok(())
                    };

                    let enum_res: Result<(), VolumeScanError> = {
                        #[cfg(target_os = "macos")]
                        {
                            match macos_bulk::enumerate_dir_bulk(
                                &dir,
                                is_root,
                                true,
                                &mut bulk_buf,
                                cancel_ref,
                                is_mounted,
                                &mut handle_entry,
                            ) {
                                Ok(macos_bulk::BulkOutcome::Done) => Ok(()),
                                Ok(macos_bulk::BulkOutcome::Unsupported) => enumerate_dir_fallback(
                                    &dir,
                                    is_root,
                                    true,
                                    cancel_ref,
                                    is_mounted,
                                    &mut handle_entry,
                                ),
                                Err(err) => Err(err),
                            }
                        }
                        #[cfg(not(target_os = "macos"))]
                        {
                            enumerate_dir_fallback(
                                &dir,
                                is_root,
                                true,
                                cancel_ref,
                                is_mounted,
                                &mut handle_entry,
                            )
                        }
                    };

                    if let Err(err) = enum_res {
                        let mut slot = first_error_ref.lock();
                        if slot.is_none() {
                            *slot = Some(err);
                        }
                        drop(slot);
                        local_abort_ref.store(true, Ordering::Relaxed);
                        queue_ref.abort();
                        dir_guard.subdirs.clear();
                        dir_guard.finish();
                        break;
                    }

                    dir_guard.finish();

                    if !items.is_empty() {
                        let parent = path_buf[..prefix_len].to_owned();
                        if worker_tx
                            .send(PrimaryDirBatch {
                                parent,
                                names,
                                items,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            });
        }
        drop(tx);

        let mut sink_path_buf = String::with_capacity(512);
        let mut res: Result<(), VolumeScanError> = Ok(());
        for batch in rx {
            if cancel_ref.load(Ordering::Relaxed) {
                res = Err(VolumeScanError::Cancelled);
                local_abort_ref.store(true, Ordering::Relaxed);
                queue_ref.abort();
                break;
            }
            sink_path_buf.clear();
            sink_path_buf.push_str(&batch.parent);
            let prefix_len = sink_path_buf.len();

            for item in &batch.items {
                let start = item.name_start as usize;
                let end = start + item.name_len as usize;
                let name = &batch.names[start..end];
                sink_path_buf.truncate(prefix_len);
                sink_path_buf.push_str(name);
                write_extension_lowercase(name, item.is_dir, &mut ext_buf);
                if let Err(err) = index.push_borrowed_with_hints(
                    name,
                    &sink_path_buf,
                    item.is_dir,
                    item.size,
                    item.modified,
                    item.created_at,
                    &ext_buf,
                    &mut hints,
                ) {
                    res = Err(VolumeScanError::IndexLimit(err.to_string()));
                    local_abort_ref.store(true, Ordering::Relaxed);
                    queue_ref.abort();
                    break;
                }
                progress_count += 1;
                on_progress(progress_count);
            }
            if res.is_err() {
                break;
            }
        }
        res
    });

    if cancel.load(Ordering::Relaxed) {
        return Err(VolumeScanError::Cancelled);
    }
    if let Some(err) = first_error.into_inner() {
        return Err(err);
    }
    sink_result?;

    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::Builder;

    fn make_progress() -> Arc<AtomicUsize> {
        Arc::new(AtomicUsize::new(0))
    }

    fn make_temp_dir() -> tempfile::TempDir {
        Builder::new().prefix("lokii_test_").tempdir().unwrap()
    }

    #[test]
    fn test_scan_basic() {
        let tmp = make_temp_dir();
        fs::write(tmp.path().join("file1.txt"), "hello").unwrap();
        fs::write(tmp.path().join("file2.rs"), "fn main() {}").unwrap();
        fs::write(tmp.path().join("file3.md"), "# README").unwrap();

        let progress = make_progress();
        let entries = scan_directories(
            &[tmp.path().to_path_buf()],
            dummy_filter(),
            true, // include hidden so we don't filter anything
            progress.clone(),
        );

        // Should have the root dir + 3 files = 4, or just 3 files
        // Scanner includes the root directory entry
        assert!(
            entries.len() >= 3,
            "Expected at least 3 entries, got {}",
            entries.len()
        );

        // Verify entries have non-empty names and paths
        for entry in &entries {
            assert!(!entry.name.is_empty());
            assert!(!entry.path.is_empty());
        }

        // Verify specific files exist
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"file1.txt"));
        assert!(names.contains(&"file2.rs"));
        assert!(names.contains(&"file3.md"));
    }

    #[test]
    fn test_scan_exclude_dir() {
        let tmp = make_temp_dir();
        fs::create_dir_all(tmp.path().join("node_modules/pkg")).unwrap();
        fs::write(
            tmp.path().join("node_modules/pkg/index.js"),
            "module.exports = {}",
        )
        .unwrap();
        fs::write(tmp.path().join("app.rs"), "fn main() {}").unwrap();

        let progress = make_progress();
        let entries = scan_directories(&[tmp.path().to_path_buf()], dummy_filter(), true, progress);

        // No entry should contain "node_modules" in its path
        for entry in &entries {
            assert!(
                !entry.path.contains("node_modules"),
                "Found excluded directory in results: {}",
                entry.path
            );
        }

        // app.rs should be present
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"app.rs"));
    }

    #[test]
    fn test_scan_package_dir_is_shallow_container() {
        let tmp = make_temp_dir();
        fs::create_dir_all(tmp.path().join("Demo.app/Resources")).unwrap();
        fs::write(tmp.path().join("Demo.app/Resources/tool"), "bin").unwrap();
        fs::write(tmp.path().join("Demo.app/Info.plist"), "plist").unwrap();
        fs::write(tmp.path().join("notes.txt"), "hello").unwrap();

        let scan = |shallow_root: bool, index_apps: bool| {
            let mut config = crate::config::AppConfig::default();
            config.index.index_paths = vec![tmp.path().to_string_lossy().into_owned()];
            config.index.index_apps = index_apps;
            let filter = if shallow_root {
                let snap = crate::scope_diff::PrimaryScopeSnapshot {
                    schema_version: crate::scope_diff::PRIMARY_SCOPE_SCHEMA_VERSION,
                    primary_roots: vec![tmp.path().to_string_lossy().into_owned()],
                    shallow_app_roots: vec![tmp.path().to_string_lossy().into_owned()],
                    excluded_mount_roots: vec![],
                    exclude_paths: vec![],
                    exclude: config.index.exclude.clone(),
                    include_hidden: true,
                    exclude_sensitive: true,
                    sensitive_names: config.index.sensitive_names.clone(),
                    has_fda: true,
                };
                crate::filter::CompiledFilter::from_primary_snapshot(&snap)
            } else {
                crate::filter::CompiledFilter::from_config(&config)
            };
            scan_directories(
                &[tmp.path().to_path_buf()],
                std::sync::Arc::new(filter),
                true,
                make_progress(),
            )
        };

        // 用户显式根（无论 index_apps 开关状态）：应用包按常规规则递归
        let names_off: Vec<String> = scan(false, false).iter().map(|e| e.name.clone()).collect();
        assert!(names_off.contains(&"Info.plist".to_string()));
        assert!(names_off.contains(&"tool".to_string()));

        let names_explicit_on: Vec<String> =
            scan(false, true).iter().map(|e| e.name.clone()).collect();
        assert!(names_explicit_on.contains(&"Info.plist".to_string()));
        assert!(names_explicit_on.contains(&"tool".to_string()));

        // 浅层应用根：包目录自身收录，内部整棵不扫
        let names_shallow: Vec<String> = scan(true, true).iter().map(|e| e.name.clone()).collect();
        assert!(
            names_shallow.contains(&"Demo.app".to_string()),
            "包目录自身应被收录"
        );
        assert!(
            !names_shallow.contains(&"Info.plist".to_string()),
            "包内文件不应被收录"
        );
        assert!(
            !names_shallow.contains(&"Resources".to_string()),
            "包内目录不应被收录"
        );
        assert!(names_shallow.contains(&"notes.txt".to_string()));
    }

    #[test]
    fn test_scan_hidden_files_excluded() {
        let tmp = make_temp_dir();
        fs::write(tmp.path().join(".hidden"), "secret").unwrap();
        fs::write(tmp.path().join("visible"), "public").unwrap();

        let progress = make_progress();
        let entries = scan_directories(
            &[tmp.path().to_path_buf()],
            dummy_filter(),
            false, // exclude hidden
            progress,
        );

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"visible"), "visible file should be present");
        assert!(
            !names.contains(&".hidden"),
            ".hidden file should be excluded"
        );
    }

    #[test]
    fn test_scan_hidden_files_included() {
        let tmp = make_temp_dir();
        fs::write(tmp.path().join(".hidden"), "secret").unwrap();
        fs::write(tmp.path().join("visible"), "public").unwrap();

        let progress = make_progress();
        let entries = scan_directories(
            &[tmp.path().to_path_buf()],
            dummy_filter(),
            true, // include hidden
            progress,
        );

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"visible"), "visible file should be present");
        assert!(
            names.contains(&".hidden"),
            ".hidden file should be included"
        );
    }

    #[test]
    fn test_scan_progress_counter() {
        let tmp = make_temp_dir();
        fs::write(tmp.path().join("a.txt"), "a").unwrap();
        fs::write(tmp.path().join("b.txt"), "b").unwrap();

        let progress = make_progress();
        let _entries = scan_directories(
            &[tmp.path().to_path_buf()],
            dummy_filter(),
            true,
            progress.clone(),
        );

        assert!(
            progress.load(Ordering::Relaxed) > 0,
            "Progress counter should be > 0"
        );
    }

    #[test]
    fn test_scan_nonexistent_path() {
        let progress = make_progress();
        let entries = scan_directories(
            &[PathBuf::from("/nonexistent/path/that/does/not/exist")],
            dummy_filter(),
            true,
            progress,
        );
        assert!(
            entries.is_empty(),
            "Scanning nonexistent path should return empty"
        );
    }

    #[test]
    fn test_scan_file_entry_correctness() {
        let tmp = make_temp_dir();
        let subdir = tmp.path().join("subdir");
        fs::create_dir(&subdir).unwrap();
        fs::write(subdir.join("test.txt"), "content").unwrap();

        let progress = make_progress();
        let entries = scan_directories(&[tmp.path().to_path_buf()], dummy_filter(), true, progress);

        // Find the test.txt entry
        let test_entry = entries.iter().find(|e| e.name == "test.txt");
        assert!(test_entry.is_some(), "test.txt should be in results");
        let test_entry = test_entry.unwrap();
        assert!(!test_entry.is_dir);
        assert_eq!(test_entry.size, 7); // "content" is 7 bytes
        assert!(test_entry.modified > 0);
        assert!(test_entry.path.ends_with("test.txt"));
        assert_eq!(test_entry.extension, "txt");

        // Find the subdir entry
        let dir_entry = entries.iter().find(|e| e.name == "subdir");
        assert!(dir_entry.is_some(), "subdir should be in results");
        assert!(dir_entry.unwrap().is_dir);
        assert_eq!(dir_entry.unwrap().extension, "");
    }

    #[test]
    fn test_scan_extension_extraction() {
        let tmp = make_temp_dir();
        fs::write(tmp.path().join("test.rs"), "fn main() {}").unwrap();
        fs::write(tmp.path().join("report.PDF"), "pdf content").unwrap();
        fs::write(tmp.path().join("Makefile"), "all:").unwrap();
        fs::create_dir(tmp.path().join("mydir")).unwrap();

        let progress = make_progress();
        let entries = scan_directories(&[tmp.path().to_path_buf()], dummy_filter(), true, progress);

        let rs_entry = entries.iter().find(|e| e.name == "test.rs").unwrap();
        assert_eq!(rs_entry.extension, "rs");

        let pdf_entry = entries.iter().find(|e| e.name == "report.PDF").unwrap();
        assert_eq!(pdf_entry.extension, "pdf"); // lowercased

        let makefile_entry = entries.iter().find(|e| e.name == "Makefile").unwrap();
        assert_eq!(makefile_entry.extension, ""); // no extension

        let dir_entry = entries.iter().find(|e| e.name == "mydir").unwrap();
        assert_eq!(dir_entry.extension, ""); // directories have no extension
    }

    #[test]
    fn test_deduplicate_scan_roots() {
        let tmp = make_temp_dir();
        let parent = tmp.path().join("parent");
        let child = parent.join("child");
        let blocked_parent = tmp.path().join("blocked_parent");
        let blocked_library = blocked_parent.join("Library");
        let pierced_sub = blocked_library.join("PiercedSub");

        fs::create_dir_all(&child).unwrap();
        fs::create_dir_all(&pierced_sub).unwrap();

        let mut config = crate::config::AppConfig::default();
        config.index.index_paths = vec![
            parent.to_string_lossy().to_string(),
            child.to_string_lossy().to_string(),
            blocked_parent.to_string_lossy().to_string(),
            pierced_sub.to_string_lossy().to_string(),
        ];
        config.index.exclude_paths = vec![blocked_library.to_string_lossy().to_string()];

        let filter = crate::filter::CompiledFilter::from_config(&config);

        let input_roots = vec![
            parent.clone(),
            child.clone(),
            blocked_parent.clone(),
            pierced_sub.clone(),
        ];
        let deduplicated = deduplicate_scan_roots(&input_roots, &filter, false);

        // child 完全被 parent 覆盖且中间无排除阻断，应被去重
        assert!(deduplicated.contains(&parent));
        assert!(!deduplicated.contains(&child));

        // pierced_sub 虽在 blocked_parent 下，但中间存在 blocked_library 排除剪枝，必须保留独立扫描
        assert!(deduplicated.contains(&blocked_parent));
        assert!(deduplicated.contains(&pierced_sub));
    }

    #[test]
    fn test_deduplicate_scan_roots_preserves_explicit_hidden_roots_when_hidden_excluded() {
        let tmp = make_temp_dir();
        let home = tmp.path().join("home");
        let dot_config = home.join(".config");
        let nvim = dot_config.join("nvim");

        fs::create_dir_all(&nvim).unwrap();

        let mut config = crate::config::AppConfig::default();
        config.index.index_paths = vec![
            home.to_string_lossy().to_string(),
            nvim.to_string_lossy().to_string(),
        ];

        let filter = crate::filter::CompiledFilter::from_config(&config);
        let input_roots = vec![home.clone(), nvim.clone()];

        // 当 include_hidden = false 时，上层 home 扫描会剪枝跳过 .config，
        // 故显式配置的 nvim 绝不可被去重丢弃，必须保留独立扫描
        let roots_no_hidden = deduplicate_scan_roots(&input_roots, &filter, false);
        assert!(roots_no_hidden.contains(&home));
        assert!(roots_no_hidden.contains(&nvim));

        // 验证子根自身直接为隐藏目录（如 home 与 home/.config）的边界情况
        let direct_hidden_roots = vec![home.clone(), dot_config.clone()];
        let direct_res = deduplicate_scan_roots(&direct_hidden_roots, &filter, false);
        assert!(direct_res.contains(&home));
        assert!(direct_res.contains(&dot_config));

        // 当 include_hidden = true 时，上层 home 扫描会自然遍历进入 .config/nvim，
        // 此时子根作为冗余项正常去重
        let roots_with_hidden = deduplicate_scan_roots(&input_roots, &filter, true);
        assert!(roots_with_hidden.contains(&home));
        assert!(!roots_with_hidden.contains(&nvim));
    }

    #[test]
    fn test_deduplicate_scan_roots_preserves_root_inside_shallow_container() {
        let tmp = make_temp_dir();
        let home = tmp.path().join("home");
        let nm = home.join("node_modules");
        let pkg = nm.join("my_pkg");
        fs::create_dir_all(&pkg).unwrap();

        let mut config = crate::config::AppConfig::default();
        config.index.index_paths = vec![
            home.to_string_lossy().to_string(),
            pkg.to_string_lossy().to_string(),
        ];
        config.index.include_hidden = true;
        let filter = crate::filter::CompiledFilter::from_config(&config);

        let roots = deduplicate_scan_roots(&[home.clone(), pkg.clone()], &filter, true);
        assert!(roots.contains(&home));
        assert!(
            roots.contains(&pkg),
            "explicit root inside shallow container must remain independent because parent stops at container"
        );
    }
    #[test]
    fn test_protected_paths_allows_system_applications() {
        let protected = protected_paths();
        assert!(
            !protected.contains(&PathBuf::from("/System/Applications")),
            "/System/Applications must not be blocked in protected_paths"
        );
    }

    #[test]
    fn test_is_disconnect_error_matrix() {
        use std::io::Error;

        let disconnect_codes = [
            libc::ENOTCONN,
            libc::ETIMEDOUT,
            libc::ESTALE,
            libc::EHOSTDOWN,
            libc::ENETDOWN,
            libc::EIO,
            libc::ENXIO,
        ];
        for code in disconnect_codes {
            let err = Error::from_raw_os_error(code);
            assert!(
                is_disconnect_error(&err, &|| true),
                "code {code} must be disconnect error"
            );
            assert!(
                is_disconnect_error(&err, &|| false),
                "code {code} must be disconnect error regardless of is_mounted"
            );
        }

        let noent = Error::from_raw_os_error(libc::ENOENT);
        assert!(
            is_disconnect_error(&noent, &|| false),
            "ENOENT when unmounted must be disconnect error"
        );
        assert!(
            !is_disconnect_error(&noent, &|| true),
            "ENOENT when mounted must NOT be disconnect error"
        );

        let local_errors = [libc::EACCES, libc::EPERM, libc::ELOOP];
        for code in local_errors {
            let err = Error::from_raw_os_error(code);
            assert!(
                !is_disconnect_error(&err, &|| true),
                "code {code} must NOT be disconnect error"
            );
            assert!(
                !is_disconnect_error(&err, &|| false),
                "code {code} must NOT be disconnect error"
            );
        }
    }

    #[test]
    fn test_scan_volume_compact_atomic_cancelled() {
        use std::sync::atomic::AtomicBool;

        let tmp = make_temp_dir();
        fs::write(tmp.path().join("file.txt"), "hello").unwrap();

        let config = crate::config::AppConfig::default();
        let filter = crate::filter::CompiledFilter::for_volume(&config, tmp.path());
        let cancel = Arc::new(AtomicBool::new(true));
        let mut progress_count = 0u64;

        let res = scan_volume_compact_atomic(
            tmp.path(),
            &filter,
            &cancel,
            &|| true,
            &mut |count| progress_count = count,
        );

        assert!(matches!(res, Err(VolumeScanError::Cancelled)));
    }

    #[test]
    fn test_scan_volume_compact_atomic_root_unavailable() {
        use std::sync::atomic::AtomicBool;

        let non_existent = PathBuf::from("/non_existent_volume_path_404");
        let config = crate::config::AppConfig::default();
        let filter = crate::filter::CompiledFilter::for_volume(&config, &non_existent);
        let cancel = Arc::new(AtomicBool::new(false));
        let mut progress_count = 0u64;

        let res_mounted = scan_volume_compact_atomic(
            &non_existent,
            &filter,
            &cancel,
            &|| true,
            &mut |count| progress_count = count,
        );
        assert!(matches!(res_mounted, Err(VolumeScanError::RootUnavailable(_))));

        let res_unmounted = scan_volume_compact_atomic(
            &non_existent,
            &filter,
            &cancel,
            &|| false,
            &mut |count| progress_count = count,
        );
        assert!(matches!(res_unmounted, Err(VolumeScanError::Disconnected(_))));
    }

    #[test]
    fn test_scan_volume_compact_atomic_success() {
        use std::sync::atomic::AtomicBool;

        let tmp = make_temp_dir();
        fs::write(tmp.path().join("file1.txt"), "hello").unwrap();
        fs::create_dir_all(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join("sub/file2.rs"), "fn main() {}").unwrap();
        fs::create_dir_all(tmp.path().join("node_modules")).unwrap();
        fs::write(tmp.path().join("node_modules/bad.js"), "console.log()").unwrap();

        let config = crate::config::AppConfig::default();
        let filter = crate::filter::CompiledFilter::for_volume(&config, tmp.path());
        let cancel = Arc::new(AtomicBool::new(false));
        let mut progress_count = 0u64;

        let res = scan_volume_compact_atomic(
            tmp.path(),
            &filter,
            &cancel,
            &|| true,
            &mut |count| progress_count = count,
        )
        .unwrap();

        let names: Vec<String> = res.iter().map(|(_, v)| v.name.to_string()).collect();
        assert!(names.iter().any(|n| n == "file1.txt"));
        assert!(names.iter().any(|n| n == "file2.rs"));
        assert!(!names.iter().any(|n| n == "bad.js"));
        assert!(progress_count >= 2);
    }

    #[test]
    fn test_scan_volume_compact_atomic_honors_scope_policy() {
        let tmp = make_temp_dir();
        let root = tmp.path().canonicalize().unwrap();
        for path in ["Demo.app", ".keys", ".visible", "excluded"] {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        for path in [
            "Demo.app/internal",
            ".keys/secret",
            ".visible/note",
            "excluded/file",
        ] {
            fs::write(root.join(path), "test").unwrap();
        }

        let mut config = crate::config::AppConfig::default();
        config.index.exclude_paths = vec![
            root.parent().unwrap().to_string_lossy().into_owned(),
            root.to_string_lossy().into_owned(),
            root.join("excluded").to_string_lossy().into_owned(),
        ];
        config.index.index_apps = true;
        config.index.include_hidden = true;
        config.index.exclude_sensitive = true;
        config.index.sensitive_names = vec![".keys".into()];
        let scan_names = |config: &crate::config::AppConfig| {
            let filter = crate::filter::CompiledFilter::for_volume(config, &root);
            scan_volume_compact_atomic(
                &root,
                &filter,
                &Arc::new(false.into()),
                &|| true,
                &mut |_| {},
            )
            .unwrap()
            .iter()
            .map(|(_, view)| view.name.to_string())
            .collect::<std::collections::HashSet<_>>()
        };

        let names = scan_names(&config);
        // 外置卷与 index_apps 彻底解耦：Demo.app 内非 Contents 路径正常收录
        for expected in ["Demo.app", "internal", ".visible", "note"] {
            assert!(names.contains(expected));
        }
        for dropped in [".keys", "secret", "excluded", "file"] {
            assert!(!names.contains(dropped));
        }

        config.index.index_apps = false;
        config.index.include_hidden = false;
        let names = scan_names(&config);
        assert!(names.contains("Demo.app"));
        assert!(names.contains("internal"));
        assert!(!names.contains(".visible"));
    }

    #[test]
    fn test_scan_volume_compact_atomic_aborted_drains_cleanly_without_leaks() {
        use std::sync::atomic::AtomicBool;

        let tmp = make_temp_dir();
        for i in 0..10 {
            let dir = tmp.path().join(format!("dir_{i}"));
            fs::create_dir_all(&dir).unwrap();
            for j in 0..10 {
                fs::write(dir.join(format!("file_{j}.txt")), "data").unwrap();
            }
        }

        let config = crate::config::AppConfig::default();
        let filter = crate::filter::CompiledFilter::for_volume(&config, tmp.path());
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_trigger = Arc::clone(&cancel);

        let res = scan_volume_compact_atomic(
            tmp.path(),
            &filter,
            &cancel,
            &|| true,
            &mut |count| {
                if count >= 3 {
                    cancel_trigger.store(true, Ordering::Relaxed);
                }
            },
        );

        assert!(matches!(res, Err(VolumeScanError::Cancelled)));

        // After scan_volume_compact_atomic returns, all descriptors must be dropped immediately,
        // allowing clean removal of the directory tree without any EBUSY.
        fs::remove_dir_all(tmp.path()).unwrap();
    }

    #[test]
    fn test_scan_volume_compact_atomic_collects_expected_entries_and_handles_cancel() {
        use std::sync::atomic::AtomicBool;

        let tmp = make_temp_dir();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("sub/nested")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(root.join("Demo.app/Contents/MacOS")).unwrap();
        fs::create_dir_all(root.join(".ssh")).unwrap();
        fs::write(root.join("alpha.TXT"), "hello world").unwrap();
        fs::write(root.join("sub/nested/文档.md"), "测试内容").unwrap();
        fs::write(root.join("node_modules/pkg/drop.js"), "ignored").unwrap();
        fs::write(root.join("Demo.app/Contents/MacOS/Demo"), "bin").unwrap();
        fs::write(root.join(".visible"), "dotfile").unwrap();
        fs::write(root.join(".ssh/id_ed25519"), "secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("sub"), root.join("sub_link")).unwrap();

        let mut config = crate::config::AppConfig::default();
        config.index.include_hidden = true;
        config.index.exclude_sensitive = true;
        config.index.index_apps = true;
        let filter = crate::filter::CompiledFilter::for_volume(&config, &root);

        let cancel = Arc::new(AtomicBool::new(false));
        let mut compact_progress = 0u64;
        let compact = scan_volume_compact_atomic(
            &root,
            &filter,
            &cancel,
            &|| true,
            &mut |c| compact_progress = c,
        )
        .unwrap();

        assert_eq!(compact_progress, compact.len() as u64);

        let mut compact_map: std::collections::HashMap<String, (bool, String, u64)> =
            std::collections::HashMap::new();
        let mut path_buf = String::new();
        for (_, view) in compact.iter() {
            view.path_into(&mut path_buf);
            compact_map.insert(
                path_buf.clone(),
                (view.is_dir, view.extension.to_string(), if view.is_dir { 0 } else { view.size }),
            );
        }

        let mut expected: std::collections::HashMap<String, (bool, String, u64)> =
            std::collections::HashMap::from([
                (root.to_string_lossy().into_owned(), (true, String::new(), 0)),
                (root.join("sub").to_string_lossy().into_owned(), (true, String::new(), 0)),
                (root.join("sub/nested").to_string_lossy().into_owned(), (true, String::new(), 0)),
                (
                    root.join("sub/nested/文档.md").to_string_lossy().into_owned(),
                    (false, "md".into(), "测试内容".len() as u64),
                ),
                (
                    root.join("alpha.TXT").to_string_lossy().into_owned(),
                    (false, "txt".into(), "hello world".len() as u64),
                ),
                (
                    root.join("Demo.app").to_string_lossy().into_owned(),
                    (true, String::new(), 0),
                ),
                (
                    root.join(".visible").to_string_lossy().into_owned(),
                    (false, String::new(), "dotfile".len() as u64),
                ),
            ]);
        #[cfg(unix)]
        {
            let link_meta = fs::symlink_metadata(root.join("sub_link")).unwrap();
            expected.insert(
                root.join("sub_link").to_string_lossy().into_owned(),
                (false, String::new(), link_meta.len()),
            );
        }
        assert_eq!(compact_map, expected);

        // 取消时应立刻中止并释放全部目录描述符
        let cancel_mid = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancel_mid);
        let cancelled = scan_volume_compact_atomic(
            &root,
            &filter,
            &cancel_mid,
            &|| true,
            &mut |count| {
                if count >= 2 {
                    trigger.store(true, Ordering::Relaxed);
                }
            },
        );
        assert!(matches!(cancelled, Err(VolumeScanError::Cancelled)));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn test_scan_volume_compact_atomic_parallel_matches_serial_and_handles_cancel() {
        let root = std::env::temp_dir().join(format!(
            "lokii_vol_parallel_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("a/b/c")).unwrap();
        fs::create_dir_all(root.join("d/e")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(root.join("App.app/Contents/MacOS")).unwrap();
        fs::create_dir_all(root.join(".ssh")).unwrap();
        fs::write(root.join("root.txt"), "root").unwrap();
        fs::write(root.join("a/b/c/deep.rs"), "fn main() {}").unwrap();
        fs::write(root.join("d/e/中文报告.md"), "内容").unwrap();
        fs::write(root.join("node_modules/pkg/drop.js"), "ignored").unwrap();
        fs::write(root.join("App.app/Contents/MacOS/App"), "bin").unwrap();
        fs::write(root.join(".ssh/id_rsa"), "secret").unwrap();

        let mut config = crate::config::AppConfig::default();
        config.index.include_hidden = true;
        config.index.exclude_sensitive = true;
        config.index.index_apps = true;
        let filter = crate::filter::CompiledFilter::for_volume(&config, &root);

        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut serial_progress = 0u64;
        let serial = scan_volume_compact_atomic(
            &root,
            &filter,
            &cancel,
            &|| true,
            &mut |c| serial_progress = c,
        )
        .unwrap();

        let mut parallel_progress = 0u64;
        let parallel = scan_volume_compact_atomic_parallel(
            &root,
            &filter,
            &cancel,
            &|| true,
            &mut |c| parallel_progress = c,
        )
        .unwrap();

        assert_eq!(parallel.len(), serial.len());
        assert_eq!(parallel_progress, parallel.len() as u64);
        assert_eq!(serial_progress, serial.len() as u64);

        let collect_map = |idx: &crate::compact_index::CompactIndex| {
            let mut map = std::collections::HashMap::new();
            let mut path_buf = String::new();
            for (_, view) in idx.iter() {
                view.path_into(&mut path_buf);
                map.insert(
                    path_buf.clone(),
                    (
                        view.is_dir,
                        view.extension.to_string(),
                        if view.is_dir { 0 } else { view.size },
                    ),
                );
            }
            map
        };
        assert_eq!(collect_map(&parallel), collect_map(&serial));

        // 预取消与未挂载均应快速返回对应错误
        let cancelled = scan_volume_compact_atomic_parallel(
            &root,
            &filter,
            &Arc::new(std::sync::atomic::AtomicBool::new(true)),
            &|| true,
            &mut |_| {},
        );
        assert!(matches!(cancelled, Err(VolumeScanError::Cancelled)));

        let disconnected = scan_volume_compact_atomic_parallel(
            &root,
            &filter,
            &cancel,
            &|| false,
            &mut |_| {},
        );
        assert!(matches!(disconnected, Err(VolumeScanError::Disconnected(_))));

        fs::remove_dir_all(&root).unwrap();
    }
}
