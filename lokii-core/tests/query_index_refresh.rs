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

#[test]
fn incremental_event_batches_and_renames_preserve_search_consistency() {
    let store = IndexStore::new();
    store.replace(vec![
        entry("base.rs", "/proj/src/base.rs"),
        entry("old_name.rs", "/proj/src/old_name.rs"),
    ]);
    store.warm_query_index();

    // 增量新增与尾部删除批次
    assert_eq!(
        names(&store, "old_name", SearchMode::Substring),
        ["old_name.rs"]
    );
    store.add_entries(vec![entry("tail.rs", "/churn/tail.rs")]);
    // 通配符接入双层复合排序后，浅路径 /churn/tail.rs 优先于更深目录 /proj/src/{base,old_name}.rs
    assert_eq!(
        names(&store, "*.rs", SearchMode::Wildcard),
        ["tail.rs", "base.rs", "old_name.rs"]
    );

    // 删除尾部新增记录（不触发 Base CSR 重建）
    store.remove_paths(&HashSet::from(["/churn/tail.rs".to_string()]));
    assert_eq!(
        names(&store, "*.rs", SearchMode::Wildcard),
        ["base.rs", "old_name.rs"]
    );
}

#[test]
fn parent_meta_stays_consistent_across_renames_and_slot_reuse() {
    use lokii_core::compact_index::CompactIndex;
    use lokii_core::search::{dispatch_multi, CaseSensitivity, QueryFilter, SearchShard};

    let mut index = CompactIndex::from_entries(vec![
        entry("target.rs", "/shallow/target.rs"),
        entry("target.rs", "/deep/a/b/c/target.rs"),
    ])
    .unwrap();
    index.warm_query_index();

    let query_top = |idx: &CompactIndex| {
        let shard = SearchShard {
            index: idx,
            configured_path: None,
            active_path: None,
            volume_name: None,
            is_offline: false,
        };
        dispatch_multi(
            &[shard],
            "target.rs",
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            10,
            true,
        )
        .0
    };

    // 初始浅层排在深层之前
    let initial = query_top(&index);
    assert_eq!(initial[0].result.entry.path, "/shallow/target.rs");
    assert_eq!(initial[1].result.entry.path, "/deep/a/b/c/target.rs");

    // 目录重命名（rename_prefix）：将原浅层目录 /shallow 移入隐藏且更深目录 /.hidden/x/y/z/shallow，
    // 文件名不变但 parent 槽位变化，parent_meta 必须同步刷新
    index
        .rename_prefix("/shallow", "/.hidden/x/y/z/shallow")
        .unwrap();
    let after_rename = query_top(&index);
    assert_eq!(
        after_rename[0].result.entry.path, "/deep/a/b/c/target.rs",
        "renamed hidden parent must refresh parent_meta and demote below non-hidden /deep/a/b/c"
    );
    assert_eq!(
        after_rename[1].result.entry.path,
        "/.hidden/x/y/z/shallow/target.rs"
    );

    // 经 apply_event_batch 释放隐藏父目录槽位后，用新的浅层非隐藏父目录复用该槽位，验证 parent_meta 未残留旧隐藏位
    index
        .apply_event_batch(
            &[],
            &[],
            &["/.hidden/x/y/z/shallow/target.rs".to_string()],
            vec![entry("target.rs", "/top/target.rs")],
        )
        .unwrap();
    let after_reuse = query_top(&index);
    assert_eq!(after_reuse[0].result.entry.path, "/top/target.rs");
    assert!(after_reuse[0].result.score > after_reuse[1].result.score);

    // 经 apply_event_batch 重命名将 /top 移入隐藏深目录，同样触发 parent_meta 刷新
    index
        .apply_event_batch(
            &[("/top".to_string(), "/.cache/deep/a/b/c/top".to_string())],
            &[],
            &[],
            Vec::new(),
        )
        .unwrap();
    let after_batch_rename = query_top(&index);
    assert_eq!(
        after_batch_rename[0].result.entry.path,
        "/deep/a/b/c/target.rs"
    );
    assert_eq!(
        after_batch_rename[1].result.entry.path,
        "/.cache/deep/a/b/c/top/target.rs"
    );
}
