pub mod fuzzy;
pub mod substring;
pub mod wildcard;
pub mod regex_search;

use crate::indexer::{FileEntry, IndexStore};
use serde::Serialize;

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

/// Main search dispatcher. Per D-01, D-03, D-05.
///
/// If mode is Auto: run substring first; if no results, fallback to fuzzy (Plan 03 adds this).
/// If mode is explicit: run that mode only. Per D-04.
pub fn dispatch(
    store: &IndexStore,
    query: &str,
    mode: SearchMode,
    max_results: usize,
) -> (Vec<AdvancedSearchResult>, Option<String>) {
    let effective_mode = match mode {
        SearchMode::Auto => detect_mode(query),
        other => other,
    };

    // For Auto that detected as Auto (no special chars), try substring first
    let (results, error) = match effective_mode {
        SearchMode::Auto | SearchMode::Substring => {
            let results = substring::search(store, query, max_results);
            if results.is_empty() && matches!(mode, SearchMode::Auto) {
                // D-01: fallback to fuzzy when substring has no results
                // D-03: fuzzy results only appear when substring found nothing
                let fuzzy_results = fuzzy::search(store, query, max_results);
                (fuzzy_results, None)
            } else {
                (results, None)
            }
        }
        SearchMode::Wildcard => {
            let results = wildcard::search(store, query, max_results);
            (results, None)
        }
        SearchMode::Regex => {
            regex_search::search(store, query, max_results)
        }
        SearchMode::Fuzzy => {
            let results = fuzzy::search(store, query, max_results);
            (results, None)
        }
    };

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
        // "mnrs" won't match substring, should fallback to fuzzy in Auto mode
        let (results, error) = dispatch(&store, "mnrs", SearchMode::Auto, 500);
        assert!(error.is_none());
        assert!(!results.is_empty(), "Auto mode should fallback to fuzzy");
        assert_eq!(results[0].search_mode, SearchMode::Fuzzy);
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
            search_mode: SearchMode::Substring,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("matchPositions"), "should have camelCase matchPositions");
        assert!(json.contains("searchMode"), "should have camelCase searchMode");
    }
}
