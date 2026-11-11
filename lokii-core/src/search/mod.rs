pub mod disjunction;
pub mod filter;
pub mod fuzzy;
pub mod path_query;
pub mod path_search;
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
    is_dotfile_query, parse_query_filter, CompiledRecordFilter, FileType, QueryFilter, SizeFilter,
};
pub use regex_sugar::{is_regex_metachar, parse_regex_sugar, RegexSugar};
pub use substring::search_disjunction;

use crate::compact_index::CompactIndex;
use crate::indexer::{FileEntry, IndexStore};
use crate::string_pool::NameSignature;
use serde::{Deserialize, Serialize};

use top_k::{Candidate, TopK};

/// 在原始字符串 `name` 中查找是否有一段子串经繁简与小写归一化后与 `keyword` 相同。
/// 若匹配，返回该子串在 `name` 中的精确原始 UTF-8 字节区间 `(start, end)`。
fn find_cjk_folded_match(name: &str, keyword: &str) -> Option<(usize, usize)> {
    if keyword.is_empty() || name.is_empty() || (name.is_ascii() && keyword.is_ascii()) {
        return None;
    }
    let kw_chars: Vec<char> = keyword.chars().map(crate::cjk::fold_char).collect();
    let name_chars: Vec<(usize, char)> = name.char_indices().collect();
    if kw_chars.len() > name_chars.len() {
        return None;
    }

    for i in 0..=(name_chars.len() - kw_chars.len()) {
        let mut matched = true;
        for (j, &kw_c) in kw_chars.iter().enumerate() {
            let name_c = name_chars[i + j].1;
            let folded_c = crate::cjk::fold_char(name_c);
            if folded_c != kw_c {
                matched = false;
                break;
            }
        }
        if matched {
            let start = name_chars[i].0;
            let last_idx = i + kw_chars.len() - 1;
            let end = name_chars[last_idx].0 + name_chars[last_idx].1.len_utf8();
            return Some((start, end));
        }
    }
    None
}

/// 计算各关键词在 `name` 或拼音转写中的字节区间，供前端高亮。
/// 支持混合查询（如部分关键词直接命中，另一部分关键词通过拼音匹配命中）。
/// 接收提前映射好的 `(raw_term, lower_term)` 关键词对，避免循环内部重复小写堆分配。
pub(crate) fn find_positions<'a>(
    name: &str,
    name_lower: &str,
    keywords: &[(&'a str, &'a str)],
) -> Vec<(usize, usize)> {
    let mut positions = Vec::new();
    let mut unmatched_keywords = Vec::new();

    for &(keyword, keyword_lower) in keywords {
        if keyword.is_empty() {
            continue;
        }
        if let Some(start) = name.find(keyword) {
            positions.push((start, start + keyword.len()));
        } else if let Some(range) = find_cjk_folded_match(name, keyword) {
            positions.push(range);
        } else if let Some(start) = name_lower.find(keyword_lower) {
            let lower_start = start;
            let lower_end = start + keyword_lower.len();
            if let Some(pos) = map_lower_span_to_orig(name, lower_start, lower_end) {
                positions.push(pos);
            }
        } else {
            unmatched_keywords.push(keyword);
        }
    }

    if !unmatched_keywords.is_empty() && name.as_bytes().iter().any(|&b| b >= 0x80) {
        let pinyin_pos = crate::pinyin::find_pinyin_positions(name, &unmatched_keywords);
        positions.extend(pinyin_pos);
    }

    positions.sort_by_key(|&(s, e)| (s, e));
    positions.dedup();
    positions
}

/// 将小写字符串中的字节匹配区间安全映射回原字符串的字节区间。
/// 快速路径：原串与小写串字节长度相等且字符边界对齐时直接复用区间；
/// 慢速路径：逐字符对齐，严格确保字符边界与字节长度安全。
fn map_lower_span_to_orig(name: &str, lower_start: usize, lower_end: usize) -> Option<(usize, usize)> {
    if lower_end <= name.len() && name.is_char_boundary(lower_start) && name.is_char_boundary(lower_end) {
        return Some((lower_start, lower_end));
    }
    let mut orig_byte = 0;
    let mut lower_byte = 0;
    let mut matched_start = None;
    let mut matched_end = None;

    for c in name.chars() {
        if lower_byte == lower_start {
            matched_start = Some(orig_byte);
        }
        let lower_c_len = c.to_lowercase().fold(0, |acc, lc| acc + lc.len_utf8());
        lower_byte += lower_c_len;
        orig_byte += c.len_utf8();
        if lower_byte == lower_end {
            matched_end = Some(orig_byte);
            break;
        }
    }
    match (matched_start, matched_end) {
        (Some(s), Some(e)) if s < e && e <= name.len() => Some((s, e)),
        _ => None,
    }
}

/// 在按名字字节序排好序的唯一名表上使用 128 位签名超集预筛后对每个唯一名只计算一次 `score`，
/// 并将命中的唯一名通过预建的 `name_entries` CSR 反表展开为 Top-K 记录。
pub(crate) fn rank_unique_names<'a, K: Ord + Copy>(
    index: &'a CompactIndex,
    filter: &CompiledRecordFilter,
    max_results: usize,
    required_sig: NameSignature,
    mut score: impl FnMut(&'a str, Option<&'a str>) -> Option<K>,
) -> Vec<Candidate<'a, K>> {
    let mut top = TopK::new(max_results);
    if max_results == 0 {
        return Vec::new();
    }
    let query_index = index.query_index();
    let passthrough = filter.is_passthrough();
    for name_id in 0..query_index.slot_count() as u32 {
        let sig = query_index.signature_at(name_id);
        if sig == crate::string_pool::NameSignature::default() && !query_index.is_ready(name_id) {
            continue;
        }
        if !sig.contains(required_sig) {
            continue;
        }
        let Some(name) = index.name_text(name_id) else {
            continue;
        };
        if let Some(s) = score(name, query_index.folded(name_id)) {
            let entries = query_index.entries(name_id);
            if passthrough {
                for &ordinal in entries.iter().take(max_results) {
                    if (ordinal as usize) < index.records().len() {
                        top.offer(ordinal as usize, s, name);
                    }
                }
            } else {
                for &ordinal in entries.iter() {
                    let Some(record) = index.records().get(ordinal as usize) else {
                        continue;
                    };
                    if !filter.matches_record(record, || Some(name)) {
                        continue;
                    }
                    top.offer(ordinal as usize, s, name);
                }
            }
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

/// 提取参与 Smart Case 判定的有效文件名词元。
///
/// 过滤 `in:` 作用域语法与路径中的父目录前缀，避免路径修饰词的大写误触发文件名敏感匹配。
pub fn extract_content_terms_for_smart_case<'a>(
    clean_query: &'a str,
    search_mode: SearchMode,
) -> Vec<&'a str> {
    if search_mode == SearchMode::Regex {
        let raw = clean_query.strip_prefix("regex:").unwrap_or(clean_query);
        return vec![raw];
    }

    let mut terms = Vec::new();
    for word in clean_query.split_whitespace() {
        if word.starts_with("in:") {
            continue;
        }
        if word.contains('/') {
            if word.ends_with('/') {
                continue;
            }
            if let Some((_, name_part)) = word.rsplit_once('/') {
                if !name_part.is_empty() {
                    terms.push(name_part);
                }
            }
            continue;
        }
        if word.starts_with('.') && !crate::search::filter::is_dotfile_query(word) {
            continue;
        }
        terms.push(word);
    }
    terms
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
            terms.into_iter().any(|term| has_effective_uppercase(term, is_regex))
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

/// Detect search mode from query string.
/// - Starts with `regex:` or matches regex sugar `/<pattern>/` -> Regex
/// - Contains unescaped `|` -> Auto (or disjunction)
/// - Contains `*` or `?` -> Wildcard
/// - Otherwise -> Auto
pub fn detect_mode(query: &str) -> SearchMode {
    let trimmed = query.trim();
    if trimmed.starts_with("regex:") || parse_regex_sugar(trimmed).is_some() {
        SearchMode::Regex
    } else if has_unescaped_pipe(trimmed) {
        SearchMode::Auto
    } else if trimmed.contains('*') || trimmed.contains('?') {
        SearchMode::Wildcard
    } else {
        SearchMode::Auto
    }
}

/// 携带过滤条件的通用检索分发入口。
///
/// 优先级机制：若查询词中包含显式 `type:` / `ext:` 等内联语法，
/// 提取后的内联过滤规则优先级高于入参中的 `filter`；若查询词未显式指定，则继承入参中的 `filter`。
/// 随后根据目标索引编译零堆分配的位掩码过滤器 `CompiledRecordFilter`，
/// 并按检索模式派发至对应的底层子检索器。
pub fn dispatch_with_filter(
    store: &IndexStore,
    query: &str,
    mode: SearchMode,
    filter: QueryFilter,
    case_sensitivity: CaseSensitivity,
    max_results: usize,
) -> (Vec<AdvancedSearchResult>, bool, Option<String>, SearchMode) {
    let (clean_query, inline_filter) = parse_query_filter(query);

    let has_explicit_type_all = query.split_whitespace().any(|t| {
        t.len() >= 5
            && t.is_char_boundary(5)
            && t[..5].eq_ignore_ascii_case("type:")
            && t[5..].split(',').any(|p| p.trim().eq_ignore_ascii_case("all"))
    });

    let effective_file_type = if inline_filter.file_type != FileType::All || has_explicit_type_all {
        inline_filter.file_type
    } else {
        filter.file_type
    };

    let effective_custom_ext = inline_filter.custom_ext.or(filter.custom_ext);

    let effective_file_types = if !inline_filter.file_types.is_empty() || has_explicit_type_all {
        inline_filter.file_types
    } else if !filter.file_types.is_empty() {
        filter.file_types
    } else if effective_file_type != FileType::All {
        vec![effective_file_type]
    } else {
        Vec::new()
    };

    let effective_custom_exts = if !inline_filter.custom_exts.is_empty() {
        inline_filter.custom_exts
    } else if !filter.custom_exts.is_empty() {
        filter.custom_exts
    } else if let Some(ext) = &effective_custom_ext {
        vec![ext.clone()]
    } else {
        Vec::new()
    };

    let effective_size_filter = if !inline_filter.size_filter.is_unconstrained() {
        inline_filter.size_filter
    } else {
        filter.size_filter
    };

    let read = store.read();

    let mut clean_query = clean_query;
    let mut effective_custom_exts = effective_custom_exts;
    let mut effective_custom_ext = effective_custom_ext;

    // 针对多词元中隐式推断的独立 `.ext` 词元，校验只读索引库中是否存在该扩展名：
    // 若当前索引中查无此扩展名，说明该词元并非合法扩展名修饰符，而是用户输入的中间态词条或未知点文件前缀；
    // 此时将该候选扩展名从过滤器中移除，并将原词元归还至 clean_query，避免误将搜索结果归零。
    if !inline_filter.inferred_exts.is_empty() {
        for (inferred_ext, raw_token) in &inline_filter.inferred_exts {
            if read.extension_id(inferred_ext).is_none() {
                effective_custom_exts.retain(|e| e != inferred_ext);
                if effective_custom_ext.as_deref() == Some(inferred_ext.as_str()) {
                    effective_custom_ext = effective_custom_exts.first().cloned();
                }
                if clean_query.is_empty() {
                    clean_query = raw_token.clone();
                } else {
                    clean_query.push(' ');
                    clean_query.push_str(raw_token);
                }
            }
        }
    }

    let effective_filter = QueryFilter {
        file_type: effective_file_type,
        custom_ext: effective_custom_ext,
        file_types: effective_file_types,
        custom_exts: effective_custom_exts,
        size_filter: effective_size_filter,
        has_explicit_type_all: has_explicit_type_all
            || inline_filter.has_explicit_type_all
            || filter.has_explicit_type_all,
        inferred_exts: inline_filter.inferred_exts,
    };

    // 识别行内正则语法糖（仅在 Auto 模式下推导，Locked 模式下用户输入直接作为纯正则模式处理）
    let regex_sugar = if mode == SearchMode::Auto {
        parse_regex_sugar(&clean_query)
    } else {
        None
    };

    // 权威裁决实际生效模式：显式 Regex 锁定或语法糖命中均派发至 Regex 模式；显式指定非 Auto 模式时保持原模式
    let effective_mode = match mode {
        SearchMode::Auto => {
            if regex_sugar.is_some() || clean_query.starts_with("regex:") {
                SearchMode::Regex
            } else {
                detect_mode(&clean_query)
            }
        }
        other => other,
    };

    // 提取参与底层编译的纯模式文本与三级大小写仲裁策略：
    // 第 1 级（最高）：行内语法糖显式标志（如 /pattern/i 强制不敏感）
    // 第 2 级：全局显式锁定（Sensitive / Insensitive）
    // 第 3 级：Smart Case 启发式自适应推导
    let (pattern_to_run, is_case_sensitive) = if effective_mode == SearchMode::Regex {
        if let Some(sugar) = regex_sugar {
            let case_sens = if sugar.case_insensitive {
                false
            } else {
                resolve_case_sensitive(&sugar.pattern, case_sensitivity, SearchMode::Regex)
            };
            (Some(sugar.pattern), case_sens)
        } else {
            let raw = clean_query.strip_prefix("regex:").unwrap_or(&clean_query);
            let case_sens = resolve_case_sensitive(raw, case_sensitivity, SearchMode::Regex);
            (Some(raw.to_string()), case_sens)
        }
    } else {
        let case_sens = resolve_case_sensitive(&clean_query, case_sensitivity, effective_mode);
        (None, case_sens)
    };

    let explicit_lock = case_sensitivity == CaseSensitivity::Sensitive;

    let compiled_filter = CompiledRecordFilter::compile(&read, &effective_filter);

    // 并集检索（OR 查询）路由：
    // 仅在智能模式（Auto）下将未转义管道符解析为并集子句；
    // 纯字面量模式（Substring）下管道符按原始字面字符匹配。
    if effective_mode == SearchMode::Auto
        && !clean_query.starts_with("regex:")
        && has_unescaped_pipe(&clean_query)
    {
        let global_case = match case_sensitivity {
            CaseSensitivity::Auto => None,
            CaseSensitivity::Sensitive => Some(true),
            CaseSensitivity::Insensitive => Some(false),
        };
        let disjunction_query = DisjunctionQuery::parse(&clean_query, global_case);
        if disjunction_query.is_empty() {
            return (Vec::new(), false, None, effective_mode);
        }
        let is_case_sensitive = match case_sensitivity {
            CaseSensitivity::Sensitive => true,
            CaseSensitivity::Insensitive => false,
            CaseSensitivity::Auto => disjunction_query.clauses.iter().any(|c| c.is_case_sensitive),
        };
        let results = substring::search_disjunction(
            &read,
            &disjunction_query,
            &compiled_filter,
            max_results,
            explicit_lock,
        );
        return (results, is_case_sensitive, None, effective_mode);
    }

    // 含路径语法（'/' 或 'in:'）时交由路径检索器。
    if effective_mode != SearchMode::Regex {
        if let Some(path_query) = path_query::PathQuery::parse_with_case(&clean_query, is_case_sensitive) {
            let results = path_search::search_index(
                &read,
                &path_query,
                &compiled_filter,
                max_results,
                is_case_sensitive,
                explicit_lock,
            );
            return (results, is_case_sensitive, None, effective_mode);
        }
    }

    // For Auto that detected as Auto (no special chars), run substring search.
    // 保持高确定性与精准度：普通文本输入严格按连续子串匹配，搜不到即返回空列表，避免无脑降级为模糊跳字匹配。
    let (results, error) = match effective_mode {
        SearchMode::Auto | SearchMode::Substring => {
            let results = substring::search_index(
                &read,
                &clean_query,
                &compiled_filter,
                max_results,
                is_case_sensitive,
                explicit_lock,
            );
            (results, None)
        }
        SearchMode::Wildcard => {
            let results = wildcard::search_index(
                &read,
                &clean_query,
                &compiled_filter,
                max_results,
                is_case_sensitive,
            );
            (results, None)
        }
        SearchMode::Regex => {
            let pattern = pattern_to_run.as_deref().unwrap_or(&clean_query);
            regex_search::search_index(
                &read,
                pattern,
                &compiled_filter,
                max_results,
                is_case_sensitive,
            )
        }
        SearchMode::Fuzzy => {
            // 模糊模式是 Auto 的放宽：子串/拼音命中在前，跳字命中去重后补足剩余名额
            let mut results = substring::search_index(
                &read,
                &clean_query,
                &compiled_filter,
                max_results,
                is_case_sensitive,
                explicit_lock,
            );
            let remaining = max_results.saturating_sub(results.len());
            if remaining == 0 {
                return (results, is_case_sensitive, None, effective_mode);
            }
            let seen: std::collections::HashSet<String> =
                results.iter().map(|r| r.entry.path.clone()).collect();
            results.extend(
                fuzzy::search_index(
                    &read,
                    &clean_query,
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
}
