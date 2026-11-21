use lokii_core::indexer::{FileEntry, IndexStore};
use lokii_core::search::disjunction::DisjunctionQuery;
use lokii_core::search::path_query::PathQuery;
use lokii_core::search::path_search;
use lokii_core::search::{
    dispatch, dispatch_with_options, substring, CaseSensitivity, CompiledRecordFilter, SearchMode,
};

fn make_entry(name: &str, path: &str, is_dir: bool) -> FileEntry {
    let extension = if is_dir {
        String::new()
    } else {
        std::path::Path::new(name)
            .extension()
            .map(|ext| ext.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    FileEntry {
        name: name.to_string(),
        path: path.to_string(),
        is_dir,
        size: 1024,
        modified: 1700000000,
        created_at: 0,
        extension,
    }
}

#[test]
fn stem_exact_beats_prefix_beats_word_boundary_beats_substring() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry("xreadmex.md", "/docs/xreadmex.md", false),
        make_entry("my_readme.md", "/docs/my_readme.md", false),
        make_entry("readmex.md", "/docs/readmex.md", false),
        make_entry("README_old.md", "/docs/README_old.md", false),
        make_entry("README.md", "/docs/README.md", false),
        make_entry("foo.tar.gz", "/docs/foo.tar.gz", false),
    ]);

    let (results, err) = dispatch(&store, "readme", SearchMode::Auto, 50);
    assert!(err.is_none());
    let names: Vec<_> = results.iter().map(|r| r.entry.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "README.md",
            "README_old.md",
            "readmex.md",
            "my_readme.md",
            "xreadmex.md"
        ],
        "expected StemExact > PrefixBoundary > Prefix > WordBoundary > Substring, got {names:?}"
    );
    for pair in results.windows(2) {
        assert!(
            pair[0].score > pair[1].score,
            "expected strictly decreasing scores: {} ({}) vs {} ({})",
            pair[0].entry.name,
            pair[0].score,
            pair[1].entry.name,
            pair[1].score
        );
    }

    let (stem_tar, _) = dispatch(&store, "foo.tar", SearchMode::Auto, 10);
    let (prefix_foo, _) = dispatch(&store, "foo", SearchMode::Auto, 10);
    assert_eq!(stem_tar[0].entry.name, "foo.tar.gz");
    assert_eq!(prefix_foo[0].entry.name, "foo.tar.gz");
    assert!(
        stem_tar[0].score > prefix_foo[0].score,
        "StemExact for 'foo.tar' ({}) must outscore PrefixBoundary for 'foo' ({})",
        stem_tar[0].score,
        prefix_foo[0].score
    );
}

#[test]
fn word_boundary_rules_cover_cjk_punctuation_camelcase_and_non_first_occurrence() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry("username.rs", "/src/username.rs", false),
        make_entry("userHelper.rs", "/src/userHelper.rs", false),
        make_entry("user_helper.rs", "/src/user_helper.rs", false),
        make_entry("xusera.rs", "/src/xusera.rs", false),
        make_entry("xuser_user.rs", "/src/xuser_user.rs", false),
        make_entry("超级用户系统.pdf", "/docs/超级用户系统.pdf", false),
        make_entry("【通知】用户手册.pdf", "/docs/【通知】用户手册.pdf", false),
        make_entry("工作总结.docx", "/docs/工作总结.docx", false),
        make_entry("工作—总结.docx", "/docs/工作—总结.docx", false),
        make_entry("甲乙.txt", "/docs/甲乙.txt", false),
        make_entry("（草稿）乙.txt", "/docs/（草稿）乙.txt", false),
    ]);

    let (user_res, _) = dispatch(&store, "user", SearchMode::Auto, 20);
    let user_names: Vec<_> = user_res.iter().map(|r| r.entry.name.as_str()).collect();
    let pos_helper_snake = user_names
        .iter()
        .position(|&n| n == "user_helper.rs")
        .unwrap();
    let pos_helper_camel = user_names
        .iter()
        .position(|&n| n == "userHelper.rs")
        .unwrap();
    let pos_username = user_names.iter().position(|&n| n == "username.rs").unwrap();
    let pos_xuser_user = user_names
        .iter()
        .position(|&n| n == "xuser_user.rs")
        .unwrap();
    let pos_xusera = user_names.iter().position(|&n| n == "xusera.rs").unwrap();

    assert!(
        pos_helper_snake < pos_username && pos_helper_camel < pos_username,
        "boundary prefixes must beat mid-word prefix: {user_names:?}"
    );
    assert!(
        pos_username < pos_xuser_user && pos_xuser_user < pos_xusera,
        "non-first word-boundary occurrence must outrank substring-only: {user_names:?}"
    );

    let (cjk_res, _) = dispatch(&store, "用户", SearchMode::Auto, 10);
    let cjk_names: Vec<_> = cjk_res.iter().map(|r| r.entry.name.as_str()).collect();
    assert_eq!(cjk_names, ["【通知】用户手册.pdf", "超级用户系统.pdf"]);
    assert!(cjk_res[0].score > cjk_res[1].score);

    let (dash_res, _) = dispatch(&store, "总结", SearchMode::Auto, 10);
    let dash_names: Vec<_> = dash_res.iter().map(|r| r.entry.name.as_str()).collect();
    assert_eq!(dash_names, ["工作—总结.docx", "工作总结.docx"]);
    assert!(dash_res[0].score > dash_res[1].score);

    let (paren_res, _) = dispatch(&store, "乙", SearchMode::Auto, 10);
    let paren_names: Vec<_> = paren_res.iter().map(|r| r.entry.name.as_str()).collect();
    assert_eq!(paren_names, ["（草稿）乙.txt", "甲乙.txt"]);
}

#[test]
fn app_bundles_and_directories_outrank_inner_and_deep_files() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry(
            "Safari",
            "/Applications/Safari.app/Contents/MacOS/Safari",
            false,
        ),
        make_entry("Safari.txt", "/Users/u/a/b/c/Safari.txt", false),
        make_entry("Safari.app", "/Applications/Safari.app", true),
        make_entry("docs", "/Users/u/a/b/c/docs", false),
        make_entry("docs", "/Users/u/docs", true),
    ]);

    for query in ["Safari", "safari"] {
        let (safari_res, _, _) = dispatch_with_options(
            &store,
            query,
            SearchMode::Auto,
            CaseSensitivity::Auto,
            10,
        );
        assert_eq!(
            safari_res[0].entry.path, "/Applications/Safari.app",
            "query {query:?} must rank Safari.app bundle first, got {:?}",
            safari_res
                .iter()
                .map(|r| (&r.entry.path, r.score))
                .collect::<Vec<_>>()
        );
        assert!(safari_res[0].score > safari_res[1].score);
    }

    let (docs_res, _) = dispatch(&store, "docs", SearchMode::Auto, 10);
    assert_eq!(docs_res[0].entry.path, "/Users/u/docs");
    assert!(docs_res[0].entry.is_dir);
    assert!(docs_res[0].score > docs_res[1].score);
}

#[test]
fn shallow_paths_outrank_deep_paths_across_regular_ext_and_disjunction() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry("main.rs", "/Users/u/a/b/c/d/main.rs", false),
        make_entry("main.rs", "/Users/u/doc/main.rs", false),
    ]);

    let (regular, _) = dispatch(&store, "main", SearchMode::Auto, 10);
    assert_eq!(regular[0].entry.path, "/Users/u/doc/main.rs");
    assert!(regular[0].score > regular[1].score);

    let (ext_res, _) = dispatch(&store, ".rs", SearchMode::Auto, 10);
    assert_eq!(ext_res[0].entry.path, "/Users/u/doc/main.rs");
    assert!(ext_res[0].score > ext_res[1].score);

    let (or_res, _) = dispatch(&store, "main | missing", SearchMode::Auto, 10);
    assert_eq!(or_res[0].entry.path, "/Users/u/doc/main.rs");
    assert!(or_res[0].score > or_res[1].score);

    let read = store.read();
    let q = DisjunctionQuery::parse("main | missing", None);
    let direct_or = substring::search_disjunction(
        &read,
        &q,
        &CompiledRecordFilter::default(),
        10,
        false,
    );
    assert_eq!(direct_or[0].entry.path, "/Users/u/doc/main.rs");
    assert!(direct_or[0].score > direct_or[1].score);

    let legacy_res = store.search("main", 10);
    assert_eq!(legacy_res[0].entry.path, "/Users/u/doc/main.rs");
}

#[test]
fn dotfiles_and_hidden_ancestors_are_demoted_unless_explicitly_targeted() {
    let store = IndexStore::new();
    store.replace(vec![
        make_entry(".config", "/Users/u/.config", false),
        make_entry("config", "/.hidden/config", false),
        make_entry("config.toml", "/Users/u/config.toml", false),
        make_entry(".foo.toml", "/Users/u/.foo.toml", false),
        make_entry("hidden_file.toml", "/.hidden/hidden_file.toml", false),
        make_entry("bar.toml", "/Users/u/bar.toml", false),
        make_entry(".config_foo", "/Users/u/.config_foo", false),
        make_entry("my_foo_config.txt", "/Users/u/my_foo_config.txt", false),
    ]);

    let (cfg_res, _) = dispatch(&store, "config", SearchMode::Auto, 20);
    let cfg_paths: Vec<_> = cfg_res.iter().map(|r| r.entry.path.as_str()).collect();
    assert_eq!(
        &cfg_paths[..3],
        [
            "/Users/u/config.toml",
            "/Users/u/my_foo_config.txt",
            "/.hidden/config"
        ],
        "expected non-hidden config.toml & my_foo_config.txt > hidden-ancestor FullExact > dotfiles, got {cfg_paths:?}"
    );
    assert!(
        cfg_paths
            .iter()
            .position(|&p| p == "/.hidden/config")
            .unwrap()
            < cfg_paths
                .iter()
                .position(|&p| p == "/Users/u/.config")
                .unwrap()
    );
    assert!(cfg_res[0].score > cfg_res[1].score);
    assert!(cfg_res[1].score > cfg_res[2].score);

    let (dot_cfg, _) = dispatch(&store, ".config", SearchMode::Auto, 10);
    assert_eq!(dot_cfg[0].entry.path, "/Users/u/.config");
    assert!(dot_cfg[0].score > 0.9);

    let (multi_dot_cfg, _) = dispatch(&store, "foo .config", SearchMode::Auto, 10);
    assert_eq!(multi_dot_cfg[0].entry.path, "/Users/u/.config_foo");

    let (ext_toml, _) = dispatch(&store, ".toml", SearchMode::Auto, 10);
    let toml_paths: Vec<_> = ext_toml.iter().map(|r| r.entry.path.as_str()).collect();
    assert!(
        toml_paths.iter().position(|&p| p == "/Users/u/bar.toml").unwrap()
            < toml_paths.iter().position(|&p| p == "/Users/u/.foo.toml").unwrap()
    );
    assert!(
        toml_paths.iter().position(|&p| p == "/Users/u/bar.toml").unwrap()
            < toml_paths
                .iter()
                .position(|&p| p == "/.hidden/hidden_file.toml")
                .unwrap()
    );
}

#[test]
fn large_duplicate_bucket_never_truncates_shallow_entries() {
    let mut entries = Vec::with_capacity(600);
    for i in 0..550 {
        entries.push(make_entry(
            "index.ts",
            &format!("/a/b/c/d/e/f/g/h{i}/index.ts"),
            false,
        ));
    }
    for i in 0..50 {
        entries.push(make_entry("index.ts", &format!("/s{i}/index.ts"), false));
    }

    let store = IndexStore::new();
    store.replace(entries);

    let (top50, _) = dispatch(&store, "index.ts", SearchMode::Auto, 50);
    assert_eq!(top50.len(), 50);
    assert!(
        top50.iter().all(|r| r.entry.path.starts_with("/s")),
        "all 50 shallow index.ts entries must win over 550 deep ones, first 5: {:?}",
        top50.iter().take(5).map(|r| &r.entry.path).collect::<Vec<_>>()
    );

    let (top10, _) = dispatch(&store, "index.ts", SearchMode::Auto, 10);
    assert_eq!(top10.len(), 10);
    assert!(top10.iter().all(|r| r.entry.path.starts_with("/s")));

    let (ext10, _) = dispatch(&store, ".ts", SearchMode::Auto, 10);
    assert_eq!(ext10.len(), 10);
    assert!(ext10.iter().all(|r| r.entry.path.starts_with("/s")));

    let pq = PathQuery::parse("index.ts").is_none();
    assert!(pq);
    let _ = path_search::search;
}

#[test]
fn pinyin_fuzzy_and_path_search_respect_composite_ranking_and_hidden_rules() {
    use lokii_core::indexer::MatchScore;
    use lokii_core::search::ranking::{MatchTier, RankScore};

    // 1. 拼音全拼前缀 > 拼音全拼包含 > 首字母含括
    let pinyin_store = IndexStore::new();
    pinyin_store.replace(vec![
        make_entry("说明wj笔记.txt", "/d/说明wj笔记.txt", false),
        make_entry("文件报告.txt", "/d/文件报告.txt", false),
        make_entry("项目文件说明.txt", "/d/项目文件说明.txt", false),
        make_entry("wxyz.txt", "/d/wxyz.txt", false),
        make_entry("文件.txt", "/d/文件.txt", false),
    ]);
    let (py_res, _) = dispatch(&pinyin_store, "wenjian", SearchMode::Auto, 10);
    let py_names: Vec<_> = py_res.iter().map(|r| r.entry.name.as_str()).collect();
    assert_eq!(
        py_names,
        ["文件.txt", "文件报告.txt", "项目文件说明.txt"],
        "pinyin prefix-full must outrank substring-full, got {py_names:?}"
    );
    assert!(py_res[0].score > py_res[1].score);
    assert!(py_res[1].score > py_res[2].score);

    // 2. Fuzzy 模式也按浅层优先、降权隐藏祖先路径，并在显式点号时恢复权重
    let fuzzy_store = IndexStore::new();
    fuzzy_store.replace(vec![
        make_entry("app_config.json", "/.hidden/app_config.json", false),
        make_entry(".app_config.json", "/repo/.app_config.json", false),
        make_entry("app_config.json", "/a/b/c/d/app_config.json", false),
        make_entry("app_config.json", "/repo/app_config.json", false),
    ]);
    let (fz_res, _) = dispatch(&fuzzy_store, "apcfg", SearchMode::Fuzzy, 10);
    let fz_paths: Vec<_> = fz_res.iter().map(|r| r.entry.path.as_str()).collect();
    assert_eq!(
        fz_paths[0], "/repo/app_config.json",
        "shallow non-hidden fuzzy hit must win, got {fz_paths:?}"
    );
    assert_eq!(
        fz_paths[1], "/a/b/c/d/app_config.json",
        "deep non-hidden fuzzy hit must beat hidden hits, got {fz_paths:?}"
    );
    assert!(fz_res[0].score > fz_res[1].score && fz_res[1].score > fz_res[2].score);

    let (fz_dot, _) = dispatch(&fuzzy_store, ".apcfg", SearchMode::Fuzzy, 10);
    assert_eq!(fz_dot[0].entry.path, "/repo/.app_config.json");
    assert!(fz_dot[0].score > fz_res[2].score);

    // 3. PathQuery 搜索同样偏好浅路径与降权隐藏项，且显式瞄准点号段恢复权重
    let path_store = IndexStore::new();
    path_store.replace(vec![
        make_entry("settings.json", "/Users/u/.config/app/settings.json", false),
        make_entry("settings.json", "/Users/u/work/a/b/app/settings.json", false),
        make_entry("settings.json", "/Users/u/app/settings.json", false),
    ]);
    let (path_res, _) = dispatch(&path_store, "app/settings", SearchMode::Auto, 10);
    assert_eq!(path_res[0].entry.path, "/Users/u/app/settings.json");
    assert_eq!(
        path_res.last().unwrap().entry.path,
        "/Users/u/.config/app/settings.json"
    );

    let (dot_path_res, _) = dispatch(&path_store, ".config/settings", SearchMode::Auto, 10);
    assert_eq!(
        dot_path_res[0].entry.path,
        "/Users/u/.config/app/settings.json"
    );

    // 4. RankScore::to_match_score 映射验证
    assert_eq!(
        RankScore::assemble(MatchTier::FullExact, 0, 0, false).to_match_score(),
        MatchScore::Exact
    );
    assert_eq!(
        RankScore::assemble(MatchTier::StemExact, 0, 0, false).to_match_score(),
        MatchScore::Prefix
    );
    assert_eq!(
        RankScore::assemble(MatchTier::WordBoundary, 1, 0, false).to_match_score(),
        MatchScore::Substring
    );
}
