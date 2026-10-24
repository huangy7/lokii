use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

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
) -> Vec<AdvancedSearchResult> {
    let query = query.trim();
    if query.is_empty() || max_results == 0 {
        return Vec::new();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    // Fuzzy 匹配：由 NameSignature 内部统一封装查询串特征位构造
    let req = crate::string_pool::NameSignature::from_fuzzy_query(query);
    let read = store.read();
    let mut buffer = Vec::new();
    let matches = super::rank_unique_names(&read, filter, max_results, req, |name, _| {
        let haystack = Utf32Str::new(name, &mut buffer);
        pattern.score(haystack, &mut matcher)
    });
    let mut indices = Vec::new();
    matches
        .into_iter()
        .map(|candidate| {
            let view = read
                .view(crate::compact_index::EntryId(candidate.ordinal))
                .unwrap();
            let haystack = Utf32Str::new(view.name, &mut buffer);
            indices.clear();
            indices.resize(query.len(), 0);
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
        let results = search(&store, "mnrs", &filter, 500);
        assert!(!results.is_empty(), "mnrs should fuzzy match main.rs");
        assert_eq!(results[0].entry.name, "main.rs");
        assert_eq!(results[0].search_mode, SearchMode::Fuzzy);
    }

    #[test]
    fn test_fuzzy_no_match() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs")]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "xxxxxx", &filter, 500);
        assert!(results.is_empty());
    }

    #[test]
    fn test_fuzzy_match_positions() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs")]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "mnrs", &filter, 500);
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
        let results = search(&store, "mnrs", &filter, 500);
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
        let results = search(&store, "", &filter, 500);
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

        let results = search(&store, "mn", &filter, 500);
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

        let cafe_results: Vec<_> = search(&store, "cafe", &filter, 500)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert_eq!(cafe_results, ["café.txt"]);

        let naive_results: Vec<_> = search(&store, "naive", &filter, 500)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert_eq!(naive_results, ["naïve.md"]);
    }
}
