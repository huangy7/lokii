use lokii_core::indexer::{FileEntry, IndexStore};
use lokii_core::search::top_k::TopK;

#[test]
fn stable_top_k_matches_complete_order() {
    for limit in [0, 1, 2, 5, 500, usize::MAX] {
        let names = ["main", "zeta", "main", "alpha", "main"];
        let scores = [2, 3, 2, 2, 2];
        let mut top = TopK::new(limit);
        for (ordinal, name) in names.iter().enumerate() {
            top.offer(ordinal, scores[ordinal], name);
        }
        let mut expected: Vec<_> = (0..names.len()).collect();
        expected.sort_by(|left, right| {
            scores[*right]
                .cmp(&scores[*left])
                .then_with(|| names[*left].cmp(names[*right]))
                .then(left.cmp(right))
        });
        expected.truncate(limit);
        assert_eq!(
            top.finish()
                .into_iter()
                .map(|candidate| candidate.ordinal)
                .collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn worst_pruning_matches_unpruned_with_score_and_name_ties() {
    // 构造多组同分与字典序平手用例，验证上界剪枝与全量堆写入结果完全一致
    let items: [(&str, i64, i64, usize); 10] = [
        ("delta", 100, 120, 9),
        ("beta", 100, 100, 5),
        ("gamma", 100, 100, 2),
        ("alpha", 90, 95, 7),
        ("beta", 100, 110, 3),
        ("beta", 80, 100, 1), // 上界虽为 100 且同名，但序号更小，不可误剪；实得 80 会落选
        ("omega", 90, 100, 0), // upper == worst.score && name > worst.name() -> 可安全剪枝
        ("alpha", 110, 110, 8),
        ("beta", 100, 100, 4), // 同分同名同上界但序号 4 < 堆顶 5，必须被纳入
        ("epsilon", 50, 90, 6),
    ];

    for limit in [0, 1, 2, 3, 5, 10] {
        let mut unpruned = TopK::new(limit);
        let mut pruned = TopK::new(limit);

        if limit == 0 {
            assert!(pruned.worst().is_none());
        }

        for &(name, actual, upper, ordinal) in &items {
            unpruned.offer(ordinal, actual, name);

            if let Some(worst) = pruned.worst() {
                if upper < worst.score || (upper == worst.score && name > worst.name()) {
                    continue;
                }
            }
            pruned.offer(ordinal, actual, name);
        }

        if limit > 0 && items.len() >= limit {
            assert!(pruned.is_full());
            assert!(pruned.worst().is_some());
        }

        let got: Vec<_> = pruned
            .finish()
            .into_iter()
            .map(|c| (c.ordinal, c.score, c.name()))
            .collect();
        let exp: Vec<_> = unpruned
            .finish()
            .into_iter()
            .map(|c| (c.ordinal, c.score, c.name()))
            .collect();
        assert_eq!(got, exp, "limit={limit} pruned top-k diverged from unpruned");
    }
}

#[test]
fn index_store_large_duplicate_bucket_does_not_truncate_shallow_hits() {
    let mut entries = Vec::with_capacity(600);
    for i in 0..550 {
        entries.push(FileEntry {
            name: "index.ts".to_string(),
            path: format!("/repo/deep/l1/l2/l3/l4/l5/p{i}/index.ts"),
            is_dir: false,
            size: 100,
            modified: 1,
            created_at: 0,
            extension: "ts".to_string(),
        });
    }
    for i in 0..50 {
        entries.push(FileEntry {
            name: "index.ts".to_string(),
            path: format!("/repo/s{i}/index.ts"),
            is_dir: false,
            size: 100,
            modified: 1,
            created_at: 0,
            extension: "ts".to_string(),
        });
    }
    let store = IndexStore::new();
    store.add_entries(entries);
    let results_50 = store.search("index.ts", 50);
    assert_eq!(results_50.len(), 50);
    assert!(
        results_50
            .iter()
            .all(|r| r.entry.path.starts_with("/repo/s")),
        "top 50 must all be shallow /repo/s*, first={:?}",
        results_50.first().map(|r| &r.entry.path)
    );

    let results_10 = store.search("index.ts", 10);
    assert_eq!(results_10.len(), 10);
    assert!(
        results_10
            .iter()
            .all(|r| r.entry.path.starts_with("/repo/s")),
        "top 10 must all be shallow /repo/s*, first={:?}",
        results_10.first().map(|r| &r.entry.path)
    );
}
