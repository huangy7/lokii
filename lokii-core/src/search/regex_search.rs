use regex::RegexBuilder;

use crate::indexer::IndexStore;
use super::{AdvancedSearchResult, SearchMode};

/// Regex search using the `regex` crate.
///
/// Queries are expected to start with "regex:" prefix (stripped before compilation).
/// Case-insensitive matching per R1.3. Invalid regex returns an error string
/// instead of panicking (per D-07). A size_limit caps pathological patterns.
pub fn search(
    store: &IndexStore,
    query: &str,
    max_results: usize,
) -> (Vec<AdvancedSearchResult>, Option<String>) {
    let pattern = query
        .trim()
        .strip_prefix("regex:")
        .unwrap_or(query.trim());

    if pattern.is_empty() {
        return (Vec::new(), None);
    }

    let re = match RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(10_000_000)
        .build()
    {
        Ok(re) => re,
        Err(err) => {
            return (Vec::new(), Some(format!("Invalid regex: {}", err)));
        }
    };

    let entries = store.entries();
    let mut results: Vec<AdvancedSearchResult> = entries
        .iter()
        .filter_map(|entry| {
            re.find(&entry.name).map(|m| AdvancedSearchResult {
                entry: entry.clone(),
                score: 1.0,
                match_positions: vec![(m.start(), m.end())],
                search_mode: SearchMode::Regex,
            })
        })
        .collect();

    results.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));
    results.truncate(max_results);
    (results, None)
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
            make_entry("main.rs", "/main.rs"),
            make_entry("my_main.rs", "/my_main.rs"),
            make_entry("main.py", "/main.py"),
            make_entry("readme.md", "/readme.md"),
        ]);
        store
    }

    #[test]
    fn test_regex_caret_main() {
        let store = test_store();
        let (results, error) = search(&store, "regex:^main", 500);
        assert!(error.is_none());
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].entry.name, "main.py");
        assert_eq!(results[1].entry.name, "main.rs");
    }

    #[test]
    fn test_regex_rs_extension() {
        let store = test_store();
        let (results, error) = search(&store, r"regex:\.rs$", 500);
        assert!(error.is_none());
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].entry.name, "main.rs");
        assert_eq!(results[1].entry.name, "my_main.rs");
    }

    #[test]
    fn test_regex_case_insensitive() {
        let store = test_store();
        let (results, error) = search(&store, "regex:MAIN", 500);
        assert!(error.is_none());
        // All entries with "main" in name
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_invalid_regex_returns_error() {
        let store = test_store();
        let (results, error) = search(&store, "regex:[invalid", 500);
        assert!(results.is_empty());
        assert!(error.is_some());
        assert!(error.unwrap().contains("Invalid regex:"));
    }

    #[test]
    fn test_match_positions_contains_regex_range() {
        let store = test_store();
        let (results, _) = search(&store, "regex:^main", 500);
        assert!(!results.is_empty());
        // "main.py" -> match at (0, 4) for "main"
        let main_py = results.iter().find(|r| r.entry.name == "main.py").unwrap();
        assert_eq!(main_py.match_positions, vec![(0, 4)]);
    }

    #[test]
    fn test_results_sorted_by_name() {
        let store = test_store();
        let (results, _) = search(&store, "regex:main", 500);
        let names: Vec<&str> = results.iter().map(|r| r.entry.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn test_empty_pattern_after_prefix() {
        let store = test_store();
        let (results, error) = search(&store, "regex:", 500);
        assert!(results.is_empty());
        assert!(error.is_none());
    }

    #[test]
    fn test_search_mode_is_regex() {
        let store = test_store();
        let (results, _) = search(&store, "regex:main", 500);
        for r in &results {
            assert_eq!(r.search_mode, SearchMode::Regex);
        }
    }

    #[test]
    fn test_max_results_truncation() {
        let store = test_store();
        let (results, _) = search(&store, "regex:main", 1);
        assert_eq!(results.len(), 1);
    }
}
