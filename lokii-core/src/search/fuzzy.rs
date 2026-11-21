use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::compact_index::CompactIndex;
use crate::indexer::IndexStore;

use super::{AdvancedSearchResult, CompiledRecordFilter, SearchMode};

/// 模糊检索器，接入预编译位掩码记录过滤器以达成零堆分配预筛选。
///
/// Returns results sorted by fuzzy score descending. Entries where nucleo
/// returns None (no match) are excluded -- trusting nucleo's threshold per D-08.
///
/// match_positions are individual character indices converted to byte-range pairs
/// of (byte_start, byte_start + char_len) for each matched character.
pub fn search(
    store: &IndexStore,
    query: &str,
    filter: &CompiledRecordFilter,
    max_results: usize,
    case_sensitive: bool,
) -> Vec<AdvancedSearchResult> {
    let read = store.read();
    search_index(&read, query, filter, max_results, case_sensitive)
}

/// 将 `nucleo_matcher` 产出的索引位置转换为原始字符串 `orig` 中的安全 UTF-8 字符簇字节区间。
/// 纯 ASCII 走零分配直接坐标映射；非 ASCII 根据 haystack 底层编码形态（全 ASCII 起首簇走字节偏移，
/// Unicode 簇流走 UAX #29 字符簇序号）定位归一化字节区间后通过 `map_norm_span_to_orig` 投影。
fn map_fuzzy_indices_to_orig(orig: &str, norm: &str, haystack: Utf32Str<'_>, indices: &[u32]) -> Vec<(usize, usize)> {
    if orig.is_ascii() && norm.is_ascii() && orig.len() == norm.len() {
        return indices
            .iter()
            .filter_map(|&idx| {
                let start = idx as usize;
                (start < orig.len()).then_some((start, start + 1))
            })
            .collect();
    }
    use unicode_segmentation::UnicodeSegmentation;
    let norm_graphemes: Vec<(usize, &str)> = norm.grapheme_indices(true).collect();
    let is_ascii_haystack = matches!(haystack, Utf32Str::Ascii(_));
    indices
        .iter()
        .filter_map(|&idx| {
            let idx = idx as usize;
            let (norm_start, norm_end) = if is_ascii_haystack {
                let end = norm_graphemes
                    .iter()
                    .find(|&&(gs, _)| gs == idx)
                    .map(|&(gs, g)| gs + g.len())
                    .unwrap_or_else(|| (idx + 1).min(norm.len()));
                (idx, end)
            } else {
                let &(gs, g) = norm_graphemes.get(idx)?;
                (gs, gs + g.len())
            };
            super::map_norm_span_to_orig(orig, norm, norm_start, norm_end)
        })
        .collect()
}

/// 接收统一快照读锁的模糊检索实现，委托结构化正向词元实现以避免重复路径。
pub fn search_index(
    read: &CompactIndex,
    query: &str,
    filter: &CompiledRecordFilter,
    max_results: usize,
    case_sensitive: bool,
) -> Vec<AdvancedSearchResult> {
    let tokens: Vec<super::filter::QueryToken> = query
        .split_whitespace()
        .map(|s| super::filter::QueryToken {
            text: s.to_string(),
            quoted: false,
            negated: false,
            raw: s.to_string(),
            had_escaped_meta: false,
            was_sanitized_pasted_path: false,
        })
        .collect();
    search_index_tokens(read, &tokens, filter, max_results, case_sensitive)
}

/// 基于结构化正向词元的模糊检索实现：仅正向词元参与模式构造与隐藏文件瞄准判定（`!.hidden` 不消除隐藏惩罚）。
pub fn search_index_tokens(
    read: &CompactIndex,
    positive_tokens: &[super::filter::QueryToken],
    filter: &CompiledRecordFilter,
    max_results: usize,
    case_sensitive: bool,
) -> Vec<AdvancedSearchResult> {
    search_index_tokens_inner(
        read,
        positive_tokens,
        filter,
        max_results,
        case_sensitive,
        None,
    )
    .into_iter()
    .map(|(result, _)| result)
    .collect()
}

pub(crate) fn search_index_tokens_raw(
    read: &CompactIndex,
    positive_tokens: &[super::filter::QueryToken],
    filter: &CompiledRecordFilter,
    max_results: usize,
    case_sensitive: bool,
    seen: &std::collections::HashSet<String>,
) -> Vec<(AdvancedSearchResult, u32)> {
    search_index_tokens_inner(
        read,
        positive_tokens,
        filter,
        max_results,
        case_sensitive,
        Some(seen),
    )
}

fn search_index_tokens_inner(
    read: &CompactIndex,
    positive_tokens: &[super::filter::QueryToken],
    filter: &CompiledRecordFilter,
    max_results: usize,
    case_sensitive: bool,
    raw_order_excluding: Option<&std::collections::HashSet<String>>,
) -> Vec<(AdvancedSearchResult, u32)> {
    if positive_tokens.is_empty() || max_results == 0 {
        return Vec::new();
    }
    let joined: String = positive_tokens
        .iter()
        .map(|t| t.text.as_str())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let query = joined.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let query_norm = crate::cjk::normalize_and_simplify_cow(query);
    let case_matching = if case_sensitive {
        CaseMatching::Respect
    } else {
        CaseMatching::Ignore
    };
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::new(
        &query_norm,
        case_matching,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    // Fuzzy 匹配：由 NameSignature 内部统一封装查询串特征位构造
    let req = crate::string_pool::NameSignature::from_fuzzy_query(&query_norm);
    let targets_hidden = positive_tokens.iter().any(|t| t.text.starts_with('.'));
    let rank_ctx = super::ranking::RecordRankContext::default();
    let mut buffer = Vec::new();
    let mut score_name = |name: &str| {
        let name_norm = crate::cjk::normalize_and_simplify_cow(name);
        let haystack = Utf32Str::new(&name_norm, &mut buffer);
        pattern.score(haystack, &mut matcher)
    };
    let matches: Vec<_> = if let Some(seen) = raw_order_excluding {
        let mut top = super::top_k::TopK::new(max_results);
        let mut path = String::new();
        for &name_id in &read.sorted_names().ids {
            if read
                .name_signature(name_id)
                .is_some_and(|sig| !sig.contains(req))
            {
                continue;
            }
            let Some((name, _)) = read.name_entry(name_id) else {
                continue;
            };
            let Some(raw_score) = score_name(name) else {
                continue;
            };
            for &ordinal in read.entries_for_name(name_id) {
                let ordinal = ordinal as usize;
                if !filter.matches_record_in(read, &read.records()[ordinal], Some(name)) {
                    continue;
                }
                let Some(view) = read.view(crate::compact_index::EntryId(ordinal)) else {
                    continue;
                };
                if !seen.is_empty() {
                    view.path_into(&mut path);
                    if seen.contains(&path) {
                        continue;
                    }
                }
                top.offer_path(ordinal, raw_score, view);
            }
        }
        top.finish()
            .into_iter()
            .map(|candidate| {
                let view = read
                    .view(crate::compact_index::EntryId(candidate.ordinal))
                    .unwrap();
                let base = super::ranking::BaseNameScore::for_fuzzy(view.name, candidate.score);
                let display_score = super::ranking::score_record(
                    base,
                    view.is_dir,
                    view.parent,
                    &rank_ctx,
                    targets_hidden,
                );
                (candidate.ordinal, display_score, candidate.score)
            })
            .collect()
    } else {
        super::rank_unique_names(
            read,
            filter,
            max_results,
            req,
            targets_hidden,
            &rank_ctx,
            |name, _| {
                score_name(name).map(|raw| super::ranking::BaseNameScore::for_fuzzy(name, raw))
            },
        )
        .into_iter()
        .map(|candidate| (candidate.ordinal, candidate.score, 0))
        .collect()
    };
    let mut indices = Vec::new();
    matches
        .into_iter()
        .map(|(ordinal, score, raw_score)| {
            let view = read.view(crate::compact_index::EntryId(ordinal)).unwrap();
            let name_norm = crate::cjk::normalize_and_simplify_cow(view.name);
            let haystack = Utf32Str::new(&name_norm, &mut buffer);
            indices.clear();
            pattern.indices(haystack, &mut matcher, &mut indices);
            let positions = map_fuzzy_indices_to_orig(view.name, &name_norm, haystack, &indices);
            let result = AdvancedSearchResult {
                entry: view.materialize(),
                score: score.to_f64(),
                match_positions: positions,
                parent_match_positions: Vec::new(),
                search_mode: SearchMode::Fuzzy,
            };
            (result, raw_score)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::{FileEntry, IndexStore};

    fn make_entry(name: &str) -> FileEntry {
        let extension = std::path::Path::new(name)
            .extension()
            .map(|ext| ext.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        FileEntry {
            name: name.to_string(),
            path: format!("/{name}"),
            is_dir: false,
            size: 1024,
            modified: 1700000000,
            created_at: 0,
            extension,
        }
    }

    #[test]
    fn test_fuzzy_basic() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs"), make_entry("readme.md")]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "mnrs", &filter, 500, false);
        assert!(!results.is_empty(), "mnrs should fuzzy match main.rs");
        assert_eq!(results[0].entry.name, "main.rs");
        assert_eq!(results[0].search_mode, SearchMode::Fuzzy);
    }

    #[test]
    fn test_fuzzy_no_match() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs")]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "xxxxxx", &filter, 500, false);
        assert!(results.is_empty());
    }

    #[test]
    fn test_fuzzy_case_sensitive() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs"), make_entry("MAIN.rs")]);
        let filter = CompiledRecordFilter::default();

        let sensitive: Vec<_> = search(&store, "MAIN", &filter, 500, true)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert_eq!(sensitive, ["MAIN.rs"]);

        let insensitive: Vec<_> = search(&store, "MAIN", &filter, 500, false)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert_eq!(insensitive.len(), 2);
    }

    #[test]
    fn test_fuzzy_match_positions() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs")]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "mnrs", &filter, 500, false);
        assert!(
            !results[0].match_positions.is_empty(),
            "Should have match positions for highlighted chars"
        );
    }

    #[test]
    fn test_fuzzy_score_ranking() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("main.rs"),
            make_entry("my_amazing_narrative.rs"),
        ]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "mnrs", &filter, 500, false);
        // main.rs should score higher (shorter, more compact match)
        if results.len() >= 2 {
            assert!(results[0].score >= results[1].score);
        }
    }

    #[test]
    fn test_fuzzy_empty_query() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs")]);
        let filter = CompiledRecordFilter::default();
        let results = search(&store, "", &filter, 500, false);
        assert!(results.is_empty());
    }

    #[test]
    fn test_fuzzy_with_compiled_filter() {
        let store = IndexStore::new();
        store.replace(vec![make_entry("main.rs"), make_entry("main.md")]);
        let read = store.read();
        let query_filter = super::super::QueryFilter {
            file_type: super::super::FileType::Document,
            custom_ext: None,
            ..Default::default()
        };
        let filter = CompiledRecordFilter::compile(&read, &query_filter);
        drop(read);

        let results = search(&store, "mn", &filter, 500, false);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "main.md");
    }

    #[test]
    fn test_fuzzy_matches_accented_filenames() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("café.txt"),
            make_entry("naïve.md"),
            make_entry("中文文档.txt"),
        ]);
        let filter = CompiledRecordFilter::default();

        let cafe_results: Vec<_> = search(&store, "cafe", &filter, 500, false)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert_eq!(cafe_results, ["café.txt"]);

        let naive_results: Vec<_> = search(&store, "naive", &filter, 500, false)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert_eq!(naive_results, ["naïve.md"]);
    }

    /// 签名预筛只能做超集剪枝：nucleo 判定的每个命中，其索引签名必须包含查询要求的全部位。
    /// 覆盖汉字、繁体、假名、谚文、变音拉丁与纯 ASCII，防止收紧预筛后静默漏结果。
    #[test]
    fn signature_prefilter_never_drops_a_real_match() {
        let names = [
            "作业.docx", "作业本", "zuoye.txt", "哈哈哈.txt", "amplitude.txt",
            "cafe.md", "café.md", "檔案.txt", "档案.txt", "ファイル.txt", "한글.txt",
            "example.rs", "hc_a.txt", "Hadoopabc.md",
        ];
        let store = IndexStore::new();
        store.replace(names.iter().map(|name| make_entry(name)).collect());

        let queries = [
            "作业", "哈哈哈", "档案", "檔案", "cafe", "café", "abcd", "ha", "la",
            "ファイル", "한글", "xyz",
        ];
        // 直接检查索引里真实生成的签名，避免在测试里复刻折叠规则
        let read = store.read();
        let index = read.query_index();
        let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);
        let mut buffer = Vec::new();
        let mut checked = 0usize;

        for query in queries {
            let query_fold = crate::cjk::to_simplified_str(query);
            let required = crate::string_pool::NameSignature::from_fuzzy_query(&query_fold);
            let pattern = nucleo_matcher::pattern::Pattern::new(
                &query_fold,
                nucleo_matcher::pattern::CaseMatching::Ignore,
                nucleo_matcher::pattern::Normalization::Smart,
                nucleo_matcher::pattern::AtomKind::Fuzzy,
            );
            for name_id in 0..index.slot_count() as u32 {
                if !index.is_ready(name_id) {
                    continue;
                }
                let Some(name) = read.name_text(name_id) else {
                    continue;
                };
                // 与模糊检索一致：打分输入是折叠后的文件名，不是原始名
                let name_cjk = crate::cjk::to_simplified_str(name);
                let matched = pattern
                    .score(nucleo_matcher::Utf32Str::new(&name_cjk, &mut buffer), &mut matcher)
                    .is_some();
                if !matched {
                    continue;
                }
                checked += 1;
                assert!(
                    index.signature_at(name_id).contains(required),
                    "签名预筛会漏掉命中: q={query} name={name}"
                );
            }
        }
        assert!(checked > 0, "用例应至少产生一个真实命中");
    }

    #[test]
    fn cjk_fuzzy_query_still_finds_results() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("作业.docx"),
            make_entry("作业本"),
            make_entry("zuoye.txt"),
        ]);
        let filter = CompiledRecordFilter::default();

        let names: Vec<_> = search(&store, "作业", &filter, 500, false)
            .into_iter()
            .map(|r| r.entry.name)
            .collect();
        assert!(names.contains(&"作业.docx".to_string()), "汉字模糊检索结果: {names:?}");
        assert!(names.contains(&"作业本".to_string()), "汉字模糊检索结果: {names:?}");
    }
}
