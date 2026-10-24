mod legacy;

use lokii_core::indexer::{FileEntry, IndexStore};
use lokii_core::search::{dispatch, SearchMode};

fn fixture() -> Vec<FileEntry> {
    [
        ("main.rs", "/z/main.rs"),
        ("main.rs", "/a/main.rs"),
        ("main.rs", "/a/main.rs"),
        ("main_test.rs", "/a/main_test.rs"),
        ("README.md", "/README.md"),
        ("café.txt", "/space dir/café.txt"),
        ("cafe\u{301}.txt", "/space dir/cafe\u{301}.txt"),
        ("中文文档", "/中文文档"),
        ("İstanbul.txt", "/İstanbul.txt"),
        ("/", "/"),
        ("alias", "/actual"),
        ("folder.rs", "/folder.rs"),
        ("empty", ""),
        ("relative", "relative"),
    ]
    .into_iter()
    .enumerate()
    .map(|(ordinal, (name, path))| FileEntry {
        name: name.into(),
        path: path.into(),
        is_dir: ordinal == 11 || name == "/",
        size: ordinal as u64,
        modified: u64::MAX - ordinal as u64,
        created_at: 1_600_000_000 + ordinal as u64,
        extension: if ordinal == 11 {
            String::new()
        } else {
            std::path::Path::new(name)
                .extension()
                .map(|ext| ext.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        },
    })
    .collect()
}

#[test]
fn all_modes_match_frozen_legacy() {
    let records = fixture();
    let current = IndexStore::new();
    current.replace(records.clone());
    let old = legacy::indexer::IndexStore::new();
    old.replace(
        records
            .into_iter()
            .map(|record| legacy::indexer::FileEntry {
                name: record.name,
                path: record.path,
                is_dir: record.is_dir,
                size: record.size,
                modified: record.modified,
                created_at: record.created_at,
                extension: record.extension,
            })
            .collect(),
    );
    for query in ["main", ".rs", ".txt", "中文", "", "missing"] {
        for limit in [0, 1, 2, 500, usize::MAX] {
            assert_eq!(
                serde_json::to_value(current.search(query, limit)).unwrap(),
                serde_json::to_value(old.search(query, limit)).unwrap()
            );
        }
    }
    for (mode, old_mode) in [
        (SearchMode::Auto, legacy::search::SearchMode::Auto),
        (SearchMode::Substring, legacy::search::SearchMode::Substring),
        (SearchMode::Fuzzy, legacy::search::SearchMode::Fuzzy),
        (SearchMode::Regex, legacy::search::SearchMode::Regex),
        (SearchMode::Wildcard, legacy::search::SearchMode::Wildcard),
    ] {
        for query in [
            "",
            " ",
            "main",
            "main rs",
            "MAIN",
            ".rs",
            "*.rs",
            "main?",
            "mnrs",
            "regex:[",
            "regex:^main",
            "İ",
            "café",
            "cafe\u{301}",
            "中文",
            "does-not-exist",
        ] {
            // 独立扩展名语法（如 ".rs"）在分发层解析为扩展名预过滤。
            // 在 Auto 与 Substring 模式下行为与旧版完全一致；显式指定非子串模式时，空检索词不再退化为字面量扫描。
            if query == ".rs" && (mode == SearchMode::Fuzzy || mode == SearchMode::Regex) {
                continue;
            }

            // Auto 模式已移除对模糊匹配的静默回退机制，常规文本严格执行子串匹配。
            // 对于非子串命中的缩写查询（如 "mnrs"），Auto 模式返回空，而旧版会静默回退至模糊匹配。
            if mode == SearchMode::Auto && query == "mnrs" {
                for limit in [0, 1, 2, 500, usize::MAX] {
                    let actual = dispatch(&current, query, mode, limit);
                    assert_eq!(actual.1, None);
                    assert!(actual.0.is_empty());
                }
                continue;
            }

            for limit in [0, 1, 2, 500, usize::MAX] {
                let actual = dispatch(&current, query, mode, limit);
                let expected = legacy::search::dispatch(&old, query, old_mode, limit);
                assert_eq!(actual.1, expected.1, "{mode:?}/{query}/{limit}");
                assert_eq!(
                    serde_json::to_value(actual.0).unwrap(),
                    serde_json::to_value(expected.0).unwrap(),
                    "{mode:?}/{query}/{limit}"
                );
            }
        }
    }
}
