use std::borrow::Cow;

use crate::compact_index::{CompactIndex, CompactRecord, EntryView};

/// 将用户或过滤规则中的扩展名规范化为 NFC 小写形态。
/// 纯 ASCII 仅做 ASCII 小写化，非 ASCII 经 Unicode NFC 与大小写折叠消除 NFD/NFC 差异。
#[inline]
pub(crate) fn normalize_ext_filter(raw: &str) -> String {
    let clean = raw.trim().trim_start_matches('.').trim();
    if clean.is_ascii() {
        clean.to_ascii_lowercase()
    } else {
        crate::cjk::fold_str(clean)
    }
}

/// 判断索引扩展名（可能包含大写或 NFD 组合字符）是否与已折叠的目标扩展名等价。
#[inline]
pub(crate) fn ext_matches_folded(ext: &str, target_folded: &str) -> bool {
    if target_folded.is_ascii() {
        ext.eq_ignore_ascii_case(target_folded)
    } else {
        crate::cjk::fold_cow(ext) == target_folded
    }
}

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

/// 查询级过滤条件，包含预设文件类别、自定义扩展名、文件大小区间与 `!exclude` 否定排除规则。
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
    /// 由否定 `!.ext` 推断的长扩展名候选（>5 字符，如 `.markdown` / `.hidden`），
    /// 供分发层依据索引扩展名池裁决：若该扩展名不存在于索引中，自动回退为 `exclude_terms` 词元排除。
    pub inferred_exclude_exts: Vec<(String, String)>,
    /// 通过 `!term` / `!"phrase"` / `!.gitignore` 指定排除的文件名或路径子串词元。
    pub exclude_terms: Vec<String>,
    /// 通过 `!.ext` 指定排除的扩展名（不含前导点号，已归一化小写/折叠）。
    pub exclude_exts: Vec<String>,
    /// 通过 `!dir/` / `!src/test` / `!"my dir/"` 指定排除的路径分量或子路径（已剥离尾随 `/`）。
    pub exclude_paths: Vec<String>,
}

impl QueryFilter {
    /// 将内联过滤条件与外层传入条件合并（内联类别/扩展/大小覆盖外层默认值，否定条件取并集）。
    pub fn merged_with(&self, caller: &QueryFilter) -> QueryFilter {
        let effective_file_type =
            if self.file_type != FileType::All || self.has_explicit_type_all {
                self.file_type
            } else {
                caller.file_type
            };

        let effective_custom_ext = self
            .custom_ext
            .clone()
            .or_else(|| caller.custom_ext.clone());

        let effective_file_types = if !self.file_types.is_empty() || self.has_explicit_type_all {
            self.file_types.clone()
        } else if !caller.file_types.is_empty() {
            caller.file_types.clone()
        } else if effective_file_type != FileType::All {
            vec![effective_file_type]
        } else {
            Vec::new()
        };

        let effective_custom_exts = if !self.custom_exts.is_empty() {
            self.custom_exts.clone()
        } else if !caller.custom_exts.is_empty() {
            caller.custom_exts.clone()
        } else if let Some(ext) = &effective_custom_ext {
            vec![ext.clone()]
        } else {
            Vec::new()
        };

        let effective_size_filter = if !self.size_filter.is_unconstrained() {
            self.size_filter
        } else {
            caller.size_filter
        };

        let mut exclude_terms = caller.exclude_terms.clone();
        for t in &self.exclude_terms {
            if !exclude_terms.contains(t) {
                exclude_terms.push(t.clone());
            }
        }
        let mut exclude_exts = caller.exclude_exts.clone();
        for e in &self.exclude_exts {
            if !exclude_exts.contains(e) {
                exclude_exts.push(e.clone());
            }
        }
        let mut exclude_paths = caller.exclude_paths.clone();
        for p in &self.exclude_paths {
            if !exclude_paths.contains(p) {
                exclude_paths.push(p.clone());
            }
        }

        QueryFilter {
            file_type: effective_file_type,
            custom_ext: effective_custom_ext,
            file_types: effective_file_types,
            custom_exts: effective_custom_exts,
            size_filter: effective_size_filter,
            has_explicit_type_all: self.has_explicit_type_all || caller.has_explicit_type_all,
            inferred_exts: self.inferred_exts.clone(),
            inferred_exclude_exts: self.inferred_exclude_exts.clone(),
            exclude_terms,
            exclude_exts,
            exclude_paths,
        }
    }
}

/// 词法状态机产出的结构化检索词元，保留引号保护与 `!` 否定状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryToken {
    /// 剥离成对双引号及外部 `\"`/`\!`/`\|` 转义与前导 `!` 后的词元内容。
    pub text: String,
    /// 该词元整体是否由成对双引号包裹（如 `"hello world"` 或 `!"my dir/"`）。
    pub quoted: bool,
    /// 是否以未转义、未加引号的 `!` 开头。
    pub negated: bool,
    /// 原始输入片段（在推断 `.ext` 回退等场景中用于还原）。
    pub raw: String,
    /// 词元外部是否存在反斜杠转义（如 `\|`）。
    pub had_escaped_meta: bool,
    /// 词元是否经粘贴路径/URI/shell 转义清洗还原；为 true 时禁止再次推导为正则语法糖。
    pub was_sanitized_pasted_path: bool,
}

/// 结构化词法与过滤解析结果，供下游搜索引擎直接使用词元边界与正负过滤条件。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedQuery {
    /// 剥离过滤语法后保留的正向内容词元列表。
    pub positive_tokens: Vec<QueryToken>,
    /// 兼容字符串拼接表示（当词元包含空格时下游优先读取 `positive_tokens`）。
    pub clean_query: String,
    /// 解析提取出的内联正负过滤条件。
    pub filter: QueryFilter,
}

/// 查找从 `after_open`（跳过开引号之后）开始的下一个未转义闭合 `"` 字节偏移。
pub(crate) fn find_closing_quote(input: &str, after_open: usize) -> Option<usize> {
    let mut backslashes = 0usize;
    for (offset, ch) in input[after_open..].char_indices() {
        if ch == '\\' {
            backslashes += 1;
        } else if ch == '"' && backslashes.is_multiple_of(2) {
            return Some(after_open + offset);
        } else {
            backslashes = 0;
        }
    }
    None
}

fn is_file_uri(s: &str) -> bool {
    s.as_bytes()
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"file://"))
}

/// 仅当内部明确以文件 URI、绝对 `/` 或 `~/` 开头，且不触发行内 `/.../` 正则语法糖时，
/// 才允许剥离外部粘贴引号，避免破坏 `"foo/bar"`、`"hello world"` 与 `"/a\s+b/"` 等字面查询。
fn has_pasted_path_signature(inner: &str) -> bool {
    let trimmed = inner.trim();
    if trimmed.is_empty() {
        return false;
    }
    if is_file_uri(trimmed) || trimmed.starts_with("~/") {
        return true;
    }
    if trimmed.starts_with('/') && trimmed.len() > 1 {
        let no_trailing = trimmed.strip_suffix('/').unwrap_or(trimmed);
        let multi_segment_without_backslash =
            no_trailing[1..].contains('/') && !trimmed.contains('\\');
        return super::regex_sugar::parse_regex_sugar(trimmed).is_none()
            || multi_segment_without_backslash;
    }
    false
}

fn decode_hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 仅在剥离 `file://` 后解码 `%XX` UTF-8 字节；非合法十六进制转义原样保留，非法 UTF-8 整体回退原文。
fn percent_decode_utf8_or_verbatim(s: &str) -> Cow<'_, str> {
    let bytes = s.as_bytes();
    if !bytes.contains(&b'%') {
        return Cow::Borrowed(s);
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'%' && idx + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) =
                (decode_hex_nibble(bytes[idx + 1]), decode_hex_nibble(bytes[idx + 2]))
            {
                out.push((hi << 4) | lo);
                idx += 3;
                continue;
            }
        }
        out.push(bytes[idx]);
        idx += 1;
    }
    match std::str::from_utf8(&out) {
        Ok(decoded) => Cow::Owned(decoded.to_string()),
        Err(_) => Cow::Borrowed(s),
    }
}

fn collapse_slashes_cow(s: &str) -> String {
    if !s.contains("//") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut prev_slash = false;
    for ch in s.chars() {
        if ch == '/' {
            if !prev_slash {
                out.push('/');
            }
            prev_slash = true;
        } else {
            out.push(ch);
            prev_slash = false;
        }
    }
    out
}

fn unescape_posix_shell_chars(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next_ch) = chars.peek() {
                if matches!(next_ch, ' ' | '(' | ')' | '&' | '\'') {
                    chars.next();
                    out.push(next_ch);
                    continue;
                }
            }
        }
        out.push(ch);
    }
    out
}

/// 清洗单个粘贴路径片段（解包路径特征外层引号、剥离 `file://` 与可选 `%XX` 解码、POSIX shell 转义还原）。
pub fn sanitize_path_fragment(input: &str) -> String {
    let mut curr = input.trim();
    for _ in 0..2 {
        let mut unwrapped = None;
        for quote in ['"', '\''] {
            if let Some(inner) = curr
                .strip_prefix(quote)
                .and_then(|rest| rest.strip_suffix(quote))
            {
                if has_pasted_path_signature(inner) {
                    unwrapped = Some(inner.trim());
                    break;
                }
            }
        }
        match unwrapped {
            Some(next) => curr = next,
            None => break,
        }
    }

    let mut uri_stripped = false;
    if curr
        .as_bytes()
        .get(..16)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"file://localhost"))
        && curr.as_bytes().get(16) == Some(&b'/')
    {
        curr = &curr[16..];
        uri_stripped = true;
    } else if is_file_uri(curr) {
        curr = &curr[7..];
        uri_stripped = true;
    }

    if uri_stripped {
        let decoded = percent_decode_utf8_or_verbatim(curr);
        return collapse_slashes_cow(decoded.as_ref());
    }

    if !curr.starts_with("regex:")
        && super::regex_sugar::parse_regex_sugar(curr).is_none()
        && (curr.contains('/') || curr.starts_with("~/"))
    {
        return collapse_slashes_cow(&unescape_posix_shell_chars(curr));
    }

    curr.to_string()
}

/// 清洗粘贴的输入文本；单路径/URI 片段直接就地清洗，普通多词查询按分词边界分别保留与清洗。
pub fn sanitize_path_input(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let has_paired_outer_quote = ['"', '\''].iter().any(|&quote| {
        trimmed
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
            .is_some_and(has_pasted_path_signature)
    });
    if has_paired_outer_quote
        || (!trimmed.chars().any(char::is_whitespace) && is_file_uri(trimmed))
    {
        return sanitize_path_fragment(trimmed);
    }

    let tokens = tokenize_query(trimmed);
    if tokens.is_empty() {
        return String::new();
    }
    tokens
        .into_iter()
        .map(|tok| {
            if tok.negated {
                if tok.quoted {
                    format!("!\"{}\"", tok.text)
                } else {
                    format!("!{}", tok.text)
                }
            } else if tok.quoted {
                format!("\"{}\"", tok.text)
            } else {
                tok.text
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn find_closing_single_quote_at_token_end(input: &str, start: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    for (offset, &b) in bytes[start..].iter().enumerate() {
        if b == b'\'' {
            let idx = start + offset;
            let after = idx + 1;
            if after >= input.len()
                || input[after..].chars().next().is_some_and(char::is_whitespace)
            {
                return Some(idx);
            }
        }
    }
    None
}

fn unescape_double_quoted_inner(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'"') {
            chars.next();
            out.push('"');
        } else {
            out.push(c);
        }
    }
    out
}

/// 仅当连续 shell 转义片段内确实出现 `/` 时才允许剥离 `\ ` / `\(` 等 POSIX shell 转义，避免跨普通关键词误并词。
fn token_can_unescape_posix_shell_escapes(rest: &str) -> bool {
    if rest.is_empty() || rest.starts_with('"') {
        return false;
    }
    let mut saw_slash = false;
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next_ch) = chars.peek() {
                if matches!(next_ch, ' ' | '(' | ')' | '&' | '\'' | '"' | '!' | '|') {
                    chars.next();
                    continue;
                }
            }
            continue;
        }
        if c.is_whitespace() || c == '"' {
            break;
        }
        if c == '/' {
            saw_slash = true;
        }
    }
    saw_slash
}

/// 使用状态机扫描查询字符串，支持成对双引号 `"..."`、路径特征单双引号解包、URI/shell 转义清洗与正则透传保护。
///
/// - 仅在内部具备明确粘贴路径或 URI 特征时剥离外层成对引号，普通 `"foo/bar"`、`"hello world"`、`"README's"` 与 `"/a\s+b/"` 保持字面语义；
/// - 非路径情境下 `'` 始终视作普通字面字符，确保 `John's report.pdf` 与 `README's` 自然匹配；
/// - 未闭合的双引号 `"` 自动回退为普通字面量字符；
/// - 忽略空引号 `""`、`!""` 及孤立的 `!`；
/// - 针对 `regex:...` 词元与可被解析为正则语法糖的 `/.../` 词元保留反斜杠原貌，不破坏 `\D+`、`\.`、`\|` 等正则语法。
pub fn tokenize_query(input: &str) -> Vec<QueryToken> {
    let mut tokens = Vec::new();
    let len = input.len();
    let mut i = 0;

    while i < len {
        // 跳过前导空白
        while i < len {
            let ch = input[i..].chars().next().unwrap();
            if ch.is_whitespace() {
                i += ch.len_utf8();
            } else {
                break;
            }
        }
        if i >= len {
            break;
        }

        let token_start = i;
        let rest = &input[i..];

        // regex: 与行内正则语法糖 /.../ 整体保持反斜杠原样透传（不剥离 \D+ / \. / \|）
        let is_verbatim_regex = rest.starts_with("regex:") || {
            let ws_end = rest
                .char_indices()
                .find(|(_, c)| c.is_whitespace())
                .map(|(pos, _)| pos)
                .unwrap_or(rest.len());
            let candidate = &rest[..ws_end];
            candidate.starts_with('/') && super::regex_sugar::parse_regex_sugar(candidate).is_some()
        };

        if is_verbatim_regex {
            let ws_end = rest
                .char_indices()
                .find(|(_, c)| c.is_whitespace())
                .map(|(pos, _)| pos)
                .unwrap_or(rest.len());
            let raw_slice = &rest[..ws_end];
            tokens.push(QueryToken {
                text: raw_slice.to_string(),
                quoted: false,
                negated: false,
                raw: raw_slice.to_string(),
                had_escaped_meta: false,
                was_sanitized_pasted_path: false,
            });
            i += ws_end;
            continue;
        }

        let mut negated = false;
        let first_char = rest.chars().next().unwrap();
        if first_char == '!' {
            negated = true;
            i += first_char.len_utf8();
            if i >= len || input[i..].chars().next().is_some_and(char::is_whitespace) {
                // 孤立 '!' 忽略
                continue;
            }
        }

        let body_start = i;
        let body_rest = &input[body_start..];

        // 整词粘贴路径/URI 快速通道：成对双引号、成对单引号、直接粘贴的 file://
        if body_rest.starts_with('"') {
            if let Some(close_idx) = find_closing_quote(input, body_start + 1) {
                let after = close_idx + 1;
                if after >= len || input[after..].chars().next().is_some_and(char::is_whitespace) {
                    let inner = unescape_double_quoted_inner(&input[body_start + 1..close_idx]);
                    if has_pasted_path_signature(&inner) {
                        let sanitized = sanitize_path_fragment(&inner);
                        i = after;
                        if !sanitized.is_empty() {
                            tokens.push(QueryToken {
                                text: sanitized,
                                quoted: false,
                                negated,
                                raw: input[token_start..i].to_string(),
                                had_escaped_meta: false,
                                was_sanitized_pasted_path: true,
                            });
                        }
                        continue;
                    }
                }
            }
        } else if body_rest.starts_with('\'') {
            if let Some(close_idx) = find_closing_single_quote_at_token_end(input, body_start + 1) {
                let inner = &input[body_start + 1..close_idx];
                if has_pasted_path_signature(inner) {
                    let sanitized = sanitize_path_fragment(inner);
                    i = close_idx + 1;
                    if !sanitized.is_empty() {
                        tokens.push(QueryToken {
                            text: sanitized,
                            quoted: false,
                            negated,
                            raw: input[token_start..i].to_string(),
                            had_escaped_meta: false,
                            was_sanitized_pasted_path: true,
                        });
                    }
                    continue;
                }
            }
        } else if let Some((prefix, path_rest, ws_end)) = body_rest
            .strip_prefix("in:")
            .map(|rest| ("in:", rest))
            .or(Some(("", body_rest)))
            .and_then(|(prefix, rest)| {
                if !is_file_uri(rest) {
                    return None;
                }
                let ws_end = rest
                    .char_indices()
                    .find(|(_, c)| c.is_whitespace())
                    .map(|(pos, _)| pos)
                    .unwrap_or(rest.len());
                // 未加引号的 in: 负载若含管道符 `|`，交回常规解析以保留并集切分语义
                if !prefix.is_empty() && rest[..ws_end].contains('|') {
                    return None;
                }
                Some((prefix, rest, ws_end))
            })
        {
            let raw_slice = &path_rest[..ws_end];
            let sanitized = sanitize_path_fragment(raw_slice);
            i = body_start + prefix.len() + ws_end;
            if !sanitized.is_empty() {
                let text = if prefix.is_empty() {
                    sanitized
                } else {
                    format!("{prefix}{sanitized}")
                };
                tokens.push(QueryToken {
                    text,
                    quoted: false,
                    negated,
                    raw: input[token_start..i].to_string(),
                    had_escaped_meta: false,
                    was_sanitized_pasted_path: true,
                });
            }
            continue;
        }

        let allow_posix_shell_escapes = token_can_unescape_posix_shell_escapes(body_rest);
        let mut text = String::new();
        let mut started_with_quote = false;
        let mut has_unquoted_content = false;
        let mut had_escaped_meta = false;
        let mut was_sanitized_pasted_path = false;

        while i < len {
            let ch = input[i..].chars().next().unwrap();
            if ch.is_whitespace() {
                break;
            }

            if ch == '"' {
                if let Some(close_idx) = find_closing_quote(input, i + 1) {
                    if i == body_start {
                        started_with_quote = true;
                    }
                    let inner = unescape_double_quoted_inner(&input[i + 1..close_idx]);
                    if text == "in:" && has_pasted_path_signature(&inner) {
                        text.push_str(&sanitize_path_fragment(&inner));
                        was_sanitized_pasted_path = true;
                        has_unquoted_content = true;
                    } else {
                        text.push_str(&inner);
                    }
                    i = close_idx + 1;
                    continue;
                }
                // 未闭合的双引号回退为普通字面字符 '"'
                text.push('"');
                has_unquoted_content = true;
                i += 1;
                continue;
            }

            if ch == '\'' && text == "in:" {
                if let Some(close_idx) = find_closing_single_quote_at_token_end(input, i + 1) {
                    let inner = &input[i + 1..close_idx];
                    if has_pasted_path_signature(inner) {
                        text.push_str(&sanitize_path_fragment(inner));
                        was_sanitized_pasted_path = true;
                        has_unquoted_content = true;
                        i = close_idx + 1;
                        continue;
                    }
                }
            }

            if ch == '\\' {
                let next_idx = i + 1;
                if let Some(next_ch) = input[next_idx..].chars().next() {
                    if matches!(next_ch, '"' | '!' | '|') {
                        if next_ch == '|' {
                            had_escaped_meta = true;
                        }
                        text.push(next_ch);
                        has_unquoted_content = true;
                        i = next_idx + next_ch.len_utf8();
                        continue;
                    }
                    if allow_posix_shell_escapes && matches!(next_ch, ' ' | '(' | ')' | '&' | '\'')
                    {
                        text.push(next_ch);
                        has_unquoted_content = true;
                        was_sanitized_pasted_path = true;
                        i = next_idx + next_ch.len_utf8();
                        continue;
                    }
                }
                text.push('\\');
                has_unquoted_content = true;
                i += 1;
                continue;
            }

            text.push(ch);
            has_unquoted_content = true;
            i += ch.len_utf8();
        }

        if text.is_empty() {
            // 忽略空引号 "" / !""
            continue;
        }

        if was_sanitized_pasted_path {
            text = collapse_slashes_cow(&text);
        }

        let quoted = started_with_quote && !has_unquoted_content;
        tokens.push(QueryToken {
            text,
            quoted,
            negated,
            raw: input[token_start..i].to_string(),
            had_escaped_meta,
            was_sanitized_pasted_path,
        });
    }

    tokens
}

/// 判断经剥离 `/` 规范化后的否定路径片段（非绝对路径时）或绝对路径是否命中给定路径串。
pub(crate) fn path_matches_excluded_subpath(candidate_path: &str, needle_folded: &str) -> bool {
    if needle_folded.is_empty() {
        return false;
    }
    let cand_cow = crate::cjk::fold_cow(candidate_path);
    let cand = cand_cow.as_ref();

    if needle_folded.starts_with('/') {
        return cand == needle_folded
            || (cand.starts_with(needle_folded)
                && cand.as_bytes().get(needle_folded.len()) == Some(&b'/'));
    }

    let n_len = needle_folded.len();
    let mut search_from = 0usize;
    while let Some(rel_pos) = cand[search_from..].find(needle_folded) {
        let pos = search_from + rel_pos;
        let left_ok = pos == 0 || cand.as_bytes()[pos - 1] == b'/';
        let right_idx = pos + n_len;
        let right_ok = right_idx == cand.len() || cand.as_bytes()[right_idx] == b'/';
        if left_ok && right_ok {
            return true;
        }
        let step = cand[pos..].chars().next().map_or(1, char::len_utf8);
        search_from = pos + step;
    }
    false
}

/// 匹配当前大小写及 NFC/CJK 折叠策略的否定子串比对。
#[inline]
pub(crate) fn text_contains_excluded_term(haystack: &str, term: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        let hay_norm = crate::cjk::normalize_and_simplify_cow(haystack);
        let term_norm = crate::cjk::normalize_and_simplify_cow(term);
        hay_norm.contains(term_norm.as_ref())
    } else {
        let hay_folded = crate::cjk::fold_cow(haystack);
        let term_folded = crate::cjk::fold_cow(term);
        hay_folded.contains(term_folded.as_ref())
    }
}

/// 规范化 `!path` 否定路径模式：展开 `~/`、合并重复 `/` 并剥离尾部 `/`。
fn normalize_exclude_path_pattern(raw: &str) -> String {
    let expanded = super::path_query::expand_tilde(raw);
    let collapsed = super::path_query::collapse_consecutive_slashes(&expanded);
    let trimmed = collapsed.trim_end_matches('/');
    crate::cjk::fold_str(trimmed)
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

    // 2. 下限前缀：>=（闭区间）或 >（严格开区间，如 size:>0 排除 0 字节空文件）
    if let Some(rest) = payload.strip_prefix(">=") {
        let val = parse_size_bytes(rest)?;
        return Some(SizeFilter {
            min_bytes: Some(val),
            max_bytes: None,
        });
    }
    if let Some(rest) = payload.strip_prefix('>') {
        let val = parse_size_bytes(rest)?;
        return Some(if val == u64::MAX {
            SizeFilter {
                min_bytes: Some(1),
                max_bytes: Some(0),
            }
        } else {
            SizeFilter {
                min_bytes: Some(val + 1),
                max_bytes: None,
            }
        });
    }

    // 3. 上限前缀：<=（闭区间）或 <（严格开区间，如 size:<0 返回空区间）
    if let Some(rest) = payload.strip_prefix("<=") {
        let val = parse_size_bytes(rest)?;
        return Some(SizeFilter {
            min_bytes: None,
            max_bytes: Some(val),
        });
    }
    if let Some(rest) = payload.strip_prefix('<') {
        let val = parse_size_bytes(rest)?;
        return Some(if val == 0 {
            SizeFilter {
                min_bytes: Some(1),
                max_bytes: Some(0),
            }
        } else {
            SizeFilter {
                min_bytes: None,
                max_bytes: Some(val - 1),
            }
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
/// 解析检索输入中的类型、扩展名、文件大小与 `!exclude` 过滤指令，返回结构化词法产物 `ParsedQuery`。
///
/// 支持语法规则：
/// 1. `type:<category>`：设置文件分类（如 `type:code`、`type:doc`、`type:img` 等），支持逗号分隔复合分类（如 `type:code,doc`）。未知分类将退化为普通检索词。
/// 2. `ext:<extension>`：指定精确扩展名（如 `ext:png`、`ext:.pdf` 等），支持逗号分隔多扩展名（如 `ext:rs,swift,ts`），自动剥离前导点号。
/// 3. `size:<condition>`：指定文件大小约束（如 `size:>10m`、`size:<500k`、`size:1.5m..100m` 等），支持十进制单位与多个条件交集收敛。
/// 4. 独立 `.ext`：未加引号且多正向词元存在时识别为隐式自定义扩展名。
/// 5. `!exclude` 否定词元：
///    - 未加引号的 `!.ext`（且非 `is_dotfile_query`）-> `exclude_exts`；
///    - 含 `/`（如 `!node_modules/`、`!src/test`、`!"my dir/"`）-> 剥离尾 `/` 加入 `exclude_paths`；
///    - 其余（`!test`、`!.gitignore`、`!".pdf"`）-> `exclude_terms`。
/// 6. 成对双引号 `"..."` 包裹的词元保留字面含义，不参与过滤标签与模式触发解析。
pub fn parse_query_structured(input: &str) -> ParsedQuery {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return ParsedQuery::default();
    }

    let tokens = tokenize_query(trimmed);
    let positive_candidate_count = tokens.iter().filter(|t| !t.negated).count();

    let mut positive_tokens = Vec::new();
    let mut file_types = Vec::new();
    let mut custom_exts = Vec::new();
    let mut inferred_exts = Vec::new();
    let mut inferred_exclude_exts = Vec::new();
    let mut size_filter = SizeFilter::default();
    let mut has_explicit_type_all = false;
    let mut exclude_terms = Vec::new();
    let mut exclude_exts = Vec::new();
    let mut exclude_paths = Vec::new();

    for token in tokens {
        if token.negated {
            let payload = token.text.as_str();
            if !token.quoted
                && !is_dotfile_query(payload)
                && payload.starts_with('.')
                && payload.len() > 1
                && !payload.contains('/')
                && payload[1..].chars().count() <= 16
                && payload[1..].chars().all(|c| c.is_alphanumeric())
            {
                let ext = normalize_ext_filter(payload);
                if !ext.is_empty() {
                    if !exclude_exts.contains(&ext) {
                        exclude_exts.push(ext.clone());
                    }
                    // 超过 5 字符的否定点词（如 !.markdown 与 !.hidden）登记为待索引校验候选，
                    // 避免误把点文件前缀排除成不存在的扩展名。
                    if payload[1..].chars().count() > 5 {
                        inferred_exclude_exts.push((ext, payload.to_string()));
                    }
                }
            } else if payload.contains('/') {
                let norm = normalize_exclude_path_pattern(payload);
                if !norm.is_empty() && !exclude_paths.contains(&norm) {
                    exclude_paths.push(norm);
                }
            } else if !payload.is_empty() {
                let term = payload.to_string();
                if !exclude_terms.contains(&term) {
                    exclude_terms.push(term);
                }
            }
            continue;
        }

        // 成对双引号保护的词元不解析为 type:/ext:/size:/.ext，直接作为正向字面词元保留
        if token.quoted {
            positive_tokens.push(token);
            continue;
        }

        let term = token.text.as_str();

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
            positive_tokens.push(token);
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
                    let lower = normalize_ext_filter(p);
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
            positive_tokens.push(token);
            continue;
        }

        // 4. 独立 .ext（非隐藏点文件与相对路径守卫；上限 16 字符以兼容 .markdown/.numbers/.xcodeproj 等长扩展名，
        // 后续由 `resolve_inferred_exts` 依托索引真实扩展名池裁决）
        if positive_candidate_count > 1
            && term.starts_with('.')
            && term.len() > 1
            && !term.contains('/')
        {
            if is_dotfile_query(term) {
                positive_tokens.push(token);
                continue;
            }
            let candidate = &term[1..];
            if !candidate.is_empty()
                && candidate.chars().count() <= 16
                && candidate.chars().all(|c| c.is_alphanumeric())
            {
                let lower = normalize_ext_filter(candidate);
                if !lower.is_empty() {
                    if !custom_exts.contains(&lower) {
                        custom_exts.push(lower.clone());
                    }
                    inferred_exts.push((lower, term.to_string()));
                    continue;
                }
            }
        }

        positive_tokens.push(token);
    }

    let primary_file_type = file_types.first().copied().unwrap_or(FileType::All);
    let primary_custom_ext = custom_exts.first().cloned();
    let clean_query = positive_tokens
        .iter()
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    ParsedQuery {
        positive_tokens,
        clean_query,
        filter: QueryFilter {
            file_type: primary_file_type,
            custom_ext: primary_custom_ext,
            file_types,
            custom_exts,
            size_filter,
            has_explicit_type_all,
            inferred_exts,
            inferred_exclude_exts,
            exclude_terms,
            exclude_exts,
            exclude_paths,
        },
    }
}

/// 兼容旧签名入口：提取清理后的查询文本与内联 `QueryFilter`。
pub fn parse_query_filter(input: &str) -> (String, QueryFilter) {
    let parsed = parse_query_structured(input);
    (parsed.clean_query, parsed.filter)
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

/// 判断 `(parent, name)` 组成的非原始记录路径是否匹配包含 `/` 的否定路径模式（多段相对路径或绝对路径）。
fn full_path_matches_excluded_subpath(parent: &str, name: &str, pat_folded: &str) -> bool {
    let Some((pat_prefix, pat_leaf)) = pat_folded.rsplit_once('/') else {
        return false;
    };
    if crate::cjk::fold_cow(name).as_ref() != pat_leaf {
        return false;
    }
    let parent_folded = crate::cjk::fold_cow(parent);
    let raw_parent = parent_folded.as_ref();
    // 非根父目录在 CompactIndex 中保留末尾 '/'（如 "/proj/src/"），剥离后再与 rsplit_once 得到的无尾斜杠前缀比对；
    // 根目录 "/"（len == 1）需保持原貌以便 !/test 精确命中根下条目。
    let parent_str = if raw_parent.len() > 1 {
        raw_parent.strip_suffix('/').unwrap_or(raw_parent)
    } else {
        raw_parent
    };
    if pat_folded.starts_with('/') {
        if pat_prefix.is_empty() {
            parent_str == "/"
        } else {
            parent_str == pat_prefix
        }
    } else {
        parent_str == pat_prefix
            || (parent_str.len() > pat_prefix.len()
                && parent_str.ends_with(pat_prefix)
                && parent_str.as_bytes()[parent_str.len() - pat_prefix.len() - 1] == b'/')
    }
}

/// 针对当前紧凑索引完成编译的记录过滤器。
///
/// 通过在检索前预先将过滤规则投影为扩展名、文件名与父目录池位掩码，
/// 在单趟扫描时将代价较高的字符串比对退化为 O(1) 的下标寻址与位判断，
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
    has_scope_filter: bool,
    scope_candidates: Vec<super::path_query::PathCandidate>,
    scope_parent_mask: Vec<bool>,
    scope_raw_path_mask: Vec<bool>,
    has_negative_filter: bool,
    exclude_case_sensitive: bool,
    exclude_terms: Vec<String>,
    exclude_exts: Vec<String>,
    exclude_rel_single_paths: Vec<String>,
    exclude_slash_paths: Vec<String>,
    exclude_ext_mask: Vec<bool>,
    exclude_name_mask: Vec<bool>,
    exclude_parent_mask: Vec<bool>,
    exclude_raw_path_mask: Vec<bool>,
    exclude_path_leaf_mask: Vec<bool>,
    exclude_multi_leaf_mask: Vec<bool>,
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
            has_scope_filter: false,
            scope_candidates: Vec::new(),
            scope_parent_mask: Vec::new(),
            scope_raw_path_mask: Vec::new(),
            has_negative_filter: false,
            exclude_case_sensitive: false,
            exclude_terms: Vec::new(),
            exclude_exts: Vec::new(),
            exclude_rel_single_paths: Vec::new(),
            exclude_slash_paths: Vec::new(),
            exclude_ext_mask: Vec::new(),
            exclude_name_mask: Vec::new(),
            exclude_parent_mask: Vec::new(),
            exclude_raw_path_mask: Vec::new(),
            exclude_path_leaf_mask: Vec::new(),
            exclude_multi_leaf_mask: Vec::new(),
        }
    }
}

impl CompiledRecordFilter {
    /// 是否存在正向约束条件（类别、扩展名、大小或 `in:` 作用域过滤）。
    #[inline]
    pub fn has_positive_filter(&self) -> bool {
        self.has_type_filter || self.has_custom_ext || self.has_size_filter || self.has_scope_filter
    }

    /// 是否存在 `!` 否定排除条件。
    #[inline]
    pub fn has_negative_filter(&self) -> bool {
        self.has_negative_filter
    }

    /// 判断当前过滤器是否为全放行的直通模式（既无正向约束也无否定排除）。
    #[inline]
    pub fn is_passthrough(&self) -> bool {
        !self.has_positive_filter() && !self.has_negative_filter
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

    /// 针对特定紧凑索引将 `QueryFilter` 编译为位掩码与标量过滤器（默认大小写不敏感排除）。
    pub fn compile(index: &CompactIndex, filter: &QueryFilter) -> Self {
        Self::compile_with_case(index, filter, false)
    }

    /// 针对特定紧凑索引与当前大小写敏感状态编译记录过滤器。
    pub fn compile_with_case(
        index: &CompactIndex,
        filter: &QueryFilter,
        case_sensitive: bool,
    ) -> Self {
        Self::compile_with_case_and_scope(index, filter, case_sensitive, &[])
    }

    /// 针对特定紧凑索引、大小写敏感状态与 `in:<scope>` 作用域候选编译记录过滤器。
    /// 将 `in:` 目录约束预投影为 `parents` / `raw_paths` 位掩码，使正则、模糊、并集等模式均以 O(1) 查表正交复用作用域过滤。
    pub(crate) fn compile_with_case_and_scope(
        index: &CompactIndex,
        filter: &QueryFilter,
        case_sensitive: bool,
        scope_candidates: &[super::path_query::PathCandidate],
    ) -> Self {
        let empty_ext_id = index.extension_id("");

        // 1. 规范化 custom_exts（兼容单字段 custom_ext）
        let mut clean_custom_exts: Vec<String> = filter
            .custom_exts
            .iter()
            .map(|c| normalize_ext_filter(c))
            .filter(|t| !t.is_empty())
            .collect();

        if clean_custom_exts.is_empty() {
            if let Some(target) = filter
                .custom_ext
                .as_deref()
                .map(normalize_ext_filter)
                .filter(|t| !t.is_empty())
            {
                clean_custom_exts.push(target);
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
                    .any(|target| ext_matches_folded(ext, target))
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

        // 3. 编译否定排除条件（术语、扩展名与路径/目录）
        let exclude_terms: Vec<String> = filter
            .exclude_terms
            .iter()
            .filter(|t| !t.is_empty())
            .cloned()
            .collect();
        let exclude_exts: Vec<String> = filter
            .exclude_exts
            .iter()
            .map(|e| normalize_ext_filter(e))
            .filter(|e| !e.is_empty())
            .collect();

        let mut exclude_rel_single_paths = Vec::new();
        let mut exclude_slash_paths = Vec::new();
        for raw in &filter.exclude_paths {
            let norm = normalize_exclude_path_pattern(raw);
            if norm.is_empty() {
                continue;
            }
            if norm.contains('/') {
                if !exclude_slash_paths.contains(&norm) {
                    exclude_slash_paths.push(norm);
                }
            } else if !exclude_rel_single_paths.contains(&norm) {
                exclude_rel_single_paths.push(norm);
            }
        }

        let has_negative_filter = !exclude_terms.is_empty()
            || !exclude_exts.is_empty()
            || !exclude_rel_single_paths.is_empty()
            || !exclude_slash_paths.is_empty();

        let exclude_ext_mask = if exclude_exts.is_empty() {
            Vec::new()
        } else {
            index.evaluate_extensions(|ext| {
                exclude_exts
                    .iter()
                    .any(|target| ext_matches_folded(ext, target))
            })
        };

        let exclude_name_mask = if exclude_terms.is_empty() {
            Vec::new()
        } else {
            index.evaluate_name_mask(|name| {
                exclude_terms
                    .iter()
                    .any(|term| text_contains_excluded_term(name, term, case_sensitive))
            })
        };

        let exclude_path_leaf_mask = if exclude_rel_single_paths.is_empty() {
            Vec::new()
        } else {
            index.evaluate_name_mask(|name| {
                let folded = crate::cjk::fold_cow(name);
                exclude_rel_single_paths
                    .iter()
                    .any(|pat| folded.as_ref() == pat.as_str())
            })
        };

        let exclude_multi_leaf_mask = if exclude_slash_paths.is_empty() {
            Vec::new()
        } else {
            let leaves: Vec<&str> = exclude_slash_paths
                .iter()
                .filter_map(|p| p.rsplit('/').next())
                .filter(|leaf| !leaf.is_empty())
                .collect();
            if leaves.is_empty() {
                Vec::new()
            } else {
                index.evaluate_name_mask(|name| {
                    let folded = crate::cjk::fold_cow(name);
                    leaves.iter().any(|&leaf| folded.as_ref() == leaf)
                })
            }
        };

        let exclude_parent_mask = if exclude_terms.is_empty()
            && exclude_rel_single_paths.is_empty()
            && exclude_slash_paths.is_empty()
        {
            Vec::new()
        } else {
            index.evaluate_parent_mask(|parent| {
                exclude_terms
                    .iter()
                    .any(|term| text_contains_excluded_term(parent, term, case_sensitive))
                    || exclude_rel_single_paths
                        .iter()
                        .any(|pat| path_matches_excluded_subpath(parent, pat))
                    || exclude_slash_paths
                        .iter()
                        .any(|pat| path_matches_excluded_subpath(parent, pat))
            })
        };

        let exclude_raw_path_mask = if exclude_terms.is_empty()
            && exclude_rel_single_paths.is_empty()
            && exclude_slash_paths.is_empty()
        {
            Vec::new()
        } else {
            index.evaluate_raw_path_mask(|raw| {
                exclude_terms
                    .iter()
                    .any(|term| text_contains_excluded_term(raw, term, case_sensitive))
                    || exclude_rel_single_paths
                        .iter()
                        .any(|pat| path_matches_excluded_subpath(raw, pat))
                    || exclude_slash_paths
                        .iter()
                        .any(|pat| path_matches_excluded_subpath(raw, pat))
            })
        };

        // 4. 编译 `in:<scope>` 作用域位掩码（使非 PathQuery 检索模式以 O(1) 查表正交复用作用域过滤）
        let has_scope_filter = !scope_candidates.is_empty();
        let (scope_parent_mask, scope_raw_path_mask) = if has_scope_filter {
            let parent_mask = index.evaluate_parent_mask(|parent| {
                let parent_folded = crate::cjk::fold_cow(parent);
                super::path_search::match_scope_candidates(&parent_folded, scope_candidates)
                    .is_some()
            });
            let raw_mask = index.evaluate_raw_path_mask(|raw| {
                let (parent, _) = crate::compact_index::split_path(raw);
                let parent_folded = crate::cjk::fold_cow(parent);
                super::path_search::match_scope_candidates(&parent_folded, scope_candidates)
                    .is_some()
            });
            (parent_mask, raw_mask)
        } else {
            (Vec::new(), Vec::new())
        };

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
            has_scope_filter,
            scope_candidates: scope_candidates.to_vec(),
            scope_parent_mask,
            scope_raw_path_mask,
            has_negative_filter,
            exclude_case_sensitive: case_sensitive,
            exclude_terms,
            exclude_exts,
            exclude_rel_single_paths,
            exclude_slash_paths,
            exclude_ext_mask,
            exclude_name_mask,
            exclude_parent_mask,
            exclude_raw_path_mask,
            exclude_path_leaf_mask,
            exclude_multi_leaf_mask,
        }
    }

    /// 评估单条 `EntryView` 是否满足过滤条件。
    pub fn matches_view(&self, view: &EntryView<'_>) -> bool {
        if self.is_passthrough() {
            return true;
        }

        // 0. `in:<scope>` 作用域校验
        if self.has_scope_filter {
            let parent = match view.raw_path() {
                Some(raw) => crate::compact_index::split_path(raw).0,
                None => view.parent,
            };
            let parent_folded = crate::cjk::fold_cow(parent);
            if super::path_search::match_scope_candidates(&parent_folded, &self.scope_candidates)
                .is_none()
            {
                return false;
            }
        }

        // 1. 否定排除优先拦截（必须先于正向直通或目录放行执行）
        if self.has_negative_filter {
            if !self.exclude_exts.is_empty()
                && self
                    .exclude_exts
                    .iter()
                    .any(|target| ext_matches_folded(view.extension, target))
            {
                return false;
            }
            if let Some(raw) = view.raw_path() {
                if self.exclude_terms.iter().any(|term| {
                    text_contains_excluded_term(view.name, term, self.exclude_case_sensitive)
                        || text_contains_excluded_term(raw, term, self.exclude_case_sensitive)
                }) || self
                    .exclude_rel_single_paths
                    .iter()
                    .chain(self.exclude_slash_paths.iter())
                    .any(|pat| path_matches_excluded_subpath(raw, pat))
                {
                    return false;
                }
            } else {
                if self.exclude_terms.iter().any(|term| {
                    text_contains_excluded_term(view.name, term, self.exclude_case_sensitive)
                        || text_contains_excluded_term(view.parent, term, self.exclude_case_sensitive)
                }) {
                    return false;
                }
                if !self.exclude_rel_single_paths.is_empty() {
                    let name_folded = crate::cjk::fold_cow(view.name);
                    if self.exclude_rel_single_paths.iter().any(|pat| {
                        name_folded.as_ref() == pat.as_str()
                            || path_matches_excluded_subpath(view.parent, pat)
                    }) {
                        return false;
                    }
                }
                if self.exclude_slash_paths.iter().any(|pat| {
                    path_matches_excluded_subpath(view.parent, pat)
                        || full_path_matches_excluded_subpath(view.parent, view.name, pat)
                }) {
                    return false;
                }
            }
        }

        if !self.has_type_filter && !self.has_custom_ext && !self.has_size_filter {
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
                    .any(|target| ext_matches_folded(dotfile_candidate, target))
                {
                    return true;
                }
            }
            return self
                .custom_exts
                .iter()
                .any(|target| ext_matches_folded(view.extension, target));
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
    /// 对于特殊无后缀工程文件或带 `/` 的嵌套排除路径，调用方具备 `CompactIndex` 引用时应优先使用 `matches_record_in`。
    pub fn matches_record<'a>(
        &self,
        record: &CompactRecord,
        get_name: impl FnOnce() -> Option<&'a str>,
    ) -> bool {
        self.matches_record_core(None, record, get_name)
    }

    /// 结合完整 `CompactIndex` 上下文评估单条底层 `CompactRecord`（支持跨层级否定路径精确判定）。
    pub fn matches_record_in(
        &self,
        index: &CompactIndex,
        record: &CompactRecord,
        name: Option<&str>,
    ) -> bool {
        self.matches_record_core(Some(index), record, || {
            name.or_else(|| Some(index.record_name(record)))
        })
    }

    fn matches_record_core<'a>(
        &self,
        index: Option<&CompactIndex>,
        record: &CompactRecord,
        get_name: impl FnOnce() -> Option<&'a str>,
    ) -> bool {
        if self.is_passthrough() {
            return true;
        }

        // 0a. `in:<scope>` 作用域位图初筛（单次数组寻址）
        if self.has_scope_filter {
            let in_scope = if record.raw_path {
                self.scope_raw_path_mask
                    .get(record.path as usize)
                    .copied()
                    .unwrap_or(false)
            } else {
                self.scope_parent_mask
                    .get(record.path as usize)
                    .copied()
                    .unwrap_or(false)
            };
            if !in_scope {
                return false;
            }
        }

        // 0b. 否定排除优先拦截（必须先于仅大小放行、目录放行与无正向约束放行）
        if self.has_negative_filter {
            if self
                .exclude_ext_mask
                .get(record.extension as usize)
                .copied()
                .unwrap_or(false)
            {
                return false;
            }
            if record.raw_path {
                if self
                    .exclude_raw_path_mask
                    .get(record.path as usize)
                    .copied()
                    .unwrap_or(false)
                    || self
                        .exclude_name_mask
                        .get(record.name as usize)
                        .copied()
                        .unwrap_or(false)
                {
                    return false;
                }
            } else {
                if self
                    .exclude_parent_mask
                    .get(record.path as usize)
                    .copied()
                    .unwrap_or(false)
                    || self
                        .exclude_name_mask
                        .get(record.name as usize)
                        .copied()
                        .unwrap_or(false)
                    || self
                        .exclude_path_leaf_mask
                        .get(record.name as usize)
                        .copied()
                        .unwrap_or(false)
                {
                    return false;
                }
                if self
                    .exclude_multi_leaf_mask
                    .get(record.name as usize)
                    .copied()
                    .unwrap_or(false)
                {
                    if let Some(idx) = index {
                        let parent = idx.record_parent(record);
                        let rec_name = idx.record_name(record);
                        if self
                            .exclude_slash_paths
                            .iter()
                            .any(|pat| full_path_matches_excluded_subpath(parent, rec_name, pat))
                        {
                            return false;
                        }
                    }
                }
            }
        }

        if !self.has_type_filter && !self.has_custom_ext && !self.has_size_filter {
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
                            .any(|target| ext_matches_folded(dotfile_candidate, target))
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
        assert_eq!(f1.size_filter.min_bytes, Some(10_000_001));
        assert_eq!(f1.size_filter.max_bytes, None);

        let (c2, f2) = parse_query_filter("test size:<500k");
        assert_eq!(c2, "test");
        assert_eq!(f2.size_filter.min_bytes, None);
        assert_eq!(f2.size_filter.max_bytes, Some(499_999));

        let (c3, f3) = parse_query_filter("test size:1.5m..100m");
        assert_eq!(c3, "test");
        assert_eq!(f3.size_filter.min_bytes, Some(1_500_000));
        assert_eq!(f3.size_filter.max_bytes, Some(100_000_000));

        let (_, f_ge) = parse_query_filter("test size:>=10m size:<=500k");
        assert_eq!(f_ge.size_filter.min_bytes, Some(10_000_000));
        assert_eq!(f_ge.size_filter.max_bytes, Some(500_000));

        let (_, f_gt0) = parse_query_filter("test size:>0");
        assert_eq!(f_gt0.size_filter.min_bytes, Some(1));
        assert_eq!(f_gt0.size_filter.max_bytes, None);

        let (_, f_lt0) = parse_query_filter("test size:<0");
        assert_eq!(f_lt0.size_filter.min_bytes, Some(1));
        assert_eq!(f_lt0.size_filter.max_bytes, Some(0));
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
        // 多条件收敛交集（严格不等号）
        let (clean1, f1) = parse_query_filter("test size:>10m size:<50m");
        assert_eq!(clean1, "test");
        assert_eq!(f1.size_filter.min_bytes, Some(10_000_001));
        assert_eq!(f1.size_filter.max_bytes, Some(49_999_999));

        // 倒置区间
        let (clean2, f2) = parse_query_filter("test size:>100m size:<10m");
        assert_eq!(clean2, "test");
        assert_eq!(f2.size_filter.min_bytes, Some(100_000_001));
        assert_eq!(f2.size_filter.max_bytes, Some(9_999_999));

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
        assert_eq!(f6.size_filter.min_bytes, Some(1_500_001));
        assert_eq!(f6.size_filter.max_bytes, Some(1_999_999_999));

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

    #[test]
    fn test_tokenize_query_quotes_escapes_and_negations() {
        let tokens = tokenize_query(
            r#""hello world" John's \"lit\" \!bang foo\|bar "" !"" ! !test !.pdf !.gitignore !".pdf" !node_modules/ !src/test !"my dir/""#,
        );
        let summary: Vec<(&str, bool, bool)> = tokens
            .iter()
            .map(|t| (t.text.as_str(), t.quoted, t.negated))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("hello world", true, false),
                ("John's", false, false),
                ("\"lit\"", false, false),
                ("!bang", false, false),
                ("foo|bar", false, false),
                ("test", false, true),
                (".pdf", false, true),
                (".gitignore", false, true),
                (".pdf", true, true),
                ("node_modules/", false, true),
                ("src/test", false, true),
                ("my dir/", true, true),
            ]
        );

        // Unclosed quote falls back to literal '"'
        let unclosed = tokenize_query(r#""hello world"#);
        assert_eq!(unclosed.len(), 2);
        assert_eq!(unclosed[0].text, "\"hello");
        assert!(!unclosed[0].quoted);
        assert_eq!(unclosed[1].text, "world");

        // regex: payload preserves backslash escapes verbatim
        let regex_tokens = tokenize_query(r"regex:^main\.\d+\|\D+ !tmp");
        assert_eq!(regex_tokens[0].text, r"regex:^main\.\d+\|\D+");
        assert!(!regex_tokens[0].quoted);
        assert!(!regex_tokens[0].negated);
        assert_eq!(regex_tokens[1].text, "tmp");
        assert!(regex_tokens[1].negated);

        // Negation classification inside parse_query_filter
        let (clean, qf) = parse_query_filter(
            r#"main !.pdf !.gitignore !".pdf" !node_modules/ !src/test !"my dir/" !draft"#,
        );
        assert_eq!(clean, "main");
        assert_eq!(qf.exclude_exts, vec!["pdf"]);
        assert_eq!(qf.exclude_paths, vec!["node_modules", "src/test", "my dir"]);
        assert_eq!(qf.exclude_terms, vec![".gitignore", ".pdf", "draft"]);
    }

    #[test]
    fn test_compiled_filter_negative_masks_and_passthrough() {
        let index = CompactIndex::from_entries(vec![
            FileEntry {
                name: "keep.rs".to_string(),
                path: "/proj/src/keep.rs".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "rs".to_string(),
            },
            FileEntry {
                name: "skip_test.rs".to_string(),
                path: "/proj/src/skip_test.rs".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "rs".to_string(),
            },
            FileEntry {
                name: "index.js".to_string(),
                path: "/proj/node_modules/pkg/index.js".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "js".to_string(),
            },
            FileEntry {
                name: "report.pdf".to_string(),
                path: "/proj/docs/report.pdf".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "pdf".to_string(),
            },
        ])
        .unwrap();

        let (_clean, neg_only) = parse_query_filter("!test !node_modules/ !.pdf");
        let compiled = CompiledRecordFilter::compile(&index, &neg_only);
        assert!(!compiled.is_passthrough());
        assert!(compiled.has_negative_filter());
        assert!(!compiled.has_positive_filter());

        let records = index.records();
        assert!(compiled.matches_record_in(&index, &records[0], Some("keep.rs")));
        assert!(!compiled.matches_record_in(&index, &records[1], Some("skip_test.rs")));
        assert!(!compiled.matches_record_in(&index, &records[2], Some("index.js")));
        assert!(!compiled.matches_record_in(&index, &records[3], Some("report.pdf")));
    }

    #[test]
    fn test_path_matches_excluded_subpath_multibyte_utf8_no_panic() {
        // Multi-byte UTF-8 partial component miss must step by full char UTF-8 byte length, not +1.
        assert!(!path_matches_excluded_subpath("/a中文/file.txt", "中文"));
        assert!(!path_matches_excluded_subpath("/prefix中文suffix/file.txt", "中文"));
        assert!(!path_matches_excluded_subpath("/a中文", "中文"));
        assert!(!path_matches_excluded_subpath("/ba/中文/file.txt", "a/中文"));
        assert!(!path_matches_excluded_subpath("/a/中文suffix/file.txt", "a/中文"));

        // Partial component miss followed by a real component match later in the path.
        assert!(path_matches_excluded_subpath("/a中文/中文/file.txt", "中文"));
        assert!(path_matches_excluded_subpath("/ba/中文/a/中文/file.txt", "a/中文"));

        // Exact multi-byte UTF-8 path component hits.
        assert!(path_matches_excluded_subpath("/a/中文/file.txt", "中文"));
        assert!(path_matches_excluded_subpath("/a/中文/file.txt", "a/中文"));
        assert!(path_matches_excluded_subpath("/中文/file.txt", "/中文"));
    }

    #[test]
    fn test_excluded_slash_path_self_and_root_child_matching() {
        // Direct unit coverage for trailing-slash parent normalization in CompactIndex.
        assert!(full_path_matches_excluded_subpath("/proj/src/", "test", "src/test"));
        assert!(full_path_matches_excluded_subpath("src/", "test", "src/test"));
        assert!(full_path_matches_excluded_subpath("/proj/src/", "test", "/proj/src/test"));
        assert!(full_path_matches_excluded_subpath("/", "test", "/test"));
        assert!(!full_path_matches_excluded_subpath("/proj/other/", "test", "src/test"));
        assert!(!full_path_matches_excluded_subpath("/proj/subsrc/", "test", "src/test"));
        assert!(!full_path_matches_excluded_subpath("/proj/src/", "test", "/test"));
        assert!(!full_path_matches_excluded_subpath("", "test", "/test"));

        let index = CompactIndex::from_entries(vec![
            FileEntry {
                name: "test".to_string(),
                path: "/proj/src/test".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            FileEntry {
                name: "test".to_string(),
                path: "/proj/src/test".to_string(),
                is_dir: true,
                size: 0,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            FileEntry {
                name: "sub.rs".to_string(),
                path: "/proj/src/test/sub.rs".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "rs".to_string(),
            },
            FileEntry {
                name: "test".to_string(),
                path: "/proj/other/test".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            FileEntry {
                name: "test".to_string(),
                path: "/proj/subsrc/test".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            FileEntry {
                name: "test".to_string(),
                path: "/test".to_string(),
                is_dir: true,
                size: 0,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            FileEntry {
                name: "sub.rs".to_string(),
                path: "/test/sub.rs".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "rs".to_string(),
            },
            // raw_path records (name differs from path suffix)
            FileEntry {
                name: "alias_src".to_string(),
                path: "/proj/src/test".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            FileEntry {
                name: "alias_root".to_string(),
                path: "/test".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
            FileEntry {
                name: "alias_test".to_string(),
                path: "/actual/item".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "".to_string(),
            },
        ])
        .unwrap();

        let records = index.records();

        // 1. Relative multi-segment exclusion !src/test
        let (_c1, f_rel) = parse_query_filter("!src/test");
        let c_rel = CompiledRecordFilter::compile(&index, &f_rel);
        assert!(!c_rel.matches_record_in(&index, &records[0], Some("test")));
        assert!(!c_rel.matches_record_in(&index, &records[1], Some("test")));
        assert!(!c_rel.matches_record_in(&index, &records[2], Some("sub.rs")));
        assert!(c_rel.matches_record_in(&index, &records[3], Some("test")));
        assert!(c_rel.matches_record_in(&index, &records[4], Some("test")));
        assert!(c_rel.matches_record_in(&index, &records[5], Some("test")));
        assert!(c_rel.matches_record_in(&index, &records[6], Some("sub.rs")));
        assert!(!c_rel.matches_record_in(&index, &records[7], Some("alias_src")));
        assert!(c_rel.matches_record_in(&index, &records[8], Some("alias_root")));
        assert!(c_rel.matches_record_in(&index, &records[9], Some("alias_test")));

        for (i, record) in records.iter().enumerate() {
            let view = index.view(crate::compact_index::EntryId(i)).unwrap();
            assert_eq!(
                c_rel.matches_view(&view),
                c_rel.matches_record_in(&index, record, None),
                "matches_view vs matches_record_in mismatch at index {i} for !src/test"
            );
        }

        // 1b. Term exclusion !test also excludes raw_path record whose entry name contains "test"
        let (_c_term, f_term) = parse_query_filter("!test");
        let c_term = CompiledRecordFilter::compile(&index, &f_term);
        assert!(!c_term.matches_record_in(&index, &records[9], Some("alias_test")));
        let view9 = index.view(crate::compact_index::EntryId(9)).unwrap();
        assert!(!c_term.matches_view(&view9));

        // 2. Absolute multi-segment exclusion !/proj/src/test
        let (_c2, f_abs) = parse_query_filter("!/proj/src/test");
        let c_abs = CompiledRecordFilter::compile(&index, &f_abs);
        assert!(!c_abs.matches_record_in(&index, &records[0], Some("test")));
        assert!(!c_abs.matches_record_in(&index, &records[1], Some("test")));
        assert!(!c_abs.matches_record_in(&index, &records[2], Some("sub.rs")));
        assert!(c_abs.matches_record_in(&index, &records[3], Some("test")));
        assert!(c_abs.matches_record_in(&index, &records[5], Some("test")));
        assert!(!c_abs.matches_record_in(&index, &records[7], Some("alias_src")));

        // 3. Root child exclusion !/test
        let (_c3, f_root) = parse_query_filter("!/test");
        let c_root = CompiledRecordFilter::compile(&index, &f_root);
        assert!(c_root.matches_record_in(&index, &records[0], Some("test")));
        assert!(c_root.matches_record_in(&index, &records[1], Some("test")));
        assert!(c_root.matches_record_in(&index, &records[2], Some("sub.rs")));
        assert!(c_root.matches_record_in(&index, &records[3], Some("test")));
        assert!(!c_root.matches_record_in(&index, &records[5], Some("test")));
        assert!(!c_root.matches_record_in(&index, &records[6], Some("sub.rs")));
        assert!(c_root.matches_record_in(&index, &records[7], Some("alias_src")));
        assert!(!c_root.matches_record_in(&index, &records[8], Some("alias_root")));
    }

    #[test]
    fn test_tokenize_query_pasted_paths_and_regex_protection() {
        let quoted_abs = tokenize_query(r#""/Users/alice/My Docs/spec.md""#);
        assert_eq!(quoted_abs.len(), 1);
        assert!(!quoted_abs[0].quoted);
        assert_eq!(quoted_abs[0].text, "/Users/alice/My Docs/spec.md");

        let single_quoted_uri = tokenize_query("'file:///Users/alice/My%20Docs/spec.md'");
        assert_eq!(single_quoted_uri.len(), 1);
        assert!(!single_quoted_uri[0].quoted);
        assert_eq!(single_quoted_uri[0].text, "/Users/alice/My Docs/spec.md");

        let shell_escaped = tokenize_query(r"/Users/alice/My\ Docs/a\ \(1\)\&b\'c.txt");
        assert_eq!(shell_escaped.len(), 1);
        assert_eq!(shell_escaped[0].text, "/Users/alice/My Docs/a (1)&b'c.txt");

        // 非路径引号词元与引号正则保持原样
        let literal_slash = tokenize_query(r#""foo/bar""#);
        assert_eq!(literal_slash.len(), 1);
        assert!(literal_slash[0].quoted);
        assert_eq!(literal_slash[0].text, "foo/bar");

        let quoted_regex = tokenize_query(r#""/a\s+b/""#);
        assert_eq!(quoted_regex.len(), 1);
        assert!(quoted_regex[0].quoted);
        assert_eq!(quoted_regex[0].text, r"/a\s+b/");

        let inline_regex = tokenize_query(r"/^foo\s+bar/");
        assert_eq!(inline_regex.len(), 1);
        assert_eq!(inline_regex[0].text, r"/^foo\s+bar/");

        // Windows 路径不再做特殊处理，按普通字面量保留
        let win = tokenize_query(r"C:\(1)\foo.txt");
        assert_eq!(win.len(), 1);
        assert!(!win[0].was_sanitized_pasted_path);

        assert!(quoted_abs[0].was_sanitized_pasted_path);
        assert!(single_quoted_uri[0].was_sanitized_pasted_path);
        assert!(shell_escaped[0].was_sanitized_pasted_path);
        assert!(!literal_slash[0].was_sanitized_pasted_path);
        assert!(!quoted_regex[0].was_sanitized_pasted_path);
        assert!(!inline_regex[0].was_sanitized_pasted_path);
    }

    #[test]
    fn test_sanitize_path_fragment_and_input_variants() {
        assert_eq!(
            sanitize_path_fragment(r#"  "file:///Users/a/My%20Doc/spec.md"  "#),
            "/Users/a/My Doc/spec.md"
        );
        assert_eq!(
            sanitize_path_fragment("FILE://LOCALHOST/Users/a/My%20Doc/spec.md"),
            "/Users/a/My Doc/spec.md"
        );
        assert_eq!(
            sanitize_path_fragment("file:///Users/a/%ZZ/trunc%/%FF/spec.md"),
            "/Users/a/%ZZ/trunc%/%FF/spec.md"
        );
        assert_eq!(sanitize_path_fragment(r#""100% done""#), r#""100% done""#);
        assert_eq!(sanitize_path_fragment(r#""foo/bar""#), r#""foo/bar""#);

        assert_eq!(
            sanitize_path_input(r#""/Users/alice/My Docs/spec.md""#),
            "/Users/alice/My Docs/spec.md"
        );
        assert_eq!(sanitize_path_input(r#""100% done""#), r#""100% done""#);
        assert_eq!(sanitize_path_input("regex:^main_\\d+"), "regex:^main_\\d+");
    }

    #[test]
    fn test_strict_size_inequalities_and_long_inferred_extensions() {
        let store = crate::indexer::IndexStore::new();
        let make = |name: &str, path: &str, size: u64, ext: &str| FileEntry {
            name: name.to_string(),
            path: path.to_string(),
            is_dir: false,
            size,
            modified: 0,
            created_at: 0,
            extension: ext.to_string(),
        };
        store.replace(vec![
            make("empty.txt", "/empty.txt", 0, "txt"),
            make("one.txt", "/one.txt", 1, "txt"),
            make("exact100.txt", "/exact100.txt", 100, "txt"),
            make("notes.markdown", "/notes.markdown", 50, "markdown"),
            make("notes.md", "/notes.md", 50, "md"),
            make("budget.numbers", "/budget.numbers", 50, "numbers"),
            make("Demo.xcodeproj", "/Demo.xcodeproj", 50, "xcodeproj"),
            make(".hidden_notes.md", "/.hidden_notes.md", 50, "md"),
        ]);

        // 严格大于 0：排除 0 字节空文件
        let (gt0, _) = crate::search::dispatch(&store, ".txt size:>0", crate::search::SearchMode::Auto, 10);
        let gt0_names: Vec<_> = gt0.iter().map(|r| r.entry.name.as_str()).collect();
        assert!(!gt0_names.contains(&"empty.txt"));
        assert!(gt0_names.contains(&"one.txt"));
        assert!(gt0_names.contains(&"exact100.txt"));

        // 严格小于 100：排除恰好 100 字节文件；<=100 则包含
        let (lt100, _) = crate::search::dispatch(&store, ".txt size:<100", crate::search::SearchMode::Auto, 10);
        let lt100_names: Vec<_> = lt100.iter().map(|r| r.entry.name.as_str()).collect();
        assert!(lt100_names.contains(&"empty.txt"));
        assert!(lt100_names.contains(&"one.txt"));
        assert!(!lt100_names.contains(&"exact100.txt"));

        let (le100, _) = crate::search::dispatch(&store, ".txt size:<=100", crate::search::SearchMode::Auto, 10);
        assert!(le100.iter().any(|r| r.entry.name == "exact100.txt"));

        // size:<0 返回空结果
        let (lt0, _) = crate::search::dispatch(&store, ".txt size:<0", crate::search::SearchMode::Auto, 10);
        assert!(lt0.is_empty());

        // 多词长扩展名推断（.markdown / .numbers / .xcodeproj）
        let (md_res, _) = crate::search::dispatch(&store, "notes .markdown", crate::search::SearchMode::Auto, 10);
        assert_eq!(md_res.iter().map(|r| r.entry.name.as_str()).collect::<Vec<_>>(), vec!["notes.markdown"]);

        let (num_res, _) = crate::search::dispatch(&store, "budget .numbers", crate::search::SearchMode::Auto, 10);
        assert_eq!(num_res.iter().map(|r| r.entry.name.as_str()).collect::<Vec<_>>(), vec!["budget.numbers"]);

        let (xc_res, _) = crate::search::dispatch(&store, "demo .xcodeproj", crate::search::SearchMode::Auto, 10);
        assert_eq!(xc_res.iter().map(|r| r.entry.name.as_str()).collect::<Vec<_>>(), vec!["Demo.xcodeproj"]);

        // 否定长扩展名 !.markdown 精准排除扩展名；而索引中不存在扩展名 .hidden 时自动回退排除点文件名
        let (no_md, _) = crate::search::dispatch(&store, "notes !.markdown", crate::search::SearchMode::Auto, 10);
        assert!(!no_md.iter().any(|r| r.entry.name == "notes.markdown"));
        assert!(no_md.iter().any(|r| r.entry.name == "notes.md"));

        let (no_hidden, _) = crate::search::dispatch(&store, "notes !.hidden", crate::search::SearchMode::Auto, 10);
        assert!(!no_hidden.iter().any(|r| r.entry.name == ".hidden_notes.md"));
        assert!(no_hidden.iter().any(|r| r.entry.name == "notes.md"));
    }
}
