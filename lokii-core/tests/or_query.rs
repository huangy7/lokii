use lokii_core::indexer::{FileEntry, IndexStore};
use lokii_core::search::disjunction::{
    merge_highlight_spans, split_query_pipes, unescape_literal_pipes, DisjunctionQuery,
};
use lokii_core::search::substring;
use lokii_core::search::{dispatch_with_options, CaseSensitivity, SearchMode};
use lokii_core::search::CompiledRecordFilter;

fn make_entry(name: &str, path: &str, is_dir: bool) -> FileEntry {
    FileEntry {
        name: name.to_string(),
        path: path.to_string(),
        is_dir,
        size: 1024,
        modified: 1700000000,
        created_at: 0,
        extension: if is_dir {
            String::new()
        } else {
            std::path::Path::new(name)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        },
    }
}

#[test]
fn test_disjunction_search_single_pass() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry("report_2026.pdf", "/docs/report_2026.pdf", false),
        make_entry("summary_2025.docx", "/docs/summary_2025.docx", false),
        make_entry("unrelated.txt", "/docs/unrelated.txt", false),
        make_entry("assets.png", "/assets.png", true), // 同名目录
        make_entry("photo.png", "/photo.png", false),
    ]);

    let filter = CompiledRecordFilter::default();
    let read = store.read();

    // 1. 基本或查询
    let q1 = DisjunctionQuery::parse("report | summary", None);
    let res1 = substring::search_disjunction(&read, &q1, &filter, 10, false);
    assert_eq!(res1.len(), 2);

    // 2. 空格结合律：report 2026 | summary 2025
    let q2 = DisjunctionQuery::parse("report 2026 | summary 2025", None);
    let res2 = substring::search_disjunction(&read, &q2, &filter, 10, false);
    assert_eq!(res2.len(), 2);

    // 3. 扩展名排斥目录：*.png 应只匹配 photo.png，排除 assets.png 目录
    let q3 = DisjunctionQuery::parse("*.png", None);
    let res3 = substring::search_disjunction(&read, &q3, &filter, 10, false);
    assert_eq!(res3.len(), 1);
    assert_eq!(res3[0].entry.name, "photo.png");
}

#[test]
fn test_or_per_clause_smart_case_isolation() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry("AppCoordinator.swift", "/src/AppCoordinator.swift", false),
        make_entry("appcoordinator.swift", "/src/appcoordinator.swift", false),
        make_entry("Test.swift", "/tests/Test.swift", false),
        make_entry("test.rs", "/tests/test.rs", false),
    ]);

    let filter = CompiledRecordFilter::default();
    let read = store.read();

    // 左侧 AppCoordinator 敏感，右侧 test 不敏感
    let q = DisjunctionQuery::parse("AppCoordinator | test", None);
    let res = substring::search_disjunction(&read, &q, &filter, 10, false);

    // 应命中 AppCoordinator.swift (大写), Test.swift, test.rs 共 3 项；过滤 appcoordinator.swift
    let names: Vec<_> = res.iter().map(|r| r.entry.name.as_str()).collect();
    assert!(names.contains(&"AppCoordinator.swift"));
    assert!(!names.contains(&"appcoordinator.swift"));
    assert!(names.contains(&"Test.swift"));
    assert!(names.contains(&"test.rs"));
}

#[test]
fn test_or_query_highlights_and_ranking() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry("report_summary.pdf", "/docs/report_summary.pdf", false),
        make_entry("report.pdf", "/docs/report.pdf", false),
        make_entry("exact_match.txt", "/docs/exact_match.txt", false),
    ]);

    let filter = CompiledRecordFilter::default();
    let read = store.read();

    // 多子句高亮合并验证：report | summary 命中 report_summary.pdf，高亮应包含两个子串
    let q = DisjunctionQuery::parse("report | summary", None);
    let res = substring::search_disjunction(&read, &q, &filter, 10, false);
    assert_eq!(res.len(), 2);

    let both = res
        .iter()
        .find(|r| r.entry.name == "report_summary.pdf")
        .expect("should find report_summary.pdf");
    // "report_summary.pdf": "report" is [0..6], "summary" is [7..14]
    assert_eq!(both.match_positions, vec![(0, 6), (7, 14)]);

    // 极值打分置换堆顶：后出现的精确匹配项必须能够按分数梯度排在前列
    let q_rank = DisjunctionQuery::parse("summary | exact_match.txt", None);
    let res_rank = substring::search_disjunction(&read, &q_rank, &filter, 10, false);
    assert_eq!(res_rank[0].entry.name, "exact_match.txt");
    assert_eq!(res_rank[0].score, 1.0); // Exact score
}

#[test]
fn test_split_query_pipes_basic_and_whitespace() {
    let parts = split_query_pipes("report 2026 | summary 2025");
    assert_eq!(parts, vec!["report 2026", "summary 2025"]);

    // 紧凑无空格写法
    let compact = split_query_pipes("*.png|*.jpg");
    assert_eq!(compact, vec!["*.png", "*.jpg"]);
}

#[test]
fn test_split_query_pipes_quote_protection() {
    // 引号内的管道符不被拆分
    let parts = split_query_pipes(r#""main | helper" | worker"#);
    assert_eq!(parts, vec![r#""main | helper""#, "worker"]);

    // 未闭合引号作为前缀容错
    let parts_unclosed = split_query_pipes(r#""main | helper"#);
    assert_eq!(parts_unclosed, vec![r#""main | helper"#]);
}

#[test]
fn test_split_query_pipes_escaped_pipe() {
    // 奇数反斜杠：转义管道符，不拆分
    let parts = split_query_pipes(r"foo\|bar | baz");
    assert_eq!(parts, vec![r"foo\|bar", "baz"]);
    assert_eq!(unescape_literal_pipes(r"foo\|bar"), "foo|bar");

    // 偶数反斜杠：字面量反斜杠加并集切分
    let parts_even = split_query_pipes(r"foo\\|bar");
    assert_eq!(parts_even, vec![r"foo\\", "bar"]);
}

#[test]
fn test_dangling_and_consecutive_pipes() {
    // 悬空管道符
    let q1 = DisjunctionQuery::parse("report |", None);
    assert_eq!(q1.clauses.len(), 1);
    assert_eq!(q1.clauses[0].raw_query, "report");

    // 连续多个管道符
    let q2 = DisjunctionQuery::parse("a ||| b", None);
    assert_eq!(q2.clauses.len(), 2);
    assert_eq!(q2.clauses[0].raw_query, "a");
    assert_eq!(q2.clauses[1].raw_query, "b");

    // 纯管道符判定为空
    let q3 = DisjunctionQuery::parse("|||", None);
    assert!(q3.is_empty());
}

#[test]
fn test_merge_highlight_spans() {
    // 相邻区间合并：(0, 3) + (3, 6) -> (0, 6)
    let spans = vec![(0, 3), (3, 6)];
    let merged = merge_highlight_spans(spans, 10);
    assert_eq!(merged, vec![(0, 6)]);

    // 重叠与包含合并：(0, 4) + (2, 7) -> (0, 7)
    let spans2 = vec![(0, 4), (2, 7)];
    let merged2 = merge_highlight_spans(spans2, 10);
    assert_eq!(merged2, vec![(0, 7)]);
}

#[test]
fn test_dispatch_or_integration_and_wildcard_priority() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry("logo.png", "/assets/logo.png", false),
        make_entry("banner.jpg", "/assets/banner.jpg", false),
        make_entry("main.rs", "/src/main.rs", false),
        make_entry("lib.rs", "/src/lib.rs", false),
    ]);

    // 1. 紧凑扩展名并集：*.png | *.jpg (原本会被误判为普通 Wildcard 导致 0 召回)
    let (res1, _, err1) = dispatch_with_options(&store, "*.png | *.jpg", SearchMode::Auto, CaseSensitivity::Auto, 10);
    assert!(err1.is_none());
    assert_eq!(res1.len(), 2);
    let names1: Vec<_> = res1.iter().map(|r| r.entry.name.as_str()).collect();
    assert!(names1.contains(&"logo.png"));
    assert!(names1.contains(&"banner.jpg"));

    // 2. 悬空管道符打字体验：输入 "main |" 保持左侧结果
    let (res2, _, _) = dispatch_with_options(&store, "main |", SearchMode::Auto, CaseSensitivity::Auto, 10);
    assert_eq!(res2.len(), 1);
    assert_eq!(res2[0].entry.name, "main.rs");

    // 3. 纯管道符安全守卫：输入 "||" 返回空列表
    let (res3, _, _) = dispatch_with_options(&store, "||", SearchMode::Auto, CaseSensitivity::Auto, 10);
    assert!(res3.is_empty());

    // 4. 类型过滤与纯管道符组合守卫：输入 "type:code |" 返回空列表，阻断全量直通
    let (res4, _, _) = dispatch_with_options(&store, "type:code |", SearchMode::Auto, CaseSensitivity::Auto, 10);
    assert!(res4.is_empty());
}

#[test]
fn test_has_unescaped_pipe_semantics() {
    use lokii_core::search::has_unescaped_pipe;

    assert!(!has_unescaped_pipe("main"));
    assert!(has_unescaped_pipe("main | test"));
    assert!(has_unescaped_pipe("main |"));
    assert!(has_unescaped_pipe("|"));
    assert!(has_unescaped_pipe("||"));
    assert!(!has_unescaped_pipe("main \\| test"));
    assert!(has_unescaped_pipe("main \\\\| test"));
    assert!(!has_unescaped_pipe("\"main | test\""));
    assert!(!has_unescaped_pipe("'main | test'"));
}
