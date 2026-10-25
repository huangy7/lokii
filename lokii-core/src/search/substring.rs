use crate::compact_index::CompactIndex;
use crate::indexer::{score_match_zero_alloc, IndexStore, MatchScore};
use crate::string_pool::NameSignature;

use super::{
    find_positions, is_dotfile_query, AdvancedSearchResult, CompiledRecordFilter, SearchMode,
};

/// Convert MatchScore to normalized f64 per research recommendation.
fn score_to_f64(score: MatchScore) -> f64 {
    match score {
        MatchScore::Exact => 1.0,
        MatchScore::Prefix => 0.75,
        MatchScore::Substring => 0.5,
    }
}

/// 子串检索器，接入预编译位掩码记录过滤器以达成零堆分配预筛选。
/// 保持原有语义：扩展名过滤、多关键词 AND 匹配、Exact > Prefix > Substring 排序。
pub fn search(
    store: &IndexStore,
    query: &str,
    filter: &CompiledRecordFilter,
    max_results: usize,
) -> Vec<AdvancedSearchResult> {
    let read = store.read();
    search_index(&read, query, filter, max_results)
}

/// 接收统一快照读锁的子串检索实现，杜绝锁重入与快照位掩码漂移。
pub fn search_index(
    read: &CompactIndex,
    query: &str,
    filter: &CompiledRecordFilter,
    max_results: usize,
) -> Vec<AdvancedSearchResult> {
    let query = query.trim();
    if max_results == 0 {
        return Vec::new();
    }
    let mut extension = (!is_dotfile_query(query)
        && query.starts_with('.')
        && !query.contains(' '))
    .then(|| query[1..].to_lowercase());

    // 扩展名回退机制（Fallback Guard）：
    // 若查询形式形似单扩展名（以单点开头），但在当前索引库中查无该扩展名记录
    // （例如用户正在键入的隐藏点文件前缀 .giti、.myse 等），
    // 绝不可直接返回空列表，而应将 extension 撤销为 None，
    // 顺畅回退至常规子串匹配流程，避免静默漏搜。
    if let Some(ref ext) = extension {
        if read.extension_id(ext).is_none() {
            extension = None;
        }
    }
    let keywords: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let matches = match &extension {
        Some(extension) => {
            let Some(ext_id) = read.extension_id(extension) else {
                return Vec::new();
            };
            let req = if extension.is_empty() {
                NameSignature::default()
            } else {
                NameSignature::from_substring_query(".")
                    .union(NameSignature::from_substring_query(extension))
            };
            let ext_ascii = extension.is_ascii();
            let mut top = super::top_k::TopK::new(max_results);
            let sorted = read.sorted_names();
            for &name_id in &sorted.ids {
                if !extension.is_empty() {
                    if let Some(sig) = read.name_signature(name_id) {
                        if !sig.contains(req) {
                            continue;
                        }
                    }
                }
                let Some((name, folded)) = read.name_entry(name_id) else {
                    continue;
                };
                let matched = if extension.is_empty() {
                    true
                } else if let Some(f) = folded {
                    f.rsplit_once('.').is_some_and(|(_, ext)| ext == extension)
                } else if ext_ascii {
                    name.rsplit_once('.')
                        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case(extension))
                } else {
                    // `folded` 为 None 意味着该名字小写折叠后与原串一致（如中文或已小写的重音扩展名）
                    name.rsplit_once('.').is_some_and(|(_, ext)| ext == extension)
                };
                if !matched {
                    continue;
                }
                for &ordinal in read.entries_for_name(name_id) {
                    let record = &read.records()[ordinal as usize];
                    if record.extension == ext_id {
                        if !filter.matches_record(record, || Some(name)) {
                            continue;
                        }
                        top.offer(ordinal as usize, MatchScore::Exact, name);
                        if top.is_full() {
                            break;
                        }
                    }
                }
                if top.is_full() {
                    break;
                }
            }
            top.finish()
        }
        None => {
            if query.is_empty() {
                if filter.is_passthrough() && !filter.has_explicit_type_all() {
                    Vec::new()
                } else {
                    let mut top = super::top_k::TopK::new(max_results);
                    let records = read.records();
                    for (id, name) in read.names() {
                        if let Some(record) = records.get(id.0) {
                            if !filter.matches_record(record, || Some(name)) {
                                continue;
                            }
                        }
                        top.offer(id.0, MatchScore::Exact, name);
                    }
                    top.finish()
                }
            } else {
                let mut req = NameSignature::default();
                for kw in &keywords {
                    req = req.union(NameSignature::from_substring_query(kw));
                }
                let all_ascii = keywords.iter().all(|k| k.is_ascii());
                super::rank_unique_names(&read, filter, max_results, req, |name, folded| {
                    score_match_zero_alloc(name, folded, &keywords, all_ascii)
                })
            }
        }
    };
    matches
        .into_iter()
        .map(|candidate| {
            let view = read
                .view(crate::compact_index::EntryId(candidate.ordinal))
                .unwrap();
            AdvancedSearchResult {
                entry: view.materialize(),
                score: score_to_f64(candidate.score),
                match_positions: if extension.is_some() || query.is_empty() {
                    Vec::new()
                } else {
                    find_positions(
                        &view.name.to_lowercase(),
                        keywords.iter().map(String::as_str),
                    )
                },
                parent_match_positions: Vec::new(),
                search_mode: SearchMode::Substring,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::{FileEntry, IndexStore};

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
    fn test_substring_basic() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("main.rs", "/src/main.rs", false),
            make_entry("readme.md", "/readme.md", false),
        ]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "main", &filter, 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "main.rs");
        assert_eq!(results[0].search_mode, SearchMode::Substring);
        assert!(results[0].score > 0.0);
    }

    #[test]
    fn test_substring_match_positions() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs", "/main.rs", false)]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "main", &filter, 500);
        assert!(!results[0].match_positions.is_empty());
        assert_eq!(results[0].match_positions[0], (0, 4));
    }

    #[test]
    fn test_extension_filter_uses_field() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("report.pdf", "/report.pdf", false),
            make_entry("data.csv", "/data.csv", false),
        ]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, ".pdf", &filter, 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "report.pdf");
    }

    #[test]
    fn test_empty_query_returns_empty() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("file.txt", "/file.txt", false)]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "", &filter, 500);
        assert!(results.is_empty());
    }

    #[test]
    fn test_ranking_prefers_prefix_over_substring() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("main.rs", "/main.rs", false), // prefix match for "main"
            make_entry("my_main.rs", "/my_main.rs", false), // substring match
        ]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "main", &filter, 500);
        assert_eq!(results.len(), 2);
        // Prefix should score higher than substring
        assert!(results[0].score > results[1].score);
        assert_eq!(results[0].score, 0.75); // prefix
        assert_eq!(results[1].score, 0.5); // substring
    }

    #[test]
    fn test_substring_with_compiled_filter() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("main.rs", "/src/main.rs", false),
            make_entry("main.md", "/main.md", false),
        ]);
        let read = store.read();
        let query_filter = crate::search::QueryFilter {
            file_type: crate::search::FileType::Code,
            custom_ext: None,
            ..Default::default()
        };
        let filter = CompiledRecordFilter::compile(&read, &query_filter);
        drop(read);

        let results = search(&store, "main", &filter, 500);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "main.rs");
    }

    #[test]
    fn test_non_ascii_extensions_and_bare_dot() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("u.文档", "/u.文档", false),
            make_entry("x.café", "/x.café", false),
            make_entry("Makefile", "/Makefile", false),
            make_entry("foo.", "/foo.", false),
            make_entry("noext", "/noext", false),
            make_entry("main.rs", "/main.rs", false),
        ]);
        let filter = CompiledRecordFilter::default();

        let doc_names: Vec<_> = search(&store, ".文档", &filter, 500)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert_eq!(doc_names, ["u.文档"]);

        let cafe_names: Vec<_> = search(&store, ".café", &filter, 500)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert_eq!(cafe_names, ["x.café"]);

        let bare_dot_names: Vec<_> = search(&store, ".", &filter, 500)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert_eq!(bare_dot_names, ["Makefile", "foo.", "noext"]);

        let rs_names: Vec<_> = search(&store, ".rs", &filter, 500)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert_eq!(rs_names, ["main.rs"]);

        let store_doc: Vec<_> = store
            .search(".文档", 500)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert_eq!(store_doc, ["u.文档"]);
    }

    #[test]
    fn test_dotfiles_searchable_in_auto_and_substring() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry(".env", "/project/.env", false),
            make_entry(".env.local", "/project/.env.local", false),
            make_entry(".gitignore", "/project/.gitignore", false),
            make_entry("test.gitignore", "/project/test.gitignore", false),
            make_entry(".zshrc", "/Users/me/.zshrc", false),
            make_entry(".bashrc", "/Users/me/.bashrc", false),
            make_entry(".npmrc", "/project/.npmrc", false),
            make_entry(".dockerignore", "/project/.dockerignore", false),
            make_entry(".mysecret", "/project/.mysecret", false),
            make_entry("regular.rs", "/project/regular.rs", false),
        ]);
        let filter = CompiledRecordFilter::default();

        // 1. .env
        let env_res = search(&store, ".env", &filter, 500);
        let env_names: Vec<_> = env_res.iter().map(|r| r.entry.name.as_str()).collect();
        assert!(env_names.contains(&".env"));
        assert!(env_names.contains(&".env.local"));

        // 2. .gitignore 完整匹配与前缀匹配（如用户键入 .giti / .gitig 时）
        let gitignore_res = search(&store, ".gitignore", &filter, 500);
        let gitignore_names: Vec<_> = gitignore_res.iter().map(|r| r.entry.name.as_str()).collect();
        assert!(gitignore_names.contains(&".gitignore"));
        assert!(gitignore_names.contains(&"test.gitignore"));

        let giti_res = search(&store, ".giti", &filter, 500);
        let giti_names: Vec<_> = giti_res.iter().map(|r| r.entry.name.as_str()).collect();
        assert!(giti_names.contains(&".gitignore"));
        assert!(giti_names.contains(&"test.gitignore"));

        // 3. 未知点文件前缀（如 .myse 查找 .mysecret，数据库中无 myse 扩展名，无缝回退子串匹配）
        let myse_res = search(&store, ".myse", &filter, 500);
        assert_eq!(myse_res.len(), 1);
        assert_eq!(myse_res[0].entry.name, ".mysecret");

        // 3. .zshrc
        let zshrc_res = search(&store, ".zshrc", &filter, 500);
        assert_eq!(zshrc_res.len(), 1);
        assert_eq!(zshrc_res[0].entry.name, ".zshrc");

        // 4. .bashrc
        let bashrc_res = search(&store, ".bashrc", &filter, 500);
        assert_eq!(bashrc_res.len(), 1);
        assert_eq!(bashrc_res[0].entry.name, ".bashrc");

        // 5. .npmrc
        let npmrc_res = search(&store, ".npmrc", &filter, 500);
        assert_eq!(npmrc_res.len(), 1);
        assert_eq!(npmrc_res[0].entry.name, ".npmrc");

        // 6. .dockerignore
        let dockerignore_res = search(&store, ".dockerignore", &filter, 500);
        assert_eq!(dockerignore_res.len(), 1);
        assert_eq!(dockerignore_res[0].entry.name, ".dockerignore");

        // 7. 常规扩展名 .rs 依然走扩展名检索
        let rs_res = search(&store, ".rs", &filter, 500);
        assert_eq!(rs_res.len(), 1);
        assert_eq!(rs_res[0].entry.name, "regular.rs");
    }

    #[test]
    fn test_type_all_returns_top_k() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("file1.txt", "/file1.txt", false),
            make_entry("file2.rs", "/file2.rs", false),
        ]);

        let (results_empty, _) = crate::search::dispatch(&store, "", SearchMode::Auto, 500);
        assert!(results_empty.is_empty(), "纯空搜索框应当返回空列表");

        let (results_type_all, _) = crate::search::dispatch(&store, "type:all", SearchMode::Auto, 500);
        assert_eq!(results_type_all.len(), 2, "显式 type:all 应当返回前 Top-K 结果");

        let (results_type_all_query, _) = crate::search::dispatch(&store, "type:all file1", SearchMode::Auto, 500);
        assert_eq!(results_type_all_query.len(), 1);
        assert_eq!(results_type_all_query[0].entry.name, "file1.txt");
    }
}
