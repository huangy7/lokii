use crate::config::{expand_tilde, AppConfig};
use crate::path_utils::{path_starts_with_ci, paths_equal_ci};
use std::borrow::Cow;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

/// 规范化前缀：构造期预计算路径层级深度，并统一为小写、带尾随 '/' 的表示，
/// 使热路径的前缀比对只需字节比较，不必为每个候选重新分配字符串。
#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedPrefix {
    raw: PathBuf,
    prefix_str: String,
    depth: usize,
    /// 该索引根是否启用 `.app` 浅层容器策略（仅对 `index_apps` 自动追加且未被用户覆盖的系统应用根为 `true`）。
    shallow_apps: bool,
}

impl NormalizedPrefix {
    fn new(path: PathBuf) -> Self {
        Self::with_shallow_apps(path, false)
    }

    fn with_shallow_apps(path: PathBuf, shallow_apps: bool) -> Self {
        let depth = path.components().count();
        let mut s = path.to_string_lossy().to_string();
        #[cfg(target_os = "macos")]
        {
            s = s.to_lowercase();
        }
        if !s.ends_with('/') {
            s.push('/');
        }
        Self {
            raw: path,
            prefix_str: s,
            depth,
            shallow_apps,
        }
    }

    /// 目标路径是否落在此前缀目录下，或与该目录本身相等。
    #[inline(always)]
    fn matches_str(&self, path_str: &str) -> bool {
        let prefix = self.prefix_str.as_str();
        let p_len = prefix.len();
        // 前缀自带尾随 '/'，命中即已对齐组件边界；长度少一时退化为目录自身的精确匹配
        let hit = |s: &str| {
            s.starts_with(prefix)
                || (p_len > 1 && s.len() == p_len - 1 && s == &prefix[..p_len - 1])
        };

        #[cfg(target_os = "macos")]
        {
            let path_bytes = path_str.as_bytes();
            let p_bytes = prefix.as_bytes();
            let hit_ascii = |s: &[u8]| {
                (s.len() >= p_len && s[..p_len].eq_ignore_ascii_case(p_bytes))
                    || (p_len > 1
                        && s.len() == p_len - 1
                        && s.eq_ignore_ascii_case(&p_bytes[..p_len - 1]))
            };
            if hit_ascii(path_bytes) {
                return true;
            }
            // 重音字符的大小写折叠需要 Unicode 语义，仅当两侧含非 ASCII 字节时才付出分配代价
            if !path_bytes.is_ascii() || !p_bytes.is_ascii() {
                return hit(&path_str.to_lowercase());
            }
            false
        }

        #[cfg(not(target_os = "macos"))]
        {
            hit(path_str)
        }
    }
}

/// 匹配 `path` 的最深前缀；无任何前缀匹配时为 `None`。
fn deepest_matching_prefix<'a>(
    prefixes: &'a [NormalizedPrefix],
    path_str: &str,
) -> Option<&'a NormalizedPrefix> {
    prefixes
        .iter()
        .filter(|p| p.matches_str(path_str))
        .max_by_key(|p| p.depth)
}

/// 匹配 `path` 的最深前缀层级；无任何前缀匹配时为 `None`。
fn deepest_matching_depth(prefixes: &[NormalizedPrefix], path_str: &str) -> Option<usize> {
    deepest_matching_prefix(prefixes, path_str).map(|p| p.depth)
}

/// 索引范围策略对候选路径的归类决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeDecision {
    /// 彻底丢弃该条目及其全部后代，不进入索引。
    HardDrop,
    /// 仅收录浅层容器目录自身（`is_dir = true`），阻断向下递归与事件展开。
    IndexShallowContainer,
    /// 按正常条目收录，并允许继续遍历其子项。
    IndexNormally,
}

/// 判定单个路径组件名是否命中无条件屏蔽的系统噪声黑名单（ASCII 大小写无关）。
pub fn is_system_noise_name(name: &str) -> bool {
    if name.starts_with("._") {
        return true;
    }
    const EXACT_SYSTEM_NOISE: [&str; 8] = [
        ".DS_Store",
        ".Spotlight-V100",
        ".Trashes",
        ".fseventsd",
        ".TemporaryItems",
        ".DocumentRevisions-V100",
        "Thumbs.db",
        "ehthumbs.db",
    ];
    EXACT_SYSTEM_NOISE
        .iter()
        .any(|noise| name.eq_ignore_ascii_case(noise))
}

/// 是否为 macOS 应用包（`.app`），扩展名大小写无关。
fn is_app_bundle_name(name: &str) -> bool {
    name.rsplit_once('.')
        .map(|(stem, ext)| !stem.is_empty() && ext.eq_ignore_ascii_case("app"))
        .unwrap_or(false)
}

/// 判定给定路径的末端文件名是否命中系统噪声。
pub(crate) fn is_system_noise_path(path: &Path) -> bool {
    path.file_name()
        .map(|name| is_system_noise_name(&name.to_string_lossy()))
        .unwrap_or(false)
}

/// 目录级子项过滤上下文：在枚举目录前计算一次，使该目录下所有直接子条目
/// 只需 O(1) 检查末端文件名，免除每文件重复遍历全部祖先组件与扫描多条前缀规则。
#[derive(Debug, Clone, Copy)]
pub(crate) struct DirChildContext {
    /// 父目录自身是否已按 `evaluate_with_hidden(dir, true, include_hidden)` 常规放行。
    parent_admitted: bool,
    /// 当前目录下是否存在深度恰好为 `dir_depth + 1` 的直接子级 `include_roots` 或 `excluded_prefixes` 规则。
    has_child_prefix_override: bool,
    /// 本次遍历是否包含用户隐藏文件。
    include_hidden: bool,
    /// 父目录最深匹配根是否启用 `.app` 浅层容器策略。
    shallow_apps: bool,
}

/// 统一索引范围策略：按严格优先级裁决显式排除、系统噪声、用户隐藏开关与按根浅层容器目录。
///
/// 构造期完成路径展开与前缀预编译，确保扫描和事件热路径保持零额外字符串规范化开销。
#[derive(Clone)]
pub struct IndexScopePolicy {
    /// Normalized include root paths from config `index_paths` and uncovered shallow app roots.
    include_roots: Vec<NormalizedPrefix>,
    /// Normalized exclude path prefixes from config `exclude_paths`.
    excluded_prefixes: Vec<NormalizedPrefix>,
    /// Directory names to skip descending into (component-level match).
    excluded_names: HashSet<String>,
    /// Whether user dotfiles (outside system noise) are eligible for indexing.
    include_hidden: bool,
    /// Whether credential directories/files stay out even when hidden files are indexed.
    exclude_sensitive: bool,
    /// Names matched by the sensitive protection (user-configurable).
    sensitive_names: Vec<String>,
}

/// 保留旧称别名以兼容既有调用点。
pub type CompiledFilter = IndexScopePolicy;

impl IndexScopePolicy {
    /// Build a compiled scope policy from `AppConfig`.
    /// Path normalization (tilde expansion, absolute path) happens here.
    pub fn from_config(config: &AppConfig) -> Self {
        Self::from_resolved_roots(config, config.resolve_paths())
    }

    /// 零磁盘 I/O 的纯词法策略构建器：不调用 `Path::exists()`，专供检索热路径解释排除原因，
    /// 防止已配置的离线 SMB/外置挂载点在搜索空结果时触发阻塞式 `stat`。
    pub fn from_config_lexical(config: &AppConfig) -> Self {
        Self::from_resolved_roots(config, config.resolve_paths_lexical())
    }

    /// 内部纯词法构造器：仅展开用户显式 `index_paths`（全标 `shallow_apps = false`），
    /// 绝不调用 `resolve_shallow_app_roots()`，用于打破 `resolve_shallow_app_roots` 的自递归环。
    pub(crate) fn from_user_roots_lexical(config: &AppConfig) -> Self {
        let include_roots = config
            .index
            .index_paths
            .iter()
            .map(|p| NormalizedPrefix::with_shallow_apps(expand_tilde(p), false))
            .collect();
        let excluded_prefixes = config
            .resolve_exclude_paths()
            .into_iter()
            .map(NormalizedPrefix::new)
            .collect();
        let excluded_names: HashSet<String> = config.index.exclude.iter().cloned().collect();

        Self {
            include_roots,
            excluded_prefixes,
            excluded_names,
            include_hidden: config.index.include_hidden,
            exclude_sensitive: config.index.exclude_sensitive,
            sensitive_names: config.index.sensitive_names.clone(),
        }
    }

    fn from_resolved_roots(config: &AppConfig, roots: Vec<PathBuf>) -> Self {
        let shallow_app_roots = config.resolve_shallow_app_roots();
        let include_roots = roots
            .into_iter()
            .map(|root| {
                let is_shallow = shallow_app_roots
                    .iter()
                    .any(|shallow| paths_equal_ci(&root, shallow));
                NormalizedPrefix::with_shallow_apps(root, is_shallow)
            })
            .collect();
        let excluded_prefixes = config
            .resolve_exclude_paths()
            .into_iter()
            .map(NormalizedPrefix::new)
            .collect();
        let excluded_names: HashSet<String> = config.index.exclude.iter().cloned().collect();

        Self {
            include_roots,
            excluded_prefixes,
            excluded_names,
            include_hidden: config.index.include_hidden,
            exclude_sensitive: config.index.exclude_sensitive,
            sensitive_names: config.index.sensitive_names.clone(),
        }
    }

    /// 从结构化 `PrimaryScopeSnapshot` 构造纯词法过滤策略，供 `scope_diff` 精确判定旧范围覆盖。
    pub(crate) fn from_primary_snapshot(
        snapshot: &crate::scope_diff::PrimaryScopeSnapshot,
    ) -> Self {
        let include_roots = snapshot
            .primary_roots
            .iter()
            .map(|root| {
                let path = PathBuf::from(root);
                let is_shallow = snapshot
                    .shallow_app_roots
                    .iter()
                    .any(|s| paths_equal_ci(&path, Path::new(s)));
                NormalizedPrefix::with_shallow_apps(path, is_shallow)
            })
            .collect();
        let mut excluded_prefixes: Vec<NormalizedPrefix> = snapshot
            .exclude_paths
            .iter()
            .map(|p| NormalizedPrefix::new(PathBuf::from(p)))
            .collect();
        for m in &snapshot.excluded_mount_roots {
            excluded_prefixes.push(NormalizedPrefix::new(PathBuf::from(m)));
        }
        let excluded_names: HashSet<String> = snapshot.exclude.iter().cloned().collect();

        Self {
            include_roots,
            excluded_prefixes,
            excluded_names,
            include_hidden: snapshot.include_hidden,
            exclude_sensitive: snapshot.exclude_sensitive,
            sensitive_names: snapshot.sensitive_names.clone(),
        }
    }

    pub(crate) fn include_hidden(&self) -> bool {
        self.include_hidden
    }

    pub(crate) fn has_empty_include_roots(&self) -> bool {
        self.include_roots.is_empty()
    }

    pub(crate) fn matches_excluded_prefix(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();
        deepest_matching_depth(&self.excluded_prefixes, &path_str).is_some()
    }

    /// Construct a filter for the primary local index domain.
    pub fn from_primary_config(
        config: &AppConfig,
        primary_roots: &[PathBuf],
        mounted_exclude_roots: &[PathBuf],
    ) -> Self {
        let shallow_app_roots = config.resolve_shallow_app_roots();
        let include_roots = primary_roots
            .iter()
            .cloned()
            .map(|root| {
                let is_shallow = shallow_app_roots
                    .iter()
                    .any(|shallow| paths_equal_ci(&root, shallow));
                NormalizedPrefix::with_shallow_apps(root, is_shallow)
            })
            .collect();
        let mut excluded_prefixes: Vec<NormalizedPrefix> = config
            .resolve_exclude_paths()
            .into_iter()
            .map(NormalizedPrefix::new)
            .collect();
        for m in mounted_exclude_roots {
            excluded_prefixes.push(NormalizedPrefix::new(m.clone()));
        }
        let excluded_names: HashSet<String> = config.index.exclude.iter().cloned().collect();

        Self {
            include_roots,
            excluded_prefixes,
            excluded_names,
            include_hidden: config.index.include_hidden,
            exclude_sensitive: config.index.exclude_sensitive,
            sensitive_names: config.index.sensitive_names.clone(),
        }
    }

    /// Construct a filter isolated for a specific mounted volume (always `shallow_apps = false`).
    pub fn for_volume(config: &AppConfig, active_root: &Path) -> Self {
        let snapshot = crate::scope_diff::VolumeScopeSnapshot::from_config(
            config,
            active_root,
            active_root,
        );
        Self::for_volume_snapshot(&snapshot, active_root)
    }

    /// 直接基于结构化 `VolumeScopeSnapshot`（相对子路径拼回当前 `active_root`）编译外置卷过滤器，
    /// 保证快照与运行期扫描/裁剪/实时流过滤语义 100% 同源并天然免疫 `-1` 挂载点漂移。
    pub fn for_volume_snapshot(
        snapshot: &crate::scope_diff::VolumeScopeSnapshot,
        active_root: &Path,
    ) -> Self {
        let include_roots = vec![NormalizedPrefix::with_shallow_apps(
            active_root.to_path_buf(),
            false,
        )];
        let excluded_prefixes: Vec<NormalizedPrefix> = snapshot
            .exclude_subpaths
            .iter()
            .filter(|sub| !sub.is_empty())
            .map(|sub| NormalizedPrefix::new(active_root.join(sub)))
            .collect();
        let excluded_names: HashSet<String> = snapshot.exclude.iter().cloned().collect();

        Self {
            include_roots,
            excluded_prefixes,
            excluded_names,
            include_hidden: snapshot.include_hidden,
            exclude_sensitive: snapshot.exclude_sensitive,
            sensitive_names: snapshot.sensitive_names.clone(),
        }
    }

    /// 是否有排除前缀位于 `path` 之下：用于判断该目录内部是否还藏有需要单独监听的目标。
    pub fn has_excluded_descendant(&self, path: &Path) -> bool {
        self.excluded_prefixes
            .iter()
            .any(|prefix| path_starts_with_ci(&prefix.raw, path))
    }

    /// 按配置默认的 `include_hidden` 开关评估路径归属。
    pub fn evaluate(&self, path: &Path, is_dir: bool) -> ScopeDecision {
        self.evaluate_with_hidden(path, is_dir, self.include_hidden)
    }

    /// 返回 `skip_components` 之后首个命中排除目录名的目录/文件组件。
    /// Uses component-level matching: `/foo/node_modules/bar` matches `node_modules`,
    /// but `/foo/mynode_modules/bar` does not.
    fn excluded_component_after<'a>(
        &'a self,
        path: &'a Path,
        skip_components: usize,
    ) -> Option<Cow<'a, str>> {
        if self.excluded_names.is_empty() {
            return None;
        }
        path.components().skip(skip_components).find_map(|c| match c {
            Component::Normal(name) => {
                let name = name.to_string_lossy();
                self.excluded_names.contains(name.as_ref()).then_some(name)
            }
            _ => None,
        })
    }

    /// 排除规则命中的片段：排除前缀优先于排除目录名。
    /// `is_excluded` 与空结果解释共用，保证提示与扫描判定不漂移；借用返回，扫描热路径零分配。
    fn exclusion_reason<'a>(&'a self, path_str: &'a str, in_depth: usize) -> Option<Cow<'a, str>> {
        if let Some(prefix) = deepest_matching_prefix(&self.excluded_prefixes, path_str) {
            if prefix.depth >= in_depth {
                return Some(prefix.raw.to_string_lossy());
            }
        }
        self.excluded_component_after(Path::new(path_str), in_depth)
    }

    /// 以显式指定的 `include_hidden` 开关评估候选路径。
    ///
    /// 优先级由高到低：
    /// 1. 显式 `exclude_paths` 与最深 `include_root` 比对（更深根穿透，同深度排除优先）；
    /// 2. `!include_hidden` 用户点文件检查；
    /// 3. `config.index.exclude` 目录名匹配，命中即丢弃整棵子树；
    /// 4. 凭据目录/文件（`exclude_sensitive` 开关，仅在索引根之下生效，显式加入索引根可穿透）；
    /// 5. 应用开关开启时，`.app` 包目录只收录自身，包内条目一律丢弃。
    pub fn evaluate_with_hidden(&self, path: &Path, is_dir: bool, include_hidden: bool) -> ScopeDecision {
        let path_str = path.to_string_lossy();
        let matched_root = deepest_matching_prefix(&self.include_roots, &path_str);

        // 配置了索引根却未命中任何根：该路径不属于索引范围
        if !self.include_roots.is_empty() && matched_root.is_none() {
            return ScopeDecision::HardDrop;
        }

        let in_depth = matched_root.map(|r| r.depth).unwrap_or(0);
        let shallow_apps = matched_root.is_some_and(|r| r.shallow_apps);

        if let Some(ex_depth) = deepest_matching_depth(&self.excluded_prefixes, &path_str) {
            if ex_depth >= in_depth {
                return ScopeDecision::HardDrop;
            }
        }

        // 第 2 层优先级：用户点文件开关
        if !include_hidden {
            for comp in path.components().skip(in_depth) {
                if let std::path::Component::Normal(name) = comp {
                    if name.to_string_lossy().starts_with('.') {
                        return ScopeDecision::HardDrop;
                    }
                }
            }
        }

        // 第 3 层优先级：排除目录名与按根启用的 macOS 应用包浅层容器（只作用在索引根之后的相对组件）
        let mut comps = path
            .components()
            .skip(in_depth)
            .filter_map(|comp| match comp {
                std::path::Component::Normal(name) => Some(name),
                _ => None,
            })
            .peekable();
        while let Some(name) = comps.next() {
            let name = name.to_string_lossy();
            if self.excluded_names.contains(name.as_ref()) {
                return ScopeDecision::HardDrop;
            }
            if self.exclude_sensitive
                && self
                    .sensitive_names
                    .iter()
                    .any(|sensitive| sensitive.eq_ignore_ascii_case(&name))
            {
                return ScopeDecision::HardDrop;
            }
            if shallow_apps && is_app_bundle_name(&name) {
                return if comps.peek().is_some() {
                    // 包内部：仅对浅层应用根生效，只收录应用包自身
                    ScopeDecision::HardDrop
                } else if is_dir {
                    ScopeDecision::IndexShallowContainer
                } else {
                    ScopeDecision::IndexNormally
                };
            }
        }

        ScopeDecision::IndexNormally
    }

    /// 预计算目录 `dir` 的子项过滤上下文，供扫描器在枚举目录时为所有直接子条目执行 O(1) 判定。
    ///
    /// 任意直接子路径 `dir/child` 的组件深度严格为 `dir_depth + 1`，
    /// 因此前缀集合中只有深度恰好等于 `dir_depth + 1` 且位于 `dir` 之下的规则可能改变 `in_depth` 或 `ex_depth`。
    pub(crate) fn dir_child_context(&self, dir: &Path, include_hidden: bool) -> DirChildContext {
        let dir_depth = dir.components().count();
        let child_depth = dir_depth + 1;
        let has_child_include = self
            .include_roots
            .iter()
            .any(|p| p.depth == child_depth && path_starts_with_ci(&p.raw, dir));
        let has_child_exclude = self
            .excluded_prefixes
            .iter()
            .any(|p| p.depth == child_depth && path_starts_with_ci(&p.raw, dir));
        let parent_admitted =
            self.evaluate_with_hidden(dir, true, include_hidden) == ScopeDecision::IndexNormally;
        let shallow_apps = deepest_matching_prefix(&self.include_roots, &dir.to_string_lossy())
            .is_some_and(|p| p.shallow_apps);
        DirChildContext {
            parent_admitted,
            has_child_prefix_override: has_child_include || has_child_exclude,
            include_hidden,
            shallow_apps,
        }
    }

    /// 针对已预计算 `DirChildContext` 的目录直接子条目执行 O(1) 过滤，语义与 `evaluate_with_hidden` 100% 等价。
    #[inline]
    pub(crate) fn evaluate_child(
        &self,
        ctx: &DirChildContext,
        child_full_path: &Path,
        child_name: &str,
        is_dir: bool,
    ) -> ScopeDecision {
        // 若存在直接子级前缀覆写或非标准单段文件名，回退完整评估以保证与全路径口径严格一致
        if ctx.has_child_prefix_override
            || child_name.is_empty()
            || child_name == "."
            || child_name == ".."
            || child_name.contains('/')
        {
            return self.evaluate_with_hidden(child_full_path, is_dir, ctx.include_hidden);
        }
        if !ctx.parent_admitted {
            return ScopeDecision::HardDrop;
        }
        if !ctx.include_hidden && child_name.starts_with('.') {
            return ScopeDecision::HardDrop;
        }
        if self.excluded_names.contains(child_name) {
            return ScopeDecision::HardDrop;
        }
        if self.exclude_sensitive
            && self
                .sensitive_names
                .iter()
                .any(|sensitive| sensitive.eq_ignore_ascii_case(child_name))
        {
            return ScopeDecision::HardDrop;
        }
        if ctx.shallow_apps && is_app_bundle_name(child_name) {
            return if is_dir {
                ScopeDecision::IndexShallowContainer
            } else {
                ScopeDecision::IndexNormally
            };
        }
        ScopeDecision::IndexNormally
    }

    /// 兼容调用点：仅当路径被策略判定为 `HardDrop` 时返回 `true`。
    pub fn is_excluded(&self, path: &Path) -> bool {
        self.evaluate(path, true) == ScopeDecision::HardDrop
    }
}

/// 结果为空时解释路径为何搜不到：查询词里的路径命中排除规则时，返回该命中片段。
/// 与扫描期 `is_excluded` 共用 `exclusion_reason`，提示文案不会与实际索引范围各说各话。
pub fn exclusion_hint(query: &str, config: &AppConfig) -> Option<String> {
    let mut path_words = query
        .split_whitespace()
        .map(|word| word.strip_prefix("in:").unwrap_or(word))
        .filter(|word| word.contains('/'));
    // 没有带 '/' 的词元就不可能命中排除规则，先免掉纯词法策略构建的开销
    let first = path_words.next()?;

    // 采用纯词法路径展开，确保搜索热路径 100% 零磁盘系统调用
    let filter = CompiledFilter::from_config_lexical(config);
    std::iter::once(first).chain(path_words).find_map(|word| {
        let path = crate::search::path_query::expand_tilde(word);
        let in_depth = deepest_matching_depth(&filter.include_roots, &path).unwrap_or(0);
        filter.exclusion_reason(&path, in_depth).map(Cow::into_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, IndexConfig};
    use std::fs;
    use tempfile::TempDir;

    fn make_test_filter(index_paths: Vec<&str>, exclude: Vec<&str>, exclude_paths: Vec<&str>) -> CompiledFilter {
        let mut config = AppConfig::default();
        config.index = IndexConfig {
            index_paths: index_paths.iter().map(|s| s.to_string()).collect(),
            index_apps: false,
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
            exclude_paths: exclude_paths.iter().map(|s| s.to_string()).collect(),
            include_hidden: true,
            exclude_sensitive: true,
            sensitive_names: AppConfig::default().index.sensitive_names,
            max_results: 500,
            network_rescan_minutes: 5,
            serial_scan_paths: vec![],
        };
        CompiledFilter::from_config(&config)
    }

    #[test]
    fn test_default_config_excludes_library() {
        let config = AppConfig::default();
        let filter = CompiledFilter::from_config(&config);

        let home = dirs::home_dir().unwrap();

        // ~/Library should be excluded by prefix
        let library_cache = home.join("Library/Caches/com.apple.Safari");
        assert!(filter.is_excluded(&library_cache));

        // ~/Documents should NOT be excluded
        let docs = home.join("Documents/report.pdf");
        assert!(!filter.is_excluded(&docs));
    }

    #[test]
    fn volume_filter_allows_explicit_volume_despite_default_volumes_exclusion() {
        let mut cfg = AppConfig::default();
        cfg.index.exclude_paths = vec!["/Volumes".into(), "/Volumes/Share/PrivateCache".into()];
        let f_sub = CompiledFilter::for_volume(&cfg, Path::new("/Volumes/Share"));
        assert!(!f_sub.is_excluded(Path::new("/Volumes/Share/docs/a.txt")));
        assert!(f_sub.is_excluded(Path::new("/Volumes/Share/PrivateCache/tmp.bin")));
        let f_root = CompiledFilter::for_volume(&cfg, Path::new("/Volumes"));
        assert!(!f_root.is_excluded(Path::new("/Volumes/DiskA/a.txt")));
    }

    #[test]
    fn primary_filter_includes_mounted_exclude_roots() {
        let cfg = AppConfig::default();
        let primary_roots = vec![PathBuf::from("/Users/dev")];
        let mounted_exclude = vec![PathBuf::from("/Users/dev/MountedSub")];
        let filter = CompiledFilter::from_primary_config(&cfg, &primary_roots, &mounted_exclude);
        assert!(!filter.is_excluded(Path::new("/Users/dev/docs/file.txt")));
        assert!(filter.is_excluded(Path::new("/Users/dev/MountedSub/subfile.txt")));
    }

    #[test]
    fn test_default_index_is_home() {
        let config = AppConfig::default();
        assert_eq!(
            config.index.index_paths,
            vec!["~".to_string()],
            "Default index_paths should be the home directory only"
        );
        // But ~/Library is excluded by prefix
        assert!(
            config.index.exclude_paths.contains(&"~/Library".to_string()),
            "Default exclude_paths should include ~/Library"
        );
    }

    #[test]
    fn test_path_outside_include_root_is_excluded() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_string_lossy().to_string();
        let filter = make_test_filter(vec![&root], vec![], vec![]);

        let inside = tmp.path().join("foo.txt");
        fs::write(&inside, "data").unwrap();

        assert!(!filter.is_excluded(&inside));
        assert!(filter.is_excluded(Path::new("/tmp/outside.txt")));
    }

    #[test]
    fn test_excluded_prefix_wins_within_include_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_string_lossy().to_string();
        let excluded_dir = tmp.path().join("skip_me");
        fs::create_dir(&excluded_dir).unwrap();
        let excluded_str = excluded_dir.to_string_lossy().to_string();

        let filter = make_test_filter(
            vec![&root],
            vec![],
            vec![&excluded_str],
        );

        let inside = excluded_dir.join("nested/file.txt");
        assert!(filter.is_excluded(&inside));
        assert!(!filter.is_excluded(&tmp.path().join("ok/file.txt")));
    }

    #[test]
    fn test_excluded_component_matches_exact_dir_name_only() {
        let filter = make_test_filter(
            vec!["/tmp"],
            vec!["node_modules", ".git"],
            vec![],
        );

        assert!(filter.is_excluded(Path::new("/tmp/proj/node_modules/pkg/index.js")));
        assert!(filter.is_excluded(Path::new("/tmp/proj/.git/config")));
        // Component-level: only exact directory name match, not substring
        assert!(!filter.is_excluded(Path::new("/tmp/mynode_modules/pkg")));
        assert!(!filter.is_excluded(Path::new("/tmp/proj/src/main.rs")));
    }

    #[test]
    fn test_exclusion_hint_names_the_rule_that_hides_the_path() {
        let config = AppConfig::default();
        // .app 包内部被默认规则 "Contents" 剪掉
        assert_eq!(
            exclusion_hint("/Applications/Foo.app/Contents/Resources/x", &config).as_deref(),
            Some("Contents")
        );
        assert_eq!(
            exclusion_hint("in:/Applications/Foo.app/Contents/", &config).as_deref(),
            Some("Contents")
        );
        // 排除前缀优先于同段目录名
        assert_eq!(exclusion_hint("/usr/bin/ls", &config).as_deref(), Some("/usr"));
        // 未命中任何排除规则的位置与纯文件名查询不出提示
        assert_eq!(exclusion_hint("/Applications/Foo.app/Info.plist", &config), None);
        assert_eq!(exclusion_hint("main.rs", &config), None);

        // 多个排除前缀重叠时报告最深的那条
        let mut nested = AppConfig::default();
        nested.index.exclude_paths = vec!["/opt".to_string(), "/opt/data".to_string()];
        assert_eq!(exclusion_hint("/opt/data/x", &nested).as_deref(), Some("/opt/data"));

        // 显式索引根穿透排除前缀后，该位置确实可搜，不能误报
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_string_lossy().to_string();
        let pierced = format!("{root}/out/keep");
        fs::create_dir_all(&pierced).unwrap();
        let mut pierced_config = AppConfig::default();
        pierced_config.index.exclude_paths = vec![format!("{root}/out")];
        pierced_config.index.index_paths = vec![pierced.clone()];
        assert_eq!(exclusion_hint(&format!("{pierced}/notes.txt"), &pierced_config), None);
        assert_eq!(
            exclusion_hint(&format!("{root}/out/other.txt"), &pierced_config).as_deref(),
            Some(format!("{root}/out").as_str())
        );
    }

    #[test]
    fn test_is_excluded_full_pipeline() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_string_lossy().to_string();
        let node_modules = tmp.path().join("node_modules");
        fs::create_dir(&node_modules).unwrap();

        let cache_dir = tmp.path().join("cache_excluded");
        fs::create_dir(&cache_dir).unwrap();

        let filter = make_test_filter(
            vec![&root],
            vec!["node_modules"],
            vec![&cache_dir.to_string_lossy().to_string()],
        );

        // Outside include root → excluded
        assert!(filter.is_excluded(Path::new("/tmp/outside.txt")));

        // Under excluded prefix → excluded
        let cached_file = cache_dir.join("data.bin");
        assert!(filter.is_excluded(&cached_file));

        // Has excluded component → excluded
        assert!(filter.is_excluded(&node_modules.join("pkg/index.js")));

        // Clean path → not excluded
        let clean = tmp.path().join("src/main.rs");
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(&clean, "fn main() {}").unwrap();
        assert!(!filter.is_excluded(&clean));
    }

    #[test]
    fn test_exclude_paths_with_tilde_expansion() {
        let config = AppConfig::default();
        let filter = CompiledFilter::from_config(&config);

        let home = dirs::home_dir().unwrap();

        // ~/Library should be in excluded prefixes
        let lib = home.join("Library");
        assert!(
            filter.excluded_prefixes.iter().any(|p| p.raw == lib),
            "excluded_prefixes should contain resolved ~/Library"
        );

        // ~/.npm should be in excluded prefixes
        let npm = home.join(".npm");
        assert!(
            filter.excluded_prefixes.iter().any(|p| p.raw == npm),
            "excluded_prefixes should contain resolved ~/.npm"
        );
    }

    #[test]
    fn test_nested_include_pierces_broader_exclude() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_string_lossy().to_string();

        let library = tmp.path().join("Library");
        let wework_cache = library.join("Containers/com.tencent.WeWorkMac/Data/Documents/Files");
        fs::create_dir_all(&wework_cache).unwrap();

        let filter = make_test_filter(
            vec![&root, &wework_cache.to_string_lossy()],
            vec!["node_modules"],
            vec![&library.to_string_lossy()],
        );

        // 企微缓存目录下的文件成功穿透 ~/Library 排除
        let report = wework_cache.join("2026-10/report.pptx");
        assert!(
            !filter.is_excluded(&report),
            "深层显式索引路径应穿透父级排除前缀"
        );

        // ~/Library 下其他未显式穿透的目录依然被排除
        let safari_cache = library.join("Caches/com.apple.Safari/Cache.db");
        assert!(
            filter.is_excluded(&safari_cache),
            "未穿透的父级排除目录文件依然应被排除"
        );
    }

    #[test]
    fn test_same_depth_conflict_prefers_exclude() {
        let tmp = TempDir::new().unwrap();
        let secret = tmp.path().join("Secret");
        fs::create_dir_all(&secret).unwrap();
        let secret_str = secret.to_string_lossy().to_string();

        // 包含和排除配置完全相同的路径（同深度冲突）
        let filter = make_test_filter(
            vec![&secret_str],
            vec![],
            vec![&secret_str],
        );

        let file = secret.join("doc.txt");
        assert!(
            filter.is_excluded(&file),
            "同深度包含与排除冲突时应遵循排除优先"
        );
    }

    #[test]
    fn test_nested_exclude_re_excludes_under_pierced_include() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_string_lossy().to_string();

        let library = tmp.path().join("Library");
        let wework_cache = library.join("Containers/Files");
        let sub_ignored = wework_cache.join("Ignored");
        fs::create_dir_all(&sub_ignored).unwrap();

        let filter = make_test_filter(
            vec![&root, &wework_cache.to_string_lossy()],
            vec![],
            vec![&library.to_string_lossy(), &sub_ignored.to_string_lossy()],
        );

        // wework_cache 放行
        assert!(!filter.is_excluded(&wework_cache.join("good.txt")));
        // 更深层的 sub_ignored 再次排除
        assert!(filter.is_excluded(&sub_ignored.join("bad.txt")));
    }

    #[test]
    fn test_component_exclude_only_applies_after_include_root() {
        let tmp = TempDir::new().unwrap();
        // 显式包含的根自身路径中含有 "build" 关键字
        let build_root = tmp.path().join("my-app/build/artifacts");
        fs::create_dir_all(&build_root).unwrap();

        let filter = make_test_filter(
            vec![&build_root.to_string_lossy()],
            vec!["build", "node_modules"],
            vec![],
        );

        // 显式索引根自身的 "build" 目录名不应被误杀
        let artifact = build_root.join("bundle.js");
        assert!(
            !filter.is_excluded(&artifact),
            "显式索引根自身的黑名单组件名不应导致自身被过滤"
        );

        // 显式索引根内部子孙路径中的黑名单依然生效（方案 A）
        let node_modules_file = build_root.join("node_modules/pkg/index.js");
        assert!(
            filter.is_excluded(&node_modules_file),
            "显式索引根内部子孙的黑名单组件名应继续被过滤"
        );
    }

    #[test]
    fn test_unmatched_path_dropped_when_include_roots_configured() {
        let tmp = TempDir::new().unwrap();
        let my_dir = tmp.path().join("my_dir");
        fs::create_dir_all(&my_dir).unwrap();

        let filter = make_test_filter(
            vec![&my_dir.to_string_lossy()],
            vec![],
            vec![],
        );

        // 外部路径直接被排除
        assert!(filter.is_excluded(Path::new("/var/log/system.log")));
        assert!(filter.is_excluded(Path::new("/etc/hosts")));
    }

    #[test]
    fn test_scope_policy_explicit_exclude_vs_deeper_include_precedence() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let excluded_dir = root.join("excluded_dir");
        let pierced_dir = excluded_dir.join("pierced");
        fs::create_dir_all(&pierced_dir).unwrap();

        let mut config = AppConfig::default();
        config.index.index_paths = vec![
            root.to_string_lossy().into_owned(),
            pierced_dir.to_string_lossy().into_owned(),
        ];
        config.index.exclude_paths = vec![excluded_dir.to_string_lossy().into_owned()];
        config.index.include_hidden = true;

        let policy = IndexScopePolicy::from_config(&config);
        assert_eq!(
            policy.evaluate(&excluded_dir.join("a.txt"), false),
            ScopeDecision::HardDrop
        );
        assert_eq!(
            policy.evaluate(&pierced_dir.join("keep.txt"), false),
            ScopeDecision::IndexNormally
        );
    }

    #[test]
    fn test_scope_policy_hidden_toggle() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let mut config = AppConfig::default();
        config.index.index_paths = vec![root.to_string_lossy().into_owned()];

        config.index.include_hidden = false;
        let hidden_off = IndexScopePolicy::from_config(&config);
        assert_eq!(hidden_off.evaluate(&root.join(".git"), true), ScopeDecision::HardDrop);
        assert_eq!(hidden_off.evaluate(&root.join(".venv"), true), ScopeDecision::HardDrop);
        assert_eq!(hidden_off.evaluate(&root.join(".cache"), true), ScopeDecision::HardDrop);
        assert_eq!(hidden_off.evaluate(&root.join(".zshrc"), false), ScopeDecision::HardDrop);
        assert_eq!(hidden_off.evaluate(&root.join(".gitignore"), false), ScopeDecision::HardDrop);
        assert_eq!(
            hidden_off.evaluate(&root.join("src/.gitignore"), false),
            ScopeDecision::HardDrop
        );

        config.index.include_hidden = true;
        let hidden_on = IndexScopePolicy::from_config(&config);
        assert_eq!(hidden_on.evaluate(&root.join(".git"), true), ScopeDecision::HardDrop);
        assert_eq!(
            hidden_on.evaluate(&root.join(".git/config"), false),
            ScopeDecision::HardDrop
        );
        assert_eq!(hidden_on.evaluate(&root.join(".venv"), true), ScopeDecision::HardDrop);
        assert_eq!(
            hidden_on.evaluate(&root.join(".zshrc"), false),
            ScopeDecision::IndexNormally
        );
        assert_eq!(
            hidden_on.evaluate(&root.join(".gitignore"), false),
            ScopeDecision::IndexNormally
        );
    }

    #[test]
    fn test_sensitive_names_respect_toggle_and_explicit_roots() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".ssh")).unwrap();
        let root = tmp.path().to_string_lossy().to_string();
        let make = |exclude_sensitive: bool, extra_root: Option<&Path>| {
            let mut config = AppConfig::default();
            let mut roots = vec![root.clone()];
            if let Some(extra) = extra_root {
                roots.push(extra.to_string_lossy().into_owned());
            }
            config.index = IndexConfig {
                index_paths: roots,
                index_apps: false,
                exclude: vec![],
                exclude_paths: vec![],
                include_hidden: true,
                exclude_sensitive,
                sensitive_names: AppConfig::default().index.sensitive_names,
                max_results: 500,
                network_rescan_minutes: 5,
                serial_scan_paths: vec![],
            };
            CompiledFilter::from_config(&config)
        };

        let guarded = make(true, None);
        assert_eq!(
            guarded.evaluate(&tmp.path().join(".ssh"), true),
            ScopeDecision::HardDrop
        );
        assert_eq!(
            guarded.evaluate(&tmp.path().join(".ssh/id_rsa"), false),
            ScopeDecision::HardDrop
        );
        assert_eq!(
            guarded.evaluate(&tmp.path().join(".npmrc"), false),
            ScopeDecision::HardDrop
        );
        assert_eq!(
            guarded.evaluate(&tmp.path().join("notes.txt"), false),
            ScopeDecision::IndexNormally
        );

        let open = make(false, None);
        assert_eq!(
            open.evaluate(&tmp.path().join(".ssh/id_rsa"), false),
            ScopeDecision::IndexNormally
        );

        // 名单来自配置，用户可增删
        let mut custom = AppConfig::default();
        custom.index.index_paths = vec![root.clone()];
        custom.index.exclude = vec![];
        custom.index.exclude_paths = vec![];
        custom.index.include_hidden = true;
        custom.index.sensitive_names = vec![".mykeys".to_string()];
        let custom = CompiledFilter::from_config(&custom);
        assert_eq!(
            custom.evaluate(&tmp.path().join(".ssh/id_rsa"), false),
            ScopeDecision::IndexNormally
        );
        assert_eq!(
            custom.evaluate(&tmp.path().join(".mykeys/key"), false),
            ScopeDecision::HardDrop
        );

        // 显式把凭据目录加入索引根视为用户授权
        let explicit = make(true, Some(&tmp.path().join(".ssh")));
        assert_eq!(
            explicit.evaluate(&tmp.path().join(".ssh/id_rsa"), false),
            ScopeDecision::IndexNormally
        );
    }

    #[test]
    fn test_app_bundle_shallow_only_when_app_indexing_enabled() {
        let mut config = AppConfig::default();
        config.index.index_paths = vec!["/Applications".to_string()];
        config.index.exclude_paths = vec![];
        let off = IndexScopePolicy::from_config(&config);

        // 开关关闭：main 行为，应用包内部只靠 Contents 名称规则挡住
        assert_eq!(
            off.evaluate(Path::new("/Applications/WeChat.app"), true),
            ScopeDecision::IndexNormally
        );
        assert_eq!(
            off.evaluate(Path::new("/Applications/WeChat.app/Contents"), true),
            ScopeDecision::HardDrop
        );
        assert_eq!(
            off.evaluate(Path::new("/Applications/WeChat.app/Info.plist"), false),
            ScopeDecision::IndexNormally
        );

        // 用户显式将 /Applications 加入 index_paths 时，无论 index_apps 开关状态均按常规规则索引
        config.index.index_apps = true;
        let explicit_on = IndexScopePolicy::from_config(&config);
        assert_eq!(
            explicit_on.evaluate(Path::new("/Applications/WeChat.app"), true),
            ScopeDecision::IndexNormally
        );
        assert_eq!(
            explicit_on.evaluate(Path::new("/Applications/WeChat.app/Info.plist"), false),
            ScopeDecision::IndexNormally
        );
        assert_eq!(
            explicit_on.evaluate(
                Path::new("/Applications/WeChat.app/Contents/MacOS/WeChat"),
                false
            ),
            ScopeDecision::HardDrop
        );

        // 索引根只有 ~ 时，自动追加的 /Applications 与 /System/Applications 启用浅层容器（只收录 .app 自身），
        // 且 ~ 与外置卷完全不受 index_apps 影响
        config.index.index_paths = vec!["~".to_string()];
        config.index.exclude_paths = AppConfig::default().index.exclude_paths;
        let auto_policy = IndexScopePolicy::from_config_lexical(&config);
        assert_eq!(
            auto_policy.evaluate(Path::new("/Applications/WeChat.app"), true),
            ScopeDecision::IndexShallowContainer
        );
        assert_eq!(
            auto_policy.evaluate(Path::new("/Applications/WeChat.app/Info.plist"), false),
            ScopeDecision::HardDrop
        );
        assert_eq!(
            auto_policy.evaluate(
                Path::new("/Applications/WeChat.app/Contents/MacOS/WeChat"),
                false
            ),
            ScopeDecision::HardDrop
        );
        assert_eq!(
            auto_policy.evaluate(Path::new("/Applications/Demo.framework"), true),
            ScopeDecision::IndexNormally
        );
        assert_eq!(
            auto_policy.evaluate(Path::new("/Applications/Notes.app"), false),
            ScopeDecision::IndexNormally
        );
        assert_eq!(
            auto_policy.evaluate(Path::new("/System/Applications/Calculator.app"), true),
            ScopeDecision::IndexShallowContainer
        );
        assert_eq!(
            auto_policy.evaluate(
                Path::new("/System/Applications/Calculator.app/Contents/Info.plist"),
                false
            ),
            ScopeDecision::HardDrop
        );

        // 用户主目录下的 .app 不受 index_apps 浅层影响
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            auto_policy.evaluate(&home.join("Tools/MyTool.app"), true),
            ScopeDecision::IndexNormally
        );
        assert_eq!(
            auto_policy.evaluate(&home.join("Tools/MyTool.app/Info.plist"), false),
            ScopeDecision::IndexNormally
        );

        // 显式排除 /Applications 本身时，同深度排除优先依然 HardDrop
        let mut excluded_apps = config.clone();
        excluded_apps.index.exclude_paths.push("/Applications".into());
        let excluded_policy = IndexScopePolicy::from_config_lexical(&excluded_apps);
        assert_eq!(
            excluded_policy.evaluate(Path::new("/Applications/WeChat.app"), true),
            ScopeDecision::HardDrop
        );

        // 仅显式索引 /Applications/Utilities 且打开 index_apps：Utilities 按常规索引，/Applications 其余按浅层索引
        let mut sub_cfg = AppConfig::default();
        sub_cfg.index.index_paths = vec!["/Applications/Utilities".into()];
        sub_cfg.index.index_apps = true;
        let sub_policy = IndexScopePolicy::from_config_lexical(&sub_cfg);
        assert_eq!(
            sub_policy.evaluate(Path::new("/Applications/Safari.app"), true),
            ScopeDecision::IndexShallowContainer
        );
        assert_eq!(
            sub_policy.evaluate(Path::new("/Applications/Safari.app/Info.plist"), false),
            ScopeDecision::HardDrop
        );
        assert_eq!(
            sub_policy.evaluate(Path::new("/Applications/Utilities/Terminal.app"), true),
            ScopeDecision::IndexNormally
        );
        assert_eq!(
            sub_policy.evaluate(
                Path::new("/Applications/Utilities/Terminal.app/Info.plist"),
                false
            ),
            ScopeDecision::IndexNormally
        );

        // 索引根为 / 且默认排除 /System：/Applications 已被覆盖（不追加浅层），/System/Applications 被挡住（追加浅层）
        let mut root_cfg = AppConfig::default();
        root_cfg.index.index_paths = vec!["/".into()];
        root_cfg.index.index_apps = true;
        let shallow = root_cfg.resolve_shallow_app_roots();
        assert!(!shallow.contains(&PathBuf::from("/Applications")));
        assert!(shallow.contains(&PathBuf::from("/System/Applications")));
        let root_policy = IndexScopePolicy::from_config_lexical(&root_cfg);
        assert_eq!(
            root_policy.evaluate(Path::new("/Applications/Safari.app"), true),
            ScopeDecision::IndexNormally
        );
        assert_eq!(
            root_policy.evaluate(Path::new("/System/Applications/Calculator.app"), true),
            ScopeDecision::IndexShallowContainer
        );
    }

    #[test]
    fn test_exclusion_hint_lexical_respects_unmounted_include_root() {
        let mut config = AppConfig::default();
        config.index.index_paths = vec!["/Volumes/OfflineNAS_NonExistent/Workspace".to_string()];
        config.index.exclude_paths =
            vec!["/Volumes/OfflineNAS_NonExistent/Workspace/Build".to_string()];
        config.index.exclude = vec!["node_modules".to_string()];

        assert_eq!(
            exclusion_hint(
                "/Volumes/OfflineNAS_NonExistent/Workspace/Build/out.bin",
                &config
            ),
            Some("/Volumes/OfflineNAS_NonExistent/Workspace/Build".to_string())
        );
        assert_eq!(
            exclusion_hint(
                "/Volumes/OfflineNAS_NonExistent/Workspace/app/node_modules/pkg.js",
                &config
            ),
            Some("node_modules".to_string())
        );
    }

    #[test]
    fn evaluate_child_matches_evaluate_with_hidden_across_edge_cases() {
        let mut config = AppConfig::default();
        config.index.index_paths = vec![
            "/root".to_string(),
            "/root/blocked/pierced".to_string(),
            "/root/.ssh".to_string(),
        ];
        config.index.exclude_paths = vec![
            "/root/blocked".to_string(),
            "/root/sub/deep_blocked".to_string(),
        ];
        config.index.exclude = vec!["node_modules".to_string(), "target".to_string()];
        config.index.exclude_sensitive = true;

        let dirs = [
            "/root",
            "/root/sub",
            "/root/blocked",
            "/root/blocked/pierced",
            "/root/.ssh",
            "/root/Demo.app",
            "/outside",
        ];
        let children = [
            ("readme.md", false),
            ("docs", true),
            (".hidden", false),
            (".hidden_dir", true),
            ("node_modules", true),
            ("target", false),
            (".ssh", true),
            (".AWS", true),
            ("blocked", true),
            ("pierced", true),
            ("deep_blocked", true),
            ("Demo.app", true),
            ("Demo.app", false),
            ("Contents", true),
            ("ok", true),
        ];

        for index_apps in [false, true] {
            config.index.index_apps = index_apps;
            let policy = IndexScopePolicy::from_config_lexical(&config);
            for include_hidden in [false, true] {
                for dir in dirs {
                    let dir_path = Path::new(dir);
                    let ctx = policy.dir_child_context(dir_path, include_hidden);
                    for &(child_name, is_dir) in &children {
                        let full = dir_path.join(child_name);
                        let expected = policy.evaluate_with_hidden(&full, is_dir, include_hidden);
                        let actual = policy.evaluate_child(&ctx, &full, child_name, is_dir);
                        assert_eq!(
                            actual, expected,
                            "mismatch for dir={dir} child={child_name} is_dir={is_dir} include_hidden={include_hidden} index_apps={index_apps}"
                        );
                    }
                }
            }
        }

        // 空 include_roots 场景下父目录自身含排除组件时，子项同样保持与 evaluate_with_hidden 一致
        let empty_roots_policy = make_test_filter(vec![], vec!["node_modules"], vec!["/blocked"]);
        let excluded_dir = Path::new("/tmp/node_modules/sub");
        let ctx = empty_roots_policy.dir_child_context(excluded_dir, true);
        let child = excluded_dir.join("file.js");
        assert_eq!(
            empty_roots_policy.evaluate_child(&ctx, &child, "file.js", false),
            empty_roots_policy.evaluate_with_hidden(&child, false, true)
        );
    }
}
