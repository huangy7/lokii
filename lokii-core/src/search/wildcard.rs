use glob_match::glob_match;

use super::filter::QueryToken;
use super::ranking::{BaseNameScore, MatchTier, RecordRankContext};
use super::{AdvancedSearchResult, CompiledRecordFilter, SearchMode};
use crate::compact_index::CompactIndex;
use crate::indexer::IndexStore;
use crate::string_pool::NameSignature;

#[inline]
pub(crate) fn has_glob_meta(s: &str) -> bool {
    s.contains(['*', '?', '[', '{'])
}

/// 从通配符模式中提取所有不跨越 `*`/`?`/`[`/`]`/`{`/`}` 的字面量片段并生成签名超集掩码。
/// 进入提取前先执行 Unicode NFC 与繁简/大小写折叠，避免 NFD 字符先入签名造成假阴性剪枝。
pub(crate) fn required_signature_for_glob(pattern: &str) -> NameSignature {
    let folded = crate::cjk::fold_cow(pattern);
    if folded.contains('[') || folded.contains('{') || folded.contains('\\') {
        return NameSignature::default();
    }
    let mut sig = NameSignature::default();
    for segment in folded.split(['*', '?']) {
        if !segment.is_empty() {
            sig = sig.union(NameSignature::from_substring_query(segment));
        }
    }
    sig
}

/// 评估已折叠/规范化的单个文件名是否满足多词通配符 AND 交集条件。
///
/// 规则：
/// - 单词元或全部词元均不含通配元字符时，沿用整体 `glob_match` 全名匹配；
/// - 多词元且至少一词含通配元字符（如 `report *2026*.pdf`）时，含通配符的词元执行 `glob_match`，
///   普通词元执行子串包含（AND 交集），避免空格被误作必含字面量。
pub(crate) fn matches_wildcard_terms(
    name: &str,
    folded: Option<&str>,
    patterns: &[String],
    multi_and_mode: bool,
    simple_suffix: Option<&str>,
    case_sensitive: bool,
    ascii_buf: &mut Vec<u8>,
) -> bool {
    if !multi_and_mode {
        let pattern = match patterns.first() {
            Some(p) => p.as_str(),
            None => return false,
        };
        if case_sensitive {
            let name_norm = crate::cjk::normalize_and_simplify_cow(name);
            return if let Some(ext) = simple_suffix {
                let suffix_len = ext.len() + 1;
                name_norm.len() >= suffix_len
                    && name_norm.as_bytes()[name_norm.len() - suffix_len] == b'.'
                    && &name_norm[name_norm.len() - ext.len()..] == ext
            } else {
                glob_match(pattern, &name_norm)
            };
        }
        if let Some(ext) = simple_suffix {
            let suffix_len = ext.len() + 1;
            return if let Some(f) = folded {
                f.len() >= suffix_len
                    && f.as_bytes()[f.len() - suffix_len] == b'.'
                    && &f[f.len() - ext.len()..] == ext
            } else {
                let bytes = name.as_bytes();
                bytes.len() >= suffix_len
                    && bytes[bytes.len() - suffix_len] == b'.'
                    && bytes[bytes.len() - ext.len()..].eq_ignore_ascii_case(ext.as_bytes())
            };
        }
        return if let Some(f) = folded {
            glob_match(pattern, f)
        } else if !name.bytes().any(|b| b.is_ascii_uppercase()) {
            glob_match(pattern, name)
        } else {
            ascii_buf.clear();
            ascii_buf.extend(name.bytes().map(|b| b.to_ascii_lowercase()));
            let lower_str = std::str::from_utf8(ascii_buf).unwrap_or(name);
            glob_match(pattern, lower_str)
        };
    }

    let target: std::borrow::Cow<'_, str> = if case_sensitive {
        crate::cjk::normalize_and_simplify_cow(name)
    } else if let Some(f) = folded {
        std::borrow::Cow::Borrowed(f)
    } else if !name.bytes().any(|b| b.is_ascii_uppercase()) {
        std::borrow::Cow::Borrowed(name)
    } else {
        ascii_buf.clear();
        ascii_buf.extend(name.bytes().map(|b| b.to_ascii_lowercase()));
        std::borrow::Cow::Borrowed(std::str::from_utf8(ascii_buf).unwrap_or(name))
    };
    let target_str = target.as_ref();
    patterns.iter().all(|pat| {
        if has_glob_meta(pat) {
            glob_match(pat, target_str)
        } else {
            target_str.contains(pat.as_str())
        }
    })
}

/// 精准提取通配符查询命中的非通配字面片段，并投影为原文件名上的安全 UTF-8 高亮字节区间。
/// 若模式不含任何固定字面片段（如 `???` 或纯字符类），安全回退高亮全文件名。
pub(crate) fn find_wildcard_positions(
    orig_name: &str,
    patterns: &[String],
    case_sensitive: bool,
) -> Vec<(usize, usize)> {
    if orig_name.is_empty() || patterns.is_empty() {
        return Vec::new();
    }
    let norm_cow: std::borrow::Cow<'_, str> = if case_sensitive {
        crate::cjk::normalize_and_simplify_cow(orig_name)
    } else {
        crate::cjk::fold_cow(orig_name)
    };
    let norm = norm_cow.as_ref();
    let mut spans = Vec::new();

    for pat in patterns {
        if pat.is_empty() {
            continue;
        }
        if !has_glob_meta(pat) {
            if let Some(pos) = norm.find(pat.as_str()) {
                if let Some(orig_span) =
                    super::map_norm_span_to_orig(orig_name, norm, pos, pos + pat.len())
                {
                    spans.push(orig_span);
                }
            }
            continue;
        }
        if pat.contains('[') || pat.contains('{') || pat.contains('\\') {
            continue;
        }

        let leading_wild = pat.starts_with(['*', '?']);
        let trailing_wild = pat.ends_with(['*', '?']);
        let segments: Vec<&str> = pat.split(['*', '?']).filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            continue;
        }

        let mut cursor = 0usize;
        let last_idx = segments.len() - 1;
        for (idx, &seg) in segments.iter().enumerate() {
            let found = if idx == 0 && !leading_wild && norm.starts_with(seg) {
                Some(0)
            } else if idx == last_idx && !trailing_wild && norm[cursor..].ends_with(seg) {
                Some(norm.len() - seg.len())
            } else {
                norm[cursor..].find(seg).map(|rel| cursor + rel)
            };
            if let Some(start) = found {
                let end = start + seg.len();
                if let Some(orig_span) = super::map_norm_span_to_orig(orig_name, norm, start, end)
                {
                    spans.push(orig_span);
                }
                cursor = end;
            }
        }
    }

    if spans.is_empty() {
        return vec![(0, orig_name.len())];
    }
    super::disjunction::merge_highlight_spans(spans, orig_name.len())
}

/// 通配符检索器，接入预编译位掩码记录过滤器与双层复合排序模型。
pub fn search(
    store: &IndexStore,
    query: &str,
    filter: &CompiledRecordFilter,
    max_results: usize,
) -> Vec<AdvancedSearchResult> {
    let read = store.read();
    search_index(&read, query, filter, max_results, false)
}

/// 接收统一快照读锁的通配符检索实现，杜绝锁重入与快照位掩码漂移。
pub fn search_index(
    read: &CompactIndex,
    query: &str,
    filter: &CompiledRecordFilter,
    max_results: usize,
    case_sensitive: bool,
) -> Vec<AdvancedSearchResult> {
    let raw_pattern = query.trim();
    if raw_pattern.is_empty() || max_results == 0 {
        return Vec::new();
    }
    let parts: Vec<&str> = raw_pattern.split_whitespace().collect();
    if parts.len() > 1 && parts.iter().any(|p| has_glob_meta(p)) {
        let tokens: Vec<QueryToken> = parts
            .into_iter()
            .map(|s| QueryToken {
                text: s.to_string(),
                quoted: false,
                negated: false,
                raw: s.to_string(),
                had_escaped_meta: false,
                was_sanitized_pasted_path: false,
            })
            .collect();
        search_index_tokens(read, &tokens, filter, max_results, case_sensitive)
    } else {
        let token = QueryToken {
            text: raw_pattern.to_string(),
            quoted: false,
            negated: false,
            raw: raw_pattern.to_string(),
            had_escaped_meta: false,
            was_sanitized_pasted_path: false,
        };
        search_index_tokens(
            read,
            std::slice::from_ref(&token),
            filter,
            max_results,
            case_sensitive,
        )
    }
}

/// 基于结构化正向词元的通配符检索实现：
/// - `"*"` 走修改时间倒序全局 Top-K 并前置堆顶阈值短路；
/// - 非 `"*"` 接入 `rank_unique_names` 双层复合排序（路径深度、隐藏降权、目录与长度权重）并支持多词 AND 交集与字面片段高亮。
pub(crate) fn search_index_tokens(
    read: &CompactIndex,
    positive_tokens: &[QueryToken],
    filter: &CompiledRecordFilter,
    max_results: usize,
    case_sensitive: bool,
) -> Vec<AdvancedSearchResult> {
    if positive_tokens.is_empty() || max_results == 0 {
        return Vec::new();
    }

    let active_tokens: Vec<&QueryToken> = positive_tokens
        .iter()
        .filter(|t| !t.negated && !t.text.trim().is_empty())
        .collect();
    if active_tokens.is_empty() {
        return Vec::new();
    }

    // 单个未加引号 "*"：全局遍历所有记录并按修改时间倒序保留 Top-K，前置堆顶阈值短路规避百万次视图构造
    if active_tokens.len() == 1 && !active_tokens[0].quoted && active_tokens[0].text.trim() == "*" {
        let mut top = super::top_k::TopK::new(max_results);
        let passthrough = filter.is_passthrough();
        let records = read.records();
        for (ordinal, record) in records.iter().enumerate() {
            if let Some(worst) = top.worst() {
                if record.modified < worst.score {
                    continue;
                }
            }
            if !passthrough && !filter.matches_record_in(read, record, None) {
                continue;
            }
            if let Some(view) = read.view(crate::compact_index::EntryId(ordinal)) {
                top.offer_path(ordinal, record.modified, view);
            }
        }
        return top
            .finish()
            .into_iter()
            .map(|candidate| {
                let view = read
                    .view(crate::compact_index::EntryId(candidate.ordinal))
                    .unwrap();
                AdvancedSearchResult {
                    entry: view.materialize(),
                    score: 1.0,
                    match_positions: Vec::new(),
                    parent_match_positions: Vec::new(),
                    search_mode: SearchMode::Wildcard,
                }
            })
            .collect();
    }

    let has_any_glob = active_tokens
        .iter()
        .any(|t| !t.quoted && has_glob_meta(t.text.trim()));
    let multi_and_mode = active_tokens.len() > 1 && has_any_glob;

    let patterns: Vec<String> = if multi_and_mode {
        active_tokens
            .iter()
            .map(|t| {
                let raw = t.text.trim();
                if case_sensitive {
                    crate::cjk::normalize_and_simplify_cow(raw).into_owned()
                } else {
                    crate::cjk::fold_str(raw)
                }
            })
            .collect()
    } else {
        let joined = active_tokens
            .iter()
            .map(|t| t.text.trim())
            .collect::<Vec<_>>()
            .join(" ");
        vec![if case_sensitive {
            crate::cjk::normalize_and_simplify_cow(&joined).into_owned()
        } else {
            crate::cjk::fold_str(&joined)
        }]
    };

    let req = patterns.iter().fold(NameSignature::default(), |sig, pat| {
        sig.union(if multi_and_mode && !has_glob_meta(pat) {
            NameSignature::from_substring_query(pat)
        } else {
            required_signature_for_glob(pat)
        })
    });

    // 针对单一纯 `*.ext` 模式（无其他通配符），跳过 `glob_match` 字符串自动机直接做后缀字节比对
    let simple_suffix = if !multi_and_mode {
        patterns[0].strip_prefix("*.").filter(|rest| {
            !rest.is_empty()
                && !rest.contains(['*', '?', '[', ']', '{', '}', '\\'])
                && rest.is_ascii()
        })
    } else {
        None
    };

    let targets_hidden = active_tokens.iter().any(|t| t.text.trim().starts_with('.'));
    let rank_ctx = RecordRankContext::default();
    let mut ascii_buf = Vec::with_capacity(128);

    let candidates = super::rank_unique_names(
        read,
        filter,
        max_results,
        req,
        targets_hidden,
        &rank_ctx,
        |name, folded| {
            if matches_wildcard_terms(
                name,
                folded,
                &patterns,
                multi_and_mode,
                simple_suffix,
                case_sensitive,
                &mut ascii_buf,
            ) {
                Some(BaseNameScore::for_tier_only(name, MatchTier::Prefix, 0))
            } else {
                None
            }
        },
    );

    candidates
        .into_iter()
        .map(|candidate| {
            let view = read
                .view(crate::compact_index::EntryId(candidate.ordinal))
                .unwrap();
            let match_positions = find_wildcard_positions(view.name, &patterns, case_sensitive);
            AdvancedSearchResult {
                entry: view.materialize(),
                score: candidate.score.to_f64(),
                match_positions,
                parent_match_positions: Vec::new(),
                search_mode: SearchMode::Wildcard,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::{FileEntry, IndexStore};

    fn make_entry(name: &str, path: &str) -> FileEntry {
        let extension = std::path::Path::new(name)
            .extension()
            .map(|ext| ext.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        FileEntry {
            name: name.to_string(),
            path: path.to_string(),
            is_dir: false,
            size: 1024,
            modified: 1700000000,
            created_at: 0,
            extension,
        }
    }

    fn test_store() -> IndexStore {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("report.pdf", "/report.pdf"),
            make_entry("report.txt", "/report.txt"),
            make_entry("test_01.rs", "/test_01.rs"),
            make_entry("test_1.rs", "/test_1.rs"),
            make_entry("my_main.rs", "/my_main.rs"),
            make_entry("main.rs", "/main.rs"),
        ]);
        store
    }

    #[test]
    fn test_star_pdf_matches_pdf_not_txt() {
        let store = test_store();
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "*.pdf", &filter, 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "report.pdf");
    }

    #[test]
    fn test_question_mark_two_chars() {
        let store = test_store();
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "test_??.rs", &filter, 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "test_01.rs");
    }

    #[test]
    fn test_case_insensitive() {
        let store = test_store();
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "*.PDF", &filter, 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "report.pdf");
    }

    #[test]
    fn test_star_main_star() {
        let store = test_store();
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "*main*", &filter, 500);
        assert_eq!(results.len(), 2);
        // Alphabetical: main.rs, my_main.rs
        assert_eq!(results[0].entry.name, "main.rs");
        assert_eq!(results[1].entry.name, "my_main.rs");
    }

    #[test]
    fn test_match_positions_literal_segments() {
        let store = test_store();
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "*.pdf", &filter, 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].match_positions, vec![(6, "report.pdf".len())]);

        let qmark = search(&store, "test_??.rs", &filter, 500);
        assert_eq!(qmark.len(), 1);
        assert_eq!(qmark[0].match_positions, vec![(0, 5), (7, 10)]);
    }

    #[test]
    fn test_results_sorted_alphabetically() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("c_file.rs", "/c_file.rs"),
            make_entry("a_file.rs", "/a_file.rs"),
            make_entry("b_file.rs", "/b_file.rs"),
        ]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "*.rs", &filter, 500);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].entry.name, "a_file.rs");
        assert_eq!(results[1].entry.name, "b_file.rs");
        assert_eq!(results[2].entry.name, "c_file.rs");
    }

    #[test]
    fn test_empty_query_returns_empty() {
        let store = test_store();
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "", &filter, 500);
        assert!(results.is_empty());
    }

    #[test]
    fn test_whitespace_query_returns_empty() {
        let store = test_store();
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "   ", &filter, 500);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_mode_is_wildcard() {
        let store = test_store();
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "*.rs", &filter, 500);
        assert!(!results.is_empty());
        for r in &results {
            assert_eq!(r.search_mode, SearchMode::Wildcard);
        }
    }

    #[test]
    fn test_max_results_truncation() {
        let store = test_store();
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "*.rs", &filter, 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_wildcard_with_compiled_filter() {
        let store = test_store();
        let read = store.read();
        let query_filter = super::super::QueryFilter {
            file_type: super::super::FileType::Code,
            custom_ext: None,
            ..Default::default()
        };
        let filter = CompiledRecordFilter::compile(&read, &query_filter);
        drop(read);

        // "report.*" 匹配 report.pdf 和 report.txt，但在 Code 过滤下应全被排除
        let results = search(&store, "report.*", &filter, 500);
        assert!(results.is_empty());

        // "*test*" 在 Code 过滤下命中 test_01.rs 与 test_1.rs
        let results_code = search(&store, "*test*", &filter, 500);
        assert_eq!(results_code.len(), 2);
    }

    #[test]
    fn test_wildcard_star_early_pruning_preserves_exact_top_k_ties() {
        let mut entries = Vec::new();
        for i in 0..100u64 {
            entries.push(FileEntry {
                name: format!("old_{i:03}.txt"),
                path: format!("/old/old_{i:03}.txt"),
                is_dir: false,
                size: 100,
                modified: 1_000 + i,
                created_at: 0,
                extension: "txt".to_string(),
            });
        }
        // 两条相同最新时间戳，核验阈值短路不误杀平分字典序胜者
        entries.push(FileEntry {
            name: "z_latest.txt".to_string(),
            path: "/z_latest.txt".to_string(),
            is_dir: false,
            size: 100,
            modified: 9_999,
            created_at: 0,
            extension: "txt".to_string(),
        });
        entries.push(FileEntry {
            name: "a_latest.txt".to_string(),
            path: "/a_latest.txt".to_string(),
            is_dir: false,
            size: 100,
            modified: 9_999,
            created_at: 0,
            extension: "txt".to_string(),
        });
        entries.push(FileEntry {
            name: "mid_recent.txt".to_string(),
            path: "/mid_recent.txt".to_string(),
            is_dir: false,
            size: 100,
            modified: 5_000,
            created_at: 0,
            extension: "txt".to_string(),
        });

        let store = IndexStore::new();
        store.replace(entries);
        let filter = CompiledRecordFilter::default();

        let top2 = search(&store, "*", &filter, 2);
        let top2_names: Vec<_> = top2.iter().map(|r| r.entry.name.as_str()).collect();
        assert_eq!(top2_names, vec!["a_latest.txt", "z_latest.txt"]);

        let top3 = search(&store, "*", &filter, 3);
        let top3_names: Vec<_> = top3.iter().map(|r| r.entry.name.as_str()).collect();
        assert_eq!(top3_names, vec!["a_latest.txt", "z_latest.txt", "mid_recent.txt"]);
    }

    #[test]
    fn test_multi_token_wildcard_and_literal_highlights() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("my_report_2026_final.pdf", "/docs/my_report_2026_final.pdf"),
            make_entry("report_2025_final.pdf", "/docs/report_2025_final.pdf"),
            make_entry("summary_2026.pdf", "/docs/summary_2026.pdf"),
        ]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "report *2026*.pdf", &filter, 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "my_report_2026_final.pdf");
        // 高亮精准落于 "report"(3..9), "2026"(10..14), ".pdf"(20..24)
        assert_eq!(results[0].match_positions, vec![(3, 9), (10, 14), (20, 24)]);
    }
}
