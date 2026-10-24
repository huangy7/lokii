use crate::indexer::{score_match_zero_alloc, IndexStore, MatchScore};
use crate::string_pool::NameSignature;

use super::{find_positions, AdvancedSearchResult, CompiledRecordFilter, SearchMode};

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
    let query = query.trim();
    if max_results == 0 {
        return Vec::new();
    }
    let extension =
        (query.starts_with('.') && !query.contains(' ')).then(|| query[1..].to_lowercase());
    let keywords: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let read = store.read();
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
                if filter.is_passthrough() {
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
}
