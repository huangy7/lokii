use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::compact_index::CompactIndex;
use crate::indexer::IndexStore;

use super::{AdvancedSearchResult, CompiledRecordFilter, SearchMode};

/// 模糊检索器，接入预编译位掩码记录过滤器以达成零堆分配预筛选。
///
/// Returns results sorted by fuzzy score descending. Entries where nucleo
/// returns None (no match) are excluded -- trusting nucleo's threshold per D-08.
///
/// match_positions are individual character indices converted to byte-range pairs
/// of (byte_start, byte_start + char_len) for each matched character.
pub fn search(
    store: &IndexStore,
    query: &str,
    filter: &CompiledRecordFilter,
    max_results: usize,
    case_sensitive: bool,
) -> Vec<AdvancedSearchResult> {
    let read = store.read();
    search_index(&read, query, filter, max_results, case_sensitive)
}

/// 接收统一快照读锁的模糊检索实现，杜绝锁重入与快照位掩码漂移。
pub fn search_index(
    read: &CompactIndex,
    query: &str,
    filter: &CompiledRecordFilter,
    max_results: usize,
    case_sensitive: bool,
) -> Vec<AdvancedSearchResult> {
    let query = query.trim();
    if query.is_empty() || max_results == 0 {
        return Vec::new();
    }
    let query_cjk = crate::cjk::to_simplified_str(query);
    let case_matching = if case_sensitive {
        CaseMatching::Respect
    } else {
        CaseMatching::Ignore
    };
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::new(
        &query_cjk,
        case_matching,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    // Fuzzy 匹配：由 NameSignature 内部统一封装查询串特征位构造
    let req = crate::string_pool::NameSignature::from_fuzzy_query(&query_cjk);
    let mut buffer = Vec::new();
    let matches = super::rank_unique_names(read, filter, max_results, req, |name, _| {
        let name_cjk = crate::cjk::to_simplified_str(name);
        let haystack = Utf32Str::new(&name_cjk, &mut buffer);
        pattern.score(haystack, &mut matcher)
    });
    let mut indices = Vec::new();
    matches
        .into_iter()
        .map(|candidate| {
            let view = read
                .view(crate::compact_index::EntryId(candidate.ordinal))
                .unwrap();
            let name_cjk = crate::cjk::to_simplified_str(view.name);
            let haystack = Utf32Str::new(&name_cjk, &mut buffer);
            indices.clear();
            pattern.indices(haystack, &mut matcher, &mut indices);
            let positions = indices
                .iter()
                .filter_map(|&ordinal| {
                    view.name
                        .char_indices()
                        .nth(ordinal as usize)
                        .map(|(start, character)| (start, start + character.len_utf8()))
                })
                .collect();
            AdvancedSearchResult {
                entry: view.materialize(),
                score: (candidate.score as f64 / 1000.0).min(1.0),
                match_positions: positions,
                parent_match_positions: Vec::new(),
                search_mode: SearchMode::Fuzzy,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::{FileEntry, IndexStore};

    fn make_entry(name: &str) -> FileEntry {
        let extension = std::path::Path::new(name)
            .extension()
            .map(|ext| ext.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        FileEntry {
            name: name.to_string(),
            path: format!("/{name}"),
            is_dir: false,
            size: 1024,
            modified: 1700000000,
            created_at: 0,
            extension,
        }
    }

    #[test]
    fn test_fuzzy_basic() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs"), make_entry("readme.md")]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "mnrs", &filter, 500, false);
        assert!(!results.is_empty(), "mnrs should fuzzy match main.rs");
        assert_eq!(results[0].entry.name, "main.rs");
        assert_eq!(results[0].search_mode, SearchMode::Fuzzy);
    }

    #[test]
    fn test_fuzzy_no_match() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs")]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "xxxxxx", &filter, 500, false);
        assert!(results.is_empty());
    }

    #[test]
    fn test_fuzzy_case_sensitive() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs"), make_entry("MAIN.rs")]);
        let filter = CompiledRecordFilter::default();

        let sensitive: Vec<_> = search(&store, "MAIN", &filter, 500, true)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert_eq!(sensitive, ["MAIN.rs"]);

        let insensitive: Vec<_> = search(&store, "MAIN", &filter, 500, false)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert_eq!(insensitive.len(), 2);
    }

    #[test]
    fn test_fuzzy_match_positions() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs")]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "mnrs", &filter, 500, false);
        assert!(
            !results[0].match_positions.is_empty(),
            "Should have match positions for highlighted chars"
        );
    }

    #[test]
    fn test_fuzzy_score_ranking() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("main.rs"),
            make_entry("my_amazing_narrative.rs"),
        ]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "mnrs", &filter, 500, false);
        // main.rs should score higher (shorter, more compact match)
        if results.len() >= 2 {
            assert!(results[0].score >= results[1].score);
        }
    }

    #[test]
    fn test_fuzzy_empty_query() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs")]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "", &filter, 500, false);
        assert!(results.is_empty());
    }

    #[test]
    fn test_fuzzy_with_compiled_filter() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs"), make_entry("main.md")]);
        let read = store.read();
        let query_filter = super::super::QueryFilter {
            file_type: super::super::FileType::Document,
            custom_ext: None,
            ..Default::default()
        };
        let filter = CompiledRecordFilter::compile(&read, &query_filter);
        drop(read);

        let results = search(&store, "mn", &filter, 500, false);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "main.md");
    }

    #[test]
    fn test_fuzzy_matches_accented_filenames() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("café.txt"),
            make_entry("naïve.md"),
            make_entry("中文文档.txt"),
        ]);
        let filter = CompiledRecordFilter::default();

        let cafe_results: Vec<_> = search(&store, "cafe", &filter, 500, false)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert_eq!(cafe_results, ["café.txt"]);

        let naive_results: Vec<_> = search(&store, "naive", &filter, 500, false)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert_eq!(naive_results, ["naïve.md"]);
    }

    /// 签名预筛只能做超集剪枝：nucleo 判定的每个命中，其索引签名必须包含查询要求的全部位。
    /// 覆盖汉字、繁体、假名、谚文、变音拉丁与纯 ASCII，防止收紧预筛后静默漏结果。
    #[test]
    fn signature_prefilter_never_drops_a_real_match() {
        let names = [
            "作业.docx", "作业本", "zuoye.txt", "哈哈哈.txt", "amplitude.txt",
            "cafe.md", "café.md", "檔案.txt", "档案.txt", "ファイル.txt", "한글.txt",
            "example.rs", "hc_a.txt", "Hadoopabc.md",
        ];
        let store = IndexStore::new();
        store.replace(names.iter().map(|name| make_entry(name)).collect());

        let queries = [
            "作业", "哈哈哈", "档案", "檔案", "cafe", "café", "abcd", "ha", "la",
            "ファイル", "한글", "xyz",
        ];
        // 直接检查索引里真实生成的签名，避免在测试里复刻折叠规则
        let read = store.read();
        let index = read.query_index();
        let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);
        let mut buffer = Vec::new();
        let mut checked = 0usize;

        for query in queries {
            let query_fold = crate::cjk::to_simplified_str(query);
            let required = crate::string_pool::NameSignature::from_fuzzy_query(&query_fold);
            let pattern = nucleo_matcher::pattern::Pattern::new(
                &query_fold,
                nucleo_matcher::pattern::CaseMatching::Ignore,
                nucleo_matcher::pattern::Normalization::Smart,
                nucleo_matcher::pattern::AtomKind::Fuzzy,
            );
            for name_id in 0..index.slot_count() as u32 {
                if !index.is_ready(name_id) {
                    continue;
                }
                let Some(name) = read.name_text(name_id) else {
                    continue;
                };
                // 与模糊检索一致：打分输入是折叠后的文件名，不是原始名
                let name_cjk = crate::cjk::to_simplified_str(name);
                let matched = pattern
                    .score(nucleo_matcher::Utf32Str::new(&name_cjk, &mut buffer), &mut matcher)
                    .is_some();
                if !matched {
                    continue;
                }
                checked += 1;
                assert!(
                    index.signature_at(name_id).contains(required),
                    "签名预筛会漏掉命中: q={query} name={name}"
                );
            }
        }
        assert!(checked > 0, "用例应至少产生一个真实命中");
    }

    #[test]
    fn cjk_fuzzy_query_still_finds_results() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("作业.docx"),
            make_entry("作业本"),
            make_entry("zuoye.txt"),
        ]);
        let filter = CompiledRecordFilter::default();

        let names: Vec<_> = search(&store, "作业", &filter, 500, false)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert!(names.contains(&"作业.docx".to_string()), "汉字模糊检索结果: {names:?}");
        assert!(names.contains(&"作业本".to_string()), "汉字模糊检索结果: {names:?}");
    }
}
