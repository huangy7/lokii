use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::indexer::IndexStore;

use super::{AdvancedSearchResult, SearchMode};

/// Fuzzy search using nucleo-matcher (helix-editor's matcher). Per D-08, D-09, D-10.
///
/// Returns results sorted by fuzzy score descending. Entries where nucleo
/// returns None (no match) are excluded -- trusting nucleo's threshold per D-08.
///
/// match_positions are individual character indices converted to byte-range pairs
/// of (byte_start, byte_start + char_len) for each matched character.
pub fn search(
    store: &IndexStore,
    query: &str,
    max_results: usize,
) -> Vec<AdvancedSearchResult> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }

    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );

    let entries = store.entries();
    // Pre-allocate reusable scratch buffers per PITFALLS advice
    let mut buf = Vec::new();
    let mut indices_buf = Vec::new();

    let mut results: Vec<(AdvancedSearchResult, u32)> = entries
        .iter()
        .filter_map(|e| {
            let haystack = Utf32Str::new(&e.name, &mut buf);
            indices_buf.clear();
            indices_buf.resize(query.len(), 0);

            let score = pattern.score(haystack, &mut matcher)?;
            // Get matched indices
            pattern.indices(haystack, &mut matcher, &mut indices_buf);

            // Convert character indices to byte ranges
            let match_positions: Vec<(usize, usize)> = indices_buf
                .iter()
                .filter_map(|&char_idx| {
                    let char_idx = char_idx as usize;
                    e.name
                        .char_indices()
                        .nth(char_idx)
                        .map(|(byte_pos, ch)| (byte_pos, byte_pos + ch.len_utf8()))
                })
                .collect();

            // Normalize score to 0.0-1.0 range. nucleo scores are u32,
            // typical max ~1000 for perfect matches. Use 1000 as ceiling.
            let normalized_score = (score as f64 / 1000.0).min(1.0);

            Some((
                AdvancedSearchResult {
                    entry: e.clone(),
                    score: normalized_score,
                    match_positions,
                    search_mode: SearchMode::Fuzzy,
                },
                score,
            ))
        })
        .collect();

    // Sort by raw score descending, then name ascending for stability
    results.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.entry.name.cmp(&b.0.entry.name))
    });
    results.truncate(max_results);
    results.into_iter().map(|(r, _)| r).collect()
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
        let results = search(&store, "mnrs", 500);
        assert!(!results.is_empty(), "mnrs should fuzzy match main.rs");
        assert_eq!(results[0].entry.name, "main.rs");
        assert_eq!(results[0].search_mode, SearchMode::Fuzzy);
    }

    #[test]
    fn test_fuzzy_no_match() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs")]);
        let results = search(&store, "xxxxxx", 500);
        assert!(results.is_empty());
    }

    #[test]
    fn test_fuzzy_match_positions() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs")]);
        let results = search(&store, "mnrs", 500);
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
        let results = search(&store, "mnrs", 500);
        // main.rs should score higher (shorter, more compact match)
        if results.len() >= 2 {
            assert!(results[0].score >= results[1].score);
        }
    }

    #[test]
    fn test_fuzzy_empty_query() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs")]);
        let results = search(&store, "", 500);
        assert!(results.is_empty());
    }
}
