use crate::compact_index::{CompactIndex, CompactRecord, EntryView};

/// 文件分类枚举，用于在检索结果中按文件类型快速过滤。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileType {
    /// 不限制文件类型（包含目录与所有文件）
    #[default]
    All,
    /// 文档类文件（PDF、Office、文本与标记语言等）
    Document,
    /// 源代码与脚本文件
    Code,
    /// 图像文件
    Image,
    /// 音视频多媒体文件
    Media,
    /// 压缩包与磁盘镜像文件
    Archive,
    /// 仅目录
    Folder,
}

impl FileType {
    /// 根据名称解析预设文件分类（如 "doc"、"code"、"image" 等）。
    pub fn from_category(cat: &str) -> Option<Self> {
        if cat.eq_ignore_ascii_case("all") {
            Some(Self::All)
        } else if cat.eq_ignore_ascii_case("doc") || cat.eq_ignore_ascii_case("document") {
            Some(Self::Document)
        } else if cat.eq_ignore_ascii_case("code") || cat.eq_ignore_ascii_case("dev") {
            Some(Self::Code)
        } else if cat.eq_ignore_ascii_case("img")
            || cat.eq_ignore_ascii_case("image")
            || cat.eq_ignore_ascii_case("pic")
        {
            Some(Self::Image)
        } else if cat.eq_ignore_ascii_case("media")
            || cat.eq_ignore_ascii_case("video")
            || cat.eq_ignore_ascii_case("audio")
        {
            Some(Self::Media)
        } else if cat.eq_ignore_ascii_case("archive") || cat.eq_ignore_ascii_case("zip") {
            Some(Self::Archive)
        } else if cat.eq_ignore_ascii_case("folder") || cat.eq_ignore_ascii_case("dir") {
            Some(Self::Folder)
        } else {
            None
        }
    }

    /// 判断给定的文件属性与扩展名是否匹配当前类型过滤规则。
    ///
    /// 匹配规则：
    /// - `All`：匹配所有项目（包括目录与各类文件）。
    /// - `Folder`：仅当 `is_dir` 为 true 时匹配。
    /// - 其它具体文件类别（`Document`, `Code`, `Image`, `Media`, `Archive`）：
    ///   排斥所有目录（若 `is_dir` 为 true 直接返回 false），
    ///   仅当扩展名属于对应类型的白名单时返回 true。
    #[inline]
    pub fn matches(&self, is_dir: bool, ext: &str) -> bool {
        match self {
            Self::All => true,
            Self::Folder => is_dir,
            _ if is_dir => false,
            Self::Document => matches!(
                ext,
                "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md"
                    | "markdown" | "rtf" | "pages" | "numbers" | "key" | "keynote" | "csv" | "epub" | "mobi" | "odt"
            ),
            Self::Code => matches!(
                ext,
                "rs" | "swift" | "c" | "cpp" | "h" | "hpp" | "m" | "mm" | "go" | "py"
                    | "js" | "ts" | "jsx" | "tsx" | "vue" | "svelte" | "html" | "css" | "scss"
                    | "json" | "yaml" | "yml" | "toml" | "xml" | "sql" | "sh" | "zsh" | "bash"
                    | "kt" | "java" | "rb" | "php" | "lua" | "zig" | "dart" | "proto" | "graphql"
            ),
            Self::Image => matches!(
                ext,
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "heic" | "heif"
                    | "tiff" | "bmp" | "ico" | "psd" | "ai" | "raw"
            ),
            Self::Media => matches!(
                ext,
                "mp4" | "mov" | "mkv" | "avi" | "webm" | "mp3" | "m4a" | "wav" | "flac" | "aac" | "ogg"
            ),
            Self::Archive => matches!(
                ext,
                "zip" | "tar" | "gz" | "tgz" | "7z" | "rar" | "bz2" | "xz" | "zst" | "dmg" | "iso" | "pkg"
            ),
        }
    }
}

/// 文件大小范围过滤条件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SizeFilter {
    /// 最小字节阈值（包含），若为 `None` 则无下限。
    pub min_bytes: Option<u64>,
    /// 最大字节阈值（包含），若为 `None` 则无上限。
    pub max_bytes: Option<u64>,
}

impl SizeFilter {
    /// 判断当前大小过滤器是否未施加任何限制。
    #[inline]
    pub fn is_unconstrained(&self) -> bool {
        self.min_bytes.is_none() && self.max_bytes.is_none()
    }

    /// 与另一大小过滤规则求交集。
    /// 若区间倒置（min_bytes > max_bytes），初筛器将直接拒绝所有记录（零命中）。
    pub fn intersect(&mut self, other: SizeFilter) {
        if let Some(other_min) = other.min_bytes {
            self.min_bytes = Some(match self.min_bytes {
                Some(curr_min) => curr_min.max(other_min),
                None => other_min,
            });
        }
        if let Some(other_max) = other.max_bytes {
            self.max_bytes = Some(match self.max_bytes {
                Some(curr_max) => curr_max.min(other_max),
                None => other_max,
            });
        }
    }
}

/// 查询级过滤条件，包含预设文件类别、自定义扩展名与文件大小区间过滤。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueryFilter {
    /// 目标文件类别，默认为 `FileType::All`（兼容单分类模式）。
    pub file_type: FileType,
    /// 用户指定的自定义扩展名（如 `"jpg"`、`"png"` 等，大小写不敏感，兼容单后缀模式）。
    pub custom_ext: Option<String>,
    /// 复合目标文件类别列表。
    pub file_types: Vec<FileType>,
    /// 用户指定的多个自定义扩展名列表（大小写不敏感，不含前导点号）。
    pub custom_exts: Vec<String>,
    /// 文件大小区间过滤条件。
    pub size_filter: SizeFilter,
    /// 是否显式指定了 `type:all` 过滤词元。
    pub has_explicit_type_all: bool,
    /// 由独立 `.ext` 语法隐式推断出的候选扩展名与原始词元 (extension, raw_token)
    pub inferred_exts: Vec<(String, String)>,
}

/// 解析单项带单位的大小字符串（如 "10m"、"500k"、"1.5g"、"1024b"），遵循 macOS 原生十进制规范（1KB = 10^3, 1MB = 10^6 等）。
pub(crate) fn parse_size_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    let lower = s.to_ascii_lowercase();
    let (num_str, multiplier): (&str, u64) = if lower.ends_with("bytes") {
        (&s[..s.len() - 5], 1)
    } else if lower.ends_with("byte") {
        (&s[..s.len() - 4], 1)
    } else if lower.ends_with("kb") {
        (&s[..s.len() - 2], 1_000)
    } else if lower.ends_with("mb") {
        (&s[..s.len() - 2], 1_000_000)
    } else if lower.ends_with("gb") {
        (&s[..s.len() - 2], 1_000_000_000)
    } else if lower.ends_with("tb") {
        (&s[..s.len() - 2], 1_000_000_000_000)
    } else if lower.ends_with('k') {
        (&s[..s.len() - 1], 1_000)
    } else if lower.ends_with('m') {
        (&s[..s.len() - 1], 1_000_000)
    } else if lower.ends_with('g') {
        (&s[..s.len() - 1], 1_000_000_000)
    } else if lower.ends_with('t') {
        (&s[..s.len() - 1], 1_000_000_000_000)
    } else if lower.ends_with('b') {
        (&s[..s.len() - 1], 1)
    } else {
        (s, 1)
    };

    let num_str = num_str.trim();
    if num_str.is_empty() {
        return None;
    }

    // 优先尝试无损无符号整型解析并使用 saturating_mul 防御溢出
    if let Ok(val) = num_str.parse::<u64>() {
        return Some(val.saturating_mul(multiplier));
    }

    // 包含小数时采用浮点解析
    if let Ok(val) = num_str.parse::<f64>() {
        if val.is_nan() || val.is_infinite() || val < 0.0 {
            return None;
        }
        let result = val * (multiplier as f64);
        if result >= u64::MAX as f64 {
            return Some(u64::MAX);
        }
        return Some(result.round() as u64);
    }

    None
}

/// 解析单个大小过滤语法词元（如 ">10m", "<500k", "1.5m..100m"）。
pub(crate) fn parse_size_token(payload: &str) -> Option<SizeFilter> {
    let payload = payload.trim();
    if payload.is_empty() {
        return None;
    }

    // 1. 双点闭区间语法：min..max
    if let Some((left, right)) = payload.split_once("..") {
        let left = left.trim();
        let right = right.trim();
        if left.is_empty() && right.is_empty() {
            return None;
        }
        let min_bytes = if !left.is_empty() {
            Some(parse_size_bytes(left)?)
        } else {
            None
        };
        let max_bytes = if !right.is_empty() {
            Some(parse_size_bytes(right)?)
        } else {
            None
        };
        return Some(SizeFilter {
            min_bytes,
            max_bytes,
        });
    }

    // 2. 下限前缀：>= 或 >
    if let Some(rest) = payload.strip_prefix(">=") {
        let val = parse_size_bytes(rest)?;
        return Some(SizeFilter {
            min_bytes: Some(val),
            max_bytes: None,
        });
    }
    if let Some(rest) = payload.strip_prefix('>') {
        let val = parse_size_bytes(rest)?;
        return Some(SizeFilter {
            min_bytes: Some(val),
            max_bytes: None,
        });
    }

    // 3. 上限前缀：<= 或 <
    if let Some(rest) = payload.strip_prefix("<=") {
        let val = parse_size_bytes(rest)?;
        return Some(SizeFilter {
            min_bytes: None,
            max_bytes: Some(val),
        });
    }
    if let Some(rest) = payload.strip_prefix('<') {
        let val = parse_size_bytes(rest)?;
        return Some(SizeFilter {
            min_bytes: None,
            max_bytes: Some(val),
        });
    }

    // 4. 等值前缀：= 或直接数值匹配
    if let Some(rest) = payload.strip_prefix('=') {
        let val = parse_size_bytes(rest)?;
        return Some(SizeFilter {
            min_bytes: Some(val),
            max_bytes: Some(val),
        });
    }

    if let Some(val) = parse_size_bytes(payload) {
        return Some(SizeFilter {
            min_bytes: Some(val),
            max_bytes: Some(val),
        });
    }

    None
}

/// 判断给定的词元或查询串是否为点文件（隐藏配置/工程文件）而非扩展名过滤。
///
/// 判定规则：
/// 1. 基础守卫：必须以 `.` 开头，且长度大于 1，不包含空格与路径分隔符；
/// 2. 结构特征：包含后续点号（如 `.env.local`、`.config.json` 等多段点文件）；
/// 3. 分隔符特征：包含连字符或下划线（如 `.clang-format`、`.bash_profile`、`.ds_store` 等典型文件名）；
/// 4. 常见配置尾缀：以 `rc` 结尾且长度 >= 4（如 `.zshrc`、`.bashrc`、`.npmrc`），或以 `ignore` 结尾（如 `.gitignore`、`.dockerignore`）；
/// 5. 常见点文件族前缀（长度 >= 4，覆盖键入过程中的中间态与衍生文件，如 `.git*`、`.env*`、`.docker*`、`.zsh*`、`.bash*`、`.cargo*` 等）；
/// 6. 知名单段点文件与隐藏目录白名单（如 `.vscode`、`.idea`、`.profile`、`.local` 等）。
pub fn is_dotfile_query(query: &str) -> bool {
    let s = query.trim();
    if !s.starts_with('.') || s.len() <= 1 || s.contains(' ') || s.contains('/') {
        return false;
    }

    let rest = &s[1..];
    // 多段点号（如 .env.local, .foo.bar）
    if rest.contains('.') {
        return true;
    }

    // 包含常见点文件名分隔符（如 .clang-format, .bash_profile, .ds_store）
    if rest.contains('-') || rest.contains('_') {
        return true;
    }

    let lower = s.to_ascii_lowercase();

    // 典型配置尾缀：.*rc（长度 >= 4，避免匹配 .rc 资源脚本扩展名，覆盖 .zshrc, .bashrc, .npmrc, .vimrc 等）
    if lower.len() >= 4 && lower.ends_with("rc") {
        return true;
    }

    // 典型排除尾缀：.*ignore（如 .gitignore, .dockerignore, .npmignore）
    if lower.len() >= 7 && lower.ends_with("ignore") {
        return true;
    }

    // 常见点文件/配置族前缀（覆盖输入过程中的中间态与派生文件，如 .giti、.dock、.zsh 等）
    const DOTFILE_FAMILY_PREFIXES: &[&str] = &[
        ".git",
        ".env",
        ".docker",
        ".editor",
        ".eslint",
        ".prett",
        ".config",
        ".cargo",
        ".vscode",
        ".idea",
        ".clang",
        ".rustf",
        ".pylint",
        ".ruboc",
        ".flake",
        ".gnupg",
        ".mailm",
        ".zsh",
        ".bash",
        ".npm",
        ".yarn",
    ];

    if DOTFILE_FAMILY_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return true;
    }

    // 知名单段点文件与隐藏目录
    matches!(
        lower.as_str(),
        ".local"
            | ".ssh"
            | ".profile"
            | ".zprofile"
            | ".zshenv"
            | ".zlogin"
            | ".cache"
            | ".localized"
            | ".trash"
    )
}

/// 解析检索输入中的类型、扩展名与文件大小过滤指令，提取清理后的纯检索词与对应的 `QueryFilter`。
///
/// 支持语法规则：
/// 1. `type:<category>`：设置文件分类（如 `type:code`、`type:doc`、`type:img` 等），支持逗号分隔复合分类（如 `type:code,doc`）。未知分类将退化为普通检索词。
/// 2. `ext:<extension>`：指定精确扩展名（如 `ext:png`、`ext:.pdf` 等），支持逗号分隔多扩展名（如 `ext:rs,swift,ts`），自动剥离前导点号。
/// 3. `size:<condition>`：指定文件大小约束（如 `size:>10m`、`size:<500k`、`size:1.5m..100m` 等），支持十进制单位与多个条件交集收敛。
/// 4. 独立 `.ext`：当词以点号开头、长度合理（2..=6）、纯字母数字且不包含路径分隔符时，若非知名隐藏点文件（如 `.gitignore`、`.idea` 等），识别为自定义扩展名。
pub fn parse_query_filter(input: &str) -> (String, QueryFilter) {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return (String::new(), QueryFilter::default());
    }

    let raw_terms: Vec<&str> = trimmed.split_whitespace().collect();
    let term_count = raw_terms.len();

    let mut remaining_terms = Vec::new();
    let mut file_types = Vec::new();
    let mut custom_exts = Vec::new();
    let mut inferred_exts = Vec::new();
    let mut size_filter = SizeFilter::default();
    let mut has_explicit_type_all = false;

    for term in raw_terms {
        // 1. type:<category>（支持以逗号切分的复合分类，如 type:code,doc）
        if term.len() >= 5 && term.is_char_boundary(5) && term[..5].eq_ignore_ascii_case("type:") {
            let cat_str = &term[5..];
            let parts: Vec<&str> = cat_str
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();

            if !parts.is_empty() {
                let parsed_parts: Vec<Option<FileType>> = parts
                    .iter()
                    .map(|&p| FileType::from_category(p))
                    .collect();

                if parsed_parts.iter().all(|opt| opt.is_some()) {
                    for ft in parsed_parts.into_iter().flatten() {
                        if ft == FileType::All {
                            file_types.clear();
                            has_explicit_type_all = true;
                        } else if !file_types.contains(&ft) {
                            file_types.push(ft);
                        }
                    }
                    continue;
                }
            }

            // 未知或空分类保留为检索词
            remaining_terms.push(term);
            continue;
        }

        // 2. ext:<extension>（支持以逗号切分的多扩展名，如 ext:rs,swift,ts）
        if term.len() >= 4 && term.is_char_boundary(4) && term[..4].eq_ignore_ascii_case("ext:") {
            let ext_payload = &term[4..];
            let parts: Vec<&str> = ext_payload
                .split(',')
                .map(|p| p.trim().trim_start_matches('.'))
                .filter(|p| !p.is_empty())
                .collect();

            if !parts.is_empty() {
                for p in parts {
                    let lower = p.to_ascii_lowercase();
                    if !custom_exts.contains(&lower) {
                        custom_exts.push(lower);
                    }
                }
                continue;
            }
        }

        // 3. size:<condition>（支持 size:>10m, size:<500k, size:1.5m..100m 以及多个 size 组合）
        if term.len() >= 5 && term.is_char_boundary(5) && term[..5].eq_ignore_ascii_case("size:") {
            let size_payload = &term[5..];
            if let Some(parsed) = parse_size_token(size_payload) {
                size_filter.intersect(parsed);
                continue;
            }
            remaining_terms.push(term);
            continue;
        }

        // 4. 独立 .ext（非隐藏点文件与相对路径守卫）
        // 关键语义约束：仅当查询包含多个词元时（例如存在前置主检索词时，如 "config .json" 或 "main .rs"），
        // 独立的 .ext 词元才作为隐式扩展名修饰符；
        // 若整条查询仅由单个词元构成（如 ".rs"、".giti"、".secret"），则保留在 remaining_terms 中为主检索词，
        // 避免在无其他检索词时误吞输入导致检索词变空，或使未列入白名单的点文件/隐藏目录前缀被误杀漏搜。
        if term_count > 1
            && term.starts_with('.')
            && term.len() > 1
            && term.len() <= 6
            && !term.contains('/')
        {
            if is_dotfile_query(term) {
                remaining_terms.push(term);
                continue;
            }
            let candidate = &term[1..];
            // 边界守卫：后缀候选必须全为字母或数字，防止 `..` 或 `...` 等相对路径/省略词元被误识别为扩展名过滤
            if !candidate.is_empty() && candidate.chars().all(|c| c.is_ascii_alphanumeric()) {
                let lower = candidate.to_ascii_lowercase();
                if !custom_exts.contains(&lower) {
                    custom_exts.push(lower.clone());
                }
                inferred_exts.push((lower, term.to_string()));
                continue;
            }
        }

        remaining_terms.push(term);
    }

    let primary_file_type = file_types.first().copied().unwrap_or(FileType::All);
    let primary_custom_ext = custom_exts.first().cloned();

    (
        remaining_terms.join(" "),
        QueryFilter {
            file_type: primary_file_type,
            custom_ext: primary_custom_ext,
            file_types,
            custom_exts,
            size_filter,
            has_explicit_type_all,
            inferred_exts,
        },
    )
}

/// 判断无后缀的隐藏点文件是否属于代码工程或环境配置文件（用于 `type:code` 分类判定）。
///
/// 将实体文件分类逻辑与用户查询意图探测彻底解耦：
/// 1. 严格排斥系统级描述文件、缓存与临时目录（如 `.localized`、`.DS_Store`、`.Trash` 等）；
/// 2. 覆盖常见配置尾缀（如 `.*ignore`、`.*rc`）；
/// 3. 精确匹配核心代码与开发环境配置资产（如 `.editorconfig`、`.clang-format`、`.env*` 等）。
#[inline]
pub(crate) fn is_code_dotfile(name: &str) -> bool {
    if !name.starts_with('.') || name.len() <= 1 {
        return false;
    }
    let lower = name.to_ascii_lowercase();

    // 系统级描述、临时、缓存与历史文件防御拦截
    if matches!(
        lower.as_str(),
        ".trash"
            | ".localized"
            | ".ds_store"
            | ".cache"
            | ".spotlight-v100"
            | ".fseventsd"
            | ".temporaryitems"
            | ".cfusertextencoding"
    ) || lower.ends_with("history")
    {
        return false;
    }

    // 1. 典型配置尾缀：.*ignore（如 .gitignore, .dockerignore, .npmignore）
    if lower.len() >= 7 && lower.ends_with("ignore") {
        return true;
    }

    // 2. 典型配置尾缀：.*rc（长度 >= 4，如 .zshrc, .bashrc, .npmrc, .prettierrc, .eslintrc）
    if lower.len() >= 4 && lower.ends_with("rc") {
        return true;
    }

    // 3. 常见核心开发环境与工程代码配置文件
    if lower.starts_with(".env") {
        return true;
    }

    matches!(
        lower.as_str(),
        ".editorconfig"
            | ".clang-format"
            | ".clang-tidy"
            | ".flake8"
            | ".mailmap"
            | ".gitattributes"
            | ".gitmodules"
            | ".gitconfig"
            | ".gitkeep"
            | ".bash_profile"
            | ".zprofile"
            | ".zshenv"
            | ".zlogin"
    )
}

/// 判断无扩展名的特殊文件名是否归类于代码工程文件（如 Makefile、Dockerfile 以及常见工程点配置文件等）。
#[inline]
pub(crate) fn is_code_special_filename(name: &str) -> bool {
    if matches!(
        name,
        "Makefile"
            | "makefile"
            | "Dockerfile"
            | "dockerfile"
            | "Containerfile"
            | "containerfile"
            | "Justfile"
            | "justfile"
            | "LICENSE"
            | "Gemfile"
            | "Rakefile"
            | "CMakeLists.txt"
    ) {
        return true;
    }
    is_code_dotfile(name)
}

/// 针对当前紧凑索引完成编译的记录过滤器。
///
/// 通过在检索前预先将过滤规则投影为扩展名池位掩码（`valid_ext_mask`），
/// 在单趟扫描时将原本代价较高的字符串比对与模式匹配退化为 O(1) 的下标寻址与位判断，
/// 达成零堆内存分配的亚毫秒级记录初筛。
#[derive(Debug, Clone)]
pub struct CompiledRecordFilter {
    has_custom_ext: bool,
    empty_ext_id: Option<u32>,
    valid_ext_mask: Vec<bool>,
    has_type_filter: bool,
    allow_dirs: bool,
    dirs_only: bool,
    has_code_type: bool,
    valid_file_types: Vec<FileType>,
    custom_exts: Vec<String>,
    min_size_bytes: u64,
    max_size_bytes: u64,
    has_size_filter: bool,
    has_explicit_type_all: bool,
}

impl Default for CompiledRecordFilter {
    /// 默认构造无限制直通过滤器。
    fn default() -> Self {
        Self {
            has_custom_ext: false,
            empty_ext_id: None,
            valid_ext_mask: Vec::new(),
            has_type_filter: false,
            allow_dirs: true,
            dirs_only: false,
            has_code_type: false,
            valid_file_types: Vec::new(),
            custom_exts: Vec::new(),
            min_size_bytes: 0,
            max_size_bytes: u64::MAX,
            has_size_filter: false,
            has_explicit_type_all: false,
        }
    }
}

impl CompiledRecordFilter {
    /// 判断当前过滤器是否为全放行的直通模式。
    #[inline]
    pub fn is_passthrough(&self) -> bool {
        !self.has_type_filter && !self.has_custom_ext && !self.has_size_filter
    }

    /// 是否显式指定了 `type:all`。
    #[inline]
    pub fn has_explicit_type_all(&self) -> bool {
        self.has_explicit_type_all
    }

    /// 是否指定了自定义扩展名。
    #[inline]
    pub fn has_custom_ext(&self) -> bool {
        self.has_custom_ext
    }

    /// 是否允许目录项通过。
    #[inline]
    pub fn allow_dirs(&self) -> bool {
        self.allow_dirs
    }

    /// 是否仅允许目录项通过。
    #[inline]
    pub fn dirs_only(&self) -> bool {
        self.dirs_only
    }

    /// 针对特定紧凑索引将 `QueryFilter` 编译为位掩码与标量过滤器。
    pub fn compile(index: &CompactIndex, filter: &QueryFilter) -> Self {
        let empty_ext_id = index.extension_id("");

        // 1. 规范化 custom_exts（兼容单字段 custom_ext）
        let mut clean_custom_exts: Vec<String> = filter
            .custom_exts
            .iter()
            .map(|c| c.trim().trim_start_matches('.').trim().to_ascii_lowercase())
            .filter(|t| !t.is_empty())
            .collect();

        if clean_custom_exts.is_empty() {
            if let Some(target) = filter
                .custom_ext
                .as_deref()
                .map(|c| c.trim().trim_start_matches('.').trim())
                .filter(|t| !t.is_empty())
            {
                clean_custom_exts.push(target.to_ascii_lowercase());
            }
        }

        // 2. 规范化 file_types（兼容单字段 file_type）
        let mut raw_file_types = filter.file_types.clone();
        if raw_file_types.is_empty() && filter.file_type != FileType::All {
            raw_file_types.push(filter.file_type);
        }

        let valid_file_types: Vec<FileType> = raw_file_types
            .into_iter()
            .filter(|&ft| ft != FileType::All)
            .collect();

        let has_custom_ext = !clean_custom_exts.is_empty();
        let has_type_filter = !valid_file_types.is_empty();

        let (allow_dirs, dirs_only) = if has_custom_ext {
            (false, false)
        } else if has_type_filter {
            let has_folder = valid_file_types.contains(&FileType::Folder);
            let non_folder_count = valid_file_types
                .iter()
                .filter(|&&ft| ft != FileType::Folder)
                .count();

            if has_folder && non_folder_count == 0 {
                (true, true)
            } else if has_folder {
                (true, false)
            } else {
                (false, false)
            }
        } else {
            (true, false)
        };

        let has_code_type = !has_custom_ext && valid_file_types.contains(&FileType::Code);

        let valid_ext_mask = if has_custom_ext {
            index.evaluate_extensions(|ext| {
                clean_custom_exts
                    .iter()
                    .any(|target| ext.eq_ignore_ascii_case(target))
            })
        } else if has_type_filter {
            if dirs_only {
                Vec::new()
            } else {
                index.evaluate_extensions(|ext| {
                    valid_file_types.iter().any(|ft| ft.matches(false, ext))
                })
            }
        } else {
            Vec::new()
        };

        let min_size_bytes = filter.size_filter.min_bytes.unwrap_or(0);
        let max_size_bytes = filter.size_filter.max_bytes.unwrap_or(u64::MAX);
        let has_size_filter =
            filter.size_filter.min_bytes.is_some() || filter.size_filter.max_bytes.is_some();

        Self {
            has_custom_ext,
            empty_ext_id,
            valid_ext_mask,
            has_type_filter,
            allow_dirs,
            dirs_only,
            has_code_type,
            valid_file_types,
            custom_exts: clean_custom_exts,
            min_size_bytes,
            max_size_bytes,
            has_size_filter,
            has_explicit_type_all: filter.has_explicit_type_all,
        }
    }

    /// 评估单条 `EntryView` 是否满足过滤条件。
    pub fn matches_view(&self, view: &EntryView<'_>) -> bool {
        if self.is_passthrough() {
            return true;
        }
        if view.is_dir {
            if !self.allow_dirs {
                return false;
            }
            if self.dirs_only {
                return view.size >= self.min_size_bytes && view.size <= self.max_size_bytes;
            }
            return !self.has_size_filter;
        }
        if self.dirs_only {
            return false;
        }
        if view.size < self.min_size_bytes || view.size > self.max_size_bytes {
            return false;
        }
        if !self.has_type_filter && !self.has_custom_ext {
            return true;
        }
        if self.has_custom_ext {
            if view.extension.is_empty() && view.name.starts_with('.') && view.name.len() > 1 {
                let dotfile_candidate = &view.name[1..];
                if self
                    .custom_exts
                    .iter()
                    .any(|target| dotfile_candidate.eq_ignore_ascii_case(target))
                {
                    return true;
                }
            }
            return self
                .custom_exts
                .iter()
                .any(|target| view.extension.eq_ignore_ascii_case(target));
        }
        if self.has_code_type
            && view.extension.is_empty()
            && is_code_special_filename(view.name)
        {
            return true;
        }
        self.valid_file_types
            .iter()
            .any(|ft| ft.matches(false, view.extension))
    }

    /// 评估单条底层 `CompactRecord` 是否满足过滤条件。
    ///
    /// 对于特殊无后缀工程文件（如 Makefile、Dockerfile 等），
    /// 通过惰性闭包 `get_name` 检查文件名，避免常规记录解析时的额外开销。
    pub fn matches_record<'a>(
        &self,
        record: &CompactRecord,
        get_name: impl FnOnce() -> Option<&'a str>,
    ) -> bool {
        if self.is_passthrough() {
            return true;
        }
        // 1. 目录守卫：纯目录检索时按目录项自身大小比对，混排检索时目录不参与大小比较
        if record.is_dir {
            if !self.allow_dirs {
                return false;
            }
            if self.dirs_only {
                return record.size >= self.min_size_bytes && record.size <= self.max_size_bytes;
            }
            return !self.has_size_filter;
        }
        if self.dirs_only {
            return false;
        }

        // 2. 文件大小标量范围比对（单时钟周期指令，零堆分配极速剪枝）
        if record.size < self.min_size_bytes || record.size > self.max_size_bytes {
            return false;
        }

        // 3. 用户仅指定大小过滤而无类型与扩展名约束时，直通放行非目录文件
        if !self.has_type_filter && !self.has_custom_ext {
            return true;
        }

        // 4. 扩展名位掩码初筛与特殊代码文件名 / 根点文件兜底
        if self.has_custom_ext {
            let is_empty = self.empty_ext_id == Some(record.extension);
            if is_empty {
                if let Some(name) = get_name() {
                    if name.starts_with('.') && name.len() > 1 {
                        let dotfile_candidate = &name[1..];
                        if self
                            .custom_exts
                            .iter()
                            .any(|target| dotfile_candidate.eq_ignore_ascii_case(target))
                        {
                            return true;
                        }
                    }
                }
            }
        } else if self.has_code_type {
            let is_empty = self.empty_ext_id == Some(record.extension);
            if is_empty {
                if let Some(name) = get_name() {
                    if is_code_special_filename(name) {
                        return true;
                    }
                }
            }
        }
        self.valid_ext_mask
            .get(record.extension as usize)
            .copied()
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::FileEntry;

    #[test]
    fn test_file_type_matching() {
        assert!(FileType::Document.matches(false, "pdf"));
        assert!(FileType::Document.matches(false, "docx"));
        assert!(!FileType::Document.matches(true, "pdf"));
        assert!(!FileType::Document.matches(false, "rs"));

        assert!(FileType::Code.matches(false, "rs"));
        assert!(FileType::Code.matches(false, "swift"));
        assert!(FileType::Code.matches(false, "vue"));
        assert!(FileType::Code.matches(false, "svelte"));
        assert!(FileType::Code.matches(false, "lua"));
        assert!(FileType::Code.matches(false, "zig"));
        assert!(FileType::Code.matches(false, "dart"));
        assert!(FileType::Code.matches(false, "proto"));
        assert!(FileType::Code.matches(false, "graphql"));

        assert!(FileType::Image.matches(false, "png"));
        assert!(FileType::Image.matches(false, "heic"));
        assert!(!FileType::Image.matches(false, "mp4"));

        assert!(FileType::Media.matches(false, "mp4"));
        assert!(FileType::Media.matches(false, "mp3"));
        assert!(FileType::Media.matches(false, "flac"));
        assert!(!FileType::Media.matches(false, "png"));
        assert!(!FileType::Media.matches(true, "mp4"));

        assert!(FileType::Archive.matches(false, "zip"));
        assert!(FileType::Archive.matches(false, "tar"));
        assert!(FileType::Archive.matches(false, "zst"));
        assert!(FileType::Archive.matches(false, "dmg"));
        assert!(!FileType::Archive.matches(false, "txt"));
        assert!(!FileType::Archive.matches(true, "zip"));

        assert!(FileType::Folder.matches(true, ""));
        assert!(!FileType::Folder.matches(false, "rs"));

        assert!(FileType::All.matches(false, "rs"));
        assert!(FileType::All.matches(true, ""));
        assert_eq!(FileType::default(), FileType::All);
    }

    #[test]
    fn test_is_code_special_filename() {
        assert!(is_code_special_filename("Makefile"));
        assert!(is_code_special_filename("makefile"));
        assert!(is_code_special_filename("Dockerfile"));
        assert!(is_code_special_filename("dockerfile"));
        assert!(is_code_special_filename("Containerfile"));
        assert!(is_code_special_filename("containerfile"));
        assert!(is_code_special_filename("Justfile"));
        assert!(is_code_special_filename("justfile"));
        assert!(is_code_special_filename("LICENSE"));
        assert!(is_code_special_filename("Gemfile"));
        assert!(is_code_special_filename("Rakefile"));
        assert!(!is_code_special_filename("normal.txt"));
    }

    fn create_test_index() -> CompactIndex {
        let entries = vec![
            FileEntry {
                name: "src".to_string(),
                path: "/workspace/src".to_string(),
                is_dir: true,
                size: 0,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            FileEntry {
                name: "main.rs".to_string(),
                path: "/workspace/src/main.rs".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "rs".to_string(),
            },
            FileEntry {
                name: "Makefile".to_string(),
                path: "/workspace/Makefile".to_string(),
                is_dir: false,
                size: 200,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            FileEntry {
                name: "unknown".to_string(),
                path: "/workspace/unknown".to_string(),
                is_dir: false,
                size: 50,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            FileEntry {
                name: "doc.pdf".to_string(),
                path: "/workspace/doc.pdf".to_string(),
                is_dir: false,
                size: 300,
                modified: 0,
                created_at: 0,
                extension: "pdf".to_string(),
            },
            FileEntry {
                name: "photo.jpg".to_string(),
                path: "/workspace/photo.jpg".to_string(),
                is_dir: false,
                size: 400,
                modified: 0,
                created_at: 0,
                extension: "jpg".to_string(),
            },
            FileEntry {
                name: "icon.png".to_string(),
                path: "/workspace/icon.png".to_string(),
                is_dir: false,
                size: 500,
                modified: 0,
                created_at: 0,
                extension: "png".to_string(),
            },
            FileEntry {
                name: "clip.mp4".to_string(),
                path: "/workspace/clip.mp4".to_string(),
                is_dir: false,
                size: 1000,
                modified: 0,
                created_at: 0,
                extension: "mp4".to_string(),
            },
            FileEntry {
                name: "track.mp3".to_string(),
                path: "/workspace/track.mp3".to_string(),
                is_dir: false,
                size: 2000,
                modified: 0,
                created_at: 0,
                extension: "mp3".to_string(),
            },
            FileEntry {
                name: "bundle.zip".to_string(),
                path: "/workspace/bundle.zip".to_string(),
                is_dir: false,
                size: 3000,
                modified: 0,
                created_at: 0,
                extension: "zip".to_string(),
            },
            FileEntry {
                name: "backup.tar".to_string(),
                path: "/workspace/backup.tar".to_string(),
                is_dir: false,
                size: 4000,
                modified: 0,
                created_at: 0,
                extension: "tar".to_string(),
            },
        ];
        CompactIndex::from_entries(entries).unwrap()
    }

    #[test]
    fn test_compiled_filter_all_matches_all_entries() {
        let index = create_test_index();
        let query_filter = QueryFilter::default();
        let compiled = CompiledRecordFilter::compile(&index, &query_filter);

        for (id, view) in index.iter() {
            assert!(compiled.matches_view(&view), "view {:?} should match All", view.name);
            let record = &index.records()[id.0];
            assert!(
                compiled.matches_record(record, || Some(index.record_name(record))),
                "record {:?} should match All",
                index.record_name(record)
            );
        }
    }

    #[test]
    fn test_compiled_filter_folder_only_matches_directories() {
        let index = create_test_index();
        let query_filter = QueryFilter {
            file_type: FileType::Folder,
            custom_ext: None,
            ..Default::default()
        };
        let compiled = CompiledRecordFilter::compile(&index, &query_filter);

        for (id, view) in index.iter() {
            let expected = view.is_dir;
            assert_eq!(
                compiled.matches_view(&view),
                expected,
                "view {:?} (is_dir: {}) folder match mismatch",
                view.name,
                view.is_dir
            );
            let record = &index.records()[id.0];
            assert_eq!(
                compiled.matches_record(record, || Some(index.record_name(record))),
                expected,
                "record {:?} folder match mismatch",
                index.record_name(record)
            );
        }
    }

    #[test]
    fn test_compiled_filter_code_matches_rs_and_makefile() {
        let index = create_test_index();
        let query_filter = QueryFilter {
            file_type: FileType::Code,
            custom_ext: None,
            ..Default::default()
        };
        let compiled = CompiledRecordFilter::compile(&index, &query_filter);

        for (id, view) in index.iter() {
            let expected = matches!(view.name, "main.rs" | "Makefile");
            assert_eq!(
                compiled.matches_view(&view),
                expected,
                "view {:?} code match mismatch",
                view.name
            );
            let record = &index.records()[id.0];
            assert_eq!(
                compiled.matches_record(record, || Some(index.record_name(record))),
                expected,
                "record {:?} code match mismatch",
                index.record_name(record)
            );
        }
    }

    #[test]
    fn test_compiled_filter_document_matches_pdf_not_rs() {
        let index = create_test_index();
        let query_filter = QueryFilter {
            file_type: FileType::Document,
            custom_ext: None,
            ..Default::default()
        };
        let compiled = CompiledRecordFilter::compile(&index, &query_filter);

        for (id, view) in index.iter() {
            let expected = view.name == "doc.pdf";
            assert_eq!(
                compiled.matches_view(&view),
                expected,
                "view {:?} document match mismatch",
                view.name
            );
            let record = &index.records()[id.0];
            assert_eq!(
                compiled.matches_record(record, || Some(index.record_name(record))),
                expected,
                "record {:?} document match mismatch",
                index.record_name(record)
            );
        }
    }

    #[test]
    fn test_compiled_filter_custom_ext_matching() {
        let index = create_test_index();
        let query_filter = QueryFilter {
            file_type: FileType::All,
            custom_ext: Some("jpg".to_string()),
            ..Default::default()
        };
        let compiled = CompiledRecordFilter::compile(&index, &query_filter);

        for (id, view) in index.iter() {
            let expected = view.name == "photo.jpg";
            assert_eq!(
                compiled.matches_view(&view),
                expected,
                "view {:?} custom ext match mismatch",
                view.name
            );
            let record = &index.records()[id.0];
            assert_eq!(
                compiled.matches_record(record, || Some(index.record_name(record))),
                expected,
                "record {:?} custom ext match mismatch",
                index.record_name(record)
            );
        }

        // 大小写与带点前缀容错测试
        let dot_upper_filter = QueryFilter {
            file_type: FileType::All,
            custom_ext: Some(".JPG".to_string()),
            ..Default::default()
        };
        let compiled_dot_upper = CompiledRecordFilter::compile(&index, &dot_upper_filter);
        for (id, view) in index.iter() {
            let expected = view.name == "photo.jpg";
            assert_eq!(compiled_dot_upper.matches_view(&view), expected);
            let record = &index.records()[id.0];
            assert_eq!(
                compiled_dot_upper.matches_record(record, || Some(index.record_name(record))),
                expected
            );
        }
    }

    #[test]
    fn test_compiled_filter_custom_ext_empty_fallback() {
        let index = create_test_index();
        for empty_or_dot in ["", "   ", ".", "...", "  .  "] {
            let query_filter = QueryFilter {
                file_type: FileType::Code,
                custom_ext: Some(empty_or_dot.to_string()),
                ..Default::default()
            };
            let compiled = CompiledRecordFilter::compile(&index, &query_filter);
            assert!(!compiled.has_custom_ext);

            for (id, view) in index.iter() {
                let expected = matches!(view.name, "main.rs" | "Makefile");
                assert_eq!(compiled.matches_view(&view), expected);
                let record = &index.records()[id.0];
                assert_eq!(
                    compiled.matches_record(record, || Some(index.record_name(record))),
                    expected
                );
            }
        }
    }

    #[test]
    fn test_compiled_filter_image_matches_jpg_and_png() {
        let index = create_test_index();
        let query_filter = QueryFilter {
            file_type: FileType::Image,
            custom_ext: None,
            ..Default::default()
        };
        let compiled = CompiledRecordFilter::compile(&index, &query_filter);

        for (id, view) in index.iter() {
            let expected = matches!(view.name, "photo.jpg" | "icon.png");
            assert_eq!(
                compiled.matches_view(&view),
                expected,
                "view {:?} image match mismatch",
                view.name
            );
            let record = &index.records()[id.0];
            assert_eq!(
                compiled.matches_record(record, || Some(index.record_name(record))),
                expected,
                "record {:?} image match mismatch",
                index.record_name(record)
            );
        }
    }

    #[test]
    fn test_compiled_filter_media_matches_mp4_and_mp3() {
        let index = create_test_index();
        let query_filter = QueryFilter {
            file_type: FileType::Media,
            custom_ext: None,
            ..Default::default()
        };
        let compiled = CompiledRecordFilter::compile(&index, &query_filter);

        for (id, view) in index.iter() {
            let expected = matches!(view.name, "clip.mp4" | "track.mp3");
            assert_eq!(
                compiled.matches_view(&view),
                expected,
                "view {:?} media match mismatch",
                view.name
            );
            let record = &index.records()[id.0];
            assert_eq!(
                compiled.matches_record(record, || Some(index.record_name(record))),
                expected,
                "record {:?} media match mismatch",
                index.record_name(record)
            );
        }
    }

    #[test]
    fn test_compiled_filter_archive_matches_zip_and_tar() {
        let index = create_test_index();
        let query_filter = QueryFilter {
            file_type: FileType::Archive,
            custom_ext: None,
            ..Default::default()
        };
        let compiled = CompiledRecordFilter::compile(&index, &query_filter);

        for (id, view) in index.iter() {
            let expected = matches!(view.name, "bundle.zip" | "backup.tar");
            assert_eq!(
                compiled.matches_view(&view),
                expected,
                "view {:?} archive match mismatch",
                view.name
            );
            let record = &index.records()[id.0];
            assert_eq!(
                compiled.matches_record(record, || Some(index.record_name(record))),
                expected,
                "record {:?} archive match mismatch",
                index.record_name(record)
            );
        }
    }

    #[test]
    fn test_compiled_filter_custom_ext_overrides_folder_filter() {
        let index = create_test_index();
        let query_filter = QueryFilter {
            file_type: FileType::Folder,
            custom_ext: Some("jpg".to_string()),
            ..Default::default()
        };
        let compiled = CompiledRecordFilter::compile(&index, &query_filter);

        for (id, view) in index.iter() {
            let expected = view.name == "photo.jpg";
            assert_eq!(
                compiled.matches_view(&view),
                expected,
                "view {:?} custom ext override folder mismatch",
                view.name
            );
            let record = &index.records()[id.0];
            assert_eq!(
                compiled.matches_record(record, || Some(index.record_name(record))),
                expected,
                "record {:?} custom ext override folder mismatch",
                index.record_name(record)
            );
        }
    }

    #[test]
    fn test_empty_ext_id_when_slot_zero_is_not_empty_ext() {
        // 构造首个条目非空扩展名的紧凑索引，使得 slot 0 映射为 "txt" 而非空扩展名 ""
        let entries = vec![
            FileEntry {
                name: "Makefile".to_string(), // 名字虽然叫 Makefile，但扩展名为 txt
                path: "/workspace/Makefile.txt".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "txt".to_string(),
            },
            FileEntry {
                name: "Makefile".to_string(), // 真实的无后缀特殊代码文件
                path: "/workspace/Makefile".to_string(),
                is_dir: false,
                size: 200,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            FileEntry {
                name: "main.rs".to_string(),
                path: "/workspace/main.rs".to_string(),
                is_dir: false,
                size: 300,
                modified: 0,
                created_at: 0,
                extension: "rs".to_string(),
            },
        ];
        let index = CompactIndex::from_entries(entries).unwrap();

        // 验证 slot 0 确实是 "txt"，slot 1 是 ""
        assert_eq!(index.extension_id("txt"), Some(0));
        assert_eq!(index.extension_id(""), Some(1));

        let query_filter = QueryFilter {
            file_type: FileType::Code,
            custom_ext: None,
            ..Default::default()
        };
        let compiled = CompiledRecordFilter::compile(&index, &query_filter);

        // Record 0 (Makefile.txt): 扩展名为 txt，既不匹配 Code 掩码，也不是真正的空扩展名，不能因 slot == 0 误判为代码文件
        let record_txt = &index.records()[0];
        let view_txt = index.view(crate::compact_index::EntryId(0)).unwrap();
        assert!(!compiled.matches_view(&view_txt));
        assert!(!compiled.matches_record(record_txt, || Some("Makefile")));

        // Record 1 (Makefile): 真正的无扩展名工程文件，应正确匹配
        let record_makefile = &index.records()[1];
        let view_makefile = index.view(crate::compact_index::EntryId(1)).unwrap();
        assert!(compiled.matches_view(&view_makefile));
        assert!(compiled.matches_record(record_makefile, || Some("Makefile")));

        // Record 2 (main.rs): 扩展名为 rs，应正确匹配
        let record_rs = &index.records()[2];
        let view_rs = index.view(crate::compact_index::EntryId(2)).unwrap();
        assert!(compiled.matches_view(&view_rs));
        assert!(compiled.matches_record(record_rs, || Some("main.rs")));
    }

    #[test]
    fn test_empty_ext_id_none_when_no_empty_extension_in_index() {
        let entries = vec![FileEntry {
            name: "Makefile".to_string(),
            path: "/workspace/Makefile.txt".to_string(),
            is_dir: false,
            size: 100,
            modified: 0,
            created_at: 0,
            extension: "txt".to_string(),
        }];
        let index = CompactIndex::from_entries(entries).unwrap();
        assert_eq!(index.extension_id(""), None);

        let query_filter = QueryFilter {
            file_type: FileType::Code,
            custom_ext: None,
            ..Default::default()
        };
        let compiled = CompiledRecordFilter::compile(&index, &query_filter);
        let record = &index.records()[0];
        assert!(!compiled.matches_record(record, || Some("Makefile")));
    }

    #[test]
    fn test_compiled_filter_out_of_bounds_protection() {
        let index = create_test_index();
        let query_filter = QueryFilter {
            file_type: FileType::Document,
            custom_ext: None,
            ..Default::default()
        };
        let compiled = CompiledRecordFilter::compile(&index, &query_filter);

        let out_of_bounds_record = CompactRecord::from_raw_parts(false, 9999);
        assert!(
            !compiled.matches_record(&out_of_bounds_record, || None),
            "out of bounds slot must safely return false"
        );

        let max_record = CompactRecord::from_raw_parts(false, u32::MAX);
        assert!(
            !compiled.matches_record(&max_record, || None),
            "u32::MAX slot must safely return false"
        );
    }

    #[test]
    fn test_parse_query_filter() {
        let (clean, filter) = parse_query_filter("lokii type:code");
        assert_eq!(clean, "lokii");
        assert_eq!(filter.file_type, FileType::Code);
        assert_eq!(filter.custom_ext, None);

        let (clean2, filter2) = parse_query_filter("type:doc report 2026");
        assert_eq!(clean2, "report 2026");
        assert_eq!(filter2.file_type, FileType::Document);

        let (clean3, filter3) = parse_query_filter("avatar ext:png");
        assert_eq!(clean3, "avatar");
        assert_eq!(filter3.custom_ext.as_deref(), Some("png"));

        let (clean4, filter4) = parse_query_filter("config .json");
        assert_eq!(clean4, "config");
        assert_eq!(filter4.custom_ext.as_deref(), Some("json"));

        // 隐藏文件不应被误判为扩展名
        let (clean5, filter5) = parse_query_filter(".gitignore");
        assert_eq!(clean5, ".gitignore");
        assert_eq!(filter5.custom_ext, None);

        let (clean_idea, filter_idea) = parse_query_filter(".idea");
        assert_eq!(clean_idea, ".idea");
        assert_eq!(filter_idea.custom_ext, None);

        let (clean_cargo, filter_cargo) = parse_query_filter(".cargo");
        assert_eq!(clean_cargo, ".cargo");
        assert_eq!(filter_cargo.custom_ext, None);

        let (clean_cache, filter_cache) = parse_query_filter(".cache");
        assert_eq!(clean_cache, ".cache");
        assert_eq!(filter_cache.custom_ext, None);

        // 边界守卫：.. 与 ... 不应被误识别为扩展名过滤
        let (clean_dots, filter_dots) = parse_query_filter("lokii ..");
        assert_eq!(clean_dots, "lokii ..");
        assert_eq!(filter_dots.custom_ext, None);
        assert_eq!(filter_dots.file_type, FileType::All);

        let (clean_dots3, filter_dots3) = parse_query_filter("lokii ...");
        assert_eq!(clean_dots3, "lokii ...");
        assert_eq!(filter_dots3.custom_ext, None);

        // 独立类型指令测试
        let (clean_type, filter_type) = parse_query_filter("type:code");
        assert_eq!(clean_type, "");
        assert_eq!(filter_type.file_type, FileType::Code);
        assert_eq!(filter_type.custom_ext, None);

        // 复合测试：空格与大小写容错
        let (clean6, filter6) = parse_query_filter("  APP  TYPE:IMG  ext:.PNG  ");
        assert_eq!(clean6, "APP");
        assert_eq!(filter6.file_type, FileType::Image);
        assert_eq!(filter6.custom_ext.as_deref(), Some("png"));
    }

    #[test]
    fn test_parse_multi_file_type() {
        let (clean, filter) = parse_query_filter("main type:code,doc");
        assert_eq!(clean, "main");
        assert_eq!(filter.file_types, vec![FileType::Code, FileType::Document]);
    }

    #[test]
    fn test_parse_multi_extension() {
        let (clean, filter) = parse_query_filter("test ext:rs,swift,ts");
        assert_eq!(clean, "test");
        assert_eq!(filter.custom_exts, vec!["rs", "swift", "ts"]);
    }

    #[test]
    fn test_compiled_filter_multi_type_code_and_doc() {
        let index = create_test_index();
        let (clean, query_filter) = parse_query_filter("lokii type:code,doc");
        assert_eq!(clean, "lokii");
        let compiled = CompiledRecordFilter::compile(&index, &query_filter);

        assert!(!compiled.is_passthrough());
        assert!(!compiled.allow_dirs());
        assert!(!compiled.dirs_only());

        for (id, view) in index.iter() {
            let expected = matches!(view.name, "main.rs" | "Makefile" | "doc.pdf");
            assert_eq!(
                compiled.matches_view(&view),
                expected,
                "view {:?} multi-type match mismatch",
                view.name
            );
            let record = &index.records()[id.0];
            assert_eq!(
                compiled.matches_record(record, || Some(index.record_name(record))),
                expected,
                "record {:?} multi-type match mismatch",
                index.record_name(record)
            );
        }
    }

    #[test]
    fn test_compiled_filter_multi_type_with_folder() {
        let index = create_test_index();
        let (clean, query_filter) = parse_query_filter("lokii type:folder,code");
        assert_eq!(clean, "lokii");
        let compiled = CompiledRecordFilter::compile(&index, &query_filter);

        assert!(!compiled.is_passthrough());
        assert!(compiled.allow_dirs());
        assert!(!compiled.dirs_only());

        for (id, view) in index.iter() {
            let expected = view.is_dir || matches!(view.name, "main.rs" | "Makefile");
            assert_eq!(
                compiled.matches_view(&view),
                expected,
                "view {:?} folder+code match mismatch",
                view.name
            );
            let record = &index.records()[id.0];
            assert_eq!(
                compiled.matches_record(record, || Some(index.record_name(record))),
                expected,
                "record {:?} folder+code match mismatch",
                index.record_name(record)
            );
        }
    }

    #[test]
    fn test_compiled_filter_multi_extension() {
        let index = create_test_index();
        let (clean, query_filter) = parse_query_filter("test ext:rs,png");
        assert_eq!(clean, "test");
        let compiled = CompiledRecordFilter::compile(&index, &query_filter);

        assert!(!compiled.is_passthrough());
        assert!(compiled.has_custom_ext());

        for (id, view) in index.iter() {
            let expected = matches!(view.name, "main.rs" | "icon.png");
            assert_eq!(
                compiled.matches_view(&view),
                expected,
                "view {:?} multi-ext match mismatch",
                view.name
            );
            let record = &index.records()[id.0];
            assert_eq!(
                compiled.matches_record(record, || Some(index.record_name(record))),
                expected,
                "record {:?} multi-ext match mismatch",
                index.record_name(record)
            );
        }
    }

    #[test]
    fn test_compiled_filter_empty_all_passthrough() {
        let index = create_test_index();
        let (clean, query_filter) = parse_query_filter("test type:all");
        assert_eq!(clean, "test");
        let compiled = CompiledRecordFilter::compile(&index, &query_filter);

        assert!(compiled.is_passthrough());
        for (id, view) in index.iter() {
            assert!(compiled.matches_view(&view));
            let record = &index.records()[id.0];
            assert!(compiled.matches_record(record, || Some(index.record_name(record))));
        }
    }

    #[test]
    fn test_parse_size_filters() {
        let (c1, f1) = parse_query_filter("test size:>10m");
        assert_eq!(c1, "test");
        assert_eq!(f1.size_filter.min_bytes, Some(10_000_000));
        assert_eq!(f1.size_filter.max_bytes, None);

        let (c2, f2) = parse_query_filter("test size:<500k");
        assert_eq!(c2, "test");
        assert_eq!(f2.size_filter.min_bytes, None);
        assert_eq!(f2.size_filter.max_bytes, Some(500_000));

        let (c3, f3) = parse_query_filter("test size:1.5m..100m");
        assert_eq!(c3, "test");
        assert_eq!(f3.size_filter.min_bytes, Some(1_500_000));
        assert_eq!(f3.size_filter.max_bytes, Some(100_000_000));
    }

    #[test]
    fn test_compiled_filter_matches_size() {
        let store = crate::indexer::IndexStore::new();
        store.replace(vec![
            crate::indexer::FileEntry {
                name: "small.txt".to_string(),
                path: "/small.txt".to_string(),
                is_dir: false,
                size: 1_000,
                modified: 0,
                created_at: 0,
                extension: "txt".to_string(),
            },
            crate::indexer::FileEntry {
                name: "large.txt".to_string(),
                path: "/large.txt".to_string(),
                is_dir: false,
                size: 20_000_000,
                modified: 0,
                created_at: 0,
                extension: "txt".to_string(),
            },
            crate::indexer::FileEntry {
                name: "folder".to_string(),
                path: "/folder".to_string(),
                is_dir: true,
                size: 0,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
        ]);

        let (results, _) =
            crate::search::dispatch(&store, "txt size:>10m", crate::search::SearchMode::Auto, 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "large.txt");

        // 目录不匹配大小过滤
        let (results_dir, _) =
            crate::search::dispatch(&store, "folder size:>0", crate::search::SearchMode::Auto, 10);
        assert!(results_dir.is_empty());
    }

    #[test]
    fn test_parse_size_filters_compound_and_inverted() {
        // 多条件收敛交集
        let (clean1, f1) = parse_query_filter("test size:>10m size:<50m");
        assert_eq!(clean1, "test");
        assert_eq!(f1.size_filter.min_bytes, Some(10_000_000));
        assert_eq!(f1.size_filter.max_bytes, Some(50_000_000));

        // 倒置区间
        let (clean2, f2) = parse_query_filter("test size:>100m size:<10m");
        assert_eq!(clean2, "test");
        assert_eq!(f2.size_filter.min_bytes, Some(100_000_000));
        assert_eq!(f2.size_filter.max_bytes, Some(10_000_000));

        // 单点等值
        let (clean3, f3) = parse_query_filter("report size:10m");
        assert_eq!(clean3, "report");
        assert_eq!(f3.size_filter.min_bytes, Some(10_000_000));
        assert_eq!(f3.size_filter.max_bytes, Some(10_000_000));

        // 开区间语法
        let (clean4, f4) = parse_query_filter("data size:10m..");
        assert_eq!(clean4, "data");
        assert_eq!(f4.size_filter.min_bytes, Some(10_000_000));
        assert_eq!(f4.size_filter.max_bytes, None);

        let (clean5, f5) = parse_query_filter("data size:..50m");
        assert_eq!(clean5, "data");
        assert_eq!(f5.size_filter.min_bytes, None);
        assert_eq!(f5.size_filter.max_bytes, Some(50_000_000));

        // 大小写与复合单位
        let (clean6, f6) = parse_query_filter("doc size:>1.5M size:<2GB");
        assert_eq!(clean6, "doc");
        assert_eq!(f6.size_filter.min_bytes, Some(1_500_000));
        assert_eq!(f6.size_filter.max_bytes, Some(2_000_000_000));

        // 无效大小语法回退为检索词
        let (clean7, f7) = parse_query_filter("doc size:unknown");
        assert_eq!(clean7, "doc size:unknown");
        assert_eq!(f7.size_filter, SizeFilter::default());
    }

    #[test]
    fn test_compiled_filter_inverted_size_matches_zero_records() {
        let store = crate::indexer::IndexStore::new();
        store.replace(vec![
            crate::indexer::FileEntry {
                name: "file.txt".to_string(),
                path: "/file.txt".to_string(),
                is_dir: false,
                size: 20_000_000,
                modified: 0,
                created_at: 0,
                extension: "txt".to_string(),
            },
        ]);

        let (results, _) = crate::search::dispatch(
            &store,
            "file size:>100m size:<10m",
            crate::search::SearchMode::Auto,
            10,
        );
        assert!(results.is_empty(), "inverted size range must yield zero hits");
    }

    #[test]
    fn test_compiled_filter_combined_type_and_size() {
        let store = crate::indexer::IndexStore::new();
        store.replace(vec![
            crate::indexer::FileEntry {
                name: "small.rs".to_string(),
                path: "/small.rs".to_string(),
                is_dir: false,
                size: 1_000,
                modified: 0,
                created_at: 0,
                extension: "rs".to_string(),
            },
            crate::indexer::FileEntry {
                name: "large.rs".to_string(),
                path: "/large.rs".to_string(),
                is_dir: false,
                size: 20_000_000,
                modified: 0,
                created_at: 0,
                extension: "rs".to_string(),
            },
            crate::indexer::FileEntry {
                name: "large.txt".to_string(),
                path: "/large.txt".to_string(),
                is_dir: false,
                size: 20_000_000,
                modified: 0,
                created_at: 0,
                extension: "txt".to_string(),
            },
        ]);

        let (results, _) = crate::search::dispatch(
            &store,
            "large type:code size:>10m",
            crate::search::SearchMode::Auto,
            10,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "large.rs");
    }

    #[test]
    fn test_is_dotfile_query_rules() {
        // 典型点文件与配置项应当识别为 dotfile 而非扩展名
        assert!(is_dotfile_query(".env"));
        assert!(is_dotfile_query(".env.local"));
        assert!(is_dotfile_query(".gitignore"));
        assert!(is_dotfile_query(".dockerignore"));
        assert!(is_dotfile_query(".npmignore"));
        assert!(is_dotfile_query(".zshrc"));
        assert!(is_dotfile_query(".bashrc"));
        assert!(is_dotfile_query(".npmrc"));
        assert!(is_dotfile_query(".vimrc"));
        assert!(is_dotfile_query(".editorconfig"));
        assert!(is_dotfile_query(".clang-format"));
        assert!(is_dotfile_query(".bash_profile"));
        assert!(is_dotfile_query(".ds_store"));
        assert!(is_dotfile_query(".git"));
        assert!(is_dotfile_query(".idea"));
        assert!(is_dotfile_query(".vscode"));

        // 点文件家族常见前缀（键入过程与派生词元）亦应当识别为 dotfile
        assert!(is_dotfile_query(".giti"));
        assert!(is_dotfile_query(".gitig"));
        assert!(is_dotfile_query(".gitign"));
        assert!(is_dotfile_query(".docker"));
        assert!(is_dotfile_query(".zsh"));
        assert!(is_dotfile_query(".bash"));
        assert!(is_dotfile_query(".npm"));
        assert!(is_dotfile_query(".cargo"));

        // 普通扩展名与裸点号不应误判为 dotfile
        assert!(!is_dotfile_query(".rs"));
        assert!(!is_dotfile_query(".pdf"));
        assert!(!is_dotfile_query(".png"));
        assert!(!is_dotfile_query(".swift"));
        assert!(!is_dotfile_query(".c"));
        assert!(!is_dotfile_query(".rc"));
        assert!(!is_dotfile_query("."));
        assert!(!is_dotfile_query(""));
    }

    #[test]
    fn test_parse_query_filter_dotfiles_and_explicit_type_all() {
        let (clean, filter) = parse_query_filter(".env");
        assert_eq!(clean, ".env");
        assert!(filter.custom_exts.is_empty());
        assert!(!filter.has_explicit_type_all);

        let (clean2, filter2) = parse_query_filter(".gitignore");
        assert_eq!(clean2, ".gitignore");
        assert!(filter2.custom_exts.is_empty());

        // 点文件前缀在单词元输入下保留为检索词，不被误吞为扩展名
        let (clean_giti, filter_giti) = parse_query_filter(".giti");
        assert_eq!(clean_giti, ".giti");
        assert!(filter_giti.custom_exts.is_empty());

        // 单词元纯扩展名格式保留在 clean 中由底层索引做扩展名匹配或子串回退
        let (clean_rs, filter_rs) = parse_query_filter(".rs");
        assert_eq!(clean_rs, ".rs");
        assert!(filter_rs.custom_exts.is_empty());

        // 多词元输入下，后置独立扩展名正常识别为过滤条件
        let (clean_multi, filter_multi) = parse_query_filter("main .rs");
        assert_eq!(clean_multi, "main");
        assert_eq!(filter_multi.custom_ext.as_deref(), Some("rs"));

        let (clean3, filter3) = parse_query_filter("type:all");
        assert_eq!(clean3, "");
        assert!(filter3.has_explicit_type_all);

        let (clean4, filter4) = parse_query_filter("type:all foo");
        assert_eq!(clean4, "foo");
        assert!(filter4.has_explicit_type_all);
    }

    #[test]
    fn test_compiled_filter_folder_with_size_unlocks_deadlock() {
        let store = crate::indexer::IndexStore::new();
        store.replace(vec![
            crate::indexer::FileEntry {
                name: "dir_small".to_string(),
                path: "/dir_small".to_string(),
                is_dir: true,
                size: 64,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            crate::indexer::FileEntry {
                name: "dir_large".to_string(),
                path: "/dir_large".to_string(),
                is_dir: true,
                size: 20_000_000,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            crate::indexer::FileEntry {
                name: "file_small.txt".to_string(),
                path: "/file_small.txt".to_string(),
                is_dir: false,
                size: 64,
                modified: 0,
                created_at: 0,
                extension: "txt".to_string(),
            },
        ]);

        // 纯文件夹检索 + 大小约束：目录参与大小比较，普通文件被拒绝，解除历史恒空死锁
        let (results, _) = crate::search::dispatch(
            &store,
            "type:folder size:<10m",
            crate::search::SearchMode::Auto,
            10,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "dir_small");

        // 仅匹配超大目录
        let (results_large, _) = crate::search::dispatch(
            &store,
            "type:folder size:>10m",
            crate::search::SearchMode::Auto,
            10,
        );
        assert_eq!(results_large.len(), 1);
        assert_eq!(results_large[0].entry.name, "dir_large");
    }

    #[test]
    fn test_root_dotfiles_matched_by_custom_ext() {
        let store = crate::indexer::IndexStore::new();
        store.replace(vec![
            crate::indexer::FileEntry {
                name: ".gitignore".to_string(),
                path: "/project/.gitignore".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            crate::indexer::FileEntry {
                name: "test.gitignore".to_string(),
                path: "/project/test.gitignore".to_string(),
                is_dir: false,
                size: 200,
                modified: 0,
                created_at: 0,
                extension: "gitignore".to_string(),
            },
            crate::indexer::FileEntry {
                name: ".zshrc".to_string(),
                path: "/Users/me/.zshrc".to_string(),
                is_dir: false,
                size: 300,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
        ]);

        let (results_git, _) = crate::search::dispatch(
            &store,
            "ext:gitignore",
            crate::search::SearchMode::Auto,
            10,
        );
        let names_git: Vec<_> = results_git.iter().map(|r| r.entry.name.as_str()).collect();
        assert!(names_git.contains(&".gitignore"));
        assert!(names_git.contains(&"test.gitignore"));

        let (results_zsh, _) = crate::search::dispatch(
            &store,
            "ext:zshrc",
            crate::search::SearchMode::Auto,
            10,
        );
        assert_eq!(results_zsh.len(), 1);
        assert_eq!(results_zsh[0].entry.name, ".zshrc");
    }

    #[test]
    fn test_type_code_matches_engineering_dotfiles() {
        let store = crate::indexer::IndexStore::new();
        store.replace(vec![
            crate::indexer::FileEntry {
                name: ".gitignore".to_string(),
                path: "/project/.gitignore".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            crate::indexer::FileEntry {
                name: ".editorconfig".to_string(),
                path: "/project/.editorconfig".to_string(),
                is_dir: false,
                size: 150,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            crate::indexer::FileEntry {
                name: ".Trash".to_string(),
                path: "/Users/me/.Trash".to_string(),
                is_dir: true,
                size: 0,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            crate::indexer::FileEntry {
                name: "normal.txt".to_string(),
                path: "/project/normal.txt".to_string(),
                is_dir: false,
                size: 500,
                modified: 0,
                created_at: 0,
                extension: "txt".to_string(),
            },
            crate::indexer::FileEntry {
                name: ".localized".to_string(),
                path: "/Users/me/.localized".to_string(),
                is_dir: false,
                size: 0,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            crate::indexer::FileEntry {
                name: ".DS_Store".to_string(),
                path: "/project/.DS_Store".to_string(),
                is_dir: false,
                size: 6148,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            crate::indexer::FileEntry {
                name: ".zsh_history".to_string(),
                path: "/Users/me/.zsh_history".to_string(),
                is_dir: false,
                size: 1024,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            crate::indexer::FileEntry {
                name: "Makefile".to_string(),
                path: "/project/Makefile".to_string(),
                is_dir: false,
                size: 1000,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
        ]);

        let (results, _) = crate::search::dispatch(
            &store,
            "type:code",
            crate::search::SearchMode::Auto,
            10,
        );
        let names: Vec<_> = results.iter().map(|r| r.entry.name.as_str()).collect();
        assert!(names.contains(&".gitignore"));
        assert!(names.contains(&".editorconfig"));
        assert!(names.contains(&"Makefile"));
        assert!(!names.contains(&".Trash"));
        assert!(!names.contains(&".localized"));
        assert!(!names.contains(&".DS_Store"));
        assert!(!names.contains(&".zsh_history"));
        assert!(!names.contains(&"normal.txt"));
    }
}
