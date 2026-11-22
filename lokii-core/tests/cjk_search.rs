use lokii_core::indexer::{FileEntry, IndexStore};
use lokii_core::search::filter::CompiledRecordFilter;
use lokii_core::search::fuzzy;
use lokii_core::search::path_search;
use lokii_core::search::substring::search;
use lokii_core::search::wildcard;

fn make_entry(name: &str, path: &str) -> FileEntry {
    make_entry_with_modified(name, path, 1700000000)
}

fn make_entry_with_modified(name: &str, path: &str, modified: u64) -> FileEntry {
    FileEntry {
        name: name.to_string(),
        path: path.to_string(),
        is_dir: false,
        size: 1024,
        modified,
        created_at: 0,
        extension: std::path::Path::new(name)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase(),
    }
}

#[test]
fn test_simplified_query_matches_traditional_files() {
    let entries = vec![
        make_entry("開發規範.md", "/Users/test/Projects/開發規範.md"),
        make_entry("備忘錄.txt", "/Users/test/Documents/備忘錄.txt"),
        make_entry("2026年財務預算表.xlsx", "/Users/test/Documents/2026年財務預算表.xlsx"),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    // 1. 简体单字/词组搜繁体文件名
    let results = search(&store, "开发", &filter, 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.name, "開發規範.md");
    assert!(!results[0].match_positions.is_empty());
    let (s, e) = results[0].match_positions[0];
    assert_eq!(&results[0].entry.name[s..e], "開發");

    // 2. 简体搜财务预算
    let results = search(&store, "财务", &filter, 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.name, "2026年財務預算表.xlsx");
    let (s, e) = results[0].match_positions[0];
    assert_eq!(&results[0].entry.name[s..e], "財務");

    // 3. 多关键词联合检索
    let results = search(&store, "预算 表", &filter, 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.name, "2026年財務預算表.xlsx");
}

#[test]
fn test_traditional_query_matches_simplified_files() {
    let entries = vec![
        make_entry("开发规范.md", "/Users/test/Projects/开发规范.md"),
        make_entry("备忘录.txt", "/Users/test/Documents/备忘录.txt"),
        make_entry("2026年财务预算表.xlsx", "/Users/test/Documents/2026年财务预算表.xlsx"),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    // 1. 繁体单字/词组搜简体文件名
    let results = search(&store, "開發", &filter, 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.name, "开发规范.md");
    assert!(!results[0].match_positions.is_empty());
    let (s, e) = results[0].match_positions[0];
    assert_eq!(&results[0].entry.name[s..e], "开发");

    // 2. 繁体搜备忘录
    let results = search(&store, "備忘錄", &filter, 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.name, "备忘录.txt");
    let (s, e) = results[0].match_positions[0];
    assert_eq!(&results[0].entry.name[s..e], "备忘录");
}

#[test]
fn test_pinyin_search_on_traditional_files() {
    let entries = vec![
        make_entry("開發規範.md", "/Users/test/Projects/開發規範.md"),
        make_entry("備忘錄.txt", "/Users/test/Documents/備忘錄.txt"),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    // 1. 首字母匹配繁体文件名
    let results = search(&store, "kfgf", &filter, 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.name, "開發規範.md");
    assert!(!results[0].match_positions.is_empty());

    let results = search(&store, "bwl", &filter, 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.name, "備忘錄.txt");

    // 2. 全拼匹配繁体文件名
    let results = search(&store, "kaifa", &filter, 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.name, "開發規範.md");

    let results = search(&store, "beiwanglu", &filter, 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.name, "備忘錄.txt");
}

#[test]
fn test_fuzzy_and_wildcard_cjk_interoperability() {
    let entries = vec![
        make_entry("開發規範.md", "/Users/test/Projects/開發規範.md"),
        make_entry("系统设置指南.pdf", "/Users/test/Docs/系统设置指南.pdf"),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    // 1. 模糊匹配：简体搜繁体并校验高亮区间非空
    let fuzzy_results = fuzzy::search(&store, "开发规范", &filter, 10, false);
    assert!(!fuzzy_results.is_empty());
    assert_eq!(fuzzy_results[0].entry.name, "開發規範.md");
    assert!(!fuzzy_results[0].match_positions.is_empty());

    // 2. 模糊匹配：繁体搜简体
    let fuzzy_results = fuzzy::search(&store, "系統設置", &filter, 10, false);
    assert!(!fuzzy_results.is_empty());
    assert_eq!(fuzzy_results[0].entry.name, "系统设置指南.pdf");

    // 3. 通配符匹配：简体搜繁体
    let wildcard_results = wildcard::search(&store, "*开发*", &filter, 10);
    assert!(!wildcard_results.is_empty());
    assert_eq!(wildcard_results[0].entry.name, "開發規範.md");

    // 4. 路径检索：繁体搜简体
    let query = lokii_core::search::path_query::PathQuery::parse("Docs/設置").unwrap();
    let path_results = path_search::search(&store, &query, &filter, 10);
    assert!(!path_results.is_empty());
    assert_eq!(path_results[0].entry.name, "系统设置指南.pdf");
}

#[test]
fn test_root_raw_path_search_with_cjk_and_case() {
    let entries = vec![
        make_entry("開發手冊.txt", "/開發手冊.txt"),
        make_entry("Übung_01.pdf", "/Übung_01.pdf"),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    // 1. 简体搜根路径繁体文件
    let query = lokii_core::search::path_query::PathQuery::parse("/开发").unwrap();
    let path_results = path_search::search(&store, &query, &filter, 10);
    assert_eq!(path_results.len(), 1);
    assert_eq!(path_results[0].entry.name, "開發手冊.txt");

    // 2. 小写搜根路径非 ASCII 大写文件
    let query = lokii_core::search::path_query::PathQuery::parse("/übung").unwrap();
    let path_results = path_search::search(&store, &query, &filter, 10);
    assert_eq!(path_results.len(), 1);
    assert_eq!(path_results[0].entry.name, "Übung_01.pdf");
}

#[test]
fn test_cjk_fold_char_aligns_with_fold_str() {
    assert_eq!(lokii_core::cjk::fold_char('É'), 'é');
    assert_eq!(lokii_core::cjk::fold_char('開'), '开');
    assert_eq!(lokii_core::cjk::fold_char('A'), 'a');
}

#[test]
fn test_path_search_cjk_and_memoized_scoring() {
    let entries = vec![
        make_entry("開發手冊.md", "/Users/test/Projects/開發手冊.md"),
        make_entry("開發手冊.md", "/Users/test/Archive/開發手冊.md"),
        make_entry("说明文档.docx", "/Users/test/Projects/说明文档.docx"),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    // 1. 简体搜繁体路径
    let query = lokii_core::search::path_query::PathQuery::parse("Projects/开发").unwrap();
    let results = path_search::search(&store, &query, &filter, 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.path, "/Users/test/Projects/開發手冊.md");

    // 2. 拼音搜繁体路径
    let query = lokii_core::search::path_query::PathQuery::parse("Projects/kaifa").unwrap();
    let results = path_search::search(&store, &query, &filter, 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.path, "/Users/test/Projects/開發手冊.md");
}

#[test]
fn test_unicode_nfd_nfc_cross_matching_across_engines() {
    use lokii_core::search::{dispatch_with_options, CaseSensitivity, SearchMode};

    let nfd_cafe = "cafe\u{0301}.txt";
    let nfc_cafe = "café.txt";
    let nfd_hangul = "\u{1112}\u{1161}\u{11AB}\u{1100}\u{1173}\u{11AF}.txt";
    let mixed_name = "測Cafe\u{0301}試.md";

    let entries = vec![
        make_entry(nfd_cafe, "/Users/test/nfd/cafe\u{0301}.txt"),
        make_entry(nfc_cafe, "/Users/test/nfc/café.txt"),
        make_entry(
            nfd_hangul,
            "/Users/test/\u{1112}\u{1161}\u{11AB}\u{1100}\u{1173}\u{11AF}.txt",
        ),
        make_entry(mixed_name, "/Users/test/測Cafe\u{0301}試.md"),
    ];

    let store = IndexStore::new();
    store.replace(entries);

    // NFC 查询跨越子串、通配符、正则、模糊、析取以及 IndexStore 原生查询命中 NFD 文件名
    for mode in [
        SearchMode::Auto,
        SearchMode::Substring,
        SearchMode::Fuzzy,
    ] {
        let (res, _, err) =
            dispatch_with_options(&store, "café", mode, CaseSensitivity::Insensitive, 10);
        assert!(err.is_none(), "mode={mode:?} error={err:?}");
        let names: Vec<&str> = res.iter().map(|r| r.entry.name.as_str()).collect();
        assert!(
            names.contains(&nfd_cafe) && names.contains(&nfc_cafe) && names.contains(&mixed_name),
            "mode={mode:?} unexpected matches: {names:?}"
        );
        for item in &res {
            assert!(!item.match_positions.is_empty(), "missing highlights for {}", item.entry.name);
            for &(s, e) in &item.match_positions {
                assert!(item.entry.name.get(s..e).is_some(), "invalid UTF-8 slice ({s}, {e})");
            }
        }
        if let Some(nfd_item) = res.iter().find(|r| r.entry.name == nfd_cafe) {
            assert_eq!(nfd_item.match_positions, vec![(0, 6)]);
        }
    }

    // Wildcard 与 Regex 同样应支持 NFC/NFD 双向命中且保留完整组合字符簇高亮
    let (wc_res, _, wc_err) =
        dispatch_with_options(&store, "*café*", SearchMode::Wildcard, CaseSensitivity::Insensitive, 10);
    assert!(wc_err.is_none());
    assert_eq!(wc_res.len(), 3);

    let (re_res, _, re_err) = dispatch_with_options(
        &store,
        "regex:cafe\u{0301}",
        SearchMode::Regex,
        CaseSensitivity::Insensitive,
        10,
    );
    assert!(re_err.is_none());
    assert_eq!(re_res.len(), 3);
    let nfc_item = re_res.iter().find(|r| r.entry.name == nfc_cafe).unwrap();
    assert_eq!(nfc_item.match_positions, vec![(0, "café".len())]);

    // NFD 韩文字母组合应被 NFC 韩文音节 "한글" 在全引擎下双向匹配
    for (query, mode) in [
        ("한글", SearchMode::Substring),
        ("*한글*", SearchMode::Wildcard),
        ("한글", SearchMode::Fuzzy),
        ("regex:한글", SearchMode::Regex),
        ("한글 | 不存在", SearchMode::Auto),
    ] {
        let (res, _, err) =
            dispatch_with_options(&store, query, mode, CaseSensitivity::Insensitive, 10);
        assert!(err.is_none(), "query={query} mode={mode:?} err={err:?}");
        assert_eq!(res.len(), 1, "query={query} mode={mode:?} res={res:?}");
        assert_eq!(res[0].entry.name, nfd_hangul);
        // 全引擎（含通配符字面片段高亮）均应精准投影到 NFD 韩文双音节的完整 18 字节 Jamo 组合簇边界
        let expected_end = "\u{1112}\u{1161}\u{11AB}\u{1100}\u{1173}\u{11AF}".len();
        assert_eq!(res[0].match_positions, vec![(0, expected_end)], "query={query} mode={mode:?}");
    }

    // 直接调用模糊检索器验证 NFD 韩文字母双音节高亮分别覆盖各自完整 Jamo 组合簇
    let filter = CompiledRecordFilter::default();
    let direct_fuzzy = fuzzy::search(&store, "한글", &filter, 10, false);
    assert_eq!(direct_fuzzy.len(), 1);
    assert_eq!(direct_fuzzy[0].entry.name, nfd_hangul);
    assert_eq!(direct_fuzzy[0].match_positions, vec![(0, 9), (9, 18)]);

    // 繁简 + NFD 混合查询高亮映射检查
    let (mixed_res, _, _) = dispatch_with_options(
        &store,
        "测café试",
        SearchMode::Substring,
        CaseSensitivity::Insensitive,
        10,
    );
    assert_eq!(mixed_res.len(), 1);
    assert_eq!(mixed_res[0].entry.name, mixed_name);
    assert_eq!(mixed_res[0].match_positions, vec![(0, "測Cafe\u{0301}試".len())]);

    // IndexStore::search 直接检索同样生效
    let basic_hits = store.search("한글", 10);
    assert_eq!(basic_hits.len(), 1);
    assert_eq!(basic_hits[0].entry.name, nfd_hangul);
}

#[test]
fn test_combining_mark_boundary_and_grapheme_cluster_highlights() {
    use lokii_core::search::{dispatch_with_options, CaseSensitivity, SearchMode};

    let nfd_title = "Cafe\u{0301}.txt";
    let turkish = "İstanbul.txt";
    let entries = vec![
        make_entry(nfd_title, "/Users/test/Cafe\u{0301}.txt"),
        make_entry(turkish, "/Users/test/İstanbul.txt"),
    ];

    let store = IndexStore::new();
    store.replace(entries);

    // 纯 "cafe"/"Cafe" 不应在精确或子串/通配模式下切半组合字符后误命中 "Cafe\u{0301}.txt"
    for mode in [SearchMode::Auto, SearchMode::Substring, SearchMode::Wildcard] {
        let query = if mode == SearchMode::Wildcard { "*cafe*" } else { "cafe" };
        let (res, _, _) = dispatch_with_options(&store, query, mode, CaseSensitivity::Insensitive, 10);
        assert!(res.is_empty(), "mode={mode:?} should not slice combining mark: {res:?}");
    }

    // 模糊搜索保留去重音语义，且第 4 簇高亮必须完整覆盖 "e\u{0301}" 组合字符簇 (3..6)
    let (fuzzy_res, _, _) =
        dispatch_with_options(&store, "cafe", SearchMode::Fuzzy, CaseSensitivity::Insensitive, 10);
    assert_eq!(fuzzy_res.len(), 1);
    assert_eq!(fuzzy_res[0].entry.name, nfd_title);
    assert_eq!(fuzzy_res[0].match_positions, vec![(0, 1), (1, 2), (2, 3), (3, 6)]);

    // 大小写敏感模式下，NFC 变体仅匹配对应大小写
    let (sens_hit, _, _) =
        dispatch_with_options(&store, "Café", SearchMode::Substring, CaseSensitivity::Sensitive, 10);
    assert_eq!(sens_hit.len(), 1);
    let (sens_miss, _, _) =
        dispatch_with_options(&store, "café", SearchMode::Substring, CaseSensitivity::Sensitive, 10);
    assert!(sens_miss.is_empty());

    // 土耳其语 'İ' (2字节) 小写折叠后膨胀为 "i\u{0307}" (3字节)，高亮需反投回原始完整字符簇 (0, 2)
    let (res_upper, _, _) =
        dispatch_with_options(&store, "İ", SearchMode::Substring, CaseSensitivity::Insensitive, 10);
    assert_eq!(res_upper.len(), 1);
    assert_eq!(res_upper[0].match_positions, vec![(0, 2)]);

    let (res_lower, _, _) =
        dispatch_with_options(&store, "i", SearchMode::Substring, CaseSensitivity::Insensitive, 10);
    assert_eq!(res_lower.len(), 1);
    assert_eq!(res_lower[0].match_positions, vec![(0, 2)]);

    let (res_tail, _, _) = dispatch_with_options(
        &store,
        "stanbul",
        SearchMode::Substring,
        CaseSensitivity::Insensitive,
        10,
    );
    assert_eq!(res_tail.len(), 1);
    assert_eq!(res_tail[0].match_positions, vec![(2, 9)]);
}

#[test]
fn test_nfd_nfc_path_search_and_non_ascii_extension() {
    use lokii_core::search::{dispatch_with_options, CaseSensitivity, SearchMode};

    let entries = vec![
        make_entry("note.txt", "/Users/test/Cafe\u{0301}/note.txt"),
        make_entry("x.cafe\u{0301}", "/Users/test/x.cafe\u{0301}"),
        make_entry("y.café", "/Users/test/y.café"),
        make_entry("z.CAFÉ", "/Users/test/z.CAFÉ"),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    // 路径搜索跨越 NFD 父目录与 NFC 查询，且父目录高亮不撕裂组合字符
    let pq = lokii_core::search::path_query::PathQuery::parse("café/note").unwrap();
    let path_hits = path_search::search(&store, &pq, &filter, 10);
    assert_eq!(path_hits.len(), 1);
    assert_eq!(path_hits[0].entry.path, "/Users/test/Cafe\u{0301}/note.txt");
    assert_eq!(path_hits[0].parent_match_positions, vec![(12, 18)]);

    // 非 ASCII 扩展名与 ext: 过滤器跨越 NFD/NFC 与大小写变体
    for ext_query in [".café", ".cafe\u{0301}", "ext:café", ".café | .不存在"] {
        let (res, _, err) =
            dispatch_with_options(&store, ext_query, SearchMode::Auto, CaseSensitivity::Insensitive, 10);
        assert!(err.is_none(), "ext_query={ext_query} err={err:?}");
        let mut names: Vec<&str> = res.iter().map(|r| r.entry.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["x.cafe\u{0301}", "y.café", "z.CAFÉ"], "ext_query={ext_query}");
    }

    let mut basic_ext_hits: Vec<String> = store
        .search(".café", 10)
        .into_iter()
        .map(|r| r.entry.name)
        .collect();
    basic_ext_hits.sort_unstable();
    assert_eq!(basic_ext_hits, vec!["x.cafe\u{0301}", "y.café", "z.CAFÉ"]);
}
