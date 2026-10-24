use lokii_core::indexer::{FileEntry, IndexStore};
use lokii_core::search::{dispatch, SearchMode};
use std::collections::HashSet;

fn entry(name: &str, path: &str) -> FileEntry {
    FileEntry {
        name: name.to_string(),
        path: path.to_string(),
        is_dir: false,
        size: 1,
        modified: 1,
        created_at: 1,
        extension: name.rsplit('.').next().unwrap_or("").to_lowercase(),
    }
}

fn names(store: &IndexStore, query: &str, mode: SearchMode) -> Vec<String> {
    let (results, error) = dispatch(store, query, mode, 100);
    assert!(error.is_none(), "{query}: {error:?}");
    results.into_iter().map(|result| result.entry.name).collect()
}

#[test]
fn searches_see_additions_removals_and_reused_name_slots() {
    let store = IndexStore::new();
    store.replace(vec![
        entry("alpha.rs", "/a/alpha.rs"),
        entry("beta.txt", "/b/beta.txt"),
    ]);

    // Build the query index (CSR + signatures) and the lazy sorted-name cache.
    assert_eq!(names(&store, "alpha", SearchMode::Substring), ["alpha.rs"]);
    assert_eq!(names(&store, "*.rs", SearchMode::Wildcard), ["alpha.rs"]);

    store.remove_paths(&HashSet::from(["/a/alpha.rs".to_string()]));
    assert!(names(&store, "alpha", SearchMode::Substring).is_empty());
    assert!(names(&store, "*.rs", SearchMode::Wildcard).is_empty());

    // The removed name's pool slot is the first candidate for reuse. The new
    // name must not inherit the old name's signature, CSR entry, or sort position.
    store.add_entries(vec![entry("omega.txt", "/c/omega.txt")]);
    assert_eq!(names(&store, "omega", SearchMode::Substring), ["omega.txt"]);
    assert_eq!(names(&store, "*.txt", SearchMode::Wildcard), ["beta.txt", "omega.txt"]);
    assert!(names(&store, "alpha", SearchMode::Substring).is_empty());

    store.remove_paths(&HashSet::from(["/c/omega.txt".to_string()]));
    store.add_entries(vec![entry("gamma.md", "/d/gamma.md")]);
    assert_eq!(names(&store, "gamma", SearchMode::Substring), ["gamma.md"]);
    assert!(names(&store, "omega", SearchMode::Substring).is_empty());
    assert_eq!(names(&store, "*.md", SearchMode::Wildcard), ["gamma.md"]);
}

#[test]
fn searches_see_extension_changes_after_updates() {
    let store = IndexStore::new();
    store.replace(vec![entry("one.rs", "/one.rs")]);

    // Populate the lazy sorted cache with the extension path.
    assert_eq!(names(&store, ".rs", SearchMode::Substring), ["one.rs"]);

    store.add_entries(vec![entry("two.pdf", "/two.pdf")]);
    assert_eq!(names(&store, ".pdf", SearchMode::Substring), ["two.pdf"]);
    assert_eq!(
        names(&store, "*.pdf", SearchMode::Wildcard),
        ["two.pdf"]
    );
}

#[test]
fn non_ascii_names_remain_searchable_after_updates() {
    let store = IndexStore::new();
    store.replace(vec![entry("中文文档.txt", "/中文文档.txt")]);
    assert_eq!(names(&store, "中文", SearchMode::Substring), ["中文文档.txt"]);

    store.add_entries(vec![entry("Éclair.md", "/Éclair.md")]);
    assert_eq!(names(&store, "éclair", SearchMode::Substring), ["Éclair.md"]);
    assert_eq!(names(&store, "中文", SearchMode::Substring), ["中文文档.txt"]);
    assert_eq!(
        names(&store, "*.md", SearchMode::Wildcard),
        ["Éclair.md"]
    );
}
