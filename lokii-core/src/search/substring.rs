use crate::indexer::{score_match, IndexStore, MatchScore};

use super::{AdvancedSearchResult, SearchMode};

/// Convert MatchScore to normalized f64 per research recommendation.
fn score_to_f64(score: MatchScore) -> f64 {
    match score {
        MatchScore::Exact => 1.0,
        MatchScore::Prefix => 0.75,
        MatchScore::Substring => 0.5,
    }
}

/// Compute match positions for substring matches.
/// Returns byte ranges of each keyword match in the name.
fn find_positions(name_lower: &str, keywords: &[String]) -> Vec<(usize, usize)> {
    let mut positions = Vec::new();
    for kw in keywords {
        if let Some(start) = name_lower.find(kw.as_str()) {
            positions.push((start, start + kw.len()));
        }
    }
    positions
}

/// Substring search wrapping existing IndexStore logic.
/// Preserves v1.0 behavior: extension filter, multi-keyword AND, Exact > Prefix > Substring ranking.
pub fn search(store: &IndexStore, query: &str, max_results: usize) -> Vec<AdvancedSearchResult> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }

    // Extension filter path: query starts with '.' and contains no spaces
    if query.starts_with('.') && !query.contains(' ') {
        let ext = query[1..].to_lowercase();
        let entries = store.entries();
        let mut results: Vec<AdvancedSearchResult> = entries
            .iter()
            .filter(|e| e.extension == ext)
            .map(|e| AdvancedSearchResult {
                entry: e.clone(),
                score: 1.0,
                match_positions: vec![],
                search_mode: SearchMode::Substring,
            })
            .collect();
        results.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));
        results.truncate(max_results);
        return results;
    }

    // Standard substring search
    let keywords: Vec<String> = query.split_whitespace().map(|s| s.to_lowercase()).collect();

    let entries = store.entries();
    let mut results: Vec<AdvancedSearchResult> = entries
        .iter()
        .filter_map(|e| {
            let name_lower = e.name.to_lowercase();
            score_match(&name_lower, &keywords).map(|score| {
                let positions = find_positions(&name_lower, &keywords);
                AdvancedSearchResult {
                    entry: e.clone(),
                    score: score_to_f64(score),
                    match_positions: positions,
                    search_mode: SearchMode::Substring,
                }
            })
        })
        .collect();

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.entry.name.cmp(&b.entry.name))
    });
    results.truncate(max_results);
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::{FileEntry, IndexStore};

    fn make_entry(name: &str, path: &str, is_dir: bool) -> FileEntry {
        let extension = if is_dir {
            String::new()
        } else {
            std::path::Path::new(name)
                .extension()
                .map(|ext| ext.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        };
        FileEntry {
            name: name.to_string(),
            path: path.to_string(),
            is_dir,
            size: 1024,
            modified: 1700000000,
            created_at: 0,
            extension,
        }
    }

    #[test]
    fn test_substring_basic() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("main.rs", "/src/main.rs", false),
            make_entry("readme.md", "/readme.md", false),
        ]);
        let results = search(&store, "main", 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "main.rs");
        assert_eq!(results[0].search_mode, SearchMode::Substring);
        assert!(results[0].score > 0.0);
    }

    #[test]
    fn test_substring_match_positions() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs", "/main.rs", false)]);
        let results = search(&store, "main", 500);
        assert!(!results[0].match_positions.is_empty());
        assert_eq!(results[0].match_positions[0], (0, 4));
    }

    #[test]
    fn test_extension_filter_uses_field() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("report.pdf", "/report.pdf", false),
            make_entry("data.csv", "/data.csv", false),
        ]);
        let results = search(&store, ".pdf", 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "report.pdf");
    }

    #[test]
    fn test_empty_query_returns_empty() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("file.txt", "/file.txt", false)]);
        let results = search(&store, "", 500);
        assert!(results.is_empty());
    }

    #[test]
    fn test_score_normalization() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("main.rs", "/main.rs", false),       // prefix match for "main"
            make_entry("my_main.rs", "/my_main.rs", false),  // substring match
        ]);
        let results = search(&store, "main", 500);
        assert_eq!(results.len(), 2);
        // Prefix should score higher than substring
        assert!(results[0].score > results[1].score);
        assert_eq!(results[0].score, 0.75); // prefix
        assert_eq!(results[1].score, 0.5);  // substring
    }
}
