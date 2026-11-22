pub mod disjunction;
pub mod filter;
pub mod fuzzy;
pub mod path_query;
pub mod path_search;
pub mod ranking;
pub mod regex_search;
pub mod regex_sugar;
pub mod substring;
pub mod top_k;
pub mod wildcard;

pub use disjunction::{
    has_unescaped_pipe, merge_highlight_spans, split_query_pipes, unescape_literal_pipes,
    DisjunctionQuery, OrClause,
};
pub use filter::{
    is_dotfile_query, parse_query_filter, parse_query_structured, CompiledRecordFilter, FileType,
    ParsedQuery, QueryFilter, QueryToken, SizeFilter,
};
pub use regex_sugar::{is_regex_metachar, parse_regex_sugar, RegexSugar};
pub use substring::search_disjunction;

use crate::compact_index::CompactIndex;
use crate::indexer::{FileEntry, IndexStore};
use crate::string_pool::NameSignature;
use serde::{Deserialize, Serialize};

use top_k::{Candidate, TopK};

/// 计算各关键词在 `name` 或拼音转写中的字节区间，供前端高亮。
/// 非 ASCII 文件名禁止直接复用原始字节切片索引，避免在 NFD 组合字符中间撕裂或遗漏，
/// 统一经 Unicode NFC/繁简归一化串定位后通过 `map_norm_span_to_orig` 按组合字符簇反投回原始字节边界。
pub(crate) fn find_positions<'a>(
    name: &str,
    name_lower: &str,
    keywords: &[(&'a str, &'a str)],
) -> Vec<(usize, usize)> {
    let mut positions = Vec::new();
    let mut unmatched_keywords = Vec::new();
    let name_is_ascii = name.is_ascii();
    let name_norm = (!name_is_ascii).then(|| crate::cjk::normalize_and_simplify_cow(name));
    let name_folded_buf = (!name_is_ascii && crate::cjk::needs_fold(name_lower))
        .then(|| crate::cjk::fold_str(name_lower));
    let name_folded = name_folded_buf.as_deref().unwrap_or(name_lower);

    for &(keyword, keyword_lower) in keywords {
        if keyword.is_empty() {
            continue;
        }
        if name_is_ascii && keyword.is_ascii() {
            if let Some(start) = name.find(keyword) {
                positions.push((start, start + keyword.len()));
            } else if let Some(start) = name_folded.find(keyword_lower) {
                positions.push((start, start + keyword_lower.len()));
            } else {
                unmatched_keywords.push(keyword);
            }
            continue;
        }

        let kw_norm = crate::cjk::normalize_and_simplify_cow(keyword);
        if let Some(ref norm) = name_norm {
            if let Some(start) = norm.find(kw_norm.as_ref()) {
                if let Some(pos) = map_norm_span_to_orig(name, norm, start, start + kw_norm.len()) {
                    positions.push(pos);
                    continue;
                }
            }
        } else if let Some(start) = name.find(kw_norm.as_ref()) {
            positions.push((start, start + kw_norm.len()));
            continue;
        }

        let kw_folded = crate::cjk::fold_cow(keyword_lower);
        if let Some(start) = name_folded.find(kw_folded.as_ref()) {
            if let Some(pos) =
                map_norm_span_to_orig(name, name_folded, start, start + kw_folded.len())
            {
                positions.push(pos);
                continue;
            }
        }

        unmatched_keywords.push(keyword);
    }

    if !unmatched_keywords.is_empty() && name.as_bytes().iter().any(|&b| b >= 0x80) {
        let pinyin_pos = crate::pinyin::find_pinyin_positions(name, &unmatched_keywords);
        positions.extend(pinyin_pos);
    }

    positions.sort_by_key(|&(s, e)| (s, e));
    positions.dedup();
    positions
}

/// 将归一化或大小写折叠后字符串中的字节区间，按 UAX #29 扩展组合字符簇（grapheme cluster）
/// 反投影为原字符串的安全 UTF-8 字节区间。
///
/// 仅当原串与目标串均为纯 ASCII 且长度相同时直接复用字节坐标；
/// 任何非 ASCII 文本（如 `İ` -> `i\u{0307}`、NFD `e\u{0301}` -> `é`、Hangul Jamo -> 音节块）
/// 都会映射到完整覆盖重叠字符簇的原串字节范围，绝不在组合附加符号中间截断。
pub(crate) fn map_norm_span_to_orig(
    orig: &str,
    norm: &str,
    norm_start: usize,
    norm_end: usize,
) -> Option<(usize, usize)> {
    if norm_start >= norm_end || norm_end > norm.len() {
        return None;
    }
    if orig.is_ascii() && norm.is_ascii() && orig.len() == norm.len() {
        return Some((norm_start, norm_end));
    }

    use unicode_segmentation::UnicodeSegmentation;
    let orig_graphemes: Vec<(usize, &str)> = orig.grapheme_indices(true).collect();
    let norm_graphemes: Vec<(usize, &str)> = norm.grapheme_indices(true).collect();

    if orig_graphemes.len() == norm_graphemes.len() {
        let mut mapped_start = None;
        let mut mapped_end = 0usize;
        for ((og_start, og), (ng_start, ng)) in orig_graphemes.iter().zip(norm_graphemes.iter()) {
            let ng_end = ng_start + ng.len();
            if ng_end > norm_start && *ng_start < norm_end {
                if mapped_start.is_none() {
                    mapped_start = Some(*og_start);
                }
                mapped_end = og_start + og.len();
            }
        }
        return mapped_start.map(|s| (s, mapped_end));
    }

    // 当极少数 Unicode 大写字符折叠后裂变为多个基本字母簇（如 'ẞ' -> "ss"）时，按每簇折叠长度步进投影
    map_norm_span_with_graphemes(&orig_graphemes, norm, norm_start, norm_end)
}

/// 复用已切分的 `orig_graphemes` 将归一化/折叠串字节区间按字符簇步进投影回原串，
/// 避免批量高亮映射（如 `fuzzy` 逐索引投影）在循环内重复分配字素簇数组。
pub(crate) fn map_norm_span_with_graphemes(
    orig_graphemes: &[(usize, &str)],
    norm: &str,
    norm_start: usize,
    norm_end: usize,
) -> Option<(usize, usize)> {
    if norm_start >= norm_end || norm_end > norm.len() {
        return None;
    }
    let mut mapped_start = None;
    let mut mapped_end = 0usize;
    let mut norm_cursor = 0usize;
    for &(og_start, og) in orig_graphemes {
        let adv = if norm[norm_cursor..].starts_with(og) {
            og.len()
        } else {
            let folded = crate::cjk::fold_cow(og);
            if norm[norm_cursor..].starts_with(folded.as_ref()) {
                folded.len()
            } else {
                crate::cjk::normalize_nfc_cow(og).len()
            }
        };
        let seg_end = (norm_cursor + adv).min(norm.len());
        if seg_end > norm_start && norm_cursor < norm_end {
            if mapped_start.is_none() {
                mapped_start = Some(og_start);
            }
            mapped_end = og_start + og.len();
        }
        norm_cursor = seg_end;
        if norm_cursor >= norm_end && mapped_start.is_some() {
            break;
        }
    }
    mapped_start.map(|s| (s, mapped_end))
}

/// 在按名字字节序排好序的唯一名表上使用 128 位签名超集预筛后，先计算文件名级 `BaseNameScore`；
/// 再依据上界剪枝跳过必然落败的桶，最后按记录展开并计算上下文分数写入 Top-K 堆。
pub(crate) fn rank_unique_names<'a>(
    index: &'a CompactIndex,
    filter: &CompiledRecordFilter,
    max_results: usize,
    required_sig: NameSignature,
    targets_hidden: bool,
    rank_ctx: &ranking::RecordRankContext,
    mut score: impl FnMut(&'a str, Option<&'a str>) -> Option<ranking::BaseNameScore>,
) -> Vec<Candidate<'a, ranking::RankScore>> {
    let mut top = TopK::new(max_results);
    if max_results == 0 {
        return Vec::new();
    }
    let query_index = index.query_index();
    let passthrough = filter.is_passthrough();
    let allow_dirs = filter.allow_dirs();
    let records = index.records();
    for name_id in 0..query_index.slot_count() as u32 {
        let sig = query_index.signature_at(name_id);
        if sig == crate::string_pool::NameSignature::default() && !query_index.is_ready(name_id) {
            continue;
        }
        if !sig.contains(required_sig) {
            continue;
        }
        let entries = query_index.entries(name_id);
        if entries.is_empty() {
            continue;
        }
        let Some(name) = index.name_text(name_id) else {
            continue;
        };
        // 折叠串只为非 ASCII 名字保存，ASCII 名字直接跳过这次哈希查找
        let folded = if name.is_ascii() {
            None
        } else {
            query_index.folded(name_id)
        };
        let Some(base) = score(name, folded) else {
            continue;
        };
        let upper = base.upper_bound(rank_ctx, allow_dirs, targets_hidden);
        if let Some(worst) = top.worst() {
            if upper < worst.score || (upper == worst.score && name > worst.name()) {
                continue;
            }
        }
        let (head, tail) = ranking::bucket_slices(entries);
        for &ordinal in head.iter().chain(tail.iter()) {
            let Some(record) = records.get(ordinal as usize) else {
                continue;
            };
            if !passthrough && !filter.matches_record_in(index, record, Some(name)) {
                continue;
            }
            let (parent_depth, parent_hidden) = index.record_parent_meta(record, query_index);
            let final_score = ranking::score_record_with_meta(
                base,
                record.is_dir,
                parent_depth,
                parent_hidden,
                rank_ctx,
                targets_hidden,
                0,
            );
            top.offer(ordinal as usize, final_score, name);
        }
    }
    top.finish()
}

/// Search mode per D-01, D-02, D-04.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Auto,
    Substring,
    Wildcard,
    Regex,
    Fuzzy,
}

/// 大小写匹配策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CaseSensitivity {
    /// 自动启发式判定：仅当目标词元包含有效大写字符时启用敏感匹配
    #[default]
    Auto,
    /// 强制区分大小写
    Sensitive,
    /// 强制忽略大小写
    Insensitive,
}

/// 在剥离否定词与未加引号的 `in:<scope>` 词元后，提取行内正则语法糖 `/pattern/[i]`。
/// 使 `in:src /^test_.*\.rs/`、`in:"/My Docs" /(foo|bar)/i` 等正交组合都能被稳定识别为正则模式。
fn extract_regex_sugar_from_tokens(tokens: &[QueryToken]) -> Option<RegexSugar> {
    let mut non_scope = Vec::new();
    for token in tokens {
        if token.negated || token.text.is_empty() {
            continue;
        }
        if !token.quoted && token.text.starts_with("in:") {
            continue;
        }
        if token.quoted || token.had_escaped_meta || token.was_sanitized_pasted_path {
            return None;
        }
        non_scope.push(token.text.as_str());
    }
    if non_scope.is_empty() {
        return None;
    }
    if non_scope.len() == 1 {
        parse_regex_sugar(non_scope[0])
    } else {
        parse_regex_sugar(&non_scope.join(" "))
    }
}

/// 基于结构化正向词元提取参与 Smart Case 判定的有效文件名词元。
///
/// 跳过否定排除词（`!Test` / `!"Test"`）、`in:` 作用域与父路径前缀，
/// 对被成对双引号保护的正向词元（如 `"Test"` / `"type:PDF"`）保留原始内容参与大写判定。
pub(crate) fn extract_content_terms_from_tokens(
    tokens: &[QueryToken],
    search_mode: SearchMode,
) -> Vec<String> {
    if search_mode == SearchMode::Regex {
        let mut raw_terms = Vec::new();
        for token in tokens {
            if token.negated || token.text.is_empty() {
                continue;
            }
            if !token.quoted && token.text.starts_with("in:") {
                continue;
            }
            let raw = if raw_terms.is_empty() {
                token.text.strip_prefix("regex:").unwrap_or(&token.text)
            } else {
                token.text.as_str()
            };
            if !raw.is_empty() {
                raw_terms.push(raw.to_string());
            }
        }
        if raw_terms.len() == 1 {
            if let Some(sugar) = parse_regex_sugar(&raw_terms[0]) {
                raw_terms[0] = sugar.pattern;
            }
        }
        return raw_terms;
    }

    let mut terms = Vec::new();
    for token in tokens {
        if token.negated {
            continue;
        }
        let word = token.text.as_str();
        if word.is_empty() {
            continue;
        }
        if token.quoted {
            terms.push(word.to_string());
            continue;
        }
        if word.starts_with("in:") {
            continue;
        }
        if word.contains('/') {
            if word.ends_with('/') {
                continue;
            }
            if let Some((_, name_part)) = word.rsplit_once('/') {
                if !name_part.is_empty() {
                    terms.push(name_part.to_string());
                }
            }
            continue;
        }
        if word.starts_with('.') && !is_dotfile_query(word) {
            continue;
        }
        terms.push(word.to_string());
    }
    terms
}

/// 提取参与 Smart Case 判定的有效文件名词元。
///
/// 先解析引号与 `!exclude` 排除语法，过滤否定词、`type:`/`ext:`/`size:`/`in:` 修饰词与路径前缀，
/// 避免排除词或路径大写误触发文件名大小写敏感匹配。
pub fn extract_content_terms_for_smart_case(
    query: &str,
    search_mode: SearchMode,
) -> Vec<String> {
    let parsed = parse_query_structured(query);
    extract_content_terms_from_tokens(&parsed.positive_tokens, search_mode)
}

/// 检测词元是否包含有效大写字符。正则模式下跳过 `\D`、`\S` 等反斜杠转义元字符。
pub fn has_effective_uppercase(term: &str, is_regex: bool) -> bool {
    if !is_regex {
        return term.chars().any(|c| c.is_uppercase());
    }
    let mut chars = term.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            chars.next();
            continue;
        }
        if c.is_uppercase() {
            return true;
        }
    }
    false
}

/// 基于结构化正向词元解析指定配置下的最终大小写敏感判定结果。
pub(crate) fn resolve_case_sensitive_tokens(
    tokens: &[QueryToken],
    setting: CaseSensitivity,
    search_mode: SearchMode,
) -> bool {
    match setting {
        CaseSensitivity::Sensitive => true,
        CaseSensitivity::Insensitive => false,
        CaseSensitivity::Auto => {
            let terms = extract_content_terms_from_tokens(tokens, search_mode);
            let is_regex = search_mode == SearchMode::Regex;
            terms
                .into_iter()
                .any(|term| has_effective_uppercase(&term, is_regex))
        }
    }
}

/// 解析查询语句在指定配置下的最终大小写敏感判定结果。
pub fn resolve_case_sensitive(
    clean_query: &str,
    setting: CaseSensitivity,
    search_mode: SearchMode,
) -> bool {
    match setting {
        CaseSensitivity::Sensitive => true,
        CaseSensitivity::Insensitive => false,
        CaseSensitivity::Auto => {
            let terms = extract_content_terms_for_smart_case(clean_query, search_mode);
            let is_regex = search_mode == SearchMode::Regex;
            terms
                .into_iter()
                .any(|term| has_effective_uppercase(&term, is_regex))
        }
    }
}

/// Result from advanced search. Per D-06.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdvancedSearchResult {
    pub entry: FileEntry,
    pub score: f64,
    pub match_positions: Vec<(usize, usize)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_match_positions: Vec<(usize, usize)>,
    pub search_mode: SearchMode,
}

/// 基于结构化正向词元判断自动检索模式；被引号保护的词元不触发正则、通配符或管道语法。
pub(crate) fn detect_mode_from_tokens(tokens: &[QueryToken]) -> SearchMode {
    if tokens.is_empty() {
        return SearchMode::Auto;
    }

    if extract_regex_sugar_from_tokens(tokens).is_some() {
        return SearchMode::Regex;
    }

    if tokens.iter().any(|t| {
        !t.negated
            && !t.quoted
            && !t.was_sanitized_pasted_path
            && (t.text.starts_with("regex:") || parse_regex_sugar(&t.text).is_some())
    }) {
        return SearchMode::Regex;
    }

    if tokens
        .iter()
        .any(|t| !t.negated && !t.quoted && !t.had_escaped_meta && t.text.contains('|'))
    {
        return SearchMode::Auto;
    }

    if tokens
        .iter()
        .any(|t| !t.negated && !t.quoted && (t.text.contains('*') || t.text.contains('?')))
    {
        return SearchMode::Wildcard;
    }

    SearchMode::Auto
}

/// 参与多源检索的分片描述符。
#[derive(Clone, Copy)]
pub struct SearchShard<'a> {
    pub index: &'a CompactIndex,
    pub configured_path: Option<&'a str>,
    pub active_path: Option<&'a str>,
    pub volume_name: Option<&'a str>,
    pub is_offline: bool,
}

/// 携带来源分片元信息的高级检索结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MultiSearchResult {
    pub result: AdvancedSearchResult,
    pub volume_name: Option<String>,
    pub is_offline: bool,
}

/// Detect search mode from query string.
/// - Starts with `regex:` or matches regex sugar `/<pattern>/` (unquoted) -> Regex
/// - Contains unescaped `|` -> Auto (or disjunction)
/// - Contains unquoted `*` or `?` -> Wildcard
/// - Otherwise -> Auto
pub fn detect_mode(query: &str) -> SearchMode {
    let parsed = parse_query_structured(query);
    detect_mode_from_tokens(&parsed.positive_tokens)
}

/// 校验证伪隐式扩展名（含正/负向长扩展名候选）在当前索引中是否存在；
/// 若不存在则移出扩展名过滤并归还至正向检索词元或否定排除词元。
fn resolve_inferred_exts(parsed: &mut ParsedQuery, has_extension: impl Fn(&str) -> bool) {
    if !parsed.filter.inferred_exts.is_empty() {
        let inferred = std::mem::take(&mut parsed.filter.inferred_exts);
        let mut restored_tokens = false;
        for (inferred_ext, raw_token) in inferred {
            if !has_extension(&inferred_ext) {
                parsed.filter.custom_exts.retain(|e| e != &inferred_ext);
                if parsed.filter.custom_ext.as_deref() == Some(inferred_ext.as_str()) {
                    parsed.filter.custom_ext = parsed.filter.custom_exts.first().cloned();
                }
                if parsed.clean_query.is_empty() {
                    parsed.clean_query = raw_token.clone();
                } else {
                    parsed.clean_query.push(' ');
                    parsed.clean_query.push_str(&raw_token);
                }
                parsed.positive_tokens.push(QueryToken {
                    text: raw_token.clone(),
                    quoted: false,
                    negated: false,
                    raw: raw_token,
                    had_escaped_meta: false,
                    was_sanitized_pasted_path: false,
                });
                restored_tokens = true;
            } else {
                parsed.filter.inferred_exts.push((inferred_ext, raw_token));
            }
        }
        if restored_tokens {
            parsed.clean_query = parsed
                .positive_tokens
                .iter()
                .map(|t| t.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
        }
    }

    if !parsed.filter.inferred_exclude_exts.is_empty() {
        let inferred_neg = std::mem::take(&mut parsed.filter.inferred_exclude_exts);
        for (inferred_ext, raw_token) in inferred_neg {
            if !has_extension(&inferred_ext) {
                parsed.filter.exclude_exts.retain(|e| e != &inferred_ext);
                if !parsed.filter.exclude_terms.contains(&raw_token) {
                    parsed.filter.exclude_terms.push(raw_token);
                }
            } else {
                parsed
                    .filter
                    .inferred_exclude_exts
                    .push((inferred_ext, raw_token));
            }
        }
    }
}

/// 当仅含单个 `/` 的未加引号普通路径词元未能命中路径索引时，按 macOS Finder/POSIX 冒号转义规则回退为 `:` 检索（如 `AC/DC` -> `AC:DC`）。
#[allow(clippy::too_many_arguments)]
fn try_posix_colon_fallback(
    read: &crate::compact_index::CompactIndex,
    tokens: &[QueryToken],
    path_query: &path_query::PathQuery,
    effective_filter: &QueryFilter,
    compiled_filter: &CompiledRecordFilter,
    case_sensitivity: CaseSensitivity,
    base_case_sensitive: bool,
    explicit_lock: bool,
    effective_mode: SearchMode,
    max_results: usize,
) -> Option<(Vec<AdvancedSearchResult>, bool)> {
    if effective_mode == SearchMode::Regex
        || path_query.scope_prefix.is_some()
        || path_query.is_dir_only
        || path_query.is_absolute_prefix
    {
        return None;
    }

    let mut slash_idx = None;
    for (idx, tok) in tokens.iter().enumerate() {
        if !tok.quoted && tok.text.contains('/') {
            if slash_idx.is_some() {
                return None;
            }
            slash_idx = Some(idx);
        }
    }
    let idx = slash_idx?;
    let target = &tokens[idx];
    if target.quoted
        || target.was_sanitized_pasted_path
        || target.text.starts_with('/')
        || target.text.starts_with("~/")
        || target.text.ends_with('/')
        || target.text.matches('/').count() != 1
    {
        return None;
    }

    let mut colon_tokens = tokens.to_vec();
    colon_tokens[idx].text = target.text.replacen('/', ":", 1);
    colon_tokens[idx].raw = colon_tokens[idx].text.clone();

    // `/` 替换为 `:` 后，原先处于目录段的大写字母转为文件名词元，需重算 Smart Case 判定。
    let fallback_cs = resolve_case_sensitive_tokens(&colon_tokens, case_sensitivity, effective_mode);
    let recompiled;
    let filter_ref = if fallback_cs == base_case_sensitive {
        compiled_filter
    } else {
        recompiled = CompiledRecordFilter::compile_with_case(read, effective_filter, fallback_cs);
        &recompiled
    };

    let results = substring::search_index_tokens(
        read,
        &colon_tokens,
        filter_ref,
        max_results,
        fallback_cs,
        explicit_lock,
    );
    (!results.is_empty()).then_some((results, fallback_cs))
}

/// 未索引绝对路径目录的单层实时兜底列举（`std::fs::read_dir`），仅用于补齐系统目录或外挂卷等尚未建库路径。
fn fallback_read_dir(
    path_query: &path_query::PathQuery,
    compiled_filter: &CompiledRecordFilter,
    max_results: usize,
    is_case_sensitive: bool,
    explicit_lock: bool,
    effective_mode: SearchMode,
) -> Vec<AdvancedSearchResult> {
    if max_results == 0
        || effective_mode == SearchMode::Regex
        || effective_mode == SearchMode::Fuzzy
        || !path_query.is_absolute_prefix
        || !path_query.is_dir_only
    {
        return Vec::new();
    }
    let Some(raw_dir) = path_query.raw_dir_pattern.as_deref() else {
        return Vec::new();
    };

    let (dir_disk, parent) = if raw_dir == "/" {
        ("/".to_string(), "/".to_string())
    } else {
        let trimmed = raw_dir.trim_end_matches('/');
        (trimmed.to_string(), format!("{trimmed}/"))
    };

    if !path_query.scope_candidates.is_empty() {
        let parent_folded = crate::cjk::fold_cow(&parent);
        if path_search::match_scope_candidates(&parent_folded, &path_query.scope_candidates)
            .is_none()
        {
            return Vec::new();
        }
    }

    let Ok(read_dir) = std::fs::read_dir(&dir_disk) else {
        return Vec::new();
    };

    let limit = max_results.min(500);
    let targets_hidden = path_search::path_query_targets_hidden(path_query);
    let rank_ctx = ranking::RecordRankContext::default();
    let has_wildcard = path_query
        .name_terms
        .iter()
        .any(|t| t.contains('*') || t.contains('?'));
    let all_terms_ascii = path_query.name_terms.iter().all(|t| t.is_ascii());
    let mut ascii_buf = Vec::new();

    let kw_prepared: Vec<(&str, String)> = path_query
        .name_terms
        .iter()
        .map(|t| (t.as_str(), crate::cjk::fold_str(t)))
        .collect();
    let kw_refs: Vec<(&str, &str)> = kw_prepared
        .iter()
        .map(|(raw, lower)| (*raw, lower.as_str()))
        .collect();
    let parent_highlight_end = parent.trim_end_matches('/').len();
    let parent_match_positions = if parent_highlight_end > 0 {
        vec![(0, parent_highlight_end)]
    } else {
        Vec::new()
    };

    let mut matched: Vec<(ranking::RankScore, AdvancedSearchResult)> = Vec::new();
    for dir_entry in read_dir.flatten() {
        let raw_name = dir_entry.file_name().to_string_lossy().into_owned();
        if crate::filter::is_system_noise_name(&raw_name) {
            continue;
        }
        let Ok(metadata) = dir_entry
            .metadata()
            .or_else(|_| std::fs::symlink_metadata(dir_entry.path()))
        else {
            continue;
        };

        let is_dir = metadata.is_dir();
        let size = if is_dir { 0 } else { metadata.len() };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let created_at = metadata
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let extension = if is_dir {
            String::new()
        } else {
            std::path::Path::new(&raw_name)
                .extension()
                .map(|ext| ext.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        };

        let view = crate::compact_index::EntryView::from_parts(
            &raw_name,
            &parent,
            &extension,
            is_dir,
            size,
            modified,
            created_at,
        );
        if !compiled_filter.matches_view(&view) {
            continue;
        }
        if let Some(ref expected_ext) = path_query.extension {
            if !filter::ext_matches_folded(&extension, expected_ext) {
                continue;
            }
        }

        let base_score = if path_query.name_terms.is_empty() {
            ranking::BaseNameScore::for_tier_only(&raw_name, ranking::MatchTier::Prefix, 0)
        } else {
            // 磁盘实时读取的条目可能含繁体中文、非 ASCII 大写或 NFD 组合字符，
            // 在不区分大小写时须与已折叠的查询词保持一致预处理，避免 score_name 误将其视为已折叠。
            let folded_buf = (!is_case_sensitive
                && !raw_name.is_ascii()
                && crate::cjk::needs_fold(&raw_name))
                .then(|| crate::cjk::fold_str(&raw_name));
            let Some(score) = path_search::score_name(
                &raw_name,
                folded_buf.as_deref(),
                path_query,
                has_wildcard,
                all_terms_ascii,
                &mut ascii_buf,
                is_case_sensitive,
                explicit_lock,
            ) else {
                continue;
            };
            score
        };

        let rank_score = ranking::score_record_with_extra(
            base_score,
            is_dir,
            &parent,
            &rank_ctx,
            targets_hidden,
            ranking::PATH_BONUS_DIRECT,
        );
        let match_positions = if path_query.name_terms.is_empty() {
            Vec::new()
        } else {
            let name_folded = crate::cjk::fold_cow(&raw_name);
            find_positions(&raw_name, name_folded.as_ref(), &kw_refs)
        };

        matched.push((
            rank_score,
            AdvancedSearchResult {
                entry: view.materialize(),
                score: rank_score.to_f64(),
                match_positions,
                parent_match_positions: parent_match_positions.clone(),
                search_mode: SearchMode::Substring,
            },
        ));
    }

    matched.sort_by(|(sa, ra), (sb, rb)| sb.cmp(sa).then_with(|| ra.entry.name.cmp(&rb.entry.name)));
    matched.truncate(limit);
    matched.into_iter().map(|(_, res)| res).collect()
}

/// 携带过滤条件的通用检索分发入口。
///
/// 优先级与作用域机制：
/// 1. 在自动模式下先判断顶层是否正则，否则按引号与转义感知的状态机在最高层以未转义 `|` 切分并集子句，使每个子句各自解析 `type:` / `ext:` / `!exclude` 并继承外层默认条件；
/// 2. 单子句路径下先提取结构化词元与过滤规则，随后核验隐式 `.ext` 并决定生效模式；
/// 3. 依据子句或全句的 Smart Case 状态编译含否定掩码的 `CompiledRecordFilter`，派发至对应的子检索引擎。
pub fn dispatch_with_filter(
    store: &IndexStore,
    query: &str,
    mode: SearchMode,
    filter: QueryFilter,
    case_sensitivity: CaseSensitivity,
    max_results: usize,
) -> (Vec<AdvancedSearchResult>, bool, Option<String>, SearchMode) {
    let read = store.read();
    let prepared = prepare_search(query, mode, case_sensitivity, false, |ext| {
        read.extension_id(ext).is_some()
    });
    dispatch_with_filter_inner(
        &read,
        &prepared,
        mode,
        &filter,
        case_sensitivity,
        max_results,
        None,
        FuzzyPass::Both,
    )
}

struct PreparedSearch {
    parsed: ParsedQuery,
    disjunction: Option<DisjunctionQuery>,
    single_extension_exists: Option<bool>,
}

fn prepare_search(
    query: &str,
    mode: SearchMode,
    case_sensitivity: CaseSensitivity,
    across_shards: bool,
    has_extension: impl Fn(&str) -> bool,
) -> PreparedSearch {
    let mut parsed = parse_query_structured(query);
    let has_regex_prefix = parsed.positive_tokens.iter().any(|t| {
        !t.negated
            && !t.quoted
            && !t.was_sanitized_pasted_path
            && t.text.starts_with("regex:")
    });
    let top_level_regex = mode == SearchMode::Regex
        || has_regex_prefix
        || extract_regex_sugar_from_tokens(&parsed.positive_tokens).is_some();

    // 并集检索（OR 查询）路由：在自动且非顶层正则模式下，先按双引号/转义感知的 `|` 切分，再逐子句解析过滤器。
    let disjunction = if mode == SearchMode::Auto && !top_level_regex && has_unescaped_pipe(query) {
        let global_case = match case_sensitivity {
            CaseSensitivity::Auto => None,
            CaseSensitivity::Sensitive => Some(true),
            CaseSensitivity::Insensitive => Some(false),
        };
        let clauses = split_query_pipes(query)
            .into_iter()
            .map(|raw| {
                let mut clause = parse_query_structured(&raw);
                resolve_inferred_exts(&mut clause, &has_extension);
                DisjunctionQuery::build_clause_with_extension_check(
                    raw,
                    clause,
                    global_case,
                    |ext| !across_shards || has_extension(ext),
                )
            })
            .collect();
        Some(DisjunctionQuery { clauses })
    } else {
        resolve_inferred_exts(&mut parsed, &has_extension);
        None
    };
    let single_extension_exists = if across_shards && disjunction.is_none() {
        substring::single_inferred_extension(&parsed.positive_tokens).map(|ext| has_extension(&ext))
    } else {
        None
    };
    PreparedSearch {
        parsed,
        disjunction,
        single_extension_exists,
    }
}

enum FuzzyPass<'a> {
    Both,
    Precise,
    Jump {
        seen: &'a std::collections::HashSet<String>,
        output: &'a mut Vec<(AdvancedSearchResult, u32)>,
    },
}

#[allow(clippy::too_many_arguments)]
fn dispatch_with_filter_inner(
    read: &CompactIndex,
    prepared: &PreparedSearch,
    mode: SearchMode,
    filter: &QueryFilter,
    case_sensitivity: CaseSensitivity,
    max_results: usize,
    shard_context: Option<(&SearchShard<'_>, &[SearchShard<'_>])>,
    fuzzy_pass: FuzzyPass<'_>,
) -> (Vec<AdvancedSearchResult>, bool, Option<String>, SearchMode) {
    let explicit_lock = case_sensitivity == CaseSensitivity::Sensitive;
    if let Some(disjunction_query) = &prepared.disjunction {
        if disjunction_query.is_empty() {
            return (Vec::new(), false, None, SearchMode::Auto);
        }
        let is_case_sensitive = match case_sensitivity {
            CaseSensitivity::Sensitive => true,
            CaseSensitivity::Insensitive => false,
            CaseSensitivity::Auto => disjunction_query
                .clauses
                .iter()
                .any(|c| c.is_case_sensitive),
        };
        let clause_compiled_filters: Vec<_> = disjunction_query
            .clauses
            .iter()
            .map(|clause| {
                let clause_filter = clause.filter.merged_with(filter);
                let clause_scopes = if let Some((shard, _)) = shard_context {
                    if let (Some(configured), Some(active)) =
                        (shard.configured_path, shard.active_path)
                    {
                        path_query::rewrite_scope_candidates_drift_alias(
                            &clause.scope_candidates,
                            configured,
                            active,
                        )
                    } else {
                        clause.scope_candidates.clone()
                    }
                } else {
                    clause.scope_candidates.clone()
                };
                CompiledRecordFilter::compile_with_case_and_scope(
                    read,
                    &clause_filter,
                    clause.is_case_sensitive,
                    &clause_scopes,
                )
            })
            .collect();
        let fallback_filter =
            CompiledRecordFilter::compile_with_case(read, filter, is_case_sensitive);
        let results = substring::search_disjunction_with_filters(
            read,
            disjunction_query,
            &fallback_filter,
            Some(&clause_compiled_filters),
            max_results,
            explicit_lock,
        );
        return (results, is_case_sensitive, None, SearchMode::Auto);
    }

    let parsed = &prepared.parsed;
    let effective_filter = parsed.filter.merged_with(filter);

    // 识别行内正则语法糖（剥离 `in:<scope>` 后提取，支持 `in:src /^test_.*\.rs/` 正交组合）
    let regex_sugar = if mode == SearchMode::Auto {
        extract_regex_sugar_from_tokens(&parsed.positive_tokens)
    } else {
        None
    };

    // 权威裁决实际生效模式：显式 Regex 锁定或非引号正则语法糖命中均派发至 Regex 模式
    let effective_mode = match mode {
        SearchMode::Auto => {
            if regex_sugar.is_some()
                || parsed.positive_tokens.iter().any(|t| {
                    !t.negated
                        && !t.quoted
                        && !t.was_sanitized_pasted_path
                        && t.text.starts_with("regex:")
                })
            {
                SearchMode::Regex
            } else {
                detect_mode_from_tokens(&parsed.positive_tokens)
            }
        }
        other => other,
    };

    // 提取参与底层编译的纯模式文本与三级大小写仲裁策略：
    // 第 1 级（最高）：行内语法糖显式标志（如 /pattern/i 强制不敏感）
    // 第 2 级：全局显式锁定（Sensitive / Insensitive）
    // 第 3 级：Smart Case 启发式自适应推导（跳过 `!` 否定词与过滤标签）
    let (pattern_to_run, is_case_sensitive) = if effective_mode == SearchMode::Regex {
        if let Some(sugar) = regex_sugar {
            let case_sens = if sugar.case_insensitive {
                false
            } else {
                resolve_case_sensitive(&sugar.pattern, case_sensitivity, SearchMode::Regex)
            };
            (Some(sugar.pattern), case_sens)
        } else {
            let raw = extract_content_terms_from_tokens(&parsed.positive_tokens, SearchMode::Regex)
                .join(" ");
            let case_sens = resolve_case_sensitive_tokens(
                &parsed.positive_tokens,
                case_sensitivity,
                SearchMode::Regex,
            );
            (Some(raw), case_sens)
        }
    } else {
        let case_sens = resolve_case_sensitive_tokens(
            &parsed.positive_tokens,
            case_sensitivity,
            effective_mode,
        );
        (None, case_sens)
    };

    // 为正则模式以及仅带 `in:<scope>`（无 `/` 路径段）的模糊模式编译父目录作用域掩码，
    // 确保 `in:` 作用域与正则、模糊检索完全正交，且兼容挂载卷漂移别名重写。
    let has_unquoted_slash = parsed
        .positive_tokens
        .iter()
        .any(|t| !t.negated && !t.quoted && !t.text.starts_with("in:") && t.text.contains('/'));
    let filter_scope_candidates = if effective_mode == SearchMode::Regex
        || (effective_mode == SearchMode::Fuzzy && !has_unquoted_slash)
    {
        let raw_scopes = path_query::build_scope_candidates_from_tokens(&parsed.positive_tokens);
        if let Some((shard, _)) = shard_context {
            if let (Some(configured), Some(active)) = (shard.configured_path, shard.active_path) {
                path_query::rewrite_scope_candidates_drift_alias(&raw_scopes, configured, active)
            } else {
                raw_scopes
            }
        } else {
            raw_scopes
        }
    } else {
        Vec::new()
    };

    let compiled_filter = CompiledRecordFilter::compile_with_case_and_scope(
        read,
        &effective_filter,
        is_case_sensitive,
        &filter_scope_candidates,
    );

    // 含未加引号路径语法（'/' 或 'in:'）时交由路径检索器（正则及纯 `in:` 模糊已通过作用域掩码正交处理）。
    let bypass_path_query = effective_mode == SearchMode::Regex
        || (effective_mode == SearchMode::Fuzzy && !has_unquoted_slash && !filter_scope_candidates.is_empty());
    if !bypass_path_query {
        if let Some(mut path_query) =
            path_query::PathQuery::parse_from_tokens(&parsed.positive_tokens, is_case_sensitive)
        {
            if matches!(fuzzy_pass, FuzzyPass::Jump { .. }) {
                return (Vec::new(), is_case_sensitive, None, effective_mode);
            }
            if let Some((shard, _)) = shard_context {
                if let (Some(configured), Some(active)) = (shard.configured_path, shard.active_path)
                {
                    path_query = path_query.rewrite_drift_alias(configured, active);
                }
            }
            let results = path_search::search_index(
                read,
                &path_query,
                &compiled_filter,
                max_results,
                is_case_sensitive,
                explicit_lock,
            );
            if !results.is_empty() || max_results == 0 {
                return (results, is_case_sensitive, None, effective_mode);
            }

            if let Some((colon_results, fallback_cs)) = try_posix_colon_fallback(
                read,
                &parsed.positive_tokens,
                &path_query,
                &effective_filter,
                &compiled_filter,
                case_sensitivity,
                is_case_sensitive,
                explicit_lock,
                effective_mode,
                max_results,
            ) {
                return (colon_results, fallback_cs, None, effective_mode);
            }

            if let Some((shard, shards)) = shard_context {
                if !shard_allows_live_fallback(shard, shards, &path_query) {
                    return (Vec::new(), is_case_sensitive, None, effective_mode);
                }
            }
            let read_dir_results = fallback_read_dir(
                &path_query,
                &compiled_filter,
                max_results,
                is_case_sensitive,
                explicit_lock,
                effective_mode,
            );
            return (read_dir_results, is_case_sensitive, None, effective_mode);
        }
    }

    // For Auto that detected as Auto (no special chars), run substring search.
    // 保持高确定性与精准度：普通文本输入严格按连续子串匹配，搜不到即返回空列表，避免无脑降级为模糊跳字匹配。
    let (mut results, error) = match effective_mode {
        SearchMode::Auto | SearchMode::Substring => {
            let results = substring::search_index_tokens_with_hybrid(
                read,
                &parsed.positive_tokens,
                &compiled_filter,
                max_results,
                is_case_sensitive,
                case_sensitivity,
                explicit_lock,
                prepared.single_extension_exists,
            );
            (results, None)
        }
        SearchMode::Wildcard => {
            let results = wildcard::search_index_tokens(
                read,
                &parsed.positive_tokens,
                &compiled_filter,
                max_results,
                is_case_sensitive,
            );
            (results, None)
        }
        SearchMode::Regex => {
            let pattern = pattern_to_run.as_deref().unwrap_or(&parsed.clean_query);
            regex_search::search_index(
                read,
                pattern,
                &compiled_filter,
                max_results,
                is_case_sensitive,
            )
        }
        SearchMode::Fuzzy => {
            let stripped_tokens: Vec<QueryToken>;
            let fuzzy_tokens: &[QueryToken] = if filter_scope_candidates.is_empty() {
                &parsed.positive_tokens
            } else {
                stripped_tokens = parsed
                    .positive_tokens
                    .iter()
                    .filter(|t| !(!t.quoted && t.text.starts_with("in:")))
                    .cloned()
                    .collect();
                &stripped_tokens
            };
            if let FuzzyPass::Jump { seen, output } = fuzzy_pass {
                if prepared.single_extension_exists == Some(true) {
                    return (Vec::new(), is_case_sensitive, None, effective_mode);
                }
                let mut raw = fuzzy::search_index_tokens_raw(
                    read,
                    fuzzy_tokens,
                    &compiled_filter,
                    max_results,
                    is_case_sensitive,
                    seen,
                );
                if !filter_scope_candidates.is_empty() {
                    for (item, _) in &mut raw {
                        let parent = crate::compact_index::split_path(&item.entry.path).0;
                        if let Some(span) =
                            path_search::find_scope_highlight_range(parent, &filter_scope_candidates)
                        {
                            item.parent_match_positions = vec![span];
                        }
                    }
                }
                *output = raw;
                return (Vec::new(), is_case_sensitive, None, effective_mode);
            }
            // 模糊模式是 Auto 的放宽：子串/混合/拼音命中在前，跳字命中去重后补足剩余名额
            let mut results = substring::search_index_tokens_with_hybrid(
                read,
                fuzzy_tokens,
                &compiled_filter,
                max_results,
                is_case_sensitive,
                case_sensitivity,
                explicit_lock,
                prepared.single_extension_exists,
            );
            let remaining = max_results.saturating_sub(results.len());
            if remaining == 0 || matches!(fuzzy_pass, FuzzyPass::Precise) {
                if !filter_scope_candidates.is_empty() {
                    for item in &mut results {
                        if item.parent_match_positions.is_empty() {
                            let parent = crate::compact_index::split_path(&item.entry.path).0;
                            if let Some(span) = path_search::find_scope_highlight_range(
                                parent,
                                &filter_scope_candidates,
                            ) {
                                item.parent_match_positions = vec![span];
                            }
                        }
                    }
                }
                return (results, is_case_sensitive, None, effective_mode);
            }
            let seen: std::collections::HashSet<String> =
                results.iter().map(|r| r.entry.path.clone()).collect();
            results.extend(
                fuzzy::search_index_tokens(
                    read,
                    fuzzy_tokens,
                    &compiled_filter,
                    max_results,
                    is_case_sensitive,
                )
                .into_iter()
                .filter(|r| !seen.contains(&r.entry.path))
                .take(remaining),
            );
            (results, None)
        }
    };

    if !filter_scope_candidates.is_empty() {
        for item in &mut results {
            if item.parent_match_positions.is_empty() {
                let parent = crate::compact_index::split_path(&item.entry.path).0;
                if let Some(span) =
                    path_search::find_scope_highlight_range(parent, &filter_scope_candidates)
                {
                    item.parent_match_positions = vec![span];
                }
            }
        }
    }

    (results, is_case_sensitive, error, effective_mode)
}

/// 支持指定大小写匹配策略的高级检索分发入口。
pub fn dispatch_with_options(
    store: &IndexStore,
    query: &str,
    mode: SearchMode,
    case_sensitivity: CaseSensitivity,
    max_results: usize,
) -> (Vec<AdvancedSearchResult>, bool, Option<String>) {
    let (results, is_case_sensitive, error, _) = dispatch_with_filter(
        store,
        query,
        mode,
        QueryFilter::default(),
        case_sensitivity,
        max_results,
    );
    (results, is_case_sensitive, error)
}

/// 支持指定大小写匹配策略的高级检索分发入口，附带权威生效模式反馈。
pub fn dispatch_with_details(
    store: &IndexStore,
    query: &str,
    mode: SearchMode,
    case_sensitivity: CaseSensitivity,
    max_results: usize,
) -> (Vec<AdvancedSearchResult>, bool, Option<String>, SearchMode) {
    dispatch_with_filter(
        store,
        query,
        mode,
        QueryFilter::default(),
        case_sensitivity,
        max_results,
    )
}

/// 主检索分发器包装入口。
///
/// 委托 `dispatch_with_options` 并传入默认 Auto 大小写策略与默认过滤器，保持向后兼容。
pub fn dispatch(
    store: &IndexStore,
    query: &str,
    mode: SearchMode,
    max_results: usize,
) -> (Vec<AdvancedSearchResult>, Option<String>) {
    let (results, _, error) =
        dispatch_with_options(store, query, mode, CaseSensitivity::Auto, max_results);
    (results, error)
}
/// 单个分片达到该条目阈值且至少存在两个大盘分片时，才启用跨分片并发检索；
/// 常规场景（主索引 + 若干中小容量挂载卷）直接在调用线程串行执行，避免每次按键反复创建和销毁 OS 线程。
const PARALLEL_SHARD_MIN_ENTRIES: usize = 50_000;

fn run_parallel_map<T: Send>(
    active_shards: &[&SearchShard<'_>],
    f: impl Fn(&SearchShard<'_>) -> T + Sync,
) -> Vec<T> {
    if active_shards.len() <= 1 {
        return active_shards.iter().map(|shard| f(shard)).collect();
    }

    let large_shard_count = active_shards
        .iter()
        .filter(|shard| shard.index.len() >= PARALLEL_SHARD_MIN_ENTRIES)
        .count();
    if large_shard_count < 2 {
        return active_shards.iter().map(|shard| f(shard)).collect();
    }

    // 主调用线程直接承担首个分片，其余分片按 CPU 上限分组并发，避免多盘场景派生过多 OS 线程。
    let max_workers = std::thread::available_parallelism()
        .map(|n| n.get().clamp(2, 8))
        .unwrap_or(4);
    let extra_shards = &active_shards[1..];
    let worker_count = (max_workers - 1).min(extra_shards.len());
    let chunk_size = extra_shards.len().div_ceil(worker_count);

    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(worker_count);
        for chunk in extra_shards.chunks(chunk_size) {
            let f_ref = &f;
            handles.push(scope.spawn(move || {
                chunk.iter().map(|shard| f_ref(shard)).collect::<Vec<_>>()
            }));
        }
        let first = f(active_shards[0]);
        let mut results = Vec::with_capacity(active_shards.len());
        results.push(first);
        for handle in handles {
            results.extend(handle.join().unwrap());
        }
        results
    })
}

fn wrap_shard_results(
    results: Vec<AdvancedSearchResult>,
    shard: &SearchShard<'_>,
) -> Vec<MultiSearchResult> {
    results
        .into_iter()
        .map(|result| MultiSearchResult {
            result,
            volume_name: shard.volume_name.map(|v| v.to_string()),
            is_offline: shard.is_offline,
        })
        .collect()
}

fn merge_shard_results_by(
    shard_results: Vec<Vec<MultiSearchResult>>,
    max_results: usize,
    mut cmp: impl FnMut(&MultiSearchResult, &MultiSearchResult) -> std::cmp::Ordering,
) -> Vec<MultiSearchResult> {
    if shard_results.is_empty() || max_results == 0 {
        return Vec::new();
    }
    let total_count: usize = shard_results.iter().map(|v| v.len()).sum();
    let mut all = Vec::with_capacity(total_count);
    for list in shard_results {
        all.extend(list);
    }
    all.sort_by(|a, b| {
        cmp(a, b)
            .then_with(|| a.result.entry.name.cmp(&b.result.entry.name))
            .then_with(|| a.result.entry.path.cmp(&b.result.entry.path))
    });
    all.truncate(max_results);
    all
}

fn cmp_score_desc(a: &MultiSearchResult, b: &MultiSearchResult) -> std::cmp::Ordering {
    b.result
        .score
        .partial_cmp(&a.result.score)
        .unwrap_or(std::cmp::Ordering::Equal)
}

fn cmp_modified_desc(a: &MultiSearchResult, b: &MultiSearchResult) -> std::cmp::Ordering {
    b.result.entry.modified.cmp(&a.result.entry.modified)
}

fn shard_allows_live_fallback(
    shard: &SearchShard<'_>,
    shards: &[SearchShard<'_>],
    query: &path_query::PathQuery,
) -> bool {
    use crate::path_utils::path_starts_with_ci;
    use std::path::Path;

    let Some(dir) = query.raw_dir_pattern.as_deref().map(Path::new) else {
        return false;
    };
    if shard.is_offline {
        return false;
    }
    if let Some(active) = shard.active_path {
        return path_starts_with_ci(dir, Path::new(active));
    }
    // 主索引分片严禁对 /Volumes/... 命名空间做同步 read_dir 兜底，防止未配置或父级失联网络挂载点卡死搜索线程
    if path_starts_with_ci(dir, Path::new("/Volumes")) {
        return false;
    }
    !shards.iter().any(|other| {
        [other.active_path, other.configured_path]
            .into_iter()
            .flatten()
            .any(|root| path_starts_with_ci(dir, Path::new(root)))
    })
}

/// 检索活跃分片的快照，卷间共享查询解析与隐式扩展名裁决，卷内独立编译记录过滤器。
pub fn dispatch_multi(
    shards: &[SearchShard<'_>],
    query: &str,
    mode: SearchMode,
    filter: QueryFilter,
    case_sensitivity: CaseSensitivity,
    max_results: usize,
    include_offline: bool,
) -> (Vec<MultiSearchResult>, bool, Option<String>, SearchMode) {
    let active_shards: Vec<_> = shards
        .iter()
        .filter(|s| include_offline || !s.is_offline)
        .collect();
    let prepared = prepare_search(query, mode, case_sensitivity, true, |ext| {
        active_shards
            .iter()
            .any(|shard| shard.index.extension_id(ext).is_some())
    });
    if active_shards.is_empty() || max_results == 0 {
        let empty = CompactIndex::default();
        let (_, is_case_sensitive, _, effective_mode) = dispatch_with_filter_inner(
            &empty,
            &prepared,
            mode,
            &filter,
            case_sensitivity,
            0,
            None,
            FuzzyPass::Precise,
        );
        return (Vec::new(), is_case_sensitive, None, effective_mode);
    }

    let per_shard = run_parallel_map(&active_shards, |shard| {
        let (results, is_case_sensitive, error, effective_mode) = dispatch_with_filter_inner(
            shard.index,
            &prepared,
            mode,
            &filter,
            case_sensitivity,
            max_results,
            Some((shard, shards)),
            FuzzyPass::Precise,
        );
        (
            wrap_shard_results(results, shard),
            is_case_sensitive,
            error,
            effective_mode,
        )
    });
    let is_case_sensitive = per_shard.iter().any(|p| p.1);
    let effective_mode = per_shard[0].3;
    let mut shard_results = Vec::with_capacity(per_shard.len());
    let mut error = None;
    for (results, _, shard_error, _) in per_shard {
        error = error.or(shard_error);
        shard_results.push(results);
    }
    let cmp = if effective_mode == SearchMode::Wildcard && prepared.parsed.clean_query.trim() == "*"
    {
        cmp_modified_desc
    } else {
        cmp_score_desc
    };
    let mut merged = merge_shard_results_by(shard_results, max_results, cmp);

    let remaining = max_results - merged.len();
    if effective_mode == SearchMode::Fuzzy && remaining > 0 {
        let mut seen: std::collections::HashSet<String> = merged
            .iter()
            .map(|res| res.result.entry.path.clone())
            .collect();
        let mut candidates: Vec<_> = run_parallel_map(&active_shards, |shard| {
            let mut raw = Vec::new();
            dispatch_with_filter_inner(
                shard.index,
                &prepared,
                mode,
                &filter,
                case_sensitivity,
                remaining,
                Some((shard, shards)),
                FuzzyPass::Jump {
                    seen: &seen,
                    output: &mut raw,
                },
            );
            raw.into_iter()
                .map(|(result, score)| {
                    (
                        MultiSearchResult {
                            result,
                            volume_name: shard.volume_name.map(str::to_owned),
                            is_offline: shard.is_offline,
                        },
                        score,
                    )
                })
                .collect::<Vec<_>>()
        })
        .into_iter()
        .flatten()
        .collect();
        candidates.sort_by(|(left, left_score), (right, right_score)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.result.entry.name.cmp(&right.result.entry.name))
                .then_with(|| left.result.entry.path.cmp(&right.result.entry.path))
        });
        merged.extend(
            candidates
                .into_iter()
                .map(|(res, _)| res)
                .filter(|res| seen.insert(res.result.entry.path.clone()))
                .take(remaining),
        );
    }
    (merged, is_case_sensitive, error, effective_mode)
}

/// 多源并行检索协调器核心入口。
///

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_mode_wildcard_star() {
        assert_eq!(detect_mode("*.pdf"), SearchMode::Wildcard);
    }

    #[test]
    fn test_detect_mode_wildcard_question() {
        assert_eq!(detect_mode("test??.rs"), SearchMode::Wildcard);
    }

    #[test]
    fn test_detect_mode_regex() {
        assert_eq!(detect_mode("regex:^main"), SearchMode::Regex);
    }

    #[test]
    fn test_detect_mode_auto() {
        assert_eq!(detect_mode("main"), SearchMode::Auto);
    }

    #[test]
    fn test_detect_mode_empty() {
        assert_eq!(detect_mode(""), SearchMode::Auto);
    }

    #[test]
    fn test_detect_mode_or_pipe_overrides_wildcard() {
        assert_eq!(detect_mode("*.png | *.jpg"), SearchMode::Auto);
    }

    #[test]
    fn test_dispatch_substring_mode() {
        let store = IndexStore::new();
        store.replace(vec![crate::indexer::FileEntry {
            name: "main.rs".to_string(),
            path: "/src/main.rs".to_string(),
            is_dir: false,
            size: 1024,
            modified: 1700000000,
            created_at: 0,
            extension: "rs".to_string(),
        }]);
        let (results, error) = dispatch(&store, "main", SearchMode::Substring, 500);
        assert!(error.is_none());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "main.rs");
        assert_eq!(results[0].search_mode, SearchMode::Substring);
    }

    #[test]
    fn test_dispatch_auto_mode_uses_substring() {
        let store = IndexStore::new();
        store.replace(vec![crate::indexer::FileEntry {
            name: "main.rs".to_string(),
            path: "/src/main.rs".to_string(),
            is_dir: false,
            size: 1024,
            modified: 1700000000,
            created_at: 0,
            extension: "rs".to_string(),
        }]);
        let (results, error) = dispatch(&store, "main", SearchMode::Auto, 500);
        assert!(error.is_none());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].search_mode, SearchMode::Substring);
    }

    #[test]
    fn test_dispatch_fuzzy_mode_extends_auto() {
        let store = IndexStore::new();
        let entry = |name: &str| crate::indexer::FileEntry {
            name: name.to_string(),
            path: format!("/docs/{name}"),
            is_dir: false,
            size: 1024,
            modified: 1700000000,
            created_at: 0,
            extension: name.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default(),
        };
        store.replace(vec![entry("z_u_o_y_e.md"), entry("作业.docx"), entry("zuoye.txt")]);
        let (results, error) = dispatch(&store, "zuoye", SearchMode::Fuzzy, 500);
        assert!(error.is_none());
        let names: Vec<_> = results.iter().map(|r| r.entry.name.as_str()).collect();
        assert_eq!(names.len(), 3, "no duplicates, nothing dropped: {names:?}");
        assert!(names[..2].contains(&"作业.docx"), "pinyin hit kept and ranked first: {names:?}");
        assert!(names[..2].contains(&"zuoye.txt"));
        assert_eq!(names[2], "z_u_o_y_e.md", "subsequence-only hit appended last");
    }

    #[test]
    fn test_dispatch_auto_mode_dotfile_prefix() {
        let store = IndexStore::new();
        store.replace(vec![
            crate::indexer::FileEntry {
                name: ".gitignore".to_string(),
                path: "/project/.gitignore".to_string(),
                is_dir: false,
                size: 100,
                modified: 1700000000,
                created_at: 0,
                extension: "".to_string(),
            },
            crate::indexer::FileEntry {
                name: "test.gitignore".to_string(),
                path: "/project/test.gitignore".to_string(),
                is_dir: false,
                size: 200,
                modified: 1700000000,
                created_at: 0,
                extension: "gitignore".to_string(),
            },
        ]);
        let (results, error) = dispatch(&store, ".giti", SearchMode::Auto, 500);
        assert!(error.is_none());
        let names: Vec<_> = results.iter().map(|r| r.entry.name.as_str()).collect();
        assert!(names.contains(&".gitignore"));
        assert!(names.contains(&"test.gitignore"));
    }

    #[test]
    fn test_multi_term_inferred_ext_fallback_to_query() {
        let store = IndexStore::new();
        store.replace(vec![
            crate::indexer::FileEntry {
                name: "main.rs".to_string(),
                path: "/project/main.rs".to_string(),
                is_dir: false,
                size: 1000,
                modified: 1700000000,
                created_at: 0,
                extension: "rs".to_string(),
            },
            crate::indexer::FileEntry {
                name: "myproject.custom_hidden".to_string(),
                path: "/project/myproject.custom_hidden".to_string(),
                is_dir: false,
                size: 500,
                modified: 1700000000,
                created_at: 0,
                extension: "custom_hidden".to_string(),
            },
        ]);

        // 1. 已知合法扩展名：正常作为过滤修饰符
        let (results_known, err_known) = dispatch(&store, "main .rs", SearchMode::Auto, 500);
        assert!(err_known.is_none());
        assert_eq!(results_known.len(), 1);
        assert_eq!(results_known[0].entry.name, "main.rs");

        // 2. 独立推断的生僻/未知扩展名前缀（索引中查无此扩展名）：安全归还为普通检索词，避免误将搜索结果清空
        let (results_unknown, err_unknown) = dispatch(&store, "myproject .custom", SearchMode::Auto, 500);
        assert!(err_unknown.is_none());
        assert_eq!(results_unknown.len(), 1);
        assert_eq!(results_unknown[0].entry.name, "myproject.custom_hidden");

        // 3. 显式 ext: 语法：严格遵循显式强约束，查无该扩展名则正常返回空列表
        let (results_explicit, err_explicit) = dispatch(&store, "myproject ext:custom", SearchMode::Auto, 500);
        assert!(err_explicit.is_none());
        assert!(results_explicit.is_empty());
    }

    #[test]
    fn test_dispatch_wildcard_mode() {
        let store = IndexStore::new();
        store.replace(vec![crate::indexer::FileEntry {
            name: "report.pdf".to_string(),
            path: "/report.pdf".to_string(),
            is_dir: false,
            size: 1024,
            modified: 1700000000,
            created_at: 0,
            extension: "pdf".to_string(),
        }]);
        let (results, error) = dispatch(&store, "*.pdf", SearchMode::Wildcard, 500);
        assert!(error.is_none());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "report.pdf");
        assert_eq!(results[0].search_mode, SearchMode::Wildcard);
    }

    #[test]
    fn test_dispatch_regex_mode() {
        let store = IndexStore::new();
        store.replace(vec![crate::indexer::FileEntry {
            name: "main.rs".to_string(),
            path: "/src/main.rs".to_string(),
            is_dir: false,
            size: 1024,
            modified: 1700000000,
            created_at: 0,
            extension: "rs".to_string(),
        }]);
        let (results, error) = dispatch(&store, "regex:^main", SearchMode::Regex, 500);
        assert!(error.is_none());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "main.rs");
        assert_eq!(results[0].search_mode, SearchMode::Regex);
    }

    #[test]
    fn test_dispatch_fuzzy_mode() {
        let store = IndexStore::new();
        store.replace(vec![crate::indexer::FileEntry {
            name: "main.rs".to_string(),
            path: "/src/main.rs".to_string(),
            is_dir: false,
            size: 1024,
            modified: 1700000000,
            created_at: 0,
            extension: "rs".to_string(),
        }]);
        let (results, error) = dispatch(&store, "mnrs", SearchMode::Fuzzy, 500);
        assert!(error.is_none());
        assert!(!results.is_empty(), "fuzzy should match mnrs to main.rs");
        assert_eq!(results[0].search_mode, SearchMode::Fuzzy);
    }

    #[test]
    fn test_dispatch_auto_fallback_to_fuzzy() {
        let store = IndexStore::new();
        store.replace(vec![crate::indexer::FileEntry {
            name: "main.rs".to_string(),
            path: "/src/main.rs".to_string(),
            is_dir: false,
            size: 1024,
            modified: 1700000000,
            created_at: 0,
            extension: "rs".to_string(),
        }]);
        // "mnrs" won't match substring, in Auto mode it returns empty (no unexpected fuzzy fallback)
        let (results, error) = dispatch(&store, "mnrs", SearchMode::Auto, 500);
        assert!(error.is_none());
        assert!(results.is_empty(), "Auto mode should not silently fallback to fuzzy");

        // When explicit Fuzzy mode is selected, fuzzy matching works as expected
        let (fuzzy_results, fuzzy_error) = dispatch(&store, "mnrs", SearchMode::Fuzzy, 500);
        assert!(fuzzy_error.is_none());
        assert!(!fuzzy_results.is_empty(), "Explicit Fuzzy mode should match");
        assert_eq!(fuzzy_results[0].search_mode, SearchMode::Fuzzy);
    }

    #[test]
    fn test_dispatch_auto_prefers_substring() {
        let store = IndexStore::new();
        store.replace(vec![crate::indexer::FileEntry {
            name: "main.rs".to_string(),
            path: "/src/main.rs".to_string(),
            is_dir: false,
            size: 1024,
            modified: 1700000000,
            created_at: 0,
            extension: "rs".to_string(),
        }]);
        // "main" matches substring, should NOT fallback to fuzzy
        let (results, error) = dispatch(&store, "main", SearchMode::Auto, 500);
        assert!(error.is_none());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].search_mode, SearchMode::Substring);
    }

    #[test]
    fn test_advanced_search_result_serializes_camelcase() {
        let result = AdvancedSearchResult {
            entry: crate::indexer::FileEntry {
                name: "test.rs".to_string(),
                path: "/test.rs".to_string(),
                is_dir: false,
                size: 100,
                modified: 1700000000,
                created_at: 0,
                extension: "rs".to_string(),
            },
            score: 0.75,
            match_positions: vec![(0, 4)],
            parent_match_positions: Vec::new(),
            search_mode: SearchMode::Substring,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(
            json.contains("matchPositions"),
            "should have camelCase matchPositions"
        );
        assert!(
            json.contains("searchMode"),
            "should have camelCase searchMode"
        );
    }

    #[test]
    fn test_dispatch_path_query_routes_to_path_search() {
        let store = IndexStore::new();
        store.replace(vec![
            crate::indexer::FileEntry {
                name: "main.rs".to_string(),
                path: "/project/src/main.rs".to_string(),
                is_dir: false,
                size: 1024,
                modified: 1700000000,
                created_at: 0,
                extension: "rs".to_string(),
            },
            crate::indexer::FileEntry {
                name: "main.rs".to_string(),
                path: "/other/pkg/main.rs".to_string(),
                is_dir: false,
                size: 1024,
                modified: 1700000000,
                created_at: 0,
                extension: "rs".to_string(),
            },
        ]);

        let (results, err) = dispatch(&store, "src/main.rs", SearchMode::Auto, 10);
        assert!(err.is_none());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.path, "/project/src/main.rs");
        assert!(!results[0].parent_match_positions.is_empty());
    }

    #[test]
    fn test_dispatch_regex_preserves_slash() {
        let store = IndexStore::new();
        store.replace(vec![crate::indexer::FileEntry {
            name: "main.rs".to_string(),
            path: "/src/main.rs".to_string(),
            is_dir: false,
            size: 1024,
            modified: 1700000000,
            created_at: 0,
            extension: "rs".to_string(),
        }]);

        // regex: 模式即使含 / 也不被 path_search 拦截
        let (results, err) = dispatch(&store, "regex:main\\.rs", SearchMode::Auto, 10);
        assert!(err.is_none());
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_with_file_type_filter() {
        let store = IndexStore::new();
        store.replace(vec![
            crate::indexer::FileEntry {
                name: "search_engine.rs".to_string(),
                path: "/src/search_engine.rs".to_string(),
                is_dir: false,
                size: 1024,
                modified: 1700000000,
                created_at: 0,
                extension: "rs".to_string(),
            },
            crate::indexer::FileEntry {
                name: "search_doc.md".to_string(),
                path: "/docs/search_doc.md".to_string(),
                is_dir: false,
                size: 2048,
                modified: 1700000000,
                created_at: 0,
                extension: "md".to_string(),
            },
        ]);

        let filter = QueryFilter {
            file_type: FileType::Code,
            custom_ext: None,
            ..Default::default()
        };
        let (results, _, err, _) =
            dispatch_with_filter(&store, "search", SearchMode::Auto, filter, CaseSensitivity::Auto, 10);
        assert!(err.is_none());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "search_engine.rs");
    }

    #[test]
    fn test_dispatch_type_doc_syntax() {
        let store = IndexStore::new();
        store.replace(vec![
            crate::indexer::FileEntry {
                name: "architecture.rs".to_string(),
                path: "/src/architecture.rs".to_string(),
                is_dir: false,
                size: 1024,
                modified: 1700000000,
                created_at: 0,
                extension: "rs".to_string(),
            },
            crate::indexer::FileEntry {
                name: "architecture.md".to_string(),
                path: "/docs/architecture.md".to_string(),
                is_dir: false,
                size: 2048,
                modified: 1700000000,
                created_at: 0,
                extension: "md".to_string(),
            },
        ]);

        let (results, err) = dispatch(&store, "architecture type:doc", SearchMode::Auto, 10);
        assert!(err.is_none());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "architecture.md");
    }

    #[test]
    fn test_fuzzy_with_file_type_filter() {
        let store = IndexStore::new();
        store.replace(vec![
            crate::indexer::FileEntry {
                name: "render_view.rs".to_string(),
                path: "/src/render_view.rs".to_string(),
                is_dir: false,
                size: 1024,
                modified: 1700000000,
                created_at: 0,
                extension: "rs".to_string(),
            },
            crate::indexer::FileEntry {
                name: "render_spec.md".to_string(),
                path: "/docs/render_spec.md".to_string(),
                is_dir: false,
                size: 2048,
                modified: 1700000000,
                created_at: 0,
                extension: "md".to_string(),
            },
        ]);

        // 检索 "rndrvw"，仅在 FileType::Code 下命中 render_view.rs
        let filter = QueryFilter {
            file_type: FileType::Code,
            custom_ext: None,
            ..Default::default()
        };
        let (results, _, err, _) =
            dispatch_with_filter(&store, "rndrvw", SearchMode::Fuzzy, filter, CaseSensitivity::Auto, 10);
        assert!(err.is_none());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "render_view.rs");
    }

    #[test]
    fn test_path_search_with_file_type_filter() {
        let store = IndexStore::new();
        store.replace(vec![
            crate::indexer::FileEntry {
                name: "component.rs".to_string(),
                path: "/workspace/src/component.rs".to_string(),
                is_dir: false,
                size: 1024,
                modified: 1700000000,
                created_at: 0,
                extension: "rs".to_string(),
            },
            crate::indexer::FileEntry {
                name: "component.md".to_string(),
                path: "/workspace/src/component.md".to_string(),
                is_dir: false,
                size: 2048,
                modified: 1700000000,
                created_at: 0,
                extension: "md".to_string(),
            },
        ]);

        let filter = QueryFilter {
            file_type: FileType::Code,
            custom_ext: None,
            ..Default::default()
        };
        let (results, _, err, _) =
            dispatch_with_filter(&store, "src/component", SearchMode::Auto, filter, CaseSensitivity::Auto, 10);
        assert!(err.is_none());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "component.rs");
    }

    #[test]
    fn test_inline_filter_overrides_passed_filter() {
        let store = IndexStore::new();
        store.replace(vec![
            crate::indexer::FileEntry {
                name: "pipeline.rs".to_string(),
                path: "/src/pipeline.rs".to_string(),
                is_dir: false,
                size: 1024,
                modified: 1700000000,
                created_at: 0,
                extension: "rs".to_string(),
            },
            crate::indexer::FileEntry {
                name: "pipeline.md".to_string(),
                path: "/docs/pipeline.md".to_string(),
                is_dir: false,
                size: 2048,
                modified: 1700000000,
                created_at: 0,
                extension: "md".to_string(),
            },
        ]);

        // 外部传入 Code，但查询字符串内联指定了 type:doc，应以查询内联指定为最高优先级
        let filter = QueryFilter {
            file_type: FileType::Code,
            custom_ext: None,
            ..Default::default()
        };
        let (results, _, err, _) =
            dispatch_with_filter(&store, "pipeline type:doc", SearchMode::Auto, filter, CaseSensitivity::Auto, 10);
        assert!(err.is_none());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "pipeline.md");
    }

    #[test]
    fn test_dispatch_with_size_filter() {
        let store = IndexStore::new();
        store.replace(vec![
            crate::indexer::FileEntry {
                name: "small.rs".to_string(),
                path: "/src/small.rs".to_string(),
                is_dir: false,
                size: 500,
                modified: 1700000000,
                created_at: 0,
                extension: "rs".to_string(),
            },
            crate::indexer::FileEntry {
                name: "large.rs".to_string(),
                path: "/src/large.rs".to_string(),
                is_dir: false,
                size: 50_000_000,
                modified: 1700000000,
                created_at: 0,
                extension: "rs".to_string(),
            },
        ]);

        let filter = QueryFilter {
            size_filter: SizeFilter {
                min_bytes: Some(10_000_000),
                max_bytes: None,
            },
            ..Default::default()
        };
        let (results, _, err, _) = dispatch_with_filter(&store, "rs", SearchMode::Auto, filter, CaseSensitivity::Auto, 10);
        assert!(err.is_none());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "large.rs");
    }

    #[test]
    fn test_substring_mode_treats_metachars_literally() {
        let store = IndexStore::new();
        store.replace(vec![
            crate::indexer::FileEntry {
                name: "a|b.txt".to_string(),
                path: "/docs/a|b.txt".to_string(),
                is_dir: false,
                size: 100,
                modified: 1700000000,
                created_at: 0,
                extension: "txt".to_string(),
            },
            crate::indexer::FileEntry {
                name: "alpha.txt".to_string(),
                path: "/docs/alpha.txt".to_string(),
                is_dir: false,
                size: 100,
                modified: 1700000000,
                created_at: 0,
                extension: "txt".to_string(),
            },
        ]);

        // 在 Substring（纯字面量）模式下，"a|b" 仅匹配真正包含字面量 "a|b" 的文件名，不展开为并集查询
        let (results, _, err, effective_mode) = dispatch_with_filter(
            &store,
            "a|b",
            SearchMode::Substring,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            10,
        );
        assert!(err.is_none());
        assert_eq!(effective_mode, SearchMode::Substring);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "a|b.txt");
    }

    #[test]
    fn test_map_norm_span_to_orig_grapheme_clusters() {
        // ASCII 快速路径
        assert_eq!(map_norm_span_to_orig("hello.txt", "hello.txt", 0, 5), Some((0, 5)));

        // 土耳其语 'İ' (0..2) -> "i\u{0307}" (0..3)
        let orig_tr = "İstanbul.txt";
        let norm_tr = crate::cjk::fold_str(orig_tr);
        assert_eq!(map_norm_span_to_orig(orig_tr, &norm_tr, 0, 3), Some((0, 2)));
        assert_eq!(map_norm_span_to_orig(orig_tr, &norm_tr, 0, 1), Some((0, 2)));
        assert_eq!(map_norm_span_to_orig(orig_tr, &norm_tr, 3, 10), Some((2, 9)));

        // NFD 拉丁组合重音 -> NFC
        let orig_nfd = "Cafe\u{0301}.txt";
        let norm_nfc = crate::cjk::normalize_and_simplify_cow(orig_nfd);
        assert_eq!(map_norm_span_to_orig(orig_nfd, &norm_nfc, 0, 5), Some((0, 6)));
        assert!(find_positions(orig_nfd, &crate::cjk::fold_str(orig_nfd), &[("Cafe", "cafe")]).is_empty());

        // NFD 韩文字母组合 -> NFC 音节块
        let orig_jamo = "\u{1112}\u{1161}\u{11AB}\u{1100}\u{1173}\u{11AF}.txt";
        let norm_hangul = crate::cjk::fold_str(orig_jamo);
        assert_eq!(map_norm_span_to_orig(orig_jamo, &norm_hangul, 0, 6), Some((0, 18)));
    }

    #[test]
    fn test_quoted_phrase_and_literal_mode_suppression() {
        let store = IndexStore::new();
        let make = |name: &str, path: &str, is_dir: bool, ext: &str| FileEntry {
            name: name.to_string(),
            path: path.to_string(),
            is_dir,
            size: 100,
            modified: 1700000000,
            created_at: 0,
            extension: ext.to_string(),
        };
        store.replace(vec![
            make("my hello world.txt", "/docs/my hello world.txt", false, "txt"),
            make("world hello.txt", "/docs/world hello.txt", false, "txt"),
            make("star*file.txt", "/docs/star*file.txt", false, "txt"),
            make("what?file.txt", "/docs/what?file.txt", false, "txt"),
            make("type:doc.txt", "/docs/type:doc.txt", false, "txt"),
            make("in:dir.txt", "/docs/in:dir.txt", false, "txt"),
            make("report.pdf.bak", "/docs/report.pdf.bak", false, "bak"),
            make("notes.pdf", "/docs/notes.pdf", false, "pdf"),
            make("README's guide.md", "/docs/README's guide.md", false, "md"),
        ]);

        // Contiguous phrase inside double quotes matches only "my hello world.txt"
        let (phrase_res, _, _, _) = dispatch_with_filter(
            &store,
            r#""hello world""#,
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            10,
        );
        assert_eq!(phrase_res.len(), 1);
        assert_eq!(phrase_res[0].entry.name, "my hello world.txt");

        // Quoted "*", "?", "type:doc", "in:dir", ".pdf" stay literal Substring queries
        assert_eq!(detect_mode(r#""*""#), SearchMode::Auto);
        assert_eq!(detect_mode(r#""?""#), SearchMode::Auto);
        assert_eq!(detect_mode(r#""foo/bar""#), SearchMode::Auto);

        let (star_res, _, _, _) = dispatch_with_filter(
            &store,
            r#""*""#,
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            10,
        );
        assert_eq!(star_res.iter().map(|r| r.entry.name.as_str()).collect::<Vec<_>>(), vec!["star*file.txt"]);

        let (q_res, _, _, _) = dispatch_with_filter(
            &store,
            r#""?""#,
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            10,
        );
        assert_eq!(q_res.iter().map(|r| r.entry.name.as_str()).collect::<Vec<_>>(), vec!["what?file.txt"]);

        let (type_lit, _, _, _) = dispatch_with_filter(
            &store,
            r#""type:doc""#,
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            10,
        );
        assert_eq!(type_lit.iter().map(|r| r.entry.name.as_str()).collect::<Vec<_>>(), vec!["type:doc.txt"]);

        let (in_lit, _, _, _) = dispatch_with_filter(
            &store,
            r#""in:dir""#,
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            10,
        );
        assert_eq!(in_lit.iter().map(|r| r.entry.name.as_str()).collect::<Vec<_>>(), vec!["in:dir.txt"]);

        let (dot_lit, _, _, _) = dispatch_with_filter(
            &store,
            r#"".pdf""#,
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            10,
        );
        let dot_names: Vec<_> = dot_lit.iter().map(|r| r.entry.name.as_str()).collect();
        assert!(dot_names.contains(&"report.pdf.bak"));

        // Single quote stays literal
        let (apos_res, _, _, _) = dispatch_with_filter(
            &store,
            "README's",
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            10,
        );
        assert_eq!(apos_res.len(), 1);
    }

    #[test]
    fn test_negation_across_all_search_modes_and_hidden_targeting() {
        let store = IndexStore::new();
        let make = |name: &str, path: &str, is_dir: bool, ext: &str| FileEntry {
            name: name.to_string(),
            path: path.to_string(),
            is_dir,
            size: 100,
            modified: 1700000000,
            created_at: 0,
            extension: ext.to_string(),
        };
        store.replace(vec![
            make("main.rs", "/src/main.rs", false, "rs"),
            make("main_test.rs", "/src/main_test.rs", false, "rs"),
            make("helper.py", "/src/helper.py", false, "py"),
            make("main_12.rs", "/src/main_12.rs", false, "rs"),
            make("main.tmp.rs", "/src/tmp/main.tmp.rs", false, "rs"),
            make("keep.pdf", "/my dir/keep.pdf", false, "pdf"),
            make("sub.rs", "/src/test/sub.rs", false, "rs"),
            make("pkg.js", "/node_modules/pkg/pkg.js", false, "js"),
            make(".gitignore", "/.gitignore", false, ""),
            make(".hidden_readme.txt", "/.hidden_readme.txt", false, "txt"),
            make("readme.txt", "/docs/readme.txt", false, "txt"),
            make(".a_note", "/docs/.a_note", false, ""),
            make("a_note_much_longer.txt", "/docs/a_note_much_longer.txt", false, "txt"),
        ]);

        // Pure negative queries must return empty
        assert!(dispatch_with_options(&store, "!test", SearchMode::Auto, CaseSensitivity::Auto, 10).0.is_empty());
        assert!(dispatch_with_options(&store, "!.pdf", SearchMode::Auto, CaseSensitivity::Auto, 10).0.is_empty());

        // type:code !.py and type:all !test work
        let (code_res, _, _) = dispatch_with_options(&store, "type:code !.py", SearchMode::Auto, CaseSensitivity::Auto, 20);
        let code_names: Vec<_> = code_res.iter().map(|r| r.entry.name.as_str()).collect();
        assert!(code_names.contains(&"main.rs"));
        assert!(!code_names.contains(&"helper.py"));

        let (all_res, _, _) = dispatch_with_options(&store, "type:all !test", SearchMode::Auto, CaseSensitivity::Auto, 20);
        let all_names: Vec<_> = all_res.iter().map(|r| r.entry.name.as_str()).collect();
        assert!(all_names.contains(&"main.rs"));
        assert!(!all_names.contains(&"main_test.rs"));
        assert!(!all_names.contains(&"sub.rs"));

        // Wildcard * !test and *.rs !test
        let (wc_all, _, _) = dispatch_with_options(&store, "* !test !node_modules/ !\"my dir/\"", SearchMode::Auto, CaseSensitivity::Auto, 20);
        let wc_names: Vec<_> = wc_all.iter().map(|r| r.entry.name.as_str()).collect();
        assert!(wc_names.contains(&"main.rs"));
        assert!(!wc_names.contains(&"main_test.rs"));
        assert!(!wc_names.contains(&"pkg.js"));
        assert!(!wc_names.contains(&"keep.pdf"));

        let (wc_rs, _, _) = dispatch_with_options(&store, "*.rs !test !src/test", SearchMode::Auto, CaseSensitivity::Auto, 20);
        let rs_names: Vec<_> = wc_rs.iter().map(|r| r.entry.name.as_str()).collect();
        assert!(rs_names.contains(&"main.rs"));
        assert!(!rs_names.contains(&"main_test.rs"));
        assert!(!rs_names.contains(&"sub.rs"));

        // Regex preserves escapes and honors !test
        let (rx_res, _, err) = dispatch_with_options(&store, r"regex:^main_\d+\.rs !test", SearchMode::Auto, CaseSensitivity::Auto, 10);
        assert!(err.is_none());
        assert_eq!(rx_res.iter().map(|r| r.entry.name.as_str()).collect::<Vec<_>>(), vec!["main_12.rs"]);

        let (rx_excl, _, _) = dispatch_with_options(&store, "regex:^main !test", SearchMode::Auto, CaseSensitivity::Auto, 10);
        assert!(!rx_excl.iter().any(|r| r.entry.name == "main_test.rs"));
        assert!(rx_excl.iter().any(|r| r.entry.name == "main.rs"));

        // Path search honors !tmp
        let (path_res, _, _) = dispatch_with_options(&store, "in:/src !tmp", SearchMode::Auto, CaseSensitivity::Auto, 20);
        assert!(path_res.iter().any(|r| r.entry.name == "main.rs"));
        assert!(!path_res.iter().any(|r| r.entry.name == "main.tmp.rs"));

        // Fuzzy honors !test
        let (fuzzy_res, _, _) = dispatch_with_options(&store, "mn !test", SearchMode::Fuzzy, CaseSensitivity::Auto, 10);
        assert!(fuzzy_res.iter().any(|r| r.entry.name == "main.rs"));
        assert!(!fuzzy_res.iter().any(|r| r.entry.name == "main_test.rs"));

        // !.hidden must NOT unpenalize dotfiles
        let (hidden_rank_res, _, _) = dispatch_with_options(&store, "readme !.hidden", SearchMode::Auto, CaseSensitivity::Auto, 10);
        assert_eq!(hidden_rank_res.iter().map(|r| r.entry.name.as_str()).collect::<Vec<_>>(), vec!["readme.txt"]);

        let (hidden_rank_res2, _, _) = dispatch_with_options(&store, "note !.dotfile", SearchMode::Auto, CaseSensitivity::Auto, 10);
        assert_eq!(hidden_rank_res2.len(), 2);
        assert_eq!(hidden_rank_res2[0].entry.name, "a_note_much_longer.txt");
    }

    #[test]
    fn test_dispatch_utf8_and_slash_path_self_exclusions() {
        let store = IndexStore::new();
        let make = |name: &str, path: &str, is_dir: bool, ext: &str| FileEntry {
            name: name.to_string(),
            path: path.to_string(),
            is_dir,
            size: 100,
            modified: 1700000000,
            created_at: 0,
            extension: ext.to_string(),
        };
        store.replace(vec![
            make("file.txt", "/a中文/file.txt", false, "txt"),
            make("file.txt", "/a/中文/file.txt", false, "txt"),
            make("test", "/proj/src/test", false, ""),
            make("test", "/proj/src/test", true, ""),
            make("sub.rs", "/proj/src/test/sub.rs", false, "rs"),
            make("test", "/proj/other/test", false, ""),
            make("test", "/test", true, ""),
            make("sub.rs", "/test/sub.rs", false, "rs"),
        ]);

        // Multi-byte UTF-8 relative path exclusion: partial miss /a中文/file.txt stays, exact /a/中文/file.txt excluded
        let (utf8_res1, _, _, _) = dispatch_with_filter(
            &store,
            "file !中文/",
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            20,
        );
        let utf8_paths1: Vec<_> = utf8_res1.iter().map(|r| r.entry.path.as_str()).collect();
        assert_eq!(utf8_paths1, vec!["/a中文/file.txt"]);

        let (utf8_res2, _, _, _) = dispatch_with_filter(
            &store,
            "file !a/中文",
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            20,
        );
        let utf8_paths2: Vec<_> = utf8_res2.iter().map(|r| r.entry.path.as_str()).collect();
        assert_eq!(utf8_paths2, vec!["/a中文/file.txt"]);

        // Self-exclusion of both regular file & directory /proj/src/test under !src/test and !/proj/src/test
        let (rel_self, _, _, _) = dispatch_with_filter(
            &store,
            "* !src/test",
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            20,
        );
        let rel_paths: Vec<_> = rel_self.iter().map(|r| r.entry.path.as_str()).collect();
        assert!(!rel_paths.contains(&"/proj/src/test"));
        assert!(!rel_paths.contains(&"/proj/src/test/sub.rs"));
        assert!(rel_paths.contains(&"/proj/other/test"));
        assert!(rel_paths.contains(&"/test"));

        let (abs_self, _, _, _) = dispatch_with_filter(
            &store,
            "test !/proj/src/test",
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            20,
        );
        let abs_paths: Vec<_> = abs_self.iter().map(|r| r.entry.path.as_str()).collect();
        assert!(!abs_paths.contains(&"/proj/src/test"));
        assert!(abs_paths.contains(&"/proj/other/test"));
        assert!(abs_paths.contains(&"/test"));

        // Root child exclusion !/test
        let (root_self, _, _, _) = dispatch_with_filter(
            &store,
            "* !/test",
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            20,
        );
        let root_paths: Vec<_> = root_self.iter().map(|r| r.entry.path.as_str()).collect();
        assert!(!root_paths.contains(&"/test"));
        assert!(!root_paths.contains(&"/test/sub.rs"));
        assert!(root_paths.contains(&"/proj/src/test"));
        assert!(root_paths.contains(&"/proj/other/test"));
    }
}
