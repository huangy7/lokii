use crate::config::{expand_tilde, AppConfig};
use crate::filter::{CompiledFilter, ScopeDecision};
use crate::path_utils::{path_starts_with_ci, paths_equal_ci};
use crate::scanner;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// 主索引结构化范围快照协议版本。
pub const PRIMARY_SCOPE_SCHEMA_VERSION: u32 = 1;
/// 外置卷/网络卷结构化范围快照协议版本。
pub const VOLUME_SCOPE_SCHEMA_VERSION: u32 = 1;

/// 外置卷/网络卷进入待确认状态的原因，按严格优先级排序：
/// `InitialScan > SchemaUpgrade > JournalInvalidated > FilterWidened`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolumeConfirmReason {
    InitialScan,
    FilterWidened,
    JournalInvalidated,
    SchemaUpgrade,
}

impl VolumeConfirmReason {
    #[inline]
    pub const fn priority(self) -> u8 {
        match self {
            VolumeConfirmReason::InitialScan => 4,
            VolumeConfirmReason::SchemaUpgrade => 3,
            VolumeConfirmReason::JournalInvalidated => 2,
            VolumeConfirmReason::FilterWidened => 1,
        }
    }

    /// 合并已有待确认原因与新触发原因，始终保留更高优先级原因，绝不降级覆盖。
    #[inline]
    pub const fn merge(current: Option<Self>, incoming: Self) -> Self {
        match current {
            Some(existing) if existing.priority() >= incoming.priority() => existing,
            _ => incoming,
        }
    }

    /// 仅当有效原因为 `FilterWidened` 时，才允许在待确认期间挂实时流并推进检查点。
    #[inline]
    pub const fn allows_live_stream(self) -> bool {
        matches!(self, VolumeConfirmReason::FilterWidened)
    }
}

/// 纯词法路径规范化：展开 `~`、清理 `.` 与冗余尾随 `/`（根 `/` 除外），绝不触碰磁盘。
pub fn normalize_lexical_path(raw: &str) -> String {
    let expanded = expand_tilde(raw.trim());
    normalize_lexical_pathbuf(&expanded)
}

pub fn normalize_lexical_pathbuf(path: &Path) -> String {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::RootDir => out.push(comp.as_os_str()),
            Component::ParentDir => out.push(comp.as_os_str()),
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::Normal(seg) => out.push(seg),
        }
    }
    let s = out.to_string_lossy();
    if s.is_empty() {
        String::new()
    } else if s == "/" {
        "/".to_string()
    } else {
        s.trim_end_matches('/').to_string()
    }
}

#[inline]
fn path_sort_key(path: &str) -> String {
    #[cfg(target_os = "macos")]
    {
        path.to_lowercase()
    }
    #[cfg(not(target_os = "macos"))]
    {
        path.to_string()
    }
}

fn dedup_and_sort_paths(paths: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut normalized: Vec<String> = paths
        .into_iter()
        .map(|p| normalize_lexical_path(&p))
        .filter(|p| !p.is_empty())
        .collect();
    normalized.sort_by(|a, b| path_sort_key(a).cmp(&path_sort_key(b)).then_with(|| a.cmp(b)));
    let mut out: Vec<String> = Vec::with_capacity(normalized.len());
    for p in normalized {
        if out
            .last()
            .is_none_or(|prev| path_sort_key(prev) != path_sort_key(&p))
        {
            out.push(p);
        }
    }
    out
}

fn dedup_and_sort_exact(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = items.into_iter().collect();
    out.sort();
    out.dedup();
    out
}

fn dedup_and_sort_lowercase(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = items.into_iter().map(|s| s.to_lowercase()).collect();
    out.sort();
    out.dedup();
    out
}

fn dedup_and_sort_subpaths(subpaths: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut items: Vec<String> = subpaths
        .into_iter()
        .map(|s| s.trim_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    items.sort_by(|a, b| path_sort_key(a).cmp(&path_sort_key(b)).then_with(|| a.cmp(b)));
    let mut out: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        if out
            .last()
            .is_none_or(|prev| path_sort_key(prev) != path_sort_key(&item))
        {
            out.push(item);
        }
    }
    out
}

fn path_list_contains(list: &[String], target: &Path) -> bool {
    list.iter().any(|item| paths_equal_ci(Path::new(item), target))
}

fn path_sets_equal(a: &[String], b: &[String]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .all(|item| path_list_contains(b, Path::new(item)))
}

fn sensitive_sets_equal(a: &[String], b: &[String]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .all(|item| b.iter().any(|other| other.eq_ignore_ascii_case(item)))
}

fn deepest_matching_root<'a>(roots: &'a [String], path: &Path) -> Option<&'a str> {
    roots
        .iter()
        .filter(|root| path_starts_with_ci(path, Path::new(root)))
        .max_by_key(|root| Path::new(root).components().count())
        .map(String::as_str)
}

#[inline]
fn fnv1a_feed_u64(hash: &mut u64, value: u64) {
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    for &b in &value.to_le_bytes() {
        *hash ^= b as u64;
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

#[inline]
fn fnv1a_feed_bytes(hash: &mut u64, bytes: &[u8]) {
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    fnv1a_feed_u64(hash, bytes.len() as u64);
    for &b in bytes {
        *hash ^= b as u64;
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn fnv1a_feed_path_list(hash: &mut u64, paths: &[String]) {
    fnv1a_feed_u64(hash, paths.len() as u64);
    for p in paths {
        let key = path_sort_key(p);
        fnv1a_feed_bytes(hash, key.as_bytes());
    }
}

fn fnv1a_feed_str_list(hash: &mut u64, items: &[String]) {
    fnv1a_feed_u64(hash, items.len() as u64);
    for item in items {
        fnv1a_feed_bytes(hash, item.as_bytes());
    }
}

/// 主索引结构化范围快照：与 `Checkpoint` 一起原子内嵌进 v6 `index.cache` 头部。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrimaryScopeSnapshot {
    pub schema_version: u32,
    pub primary_roots: Vec<String>,
    pub shallow_app_roots: Vec<String>,
    pub excluded_mount_roots: Vec<String>,
    pub exclude_paths: Vec<String>,
    pub exclude: Vec<String>,
    pub include_hidden: bool,
    pub exclude_sensitive: bool,
    pub sensitive_names: Vec<String>,
    pub has_fda: bool,
}

impl PrimaryScopeSnapshot {
    /// 根据当前配置、本地主根列表、挂载排除根与 FDA 状态构造规范化快照。
    pub fn from_inputs(
        config: &AppConfig,
        primary_roots: &[PathBuf],
        excluded_mount_roots: &[PathBuf],
        has_fda: bool,
    ) -> Self {
        let normalized_primary = dedup_and_sort_paths(
            primary_roots
                .iter()
                .map(|p| normalize_lexical_pathbuf(p)),
        );
        let shallow_candidates = config.resolve_shallow_app_roots();
        let shallow_app_roots = dedup_and_sort_paths(
            normalized_primary
                .iter()
                .filter(|root| {
                    shallow_candidates
                        .iter()
                        .any(|candidate| paths_equal_ci(Path::new(root), candidate))
                })
                .cloned(),
        );
        let normalized_mounts = dedup_and_sort_paths(
            excluded_mount_roots
                .iter()
                .map(|p| normalize_lexical_pathbuf(p)),
        );
        let normalized_excludes = dedup_and_sort_paths(
            config
                .resolve_exclude_paths()
                .into_iter()
                .map(|p| normalize_lexical_pathbuf(&p)),
        );

        Self {
            schema_version: PRIMARY_SCOPE_SCHEMA_VERSION,
            primary_roots: normalized_primary,
            shallow_app_roots,
            excluded_mount_roots: normalized_mounts,
            exclude_paths: normalized_excludes,
            exclude: dedup_and_sort_exact(config.index.exclude.iter().cloned()),
            include_hidden: config.index.include_hidden,
            exclude_sensitive: config.index.exclude_sensitive,
            sensitive_names: dedup_and_sort_lowercase(config.index.sensitive_names.iter().cloned()),
            has_fda,
        }
    }

    /// 跨工具链确定的 64 位 FNV-1a 范围指纹，永不返回 0（0 保留给无检查点哨兵）。
    pub fn fingerprint_fnv1a(&self) -> u64 {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        let mut hash = FNV_OFFSET;
        fnv1a_feed_u64(&mut hash, self.schema_version as u64);
        fnv1a_feed_path_list(&mut hash, &self.primary_roots);
        fnv1a_feed_path_list(&mut hash, &self.shallow_app_roots);
        fnv1a_feed_path_list(&mut hash, &self.excluded_mount_roots);
        fnv1a_feed_path_list(&mut hash, &self.exclude_paths);
        fnv1a_feed_str_list(&mut hash, &self.exclude);
        let lower_sensitive = dedup_and_sort_lowercase(self.sensitive_names.iter().cloned());
        fnv1a_feed_str_list(&mut hash, &lower_sensitive);
        let flags = u64::from(self.include_hidden)
            | (u64::from(self.exclude_sensitive) << 1)
            | (u64::from(self.has_fda) << 2);
        fnv1a_feed_u64(&mut hash, flags);
        if hash == 0 { 1 } else { hash }
    }
}

/// 提取严格落在该外置卷根目录之内的相对排除子路径（同时匹配配置根与 `-1` 漂移后的活跃根，
/// 忽略卷根自身与卷外路径），供 `VolumeScopeSnapshot` 与运行期过滤器共用，保证 100% 同源。
pub fn extract_volume_exclude_subpaths(
    config: &AppConfig,
    configured_path: &Path,
    active_configured_path: &Path,
) -> Vec<String> {
    let norm_configured = PathBuf::from(normalize_lexical_pathbuf(configured_path));
    let norm_active = PathBuf::from(normalize_lexical_pathbuf(active_configured_path));
    let mut subpaths = Vec::new();

    for raw_ex in config.resolve_exclude_paths() {
        let ex = PathBuf::from(normalize_lexical_pathbuf(&raw_ex));
        if ex.as_os_str().is_empty() {
            continue;
        }
        for anchor in [&norm_configured, &norm_active] {
            if anchor.as_os_str().is_empty() {
                continue;
            }
            if path_starts_with_ci(&ex, anchor) && !paths_equal_ci(&ex, anchor) {
                let anchor_depth = anchor.components().count();
                let rel_parts: Vec<String> = ex
                    .components()
                    .skip(anchor_depth)
                    .filter_map(|c| match c {
                        Component::Normal(seg) => Some(seg.to_string_lossy().into_owned()),
                        _ => None,
                    })
                    .collect();
                if !rel_parts.is_empty() {
                    subpaths.push(rel_parts.join("/"));
                }
            }
        }
    }

    dedup_and_sort_subpaths(subpaths)
}

/// 外置卷/网络卷独立结构化范围快照：与本地 `primary_roots`、`has_fda`、`index_apps` 彻底隔离。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeScopeSnapshot {
    pub schema_version: u32,
    pub exclude_subpaths: Vec<String>,
    pub exclude: Vec<String>,
    pub include_hidden: bool,
    pub exclude_sensitive: bool,
    pub sensitive_names: Vec<String>,
}

impl VolumeScopeSnapshot {
    pub fn from_config(
        config: &AppConfig,
        configured_path: &Path,
        active_configured_path: &Path,
    ) -> Self {
        Self {
            schema_version: VOLUME_SCOPE_SCHEMA_VERSION,
            exclude_subpaths: extract_volume_exclude_subpaths(
                config,
                configured_path,
                active_configured_path,
            ),
            exclude: dedup_and_sort_exact(config.index.exclude.iter().cloned()),
            include_hidden: config.index.include_hidden,
            exclude_sensitive: config.index.exclude_sensitive,
            sensitive_names: dedup_and_sort_lowercase(config.index.sensitive_names.iter().cloned()),
        }
    }

    pub fn fingerprint_fnv1a(&self) -> u64 {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        let mut hash = FNV_OFFSET;
        fnv1a_feed_u64(&mut hash, self.schema_version as u64);
        fnv1a_feed_path_list(&mut hash, &self.exclude_subpaths);
        fnv1a_feed_str_list(&mut hash, &self.exclude);
        let lower_sensitive = dedup_and_sort_lowercase(self.sensitive_names.iter().cloned());
        fnv1a_feed_str_list(&mut hash, &lower_sensitive);
        let flags = u64::from(self.include_hidden) | (u64::from(self.exclude_sensitive) << 1);
        fnv1a_feed_u64(&mut hash, flags);
        if hash == 0 { 1 } else { hash }
    }

    pub fn semantically_eq(&self, other: &Self) -> bool {
        self.schema_version == other.schema_version
            && self.include_hidden == other.include_hidden
            && self.exclude_sensitive == other.exclude_sensitive
            && self.exclude == other.exclude
            && sensitive_sets_equal(&self.sensitive_names, &other.sensitive_names)
            && path_sets_equal(&self.exclude_subpaths, &other.exclude_subpaths)
    }
}

/// 外置卷范围变更动作决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeScopeAction {
    NoOp,
    PruneOnly,
    FullRebuild { reason: VolumeConfirmReason },
}

/// 主索引范围变更动作决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrimaryScopeAction {
    NoOp,
    PruneOnly,
    IncrementalUpdate {
        prune_first: bool,
        scan_roots: Vec<PathBuf>,
    },
    FullRebuild,
}

/// 外置卷安全三分法 Diff：优先检查协议升级与任意宽化，其余精确归类为 `NoOp` 或 `PruneOnly`。
pub fn diff_volume_scope(
    old: &VolumeScopeSnapshot,
    new: &VolumeScopeSnapshot,
    dirty_upper_bound: bool,
) -> VolumeScopeAction {
    if old.schema_version != new.schema_version {
        return VolumeScopeAction::FullRebuild {
            reason: VolumeConfirmReason::SchemaUpgrade,
        };
    }

    let hidden_widened = !old.include_hidden && new.include_hidden;
    let sensitive_widened = old.exclude_sensitive
        && (!new.exclude_sensitive
            || old.sensitive_names.iter().any(|s| {
                !new.sensitive_names
                    .iter()
                    .any(|n| n.eq_ignore_ascii_case(s))
            }));
    let exclude_name_removed = old.exclude.iter().any(|e| !new.exclude.contains(e));
    let exclude_subpath_removed = old.exclude_subpaths.iter().any(|p| {
        let old_p = Path::new(p);
        !new.exclude_subpaths
            .iter()
            .any(|q| path_starts_with_ci(old_p, Path::new(q)))
    });

    if hidden_widened || sensitive_widened || exclude_name_removed || exclude_subpath_removed {
        return VolumeScopeAction::FullRebuild {
            reason: VolumeConfirmReason::FilterWidened,
        };
    }

    if old.semantically_eq(new) {
        if dirty_upper_bound {
            VolumeScopeAction::PruneOnly
        } else {
            VolumeScopeAction::NoOp
        }
    } else {
        VolumeScopeAction::PruneOnly
    }
}

/// 判断从祖先根 `ancestor_root` 到目标路径 `target`（含两端）的每个目录节点，
/// 在指定词法过滤策略与受保护目录集合下是否全部允许正常递归遍历（无排除、隐藏、敏感或浅层截断）。
pub(crate) fn path_chain_admitted_normally(
    ancestor_root: &Path,
    target: &Path,
    filter: &CompiledFilter,
    include_hidden: bool,
    has_fda: bool,
    protected: &[PathBuf],
) -> bool {
    if !path_starts_with_ci(target, ancestor_root) {
        return false;
    }
    for node in target.ancestors() {
        if (!has_fda && scanner::is_protected_or_descendant(node, protected))
            || filter.evaluate_with_hidden(node, true, include_hidden)
                != ScopeDecision::IndexNormally
        {
            return false;
        }
        if paths_equal_ci(node, ancestor_root) {
            return true;
        }
    }
    false
}

/// 主索引三分法 Diff（纯词法预览版，不触碰磁盘 `exists()`）。
pub fn diff_primary_scope_lexical(
    old: &PrimaryScopeSnapshot,
    new: &PrimaryScopeSnapshot,
) -> PrimaryScopeAction {
    let protected = if old.has_fda && new.has_fda {
        Vec::new()
    } else {
        scanner::protected_paths()
    };
    diff_primary_scope_with(old, new, &protected, |_| true)
}

/// 主索引三分法 Diff（运行期版本，结合真实磁盘存在性判定移除挂载点与新获 FDA 受保护目录）。
pub fn diff_primary_scope_runtime(
    old: &PrimaryScopeSnapshot,
    new: &PrimaryScopeSnapshot,
) -> PrimaryScopeAction {
    let protected = if old.has_fda && new.has_fda {
        Vec::new()
    } else {
        scanner::protected_paths()
    };
    diff_primary_scope_with(old, new, &protected, |path| path.exists())
}

/// 主索引三分法 Diff 核心实现（支持注入 `protected` 与 `path_exists` 供测试与词法预览复用）。
pub fn diff_primary_scope_with(
    old: &PrimaryScopeSnapshot,
    new: &PrimaryScopeSnapshot,
    protected: &[PathBuf],
    path_exists: impl Fn(&Path) -> bool,
) -> PrimaryScopeAction {
    if old.schema_version != new.schema_version {
        return PrimaryScopeAction::FullRebuild;
    }

    // 步骤 2：校验全局过滤规则是否发生宽化（excluded_mount_roots 增减交由步骤 3 精准分流）
    let hidden_widened = !old.include_hidden && new.include_hidden;
    let sensitive_widened = old.exclude_sensitive
        && (!new.exclude_sensitive
            || old.sensitive_names.iter().any(|s| {
                !new.sensitive_names
                    .iter()
                    .any(|n| n.eq_ignore_ascii_case(s))
            }));
    let exclude_name_removed = old.exclude.iter().any(|e| !new.exclude.contains(e));
    let exclude_path_removed = old.exclude_paths.iter().any(|p| {
        let old_p = Path::new(p);
        let still_covered = new
            .exclude_paths
            .iter()
            .chain(new.excluded_mount_roots.iter())
            .any(|q| path_starts_with_ci(old_p, Path::new(q)));
        if still_covered {
            return false;
        }
        new.primary_roots.iter().any(|r| {
            let new_r = Path::new(r);
            path_starts_with_ci(old_p, new_r) || path_starts_with_ci(new_r, old_p)
        })
    });

    if hidden_widened || sensitive_widened || exclude_name_removed || exclude_path_removed {
        return PrimaryScopeAction::FullRebuild;
    }

    // 步骤 3：计算新增补扫目录 scan_roots 与兜底收窄标记 has_narrowing
    let old_filter = CompiledFilter::from_primary_snapshot(old);
    let new_filter = CompiledFilter::from_primary_snapshot(new);

    let mut scan_roots: Vec<PathBuf> = Vec::new();
    let push_unique_root = |roots: &mut Vec<PathBuf>, candidate: PathBuf| {
        if !roots.iter().any(|existing| paths_equal_ci(existing, &candidate)) {
            roots.push(candidate);
        }
    };

    // 3a. 严格覆盖判定（新增根 + 浅层应用根升格为普通根）
    for new_root_str in &new.primary_roots {
        let new_root = Path::new(new_root_str);
        if (!new.has_fda && scanner::is_protected_or_descendant(new_root, protected))
            || new_filter.evaluate_with_hidden(new_root, true, new.include_hidden)
                == ScopeDecision::HardDrop
        {
            continue;
        }

        let covered_by_old = match deepest_matching_root(&old.primary_roots, new_root) {
            Some(old_root_str) => {
                let old_root = Path::new(old_root_str);
                let chain_ok = path_chain_admitted_normally(
                    old_root,
                    new_root,
                    &old_filter,
                    old.include_hidden,
                    old.has_fda,
                    protected,
                );
                let old_was_shallow = path_list_contains(&old.shallow_app_roots, old_root);
                let new_is_shallow = path_list_contains(&new.shallow_app_roots, new_root);
                let upgraded_from_shallow = old_was_shallow && !new_is_shallow;
                chain_ok && !upgraded_from_shallow
            }
            None => false,
        };

        if !covered_by_old {
            push_unique_root(&mut scan_roots, new_root.to_path_buf());
        }
    }

    // 3b. 挂载排除根移除分流：嵌套卷从配置移除且落在主根下并存在时局部补扫；漂移盘卸载走 NoOp
    for old_mount_str in &old.excluded_mount_roots {
        let old_mount = Path::new(old_mount_str);
        if path_list_contains(&new.excluded_mount_roots, old_mount) {
            continue;
        }
        let under_new_primary = new
            .primary_roots
            .iter()
            .any(|r| path_starts_with_ci(old_mount, Path::new(r)));
        if under_new_primary
            && path_exists(old_mount)
            && (new.has_fda || !scanner::is_protected_or_descendant(old_mount, protected))
            && new_filter.evaluate_with_hidden(old_mount, true, new.include_hidden)
                != ScopeDecision::HardDrop
        {
            push_unique_root(&mut scan_roots, old_mount.to_path_buf());
        }
    }

    // 3c. 新获得 FDA：补扫先前受保护且在新范围内可读的目录
    if !old.has_fda && new.has_fda {
        for p in protected {
            let under_new_primary = new
                .primary_roots
                .iter()
                .any(|r| path_starts_with_ci(p, Path::new(r)));
            if under_new_primary
                && path_exists(p)
                && new_filter.evaluate_with_hidden(p, true, new.include_hidden)
                    != ScopeDecision::HardDrop
            {
                push_unique_root(&mut scan_roots, p.clone());
            }
        }
    }

    // 3d. 补集兜底判定 has_narrowing
    let root_removed_or_downgraded = old.primary_roots.iter().any(|old_root_str| {
        let old_root = Path::new(old_root_str);
        match deepest_matching_root(&new.primary_roots, old_root) {
            None => true,
            Some(new_root_str) => {
                let new_root = Path::new(new_root_str);
                let old_was_shallow = path_list_contains(&old.shallow_app_roots, old_root);
                let new_is_shallow = path_list_contains(&new.shallow_app_roots, new_root);
                if !old_was_shallow && new_is_shallow {
                    return true;
                }
                // 该根在新快照中仍原样保留，未发生删根；其余过滤或 FDA 收窄由 lost_fda / regular_filter_changed 兜底。
                if paths_equal_ci(new_root, old_root) {
                    return false;
                }
                !path_chain_admitted_normally(
                    new_root,
                    old_root,
                    &new_filter,
                    new.include_hidden,
                    new.has_fda,
                    protected,
                )
            }
        }
    });
    let mount_exclude_added = new
        .excluded_mount_roots
        .iter()
        .any(|m| !path_list_contains(&old.excluded_mount_roots, Path::new(m)));
    let lost_fda = old.has_fda && !new.has_fda;
    let regular_filter_changed = !path_sets_equal(&old.exclude_paths, &new.exclude_paths)
        || old.exclude != new.exclude
        || old.include_hidden != new.include_hidden
        || old.exclude_sensitive != new.exclude_sensitive
        || !sensitive_sets_equal(&old.sensitive_names, &new.sensitive_names);

    let has_narrowing =
        root_removed_or_downgraded || mount_exclude_added || lost_fda || regular_filter_changed;

    if !scan_roots.is_empty() {
        PrimaryScopeAction::IncrementalUpdate {
            prune_first: has_narrowing,
            scan_roots,
        }
    } else if has_narrowing {
        PrimaryScopeAction::PruneOnly
    } else {
        PrimaryScopeAction::NoOp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_primary_snapshot() -> PrimaryScopeSnapshot {
        PrimaryScopeSnapshot {
            schema_version: PRIMARY_SCOPE_SCHEMA_VERSION,
            primary_roots: vec!["/Users/dev".into()],
            shallow_app_roots: vec![],
            excluded_mount_roots: vec![],
            exclude_paths: vec!["/System".into(), "/Users/dev/Library".into()],
            exclude: vec!["Contents".into(), "node_modules".into()],
            include_hidden: false,
            exclude_sensitive: true,
            sensitive_names: vec![".aws".into(), ".ssh".into()],
            has_fda: false,
        }
    }

    fn base_volume_snapshot() -> VolumeScopeSnapshot {
        VolumeScopeSnapshot {
            schema_version: VOLUME_SCOPE_SCHEMA_VERSION,
            exclude_subpaths: vec!["Cache".into()],
            exclude: vec!["Contents".into(), "node_modules".into()],
            include_hidden: false,
            exclude_sensitive: true,
            sensitive_names: vec![".aws".into(), ".ssh".into()],
        }
    }

    #[test]
    fn confirm_reason_priority_never_downgrades() {
        use VolumeConfirmReason::*;
        assert_eq!(
            VolumeConfirmReason::merge(Some(FilterWidened), JournalInvalidated),
            JournalInvalidated
        );
        assert_eq!(
            VolumeConfirmReason::merge(Some(JournalInvalidated), FilterWidened),
            JournalInvalidated
        );
        assert_eq!(
            VolumeConfirmReason::merge(Some(JournalInvalidated), SchemaUpgrade),
            SchemaUpgrade
        );
        assert_eq!(
            VolumeConfirmReason::merge(Some(SchemaUpgrade), InitialScan),
            InitialScan
        );
        assert!(FilterWidened.allows_live_stream());
        assert!(!JournalInvalidated.allows_live_stream());
        assert!(!SchemaUpgrade.allows_live_stream());
        assert!(!InitialScan.allows_live_stream());
    }

    #[test]
    fn extract_volume_exclude_subpaths_handles_drift_and_ignores_outside_paths() {
        let mut cfg = AppConfig::default();
        cfg.index.exclude_paths = vec![
            "/Volumes".into(),
            "/Volumes/Disk".into(),
            "/Volumes/Disk/Cache/Tmp".into(),
            "/Volumes/Disk-1/Logs".into(),
            "/Users/dev/Downloads".into(),
            "/Volumes/OtherDisk/Secret".into(),
        ];

        let subpaths = extract_volume_exclude_subpaths(
            &cfg,
            Path::new("/Volumes/Disk"),
            Path::new("/Volumes/Disk-1"),
        );
        assert_eq!(subpaths, vec!["Cache/Tmp".to_string(), "Logs".to_string()]);
    }

    #[test]
    fn volume_diff_classifies_widening_narrowing_aba_and_noop() {
        let old = base_volume_snapshot();
        assert_eq!(diff_volume_scope(&old, &old, false), VolumeScopeAction::NoOp);
        assert_eq!(
            diff_volume_scope(&old, &old, true),
            VolumeScopeAction::PruneOnly
        );

        let mut schema_bumped = old.clone();
        schema_bumped.schema_version += 1;
        assert_eq!(
            diff_volume_scope(&old, &schema_bumped, false),
            VolumeScopeAction::FullRebuild {
                reason: VolumeConfirmReason::SchemaUpgrade
            }
        );

        // 新增本卷排除子路径 -> PruneOnly
        let mut narrowed = old.clone();
        narrowed.exclude_subpaths = vec!["Cache".into(), "Temp".into()];
        assert_eq!(
            diff_volume_scope(&old, &narrowed, false),
            VolumeScopeAction::PruneOnly
        );

        // 用更浅祖先替换原排除子路径仍属收窄（未宽化）
        let mut deeper_old = old.clone();
        deeper_old.exclude_subpaths = vec!["Cache/Sub".into()];
        assert_eq!(
            diff_volume_scope(&deeper_old, &old, false),
            VolumeScopeAction::PruneOnly
        );

        // 移除排除子路径 / 排除名 / 开启隐藏 / 关闭敏感保护均属宽化
        let mut widened = old.clone();
        widened.exclude_subpaths.clear();
        assert_eq!(
            diff_volume_scope(&old, &widened, false),
            VolumeScopeAction::FullRebuild {
                reason: VolumeConfirmReason::FilterWidened
            }
        );

        let mut hidden_on = old.clone();
        hidden_on.include_hidden = true;
        assert_eq!(
            diff_volume_scope(&old, &hidden_on, false),
            VolumeScopeAction::FullRebuild {
                reason: VolumeConfirmReason::FilterWidened
            }
        );
    }

    #[test]
    fn primary_diff_handles_roots_shallow_transitions_mounts_and_fda() {
        let protected = vec![PathBuf::from("/Users/dev/Desktop")];
        let old = base_primary_snapshot();

        // 1. 新增穿透排除墙的子根 -> IncrementalUpdate（需补扫）
        let mut add_pierced = old.clone();
        add_pierced.primary_roots.push("/Users/dev/Library/WeWork".into());
        assert_eq!(
            diff_primary_scope_with(&old, &add_pierced, &protected, |_| true),
            PrimaryScopeAction::IncrementalUpdate {
                prune_first: false,
                scan_roots: vec![PathBuf::from("/Users/dev/Library/WeWork")],
            }
        );

        // 2. 同时删根 + 增根 -> IncrementalUpdate { prune_first: true }
        let mut swap_roots = old.clone();
        swap_roots.primary_roots = vec!["/Users/dev/Work".into(), "/opt/code".into()];
        assert_eq!(
            diff_primary_scope_with(&old, &swap_roots, &protected, |_| true),
            PrimaryScopeAction::IncrementalUpdate {
                prune_first: true,
                scan_roots: vec![PathBuf::from("/opt/code")],
            }
        );

        // 3. 浅层应用根升格为普通根 (/Applications) -> IncrementalUpdate
        let mut with_shallow = old.clone();
        with_shallow.primary_roots.push("/Applications".into());
        with_shallow.shallow_app_roots.push("/Applications".into());

        let mut upgraded = with_shallow.clone();
        upgraded.shallow_app_roots.clear();
        assert_eq!(
            diff_primary_scope_with(&with_shallow, &upgraded, &protected, |_| true),
            PrimaryScopeAction::IncrementalUpdate {
                prune_first: false,
                scan_roots: vec![PathBuf::from("/Applications")],
            }
        );

        // 4. 普通根降格回浅层根 -> PruneOnly
        assert_eq!(
            diff_primary_scope_with(&upgraded, &with_shallow, &protected, |_| true),
            PrimaryScopeAction::PruneOnly
        );

        // 5. 子根 /Applications/Utilities 从普通降格回浅层 /Applications -> PruneOnly
        let mut with_utilities = with_shallow.clone();
        with_utilities
            .primary_roots
            .push("/Applications/Utilities".into());
        assert_eq!(
            diff_primary_scope_with(&with_shallow, &with_utilities, &protected, |_| true),
            PrimaryScopeAction::IncrementalUpdate {
                prune_first: false,
                scan_roots: vec![PathBuf::from("/Applications/Utilities")],
            }
        );
        assert_eq!(
            diff_primary_scope_with(&with_utilities, &with_shallow, &protected, |_| true),
            PrimaryScopeAction::PruneOnly
        );

        // 6. 失去 FDA -> PruneOnly；获得 FDA -> IncrementalUpdate 补扫受保护目录
        let mut with_fda = old.clone();
        with_fda.has_fda = true;
        assert_eq!(
            diff_primary_scope_with(&with_fda, &old, &protected, |_| true),
            PrimaryScopeAction::PruneOnly
        );
        assert_eq!(
            diff_primary_scope_with(&old, &with_fda, &protected, |_| true),
            PrimaryScopeAction::IncrementalUpdate {
                prune_first: false,
                scan_roots: vec![PathBuf::from("/Users/dev/Desktop")],
            }
        );

        // 7. 挂载排除根移除分流：漂移卸载（不在主根下）-> NoOp；嵌套卷移出配置且在主根下 -> IncrementalUpdate
        let mut with_external_mount = old.clone();
        with_external_mount
            .excluded_mount_roots
            .push("/Volumes/Disk-1".into());
        assert_eq!(
            diff_primary_scope_with(&with_external_mount, &old, &protected, |_| false),
            PrimaryScopeAction::NoOp
        );

        let mut with_nested_mount = old.clone();
        with_nested_mount
            .excluded_mount_roots
            .push("/Users/dev/mnt/share".into());
        assert_eq!(
            diff_primary_scope_with(&with_nested_mount, &old, &protected, |_| true),
            PrimaryScopeAction::IncrementalUpdate {
                prune_first: false,
                scan_roots: vec![PathBuf::from("/Users/dev/mnt/share")],
            }
        );

        // 8. 移除与主根完全无交集的排除路径不触发 FullRebuild
        let mut with_unrelated_ex = old.clone();
        with_unrelated_ex
            .exclude_paths
            .push("/Volumes/Ext/Cache".into());
        assert_eq!(
            diff_primary_scope_with(&with_unrelated_ex, &old, &protected, |_| true),
            PrimaryScopeAction::PruneOnly
        );

        // 9. 无 FDA 且包含受保护目录根时，相同快照必须判定为 NoOp，绝不误报 PruneOnly
        let protected_with_doc = vec![PathBuf::from("/Users/dev/Documents")];
        let mut no_fda_protected = old.clone();
        no_fda_protected.has_fda = false;
        no_fda_protected
            .primary_roots
            .push("/Users/dev/Documents".into());
        assert_eq!(
            diff_primary_scope_with(
                &no_fda_protected,
                &no_fda_protected,
                &protected_with_doc,
                |_| true
            ),
            PrimaryScopeAction::NoOp
        );

        // 10. 主根为 `/` 时移除落在新 excluded_mount_roots 下的外置卷子排除路径，不得误判主索引 FullRebuild
        let root_old = PrimaryScopeSnapshot {
            schema_version: PRIMARY_SCOPE_SCHEMA_VERSION,
            primary_roots: vec!["/".into()],
            shallow_app_roots: vec![],
            exclude_paths: vec!["/Volumes/ExtDisk/Cache".into()],
            excluded_mount_roots: vec!["/Volumes/ExtDisk".into()],
            exclude: vec![],
            include_hidden: false,
            exclude_sensitive: true,
            sensitive_names: vec![],
            has_fda: true,
        };
        let mut root_new = root_old.clone();
        root_new.exclude_paths.clear();
        assert_eq!(
            diff_primary_scope_with(&root_old, &root_new, &[], |_| true),
            PrimaryScopeAction::PruneOnly
        );
    }
}
