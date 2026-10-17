use crate::indexer::{score_match, IndexStore, MatchScore};

use super::{find_positions, AdvancedSearchResult, SearchMode};

/// Convert MatchScore to normalized f64 per research recommendation.
fn score_to_f64(score: MatchScore) -> f64 {
    match score {
        MatchScore::Exact => 1.0,
        MatchScore::Prefix => 0.75,
        MatchScore::Substring => 0.5,
    }
}

/// Substring search wrapping existing IndexStore logic.
/// Preserves v1.0 behavior: extension filter, multi-keyword AND, Exact > Prefix > Substring ranking.
pub fn search(store: &IndexStore, query: &str, max_results: usize) -> Vec<AdvancedSearchResult> {
    let query = query.trim();
    if query.is_empty() || max_results == 0 {
        return Vec::new();
    }
    let extension =
        (query.starts_with('.') && !query.contains(' ')).then(|| query[1..].to_lowercase());
    let keywords: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let read = store.read();
    let matches = match &extension {
        Some(extension) => {
            let mut top = super::top_k::TopK::new(max_results);
            for (id, view) in read.iter() {
                if view.extension == extension {
                    top.offer(id.0, MatchScore::Exact, view.name);
                }
            }
            top.finish()
        }
        None => super::rank_names(&read, max_results, |name| {
            score_match(&name.to_lowercase(), &keywords)
        }),
    };
    matches
        .into_iter()
        .map(|candidate| {
            let view = read
                .view(crate::compact_index::EntryId(candidate.ordinal))
                .unwrap();
            AdvancedSearchResult {
                entry: view.materialize(),
                score: score_to_f64(candidate.score),
                match_positions: if extension.is_some() {
                    Vec::new()
                } else {
                    find_positions(
                        &view.name.to_lowercase(),
                        keywords.iter().map(String::as_str),
                    )
                },
                parent_match_positions: Vec::new(),
                search_mode: SearchMode::Substring,
            }
        })
        .collect()
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
            make_entry("main.rs", "/main.rs", false), // prefix match for "main"
            make_entry("my_main.rs", "/my_main.rs", false), // substring match
        ]);
        let results = search(&store, "main", 500);
        assert_eq!(results.len(), 2);
        // Prefix should score higher than substring
        assert!(results[0].score > results[1].score);
        assert_eq!(results[0].score, 0.75); // prefix
        assert_eq!(results[1].score, 0.5); // substring
    }
}
