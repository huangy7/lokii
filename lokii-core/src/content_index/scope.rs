use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use glob_match::glob_match;

use super::gram::{decode_plain_text, MAX_PLAIN_FILE_BYTES, MAX_REPO_FILE_BYTES};

/// APFS/BSD 云端占位文件标志位：设置该位的文件本体尚未下载至本地。
/// 来自 macOS SDK `<sys/stat.h>` (`SF_DATALESS`)。
pub const SF_DATALESS: u32 = 0x4000_0000;

/// 结构化数据文本（JSON / XML / CSV / TSV 等）默认体积上限（512 KiB）。
/// 这类文件经常出现数兆级自动生成样本，收敛上限可显著降低无效倒排膨胀。
pub const MAX_DATA_FILE_BYTES: u64 = 512 * 1024;

/// 富文档与图片文件索引默认体积上限（64 MiB）。
pub const MAX_RICH_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[cfg(target_os = "macos")]
const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: libc::c_int = 3;
#[cfg(target_os = "macos")]
const IOPOL_SCOPE_THREAD: libc::c_int = 1;
#[cfg(target_os = "macos")]
const IOPOL_MATERIALIZE_DATALESS_FILES_DEFAULT: libc::c_int = 0;
#[cfg(target_os = "macos")]
const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: libc::c_int = 1;

#[cfg(target_os = "macos")]
extern "C" {
    fn setiopolicy_np(
        iotype: libc::c_int,
        scope: libc::c_int,
        policy: libc::c_int,
    ) -> libc::c_int;
    fn malloc_zone_pressure_relief(zone: *mut libc::c_void, goal: libc::size_t) -> libc::size_t;
}

/// 为当前工作线程关闭无盘云端占位文件的自动物化，防止检索或索引阶段触发 iCloud 静默拉流。
pub fn set_thread_no_materialize() -> bool {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: macOS 系统调用参数均为值传递常量，仅作用于当前调用线程 I/O 策略。
        unsafe {
            setiopolicy_np(
                IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                IOPOL_SCOPE_THREAD,
                IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
            ) == 0
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// RAII 作用域守卫：进入时关闭当前线程云盘物化，退出时恢复默认策略。
pub struct NoMaterializeGuard {
    active: bool,
}

impl NoMaterializeGuard {
    pub fn enter() -> Self {
        let active = set_thread_no_materialize();
        Self { active }
    }
}

impl Drop for NoMaterializeGuard {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        if self.active {
            // SAFETY: 与 `set_thread_no_materialize` 配对恢复当前线程默认 I/O 策略。
            unsafe {
                let _ = setiopolicy_np(
                    IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                    IOPOL_SCOPE_THREAD,
                    IOPOL_MATERIALIZE_DATALESS_FILES_DEFAULT,
                );
            }
        }
    }
}

/// 在大批量建段或段归并完成后，建议系统分配器回收空闲页，抑制常驻 RSS 虚高。
pub fn release_allocator_memory() -> usize {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: 传入空指针指示释放全部分配区可回收页，goal=0 表示尽力回收。
        unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) }
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

/// 判断元数据是否标记为云端未下载占位文件。
#[inline]
pub fn is_dataless(meta: &fs::Metadata) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        (meta.st_flags() & SF_DATALESS) != 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = meta;
        false
    }
}

/// 基于 `symlink_metadata` 检查路径是否指向云端未物化占位文件。
pub fn path_is_dataless(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| is_dataless(&m))
        .unwrap_or(false)
}

/// 内容索引支持的文档来源大类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum ContentSourceKind {
    PlainText = 0,
    OfficeXml = 1,
    Pdf = 2,
    Image = 3,
}

/// 路径评估结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeDecision {
    Include(ContentSourceKind),
    Exclude(ExcludeReason),
}

/// 排除原因枚举，便于单元测试与统计遥测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExcludeReason {
    Dataless,
    IgnoredByRule,
    SkippedDir,
    MarkedDir,
    SkippedExtensionOrName,
    MinifiedOrLog,
    Lockfile,
    DisabledSourceKind,
    ExceedsMaxSize,
    NonRegularFile,
    BinaryOrLongLine,
}

/// 作用域策略配置。
#[derive(Debug, Clone)]
pub struct ScopeConfig {
    pub index_plain_text: bool,
    pub index_office: bool,
    pub index_pdf: bool,
    pub index_images: bool,
    pub respect_gitignore: bool,
    pub respect_ignore_files: bool,
    pub max_plain_file_bytes: u64,
    pub max_repo_file_bytes: u64,
    pub max_data_file_bytes: u64,
    pub max_rich_file_bytes: u64,
    pub global_gitignore_path: Option<PathBuf>,
    pub extra_exclude_dirs: Vec<String>,
}

impl Default for ScopeConfig {
    fn default() -> Self {
        Self {
            index_plain_text: true,
            index_office: true,
            index_pdf: true,
            index_images: false,
            respect_gitignore: true,
            respect_ignore_files: true,
            max_plain_file_bytes: MAX_PLAIN_FILE_BYTES,
            max_repo_file_bytes: MAX_REPO_FILE_BYTES,
            max_data_file_bytes: MAX_DATA_FILE_BYTES,
            max_rich_file_bytes: MAX_RICH_FILE_BYTES,
            global_gitignore_path: default_global_gitignore_path(),
            extra_exclude_dirs: Vec::new(),
        }
    }
}

fn default_global_gitignore_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let xdg = dirs::config_dir()
        .map(|d| d.join("git/ignore"))
        .filter(|p| p.is_file());
    if xdg.is_some() {
        return xdg;
    }
    let fallback = home.join(".gitignore_global");
    fallback.is_file().then_some(fallback)
}

/// 默认跳过的依赖、构建产物、版本控制与缓存目录名称。
const SKIP_DIR_NAMES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".bzr",
    "node_modules",
    "target",
    "DerivedData",
    ".build",
    "Pods",
    "Carthage",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".next",
    ".nuxt",
    ".turbo",
    ".parcel-cache",
    ".svelte-kit",
    ".gradle",
    ".cache",
    ".cargo",
    ".rustup",
    ".npm",
    ".pnpm-store",
    ".yarn",
    ".Spotlight-V100",
    ".fseventsd",
    ".Trashes",
    ".Trash",
    "xcuserdata",
    "xcshareddata",
];

/// macOS 束与包目录扩展名：其内部大多包含海量副本资源或已编译二进制，不应进入全文倒排。
const SKIP_DIR_EXTENSIONS: &[&str] = &[
    "app",
    "framework",
    "dsym",
    "xcassets",
    "bundle",
    "xcodeproj",
    "xcworkspace",
    "kext",
    "plugin",
    "qlgenerator",
    "photoslibrary",
    "sparsebundle",
];

/// 目录跳过标记文件：凡目录下存在其中任一标记文件即跳过整棵子树。
const DIR_MARKER_FILES: &[&str] = &[
    "CACHEDIR.TAG",
    ".nobackup",
    ".metadata_never_index",
    ".lokiiignore",
];

/// 常见锁文件精确文件名（小写对比）。
const LOCKFILE_NAMES: &[&str] = &[
    "cargo.lock",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lock",
    "bun.lockb",
    "gemfile.lock",
    "podfile.lock",
    "poetry.lock",
    "uv.lock",
    "pipfile.lock",
    "composer.lock",
    "flake.lock",
    "package.resolved",
    "mix.lock",
    "pubspec.lock",
    "go.sum",
];

/// 结构化数据扩展名：需适用更严格的 `max_data_file_bytes` 上限。
const DATA_EXTENSIONS: &[&str] = &[
    "json", "jsonc", "json5", "jsonl", "ndjson", "xml", "plist", "csv", "tsv",
];

/// 已知可索引的纯文本与源码扩展名（全小写）。
const TEXT_EXTENSIONS: &[&str] = &[
    // 系统与应用语言
    "rs", "swift", "c", "h", "cc", "cpp", "cxx", "hh", "hpp", "hxx", "m", "mm", "go", "zig", "nim",
    "d", // 脚本与动态语言
    "py", "pyi", "rb", "php", "pl", "pm", "lua", "r", "jl", "tcl", "awk", "perl", "sh", "bash",
    "zsh", "fish", "ksh", "ps1", "psm1", "bat", "cmd", // Web 与前端
    "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "html", "htm", "xhtml", "css", "scss",
    "sass", "less", "styl", "vue", "svelte", "astro", // JVM / .NET / 函数式
    "java", "kt", "kts", "scala", "sbt", "groovy", "gradle", "cs", "fs", "fsx", "vb", "clj",
    "cljs", "cljc", "edn", "ex", "exs", "erl", "hrl", "hs", "lhs", "ml", "mli", "elm", "purs",
    "dart", // 配置、构建与 DSL
    "toml", "yaml", "yml", "ini", "conf", "cfg", "env", "properties", "nix", "dhall", "cue",
    "json", "jsonc", "json5", "jsonl", "ndjson", "xml", "plist", "proto", "graphql", "gql",
    "sql", "prisma", "tf", "tfvars", "hcl", "cmake", "mk", "mak", "bazel", "bzl", "starlark",
    "dockerfile", "containerfile", // 文档与文本数据
    "md", "mdx", "markdown", "txt", "rst", "org", "adoc", "tex", "bib", "csv", "tsv", "diff",
    "patch", "srt", "vtt", "gitignore", "gitattributes", "editorconfig",
];

/// 无扩展名或点开头常见文本文件名（全小写对比）。
const TEXT_FILENAMES: &[&str] = &[
    "makefile",
    "gnumakefile",
    "dockerfile",
    "containerfile",
    "justfile",
    "rakefile",
    "gemfile",
    "podfile",
    "fastfile",
    "vagrantfile",
    "procfile",
    "brewfile",
    "cmakelists.txt",
    "readme",
    "license",
    "licence",
    "copying",
    "changelog",
    "authors",
    "contributors",
    "notice",
    "todo",
    ".gitignore",
    ".gitattributes",
    ".gitmodules",
    ".ignore",
    ".lokiiignore",
    ".dockerignore",
    ".editorconfig",
    ".prettierrc",
    ".eslintrc",
    ".babelrc",
    ".npmrc",
    ".yarnrc",
    ".env",
    ".zshrc",
    ".bashrc",
    ".profile",
    ".zprofile",
];

/// 明确排除的非文本/构建产物/压缩/媒体扩展名（全小写）。
const SKIP_EXTENSIONS: &[&str] = &[
    "o", "a", "so", "dylib", "dll", "exe", "bin", "class", "pyc", "pyo", "wasm", "node", "rlib",
    "rmeta", "zip", "tar", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "dmg", "pkg", "iso",
    "jar", "war", "ear", "ipa", "apk", "aab", "mp3", "mp4", "mov", "mkv", "avi", "wav", "flac",
    "aac", "ogg", "m4a", "m4v", "webm", "woff", "woff2", "ttf", "otf", "eot", "icns", "ico",
    "bmp", "gif", "psd", "ai", "sketch", "fig", "blend", "svg", "db", "sqlite", "sqlite3",
    "realm", "lmdb", "pack", "idx", "map", "tsbuildinfo", "pb", "dat", "pak", "vmdk", "qcow2",
    "log",
];

#[derive(Debug, Clone)]
struct IgnoreRule {
    negated: bool,
    dir_only: bool,
    anchored: bool,
    pattern: String,
    base_dir: PathBuf,
}

impl IgnoreRule {
    fn parse(raw_line: &str, base_dir: &Path) -> Option<Self> {
        let line = trim_unescaped_trailing_spaces(raw_line);
        if line.is_empty() {
            return None;
        }
        if line.starts_with('#') {
            return None;
        }

        let (negated, after_bang) = if let Some(rest) = line.strip_prefix('!') {
            (true, rest)
        } else if let Some(rest) = line.strip_prefix("\\!") {
            (false, rest)
        } else if let Some(rest) = line.strip_prefix("\\#") {
            // `\ #` 转义以 `#` 字面量开头；重新拼回 `#...`
            return Some(Self::from_body(
                false,
                format!("#{}", rest),
                base_dir.to_path_buf(),
            ));
        } else {
            (false, line)
        };

        if after_bang.is_empty() {
            return None;
        }

        Some(Self::from_body(
            negated,
            after_bang.to_string(),
            base_dir.to_path_buf(),
        ))
    }

    fn from_body(negated: bool, mut body: String, base_dir: PathBuf) -> Self {
        let dir_only = body.ends_with('/') && !body.ends_with("\\/");
        if dir_only {
            body.pop();
        }
        let anchored = if let Some(stripped) = body.strip_prefix('/') {
            body = stripped.to_string();
            true
        } else {
            body.contains('/')
        };
        let pattern = unescape_ignore_pattern(&body);
        Self {
            negated,
            dir_only,
            anchored,
            pattern,
            base_dir,
        }
    }

    fn matches(&self, target_path: &Path, entry_name: &str, is_dir: bool) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }
        if !self.anchored {
            return glob_match(&self.pattern, entry_name);
        }
        let Ok(rel) = target_path.strip_prefix(&self.base_dir) else {
            return false;
        };
        let rel_str = rel.to_string_lossy();
        #[cfg(windows)]
        let rel_norm = rel_str.replace('\\', "/");
        #[cfg(not(windows))]
        let rel_norm = rel_str.as_ref();

        if glob_match(&self.pattern, rel_norm) {
            return true;
        }
        // 处理 Gitignore 中 `a/**/b` 需要同时匹配零层目录 `a/b` 的惯例。
        if self.pattern.contains("/**/") {
            let collapsed = self.pattern.replace("/**/", "/");
            if glob_match(&collapsed, rel_norm) {
                return true;
            }
        }
        if let Some(rest) = self.pattern.strip_prefix("**/") {
            if glob_match(rest, rel_norm) {
                return true;
            }
        }
        false
    }
}

fn trim_unescaped_trailing_spaces(s: &str) -> &str {
    let s = s.trim_end_matches(['\r', '\n']);
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == b' ' {
        if end >= 2 && bytes[end - 2] == b'\\' {
            break;
        }
        end -= 1;
    }
    &s[..end]
}

fn unescape_ignore_pattern(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                if matches!(next, ' ' | '#' | '!' | '\\') {
                    out.push(next);
                    chars.next();
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

#[derive(Debug, Clone, Default)]
struct DirMeta {
    has_git_marker: bool,
    has_skip_marker: bool,
    gitignore_rules: Vec<IgnoreRule>,
    git_exclude_rules: Vec<IgnoreRule>,
    dot_ignore_rules: Vec<IgnoreRule>,
}

/// 具备目录缓存的内容检索作用域评估器。
/// 内部对目录标记（`.git`、`CACHEDIR.TAG`）与 ignore 规则做记忆化，避免大树遍历重复磁盘 stat/read。
pub struct ScopeEvaluator {
    config: ScopeConfig,
    extra_skip_dirs: HashSet<String>,
    dir_cache: HashMap<PathBuf, DirMeta>,
    global_rules: Option<Vec<IgnoreRule>>,
}

impl ScopeEvaluator {
    pub fn new(config: ScopeConfig) -> Self {
        let extra_skip_dirs = config
            .extra_exclude_dirs
            .iter()
            .map(|s| s.to_ascii_lowercase())
            .collect();
        Self {
            config,
            extra_skip_dirs,
            dir_cache: HashMap::new(),
            global_rules: None,
        }
    }

    pub fn config(&self) -> &ScopeConfig {
        &self.config
    }

    /// 针对目录快速剪枝：命中跳过目录或标记文件时返回 `Exclude(...)`。
    pub fn should_enter_dir(&mut self, dir: &Path) -> Result<(), ExcludeReason> {
        if path_is_dataless(dir) {
            return Err(ExcludeReason::Dataless);
        }
        if is_skipped_dir_path(dir, &self.extra_skip_dirs) {
            return Err(ExcludeReason::SkippedDir);
        }
        if self.has_skip_marker(dir) {
            return Err(ExcludeReason::MarkedDir);
        }
        if self.is_ignored_path(dir, true) {
            return Err(ExcludeReason::IgnoredByRule);
        }
        Ok(())
    }

    /// 依据路径、目录上下文、ignore 规则与文件元数据判定是否纳入内容索引。
    /// 该阶段不读取文件正文（仅读祖先 ignore 与 `symlink_metadata`），可安全过滤云端占位与超大文件。
    pub fn evaluate_path(&mut self, path: &Path) -> ScopeDecision {
        let Ok(meta) = fs::symlink_metadata(path) else {
            return ScopeDecision::Exclude(ExcludeReason::NonRegularFile);
        };
        self.evaluate_path_with_meta(path, &meta)
    }

    /// 已有 `Metadata` 时的快速评估入口，适合增量批次与并行过滤前预处理。
    pub fn evaluate_path_with_meta(
        &mut self,
        path: &Path,
        meta: &fs::Metadata,
    ) -> ScopeDecision {
        if !meta.file_type().is_file() {
            return ScopeDecision::Exclude(ExcludeReason::NonRegularFile);
        }
        if is_dataless(meta) {
            return ScopeDecision::Exclude(ExcludeReason::Dataless);
        }

        // 沿目录链快速校验跳过目录与标记文件。
        if let Some(parent) = path.parent() {
            for anc in parent.ancestors() {
                if anc.as_os_str().is_empty() {
                    break;
                }
                if is_skipped_dir_path(anc, &self.extra_skip_dirs) {
                    return ScopeDecision::Exclude(ExcludeReason::SkippedDir);
                }
                if self.has_skip_marker(anc) {
                    return ScopeDecision::Exclude(ExcludeReason::MarkedDir);
                }
                if self.dir_has_git(anc) {
                    // 抵达当前仓根后无需继续向上扫描外部不相关缓存标记。
                    break;
                }
            }
        }

        if self.is_ignored_path(path, false) {
            return ScopeDecision::Exclude(ExcludeReason::IgnoredByRule);
        }

        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return ScopeDecision::Exclude(ExcludeReason::SkippedExtensionOrName);
        };
        let lower_name = name.to_ascii_lowercase();

        if is_lockfile_name(&lower_name) {
            return ScopeDecision::Exclude(ExcludeReason::Lockfile);
        }
        if is_minified_or_log_name(&lower_name) {
            return ScopeDecision::Exclude(ExcludeReason::MinifiedOrLog);
        }

        let kind = match classify_source_kind(&lower_name) {
            Some(k) => k,
            None => return ScopeDecision::Exclude(ExcludeReason::SkippedExtensionOrName),
        };

        if !self.is_kind_enabled(kind) {
            return ScopeDecision::Exclude(ExcludeReason::DisabledSourceKind);
        }

        let in_git_repo = path
            .parent()
            .map(|p| self.find_git_repo_root(p).is_some())
            .unwrap_or(false);
        let max_bytes = max_bytes_for_kind(&self.config, kind, &lower_name, in_git_repo);
        if meta.len() > max_bytes {
            return ScopeDecision::Exclude(ExcludeReason::ExceedsMaxSize);
        }

        ScopeDecision::Include(kind)
    }

    /// 验证已读入的纯文本字节流是否可通过编码/二进制/长行拦截。
    pub fn accept_plain_bytes<'a>(&self, raw: &'a [u8]) -> Result<std::borrow::Cow<'a, [u8]>, ExcludeReason> {
        decode_plain_text(raw).ok_or(ExcludeReason::BinaryOrLongLine)
    }

    fn is_kind_enabled(&self, kind: ContentSourceKind) -> bool {
        match kind {
            ContentSourceKind::PlainText => self.config.index_plain_text,
            ContentSourceKind::OfficeXml => self.config.index_office,
            ContentSourceKind::Pdf => self.config.index_pdf,
            ContentSourceKind::Image => self.config.index_images,
        }
    }

    fn ensure_dir_meta(&mut self, dir: &Path) -> &DirMeta {
        if !self.dir_cache.contains_key(dir) {
            let meta = load_dir_meta(dir, &self.config);
            self.dir_cache.insert(dir.to_path_buf(), meta);
        }
        &self.dir_cache[dir]
    }

    fn dir_has_git(&mut self, dir: &Path) -> bool {
        self.ensure_dir_meta(dir).has_git_marker
    }

    fn has_skip_marker(&mut self, dir: &Path) -> bool {
        self.ensure_dir_meta(dir).has_skip_marker
    }

    fn find_git_repo_root(&mut self, start_dir: &Path) -> Option<PathBuf> {
        for anc in start_dir.ancestors() {
            if anc.as_os_str().is_empty() {
                break;
            }
            if self.dir_has_git(anc) {
                return Some(anc.to_path_buf());
            }
        }
        None
    }

    fn is_ignored_path(&mut self, target: &Path, target_is_dir: bool) -> bool {
        if !self.config.respect_gitignore && !self.config.respect_ignore_files {
            return false;
        }
        let Some(parent) = target.parent() else {
            return false;
        };

        // 确定祖先规则收敛边界：若命中最近 `.git` 仓库根则止步于此，防止外层规则或其它仓库 `.git` 越界穿透。
        let mut chain: Vec<PathBuf> = Vec::new();
        for anc in parent.ancestors() {
            if anc.as_os_str().is_empty() {
                break;
            }
            chain.push(anc.to_path_buf());
            if self.dir_has_git(anc) {
                break;
            }
        }
        chain.reverse(); // 自顶向下应用规则，内层与后写规则拥有更高优先级。

        let scope_root = chain.first().cloned().unwrap_or_else(|| parent.to_path_buf());
        let mut rules: Vec<IgnoreRule> = Vec::new();

        if self.config.respect_gitignore {
            if self.global_rules.is_none() {
                let loaded = self
                    .config
                    .global_gitignore_path
                    .as_deref()
                    .and_then(|p| parse_ignore_file(p, &scope_root))
                    .unwrap_or_default();
                self.global_rules = Some(loaded);
            }
            if let Some(ref g) = self.global_rules {
                // 全局 ignore 的 base_dir 在评估时动态映射到当前 scope_root。
                for r in g {
                    let mut scoped = r.clone();
                    scoped.base_dir = scope_root.clone();
                    rules.push(scoped);
                }
            }
        }

        for dir in &chain {
            let meta = self.ensure_dir_meta(dir).clone();
            if self.config.respect_gitignore {
                rules.extend(meta.git_exclude_rules);
                rules.extend(meta.gitignore_rules);
            }
            if self.config.respect_ignore_files {
                rules.extend(meta.dot_ignore_rules);
            }
        }

        if rules.is_empty() {
            return false;
        }

        // 先逐级校验中间父目录（若某父目录本身被规则忽略，则其内部内容不再纳入索引）。
        if let Ok(rel_from_root) = target.strip_prefix(&scope_root) {
            let comps: Vec<_> = rel_from_root.components().collect();
            let mut cur = scope_root.clone();
            for (idx, comp) in comps.iter().enumerate() {
                cur.push(comp.as_os_str());
                let is_last = idx + 1 == comps.len();
                let is_dir = if is_last { target_is_dir } else { true };
                let Some(name) = cur.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                let mut ignored = false;
                for rule in &rules {
                    if rule.matches(&cur, name, is_dir) {
                        ignored = !rule.negated;
                    }
                }
                if !is_last && ignored {
                    return true;
                }
                if is_last {
                    return ignored;
                }
            }
        }

        false
    }
}

fn load_dir_meta(dir: &Path, config: &ScopeConfig) -> DirMeta {
    let git_path = dir.join(".git");
    let has_git_marker = git_path.exists();

    let has_skip_marker = DIR_MARKER_FILES
        .iter()
        .any(|marker| dir.join(marker).exists());

    let git_exclude_rules = if has_git_marker && config.respect_gitignore && git_path.is_dir() {
        parse_ignore_file(&git_path.join("info/exclude"), dir).unwrap_or_default()
    } else {
        Vec::new()
    };

    let gitignore_rules = if config.respect_gitignore {
        parse_ignore_file(&dir.join(".gitignore"), dir).unwrap_or_default()
    } else {
        Vec::new()
    };

    let dot_ignore_rules = if config.respect_ignore_files {
        parse_ignore_file(&dir.join(".ignore"), dir).unwrap_or_default()
    } else {
        Vec::new()
    };

    DirMeta {
        has_git_marker,
        has_skip_marker,
        gitignore_rules,
        git_exclude_rules,
        dot_ignore_rules,
    }
}

fn parse_ignore_file(file_path: &Path, base_dir: &Path) -> Option<Vec<IgnoreRule>> {
    let content = fs::read_to_string(file_path).ok()?;
    let rules = content
        .lines()
        .filter_map(|line| IgnoreRule::parse(line, base_dir))
        .collect();
    Some(rules)
}

fn is_skipped_dir_path(dir: &Path, extra: &HashSet<String>) -> bool {
    let Some(name) = dir.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    if SKIP_DIR_NAMES.contains(&name) {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    if extra.contains(&lower) {
        return true;
    }
    if let Some((_, ext)) = lower.rsplit_once('.') {
        if SKIP_DIR_EXTENSIONS.contains(&ext) {
            return true;
        }
    }
    false
}

fn is_lockfile_name(lower_name: &str) -> bool {
    LOCKFILE_NAMES.contains(&lower_name)
        || (lower_name.ends_with(".lock") && lower_name != "sys.lock")
}

fn is_minified_or_log_name(lower_name: &str) -> bool {
    lower_name.ends_with(".min.js")
        || lower_name.ends_with(".min.css")
        || lower_name.ends_with(".min.mjs")
        || lower_name.ends_with("-min.js")
        || lower_name.ends_with("-min.css")
        || lower_name.ends_with(".bundle.js")
        || lower_name.ends_with(".chunk.js")
        || lower_name.ends_with(".d.ts.map")
        || lower_name.ends_with(".log")
        || lower_name.contains(".log.")
}

/// 仅依据小写文件名分类受支持的内容类型；未知扩展与明确二进制返回 `None`。
pub fn classify_source_kind(lower_name: &str) -> Option<ContentSourceKind> {
    if let Some((_, ext)) = lower_name.rsplit_once('.') {
        match ext {
            "docx" | "xlsx" | "pptx" => return Some(ContentSourceKind::OfficeXml),
            "pdf" => return Some(ContentSourceKind::Pdf),
            "png" | "jpg" | "jpeg" | "webp" | "tif" | "tiff" | "heic" => {
                return Some(ContentSourceKind::Image);
            }
            _ => {}
        }
        if SKIP_EXTENSIONS.contains(&ext) {
            return None;
        }
        if TEXT_EXTENSIONS.contains(&ext) {
            return Some(ContentSourceKind::PlainText);
        }
    }
    if TEXT_FILENAMES.contains(&lower_name) {
        return Some(ContentSourceKind::PlainText);
    }
    None
}

fn max_bytes_for_kind(
    config: &ScopeConfig,
    kind: ContentSourceKind,
    lower_name: &str,
    in_git_repo: bool,
) -> u64 {
    match kind {
        ContentSourceKind::PlainText => {
            if let Some((_, ext)) = lower_name.rsplit_once('.') {
                if DATA_EXTENSIONS.contains(&ext) {
                    return config.max_data_file_bytes;
                }
            }
            if in_git_repo {
                config.max_repo_file_bytes
            } else {
                config.max_plain_file_bytes
            }
        }
        ContentSourceKind::OfficeXml | ContentSourceKind::Pdf | ContentSourceKind::Image => {
            config.max_rich_file_bytes
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_file(path: &Path, content: &[u8]) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        let mut f = fs::File::create(path).unwrap();
        f.write_all(content).unwrap();
    }

    #[test]
    fn gitignore_negation_and_git_repo_isolation() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // /root/repo 内部拥有 .gitignore 与 .git/info/exclude
        let repo = root.join("repo");
        write_file(&repo.join(".git/HEAD"), b"ref: refs/heads/main\n");
        write_file(&repo.join(".git/info/exclude"), b"*.secret\n");
        write_file(&repo.join(".gitignore"), b"*.txt\n!important.txt\nbuild/\n");
        write_file(&repo.join("ignored.txt"), b"hello\n");
        write_file(&repo.join("important.txt"), b"keep me\n");
        write_file(&repo.join("nested/important.txt"), b"also keep\n");
        write_file(&repo.join("key.secret"), b"abc\n");
        write_file(&repo.join("build/out.rs"), b"fn x() {}\n");

        // 仓库外部同目录不应受 repo/.gitignore 与 repo/.git/info/exclude 穿透
        let outside = root.join("outside");
        write_file(&outside.join("ignored.txt"), b"outside text\n");
        write_file(&outside.join("key.secret.txt"), b"outside secret\n");

        // 嵌套子仓库应受自身 .git 边界隔离，不被外层 repo/.gitignore 误伤
        let subrepo = repo.join("subrepo");
        write_file(&subrepo.join(".git/HEAD"), b"ref: refs/heads/main\n");
        write_file(&subrepo.join("notes.txt"), b"inside subrepo\n");

        let mut eval = ScopeEvaluator::new(ScopeConfig {
            global_gitignore_path: None,
            ..Default::default()
        });

        assert_eq!(
            eval.evaluate_path(&repo.join("ignored.txt")),
            ScopeDecision::Exclude(ExcludeReason::IgnoredByRule)
        );
        assert_eq!(
            eval.evaluate_path(&repo.join("important.txt")),
            ScopeDecision::Include(ContentSourceKind::PlainText)
        );
        assert_eq!(
            eval.evaluate_path(&repo.join("nested/important.txt")),
            ScopeDecision::Include(ContentSourceKind::PlainText)
        );
        assert_eq!(
            eval.evaluate_path(&repo.join("build/out.rs")),
            ScopeDecision::Exclude(ExcludeReason::IgnoredByRule)
        );
        assert_eq!(
            eval.evaluate_path(&outside.join("ignored.txt")),
            ScopeDecision::Include(ContentSourceKind::PlainText)
        );
        assert_eq!(
            eval.evaluate_path(&outside.join("key.secret.txt")),
            ScopeDecision::Include(ContentSourceKind::PlainText)
        );
        assert_eq!(
            eval.evaluate_path(&subrepo.join("notes.txt")),
            ScopeDecision::Include(ContentSourceKind::PlainText)
        );
    }

    #[test]
    fn skips_dependency_target_cachedir_lock_minified_and_oversized_json() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        write_file(&root.join("node_modules/pkg/index.js"), b"export const a = 1;\n");
        write_file(&root.join("target/debug/gen.rs"), b"fn gen() {}\n");
        write_file(&root.join("cache_dir/CACHEDIR.TAG"), b"Signature: 8a477f597d28d172789f06886806bc55\n");
        write_file(&root.join("cache_dir/item.txt"), b"cached text\n");
        write_file(&root.join("Cargo.lock"), b"# lock\n");
        write_file(&root.join("dist_app/foo.min.js"), b"var a=1;\n");
        write_file(&root.join("server.log"), b"INFO ok\n");
        write_file(&root.join("huge.json"), &vec![b' '; (MAX_DATA_FILE_BYTES + 10) as usize]);
        write_file(&root.join("small.json"), b"{\"ok\": true}\n");
        write_file(&root.join("src/main.rs"), b"fn main() {}\n");

        let mut eval = ScopeEvaluator::new(ScopeConfig {
            global_gitignore_path: None,
            ..Default::default()
        });

        assert_eq!(
            eval.evaluate_path(&root.join("node_modules/pkg/index.js")),
            ScopeDecision::Exclude(ExcludeReason::SkippedDir)
        );
        assert_eq!(
            eval.evaluate_path(&root.join("target/debug/gen.rs")),
            ScopeDecision::Exclude(ExcludeReason::SkippedDir)
        );
        assert_eq!(
            eval.evaluate_path(&root.join("cache_dir/item.txt")),
            ScopeDecision::Exclude(ExcludeReason::MarkedDir)
        );
        assert_eq!(
            eval.evaluate_path(&root.join("Cargo.lock")),
            ScopeDecision::Exclude(ExcludeReason::Lockfile)
        );
        assert_eq!(
            eval.evaluate_path(&root.join("dist_app/foo.min.js")),
            ScopeDecision::Exclude(ExcludeReason::MinifiedOrLog)
        );
        assert_eq!(
            eval.evaluate_path(&root.join("server.log")),
            ScopeDecision::Exclude(ExcludeReason::MinifiedOrLog)
        );
        assert_eq!(
            eval.evaluate_path(&root.join("huge.json")),
            ScopeDecision::Exclude(ExcludeReason::ExceedsMaxSize)
        );
        assert_eq!(
            eval.evaluate_path(&root.join("small.json")),
            ScopeDecision::Include(ContentSourceKind::PlainText)
        );
        assert_eq!(
            eval.evaluate_path(&root.join("src/main.rs")),
            ScopeDecision::Include(ContentSourceKind::PlainText)
        );

        // NUL 二进制与长行压缩文本拦截校验
        assert_eq!(
            eval.accept_plain_bytes(b"hello\x00world"),
            Err(ExcludeReason::BinaryOrLongLine)
        );
        let minified = "x".repeat(5000);
        assert_eq!(
            eval.accept_plain_bytes(minified.as_bytes()),
            Err(ExcludeReason::BinaryOrLongLine)
        );
        assert!(eval.accept_plain_bytes(b"fn normal_code() {}\n").is_ok());
    }

    #[test]
    fn thread_iopolicy_and_pressure_relief_do_not_fail() {
        let _guard = NoMaterializeGuard::enter();
        assert!(set_thread_no_materialize());
        let _ = release_allocator_memory();
    }
}
