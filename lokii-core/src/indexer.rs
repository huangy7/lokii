use std::collections::HashSet;
use std::sync::Arc;

use bitcode::{Decode, Encode};
use parking_lot::RwLock;
use serde::Serialize;

/// A single file or directory entry in the index.
///
/// All fields use bitcode-compatible types:
/// - `path` is `String` (not `PathBuf`) for bitcode native Encode/Decode
/// - `modified` is `u64` unix timestamp (not `SystemTime`) for bitcode compatibility
#[derive(Debug, Clone, PartialEq, Encode, Decode, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: u64,
    /// File creation time as Unix timestamp (seconds). 0 if unavailable.
    pub created_at: u64,
    /// File extension, lowercased (e.g., "rs", "pdf"). Empty for directories and extensionless files.
    pub extension: String,
}

/// Match quality tier for search result ranking.
///
/// Higher value = better match. Used to sort results so that
/// exact name matches appear before prefix matches, which appear
/// before substring matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MatchScore {
    Substring = 0,
    Prefix = 1,
    Exact = 2,
}

/// A search result pairing a file entry with its match score.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub entry: FileEntry,
    pub score: MatchScore,
}

/// Score a filename against a set of keywords using AND logic.
///
/// All keywords must be present in `name_lower` (case-insensitive matching
/// is the caller's responsibility -- `name_lower` should already be lowercased).
/// The score is determined by the first keyword's match quality:
/// - Exact: the entire name equals the first keyword
/// - Prefix: the name starts with the first keyword
/// - Substring: the name contains the first keyword elsewhere
///
/// Returns `None` if any keyword is not found in the name.
pub fn score_match(name_lower: &str, keywords: &[String]) -> Option<MatchScore> {
    for kw in keywords {
        if !name_lower.contains(kw.as_str()) {
            return None;
        }
    }

    let first = &keywords[0];
    let score = if name_lower == first.as_str() {
        MatchScore::Exact
    } else if name_lower.starts_with(first.as_str()) {
        MatchScore::Prefix
    } else {
        MatchScore::Substring
    };

    Some(score)
}

/// Thread-safe in-memory index store.
///
/// Wraps `Vec<FileEntry>` behind `Arc<parking_lot::RwLock>` for concurrent
/// read-heavy access (many searches, rare writes).
#[derive(Clone)]
pub struct IndexStore {
    entries: Arc<RwLock<Vec<FileEntry>>>,
}

impl IndexStore {
    /// Create a new empty index store.
    pub fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Replace the entire index with new entries.
    ///
    /// Takes a write lock and swaps the Vec. Build the new Vec outside
    /// the lock to minimize write lock duration.
    pub fn replace(&self, entries: Vec<FileEntry>) {
        let mut lock = self.entries.write();
        *lock = entries;
    }

    /// Return the number of entries in the index.
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    /// Return whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }

    /// Return a read guard to the entries for search operations.
    pub fn entries(&self) -> parking_lot::RwLockReadGuard<'_, Vec<FileEntry>> {
        self.entries.read()
    }

    /// Add new entries to the index (per D-06).
    pub fn add_entries(&self, new_entries: Vec<FileEntry>) {
        if new_entries.is_empty() {
            return;
        }
        let mut lock = self.entries.write();
        lock.extend(new_entries);
    }

    /// Remove entries whose path is in the given set (per D-06).
    pub fn remove_paths(&self, paths: &HashSet<String>) {
        if paths.is_empty() {
            return;
        }
        let mut lock = self.entries.write();
        lock.retain(|e| !paths.contains(&e.path));
    }

    /// Get a mutable write guard for complex batch operations (per D-07).
    pub fn entries_mut(&self) -> parking_lot::RwLockWriteGuard<'_, Vec<FileEntry>> {
        self.entries.write()
    }

    /// Search the index for entries matching the query.
    ///
    /// Query behavior:
    /// - Empty/whitespace-only query returns empty results
    /// - Query starting with '.' (no spaces) triggers extension-only filter
    /// - Otherwise: split by whitespace into keywords, ALL must match (AND logic)
    ///
    /// Results are ranked: Exact > Prefix > Substring. Within each tier,
    /// results are sorted alphabetically by name. Results are truncated
    /// to `max_results`.
    pub fn search(&self, query: &str, max_results: usize) -> Vec<SearchResult> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }

        // Extension filter path: query starts with '.' and contains no spaces
        if query.starts_with('.') && !query.contains(' ') {
            let ext = query[1..].to_lowercase();
            let entries = self.entries.read();
            let mut results: Vec<SearchResult> = entries
                .iter()
                .filter(|e| {
                    e.name
                        .rsplit('.')
                        .next()
                        .map(|e_ext| e_ext.to_lowercase() == ext)
                        .unwrap_or(false)
                })
                .map(|e| SearchResult {
                    entry: e.clone(),
                    score: MatchScore::Exact,
                })
                .collect();
            results.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));
            results.truncate(max_results);
            return results;
        }

        // Standard search path
        let keywords: Vec<String> = query
            .split_whitespace()
            .map(|s| s.to_lowercase())
            .collect();

        let entries = self.entries.read();
        let mut results: Vec<SearchResult> = entries
            .iter()
            .filter_map(|e| {
                let name_lower = e.name.to_lowercase();
                score_match(&name_lower, &keywords).map(|score| SearchResult {
                    entry: e.clone(),
                    score,
                })
            })
            .collect();

        // Sort: score descending, then name ascending
        results.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.entry.name.cmp(&b.entry.name))
        });
        results.truncate(max_results);
        results
    }
}

impl Default for IndexStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

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
    fn test_new_empty() {
        let store = IndexStore::new();
        assert_eq!(store.len(), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn test_replace_and_len() {
        let store = IndexStore::new();
        let entries = vec![
            make_entry("file1.rs", "/tmp/file1.rs", false),
            make_entry("file2.rs", "/tmp/file2.rs", false),
            make_entry("src", "/tmp/src", true),
        ];
        store.replace(entries);
        assert_eq!(store.len(), 3);
        assert!(!store.is_empty());

        // Replace again with fewer entries
        store.replace(vec![make_entry("only.txt", "/tmp/only.txt", false)]);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn test_file_entry_fields() {
        let entry = FileEntry {
            name: "main.rs".to_string(),
            path: "/Users/test/project/main.rs".to_string(),
            is_dir: false,
            size: 2048,
            modified: 1700000000,
            created_at: 0,
            extension: "rs".to_string(),
        };
        assert_eq!(entry.name, "main.rs");
        assert_eq!(entry.path, "/Users/test/project/main.rs");
        assert!(!entry.is_dir);
        assert_eq!(entry.size, 2048);
        assert_eq!(entry.modified, 1700000000);
        assert_eq!(entry.extension, "rs");
    }

    #[test]
    fn test_clone_shares_state() {
        let store1 = IndexStore::new();
        let store2 = store1.clone();

        store1.replace(vec![make_entry("a.txt", "/a.txt", false)]);
        assert_eq!(store2.len(), 1);
    }

    #[test]
    fn test_entries_read_guard() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("b.txt", "/b.txt", false),
            make_entry("a.txt", "/a.txt", false),
        ]);
        let guard = store.entries();
        assert_eq!(guard.len(), 2);
        assert_eq!(guard[0].name, "b.txt");
        assert_eq!(guard[1].name, "a.txt");
    }

    // --- score_match tests ---

    #[test]
    fn test_score_exact() {
        let keywords = vec!["main.rs".to_string()];
        assert_eq!(score_match("main.rs", &keywords), Some(MatchScore::Exact));
    }

    #[test]
    fn test_score_prefix() {
        let keywords = vec!["main".to_string()];
        assert_eq!(score_match("main.rs", &keywords), Some(MatchScore::Prefix));
    }

    #[test]
    fn test_score_substring() {
        let keywords = vec!["main".to_string()];
        assert_eq!(
            score_match("my_main.rs", &keywords),
            Some(MatchScore::Substring)
        );
    }

    #[test]
    fn test_score_no_match() {
        let keywords = vec!["main".to_string()];
        assert_eq!(score_match("readme.md", &keywords), None);
    }

    #[test]
    fn test_score_multi_keyword_and() {
        let keywords = vec!["main".to_string(), "rs".to_string()];
        assert_eq!(score_match("main.rs", &keywords), Some(MatchScore::Prefix));
    }

    #[test]
    fn test_score_multi_keyword_fail() {
        let keywords = vec!["main".to_string(), "rs".to_string()];
        assert_eq!(score_match("readme.md", &keywords), None);
    }

    // --- search tests ---

    #[test]
    fn test_search_basic() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("main.rs", "/src/main.rs", false),
            make_entry("readme.md", "/readme.md", false),
        ]);
        let results = store.search("main", 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "main.rs");
    }

    #[test]
    fn test_search_case_insensitive() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs", "/src/main.rs", false)]);
        let results = store.search("MAIN", 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "main.rs");
    }

    #[test]
    fn test_search_multi_keyword() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("main.rs", "/src/main.rs", false),
            make_entry("main.py", "/src/main.py", false),
        ]);
        let results = store.search("main rs", 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "main.rs");
    }

    #[test]
    fn test_search_ranking_order() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("my_main.rs", "/my_main.rs", false),
            make_entry("main_test.rs", "/main_test.rs", false),
            make_entry("main.rs", "/main.rs", false),
        ]);
        // "main.rs" lowercased matches "main" exactly? No -- "main.rs" != "main"
        // "main.rs" starts_with "main" -> Prefix
        // "main_test.rs" starts_with "main" -> Prefix
        // "my_main.rs" contains "main" -> Substring
        let results = store.search("main", 500);
        assert_eq!(results.len(), 3);
        // Prefix tier (alphabetical): main.rs, main_test.rs
        assert_eq!(results[0].entry.name, "main.rs");
        assert_eq!(results[0].score, MatchScore::Prefix);
        assert_eq!(results[1].entry.name, "main_test.rs");
        assert_eq!(results[1].score, MatchScore::Prefix);
        // Substring tier
        assert_eq!(results[2].entry.name, "my_main.rs");
        assert_eq!(results[2].score, MatchScore::Substring);
    }

    #[test]
    fn test_search_alphabetical_within_tier() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("c_main.rs", "/c_main.rs", false),
            make_entry("a_main.rs", "/a_main.rs", false),
            make_entry("b_main.rs", "/b_main.rs", false),
        ]);
        let results = store.search("main", 500);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].entry.name, "a_main.rs");
        assert_eq!(results[1].entry.name, "b_main.rs");
        assert_eq!(results[2].entry.name, "c_main.rs");
    }

    #[test]
    fn test_search_extension_filter() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("report.pdf", "/report.pdf", false),
            make_entry("notes.pdf.bak", "/notes.pdf.bak", false),
            make_entry("data.csv", "/data.csv", false),
        ]);
        let results = store.search(".pdf", 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "report.pdf");
    }

    #[test]
    fn test_search_extension_filter_case() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("report.pdf", "/report.pdf", false)]);
        let results = store.search(".PDF", 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "report.pdf");
    }

    #[test]
    fn test_search_max_results() {
        let store = IndexStore::new();
        let entries: Vec<FileEntry> = (0..10)
            .map(|i| make_entry(&format!("test_{i}.rs"), &format!("/test_{i}.rs"), false))
            .collect();
        store.replace(entries);
        let results = store.search("test", 3);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_search_empty_query() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("file.txt", "/file.txt", false)]);
        let results = store.search("", 500);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_no_matches() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("file.txt", "/file.txt", false)]);
        let results = store.search("zzzzz", 500);
        assert!(results.is_empty());
    }

    #[test]
    fn test_file_entry_bitcode_roundtrip() {
        let entry = FileEntry {
            name: "test.txt".to_string(),
            path: "/tmp/test.txt".to_string(),
            is_dir: false,
            size: 512,
            modified: 1700000000,
            created_at: 0,
            extension: "txt".to_string(),
        };
        let encoded = bitcode::encode(&entry);
        let decoded: FileEntry = bitcode::decode(&encoded).expect("decode failed");
        assert_eq!(entry, decoded);
    }

    // --- add_entries tests ---

    #[test]
    fn test_add_entries_empty_vec() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("a.txt", "/a.txt", false)]);
        store.add_entries(vec![]);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn test_add_entries_increases_length() {
        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("a.txt", "/a.txt", false),
            make_entry("b.txt", "/b.txt", false),
            make_entry("c.txt", "/c.txt", false),
        ]);
        assert_eq!(store.len(), 3);
    }

    // --- remove_paths tests ---

    #[test]
    fn test_remove_paths_matching() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("a.txt", "/a.txt", false),
            make_entry("b.txt", "/b.txt", false),
            make_entry("c.txt", "/c.txt", false),
        ]);
        let paths: HashSet<String> = ["/a.txt".to_string(), "/c.txt".to_string()]
            .into_iter()
            .collect();
        store.remove_paths(&paths);
        assert_eq!(store.len(), 1);
        let entries = store.entries();
        assert_eq!(entries[0].name, "b.txt");
    }

    #[test]
    fn test_remove_paths_empty_set() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("a.txt", "/a.txt", false)]);
        store.remove_paths(&HashSet::new());
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn test_remove_paths_nonexistent() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("a.txt", "/a.txt", false)]);
        let paths: HashSet<String> = ["/z.txt".to_string()].into_iter().collect();
        store.remove_paths(&paths);
        assert_eq!(store.len(), 1);
    }

    // --- entries_mut tests ---

    #[test]
    fn test_entries_mut_write_guard() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("a.txt", "/a.txt", false)]);
        {
            let mut guard = store.entries_mut();
            guard.push(make_entry("b.txt", "/b.txt", false));
        }
        assert_eq!(store.len(), 2);
    }

    // --- combined operations ---

    #[test]
    fn test_add_then_remove_sequence() {
        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("a.txt", "/a.txt", false),
            make_entry("b.txt", "/b.txt", false),
        ]);
        assert_eq!(store.len(), 2);

        let paths: HashSet<String> = ["/a.txt".to_string()].into_iter().collect();
        store.remove_paths(&paths);
        assert_eq!(store.len(), 1);

        let entries = store.entries();
        assert_eq!(entries[0].name, "b.txt");
    }
}
