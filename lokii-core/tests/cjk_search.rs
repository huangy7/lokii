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
