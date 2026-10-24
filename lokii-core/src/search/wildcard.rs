use glob_match::glob_match;

use super::{AdvancedSearchResult, SearchMode};
use crate::indexer::IndexStore;
use crate::string_pool::NameSignature;

/// 从通配符模式中提取所有不跨越 `*`/`?`/`[`/`]`/`{`/`}` 的字面量片段并生成签名超集掩码。
fn required_signature_for_glob(pattern_lower: &str) -> NameSignature {
    if pattern_lower.contains('[') || pattern_lower.contains('{') || pattern_lower.contains('\\') {
        return NameSignature::default();
    }
    let mut sig = NameSignature::default();
    for segment in pattern_lower.split(['*', '?']) {
        if !segment.is_empty() {
            sig = sig.union(NameSignature::from_substring_query(segment));
        }
    }
    sig
}

/// Wildcard search using glob patterns (* and ? wildcards).
///
/// Case-insensitive matching per R1.2. Both the pattern and filenames
/// are lowercased before comparison. Matching entries get score 1.0
/// (wildcard is binary match) and match_positions covering the full name.
pub fn search(store: &IndexStore, query: &str, max_results: usize) -> Vec<AdvancedSearchResult> {
    let pattern = query.trim().to_lowercase();
    if pattern.is_empty() || max_results == 0 {
        return Vec::new();
    }
    let read = store.read();
    let req = required_signature_for_glob(&pattern);

    // 针对纯 `*.ext` 模式（无其他通配符），直接跳过 `glob_match` 字符串自动机，做后缀字节比对
    let simple_suffix = pattern.strip_prefix("*.").filter(|rest| {
        !rest.is_empty()
            && !rest.contains(['*', '?', '[', ']', '{', '}', '\\'])
            && rest.is_ascii()
    });

    let mut ascii_buf = Vec::with_capacity(128);
    let mut top = super::top_k::TopK::new(max_results);
    let sorted = read.sorted_names();
    for &name_id in &sorted.ids {
        if let Some(sig) = read.name_signature(name_id) {
            if !sig.contains(req) {
                continue;
            }
        }
        let Some((name, folded)) = read.name_entry(name_id) else {
            continue;
        };
        let matched = if let Some(ext) = simple_suffix {
            let suffix_len = ext.len() + 1;
            if let Some(f) = folded {
                f.len() >= suffix_len
                    && f.as_bytes()[f.len() - suffix_len] == b'.'
                    && &f[f.len() - ext.len()..] == ext
            } else {
                let bytes = name.as_bytes();
                bytes.len() >= suffix_len
                    && bytes[bytes.len() - suffix_len] == b'.'
                    && bytes[bytes.len() - ext.len()..].eq_ignore_ascii_case(ext.as_bytes())
            }
        } else if let Some(f) = folded {
            glob_match(&pattern, f)
        } else {
            ascii_buf.clear();
            ascii_buf.extend(name.bytes().map(|b| b.to_ascii_lowercase()));
            // safety: name is guaranteed ASCII when `folded` is None
            let lower_str = unsafe { std::str::from_utf8_unchecked(&ascii_buf) };
            glob_match(&pattern, lower_str)
        };
        if matched {
            for &ordinal in read.entries_for_name(name_id).iter().take(max_results) {
                top.offer(ordinal as usize, 1, name);
                if top.is_full() {
                    break;
                }
            }
            if top.is_full() {
                break;
            }
        }
    }
    top.finish()
        .into_iter()
    .map(|candidate| {
        let view = read
            .view(crate::compact_index::EntryId(candidate.ordinal))
            .unwrap();
        AdvancedSearchResult {
            entry: view.materialize(),
            score: 1.0,
            match_positions: vec![(0, view.name.len())],
            parent_match_positions: Vec::new(),
            search_mode: SearchMode::Wildcard,
        }
    })
    .collect()
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
