use lokii_core::indexer::{FileEntry, IndexStore};
use lokii_core::search::filter::{parse_query_structured, tokenize_query, QueryFilter};
use lokii_core::search::{
    detect_mode, dispatch, dispatch_with_details, dispatch_with_filter, dispatch_with_options,
    CaseSensitivity, SearchMode,
};

fn make_file(path: &str, size: u64) -> FileEntry {
    let trimmed = path.trim_end_matches('/');
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed).to_string();
    let extension = name
        .rsplit_once('.')
        .filter(|(stem, ext)| !stem.is_empty() && !ext.is_empty())
        .map(|(_, ext)| ext.to_lowercase())
        .unwrap_or_default();
    FileEntry {
        name,
        path: path.to_string(),
        is_dir: false,
        size,
        modified: 1_700_000_000,
        created_at: 1_700_000_000,
        extension,
    }
}

fn make_dir(path: &str) -> FileEntry {
    let trimmed = path.trim_end_matches('/');
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed).to_string();
    FileEntry {
        name,
        path: trimmed.to_string(),
        is_dir: true,
        size: 0,
        modified: 1_700_000_000,
        created_at: 1_700_000_000,
        extension: String::new(),
    }
}

fn build_store(entries: Vec<FileEntry>) -> IndexStore {
    let store = IndexStore::new();
    store.replace(entries);
    store
}

fn parent_slice(entry_path: &str) -> &str {
    match entry_path.rfind('/') {
        Some(pos) => &entry_path[..=pos],
        None => "",
    }
}

fn assert_valid_highlights(entry_path: &str, name: &str, match_pos: &[(usize, usize)], parent_pos: &[(usize, usize)]) {
    for &(s, e) in match_pos {
        assert!(s <= e && e <= name.len(), "name span ({s}, {e}) out of bounds for {name:?}");
        assert!(name.is_char_boundary(s) && name.is_char_boundary(e), "invalid UTF-8 boundary in name {name:?}");
    }
    let parent = parent_slice(entry_path);
    for &(s, e) in parent_pos {
        assert!(s <= e && e <= parent.len(), "parent span ({s}, {e}) out of bounds for {parent:?}");
        assert!(parent.is_char_boundary(s) && parent.is_char_boundary(e), "invalid UTF-8 boundary in parent {parent:?}");
    }
}

// =========================================================================
// Feature 1: 容错粘贴路径清洗（Quotes / \ / file:// / %XX）
// =========================================================================

#[test]
fn paste_path_sanitizes_quotes_shell_escapes_and_file_uris() {
    let store = build_store(vec![
        make_file("/Users/alice/My Docs/spec.md", 120),
        make_file("/Users/alice/My Docs/a (1)&b'c.txt", 120),
        make_file("/Users/alice/中文/note.md", 120),
        make_file("/Users/alice/a+b/calc.md", 120),
        make_file("/Users/alice/100% done/report.txt", 120),
    ]);

    // 双引号包裹绝对路径
    let (res, err) = dispatch(&store, r#""/Users/alice/My Docs/spec.md""#, SearchMode::Auto, 10);
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.path, "/Users/alice/My Docs/spec.md");

    // 单引号包裹绝对路径
    let (res, err) = dispatch(&store, "'/Users/alice/My Docs/spec.md'", SearchMode::Auto, 10);
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.path, "/Users/alice/My Docs/spec.md");

    // POSIX shell 反斜杠转义（空格、括号、&、单引号）
    let (res, err) = dispatch(
        &store,
        r"/Users/alice/My\ Docs/a\ \(1\)\&b\'c.txt",
        SearchMode::Auto,
        10,
    );
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.path, "/Users/alice/My Docs/a (1)&b'c.txt");

    // file:/// URL 大小写不敏感 + %20 解码
    let (res, err) = dispatch(&store, "file:///Users/alice/My%20Docs/spec.md", SearchMode::Auto, 10);
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.path, "/Users/alice/My Docs/spec.md");

    // FILE://LOCALHOST/...
    let (res, err) = dispatch(&store, "FILE://LOCALHOST/Users/alice/My%20Docs/spec.md", SearchMode::Auto, 10);
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.path, "/Users/alice/My Docs/spec.md");

    // 引号内嵌 file:// URI
    let (res, err) = dispatch(&store, r#""file:///Users/alice/My%20Docs/spec.md""#, SearchMode::Auto, 10);
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.path, "/Users/alice/My Docs/spec.md");

    // 多字节 UTF-8 百分号编码
    let (res, err) = dispatch(&store, "file:///Users/alice/%E4%B8%AD%E6%96%87/note.md", SearchMode::Auto, 10);
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.path, "/Users/alice/中文/note.md");

    // 粘贴的引号绝对路径包含 + 号时不应误判为 /.../ 正则语法糖
    let (res, err) = dispatch(&store, r#""/Users/alice/a+b/calc.md""#, SearchMode::Auto, 10);
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.path, "/Users/alice/a+b/calc.md");
}

#[test]
fn unquoted_in_filter_sanitizes_pasted_paths_and_uris() {
    let store = build_store(vec![
        make_file("/share/dir/foo.txt", 100),
        make_file("/other/dir/foo.txt", 100),
        make_file("/Users/alice/My Docs/spec.md", 100),
        make_file("/Users/bob/My Docs/spec.md", 100),
    ]);

    let (res, err) = dispatch(
        &store,
        "in:file:///Users/alice/My%20Docs spec.md",
        SearchMode::Auto,
        10,
    );
    assert!(err.is_none());
    let paths: Vec<&str> = res.iter().map(|r| r.entry.path.as_str()).collect();
    assert_eq!(paths, vec!["/Users/alice/My Docs/spec.md"]);

    // 管道符不触发未加引号 in: 粘贴路径吞并，保持 disjunction 边界安全
    let pipe_tokens = tokenize_query("in:/Users/alice|foo");
    assert_eq!(pipe_tokens.len(), 1);
    assert_eq!(pipe_tokens[0].text, "in:/Users/alice|foo");
    assert!(!pipe_tokens[0].was_sanitized_pasted_path);
}

#[test]
fn paste_path_invalid_percent_encoding_never_panics_and_preserves_non_uri_percent() {
    let store = build_store(vec![
        make_file("/Users/alice/100% done.txt", 50),
        make_file("/Users/alice/100%20literal.txt", 50),
        make_file("/Users/alice/%ZZ/bad.txt", 50),
        make_file("/Users/alice/trunc%/bad.txt", 50),
    ]);

    // 非 file:// 输入不解码 %20，且 "100% done" 保留字面量
    let (res, err) = dispatch(&store, r#""100% done""#, SearchMode::Auto, 10);
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.name, "100% done.txt");

    let (res, err) = dispatch(&store, "100%20literal", SearchMode::Auto, 10);
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.name, "100%20literal.txt");

    // file:// 后含非法 %ZZ、截断 %、非法 UTF-8 %FF 绝不 panic，原样回退
    let (res_zz, err_zz) = dispatch(&store, "file:///Users/alice/%ZZ/bad.txt", SearchMode::Auto, 10);
    assert!(err_zz.is_none());
    assert_eq!(res_zz.len(), 1);
    assert_eq!(res_zz[0].entry.path, "/Users/alice/%ZZ/bad.txt");

    let (res_trunc, err_trunc) = dispatch(&store, "file:///Users/alice/trunc%/bad.txt", SearchMode::Auto, 10);
    assert!(err_trunc.is_none());
    assert_eq!(res_trunc.len(), 1);
    assert_eq!(res_trunc[0].entry.path, "/Users/alice/trunc%/bad.txt");

    let (res_ff, err_ff) = dispatch(&store, "file:///Users/alice/%FF%FE/bad.txt", SearchMode::Auto, 10);
    assert!(err_ff.is_none());
    assert!(res_ff.is_empty());
}

#[test]
fn paste_path_preserves_existing_quotes_and_regex_sugar_contracts() {
    let store = build_store(vec![
        make_file("/dir/foo_bar_literal.txt", 10),
        make_file("/dir/\"foo/bar\".txt", 10),
        make_file("/dir/foo/bar.txt", 10),
        make_file("/dir/hello world.txt", 10),
        make_file("/dir/README's.md", 10),
        make_file("/dir/foo   bar.txt", 10),
        make_file("/dir/main_12.rs", 10),
    ]);

    // Task 4 保护："foo/bar" 仍保持引号子串模式（Auto），不解包为路径搜索
    assert_eq!(detect_mode(r#""foo/bar""#), SearchMode::Auto);
    let tokens = tokenize_query(r#""foo/bar""#);
    assert_eq!(tokens.len(), 1);
    assert!(tokens[0].quoted);
    assert_eq!(tokens[0].text, "foo/bar");

    // 引号内短语与撇号均按原字面量保留
    let (res, _) = dispatch(&store, r#""hello world""#, SearchMode::Auto, 10);
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.name, "hello world.txt");

    let (res, _) = dispatch(&store, r#""README's""#, SearchMode::Auto, 10);
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.name, "README's.md");

    // 引号包裹疑似正则 `/a\s+b/` 不解包为路径，也不激活正则
    let q_regex = tokenize_query(r#""/a\s+b/""#);
    assert_eq!(q_regex.len(), 1);
    assert!(q_regex[0].quoted, "quoted /a\\s+b/ must stay quoted");
    assert_eq!(detect_mode(r#""/a\s+b/""#), SearchMode::Auto);

    // Task 4 行内正则语法糖 /.../ 与显式 regex: 不受转义清洗影响
    assert_eq!(detect_mode(r"/^foo\s+bar/"), SearchMode::Regex);
    let (res, err) = dispatch(&store, r"/^foo\s+bar/", SearchMode::Auto, 10);
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.name, "foo   bar.txt");

    assert_eq!(detect_mode(r"regex:^main_\d+\.rs"), SearchMode::Regex);
    let (res, err) = dispatch(&store, r"regex:^main_\d+\.rs", SearchMode::Auto, 10);
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.name, "main_12.rs");
}

// =========================================================================
// Feature 2: 中英标准目录双向别名 + in:<dir> 边界修复 + 高亮区间防越界
// =========================================================================

#[test]
fn directory_aliases_bidirectional_cn_en_and_traditional_chinese() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/testuser".to_string());
    let home_trimmed = home.trim_end_matches('/');

    let store = build_store(vec![
        make_dir("/Users/u/Desktop"),
        make_file("/Users/u/Desktop/周报.docx", 100),
        make_file("/Users/u/桌面/汇总.docx", 100),
        make_file("/Users/u/Downloads/report.pdf", 100),
        make_file("/Users/u/Downloads/sub/report.pdf", 100),
        make_file("/Users/u/mydownloads/report.pdf", 100),
        make_file("/Users/u/Documents/plan.md", 100),
        make_file("/Users/u/Pictures/photo.png", 100),
        make_file("/Users/u/Music/track.flac", 100),
        make_file("/Users/u/Movies/clip.mov", 100),
        make_dir("/Applications"),
        make_file("/Applications/Notes.app", 100),
        make_file("/Users/u/Trash/old.txt", 100),
        make_file("/Users/u/notes/桌面备忘.txt", 100),
        make_file(&format!("{home_trimmed}/personal_doc.txt"), 100),
        make_file("/archive/个人/personal_doc.txt", 100),
    ]);

    // 简体中文别名 -> 英文目录 + 高亮字节区间精确覆盖目标路径 "Desktop"
    let (res, err) = dispatch(&store, "桌面/周报", SearchMode::Auto, 10);
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.path, "/Users/u/Desktop/周报.docx");
    assert_valid_highlights(
        &res[0].entry.path,
        &res[0].entry.name,
        &res[0].match_positions,
        &res[0].parent_match_positions,
    );
    let parent = parent_slice(&res[0].entry.path);
    assert!(
        res[0]
            .parent_match_positions
            .iter()
            .any(|&(s, e)| parent[s..e].eq_ignore_ascii_case("desktop")),
        "expected Desktop highlighted in parent_match_positions, got {:?} for {parent}",
        res[0].parent_match_positions
    );

    // 反向英文 -> 中文目录
    let (res, err) = dispatch(&store, "Desktop/汇总", SearchMode::Auto, 10);
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.path, "/Users/u/桌面/汇总.docx");
    assert_valid_highlights(
        &res[0].entry.path,
        &res[0].entry.name,
        &res[0].match_positions,
        &res[0].parent_match_positions,
    );

    // 繁体中文别名經 fold_cow 折叠后命中英文目录
    let (res, err) = dispatch(&store, "下載/report", SearchMode::Auto, 10);
    assert!(err.is_none());
    let paths: Vec<&str> = res.iter().map(|r| r.entry.path.as_str()).collect();
    assert!(paths.contains(&"/Users/u/Downloads/report.pdf"));
    assert!(paths.contains(&"/Users/u/Downloads/sub/report.pdf"));
    assert!(!paths.contains(&"/Users/u/mydownloads/report.pdf"));

    for (query, expected_path) in [
        ("文稿/plan", "/Users/u/Documents/plan.md"),
        ("图片/photo", "/Users/u/Pictures/photo.png"),
        ("音乐/track", "/Users/u/Music/track.flac"),
        ("影片/clip", "/Users/u/Movies/clip.mov"),
        ("應用程式/Notes", "/Applications/Notes.app"),
        ("废纸篓/old.txt", "/Users/u/Trash/old.txt"),
        ("廢紙簍/old.txt", "/Users/u/Trash/old.txt"),
    ] {
        let (r, e) = dispatch(&store, query, SearchMode::Auto, 10);
        assert!(e.is_none(), "query {query} error: {e:?}");
        assert!(
            r.iter().any(|x| x.entry.path == expected_path),
            "query {query} missed {expected_path}, got {:?}",
            r.iter().map(|x| &x.entry.path).collect::<Vec<_>>()
        );
        for item in &r {
            assert_valid_highlights(
                &item.entry.path,
                &item.entry.name,
                &item.match_positions,
                &item.parent_match_positions,
            );
        }
    }

    // "个人" 位于路径首位时双向映射 "~" 与相对 "个人/"
    let (res, err) = dispatch(&store, "个人/personal_doc.txt", SearchMode::Auto, 10);
    assert!(err.is_none());
    let paths: Vec<&str> = res.iter().map(|r| r.entry.path.as_str()).collect();
    assert!(paths.contains(&format!("{home_trimmed}/personal_doc.txt").as_str()));
    assert!(paths.contains(&"/archive/个人/personal_doc.txt"));

    // 单独 "桌面"（无 '/' 且无 in:）必须保留为纯文件名检索，不匹配 Desktop 下无关子文件
    let (res, err) = dispatch(&store, "桌面", SearchMode::Auto, 10);
    assert!(err.is_none());
    let paths: Vec<&str> = res.iter().map(|r| r.entry.path.as_str()).collect();
    assert!(paths.contains(&"/Users/u/桌面/汇总.docx") || paths.contains(&"/Users/u/notes/桌面备忘.txt"));
    assert!(!paths.contains(&"/Users/u/Desktop/周报.docx"));

    // "桌面/" 召回目录本身 /Users/u/Desktop 及目录内文件，且高亮不越界
    let (res, err) = dispatch(&store, "桌面/", SearchMode::Auto, 10);
    assert!(err.is_none());
    let paths: Vec<&str> = res.iter().map(|r| r.entry.path.as_str()).collect();
    assert!(paths.contains(&"/Users/u/Desktop"));
    assert!(paths.contains(&"/Users/u/Desktop/周报.docx"));
    for item in &res {
        assert_valid_highlights(
            &item.entry.path,
            &item.entry.name,
            &item.match_positions,
            &item.parent_match_positions,
        );
    }

    // `in:下载 report` 使用段边界匹配：命中 Downloads/* 但不命中 mydownloads
    let (res, err) = dispatch(&store, "in:下载 report", SearchMode::Auto, 10);
    assert!(err.is_none());
    let paths: Vec<&str> = res.iter().map(|r| r.entry.path.as_str()).collect();
    assert!(paths.contains(&"/Users/u/Downloads/report.pdf"));
    assert!(paths.contains(&"/Users/u/Downloads/sub/report.pdf"));
    assert!(!paths.contains(&"/Users/u/mydownloads/report.pdf"));
    for item in &res {
        assert_valid_highlights(
            &item.entry.path,
            &item.entry.name,
            &item.match_positions,
            &item.parent_match_positions,
        );
    }
}

// =========================================================================
// Feature 3: POSIX 冒号文件名回退（AC/DC -> AC:DC）
// =========================================================================

#[test]
fn posix_colon_fallback_recalls_colon_filename_and_respects_all_guards() {
    let store = build_store(vec![
        make_file("/Music/Rock/AC:DC.mp3", 300),
        make_file("/Music/Rock/AC:dc_live.mp3", 300),
        make_file("/Music/Rock/ac:dc_demo.mp3", 300),
        make_file("/Notes/a:b:c.txt", 100),
    ]);

    // 正例：仅含单个 '/' 的未加引号查询无路径结果时回退为 ':'，正确高亮 AC:DC
    let (res, is_cs, err, _) = dispatch_with_details(
        &store,
        "AC/DC",
        SearchMode::Auto,
        CaseSensitivity::Auto,
        10,
    );
    assert!(err.is_none());
    assert!(is_cs, "uppercase AC/DC fallback should activate Smart Case");
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.name, "AC:DC.mp3");
    assert_eq!(res[0].match_positions, vec![(0, 5)]);

    // 全小写触发不区分大小写回退
    let (res, is_cs, err, _) = dispatch_with_details(
        &store,
        "ac/dc",
        SearchMode::Auto,
        CaseSensitivity::Auto,
        10,
    );
    assert!(err.is_none());
    assert!(!is_cs);
    assert_eq!(res.len(), 3);

    // "AC/dc" 将 "AC" 转为文件名词元后，重算 Smart Case 应激活敏感匹配，排除 ac:dc_demo
    let (res, is_cs, err, _) = dispatch_with_details(
        &store,
        "AC/dc",
        SearchMode::Auto,
        CaseSensitivity::Auto,
        10,
    );
    assert!(err.is_none());
    assert!(is_cs);
    let names: Vec<&str> = res.iter().map(|r| r.entry.name.as_str()).collect();
    assert!(names.contains(&"AC:dc_live.mp3"));
    assert!(!names.contains(&"AC:DC.mp3"));
    assert!(!names.contains(&"ac:dc_demo.mp3"));

    // 全部负例护栏：均不得触发冒号回退
    for neg_query in [
        "/AC/DC",
        "AC/DC/",
        "~/AC/DC",
        "in:Music AC/DC",
        "a/b/c",
        r#""AC/DC""#,
    ] {
        let (r, e) = dispatch(&store, neg_query, SearchMode::Auto, 10);
        assert!(e.is_none(), "neg query {neg_query} error: {e:?}");
        assert!(
            r.is_empty(),
            "negative query {neg_query} must NOT trigger colon fallback, got {:?}",
            r.iter().map(|x| &x.entry.path).collect::<Vec<_>>()
        );
    }

    // 显式 Regex 模式下不触发冒号回退
    let (r, _) = dispatch(&store, "AC/DC", SearchMode::Regex, 10);
    assert!(r.is_empty());
}

// =========================================================================
// Feature 4: 多关键词父目录 + 文件名混合召回及分级排序
// =========================================================================

#[test]
fn hybrid_parent_filename_recall_tiers_smart_case_and_fuzzy_parity() {
    let store = build_store(vec![
        make_file("/Users/alice/notes/project_readme.md", 500),
        make_file("/Users/alice/project/README.md", 500),
        make_file("/Users/alice/Project/readme_lowercase.md", 500),
        make_file("/Users/alice/project/skip_readme.md", 500),
        make_file("/Users/alice/project/.hidden_readme.md", 500),
        make_file("/Users/alice/work/2026/report.pdf", 800),
        make_file("/Users/alice/other/p_r_o_j_e_c_t_readme.txt", 100),
    ]);

    // 纯文件名命中 (project_readme.md) 必须严格高于混合父目录+文件名命中 (/project/README.md)
    let (sub_res, err) = dispatch(&store, "project readme", SearchMode::Auto, 20);
    assert!(err.is_none());
    let paths: Vec<&str> = sub_res.iter().map(|r| r.entry.path.as_str()).collect();
    let idx_pure = paths
        .iter()
        .position(|&p| p == "/Users/alice/notes/project_readme.md")
        .expect("missing pure filename hit");
    let idx_hybrid = paths
        .iter()
        .position(|&p| p == "/Users/alice/project/README.md")
        .expect("missing hybrid hit");
    assert!(
        idx_pure < idx_hybrid,
        "pure filename hit ({idx_pure}) must rank strictly above hybrid hit ({idx_hybrid}): {paths:?}"
    );
    assert!(sub_res[idx_pure].score > sub_res[idx_hybrid].score);
    assert!(!sub_res[idx_hybrid].parent_match_positions.is_empty());
    assert!(!sub_res[idx_hybrid].match_positions.is_empty());
    assert_valid_highlights(
        &sub_res[idx_hybrid].entry.path,
        &sub_res[idx_hybrid].entry.name,
        &sub_res[idx_hybrid].match_positions,
        &sub_res[idx_hybrid].parent_match_positions,
    );

    // SearchMode::Fuzzy 前缀一致性：fuzzy[..sub_res.len()] 严格一致，且混合命中高于纯 nucleo 模糊命中
    let (fuzzy_res, err) = dispatch(&store, "project readme", SearchMode::Fuzzy, 20);
    assert!(err.is_none());
    assert!(fuzzy_res.len() >= sub_res.len());
    let fuzzy_prefix_paths: Vec<&str> = fuzzy_res[..sub_res.len()]
        .iter()
        .map(|r| r.entry.path.as_str())
        .collect();
    assert_eq!(fuzzy_prefix_paths, paths);
    if let Some(idx_nucleo) = fuzzy_res
        .iter()
        .position(|r| r.entry.path == "/Users/alice/other/p_r_o_j_e_c_t_readme.txt")
    {
        assert!(
            idx_hybrid < idx_nucleo,
            "hybrid ({idx_hybrid}) must outrank pure nucleo fuzzy ({idx_nucleo})"
        );
    }

    // 文件名分片 Smart Case 独立判定：
    // "Project readme" -> 父目录含大写 "Project" 但始终不区分大小写，文件名 "readme" 全小写 -> 仍命中 README.md
    let (res, _) = dispatch(&store, "Project readme", SearchMode::Auto, 10);
    assert!(res.iter().any(|r| r.entry.path == "/Users/alice/project/README.md"));

    // "project README" -> 文件名含大写 "README" -> Smart Case 生效，只命中 README.md 排除 readme_lowercase.md
    let (res, _) = dispatch(&store, "project README", SearchMode::Auto, 10);
    let uppercase_paths: Vec<&str> = res.iter().map(|r| r.entry.path.as_str()).collect();
    assert!(uppercase_paths.contains(&"/Users/alice/project/README.md"));
    assert!(!uppercase_paths.contains(&"/Users/alice/Project/readme_lowercase.md"));

    // 否定词过滤与属性过滤生效
    let (res, _) = dispatch(&store, "project readme !skip ext:md", SearchMode::Auto, 10);
    let filtered_paths: Vec<&str> = res.iter().map(|r| r.entry.path.as_str()).collect();
    assert!(filtered_paths.contains(&"/Users/alice/project/README.md"));
    assert!(!filtered_paths.contains(&"/Users/alice/project/skip_readme.md"));

    // 3 词多段切分：work + 2026 作为父目录，report 作为文件名
    let (res, _) = dispatch(&store, "work 2026 report", SearchMode::Auto, 10);
    assert!(res.iter().any(|r| r.entry.path == "/Users/alice/work/2026/report.pdf"));

    // 引号锁定首词时不触发混合父目录拆分
    let (res, _) = dispatch(&store, r#""project" readme"#, SearchMode::Auto, 10);
    assert!(!res.iter().any(|r| r.entry.path == "/Users/alice/project/README.md"));

    // 去重检查：同时既可在文件名也可在父目录匹配的记录只出现一次
    let dedup_store = build_store(vec![make_file("/Users/alice/project/project_readme.md", 100)]);
    let (res, _) = dispatch(&dedup_store, "project readme", SearchMode::Auto, 10);
    assert_eq!(res.len(), 1);
}

// =========================================================================
// Feature 5: 未索引绝对路径单层 std::fs::read_dir 兜底列举
// =========================================================================

#[test]
fn unindexed_absolute_dir_read_dir_fallback_filters_noise_and_caps_results() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let root = temp.path();

    std::fs::create_dir(root.join("SubDir")).unwrap();
    std::fs::write(root.join("SubDir/deep.txt"), b"nested should not show").unwrap();
    std::fs::write(root.join("note.txt"), b"hello").unwrap();
    std::fs::write(root.join("skip_note.txt"), b"skip me").unwrap();
    std::fs::write(root.join("CaseFile.TXT"), b"case check").unwrap();
    std::fs::write(root.join(".DS_Store"), b"noise").unwrap();
    std::fs::write(root.join("._CaseFile.TXT"), b"appledouble").unwrap();
    std::fs::write(root.join(".hidden_file"), b"hidden").unwrap();

    let empty_store = IndexStore::new();
    let dir_query = format!("{}/", root.to_string_lossy().trim_end_matches('/'));

    // 单层兜底返回目录与直接子文件，剔除 .DS_Store 与 ._ 前缀噪音，绝不递归到 deep.txt
    let (res, err) = dispatch(&empty_store, &dir_query, SearchMode::Auto, 50);
    assert!(err.is_none());
    let names: Vec<&str> = res.iter().map(|r| r.entry.name.as_str()).collect();
    assert!(names.contains(&"SubDir"), "names: {names:?}");
    assert!(names.contains(&"note.txt"));
    assert!(names.contains(&"skip_note.txt"));
    assert!(names.contains(&"CaseFile.TXT"));
    assert!(names.contains(&".hidden_file"));
    assert!(!names.contains(&".DS_Store"));
    assert!(!names.contains(&"._CaseFile.TXT"));
    assert!(!names.contains(&"deep.txt"));

    for item in &res {
        assert!(!item.parent_match_positions.is_empty());
        assert_valid_highlights(
            &item.entry.path,
            &item.entry.name,
            &item.match_positions,
            &item.parent_match_positions,
        );
    }

    // 隐藏文件在未查询点号时应排在常规文件之后
    let idx_note = names.iter().position(|&n| n == "note.txt").unwrap();
    let idx_hidden = names.iter().position(|&n| n == ".hidden_file").unwrap();
    assert!(idx_note < idx_hidden);

    // 协同文件名过滤、排除词、ext: 与 type: 过滤器
    let filtered_q = format!("{dir_query} note !skip ext:txt");
    let (res, err) = dispatch(&empty_store, &filtered_q, SearchMode::Auto, 50);
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.name, "note.txt");

    let folder_q = format!("{dir_query} type:folder");
    let (res, err) = dispatch(&empty_store, &folder_q, SearchMode::Auto, 50);
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.name, "SubDir");
    assert!(res[0].entry.is_dir);

    // max_results == 0 时不触盘并直接返回空
    let (res_zero, _, err_zero, _) = dispatch_with_filter(
        &empty_store,
        &dir_query,
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        0,
    );
    assert!(err_zero.is_none());
    assert!(res_zero.is_empty());

    // Fuzzy 与 Regex 模式不触发 read_dir 兜底
    let (res_fuzzy, _) = dispatch(&empty_store, &dir_query, SearchMode::Fuzzy, 50);
    assert!(res_fuzzy.is_empty());
    let (res_regex, _) = dispatch(&empty_store, &dir_query, SearchMode::Regex, 50);
    assert!(res_regex.is_empty());

    // 非目录结尾绝对路径不触发 read_dir 兜底
    let non_dir_q = format!("{}/note", root.to_string_lossy().trim_end_matches('/'));
    let (res_nondir, _) = dispatch(&empty_store, &non_dir_q, SearchMode::Auto, 50);
    assert!(res_nondir.is_empty());

    // 不存在路径静默返回空
    let (res_missing, err_missing) = dispatch(
        &empty_store,
        "/definitely_nonexistent_lokii_test_dir_998877/",
        SearchMode::Auto,
        20,
    );
    assert!(err_missing.is_none());
    assert!(res_missing.is_empty());

    // 若索引已有命中项，不触发 read_dir 兜底（仅返回索引项）
    let indexed_store = build_store(vec![make_file(
        &format!("{}/indexed_only.txt", root.to_string_lossy().trim_end_matches('/')),
        10,
    )]);
    let (res_indexed, _) = dispatch(&indexed_store, &dir_query, SearchMode::Auto, 50);
    assert_eq!(res_indexed.len(), 1);
    assert_eq!(res_indexed[0].entry.name, "indexed_only.txt");

    // 500 条结果封顶保护
    let big_dir = tempfile::tempdir().expect("create big dir");
    for i in 0..520 {
        std::fs::write(big_dir.path().join(format!("item_{i:03}.txt")), b"").unwrap();
    }
    let big_q = format!("{}/", big_dir.path().to_string_lossy().trim_end_matches('/'));
    let (res_capped, err_capped) = dispatch(&empty_store, &big_q, SearchMode::Auto, 1000);
    assert!(err_capped.is_none());
    assert_eq!(res_capped.len(), 500);

    // 结构化分词保持稳定，无误报错
    let parsed = parse_query_structured(&dir_query);
    assert_eq!(parsed.positive_tokens.len(), 1);
    let _ = dispatch_with_options(&empty_store, &dir_query, SearchMode::Substring, CaseSensitivity::Auto, 5);
}

#[test]
fn fallback_read_dir_handles_non_ascii_trad_and_nfd_names() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let root = tmp.path();
    std::fs::write(root.join("周報.docx"), b"trad").unwrap();
    std::fs::write(root.join("CAFÉ.txt"), b"cafe").unwrap();
    std::fs::write(root.join("Übung.txt"), b"uebung").unwrap();
    std::fs::write(root.join("Resume\u{0301}.txt"), b"nfd").unwrap();

    let empty_store = build_store(vec![]);
    let root_str = root.to_string_lossy();
    let root_trimmed = root_str.trim_end_matches('/');

    for (query_name, expected_folded_prefix, expected_folded_match) in [
        ("周报", "周报.docx", "周报"),
        ("周報", "周报.docx", "周报"),
        ("café", "café.txt", "café"),
        ("übung", "übung.txt", "übung"),
        ("resumé", "resumé.txt", "resumé"),
    ] {
        // 以 '/' 结尾的未索引目录路径搭配过滤词触发 fallback_read_dir + score_name 非 ASCII 折叠路径
        let query = format!("{root_trimmed}/ {query_name}");
        let (res, err) = dispatch(&empty_store, &query, SearchMode::Auto, 20);
        assert!(err.is_none(), "query {query} error: {err:?}");
        assert_eq!(
            res.len(),
            1,
            "query {query} should match 1 unindexed entry, got {:?}",
            res.iter().map(|r| &r.entry.path).collect::<Vec<_>>()
        );
        assert_eq!(
            lokii_core::cjk::fold_str(&res[0].entry.name),
            expected_folded_prefix
        );
        assert_valid_highlights(
            &res[0].entry.path,
            &res[0].entry.name,
            &res[0].match_positions,
            &res[0].parent_match_positions,
        );
        assert!(
            !res[0].match_positions.is_empty(),
            "query {query} should produce non-empty match_positions on {:?}",
            res[0].entry.name
        );
        let (s, e) = res[0].match_positions[0];
        assert_eq!(
            lokii_core::cjk::fold_str(&res[0].entry.name[s..e]),
            expected_folded_match
        );
    }
}

#[test]
fn relative_alias_directory_self_enforces_segment_boundary() {
    let store = build_store(vec![
        make_dir("/Users/u/my/Desktop"),
        make_dir("/Users/u/dummy/Desktop"),
        make_dir("/Users/u/my/Downloads"),
        make_dir("/Users/u/dummy/Downloads"),
    ]);

    for (query, expected) in [
        ("my/桌面/", "/Users/u/my/Desktop"),
        ("my/下載/", "/Users/u/my/Downloads"),
    ] {
        let (res, err) = dispatch(&store, query, SearchMode::Auto, 10);
        assert!(err.is_none(), "query {query} error: {err:?}");
        let paths: Vec<&str> = res.iter().map(|r| r.entry.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![expected],
            "query {query} must enforce segment boundary and avoid dummy/ suffix false positive"
        );
    }

    let (all_desktops, err) = dispatch(&store, "Desktop/", SearchMode::Auto, 10);
    assert!(err.is_none());
    let paths: Vec<&str> = all_desktops.iter().map(|r| r.entry.path.as_str()).collect();
    assert!(paths.contains(&"/Users/u/my/Desktop"));
    assert!(paths.contains(&"/Users/u/dummy/Desktop"));
}

#[test]
fn hybrid_raw_path_record_computes_parent_highlights() {
    let raw_entry = FileEntry {
        name: "spec-v2.md".to_string(),
        path: "/Users/alice/projects/spec.md".to_string(),
        is_dir: false,
        size: 10,
        modified: 1_700_000_000,
        created_at: 1_700_000_000,
        extension: "md".to_string(),
    };
    let store = build_store(vec![raw_entry]);

    let (res, err) = dispatch(&store, "projects spec-v2.md", SearchMode::Auto, 10);
    assert!(err.is_none());
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].entry.path, "/Users/alice/projects/spec.md");
    assert_eq!(res[0].entry.name, "spec-v2.md");
    assert_eq!(res[0].match_positions, vec![(0, 10)]);
    assert_eq!(res[0].parent_match_positions, vec![(13, 21)]);
    assert_valid_highlights(
        &res[0].entry.path,
        &res[0].entry.name,
        &res[0].match_positions,
        &res[0].parent_match_positions,
    );
}
