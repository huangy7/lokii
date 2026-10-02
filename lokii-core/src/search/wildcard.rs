use glob_match::glob_match;

use crate::indexer::IndexStore;
use super::{AdvancedSearchResult, SearchMode};

/// Wildcard search using glob patterns (* and ? wildcards).
///
/// Case-insensitive matching per R1.2. Both the pattern and filenames
/// are lowercased before comparison. Matching entries get score 1.0
/// (wildcard is binary match) and match_positions covering the full name.
pub fn search(store: &IndexStore, query: &str, max_results: usize) -> Vec<AdvancedSearchResult> {
    let pattern = query.trim().to_lowercase();
    if pattern.is_empty() {
        return Vec::new();
    }

    let entries = store.entries();
    let mut results: Vec<AdvancedSearchResult> = entries
        .iter()
        .filter_map(|entry| {
            let name_lower = entry.name.to_lowercase();
            if glob_match(&pattern, &name_lower) {
                Some(AdvancedSearchResult {
                    entry: entry.clone(),
                    score: 1.0,
                    match_positions: vec![(0, entry.name.len())],
                    search_mode: SearchMode::Wildcard,
                })
            } else {
                None
            }
        })
        .collect();

    results.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));
    results.truncate(max_results);
    results
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
        let results = search(&store, "*.pdf", 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "report.pdf");
    }

    #[test]
    fn test_question_mark_two_chars() {
        let store = test_store();
        let results = search(&store, "test_??.rs", 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "test_01.rs");
    }

    #[test]
    fn test_case_insensitive() {
        let store = test_store();
        let results = search(&store, "*.PDF", 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "report.pdf");
    }

    #[test]
    fn test_star_main_star() {
        let store = test_store();
        let results = search(&store, "*main*", 500);
        assert_eq!(results.len(), 2);
        // Alphabetical: main.rs, my_main.rs
        assert_eq!(results[0].entry.name, "main.rs");
        assert_eq!(results[1].entry.name, "my_main.rs");
    }

    #[test]
    fn test_match_positions_full_name() {
        let store = test_store();
        let results = search(&store, "*.pdf", 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].match_positions, vec![(0, "report.pdf".len())]);
    }

    #[test]
    fn test_results_sorted_alphabetically() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("c_file.rs", "/c_file.rs"),
            make_entry("a_file.rs", "/a_file.rs"),
            make_entry("b_file.rs", "/b_file.rs"),
        ]);
        let results = search(&store, "*.rs", 500);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].entry.name, "a_file.rs");
        assert_eq!(results[1].entry.name, "b_file.rs");
        assert_eq!(results[2].entry.name, "c_file.rs");
    }

    #[test]
    fn test_empty_query_returns_empty() {
        let store = test_store();
        let results = search(&store, "", 500);
        assert!(results.is_empty());
    }

    #[test]
    fn test_whitespace_query_returns_empty() {
        let store = test_store();
        let results = search(&store, "   ", 500);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_mode_is_wildcard() {
        let store = test_store();
        let results = search(&store, "*.rs", 500);
        assert!(!results.is_empty());
        for r in &results {
            assert_eq!(r.search_mode, SearchMode::Wildcard);
        }
    }

    #[test]
    fn test_max_results_truncation() {
        let store = test_store();
        let results = search(&store, "*.rs", 2);
        assert_eq!(results.len(), 2);
    }
}
