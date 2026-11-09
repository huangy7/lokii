mod legacy;

use lokii_core::indexer::{FileEntry, IndexStore};
use lokii_core::search::{dispatch, dispatch_with_options, CaseSensitivity, SearchMode};

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
        ("u.文档", "/u.文档"),
        ("x.café", "/x.café"),
        ("Makefile", "/Makefile"),
        ("foo.", "/foo."),
        ("noext", "/noext"),
        ("naïve.md", "/naïve.md"),
        ("ab", "/ab"),
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
    for query in ["main", ".rs", ".txt", ".文档", ".café", ".", "中文", "", "missing"] {
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
            ".文档",
            ".café",
            ".",
            "*.rs",
            "main?",
            "mnrs",
            "cafe",
            "naive",
            "regex:[",
            "regex:^main",
            "regex:a\\.?b",
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
            // 对于非子串命中的缩写或去重音查询（如 "mnrs"、"naive"），Auto 模式返回空，而旧版会静默回退至模糊匹配。
            if mode == SearchMode::Auto && (query == "mnrs" || query == "naive") {
                for limit in [0, 1, 2, 500, usize::MAX] {
                    let actual = dispatch(&current, query, mode, limit);
                    assert_eq!(actual.1, None);
                    assert!(actual.0.is_empty());
                }
                continue;
            }

            // 大小写控制机制：当输入大写查询词（如 "MAIN"）时，Smart Case 判定为大小写敏感，
            // 严格要求匹配大写（在测试库只有 main.rs 时返回空）；在 Insensitive 模式下保持与历史全小写等价态一致。
            if query == "MAIN" && mode != SearchMode::Fuzzy {
                for limit in [0, 1, 2, 500, usize::MAX] {
                    let actual = dispatch(&current, query, mode, limit);
                    assert_eq!(actual.1, None);
                    assert!(actual.0.is_empty());

                    let (insensitive, _, err) =
                        dispatch_with_options(&current, query, mode, CaseSensitivity::Insensitive, limit);
                    assert_eq!(err, None);
                    let expected = legacy::search::dispatch(&old, query, old_mode, limit);
                    assert_eq!(
                        serde_json::to_value(insensitive).unwrap(),
                        serde_json::to_value(expected.0).unwrap(),
                        "{mode:?}/{query}/{limit}"
                    );
                }
                continue;
            }

            // "İ" (U+0130 大写带点 I)：在 Auto/Substring 模式下触发 Smart Case 大写敏感匹配，
            // 匹配位置精确指向原始 2 字节大写字符 (0..2)；
            // 在 CaseSensitivity::Insensitive 下与历史 3 字节小写重音展开折叠态 (0..3) 一致。
            if query == "İ" && (mode == SearchMode::Auto || mode == SearchMode::Substring) {
                for limit in [0, 1, 2, 500, usize::MAX] {
                    let actual = dispatch(&current, query, mode, limit);
                    assert_eq!(actual.1, None);
                    if limit > 0 {
                        assert_eq!(actual.0[0].match_positions, vec![(0, 2)]);
                    } else {
                        assert!(actual.0.is_empty());
                    }

                    let (insensitive, _, err) =
                        dispatch_with_options(&current, query, mode, CaseSensitivity::Insensitive, limit);
                    assert_eq!(err, None);
                    let expected = legacy::search::dispatch(&old, query, old_mode, limit);
                    assert_eq!(
                        serde_json::to_value(insensitive).unwrap(),
                        serde_json::to_value(expected.0).unwrap(),
                        "{mode:?}/{query}/{limit}"
                    );
                }
                continue;
            }

            // 正则模式全命中区间合并：对于单字符通配正则（如 "."），旧版仅提取单次 find 匹配首字，
            // 现重构为 find_iter 提取全量匹配并平滑合并为连续高亮区间。
            if mode == SearchMode::Regex && query == "." {
                for limit in [0, 1, 2, 500, usize::MAX] {
                    let actual = dispatch(&current, query, mode, limit);
                    let expected = legacy::search::dispatch(&old, query, old_mode, limit);
                    assert_eq!(actual.1, expected.1, "{mode:?}/{query}/{limit}");
                    assert_eq!(actual.0.len(), expected.0.len(), "{mode:?}/{query}/{limit}");
                    for (act, exp) in actual.0.iter().zip(expected.0.iter()) {
                        assert_eq!(
                            serde_json::to_value(&act.entry).unwrap(),
                            serde_json::to_value(&exp.entry).unwrap()
                        );
                        assert_eq!(act.score, exp.score);
                        assert_eq!(act.match_positions, vec![(0, act.entry.name.len())]);
                        assert_eq!(exp.match_positions.len(), 1);
                        assert_eq!(exp.match_positions[0].0, 0);
                    }
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
