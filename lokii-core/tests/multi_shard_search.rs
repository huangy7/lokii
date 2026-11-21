use lokii_core::compact_index::CompactIndex;
use lokii_core::indexer::{FileEntry, IndexStore};
use lokii_core::search::{
    dispatch_multi, dispatch_with_filter, CaseSensitivity, QueryFilter, SearchMode, SearchShard,
};

fn make_entry(path: &str, modified: u64) -> FileEntry {
    let p = std::path::Path::new(path);
    let name = p.file_name().unwrap().to_str().unwrap().to_string();
    let extension = p
        .extension()
        .map(|e| e.to_str().unwrap().to_lowercase())
        .unwrap_or_default();
    FileEntry {
        name,
        path: path.to_string(),
        is_dir: false,
        size: 1024,
        modified,
        created_at: 0,
        extension,
    }
}

#[test]
fn test_multi_shard_offline_filtering() {
    let shard_online = CompactIndex::from_entries(vec![
        make_entry("/Users/user/online_doc.txt", 100),
    ])
    .unwrap();
    let shard_offline = CompactIndex::from_entries(vec![
        make_entry("/Volumes/Backup/offline_doc.txt", 200),
    ])
    .unwrap();

    let shards = vec![
        SearchShard {
            index: &shard_online,
            configured_path: Some("/Users/user"),
            active_path: Some("/Users/user"),
            volume_name: None,
            is_offline: false,
        },
        SearchShard {
            index: &shard_offline,
            configured_path: Some("/Volumes/Backup"),
            active_path: Some("/Volumes/Backup"),
            volume_name: Some("Backup"),
            is_offline: true,
        },
    ];

    // include_offline = false
    let (res_online_only, _, _, _) = dispatch_multi(
        &shards,
        "doc",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        false,
    );
    assert_eq!(res_online_only.len(), 1);
    assert_eq!(res_online_only[0].result.entry.name, "online_doc.txt");
    assert!(!res_online_only[0].is_offline);

    // include_offline = true
    let (res_all, _, _, _) = dispatch_multi(
        &shards,
        "doc",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );
    assert_eq!(res_all.len(), 2);
    let offline_item = res_all.iter().find(|r| r.is_offline).unwrap();
    assert_eq!(offline_item.result.entry.name, "offline_doc.txt");
    assert_eq!(offline_item.volume_name.as_deref(), Some("Backup"));
}

#[test]
fn test_multi_shard_wildcard_star_modified_total_order() {
    let shard_a = CompactIndex::from_entries(vec![
        make_entry("/Volumes/VolA/alpha.txt", 1000),
        make_entry("/Volumes/VolA/gamma.txt", 3000),
    ])
    .unwrap();
    let shard_b = CompactIndex::from_entries(vec![
        make_entry("/Volumes/VolB/beta.txt", 2000),
        make_entry("/Volumes/VolB/delta.txt", 4000),
    ])
    .unwrap();

    let shards = vec![
        SearchShard {
            index: &shard_a,
            configured_path: Some("/Volumes/VolA"),
            active_path: Some("/Volumes/VolA"),
            volume_name: Some("VolA"),
            is_offline: false,
        },
        SearchShard {
            index: &shard_b,
            configured_path: Some("/Volumes/VolB"),
            active_path: Some("/Volumes/VolB"),
            volume_name: Some("VolB"),
            is_offline: false,
        },
    ];

    let (res, _, _, _) = dispatch_multi(
        &shards,
        "*",
        SearchMode::Wildcard,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );

    assert_eq!(res.len(), 4);
    let names: Vec<&str> = res.iter().map(|r| r.result.entry.name.as_str()).collect();
    // Expected modified DESC: 4000 (delta), 3000 (gamma), 2000 (beta), 1000 (alpha)
    assert_eq!(names, vec!["delta.txt", "gamma.txt", "beta.txt", "alpha.txt"]);
}

#[test]
fn test_multi_shard_fuzzy_two_phase_and_raw_score_order() {
    // Shard A has a low-score prefix/substring match "foo_bar.rs" for query "foo"
    // Shard B has jump-char matches "f_o_o_short.rs" and "f_o_o_much_longer_name.rs"
    let shard_a = CompactIndex::from_entries(vec![
        make_entry("/Users/user/foo_bar.rs", 100),
        make_entry("/Users/user/f_x_o_o_a.rs", 100),
    ])
    .unwrap();
    let shard_b = CompactIndex::from_entries(vec![
        make_entry("/Volumes/External/f_o_o_short.rs", 200),
        make_entry("/Volumes/External/f_x_o_o_b.rs", 200),
    ])
    .unwrap();

    let shards = vec![
        SearchShard {
            index: &shard_a,
            configured_path: Some("/Users/user"),
            active_path: Some("/Users/user"),
            volume_name: None,
            is_offline: false,
        },
        SearchShard {
            index: &shard_b,
            configured_path: Some("/Volumes/External"),
            active_path: Some("/Volumes/External"),
            volume_name: Some("External"),
            is_offline: false,
        },
    ];

    let (res, _, _, _) = dispatch_multi(
        &shards,
        "foo",
        SearchMode::Fuzzy,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );

    // "foo_bar.rs" is Phase 1 (substring match, Prefix MatchScore = 0.75)
    // It MUST be first before any Phase 2 jump-char matches!
    assert!(!res.is_empty());
    assert_eq!(res[0].result.entry.name, "foo_bar.rs");

    // Next should be Phase 2 fuzzy jump matches sorted by raw score DESC.
    // "f_o_o_short.rs" matches 'f', 'o', 'o' more compactly than "f_x_o_o_..."
    let names: Vec<&str> = res.iter().map(|r| r.result.entry.name.as_str()).collect();
    assert_eq!(names[0], "foo_bar.rs");
    assert_eq!(names[1], "f_o_o_short.rs");
}

#[test]
fn test_multi_shard_union_inferred_ext() {
    let shard_a = CompactIndex::from_entries(vec![
        make_entry("/Users/user/report_archive.cust_bak", 100),
    ])
    .unwrap();
    let shard_b = CompactIndex::from_entries(vec![
        make_entry("/Volumes/Work/report.cust", 200),
    ])
    .unwrap();

    let shards = vec![
        SearchShard {
            index: &shard_a,
            configured_path: Some("/Users/user"),
            active_path: Some("/Users/user"),
            volume_name: None,
            is_offline: false,
        },
        SearchShard {
            index: &shard_b,
            configured_path: Some("/Volumes/Work"),
            active_path: Some("/Volumes/Work"),
            volume_name: Some("Work"),
            is_offline: false,
        },
    ];

    let (res, _, _, _) = dispatch_multi(
        &shards,
        "report .cust",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );

    assert_eq!(res.len(), 1);
    assert_eq!(res[0].result.entry.name, "report.cust");
    assert_eq!(res[0].result.entry.path, "/Volumes/Work/report.cust");
}

#[test]
fn test_multi_shard_union_single_token_dot_ext() {
    let shard_a = CompactIndex::from_entries(vec![
        make_entry("/Users/user/notes.custom", 100),
    ])
    .unwrap();
    let shard_b = CompactIndex::from_entries(vec![
        make_entry("/Volumes/Extra/my_custom_doc.txt", 200),
    ])
    .unwrap();

    let shards = vec![
        SearchShard {
            index: &shard_a,
            configured_path: Some("/Users/user"),
            active_path: Some("/Users/user"),
            volume_name: None,
            is_offline: false,
        },
        SearchShard {
            index: &shard_b,
            configured_path: Some("/Volumes/Extra"),
            active_path: Some("/Volumes/Extra"),
            volume_name: Some("Extra"),
            is_offline: false,
        },
    ];

    // Query ".custom": Shard A has extension "custom". Shard B does NOT have extension "custom".
    // Shard B must not fall back to substring search for ".custom" and match "my_custom_doc.txt".
    let (res, _, _, _) = dispatch_multi(
        &shards,
        ".custom",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );

    assert_eq!(res.len(), 1);
    assert_eq!(res[0].result.entry.name, "notes.custom");

    // Cross-shard .custom vs dotfile .custom_note (empty extension ""):
    // Shard A has dotfile ".custom_note" (extension = ""), Shard B has "demo.custom".
    // Querying ".custom" must return ONLY "demo.custom", never falling back to match ".custom_note".
    let dot_entry = make_entry("/Users/user/.custom_note", 100);
    assert_eq!(dot_entry.extension, "");
    let shard_dotfile = CompactIndex::from_entries(vec![dot_entry]).unwrap();
    let shard_ext = CompactIndex::from_entries(vec![
        make_entry("/Volumes/Extra/demo.custom", 200),
    ])
    .unwrap();
    let cross_shards = vec![
        SearchShard {
            index: &shard_dotfile,
            configured_path: Some("/Users/user"),
            active_path: Some("/Users/user"),
            volume_name: None,
            is_offline: false,
        },
        SearchShard {
            index: &shard_ext,
            configured_path: Some("/Volumes/Extra"),
            active_path: Some("/Volumes/Extra"),
            volume_name: Some("Extra"),
            is_offline: false,
        },
    ];

    let (res_cross, _, _, _) = dispatch_multi(
        &cross_shards,
        ".custom",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );
    assert_eq!(res_cross.len(), 1);
    assert_eq!(res_cross[0].result.entry.name, "demo.custom");

    // Querying ".custom_note" directly (dotfile query) still finds ".custom_note".
    let (res_dot_query, _, _, _) = dispatch_multi(
        &cross_shards,
        ".custom_note",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );
    assert_eq!(res_dot_query.len(), 1);
    assert_eq!(res_dot_query[0].result.entry.name, ".custom_note");
}

#[test]
fn test_multi_shard_drift_alias_rewriting() {
    // Shard was configured at "/Volumes/Share", but remounted to "/Volumes/Share-1"
    let shard = CompactIndex::from_entries(vec![
        make_entry("/Volumes/Share-1/docs/specification.pdf", 100),
        make_entry("/Volumes/Share-1/budget.xlsx", 200),
    ])
    .unwrap();

    let shards = vec![SearchShard {
        index: &shard,
        configured_path: Some("/Volumes/Share"),
        active_path: Some("/Volumes/Share-1"),
        volume_name: Some("Share"),
        is_offline: false,
    }];

    // 1. in: query with configured path
    let (res1, _, _, _) = dispatch_multi(
        &shards,
        "in:/Volumes/Share spec",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );
    assert_eq!(res1.len(), 1);
    assert_eq!(res1[0].result.entry.name, "specification.pdf");
    assert_eq!(
        res1[0].result.entry.path,
        "/Volumes/Share-1/docs/specification.pdf"
    );

    // 2. Absolute path query with configured path
    let (res2, _, _, _) = dispatch_multi(
        &shards,
        "/Volumes/Share/docs/spec",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );
    assert_eq!(res2.len(), 1);
    assert_eq!(res2[0].result.entry.name, "specification.pdf");

    // 3. Boundary safety: "/Volumes/ShareExtra" must NOT match
    let (res3, _, _, _) = dispatch_multi(
        &shards,
        "in:/Volumes/ShareExtra spec",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );
    assert!(res3.is_empty());

    // 4. Case-insensitivity on macOS path prefix: in:/volumes/share
    let (res4, _, _, _) = dispatch_multi(
        &shards,
        "in:/volumes/share spec",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );
    assert_eq!(res4.len(), 1);
    assert_eq!(res4[0].result.entry.name, "specification.pdf");
}

#[test]
fn test_multi_shard_single_store_equivalence() {
    let all_entries = vec![
        make_entry("/Users/user/Documents/plan.md", 100),
        make_entry("/Users/user/Projects/src/main.rs", 200),
        make_entry("/Volumes/NAS/Work/budget.xlsx", 300),
        make_entry("/Volumes/NAS/Work/report.pdf", 400),
        make_entry("/Volumes/USB/photos/vacation.jpg", 500),
    ];

    let single_store = IndexStore::new();
    single_store.replace(all_entries.clone());

    let shard_local = CompactIndex::from_entries(vec![
        all_entries[0].clone(),
        all_entries[1].clone(),
    ])
    .unwrap();
    let shard_nas = CompactIndex::from_entries(vec![
        all_entries[2].clone(),
        all_entries[3].clone(),
    ])
    .unwrap();
    let shard_usb = CompactIndex::from_entries(vec![
        all_entries[4].clone(),
    ])
    .unwrap();

    let shards = vec![
        SearchShard {
            index: &shard_local,
            configured_path: Some("/Users/user"),
            active_path: Some("/Users/user"),
            volume_name: None,
            is_offline: false,
        },
        SearchShard {
            index: &shard_nas,
            configured_path: Some("/Volumes/NAS"),
            active_path: Some("/Volumes/NAS"),
            volume_name: Some("NAS"),
            is_offline: false,
        },
        SearchShard {
            index: &shard_usb,
            configured_path: Some("/Volumes/USB"),
            active_path: Some("/Volumes/USB"),
            volume_name: Some("USB"),
            is_offline: false,
        },
    ];

    // Test queries across Auto, Substring, Wildcard, Regex, and Disjunction
    let test_queries = [
        ("report", SearchMode::Auto),
        ("plan | budget", SearchMode::Auto),
        ("*.rs", SearchMode::Wildcard),
        ("regex:vac.*\\.jpg", SearchMode::Regex),
        ("budget", SearchMode::Substring),
    ];

    for (q, mode) in test_queries {
        let (single_res, _, _, _) = dispatch_with_filter(
            &single_store,
            q,
            mode,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            50,
        );
        let (multi_res, _, _, _) = dispatch_multi(
            &shards,
            q,
            mode,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            50,
            true,
        );

        let single_paths: Vec<&str> = single_res.iter().map(|r| r.entry.path.as_str()).collect();
        let multi_paths: Vec<&str> = multi_res.iter().map(|r| r.result.entry.path.as_str()).collect();
        assert_eq!(
            single_paths, multi_paths,
            "Equivalence failed for query '{q}' in mode {mode:?}"
        );
    }
}

#[test]
fn test_multi_shard_empty_and_zero_limits() {
    let shard = CompactIndex::from_entries(vec![
        make_entry("/Users/user/doc.txt", 100),
    ])
    .unwrap();

    let shards = vec![SearchShard {
        index: &shard,
        configured_path: Some("/Users/user"),
        active_path: Some("/Users/user"),
        volume_name: None,
        is_offline: false,
    }];

    // 1. Empty shards slice
    let (res_empty_shards, is_case, err, mode) = dispatch_multi(
        &[],
        "doc",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );
    assert!(res_empty_shards.is_empty());
    assert!(!is_case);
    assert!(err.is_none());
    assert_eq!(mode, SearchMode::Auto);

    // 2. max_results == 0
    let (res_zero_limit, _, _, _) = dispatch_multi(
        &shards,
        "doc",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        0,
        true,
    );
    assert!(res_zero_limit.is_empty());

    // 3. All shards offline with include_offline = false
    let shard_off = CompactIndex::from_entries(vec![
        make_entry("/Volumes/NAS/file.txt", 100),
    ])
    .unwrap();
    let offline_shards = vec![SearchShard {
        index: &shard_off,
        configured_path: Some("/Volumes/NAS"),
        active_path: Some("/Volumes/NAS"),
        volume_name: Some("NAS"),
        is_offline: true,
    }];
    let (res_none_active, _, _, _) = dispatch_multi(
        &offline_shards,
        "file",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        false,
    );
    assert!(res_none_active.is_empty());
}

#[test]
fn test_multi_shard_fallback_when_no_shard_has_extension() {
    let shard_a = CompactIndex::from_entries(vec![
        make_entry("/Users/user/test .fake", 100),
    ])
    .unwrap();
    let shard_b = CompactIndex::from_entries(vec![
        make_entry("/Volumes/NAS/other.txt", 200),
    ])
    .unwrap();

    let shards = vec![
        SearchShard {
            index: &shard_a,
            configured_path: Some("/Users/user"),
            active_path: Some("/Users/user"),
            volume_name: None,
            is_offline: false,
        },
        SearchShard {
            index: &shard_b,
            configured_path: Some("/Volumes/NAS"),
            active_path: Some("/Volumes/NAS"),
            volume_name: Some("NAS"),
            is_offline: false,
        },
    ];

    let (res, _, _, _) = dispatch_multi(
        &shards,
        "test .fake",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );

    assert_eq!(res.len(), 1);
    assert_eq!(res[0].result.entry.name, "test .fake");

    // Single token dot query when no shard has extension ".unknown"
    let shard_c = CompactIndex::from_entries(vec![
        make_entry("/Users/user/.unknown", 100),
    ])
    .unwrap();
    let shards_c = vec![SearchShard {
        index: &shard_c,
        configured_path: Some("/Users/user"),
        active_path: Some("/Users/user"),
        volume_name: None,
        is_offline: false,
    }];
    let (res_dot, _, _, _) = dispatch_multi(
        &shards_c,
        ".unknown",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );
    assert_eq!(res_dot.len(), 1);
    assert_eq!(res_dot[0].result.entry.name, ".unknown");
}

#[test]
fn test_multi_shard_smart_case_and_regex_case() {
    let shard_a = CompactIndex::from_entries(vec![
        make_entry("/Users/user/Report.txt", 100),
        make_entry("/Users/user/report.txt", 100),
    ])
    .unwrap();

    let shards = vec![SearchShard {
        index: &shard_a,
        configured_path: Some("/Users/user"),
        active_path: Some("/Users/user"),
        volume_name: None,
        is_offline: false,
    }];

    // 1. Lowercase query in Smart Case -> case-insensitive (both matched)
    let (res_lower, is_case_lower, _, _) = dispatch_multi(
        &shards,
        "report",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );
    assert!(!is_case_lower);
    assert_eq!(res_lower.len(), 2);

    // 2. Uppercase query in Smart Case -> case-sensitive (only Report.txt matched)
    let (res_upper, is_case_upper, _, _) = dispatch_multi(
        &shards,
        "Report",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );
    assert!(is_case_upper);
    assert_eq!(res_upper.len(), 1);
    assert_eq!(res_upper[0].result.entry.name, "Report.txt");
}

#[test]
fn test_multi_shard_fuzzy_phase1_saturates_limit() {
    let shard_a = CompactIndex::from_entries(vec![
        make_entry("/Users/user/apple_one.txt", 100),
        make_entry("/Users/user/apple_two.txt", 100),
    ])
    .unwrap();
    let shard_b = CompactIndex::from_entries(vec![
        make_entry("/Volumes/NAS/a_p_p_l_e.txt", 100),
    ])
    .unwrap();

    let shards = vec![
        SearchShard {
            index: &shard_a,
            configured_path: Some("/Users/user"),
            active_path: Some("/Users/user"),
            volume_name: None,
            is_offline: false,
        },
        SearchShard {
            index: &shard_b,
            configured_path: Some("/Volumes/NAS"),
            active_path: Some("/Volumes/NAS"),
            volume_name: Some("NAS"),
            is_offline: false,
        },
    ];

    // max_results = 2: Phase 1 produces 2 exact/prefix substring matches from Shard A
    // Phase 2 is skipped completely, so Shard B jump-char match is NOT included
    let (res, _, _, _) = dispatch_multi(
        &shards,
        "apple",
        SearchMode::Fuzzy,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        2,
        true,
    );

    assert_eq!(res.len(), 2);
    assert!(res.iter().all(|r| r.result.entry.name.starts_with("apple_")));
}

#[test]
fn test_multi_shard_path_highlight_bounds_safety() {
    let shard = CompactIndex::from_entries(vec![
        make_entry("/Volumes/Remount-1/sub/deep/file.txt", 100),
    ])
    .unwrap();

    let shards = vec![SearchShard {
        index: &shard,
        configured_path: Some("/Volumes/Remount"),
        active_path: Some("/Volumes/Remount-1"),
        volume_name: Some("Remount"),
        is_offline: false,
    }];

    let (res, _, _, _) = dispatch_multi(
        &shards,
        "in:/Volumes/Remount file",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        10,
        true,
    );

    assert_eq!(res.len(), 1);
    let item = &res[0].result;
    assert_eq!(item.entry.path, "/Volumes/Remount-1/sub/deep/file.txt");

    // Check parent highlight range bounds
    for &(start, end) in &item.parent_match_positions {
        assert!(start < end);
        assert!(end <= item.entry.path.len());
    }
}

#[test]
fn test_multi_shard_subpath_mount_root_drift_alias_rewriting() {
    let shard_drifted = CompactIndex::from_entries(vec![
        make_entry("/Volumes/Share-1/Projects/doc.txt", 100),
        make_entry("/Volumes/Share-1/Projects/nested/spec.md", 200),
    ])
    .unwrap();
    let shard_sibling = CompactIndex::from_entries(vec![
        make_entry("/Volumes/ShareExtra/Projects/doc.txt", 300),
    ])
    .unwrap();

    let shards = vec![
        SearchShard {
            index: &shard_drifted,
            configured_path: Some("/Volumes/Share/Projects"),
            active_path: Some("/Volumes/Share-1/Projects"),
            volume_name: Some("Share"),
            is_offline: false,
        },
        SearchShard {
            index: &shard_sibling,
            configured_path: Some("/Volumes/ShareExtra/Projects"),
            active_path: Some("/Volumes/ShareExtra/Projects"),
            volume_name: Some("ShareExtra"),
            is_offline: false,
        },
    ];

    // 1. Volume-root in: scope matches drifted subpath shard, excluding ShareExtra
    let (res_scope, _, _, _) = dispatch_multi(
        &shards,
        "in:/Volumes/Share doc.txt",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );
    assert_eq!(res_scope.len(), 1);
    assert_eq!(
        res_scope[0].result.entry.path,
        "/Volumes/Share-1/Projects/doc.txt"
    );

    // 2. Configured subpath absolute query matches drifted subpath shard, excluding ShareExtra
    let (res_abs, _, _, _) = dispatch_multi(
        &shards,
        "/Volumes/Share/Projects/doc.txt",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );
    assert_eq!(res_abs.len(), 1);
    assert_eq!(
        res_abs[0].result.entry.path,
        "/Volumes/Share-1/Projects/doc.txt"
    );

    // 3. Sibling ShareExtra query matches ONLY ShareExtra, never Share-1
    let (res_sibling, _, _, _) = dispatch_multi(
        &shards,
        "/Volumes/ShareExtra/Projects/doc.txt",
        SearchMode::Auto,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );
    assert_eq!(res_sibling.len(), 1);
    assert_eq!(
        res_sibling[0].result.entry.path,
        "/Volumes/ShareExtra/Projects/doc.txt"
    );
}

#[test]
fn test_multi_shard_fuzzy_with_in_scope_matches_single_store() {
    let entries = vec![
        make_entry("/Volumes/Share/doc_alpha.txt", 100),
        make_entry("/Volumes/Share/sub/doc_beta.txt", 200),
        make_entry("/Users/user/doc_alpha.txt", 300),
        make_entry("/Volumes/ShareExtra/doc_alpha.txt", 400),
    ];

    let single_store = IndexStore::new();
    single_store.replace(entries.clone());

    let shard_share = CompactIndex::from_entries(vec![entries[0].clone(), entries[1].clone()]).unwrap();
    let shard_local = CompactIndex::from_entries(vec![entries[2].clone()]).unwrap();
    let shard_extra = CompactIndex::from_entries(vec![entries[3].clone()]).unwrap();

    let shards = vec![
        SearchShard {
            index: &shard_share,
            configured_path: Some("/Volumes/Share"),
            active_path: Some("/Volumes/Share"),
            volume_name: Some("Share"),
            is_offline: false,
        },
        SearchShard {
            index: &shard_local,
            configured_path: Some("/Users/user"),
            active_path: Some("/Users/user"),
            volume_name: None,
            is_offline: false,
        },
        SearchShard {
            index: &shard_extra,
            configured_path: Some("/Volumes/ShareExtra"),
            active_path: Some("/Volumes/ShareExtra"),
            volume_name: Some("ShareExtra"),
            is_offline: false,
        },
    ];

    let (single_res, _, _, single_mode) = dispatch_with_filter(
        &single_store,
        "in:/Volumes/Share doc",
        SearchMode::Fuzzy,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
    );
    let (multi_res, _, _, multi_mode) = dispatch_multi(
        &shards,
        "in:/Volumes/Share doc",
        SearchMode::Fuzzy,
        QueryFilter::default(),
        CaseSensitivity::Auto,
        50,
        true,
    );

    assert_eq!(single_mode, SearchMode::Fuzzy);
    assert_eq!(multi_mode, SearchMode::Fuzzy);

    let single_paths: Vec<&str> = single_res.iter().map(|r| r.entry.path.as_str()).collect();
    let multi_paths: Vec<&str> = multi_res
        .iter()
        .map(|r| r.result.entry.path.as_str())
        .collect();

    assert_eq!(single_paths, multi_paths);
    assert_eq!(multi_paths.len(), 2);
    assert!(multi_paths.iter().all(|p| p.starts_with("/Volumes/Share/")));
}

#[test]
fn test_multi_shard_non_ascii_drift_parent_highlight_utf8_safety() {
    let shard = CompactIndex::from_entries(vec![
        make_entry("/Volumes/中文卷-1/子目录/报告.txt", 100),
    ])
    .unwrap();

    let shards = vec![SearchShard {
        index: &shard,
        configured_path: Some("/Volumes/中文卷"),
        active_path: Some("/Volumes/中文卷-1"),
        volume_name: Some("中文卷"),
        is_offline: false,
    }];

    for query in ["in:/Volumes/中文卷 报告", "/Volumes/中文卷/子目录/报告.txt"] {
        let (res, _, _, _) = dispatch_multi(
            &shards,
            query,
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            10,
            true,
        );
        assert_eq!(res.len(), 1, "failed query: {query}");
        let item = &res[0].result;
        assert_eq!(item.entry.path, "/Volumes/中文卷-1/子目录/报告.txt");
        assert!(!item.parent_match_positions.is_empty());

        let parent = item
            .entry
            .path
            .rsplit_once('/')
            .map(|(p, _)| p)
            .unwrap();
        for &(start, end) in &item.parent_match_positions {
            assert!(start < end);
            assert!(end <= parent.len());
            assert!(parent.is_char_boundary(start));
            assert!(parent.is_char_boundary(end));
            let slice = &parent[start..end];
            assert!(slice.starts_with("/Volumes/中文卷-1"));
        }
    }
}

#[test]
fn test_multi_shard_disjunction_with_inferred_ext_equivalence() {
    let entries = vec![
        make_entry("/Users/user/alpha.cust", 100),
        make_entry("/Volumes/Work/beta.cust", 200),
        make_entry("/Volumes/Work/alpha.txt", 300),
    ];

    let single_store = IndexStore::new();
    single_store.replace(entries.clone());

    let shard_a = CompactIndex::from_entries(vec![entries[0].clone()]).unwrap();
    let shard_b = CompactIndex::from_entries(vec![entries[1].clone(), entries[2].clone()]).unwrap();

    let shards = vec![
        SearchShard {
            index: &shard_a,
            configured_path: Some("/Users/user"),
            active_path: Some("/Users/user"),
            volume_name: None,
            is_offline: false,
        },
        SearchShard {
            index: &shard_b,
            configured_path: Some("/Volumes/Work"),
            active_path: Some("/Volumes/Work"),
            volume_name: Some("Work"),
            is_offline: false,
        },
    ];

    for q in ["alpha | beta .cust", "alpha | .missing"] {
        let (single_res, single_case, _, single_mode) = dispatch_with_filter(
            &single_store,
            q,
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            50,
        );
        let (multi_res, multi_case, _, multi_mode) = dispatch_multi(
            &shards,
            q,
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            50,
            true,
        );
        assert_eq!(single_case, multi_case, "case mismatch for {q}");
        assert_eq!(single_mode, multi_mode, "mode mismatch for {q}");
        let single_paths: Vec<&str> = single_res.iter().map(|r| r.entry.path.as_str()).collect();
        let multi_paths: Vec<&str> = multi_res
            .iter()
            .map(|r| r.result.entry.path.as_str())
            .collect();
        assert_eq!(single_paths, multi_paths, "paths mismatch for {q}");
    }
}

#[test]
fn test_multi_shard_live_fallback_respects_offline_filter() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("live.txt"), "live").unwrap();
    let path = dir.path().to_str().unwrap();
    let query = format!("{path}/");
    let empty = CompactIndex::default();
    let primary = SearchShard {
        index: &empty,
        configured_path: None,
        active_path: None,
        volume_name: None,
        is_offline: false,
    };
    let offline = SearchShard {
        index: &empty,
        configured_path: Some(path),
        active_path: None,
        volume_name: Some("Offline"),
        is_offline: true,
    };
    let search = |shards: &[SearchShard<'_>]| {
        dispatch_multi(
            shards,
            &query,
            SearchMode::Auto,
            QueryFilter::default(),
            CaseSensitivity::Auto,
            20,
            false,
        )
        .0
    };
    assert_eq!(search(&[primary]).len(), 1);
    assert!(search(&[primary, offline]).is_empty());
}
