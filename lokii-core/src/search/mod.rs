pub mod filter;
pub mod fuzzy;
pub mod path_query;
pub mod path_search;
pub mod regex_search;
pub mod substring;
pub mod top_k;
pub mod wildcard;

pub use filter::{
    parse_query_filter, CompiledRecordFilter, FileType, QueryFilter, SizeFilter,
};

use crate::compact_index::CompactIndex;
use crate::indexer::{FileEntry, IndexStore};
use crate::string_pool::NameSignature;
use serde::Serialize;

use top_k::{Candidate, TopK};

/// 计算各关键词在 `name_lower` 中的字节区间，供前端高亮。
pub(crate) fn find_positions<'a>(
    name_lower: &str,
    keywords: impl IntoIterator<Item = &'a str>,
) -> Vec<(usize, usize)> {
    let mut positions = Vec::new();
    for keyword in keywords {
        if keyword.is_empty() {
            continue;
        }
        if let Some(start) = name_lower.find(keyword) {
            positions.push((start, start + keyword.len()));
        }
    }
    positions
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
                    top.offer(ordinal as usize, s, name);
                }
            } else {
                for &ordinal in entries.iter() {
                    if let Some(record) = index.records().get(ordinal as usize) {
                        if !filter.matches_record(record, || Some(name)) {
                            continue;
                        }
                    }
                    top.offer(ordinal as usize, s, name);
                }
            }
        }
    }
    top.finish()
}

/// Search mode per D-01, D-02, D-04.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Auto,
    Substring,
    Wildcard,
    Regex,
    Fuzzy,
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

/// Detect search mode from query string. Per D-02.
/// - Contains `*` or `?` -> Wildcard
/// - Starts with `regex:` -> Regex
/// - Otherwise -> Auto
pub fn detect_mode(query: &str) -> SearchMode {
    let trimmed = query.trim();
    if trimmed.starts_with("regex:") {
        SearchMode::Regex
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
    max_results: usize,
) -> (Vec<AdvancedSearchResult>, Option<String>) {
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

    let effective_filter = QueryFilter {
        file_type: effective_file_type,
        custom_ext: effective_custom_ext,
        file_types: effective_file_types,
        custom_exts: effective_custom_exts,
        size_filter: effective_size_filter,
    };

    let effective_mode = match mode {
        SearchMode::Auto => detect_mode(&clean_query),
        other => other,
    };

    let compiled_filter = {
        let read = store.read();
        CompiledRecordFilter::compile(&read, &effective_filter)
    };

    // 含路径语法（'/' 或 'in:'）时交由路径检索器。
    if effective_mode != SearchMode::Regex {
        if let Some(path_query) = path_query::PathQuery::parse(&clean_query) {
            let results = path_search::search(store, &path_query, &compiled_filter, max_results);
            return (results, None);
        }
    }

    // For Auto that detected as Auto (no special chars), run substring search.
    // 保持高确定性与精准度：普通文本输入严格按连续子串匹配，搜不到即返回空列表，避免无脑降级为模糊跳字匹配。
    let (results, error) = match effective_mode {
        SearchMode::Auto | SearchMode::Substring => {
            let results = substring::search(store, &clean_query, &compiled_filter, max_results);
            (results, None)
        }
        SearchMode::Wildcard => {
            let results = wildcard::search(store, &clean_query, &compiled_filter, max_results);
            (results, None)
        }
        SearchMode::Regex => {
            regex_search::search(store, &clean_query, &compiled_filter, max_results)
        }
        SearchMode::Fuzzy => {
            let results = fuzzy::search(store, &clean_query, &compiled_filter, max_results);
            (results, None)
        }
    };

    (results, error)
}

/// 主检索分发器包装入口。
///
/// 委托 `dispatch_with_filter` 并传入默认过滤器，由其统一解析查询串中的过滤词元并完成检索分发。
pub fn dispatch(
    store: &IndexStore,
    query: &str,
    mode: SearchMode,
    max_results: usize,
) -> (Vec<AdvancedSearchResult>, Option<String>) {
    dispatch_with_filter(store, query, mode, QueryFilter::default(), max_results)
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
        let (results, err) = dispatch_with_filter(&store, "search", SearchMode::Auto, filter, 10);
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
        let (results, err) = dispatch_with_filter(&store, "rndrvw", SearchMode::Fuzzy, filter, 10);
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
        let (results, err) =
            dispatch_with_filter(&store, "src/component", SearchMode::Auto, filter, 10);
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
        let (results, err) =
            dispatch_with_filter(&store, "pipeline type:doc", SearchMode::Auto, filter, 10);
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
        let (results, err) = dispatch_with_filter(&store, "rs", SearchMode::Auto, filter, 10);
        assert!(err.is_none());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "large.rs");
    }
}
