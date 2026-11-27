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

/// 是否为 macOS 常见文档/媒体目录包（如 iWork、照片图库等）。
/// 这些目录在访达中表现为单个文件，仅收录包自身而不展开内部资源，以免碎片文件污染索引或触发未下载云盘包的拉取。
pub(crate) fn is_package_bundle_name(name: &str) -> bool {
    const PACKAGE_BUNDLE_EXTS: [&str; 9] = [
        "key",
        "numbers",
        "pages",
        "rtfd",
        "photoslibrary",
        "fcpbundle",
        "logicx",
        "band",
        "garageband",
    ];
    name.rsplit_once('.')
        .map(|(stem, ext)| {
            !stem.is_empty()
                && PACKAGE_BUNDLE_EXTS
                    .iter()
                    .any(|pkg| ext.eq_ignore_ascii_case(pkg))
        })
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
            if (shallow_apps && is_app_bundle_name(&name)) || is_package_bundle_name(&name) {
                return if comps.peek().is_some() {
                    // 包内部：只收录容器包目录自身，阻断向包内递归
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
        if (ctx.shallow_apps && is_app_bundle_name(child_name))
            || is_package_bundle_name(child_name)
        {
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

/// 路径索引诊断结论，用于设置页「为什么搜不到？」排查面板精确归因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathDiagnosisStatus {
    /// 路径在当前草稿配置下会被正常索引（或已穿透父级排除）。
    Indexed,
    /// 未命中任何已配置的索引根路径。
    OutsideIndexRoots,
    /// 命中了 `exclude_paths` 路径前缀排除规则（如 `~/Library`）。
    ExcludedByPathPrefix,
    /// 路径中的某个组件命中了 `exclude` 目录/文件名规则（如 `out`、`.git`）。
    ExcludedByName,
    /// 路径中存在以 `.` 开头的隐藏段且当前未开启隐藏文件索引。
    ExcludedByHidden,
    /// 路径命中了 `exclude_sensitive` 凭据保护名单（如 `.ssh`、`.aws`）。
    ExcludedBySensitive,
    /// 位于自动追加的浅层 `.app` 应用根内部，且当前路径进入包内子项。
    ExcludedByShallowApp,
    /// 位于 macOS 文档/媒体目录包（如 `.pages`、`.key`、`.photoslibrary`）内部，包自身已按单个容器收录。
    ExcludedByPackageBundle,
    /// 命中系统受保护目录且当前尚未授予完全磁盘访问权限（FDA）。
    ProtectedByFda,
    /// 沿途存在目录级符号链接，而扫描器非根目录启用 `O_NOFOLLOW` 不会跟随进入，且目标真实路径未被收录。
    SymlinkNotFollowed,
}

/// 单条路径的完整诊断结果（含命中规则、穿透状态、符号链接真实目标与安全修复建议）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathDiagnosis {
    pub normalized_path: String,
    pub status: PathDiagnosisStatus,
    pub matched_root: Option<String>,
    pub matched_rule: Option<String>,
    pub is_preset_rule: bool,
    pub pierced_exclude_path: Option<String>,
    pub symlink_target: Option<String>,
    pub suggested_index_path: Option<String>,
    pub also_requires_fda: bool,
    pub exists_on_disk: Option<bool>,
}

/// 纯词法规范化路径，处理 `.`、`..` 与冗余斜杠，绝不触碰磁盘。
fn normalize_lexical_pathbuf_resolved(path: &Path) -> PathBuf {
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

/// 若 `path` 位于用户主目录 `home` 之下或等于 `home`，折叠为 `~` / `~/...` 便于界面展示与配置持久化。
pub(crate) fn collapse_home_tilde_with(path: &Path, home: Option<&Path>) -> String {
    let normalized = normalize_lexical_pathbuf_resolved(path);
    if let Some(home_dir) = home {
        let norm_home = normalize_lexical_pathbuf_resolved(home_dir);
        if paths_equal_ci(&normalized, &norm_home) {
            return "~".to_string();
        }
        if path_starts_with_ci(&normalized, &norm_home) {
            let home_depth = norm_home.components().count();
            let mut rel = PathBuf::new();
            for comp in normalized.components().skip(home_depth) {
                rel.push(comp.as_os_str());
            }
            if rel.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rel.to_string_lossy());
        }
    }
    normalized.to_string_lossy().into_owned()
}

fn decode_percent_bytes(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push(((h << 4) | l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 清洗诊断输入：去除首尾空白、成对引号、shell 转义空格 `\ `，并将 `file://` URL 解码为本地路径。
pub fn clean_diagnostic_path_input(raw: &str) -> String {
    let mut s = raw.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        s = s[1..s.len() - 1].trim();
    }
    if let Some(rest) = s
        .strip_prefix("file://")
        .or_else(|| s.strip_prefix("FILE://"))
    {
        let without_host = if let Some(after_localhost) = rest.strip_prefix("localhost/") {
            format!("/{after_localhost}")
        } else if rest.starts_with('/') {
            rest.to_string()
        } else {
            format!("/{rest}")
        };
        return decode_percent_bytes(&without_host);
    }
    s.replace("\\ ", " ")
}

/// 在满足穿透深度（严格大于 `min_depth_exclusive`）且未被显式排除在自身或更深前缀的前提下，
/// 优先归一化为云盘根目录（`~/Library/CloudStorage/<Provider>` 或 iCloud Drive），否则回退目标目录。
fn compute_piercing_suggestion(
    target: &Path,
    is_dir: bool,
    home: Option<&Path>,
    excluded_prefixes: &[NormalizedPrefix],
    min_depth_exclusive: usize,
) -> Option<String> {
    let base_dir = if is_dir {
        target
    } else {
        target.parent().unwrap_or(target)
    };
    let norm_base = normalize_lexical_pathbuf_resolved(base_dir);
    let base_depth = norm_base.components().count();

    let can_pierce = |candidate: &Path| -> bool {
        let depth = candidate.components().count();
        if depth <= min_depth_exclusive {
            return false;
        }
        let cand_str = candidate.to_string_lossy();
        deepest_matching_depth(excluded_prefixes, &cand_str)
            .is_none_or(|ex_depth| ex_depth < depth)
    };

    if let Some(home_dir) = home {
        let norm_home = normalize_lexical_pathbuf_resolved(home_dir);
        let home_depth = norm_home.components().count();

        let cloud_storage_root = norm_home.join("Library").join("CloudStorage");
        if path_starts_with_ci(&norm_base, &cloud_storage_root) && base_depth >= home_depth + 3 {
            if let Some(Component::Normal(provider)) = norm_base.components().nth(home_depth + 2) {
                let provider_root = cloud_storage_root.join(provider);
                if can_pierce(&provider_root) {
                    return Some(collapse_home_tilde_with(&provider_root, home));
                }
            }
        }

        let icloud_root = norm_home
            .join("Library")
            .join("Mobile Documents")
            .join("com~apple~CloudDocs");
        if path_starts_with_ci(&norm_base, &icloud_root) && can_pierce(&icloud_root) {
            return Some(collapse_home_tilde_with(&icloud_root, home));
        }
    }

    if can_pierce(&norm_base) {
        Some(collapse_home_tilde_with(&norm_base, home))
    } else {
        None
    }
}

/// 纯词法诊断给定路径在指定配置与 FDA 状态下的索引归因，零磁盘 I/O。
pub fn diagnose_path_lexical(
    path: &Path,
    is_dir: bool,
    config: &AppConfig,
    has_fda: bool,
) -> PathDiagnosis {
    let protected = if has_fda {
        Vec::new()
    } else {
        crate::scanner::protected_paths()
    };
    let home = dirs::home_dir();
    diagnose_path_lexical_with(path, is_dir, config, has_fda, &protected, home.as_deref())
}

/// 可注入受保护目录列表与主目录路径的纯词法诊断实现，便于确定性单元测试。
pub(crate) fn diagnose_path_lexical_with(
    path: &Path,
    is_dir: bool,
    config: &AppConfig,
    has_fda: bool,
    protected: &[PathBuf],
    home: Option<&Path>,
) -> PathDiagnosis {
    let normalized = normalize_lexical_pathbuf_resolved(path);
    let normalized_display = collapse_home_tilde_with(&normalized, home);
    let path_str = normalized.to_string_lossy().into_owned();

    let matched_protected = if has_fda {
        None
    } else {
        protected
            .iter()
            .find(|p| paths_equal_ci(&normalized, p) || path_starts_with_ci(&normalized, p))
    };
    let also_requires_fda = matched_protected.is_some();

    let policy = IndexScopePolicy::from_config_lexical(config);
    let defaults = AppConfig::default();
    let default_exclude_paths: Vec<PathBuf> = defaults.resolve_exclude_paths();

    let matched_root_prefix = if policy.has_empty_include_roots() {
        None
    } else {
        deepest_matching_prefix(&policy.include_roots, &path_str)
    };

    let Some(matched_root_prefix) = matched_root_prefix else {
        return PathDiagnosis {
            normalized_path: normalized_display,
            status: PathDiagnosisStatus::OutsideIndexRoots,
            matched_root: None,
            matched_rule: None,
            is_preset_rule: false,
            pierced_exclude_path: None,
            symlink_target: None,
            suggested_index_path: compute_piercing_suggestion(
                &normalized,
                is_dir,
                home,
                &policy.excluded_prefixes,
                0,
            ),
            also_requires_fda,
            exists_on_disk: None,
        };
    };

    let in_depth = matched_root_prefix.depth;
    let shallow_apps = matched_root_prefix.shallow_apps;
    let matched_root_display = collapse_home_tilde_with(&matched_root_prefix.raw, home);

    let pierced_exclude_path = policy
        .excluded_prefixes
        .iter()
        .filter(|p| p.depth < in_depth && p.matches_str(&path_str))
        .max_by_key(|p| p.depth)
        .map(|p| collapse_home_tilde_with(&p.raw, home));

    if let Some(ex) = deepest_matching_prefix(&policy.excluded_prefixes, &path_str) {
        if ex.depth >= in_depth {
            let is_preset = default_exclude_paths
                .iter()
                .any(|def| paths_equal_ci(def, &ex.raw));
            return PathDiagnosis {
                normalized_path: normalized_display,
                status: PathDiagnosisStatus::ExcludedByPathPrefix,
                matched_root: Some(matched_root_display),
                matched_rule: Some(collapse_home_tilde_with(&ex.raw, home)),
                is_preset_rule: is_preset,
                pierced_exclude_path,
                symlink_target: None,
                suggested_index_path: compute_piercing_suggestion(
                    &normalized,
                    is_dir,
                    home,
                    &policy.excluded_prefixes,
                    ex.depth,
                ),
                also_requires_fda,
                exists_on_disk: None,
            };
        }
    }

    // 逐段检查相对路径组件；将显式目录名与凭据保护置前，避免 `.git`/`.ssh` 误导用户打开隐藏文件开关
    let all_comps: Vec<Component<'_>> = normalized.components().collect();
    let mut comp_path = PathBuf::new();
    for comp in &all_comps[..in_depth.min(all_comps.len())] {
        comp_path.push(comp.as_os_str());
    }

    for idx in in_depth..all_comps.len() {
        let comp = all_comps[idx];
        comp_path.push(comp.as_os_str());
        let Component::Normal(os_name) = comp else {
            continue;
        };
        let name = os_name.to_string_lossy().into_owned();
        let has_next = idx + 1 < all_comps.len();
        let comp_is_dir = has_next || is_dir;

        if policy.excluded_names.contains(&name) {
            let is_preset = defaults.index.exclude.iter().any(|e| e == &name);
            let suggestion = if comp_is_dir {
                compute_piercing_suggestion(&comp_path, true, home, &policy.excluded_prefixes, 0)
            } else {
                None
            };
            return PathDiagnosis {
                normalized_path: normalized_display,
                status: PathDiagnosisStatus::ExcludedByName,
                matched_root: Some(matched_root_display),
                matched_rule: Some(name),
                is_preset_rule: is_preset,
                pierced_exclude_path,
                symlink_target: None,
                suggested_index_path: suggestion,
                also_requires_fda,
                exists_on_disk: None,
            };
        }

        if policy.exclude_sensitive
            && policy
                .sensitive_names
                .iter()
                .any(|sensitive| sensitive.eq_ignore_ascii_case(&name))
        {
            let is_preset = defaults
                .index
                .sensitive_names
                .iter()
                .any(|sensitive| sensitive.eq_ignore_ascii_case(&name));
            let suggestion = if comp_is_dir {
                compute_piercing_suggestion(&comp_path, true, home, &policy.excluded_prefixes, 0)
            } else {
                None
            };
            return PathDiagnosis {
                normalized_path: normalized_display,
                status: PathDiagnosisStatus::ExcludedBySensitive,
                matched_root: Some(matched_root_display),
                matched_rule: Some(name),
                is_preset_rule: is_preset,
                pierced_exclude_path,
                symlink_target: None,
                suggested_index_path: suggestion,
                also_requires_fda,
                exists_on_disk: None,
            };
        }

        if !policy.include_hidden && name.starts_with('.') {
            let remaining_has_dot = all_comps[(idx + 1)..].iter().any(|c| match c {
                Component::Normal(seg) => seg.to_string_lossy().starts_with('.'),
                _ => false,
            });
            let suggestion = if comp_is_dir && !remaining_has_dot {
                compute_piercing_suggestion(&comp_path, true, home, &policy.excluded_prefixes, 0)
            } else {
                None
            };
            return PathDiagnosis {
                normalized_path: normalized_display,
                status: PathDiagnosisStatus::ExcludedByHidden,
                matched_root: Some(matched_root_display),
                matched_rule: Some(name),
                is_preset_rule: false,
                pierced_exclude_path,
                symlink_target: None,
                suggested_index_path: suggestion,
                also_requires_fda,
                exists_on_disk: None,
            };
        }

        if shallow_apps && is_app_bundle_name(&name) {
            if has_next {
                let bundle_display = collapse_home_tilde_with(&comp_path, home);
                return PathDiagnosis {
                    normalized_path: normalized_display,
                    status: PathDiagnosisStatus::ExcludedByShallowApp,
                    matched_root: Some(matched_root_display),
                    matched_rule: Some(bundle_display.clone()),
                    is_preset_rule: false,
                    pierced_exclude_path,
                    symlink_target: None,
                    suggested_index_path: Some(bundle_display),
                    also_requires_fda,
                    exists_on_disk: None,
                };
            }
            break;
        }

        if is_package_bundle_name(&name) {
            if has_next {
                let bundle_display = collapse_home_tilde_with(&comp_path, home);
                return PathDiagnosis {
                    normalized_path: normalized_display,
                    status: PathDiagnosisStatus::ExcludedByPackageBundle,
                    matched_root: Some(matched_root_display),
                    matched_rule: Some(bundle_display),
                    is_preset_rule: false,
                    pierced_exclude_path,
                    symlink_target: None,
                    suggested_index_path: None,
                    also_requires_fda,
                    exists_on_disk: None,
                };
            }
            break;
        }
    }

    if let Some(protected_dir) = matched_protected {
        return PathDiagnosis {
            normalized_path: normalized_display,
            status: PathDiagnosisStatus::ProtectedByFda,
            matched_root: Some(matched_root_display),
            matched_rule: Some(collapse_home_tilde_with(protected_dir, home)),
            is_preset_rule: false,
            pierced_exclude_path,
            symlink_target: None,
            suggested_index_path: None,
            also_requires_fda: false,
            exists_on_disk: None,
        };
    }

    PathDiagnosis {
        normalized_path: normalized_display,
        status: PathDiagnosisStatus::Indexed,
        matched_root: Some(matched_root_display),
        matched_rule: None,
        is_preset_rule: false,
        pierced_exclude_path,
        symlink_target: None,
        suggested_index_path: None,
        also_requires_fda: false,
        exists_on_disk: None,
    }
}

/// 运行期路径诊断：先避开失联外部卷做安全存在性与符号链接检测，再委托纯词法诊断引擎归因。
pub fn diagnose_path_runtime(
    raw_path: &str,
    config: &AppConfig,
    has_fda: bool,
    is_offline_volume: impl Fn(&Path) -> bool,
) -> PathDiagnosis {
    let cleaned = clean_diagnostic_path_input(raw_path);
    let expanded = expand_tilde(&cleaned);
    let normalized = normalize_lexical_pathbuf_resolved(&expanded);
    let home = dirs::home_dir();

    if is_offline_volume(&normalized) {
        let is_dir = normalized.extension().is_none();
        return diagnose_path_lexical(&normalized, is_dir, config, has_fda);
    }

    let protected_skip = !has_fda
        && crate::scanner::is_protected_or_descendant(&normalized, &crate::scanner::protected_paths());
    let (exists_on_disk, is_dir) = if protected_skip {
        (None, normalized.extension().is_none())
    } else {
        match std::fs::metadata(&normalized) {
            Ok(meta) => (Some(true), meta.is_dir()),
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                (None, normalized.extension().is_none())
            }
            Err(_) => (Some(false), normalized.extension().is_none()),
        }
    };

    let mut diag = diagnose_path_lexical(&normalized, is_dir, config, has_fda);
    diag.exists_on_disk = exists_on_disk;

    if diag.status == PathDiagnosisStatus::Indexed && exists_on_disk == Some(true) {
        let policy = IndexScopePolicy::from_config_lexical(config);
        let path_str = normalized.to_string_lossy();
        if let Some(matched_root) = deepest_matching_prefix(&policy.include_roots, &path_str) {
            let in_depth = matched_root.depth;
            let all_comps: Vec<Component<'_>> = normalized.components().collect();
            let mut prefix_path = PathBuf::new();
            for comp in &all_comps[..in_depth.min(all_comps.len())] {
                prefix_path.push(comp.as_os_str());
            }

            // 扫描器对非根目录启用 O_NOFOLLOW，仅在匹配根之后出现目录级符号链接时进行穿透复检
            for idx in in_depth..all_comps.len() {
                prefix_path.push(all_comps[idx].as_os_str());
                let is_symlink = std::fs::symlink_metadata(&prefix_path)
                    .is_ok_and(|m| m.file_type().is_symlink());
                if !is_symlink {
                    continue;
                }
                let has_next = idx + 1 < all_comps.len();
                let points_to_dir =
                    std::fs::metadata(&prefix_path).is_ok_and(|m| m.is_dir());
                if !has_next && !points_to_dir {
                    break;
                }

                let target_raw = std::fs::read_link(&prefix_path)
                    .map(|link_target| {
                        let joined = if link_target.is_absolute() {
                            link_target
                        } else {
                            prefix_path
                                .parent()
                                .unwrap_or_else(|| Path::new("/"))
                                .join(link_target)
                        };
                        normalize_lexical_pathbuf_resolved(&joined)
                    })
                    .unwrap_or_else(|_| {
                        std::fs::canonicalize(&prefix_path)
                            .unwrap_or_else(|_| prefix_path.clone())
                    });

                let mut resolved_full = target_raw.clone();
                for suffix_comp in &all_comps[(idx + 1)..] {
                    resolved_full.push(suffix_comp.as_os_str());
                }
                resolved_full = normalize_lexical_pathbuf_resolved(&resolved_full);

                let target_diag =
                    diagnose_path_lexical(&resolved_full, is_dir, config, has_fda);
                let target_display = collapse_home_tilde_with(&target_raw, home.as_deref());
                if target_diag.status == PathDiagnosisStatus::Indexed {
                    diag.symlink_target = Some(target_display);
                } else {
                    diag.status = PathDiagnosisStatus::SymlinkNotFollowed;
                    diag.matched_rule =
                        Some(collapse_home_tilde_with(&prefix_path, home.as_deref()));
                    diag.symlink_target = Some(target_display);
                    diag.suggested_index_path = compute_piercing_suggestion(
                        &resolved_full,
                        is_dir,
                        home.as_deref(),
                        &policy.excluded_prefixes,
                        0,
                    );
                }
                break;
            }
        }
    }

    diag
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
            network_rescan_enabled: false,
            network_rescan_minutes: 1440,
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
                network_rescan_enabled: false,
                network_rescan_minutes: 1440,
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

    #[test]
    fn test_diagnose_path_lexical_covers_cloud_storage_piercing_and_rules() {
        let home = dirs::home_dir().unwrap();
        let config = AppConfig::default();

        // 1. Synology Drive under ~/Library/CloudStorage -> ExcludedByPathPrefix("~/Library") + cloud root suggestion
        let synology_file = home.join("Library/CloudStorage/SynologyDrive-NAS/2026/合同.pdf");
        let diag = diagnose_path_lexical_with(
            &synology_file,
            false,
            &config,
            true,
            &[],
            Some(&home),
        );
        assert_eq!(diag.status, PathDiagnosisStatus::ExcludedByPathPrefix);
        assert_eq!(diag.matched_rule.as_deref(), Some("~/Library"));
        assert!(diag.is_preset_rule);
        assert_eq!(
            diag.suggested_index_path.as_deref(),
            Some("~/Library/CloudStorage/SynologyDrive-NAS")
        );

        // 2. iCloud Drive -> normalizes suggestion to iCloud root
        let icloud_doc = home.join("Library/Mobile Documents/com~apple~CloudDocs/Work/Doc.pages");
        let diag_icloud = diagnose_path_lexical_with(
            &icloud_doc,
            false,
            &config,
            true,
            &[],
            Some(&home),
        );
        assert_eq!(diag_icloud.status, PathDiagnosisStatus::ExcludedByPathPrefix);
        assert_eq!(
            diag_icloud.suggested_index_path.as_deref(),
            Some("~/Library/Mobile Documents/com~apple~CloudDocs")
        );

        // 3. ~/Library itself -> cannot pierce same-depth exclude
        let lib_dir = home.join("Library");
        let diag_lib = diagnose_path_lexical_with(&lib_dir, true, &config, true, &[], Some(&home));
        assert_eq!(diag_lib.status, PathDiagnosisStatus::ExcludedByPathPrefix);
        assert_eq!(diag_lib.suggested_index_path, None);

        // 4. Adding Synology Drive root pierces ~/Library
        let mut pierced_cfg = config.clone();
        pierced_cfg
            .index
            .index_paths
            .push("~/Library/CloudStorage/SynologyDrive-NAS".to_string());
        let diag_pierced = diagnose_path_lexical_with(
            &synology_file,
            false,
            &pierced_cfg,
            true,
            &[],
            Some(&home),
        );
        assert_eq!(diag_pierced.status, PathDiagnosisStatus::Indexed);
        assert_eq!(
            diag_pierced.matched_root.as_deref(),
            Some("~/Library/CloudStorage/SynologyDrive-NAS")
        );
        assert_eq!(diag_pierced.pierced_exclude_path.as_deref(), Some("~/Library"));

        // 5. Explicit exclude on cloud provider root falls back to deeper subpath suggestion
        let mut explicit_ex_cfg = config.clone();
        explicit_ex_cfg
            .index
            .exclude_paths
            .push("~/Library/CloudStorage/SynologyDrive-NAS".to_string());
        let diag_fallback = diagnose_path_lexical_with(
            &synology_file,
            false,
            &explicit_ex_cfg,
            true,
            &[],
            Some(&home),
        );
        assert_eq!(diag_fallback.status, PathDiagnosisStatus::ExcludedByPathPrefix);
        assert_eq!(
            diag_fallback.matched_rule.as_deref(),
            Some("~/Library/CloudStorage/SynologyDrive-NAS")
        );
        assert_eq!(
            diag_fallback.suggested_index_path.as_deref(),
            Some("~/Library/CloudStorage/SynologyDrive-NAS/2026")
        );

        // 6. Excluded directory name "out"
        let out_file = home.join("Work/out/report.pdf");
        let diag_out = diagnose_path_lexical_with(&out_file, false, &config, true, &[], Some(&home));
        assert_eq!(diag_out.status, PathDiagnosisStatus::ExcludedByName);
        assert_eq!(diag_out.matched_rule.as_deref(), Some("out"));
        assert!(diag_out.is_preset_rule);
        assert_eq!(diag_out.suggested_index_path.as_deref(), Some("~/Work/out"));

        // 7. .git with include_hidden=false reports ExcludedByName (not ExcludedByHidden)
        let git_cfg = home.join("proj/.git/config");
        let diag_git = diagnose_path_lexical_with(&git_cfg, false, &config, true, &[], Some(&home));
        assert_eq!(diag_git.status, PathDiagnosisStatus::ExcludedByName);
        assert_eq!(diag_git.matched_rule.as_deref(), Some(".git"));
        assert!(diag_git.is_preset_rule);

        // 8. .ssh with include_hidden=false reports ExcludedBySensitive
        let ssh_key = home.join(".ssh/id_rsa");
        let diag_ssh = diagnose_path_lexical_with(&ssh_key, false, &config, true, &[], Some(&home));
        assert_eq!(diag_ssh.status, PathDiagnosisStatus::ExcludedBySensitive);
        assert_eq!(diag_ssh.matched_rule.as_deref(), Some(".ssh"));
        assert!(diag_ssh.is_preset_rule);

        // 9. Hidden dir vs hidden file
        let nvim_cfg = home.join(".config/nvim/init.lua");
        let diag_hidden_dir =
            diagnose_path_lexical_with(&nvim_cfg, false, &config, true, &[], Some(&home));
        assert_eq!(diag_hidden_dir.status, PathDiagnosisStatus::ExcludedByHidden);
        assert_eq!(diag_hidden_dir.matched_rule.as_deref(), Some(".config"));
        assert_eq!(
            diag_hidden_dir.suggested_index_path.as_deref(),
            Some("~/.config")
        );

        let zshrc = home.join(".zshrc");
        let diag_hidden_file =
            diagnose_path_lexical_with(&zshrc, false, &config, true, &[], Some(&home));
        assert_eq!(diag_hidden_file.status, PathDiagnosisStatus::ExcludedByHidden);
        assert_eq!(diag_hidden_file.matched_rule.as_deref(), Some(".zshrc"));
        assert_eq!(diag_hidden_file.suggested_index_path, None);

        // 10. FDA protected path vs also_requires_fda
        let protected = vec![home.join("Documents"), home.join("Library/Containers")];
        let doc_file = home.join("Documents/a.pdf");
        let diag_fda = diagnose_path_lexical_with(
            &doc_file,
            false,
            &config,
            false,
            &protected,
            Some(&home),
        );
        assert_eq!(diag_fda.status, PathDiagnosisStatus::ProtectedByFda);
        assert_eq!(diag_fda.matched_rule.as_deref(), Some("~/Documents"));
        assert!(!diag_fda.also_requires_fda);

        let container_file = home.join("Library/Containers/WeWork/a.pdf");
        let diag_container = diagnose_path_lexical_with(
            &container_file,
            false,
            &config,
            false,
            &protected,
            Some(&home),
        );
        assert_eq!(diag_container.status, PathDiagnosisStatus::ExcludedByPathPrefix);
        assert!(diag_container.also_requires_fda);

        // 11. Empty index_paths -> OutsideIndexRoots
        let mut empty_cfg = config.clone();
        empty_cfg.index.index_paths.clear();
        let diag_outside = diagnose_path_lexical_with(
            &home.join("Work/notes.md"),
            false,
            &empty_cfg,
            true,
            &[],
            Some(&home),
        );
        assert_eq!(diag_outside.status, PathDiagnosisStatus::OutsideIndexRoots);
        assert_eq!(diag_outside.suggested_index_path.as_deref(), Some("~/Work"));
    }

    #[test]
    fn test_diagnose_path_runtime_detects_symlink_and_cleans_input() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("root");
        let blocked = tmp.path().join("blocked");
        let blocked_sub = blocked.join("sub");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&blocked_sub).unwrap();
        let target_file = blocked_sub.join("note.txt");
        fs::write(&target_file, "hello").unwrap();

        let link_path = root.join("link");
        std::os::unix::fs::symlink(&blocked_sub, &link_path).unwrap();

        let mut cfg = AppConfig::default();
        cfg.index.index_paths = vec![root.to_string_lossy().into_owned()];
        cfg.index.exclude_paths = vec![blocked.to_string_lossy().into_owned()];

        let via_link = link_path.join("note.txt");
        let quoted_file_url = format!(
            "\"file://{}\"",
            via_link.to_string_lossy().replace(' ', "%20")
        );
        let diag = diagnose_path_runtime(&quoted_file_url, &cfg, true, |_| false);
        assert_eq!(diag.status, PathDiagnosisStatus::SymlinkNotFollowed);
        assert_eq!(diag.exists_on_disk, Some(true));
        assert_eq!(
            diag.matched_rule.as_deref(),
            Some(link_path.to_string_lossy().as_ref())
        );
        assert_eq!(
            diag.symlink_target.as_deref(),
            Some(blocked_sub.to_string_lossy().as_ref())
        );
        assert_eq!(
            diag.suggested_index_path.as_deref(),
            Some(blocked_sub.to_string_lossy().as_ref())
        );

        // Adding the real target directory resolves SymlinkNotFollowed to Indexed
        cfg.index
            .index_paths
            .push(blocked_sub.to_string_lossy().into_owned());
        let diag_ok = diagnose_path_runtime(&via_link.to_string_lossy(), &cfg, true, |_| false);
        assert_eq!(diag_ok.status, PathDiagnosisStatus::Indexed);
        assert_eq!(
            diag_ok.symlink_target.as_deref(),
            Some(blocked_sub.to_string_lossy().as_ref())
        );

        // Offline volume path does not touch disk
        let diag_offline = diagnose_path_runtime("/Volumes/OfflineNAS/a.txt", &cfg, true, |_| true);
        assert_eq!(diag_offline.exists_on_disk, None);

        // Shell-escaped spaces are unescaped for manual input/paste
        assert_eq!(
            clean_diagnostic_path_input(r"~/Library/Mobile\ Documents/com~apple~CloudDocs"),
            "~/Library/Mobile Documents/com~apple~CloudDocs"
        );
    }

    #[test]
    fn test_package_bundles_are_shallow_containers_while_plain_key_files_index_normally() {
        let home = dirs::home_dir().unwrap();
        let config = AppConfig::default();
        let policy = IndexScopePolicy::from_config_lexical(&config);

        for bundle_name in [
            "Deck.key",
            "Budget.numbers",
            "Report.pages",
            "Notes.rtfd",
            "Photos.photoslibrary",
            "Movie.fcpbundle",
            "Song.logicx",
            "Track.band",
            "Demo.garageband",
        ] {
            let dir_path = home.join("Documents").join(bundle_name);
            let ctx = policy.dir_child_context(&home.join("Documents"), false);
            assert_eq!(
                policy.evaluate(&dir_path, true),
                ScopeDecision::IndexShallowContainer,
                "bundle dir {bundle_name} must be IndexShallowContainer"
            );
            assert_eq!(
                policy.evaluate_child(&ctx, &dir_path, bundle_name, true),
                ScopeDecision::IndexShallowContainer
            );
            assert_eq!(
                policy.evaluate(&dir_path.join("Index/Document.iwa"), false),
                ScopeDecision::HardDrop,
                "internal files inside {bundle_name} must be HardDrop"
            );
        }

        // Plain `.key` files (e.g. private key files) are not directories and must index normally
        let plain_key = home.join("Documents/id_ed25519.key");
        let ctx = policy.dir_child_context(&home.join("Documents"), false);
        assert_eq!(
            policy.evaluate(&plain_key, false),
            ScopeDecision::IndexNormally
        );
        assert_eq!(
            policy.evaluate_child(&ctx, &plain_key, "id_ed25519.key", false),
            ScopeDecision::IndexNormally
        );

        let bundle_self = home.join("Documents/Q4.pages");
        let diag_self =
            diagnose_path_lexical_with(&bundle_self, true, &config, true, &[], Some(&home));
        assert_eq!(diag_self.status, PathDiagnosisStatus::Indexed);

        let bundle_inner = bundle_self.join("Index/Document.iwa");
        let diag_inner =
            diagnose_path_lexical_with(&bundle_inner, false, &config, true, &[], Some(&home));
        assert_eq!(
            diag_inner.status,
            PathDiagnosisStatus::ExcludedByPackageBundle
        );
        assert_eq!(diag_inner.matched_rule.as_deref(), Some("~/Documents/Q4.pages"));
        assert_eq!(diag_inner.suggested_index_path, None);
    }
}
