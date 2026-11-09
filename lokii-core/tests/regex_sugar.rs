use lokii_core::search::regex_sugar::{is_regex_metachar, parse_regex_sugar, RegexSugar};

#[test]
fn test_parse_regex_sugar_basic() {
    let res = parse_regex_sugar("/[a-z]+_\\d+/").expect("should parse");
    assert_eq!(
        res,
        RegexSugar {
            pattern: "[a-z]+_\\d+".to_string(),
            case_insensitive: false,
        }
    );
}

#[test]
fn test_parse_regex_sugar_case_insensitive() {
    let res = parse_regex_sugar("/^test.*\\.log$/i").expect("should parse");
    assert_eq!(res.pattern, "^test.*\\.log$");
    assert!(res.case_insensitive);
}

#[test]
fn test_parse_regex_sugar_escaped_slash() {
    let res = parse_regex_sugar("/\\/usr\\/bin/").expect("should parse escaped slashes");
    assert_eq!(res.pattern, "\\/usr\\/bin");
    assert!(!res.case_insensitive);
}

#[test]
fn test_path_not_misidentified_as_regex() {
    // 1. 绝对多级路径
    assert!(parse_regex_sugar("/usr/local/bin").is_none());
    assert!(parse_regex_sugar("/usr/local/bin/").is_none());

    // 2. 知名根目录
    assert!(parse_regex_sugar("/System/").is_none());
    assert!(parse_regex_sugar("/Applications/").is_none());
    assert!(parse_regex_sugar("/Users/").is_none());

    // 3. 纯单段无元字符单词
    assert!(parse_regex_sugar("/test/").is_none());

    // 4. 未闭合中间态
    assert!(parse_regex_sugar("/").is_none());
    assert!(parse_regex_sugar("/pattern").is_none());
    assert!(parse_regex_sugar("/pattern.*").is_none());

    // 5. 非法标志符
    assert!(parse_regex_sugar("/pattern/xyz").is_none());

    // 6. 超过 1024 字节防御
    let long_pattern = format!("/{}/", "a".repeat(1025));
    assert!(parse_regex_sugar(&long_pattern).is_none());
}

#[test]
fn test_regex_with_pipe_inside() {
    let res = parse_regex_sugar("/(foo|bar)\\.rs/").expect("should parse pipe inside regex");
    assert_eq!(res.pattern, "(foo|bar)\\.rs");
}

#[test]
fn test_is_regex_metachar() {
    for c in ['^', '$', '*', '+', '?', '(', ')', '[', ']', '{', '}', '|', '\\', '.'] {
        assert!(is_regex_metachar(c), "expected '{c}' to be regex metachar");
    }
    for c in ['a', 'z', '0', '9', '/', '_', '-', ' ', '@', '#'] {
        assert!(!is_regex_metachar(c), "expected '{c}' not to be regex metachar");
    }
}

use lokii_core::indexer::{FileEntry, IndexStore};
use lokii_core::search::regex_search;
use lokii_core::search::CompiledRecordFilter;

fn make_entry(name: &str, path: &str) -> FileEntry {
    FileEntry {
        name: name.to_string(),
        path: path.to_string(),
        is_dir: false,
        size: 1024,
        modified: 1700000000,
        created_at: 0,
        extension: std::path::Path::new(name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default(),
    }
}

#[test]
fn test_regex_search_capture_groups_and_highlights() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry("report_2026_final.pdf", "/docs/report_2026_final.pdf"),
        make_entry("unrelated.txt", "/docs/unrelated.txt"),
    ]);

    let filter = CompiledRecordFilter::default();
    let read = store.read();

    // 显式捕获组高亮测试：仅高亮数字部分
    let (results, err) =
        regex_search::search_index(&read, "report_(\\d+)_final", &filter, 10, false);
    assert!(err.is_none());
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.name, "report_2026_final.pdf");
    // 捕获组 1 为 "2026"，起始字节为 7，结束为 11
    assert_eq!(results[0].match_positions, vec![(7, 11)]);
}

#[test]
fn test_regex_search_invalid_syntax_graceful() {
    let store = IndexStore::new();
    let filter = CompiledRecordFilter::default();
    let read = store.read();

    // 括号未闭合，不 panic，返回友好错误
    let (results, err) = regex_search::search_index(&read, "(unclosed", &filter, 10, false);
    assert!(err.is_some());
    assert!(results.is_empty());
}

#[test]
fn test_regex_search_length_limit() {
    let store = IndexStore::new();
    let filter = CompiledRecordFilter::default();
    let read = store.read();

    let long_pattern = "a".repeat(1025);
    let (results, err) = regex_search::search_index(&read, &long_pattern, &filter, 10, false);
    assert!(err.is_some());
    assert!(results.is_empty());
}

use lokii_core::search::{
    detect_mode, dispatch_with_details, dispatch_with_options, CaseSensitivity, SearchMode,
};

#[test]
fn test_detect_mode_with_regex_sugar() {
    assert_eq!(detect_mode("/[a-z]+/"), SearchMode::Regex);
    assert_eq!(detect_mode("/test/i"), SearchMode::Regex);
    assert_eq!(detect_mode("/usr/local/bin"), SearchMode::Auto);
    assert_eq!(detect_mode("/System/"), SearchMode::Auto);
}

#[test]
fn test_dispatch_regex_sugar_and_or_pipe_coexistence() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry("foo.rs", "/src/foo.rs"),
        make_entry("bar.rs", "/src/bar.rs"),
        make_entry("baz.rs", "/src/baz.rs"),
    ]);

    // 1. 正则内管道符 /(foo|bar)/ 不应被误判为并集检索
    let (res1, _, err1) = dispatch_with_options(
        &store,
        "/(foo|bar)\\.rs/",
        SearchMode::Auto,
        CaseSensitivity::Auto,
        10,
    );
    assert!(err1.is_none());
    assert_eq!(res1.len(), 2);
    let names: Vec<_> = res1.iter().map(|r| r.entry.name.as_str()).collect();
    assert!(names.contains(&"foo.rs"));
    assert!(names.contains(&"bar.rs"));

    // 2. /pattern/i 标志优先级高于全局锁定
    let (res2, is_cs2, _) =
        dispatch_with_options(&store, "/foo/i", SearchMode::Auto, CaseSensitivity::Sensitive, 10);
    assert_eq!(res2.len(), 1);
    assert!(!is_cs2); // 局部 i 强制不敏感
}

#[test]
fn test_dispatch_with_details_effective_mode() {
    let store = IndexStore::new();
    store.replace(vec![make_entry("foo.rs", "/src/foo.rs")]);

    let (_, is_cs1, _, mode1) =
        dispatch_with_details(&store, "/[f]oo/", SearchMode::Auto, CaseSensitivity::Auto, 10);
    assert_eq!(mode1, SearchMode::Regex);
    assert!(!is_cs1);

    let (_, is_cs2, _, mode2) =
        dispatch_with_details(&store, "/[F]oo/", SearchMode::Auto, CaseSensitivity::Auto, 10);
    assert_eq!(mode2, SearchMode::Regex);
    assert!(is_cs2); // Smart Case triggers sensitive

    let (_, _, _, mode3) =
        dispatch_with_details(&store, "foo", SearchMode::Auto, CaseSensitivity::Auto, 10);
    assert_eq!(mode3, SearchMode::Auto);
}


