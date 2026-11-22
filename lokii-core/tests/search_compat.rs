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
            let actual = current.search(query, limit);
            let expected = old.search(query, limit);
            if matches!(query, "main" | ".rs" | ".txt" | "." | "中文") {
                // 双层排序模型与无截断浅层优先会改变局部 tie-break 与层级映射，
                // 在此核验召回数量一致、MatchScore 单调降序及足额 limit 下命中集合完全一致。
                assert_eq!(actual.len(), expected.len(), "IndexStore::{query}/{limit}");
                assert!(actual.windows(2).all(|w| w[0].score >= w[1].score));
                if limit >= 500 {
                    let mut act_paths: Vec<_> = actual.iter().map(|r| r.entry.path.as_str()).collect();
                    let mut exp_paths: Vec<_> = expected.iter().map(|r| r.entry.path.as_str()).collect();
                    act_paths.sort_unstable();
                    exp_paths.sort_unstable();
                    assert_eq!(act_paths, exp_paths, "IndexStore set mismatch::{query}/{limit}");
                }
            } else {
                assert_eq!(
                    serde_json::to_value(actual).unwrap(),
                    serde_json::to_value(expected).unwrap()
                );
            }
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
            // 独立扩展名语法（如 ".rs"）在分发层解析为扩展名预过滤；正则模式按字面量处理，不参与扩展名预过滤。
            if query == ".rs" && mode == SearchMode::Regex {
                continue;
            }

            // 模糊模式是常规检索的放宽：结果以同查询的子串结果为前缀，其后只追加去重的跳字命中。
            if mode == SearchMode::Fuzzy {
                for limit in [0, 1, 2, 500, usize::MAX] {
                    let (fuzzy, err) = dispatch(&current, query, mode, limit);
                    let (substring, _) = dispatch(&current, query, SearchMode::Substring, limit);
                    assert_eq!(err, None);
                    assert!(fuzzy.len() <= limit, "{query}/{limit}");
                    let paths: Vec<_> = fuzzy.iter().map(|r| r.entry.path.as_str()).collect();
                    assert_eq!(
                        serde_json::to_value(&fuzzy[..substring.len()]).unwrap(),
                        serde_json::to_value(&substring).unwrap(),
                        "{query}/{limit}"
                    );
                    for r in &fuzzy[substring.len()..] {
                        assert_eq!(r.search_mode, SearchMode::Fuzzy, "{query}/{limit}");
                        assert!(!paths[..substring.len()].contains(&r.entry.path.as_str()), "{query}/{limit}");
                    }
                }
                if query == "MAIN" {
                    let (smart, _, err) =
                        dispatch_with_options(&current, query, mode, CaseSensitivity::Auto, 500);
                    assert_eq!(err, None);
                    assert!(smart.is_empty(), "Smart Case 大写查询不得被跳字补充放行：{smart:?}");
                    let (insensitive, _, err) =
                        dispatch_with_options(&current, query, mode, CaseSensitivity::Insensitive, 500);
                    assert_eq!(err, None);
                    assert!(!insensitive.is_empty(), "Insensitive 下的命中必须保留");
                }
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
                    assert_eq!(insensitive.len(), expected.0.len(), "{mode:?}/{query}/{limit}");
                    assert!(
                        insensitive.iter().all(|r| r.score >= 1e-6 && r.score <= 1.0),
                        "{mode:?}/{query}/{limit}"
                    );
                    assert!(insensitive.windows(2).all(|w| w[0].score >= w[1].score));
                    if limit >= 500 {
                        let mut act_paths: Vec<_> =
                            insensitive.iter().map(|r| r.entry.path.as_str()).collect();
                        let mut exp_paths: Vec<_> =
                            expected.0.iter().map(|r| r.entry.path.as_str()).collect();
                        act_paths.sort_unstable();
                        exp_paths.sort_unstable();
                        assert_eq!(act_paths, exp_paths, "{mode:?}/{query}/{limit}");
                    }
                }
                continue;
            }

            // Auto/Substring/Regex 下禁用跨越组合附加符的非 NFC 子串截断：
            // 纯 ASCII "cafe" 不再错误截半命中 NFD "cafe\u{301}.txt"；Fuzzy 模式仍通过去重音正常匹配。
            if query == "cafe" && mode != SearchMode::Fuzzy {
                for limit in [0, 1, 2, 500, usize::MAX] {
                    let actual = dispatch(&current, query, mode, limit);
                    assert_eq!(actual.1, None);
                    assert!(actual.0.is_empty(), "{mode:?}/{query}/{limit}: {actual:?}");
                }
                continue;
            }

            // NFC "café" 与 NFD "cafe\u{301}" 在所有检索模式下保持严格双向等价召回，
            // 并按各自原始字符串的组合字符簇边界返回安全的 UTF-8 高亮区间。
            if (query == "café" || query == "cafe\u{301}") && mode != SearchMode::Fuzzy {
                for limit in [0, 1, 2, 500, usize::MAX] {
                    let (nfc_res, nfc_err) = dispatch(&current, "café", mode, limit);
                    let (nfd_res, nfd_err) = dispatch(&current, "cafe\u{301}", mode, limit);
                    assert_eq!(nfc_err, None);
                    assert_eq!(nfd_err, None);
                    assert_eq!(
                        serde_json::to_value(&nfc_res).unwrap(),
                        serde_json::to_value(&nfd_res).unwrap(),
                        "NFC/NFD parity failed for {mode:?}/{limit}"
                    );
                    if mode == SearchMode::Wildcard {
                        assert!(nfc_res.is_empty());
                    } else if limit == usize::MAX || limit >= 3 {
                        let names: Vec<&str> = nfc_res.iter().map(|r| r.entry.name.as_str()).collect();
                        assert!(names.contains(&"café.txt") && names.contains(&"cafe\u{301}.txt") && names.contains(&"x.café"));
                    }
                    for item in &nfc_res {
                        for &(s, e) in &item.match_positions {
                            assert!(item.entry.name.get(s..e).is_some(), "invalid UTF-8 highlight ({s}, {e}) on {:?}", item.entry.name);
                        }
                    }
                }
                continue;
            }

            // "İ" (U+0130 大写带点 I)：Auto/Substring 在 Smart Case 触发时匹配原始 2 字节 (0..2)；
            // CaseSensitivity::Insensitive 下虽然折叠为 3 字节 "i\u{0307}"，现按扩展组合字符簇反投影回原始 (0..2)，
            // 彻底消除了折叠膨胀越界问题，因此两者高亮结果一致收敛为 [(0, 2)]。
            if query == "İ" && (mode == SearchMode::Auto || mode == SearchMode::Substring) {
                for limit in [0, 1, 2, 500, usize::MAX] {
                    let actual = dispatch(&current, query, mode, limit);
                    assert_eq!(actual.1, None);
                    let (insensitive, _, err) =
                        dispatch_with_options(&current, query, mode, CaseSensitivity::Insensitive, limit);
                    assert_eq!(err, None);
                    if limit > 0 {
                        assert_eq!(actual.0[0].match_positions, vec![(0, 2)]);
                        assert_eq!(insensitive[0].entry.name, "İstanbul.txt");
                        assert_eq!(insensitive[0].match_positions, vec![(0, 2)]);
                    } else {
                        assert!(actual.0.is_empty());
                        assert!(insensitive.is_empty());
                    }
                }
                continue;
            }

            // 正则模式全命中区间合并与双层复合排序：对于单字符通配正则（如 "."），
            // 全量匹配区间合并为 (0, name.len()) 并按路径深度、隐藏降权与名字长度排序。
            if mode == SearchMode::Regex && query == "." {
                for limit in [0, 1, 2, 500, usize::MAX] {
                    let actual = dispatch(&current, query, mode, limit);
                    let expected = legacy::search::dispatch(&old, query, old_mode, limit);
                    assert_eq!(actual.1, expected.1, "{mode:?}/{query}/{limit}");
                    assert_eq!(actual.0.len(), expected.0.len(), "{mode:?}/{query}/{limit}");
                    assert!(actual.0.windows(2).all(|w| w[0].score >= w[1].score));
                    for act in &actual.0 {
                        assert!(act.score >= 1e-6 && act.score <= 1.0);
                        assert_eq!(act.match_positions, vec![(0, act.entry.name.len())]);
                    }
                    if limit >= 500 {
                        let mut act_paths: Vec<_> =
                            actual.0.iter().map(|r| r.entry.path.as_str()).collect();
                        let mut exp_paths: Vec<_> =
                            expected.0.iter().map(|r| r.entry.path.as_str()).collect();
                        act_paths.sort_unstable();
                        exp_paths.sort_unstable();
                        assert_eq!(act_paths, exp_paths, "{mode:?}/{query}/{limit}");
                    }
                }
                continue;
            }

            for limit in [0, 1, 2, 500, usize::MAX] {
                let actual = dispatch(&current, query, mode, limit);
                let expected = legacy::search::dispatch(&old, query, old_mode, limit);
                assert_eq!(actual.1, expected.1, "{mode:?}/{query}/{limit}");
                assert_eq!(actual.0.len(), expected.0.len(), "{mode:?}/{query}/{limit}");
                assert!(
                    actual
                        .0
                        .iter()
                        .all(|r| r.score >= 1e-6 && r.score <= 1.0),
                    "{mode:?}/{query}/{limit}"
                );
                assert!(actual.0.windows(2).all(|w| w[0].score >= w[1].score));
                for item in &actual.0 {
                    for &(s, e) in &item.match_positions {
                        assert!(
                            item.entry.name.get(s..e).is_some(),
                            "invalid UTF-8 highlight ({s}, {e}) on {:?} for {mode:?}/{query}",
                            item.entry.name
                        );
                    }
                }
                if limit >= 500 {
                    let is_wildcard_query = mode == SearchMode::Wildcard
                        || (mode == SearchMode::Auto
                            && (query.contains('*') || query.contains('?')));
                    if is_wildcard_query {
                        // 通配符现精准高亮非通配字面片段，核验完整命中路径集合一致
                        let mut act_paths: Vec<_> =
                            actual.0.iter().map(|r| r.entry.path.as_str()).collect();
                        let mut exp_paths: Vec<_> =
                            expected.0.iter().map(|r| r.entry.path.as_str()).collect();
                        act_paths.sort_unstable();
                        exp_paths.sort_unstable();
                        assert_eq!(act_paths, exp_paths, "{mode:?}/{query}/{limit}");
                    } else {
                        let mut act_items: Vec<_> = actual
                            .0
                            .iter()
                            .map(|r| (r.entry.path.clone(), r.match_positions.clone()))
                            .collect();
                        let mut exp_items: Vec<_> = expected
                            .0
                            .iter()
                            .map(|r| (r.entry.path.clone(), r.match_positions.clone()))
                            .collect();
                        act_items.sort();
                        exp_items.sort();
                        assert_eq!(act_items, exp_items, "{mode:?}/{query}/{limit}");
                    }
                }
            }
        }
    }
}
