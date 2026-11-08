use lokii_core::compact_index::{CompactIndex, EntryId};
use lokii_core::indexer::FileEntry;

fn entry(name: &str, path: &str) -> FileEntry {
    FileEntry {
        name: name.into(),
        path: path.into(),
        is_dir: false,
        size: u64::MAX,
        modified: u64::MAX,
        created_at: u64::MAX,
        extension: "rs".into(),
    }
}

#[test]
fn compact_paths_and_metadata_roundtrip() {
    let originals = [
        entry("/", "/"),
        entry("main.rs", "/a/main.rs"),
        entry("alias", "/actual"),
        entry("name", ""),
        entry("relative", "relative"),
        entry("中文", "/space dir/中文"),
        entry("name", "/trailing/name/"),
    ];
    let compact = CompactIndex::from_entries(originals.clone()).unwrap();
    for (ordinal, original) in originals.iter().enumerate() {
        assert_eq!(&compact.materialize(EntryId(ordinal)).unwrap(), original);
    }
    compact.validate().unwrap();
}

#[test]
fn repeated_paths_share_storage_and_release_strings() {
    let mut compact = CompactIndex::from_entries(
        (0..1000).map(|ordinal| entry("main.rs", &format!("/parent_{ordinal}/main.rs"))),
    )
    .unwrap();
    assert_eq!(compact.pool_counts().0, 1);
    compact.remove_matching(|_| true);
    assert_eq!(compact.pool_counts(), (0, 0, 0, 0));
    compact.validate().unwrap();
}

#[test]
fn store_uses_compact_storage() {
    let store = lokii_core::indexer::IndexStore::new();
    store.replace(vec![
        entry("main.rs", "/a/main.rs"),
        entry("main.rs", "/b/main.rs"),
    ]);
    assert_eq!(store.read().pool_counts().0, 1);
}

#[test]
fn compact_cache_migrates_legacy_and_rejects_corruption() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("index.cache");
    let originals = vec![entry("main.rs", "/a/main.rs"), entry("alias", "/actual")];
    let mut legacy = b"LOKI".to_vec();
    legacy.extend_from_slice(&3u32.to_le_bytes());
    legacy.extend_from_slice(&bitcode::encode(&originals));
    std::fs::write(&path, legacy).unwrap();
    let (compact, checkpoint) = lokii_core::cache::load_compact(&path).unwrap();
    assert_eq!(checkpoint, None);
    let saved = lokii_core::cache::Checkpoint {
        event_id: 42,
        fingerprint: 7,
    };
    lokii_core::cache::save_compact(&compact, Some(saved), &path).unwrap();
    assert_eq!(&std::fs::read(&path).unwrap()[4..8], &5u32.to_le_bytes());
    let (restored, checkpoint) = lokii_core::cache::load_compact(&path).unwrap();
    assert_eq!(checkpoint, Some(saved));
    for (ordinal, original) in originals.iter().enumerate() {
        assert_eq!(&restored.materialize(EntryId(ordinal)).unwrap(), original);
    }
    std::fs::write(&path, b"LOKI\x04\0\0\0bad").unwrap();
    assert!(lokii_core::cache::load_compact(&path).is_err());
    std::fs::write(&path, b"LOKI\x05\0\0\0short").unwrap();
    assert!(lokii_core::cache::load_compact(&path).is_err());
}

#[test]
fn legacy_migration_preserves_empty_duplicate_and_unusual_records() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("index.cache");
    let mut originals = vec![
        entry("/", "/"),
        entry("", ""),
        entry("中文.rs", "/space dir/中文.rs"),
        entry("alias", "/actual"),
        entry("relative", "relative"),
        entry("name", "/trailing/name/"),
        entry("main.rs", "/a/main.rs"),
        entry("main.rs", "/a/main.rs"),
    ];
    originals[0].is_dir = true;
    for records in [&[][..], originals.as_slice()] {
        lokii_core::cache::save_cache(records, &path).unwrap();
        let (compact, _) = lokii_core::cache::load_compact(&path).unwrap();
        assert_eq!(compact.len(), records.len());
        for (ordinal, original) in records.iter().enumerate() {
            assert_eq!(&compact.materialize(EntryId(ordinal)).unwrap(), original);
        }
        compact.validate().unwrap();
        lokii_core::cache::save_compact(&compact, None, &path).unwrap();
        let (restored, checkpoint) = lokii_core::cache::load_compact(&path).unwrap();
        assert_eq!(checkpoint, None);
        for (ordinal, original) in records.iter().enumerate() {
            assert_eq!(&restored.materialize(EntryId(ordinal)).unwrap(), original);
        }
    }
}

#[test]
fn compact_scanner_preserves_legacy_records() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("main.rs"), "content").unwrap();
    std::fs::create_dir(directory.path().join("folder")).unwrap();
    let mut config = lokii_core::config::AppConfig::default();
    config.index.index_paths = vec![directory.path().to_string_lossy().into_owned()];
    config.index.exclude_paths.clear();
    let filter = std::sync::Arc::new(lokii_core::filter::CompiledFilter::from_config(&config));
    let paths = vec![directory.path().to_owned()];
    let compact = lokii_core::scanner::scan_compact(
        &paths,
        filter.clone(),
        true,
        std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    )
    .unwrap();
    let mut expected = lokii_core::scanner::scan_directories(
        &paths,
        filter,
        true,
        std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    );
    let mut actual: Vec<_> = compact.iter().map(|(_, view)| view.materialize()).collect();
    expected.sort_by(|left, right| left.path.cmp(&right.path));
    actual.sort_by(|left, right| left.path.cmp(&right.path));
    assert_eq!(actual, expected);
    assert_eq!(actual.len(), 3);
}

#[test]
fn repeated_churn_reclaims_pool_strings() {
    let mut compact = CompactIndex::from_entries([entry("base.rs", "/base/base.rs")]).unwrap();
    for cycle in 0..20 {
        compact
            .add_entries(
                (0..1000)
                    .map(|ordinal| {
                        entry(
                            &format!("new_{cycle}_{ordinal}.rs"),
                            &format!("/churn/{cycle}/new_{cycle}_{ordinal}.rs"),
                        )
                    })
                    .collect(),
            )
            .unwrap();
        compact.remove_matching(|view| view.path_starts_with("/churn/"));
        assert_eq!(compact.len(), 1);
        assert_eq!(compact.pool_counts(), (1, 1, 1, 0));
        compact.validate().unwrap();
    }
}

#[test]
fn upsert_batch_publishes_complete_record_set() {
    let mut compact = CompactIndex::from_entries([
        entry("main.rs", "/a/main.rs"),
        entry("keep.rs", "/b/keep.rs"),
    ])
    .unwrap();
    let removed = compact
        .upsert_batch(vec![entry("main.rs", "/a/main.rs")], |view| {
            view.path_equals("/a/main.rs")
        })
        .unwrap();
    assert_eq!(removed, 1);
    assert_eq!(compact.materialize(EntryId(0)).unwrap().name, "keep.rs");
    assert_eq!(compact.materialize(EntryId(1)).unwrap().name, "main.rs");
    compact.validate().unwrap();
}

#[test]
fn concurrent_readers_only_observe_complete_replacements() {
    let store = std::sync::Arc::new(lokii_core::indexer::IndexStore::new());
    store.replace(vec![entry("old.rs", "/old/old.rs"); 32]);
    let reader = store.clone();
    let worker = std::thread::spawn(move || {
        for _ in 0..100 {
            let index = reader.read();
            index.validate().unwrap();
            let names: std::collections::HashSet<_> =
                index.iter().map(|(_, view)| view.name).collect();
            assert_eq!(names.len(), 1);
        }
    });
    for ordinal in 0..100 {
        let record = if ordinal % 2 == 0 {
            entry("new.rs", "/new/new.rs")
        } else {
            entry("old.rs", "/old/old.rs")
        };
        store.replace(vec![record; 32]);
    }
    worker.join().unwrap();
}

#[test]
fn prefix_rename_preserves_neighbor_and_merges_target_parent() {
    let mut index = CompactIndex::from_entries([
        entry("main.rs", "/foo/bar/main.rs"),
        entry("keep.rs", "/foo/barista/keep.rs"),
        entry("other.rs", "/foo/new/other.rs"),
    ])
    .unwrap();
    assert_eq!(index.rename_prefix("/foo/bar", "/foo/new").unwrap(), 1);
    assert_eq!(
        index.materialize(EntryId(0)).unwrap().path,
        "/foo/new/main.rs"
    );
    assert_eq!(
        index.materialize(EntryId(1)).unwrap().path,
        "/foo/barista/keep.rs"
    );
    assert_eq!(index.pool_counts().1, 2);
    index.validate().unwrap();
}

#[test]
fn failed_cache_save_preserves_previous_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("index.cache");
    let index = CompactIndex::from_entries([entry("main.rs", "/a/main.rs")]).unwrap();
    lokii_core::cache::save_compact(&index, None, &path).unwrap();
    let previous = std::fs::read(&path).unwrap();
    std::fs::create_dir(path.with_extension("tmp")).unwrap();
    assert!(lokii_core::cache::save_compact(&index, None, &path).is_err());
    assert_eq!(std::fs::read(path).unwrap(), previous);
}

#[test]
fn name_iteration_preserves_record_order_and_duplicates() {
    let index = CompactIndex::from_entries([
        entry("main.rs", "/a/main.rs"),
        entry("main.rs", "/b/main.rs"),
        entry("other.rs", "/b/other.rs"),
    ])
    .unwrap();
    assert_eq!(
        index
            .names()
            .map(|(id, name)| (id.0, name))
            .collect::<Vec<_>>(),
        vec![(0, "main.rs"), (1, "main.rs"), (2, "other.rs")]
    );
}

#[test]
fn event_batch_renames_removes_and_upserts_atomically() {
    let mut index = CompactIndex::from_entries([
        entry("main.rs", "/a/old/main.rs"),
        entry("gone.rs", "/gone/gone.rs"),
        entry("keep.rs", "/keep/keep.rs"),
    ])
    .unwrap();
    let result = index
        .apply_event_batch(
            &[("/a/old".into(), "/a/new".into())],
            &[],
            &["/gone".into()],
            vec![entry("new.rs", "/a/new/new.rs")],
        )
        .unwrap();
    assert_eq!(result, (1, 1));
    assert_eq!(
        index.materialize(EntryId(0)).unwrap().path,
        "/a/new/main.rs"
    );
    assert_eq!(index.materialize(EntryId(1)).unwrap().path, "/keep/keep.rs");
    assert_eq!(index.materialize(EntryId(2)).unwrap().path, "/a/new/new.rs");
    index.validate().unwrap();
}

/// The pool masks that gate the record walk are a superset of the paths the
/// batch names. A mask that misses an entry would leave deleted files indexed
/// without any error, so every remove has to still land.
#[test]
fn many_removes_drop_every_record_under_them() {
    let mut index = CompactIndex::from_entries(
        (0..200)
            .map(|ordinal| {
                entry(
                    &format!("file_{ordinal:03}.rs"),
                    &format!("/tree/dir_{:02}/file_{ordinal:03}.rs", ordinal % 50),
                )
            })
            .chain([entry("keep.rs", "/keep/keep.rs")]),
    )
    .unwrap();
    let removes: Vec<String> = (0..50).map(|n| format!("/tree/dir_{n:02}")).collect();
    let (_, removed) = index
        .apply_event_batch(&[], &[], &removes, Vec::new())
        .unwrap();
    assert_eq!(removed, 200);
    assert_eq!(index.len(), 1);
    assert_eq!(index.materialize(EntryId(0)).unwrap().path, "/keep/keep.rs");
    index.validate().unwrap();
}

/// A remove covers a subtree, not a name prefix: `/x/ab` must leave `/x/abc`.
#[test]
fn remove_spares_a_sibling_that_only_shares_the_name_prefix() {
    let mut index = CompactIndex::from_entries([
        entry("a.rs", "/x/ab/a.rs"),
        entry("b.rs", "/x/abc/b.rs"),
        entry("c.rs", "/x/ab/c.rs"),
    ])
    .unwrap();
    let (_, removed) = index
        .apply_event_batch(&[], &[], &["/x/ab".into()], Vec::new())
        .unwrap();
    assert_eq!(removed, 2);
    assert_eq!(index.len(), 1);
    assert_eq!(index.materialize(EntryId(0)).unwrap().path, "/x/abc/b.rs");
    index.validate().unwrap();
}

/// Records whose path cannot be split into parent and name live in the raw
/// pool instead, and the masks have to cover that pool too.
#[test]
fn removes_reach_raw_path_records() {
    let mut index = CompactIndex::from_entries([
        entry("a/b.rs", "/x/a/b.rs"),
        entry("keep.rs", "/keep/keep.rs"),
    ])
    .unwrap();
    let (_, removed) = index
        .apply_event_batch(&[], &[], &["/x/a".into()], Vec::new())
        .unwrap();
    assert_eq!(removed, 1);
    assert_eq!(index.len(), 1);
    assert_eq!(index.materialize(EntryId(0)).unwrap().path, "/keep/keep.rs");
    index.validate().unwrap();
}

/// A batch that also carries creates does key work, which the path masks do not
/// cover; those sides fall back to the full walk rather than skipping records.
#[test]
fn removes_beside_creates_still_drop_every_match() {
    let mut index = CompactIndex::from_entries([
        entry("old.rs", "/a/old.rs"),
        entry("gone.rs", "/g/gone.rs"),
        entry("keep.rs", "/k/keep.rs"),
    ])
    .unwrap();
    let (added, removed) = index
        .apply_event_batch(
            &[],
            &[],
            &["/g".into()],
            vec![
                entry("new.rs", "/n/new.rs"),
                entry("alias.rs", "/a/alias.rs"),
            ],
        )
        .unwrap();
    assert_eq!((added, removed), (2, 1));
    let paths: Vec<String> = (0..index.len())
        .map(|ordinal| index.materialize(EntryId(ordinal)).unwrap().path)
        .collect();
    assert_eq!(
        paths,
        ["/a/old.rs", "/k/keep.rs", "/n/new.rs", "/a/alias.rs"]
    );
    index.validate().unwrap();
}

/// `excluded` shares the walk and the masks, so it needs the same guarantee.
#[test]
fn many_excluded_paths_drop_every_record_under_them() {
    let mut index = CompactIndex::from_entries(
        (0..200)
            .map(|ordinal| {
                entry(
                    &format!("file_{ordinal:03}.rs"),
                    &format!("/tree/dir_{:02}/file_{ordinal:03}.rs", ordinal % 50),
                )
            })
            .chain([entry("keep.rs", "/keep/keep.rs")]),
    )
    .unwrap();
    let excluded: Vec<String> = (0..50).map(|n| format!("/tree/dir_{n:02}")).collect();
    // The returned count only covers `removes`; `excluded` drops are not counted.
    index
        .apply_event_batch(&[], &excluded, &[], Vec::new())
        .unwrap();
    assert_eq!(index.len(), 1);
    assert_eq!(index.materialize(EntryId(0)).unwrap().path, "/keep/keep.rs");
    index.validate().unwrap();
}
