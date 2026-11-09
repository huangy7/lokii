use lokii_core::indexer::{score_match, score_match_zero_alloc, FileEntry, IndexStore, MatchScore};
use lokii_core::search::{
    dispatch_with_options, extract_content_terms_for_smart_case, resolve_case_sensitive,
    CaseSensitivity, SearchMode,
};

#[test]
fn test_smart_case_lowercase_query() {
    let query = "report";
    assert!(!resolve_case_sensitive(query, CaseSensitivity::Auto, SearchMode::Auto));
    assert!(!resolve_case_sensitive(query, CaseSensitivity::Insensitive, SearchMode::Auto));
    assert!(resolve_case_sensitive(query, CaseSensitivity::Sensitive, SearchMode::Auto));
}

#[test]
fn test_smart_case_uppercase_query() {
    let query = "Report";
    assert!(resolve_case_sensitive(query, CaseSensitivity::Auto, SearchMode::Auto));
    assert!(!resolve_case_sensitive(query, CaseSensitivity::Insensitive, SearchMode::Auto));
    assert!(resolve_case_sensitive(query, CaseSensitivity::Sensitive, SearchMode::Auto));
}

#[test]
fn test_smart_case_path_scope_isolation() {
    // 目录名 Downloads 包含大写，但终端文件名 report 全小写 -> 判定为不敏感
    let query = "in:~/Downloads report";
    assert!(!resolve_case_sensitive(query, CaseSensitivity::Auto, SearchMode::Auto));

    // 路径包含大写目录但终端文件名全小写 -> 判定为不敏感
    let query2 = "Downloads/report";
    assert!(!resolve_case_sensitive(query2, CaseSensitivity::Auto, SearchMode::Auto));

    // 终端文件名包含大写 -> 判定为敏感
    let query3 = "downloads/Report";
    assert!(resolve_case_sensitive(query3, CaseSensitivity::Auto, SearchMode::Auto));
}

#[test]
fn test_smart_case_regex_meta_escape() {
    // 正则转义字符 \D, \S 不触发大写敏感
    let query = r"regex:test_\D+\S+";
    assert!(!resolve_case_sensitive(query, CaseSensitivity::Auto, SearchMode::Regex));

    // 正则字面量大写触发敏感
    let query_lit = r"regex:AppDelegate_\d+";
    assert!(resolve_case_sensitive(query_lit, CaseSensitivity::Auto, SearchMode::Regex));
}

#[test]
fn test_extract_content_terms() {
    let terms = extract_content_terms_for_smart_case("in:~/Downloads Downloads/report .", SearchMode::Auto);
    assert_eq!(terms, vec!["report"]);

    let regex_terms = extract_content_terms_for_smart_case("regex:test.*", SearchMode::Regex);
    assert_eq!(regex_terms, vec!["test.*"]);
}

#[test]
fn test_score_match_case_sensitive() {
    let keywords_lower = vec!["report".to_string()];
    let keywords_upper = vec!["Report".to_string()];

    // 1. 不敏感模式：大小写互相命中
    assert!(score_match_zero_alloc("Report.pdf", None, &keywords_lower, true, false, false).is_some());
    assert!(score_match_zero_alloc("report.pdf", None, &keywords_upper, true, false, false).is_some());

    // 2. 敏感模式：严格匹配大小写
    assert!(score_match_zero_alloc("Report.pdf", None, &keywords_upper, true, true, false).is_some());
    assert!(score_match_zero_alloc("report.pdf", None, &keywords_upper, true, true, false).is_none());
    assert!(score_match_zero_alloc("Report.pdf", None, &keywords_lower, true, true, false).is_none());
}

#[test]
fn test_pinyin_arbitration_in_smart_case() {
    let kw = vec!["WD".to_string()];

    // 在 Auto 激活的敏感模式下，拼音允许命中但得分为 Substring
    let pinyin_score = score_match_zero_alloc("我的文档.docx", None, &kw, true, true, false);
    assert_eq!(pinyin_score, Some(MatchScore::Substring));

    // 在强制 Sensitive 锁定下，拼音被彻底屏蔽（返回 None）
    let locked_score = score_match_zero_alloc("我的文档.docx", None, &kw, true, true, true);
    assert_eq!(locked_score, None);
}

#[test]
fn test_score_match_empty_keywords_safety() {
    let empty_keywords: Vec<String> = Vec::new();

    // 1. score_match 传入空关键词安全返回 None，杜绝越界 panic
    assert_eq!(score_match("report.pdf", &empty_keywords), None);

    // 2. score_match_zero_alloc 在敏感模式下传入空关键词安全返回 None
    assert_eq!(
        score_match_zero_alloc("Report.pdf", None, &empty_keywords, true, true, false),
        None
    );

    // 3. score_match_zero_alloc 在不敏感模式下传入空关键词安全返回 None
    assert_eq!(
        score_match_zero_alloc("Report.pdf", None, &empty_keywords, true, false, false),
        None
    );
}

#[test]
fn test_pinyin_case_insensitivity_and_non_ascii_skip() {
    // 大写全拼与首字母匹配中文
    let kw_upper_full = vec!["GONGZUO".to_string()];
    let kw_upper_init = vec!["GZHB".to_string()];

    // 不敏感模式下大写全拼命中
    let score_full = score_match_zero_alloc("工作汇报.docx", None, &kw_upper_full, true, false, false);
    assert_eq!(score_full, Some(MatchScore::Prefix));

    // 不敏感模式下大写首字母命中
    let score_init = score_match_zero_alloc("工作汇报.docx", None, &kw_upper_init, true, false, false);
    assert_eq!(score_init, Some(MatchScore::Prefix));

    // 非 ASCII 关键词在敏感模式下跳过拼音匹配，直接返回 None（不触发堆分配与无效拼音查找）
    let non_ascii_kw = vec!["中文".to_string()];
    let score_non_ascii = score_match_zero_alloc("工作汇报.docx", None, &non_ascii_kw, false, true, false);
    assert_eq!(score_non_ascii, None);
}

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
            .map(|ext| ext.to_string_lossy().to_lowercase())
            .unwrap_or_default(),
    }
}

#[test]
fn test_dispatch_smart_case_integration() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry("AppDelegate.swift", "/src/AppDelegate.swift"),
        make_entry("appdelegate.swift", "/src/appdelegate.swift"),
        make_entry("Report_2026.pdf", "/docs/Report_2026.pdf"),
        make_entry("report_2026.pdf", "/docs/report_2026.pdf"),
    ]);

    // 1. 输入全小写 -> 召回全部两项
    let (res1, is_cs1, _) = dispatch_with_options(&store, "appdelegate", SearchMode::Auto, CaseSensitivity::Auto, 10);
    assert!(!is_cs1);
    assert_eq!(res1.len(), 2);

    // 2. 输入大写 -> 仅召回 AppDelegate.swift
    let (res2, is_cs2, _) = dispatch_with_options(&store, "AppDelegate", SearchMode::Auto, CaseSensitivity::Auto, 10);
    assert!(is_cs2);
    assert_eq!(res2.len(), 1);
    assert_eq!(res2[0].entry.name, "AppDelegate.swift");

    // 3. 显式锁定 Sensitive，即使输入全小写也严格区分
    let (res3, is_cs3, _) = dispatch_with_options(&store, "appdelegate", SearchMode::Auto, CaseSensitivity::Sensitive, 10);
    assert!(is_cs3);
    assert_eq!(res3.len(), 1);
    assert_eq!(res3[0].entry.name, "appdelegate.swift");

    // 4. 显式锁定 Insensitive，即使输入大写也忽略大小写
    let (res4, is_cs4, _) = dispatch_with_options(&store, "AppDelegate", SearchMode::Auto, CaseSensitivity::Insensitive, 10);
    assert!(!is_cs4);
    assert_eq!(res4.len(), 2);

    // 5. 正则输入小写与大写
    let (res5, is_cs5, _) = dispatch_with_options(&store, "regex:^report", SearchMode::Regex, CaseSensitivity::Auto, 10);
    assert!(!is_cs5);
    assert_eq!(res5.len(), 2);

    let (res6, is_cs6, _) = dispatch_with_options(&store, "regex:^Report", SearchMode::Regex, CaseSensitivity::Auto, 10);
    assert!(is_cs6);
    assert_eq!(res6.len(), 1);
    assert_eq!(res6[0].entry.name, "Report_2026.pdf");
}

#[test]
fn test_dispatch_wildcard_smart_case() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry("AppDelegate.swift", "/src/AppDelegate.swift"),
        make_entry("appdelegate.swift", "/src/appdelegate.swift"),
    ]);

    // 1. 小写通配符：Auto 模式下匹配两者
    let (res1, is_cs1, _) = dispatch_with_options(&store, "app*.swift", SearchMode::Wildcard, CaseSensitivity::Auto, 10);
    assert!(!is_cs1);
    assert_eq!(res1.len(), 2);

    // 2. 大写通配符：Auto 模式下仅匹配 AppDelegate.swift
    let (res2, is_cs2, _) = dispatch_with_options(&store, "App*.swift", SearchMode::Wildcard, CaseSensitivity::Auto, 10);
    assert!(is_cs2);
    assert_eq!(res2.len(), 1);
    assert_eq!(res2[0].entry.name, "AppDelegate.swift");

    // 3. 大写通配符：显式 Insensitive 忽略大小写匹配两者
    let (res3, is_cs3, _) = dispatch_with_options(&store, "App*.swift", SearchMode::Wildcard, CaseSensitivity::Insensitive, 10);
    assert!(!is_cs3);
    assert_eq!(res3.len(), 2);
}

#[test]
fn test_dispatch_path_search_smart_case() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry("AppDelegate.swift", "/src/AppDelegate.swift"),
        make_entry("appdelegate.swift", "/src/appdelegate.swift"),
    ]);

    // 1. 路径 + 小写文件名：匹配两者
    let (res1, is_cs1, _) = dispatch_with_options(&store, "src/appdelegate", SearchMode::Auto, CaseSensitivity::Auto, 10);
    assert!(!is_cs1);
    assert_eq!(res1.len(), 2);

    // 2. 路径 + 大写文件名：仅匹配 AppDelegate.swift
    let (res2, is_cs2, _) = dispatch_with_options(&store, "src/AppDelegate", SearchMode::Auto, CaseSensitivity::Auto, 10);
    assert!(is_cs2);
    assert_eq!(res2.len(), 1);
    assert_eq!(res2[0].entry.name, "AppDelegate.swift");

    // 3. 大写目录路径 + 小写文件名：目录隔离，不触发敏感
    let (res3, is_cs3, _) = dispatch_with_options(&store, "SRC/appdelegate", SearchMode::Auto, CaseSensitivity::Auto, 10);
    assert!(!is_cs3);
    assert_eq!(res3.len(), 2);
}

#[test]
fn test_smart_case_in_scope_integration() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry("AppDelegate.swift", "/Users/tester/AppDelegate.swift"),
        make_entry("appdelegate.swift", "/Users/tester/appdelegate.swift"),
        make_entry("AppDelegate.swift", "/Other/tester/AppDelegate.swift"),
    ]);

    // 1. 输入大写文件名 + in: 作用域 -> 激活敏感，且父目录大小写不敏感匹配成功召回
    let (res1, is_cs1, _) = dispatch_with_options(
        &store,
        "in:/Users/tester/ AppDelegate",
        SearchMode::Auto,
        CaseSensitivity::Auto,
        10,
    );
    assert!(is_cs1);
    assert_eq!(res1.len(), 1);
    assert_eq!(res1[0].entry.name, "AppDelegate.swift");
    assert_eq!(res1[0].entry.path, "/Users/tester/AppDelegate.swift");

    // 2. 输入全小写文件名 + in: 作用域 -> 不激活敏感，召回该作用域下的所有大小写变体
    let (res2, is_cs2, _) = dispatch_with_options(
        &store,
        "in:/Users/tester/ appdelegate",
        SearchMode::Auto,
        CaseSensitivity::Auto,
        10,
    );
    assert!(!is_cs2);
    assert_eq!(res2.len(), 2);
}

#[test]
fn test_smart_case_path_dir_case_insensitivity() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry("AppDelegate.swift", "/Src/AppDelegate.swift"),
        make_entry("appdelegate.swift", "/Src/appdelegate.swift"),
    ]);

    // 1. 输入小写目录 + 大写文件名：目录大小写不敏感命中 /Src/，文件名大写敏感仅命中 AppDelegate.swift
    let (res1, is_cs1, _) = dispatch_with_options(
        &store,
        "src/AppDelegate",
        SearchMode::Auto,
        CaseSensitivity::Auto,
        10,
    );
    assert!(is_cs1);
    assert_eq!(res1.len(), 1);
    assert_eq!(res1[0].entry.name, "AppDelegate.swift");

    // 2. 输入大写目录 + 大写文件名：同样命中
    let (res2, is_cs2, _) = dispatch_with_options(
        &store,
        "SRC/AppDelegate",
        SearchMode::Auto,
        CaseSensitivity::Auto,
        10,
    );
    assert!(is_cs2);
    assert_eq!(res2.len(), 1);
    assert_eq!(res2[0].entry.name, "AppDelegate.swift");
}

#[test]
fn test_explicit_lock_path_pinyin_isolation() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry("我的文档.docx", "/src/我的文档.docx"),
        make_entry("WD_Report.docx", "/src/WD_Report.docx"),
    ]);

    // 1. 显式锁定 Sensitive：输入路径 + WD，彻底屏蔽中文拼音匹配，仅召回 WD_Report.docx
    let (res_locked, is_cs_locked, _) = dispatch_with_options(
        &store,
        "src/WD",
        SearchMode::Auto,
        CaseSensitivity::Sensitive,
        10,
    );
    assert!(is_cs_locked);
    assert_eq!(res_locked.len(), 1);
    assert_eq!(res_locked[0].entry.name, "WD_Report.docx");

    // 2. Auto 模式下输入路径 + WD：智能激活敏感，保留拼音降权兜底
    let (res_auto, is_cs_auto, _) = dispatch_with_options(
        &store,
        "src/WD",
        SearchMode::Auto,
        CaseSensitivity::Auto,
        10,
    );
    assert!(is_cs_auto);
    assert_eq!(res_auto.len(), 2);
}


