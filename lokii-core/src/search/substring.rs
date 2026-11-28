use std::collections::{HashMap, HashSet};

use crate::compact_index::CompactIndex;
use crate::indexer::IndexStore;
use crate::string_pool::NameSignature;

use super::disjunction::{extension_matches, extension_signature};
use super::filter::QueryToken;
use super::path_query::PathCandidate;
use super::ranking::{
    bucket_slices, keywords_target_hidden, score_name_rank, score_record_with_meta, BaseNameScore,
    MatchTier, RankScore, RecordRankContext,
};
use super::{
    find_positions, is_dotfile_query, map_norm_span_to_orig, merge_highlight_spans,
    resolve_case_sensitive_tokens, AdvancedSearchResult, CaseSensitivity, CompiledRecordFilter,
    DisjunctionQuery, SearchMode,
};

/// 子串检索器，接入预编译位掩码记录过滤器以达成零堆分配预筛选。
/// 保持原有语义：扩展名过滤、多关键词 AND 匹配、Exact > Prefix > Substring 排序。
pub fn search(
    store: &IndexStore,
    query: &str,
    filter: &CompiledRecordFilter,
    max_results: usize,
) -> Vec<AdvancedSearchResult> {
    let read = store.read();
    search_index(&read, query, filter, max_results, false, false)
}

/// 扩展名池槽位匹配器：纯 ASCII 退化为单一整数 ID 等值比较；非 ASCII 使用位图覆盖多规范化变体槽位。
pub(crate) enum ExtMatcher {
    ExactId(u32),
    Mask(Vec<bool>),
}

impl ExtMatcher {
    pub(crate) fn compile(read: &CompactIndex, extension: &str) -> Option<Self> {
        if extension.is_ascii() {
            read.extension_id(extension).map(Self::ExactId)
        } else {
            let mask = read.evaluate_extensions(|slot| {
                super::filter::ext_matches_folded(slot, extension)
            });
            mask.iter().any(|&m| m).then_some(Self::Mask(mask))
        }
    }

    #[inline]
    pub(crate) fn matches(&self, ext_id: u32) -> bool {
        match self {
            Self::ExactId(id) => ext_id == *id,
            Self::Mask(mask) => mask.get(ext_id as usize).copied().unwrap_or(false),
        }
    }
}

/// 接收统一快照读锁的子串检索实现，将空白分隔词元转为未加引号词元后委托 `search_index_tokens`。
pub fn search_index(
    read: &CompactIndex,
    query: &str,
    filter: &CompiledRecordFilter,
    max_results: usize,
    case_sensitive: bool,
    explicit_lock: bool,
) -> Vec<AdvancedSearchResult> {
    let tokens: Vec<QueryToken> = query
        .split_whitespace()
        .map(|s| QueryToken {
            text: s.to_string(),
            quoted: false,
            negated: false,
            raw: s.to_string(),
            had_escaped_meta: false,
            was_sanitized_pasted_path: false,
        })
        .collect();
    search_index_tokens(
        read,
        &tokens,
        filter,
        max_results,
        case_sensitive,
        explicit_lock,
    )
}

/// 基于结构化正向词元的子串检索核心实现：保留带引号短语完整性（不拆词且不误触发 `.ext` 扩展名快路径）。
pub fn search_index_tokens(
    read: &CompactIndex,
    positive_tokens: &[QueryToken],
    filter: &CompiledRecordFilter,
    max_results: usize,
    case_sensitive: bool,
    explicit_lock: bool,
) -> Vec<AdvancedSearchResult> {
    search_index_tokens_with_ordinals(
        read,
        positive_tokens,
        filter,
        max_results,
        case_sensitive,
        explicit_lock,
        None,
        super::SearchSort::Relevance,
    )
    .0
}

pub(crate) fn single_inferred_extension(tokens: &[QueryToken]) -> Option<String> {
    let [token] = tokens else { return None };
    let single = token.text.trim();
    (!token.quoted && single.starts_with('.') && !single.contains(' ') && !is_dotfile_query(single))
        .then(|| crate::cjk::fold_str(&single[1..]))
}

/// 在纯文件名子串检索未填满 `max_results` 时，按相邻切分追加 `parent_term + filename_term` 混合召回。
#[allow(clippy::too_many_arguments)]
pub(crate) fn search_index_tokens_with_hybrid_and_sort(
    read: &CompactIndex,
    positive_tokens: &[QueryToken],
    filter: &CompiledRecordFilter,
    max_results: usize,
    case_sensitive: bool,
    case_sensitivity: CaseSensitivity,
    explicit_lock: bool,
    inferred_extension_exists: Option<bool>,
    sort: super::SearchSort,
) -> Vec<AdvancedSearchResult> {
    let (mut results, matched_ordinals) = search_index_tokens_with_ordinals(
        read,
        positive_tokens,
        filter,
        max_results,
        case_sensitive,
        explicit_lock,
        inferred_extension_exists,
        sort,
    );
    if sort == super::SearchSort::Relevance {
        let remaining = max_results.saturating_sub(results.len());
        if remaining == 0 || positive_tokens.len() < 2 {
            return results;
        }

        append_hybrid_parent_name_hits(
            read,
            positive_tokens,
            filter,
            remaining,
            case_sensitivity,
            &matched_ordinals,
            sort,
            &mut results,
        );
        return results;
    }

    if positive_tokens.len() >= 2 && max_results > 0 {
        append_hybrid_parent_name_hits(
            read,
            positive_tokens,
            filter,
            max_results,
            case_sensitivity,
            &matched_ordinals,
            sort,
            &mut results,
        );
        results.sort_by(|a, b| super::cmp_advanced_by_sort(a, b, sort));
        results.truncate(max_results);
    }
    results
}

#[allow(clippy::too_many_arguments)]
fn search_index_tokens_with_ordinals(
    read: &CompactIndex,
    positive_tokens: &[QueryToken],
    filter: &CompiledRecordFilter,
    max_results: usize,
    case_sensitive: bool,
    explicit_lock: bool,
    inferred_extension_exists: Option<bool>,
    sort: super::SearchSort,
) -> (Vec<AdvancedSearchResult>, Vec<usize>) {
    if max_results == 0 {
        return (Vec::new(), Vec::new());
    }
    let mut extension = if inferred_extension_exists == Some(false) {
        None
    } else {
        single_inferred_extension(positive_tokens)
    };

    // 扩展名回退机制（Fallback Guard）：
    // 若查询形式形似单扩展名（以单点开头），但在当前索引库中查无该扩展名记录
    // （例如用户正在键入的隐藏点文件前缀 .giti、.myse 等），
    // 绝不可直接返回空列表，而应将 extension 撤销为 None，
    // 顺畅回退至常规子串匹配流程，避免静默漏搜。
    let compiled_ext = extension
        .as_deref()
        .and_then(|ext| ExtMatcher::compile(read, ext));
    if extension.is_some() && compiled_ext.is_none() {
        if inferred_extension_exists == Some(true) {
            return (Vec::new(), Vec::new());
        }
        extension = None;
    }
    let keywords: Vec<String> = positive_tokens
        .iter()
        .filter(|t| !t.text.is_empty())
        .map(|t| {
            if case_sensitive {
                crate::cjk::normalize_and_simplify_cow(&t.text).into_owned()
            } else {
                crate::cjk::fold_str(&t.text)
            }
        })
        .collect();
    let raw_keywords: Vec<String> = positive_tokens
        .iter()
        .filter(|t| !t.text.is_empty())
        .map(|t| crate::cjk::normalize_nfc_cow(&t.text).into_owned())
        .collect();
    let rank_ctx = RecordRankContext::default();
    let targets_hidden = keywords_target_hidden(extension.is_some(), &keywords);

    let matches: Vec<(usize, f64)> = match (&extension, compiled_ext.as_ref()) {
        (Some(extension), Some(ext_matcher)) => {
            let req = extension_signature(extension);
            let ext_ascii = extension.is_ascii();
            if sort != super::SearchSort::Relevance {
                let mut top: super::top_k::TopK<'_, u64> = super::top_k::TopK::new(max_results);
                let query_index = read.query_index();
                let records = read.records();
                let passthrough = filter.is_passthrough();
                for name_id in 0..query_index.slot_count() as u32 {
                    if !extension.is_empty() {
                        let sig = query_index.signature_at(name_id);
                        if sig == NameSignature::default() && !query_index.is_ready(name_id) {
                            continue;
                        }
                        if !sig.contains(req) {
                            continue;
                        }
                    }
                    let entries = query_index.entries(name_id);
                    if entries.is_empty() {
                        continue;
                    }
                    let Some((name, folded)) = read.name_entry(name_id) else {
                        continue;
                    };
                    if !extension_matches(name, folded, extension, ext_ascii) {
                        continue;
                    }
                    for &ordinal in entries {
                        let ord_usize = ordinal as usize;
                        let Some(record) = records.get(ord_usize) else {
                            continue;
                        };
                        if !ext_matcher.matches(record.extension) {
                            continue;
                        }
                        let key = super::sort_key_for_record(record, sort);
                        if let Some(worst) = top.worst() {
                            if key < worst.score || (key == worst.score && name > worst.name()) {
                                continue;
                            }
                        }
                        if !passthrough && !filter.matches_record_in(read, record, Some(name)) {
                            continue;
                        }
                        if let Some(view) = read.view(crate::compact_index::EntryId(ord_usize)) {
                            top.offer_path(ord_usize, key, view);
                        }
                    }
                }
                top.finish()
                    .into_iter()
                    .map(|c| (c.ordinal, 1.0))
                    .collect()
            } else {
                let mut top = super::top_k::TopK::new(max_results);
                let query_index = read.query_index();
                let sorted = read.sorted_names();
                let records = read.records();
                let passthrough = filter.is_passthrough();
                let allow_dirs = filter.allow_dirs();
                for &name_id in &sorted.ids {
                    if !extension.is_empty() {
                        if let Some(sig) = read.name_signature(name_id) {
                            if !sig.contains(req) {
                                continue;
                            }
                        }
                    }
                    let entries = read.entries_for_name(name_id);
                    if entries.is_empty() {
                        continue;
                    }
                    let Some((name, folded)) = read.name_entry(name_id) else {
                        continue;
                    };
                    if !extension_matches(name, folded, extension, ext_ascii) {
                        continue;
                    }
                    let base = BaseNameScore::for_exact_fast_path(name);
                    let upper = base.upper_bound(&rank_ctx, allow_dirs, false);
                    if let Some(worst) = top.worst() {
                        if upper < worst.score || (upper == worst.score && name > worst.name()) {
                            continue;
                        }
                    }
                    let (head, tail) = bucket_slices(entries);
                    for &ordinal in head.iter().chain(tail.iter()) {
                        let Some(record) = records.get(ordinal as usize) else {
                            continue;
                        };
                        if ext_matcher.matches(record.extension) {
                            if !passthrough && !filter.matches_record_in(read, record, Some(name)) {
                                continue;
                            }
                            let (parent_depth, parent_hidden) =
                                read.record_parent_meta(record, query_index);
                            let score = score_record_with_meta(
                                base,
                                record.is_dir,
                                parent_depth,
                                parent_hidden,
                                &rank_ctx,
                                false,
                                0,
                            );
                            top.offer(ordinal as usize, score, name);
                        }
                    }
                }
                top.finish()
                    .into_iter()
                    .map(|c| (c.ordinal, c.score.to_f64()))
                    .collect()
            }
        }
        _ => {
            if keywords.is_empty() {
                // 纯否定过滤词（如 `!test` / `!.pdf`）在无正向筛选条件与无 `type:all` 时不返回全量文件
                if !filter.has_positive_filter() && !filter.has_explicit_type_all() {
                    Vec::new()
                } else if sort != super::SearchSort::Relevance {
                    let mut top: super::top_k::TopK<'_, u64> = super::top_k::TopK::new(max_results);
                    let passthrough = filter.is_passthrough();
                    let records = read.records();
                    for (ordinal, record) in records.iter().enumerate() {
                        let key = super::sort_key_for_record(record, sort);
                        if let Some(worst) = top.worst() {
                            if key < worst.score {
                                continue;
                            }
                        }
                        if !passthrough && !filter.matches_record_in(read, record, None) {
                            continue;
                        }
                        if let Some(view) = read.view(crate::compact_index::EntryId(ordinal)) {
                            top.offer_path(ordinal, key, view);
                        }
                    }
                    top.finish()
                        .into_iter()
                        .map(|c| (c.ordinal, 1.0))
                        .collect()
                } else {
                    let mut top = super::top_k::TopK::new(max_results);
                    let query_index = read.query_index();
                    let records = read.records();
                    for (id, name) in read.names() {
                        let Some(record) = records.get(id.0) else {
                            continue;
                        };
                        if !filter.matches_record_in(read, record, Some(name)) {
                            continue;
                        }
                        let base = BaseNameScore::for_exact_fast_path(name);
                        let (parent_depth, parent_hidden) =
                            read.record_parent_meta(record, query_index);
                        let score = score_record_with_meta(
                            base,
                            record.is_dir,
                            parent_depth,
                            parent_hidden,
                            &rank_ctx,
                            false,
                            0,
                        );
                        top.offer(id.0, score, name);
                    }
                    top.finish()
                        .into_iter()
                        .map(|c| (c.ordinal, c.score.to_f64()))
                        .collect()
                }
            } else {
                let mut req = NameSignature::default();
                for kw in &keywords {
                    req = req.union(NameSignature::from_substring_query(kw));
                }
                let all_ascii = keywords.iter().all(|k| k.is_ascii());
                if sort != super::SearchSort::Relevance {
                    super::rank_unique_names_sorted(
                        read,
                        filter,
                        max_results,
                        req,
                        sort,
                        |name, folded| {
                            score_name_rank(
                                name,
                                folded,
                                &keywords,
                                &raw_keywords,
                                all_ascii,
                                case_sensitive,
                                explicit_lock,
                            )
                            .is_some()
                        },
                    )
                    .into_iter()
                    .map(|c| (c.ordinal, 1.0))
                    .collect()
                } else {
                    super::rank_unique_names(
                        read,
                        filter,
                        max_results,
                        req,
                        targets_hidden,
                        &rank_ctx,
                        |name, folded| {
                            score_name_rank(
                                name,
                                folded,
                                &keywords,
                                &raw_keywords,
                                all_ascii,
                                case_sensitive,
                                explicit_lock,
                            )
                        },
                    )
                    .into_iter()
                    .map(|c| (c.ordinal, c.score.to_f64()))
                    .collect()
                }
            }
        }
    };
    let kw_prepared: Vec<(&str, String)> = keywords
        .iter()
        .map(|k| (k.as_str(), crate::cjk::fold_str(k)))
        .collect();
    let kw_refs: Vec<(&str, &str)> = kw_prepared
        .iter()
        .map(|(raw, lower)| (*raw, lower.as_str()))
        .collect();

    let ordinals: Vec<usize> = matches.iter().map(|&(ord, _)| ord).collect();
    let results = matches
        .into_iter()
        .map(|(ordinal, score)| {
            let view = read
                .view(crate::compact_index::EntryId(ordinal))
                .unwrap();
            let match_positions = if extension.is_some() || keywords.is_empty() {
                Vec::new()
            } else {
                let name_folded = crate::cjk::fold_cow(view.name);
                find_positions(view.name, name_folded.as_ref(), &kw_refs)
            };
            AdvancedSearchResult {
                entry: view.materialize(),
                score,
                match_positions,
                parent_match_positions: Vec::new(),
                search_mode: SearchMode::Substring,
            }
        })
        .collect();
    (results, ordinals)
}

struct HybridSplitPlan {
    parent_terms: Vec<String>,
    all_parent_ascii: bool,
    name_keywords: Vec<String>,
    name_raw_keywords: Vec<String>,
    all_name_ascii: bool,
    name_case_sensitive: bool,
    name_explicit_lock: bool,
    required_sig: NameSignature,
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum HybridParentMatch {
    Miss = 0,
    Substring = 1,
    WordBoundary = 2,
}

#[inline]
fn is_parent_word_boundary(haystack_bytes: &[u8], pos: usize) -> bool {
    pos == 0 || matches!(haystack_bytes[pos - 1], b'/' | b'_' | b'-' | b' ' | b'.')
}

/// 在剔除末尾 `/` 后的父路径上检查是否包含所有 `parent_terms`，并判别是否全部命中在段/词边界。
/// 纯 ASCII 父路径走零分配大小写无关字节匹配，消除父目录池预评估阶段的字符串堆分配。
fn evaluate_hybrid_parent_raw(
    parent: &str,
    parent_terms: &[String],
    all_parent_ascii: bool,
) -> HybridParentMatch {
    let parent_no_slash = parent.trim_end_matches('/');
    if parent_no_slash.is_empty() {
        return HybridParentMatch::Miss;
    }

    if all_parent_ascii && parent_no_slash.is_ascii() {
        let bytes = parent_no_slash.as_bytes();
        let mut all_boundary = true;
        for term in parent_terms {
            if term.is_empty() {
                continue;
            }
            let tb = term.as_bytes();
            let mut found_any = false;
            let mut found_boundary = false;
            let mut offset = 0usize;
            while offset + tb.len() <= bytes.len() {
                let Some(rel) = crate::string_pool::find_ascii_ignore_case(&bytes[offset..], tb)
                else {
                    break;
                };
                let abs = offset + rel;
                found_any = true;
                if is_parent_word_boundary(bytes, abs) {
                    found_boundary = true;
                    break;
                }
                offset = abs + 1;
            }
            if !found_any {
                return HybridParentMatch::Miss;
            }
            if !found_boundary {
                all_boundary = false;
            }
        }
        return if all_boundary {
            HybridParentMatch::WordBoundary
        } else {
            HybridParentMatch::Substring
        };
    }

    let parent_folded = crate::cjk::fold_cow(parent_no_slash);
    match evaluate_hybrid_parent(&parent_folded, parent_terms) {
        Some(true) => HybridParentMatch::WordBoundary,
        Some(false) => HybridParentMatch::Substring,
        None => HybridParentMatch::Miss,
    }
}

/// 在剔除末尾 `/` 后的父路径上检查是否包含所有 `parent_terms`，并判别是否全部命中在段/词边界。
fn evaluate_hybrid_parent(parent_folded: &str, parent_terms: &[String]) -> Option<bool> {
    let bytes = parent_folded.as_bytes();
    let mut all_boundary = true;
    for term in parent_terms {
        if term.is_empty() {
            continue;
        }
        let mut found_any = false;
        let mut found_boundary = false;
        let mut offset = 0;
        while let Some(rel) = parent_folded[offset..].find(term.as_str()) {
            let abs = offset + rel;
            found_any = true;
            if is_parent_word_boundary(bytes, abs) {
                found_boundary = true;
                break;
            }
            let step = parent_folded[abs..].chars().next().map_or(1, char::len_utf8);
            offset = abs + step;
        }
        if !found_any {
            return None;
        }
        if !found_boundary {
            all_boundary = false;
        }
    }
    Some(all_boundary)
}

fn compute_parent_match_positions(parent: &str, parent_terms: &[String]) -> Vec<(usize, usize)> {
    let parent_no_slash = parent.trim_end_matches('/');
    if parent_no_slash.is_empty() {
        return Vec::new();
    }
    let parent_folded = crate::cjk::fold_cow(parent_no_slash);
    let bytes = parent_folded.as_bytes();
    let mut spans = Vec::new();
    for term in parent_terms {
        if term.is_empty() {
            continue;
        }
        let mut chosen: Option<(usize, usize)> = None;
        let mut offset = 0;
        while let Some(rel) = parent_folded[offset..].find(term.as_str()) {
            let abs = offset + rel;
            if chosen.is_none() {
                chosen = Some((abs, abs + term.len()));
            }
            if is_parent_word_boundary(bytes, abs) {
                chosen = Some((abs, abs + term.len()));
                break;
            }
            let step = parent_folded[abs..].chars().next().map_or(1, char::len_utf8);
            offset = abs + step;
        }
        if let Some((ns, ne)) = chosen {
            if let Some(orig) = map_norm_span_to_orig(parent_no_slash, &parent_folded, ns, ne) {
                spans.push(orig);
            }
        }
    }
    merge_highlight_spans(spans, parent_no_slash.len())
}

#[allow(clippy::too_many_arguments)]
fn append_hybrid_parent_name_hits(
    read: &CompactIndex,
    positive_tokens: &[QueryToken],
    filter: &CompiledRecordFilter,
    remaining: usize,
    case_sensitivity: CaseSensitivity,
    pure_matched_ordinals: &[usize],
    sort: super::SearchSort,
    results: &mut Vec<AdvancedSearchResult>,
) {
    let k = positive_tokens.len();
    let mut plans = Vec::new();
    for split in (1..k).rev() {
        let lead = &positive_tokens[..split];
        let tail = &positive_tokens[split..];
        if lead.iter().any(|t| t.quoted || t.text.is_empty())
            || tail.iter().all(|t| t.text.is_empty())
        {
            continue;
        }
        let parent_terms: Vec<String> = lead.iter().map(|t| crate::cjk::fold_str(&t.text)).collect();
        let all_parent_ascii = parent_terms.iter().all(|t| t.is_ascii());
        let name_cs =
            resolve_case_sensitive_tokens(tail, case_sensitivity, SearchMode::Substring);
        let name_explicit_lock = case_sensitivity == CaseSensitivity::Sensitive;
        let name_keywords: Vec<String> = tail
            .iter()
            .filter(|t| !t.text.is_empty())
            .map(|t| {
                if name_cs {
                    crate::cjk::normalize_and_simplify_cow(&t.text).into_owned()
                } else {
                    crate::cjk::fold_str(&t.text)
                }
            })
            .collect();
        if name_keywords.is_empty() {
            continue;
        }
        let name_raw_keywords: Vec<String> = tail
            .iter()
            .filter(|t| !t.text.is_empty())
            .map(|t| crate::cjk::normalize_nfc_cow(&t.text).into_owned())
            .collect();
        let all_name_ascii = name_keywords.iter().all(|kw| kw.is_ascii());
        let mut required_sig = NameSignature::default();
        for kw in &name_keywords {
            required_sig = required_sig.union(NameSignature::from_substring_query(kw));
        }
        plans.push(HybridSplitPlan {
            parent_terms,
            all_parent_ascii,
            name_keywords,
            name_raw_keywords,
            all_name_ascii,
            name_case_sensitive: name_cs,
            name_explicit_lock,
            required_sig,
        });
    }
    if plans.is_empty() {
        return;
    }

    let all_folded_terms: Vec<String> = positive_tokens
        .iter()
        .filter(|t| !t.text.is_empty())
        .map(|t| crate::cjk::fold_str(&t.text))
        .collect();
    let targets_hidden = keywords_target_hidden(false, &all_folded_terms);
    let rank_ctx = RecordRankContext::default();
    let pure_seen: HashSet<usize> = pure_matched_ordinals.iter().copied().collect();

    let query_index = read.query_index();
    let records = read.records();
    let passthrough = filter.is_passthrough();
    let allow_dirs = filter.allow_dirs();
    let has_raw_paths = read.pool_counts().3 > 0;

    // 针对最常见的双词查询（单一划分子计划）：先按父目录池预评估一次，再直接流式写入 TopK 并启用桶级上界剪枝，
    // 彻底消除记录级 `fold_cow(parent)` 堆分配与无界 `HashMap` 膨胀。
    if plans.len() == 1 {
        let plan = &plans[0];
        let parent_matches = read.evaluate_parents(HybridParentMatch::Miss, |parent| {
            evaluate_hybrid_parent_raw(parent, &plan.parent_terms, plan.all_parent_ascii)
        });
        if !has_raw_paths
            && !parent_matches
                .iter()
                .any(|&m| m != HybridParentMatch::Miss)
        {
            return;
        }

        let mut top = super::top_k::TopK::new(remaining);
        let mut sorted_top: super::top_k::TopK<'_, u64> = super::top_k::TopK::new(remaining);
        let use_custom_sort = sort != super::SearchSort::Relevance;
        for name_id in 0..query_index.slot_count() as u32 {
            let sig = query_index.signature_at(name_id);
            if sig == NameSignature::default() && !query_index.is_ready(name_id) {
                continue;
            }
            if !sig.contains(plan.required_sig) {
                continue;
            }
            let entries = query_index.entries(name_id);
            if entries.is_empty() {
                continue;
            }
            let Some(name) = read.name_text(name_id) else {
                continue;
            };
            let folded = if name.is_ascii() {
                None
            } else {
                query_index.folded(name_id)
            };
            let Some(name_base) = score_name_rank(
                name,
                folded,
                &plan.name_keywords,
                &plan.name_raw_keywords,
                plan.all_name_ascii,
                plan.name_case_sensitive,
                plan.name_explicit_lock,
            ) else {
                continue;
            };

            if use_custom_sort {
                for &ordinal in entries {
                    let ord_usize = ordinal as usize;
                    if pure_seen.contains(&ord_usize) {
                        continue;
                    }
                    let Some(record) = records.get(ord_usize) else {
                        continue;
                    };
                    let key = super::sort_key_for_record(record, sort);
                    if let Some(worst) = sorted_top.worst() {
                        if key < worst.score || (key == worst.score && name > worst.name()) {
                            continue;
                        }
                    }
                    let pmatch = if record.raw_path {
                        evaluate_hybrid_parent_raw(
                            read.record_parent(record),
                            &plan.parent_terms,
                            plan.all_parent_ascii,
                        )
                    } else {
                        parent_matches
                            .get(record.path as usize)
                            .copied()
                            .unwrap_or(HybridParentMatch::Miss)
                    };
                    if pmatch == HybridParentMatch::Miss {
                        continue;
                    }
                    if !passthrough && !filter.matches_record_in(read, record, Some(name)) {
                        continue;
                    }
                    if let Some(view) = read.view(crate::compact_index::EntryId(ord_usize)) {
                        sorted_top.offer_path(ord_usize, key, view);
                    }
                }
                continue;
            }

            let name_quality_bonus =
                ((name_base.tier as i64) * 2_000) + ((name_base.sub_tier as i64) * 200);
            let upper = BaseNameScore::for_hybrid_parent_name(name, true, name_quality_bonus)
                .upper_bound(&rank_ctx, allow_dirs, targets_hidden);
            if let Some(worst) = top.worst() {
                if upper < worst.score || (upper == worst.score && name > worst.name()) {
                    continue;
                }
            }

            let (head, tail_slice) = bucket_slices(entries);
            for &ordinal in head.iter().chain(tail_slice.iter()) {
                let ord_usize = ordinal as usize;
                if pure_seen.contains(&ord_usize) {
                    continue;
                }
                let Some(record) = records.get(ord_usize) else {
                    continue;
                };
                let pmatch = if record.raw_path {
                    evaluate_hybrid_parent_raw(
                        read.record_parent(record),
                        &plan.parent_terms,
                        plan.all_parent_ascii,
                    )
                } else {
                    parent_matches
                        .get(record.path as usize)
                        .copied()
                        .unwrap_or(HybridParentMatch::Miss)
                };
                if pmatch == HybridParentMatch::Miss {
                    continue;
                }
                if !passthrough && !filter.matches_record_in(read, record, Some(name)) {
                    continue;
                }
                let is_word_boundary = pmatch == HybridParentMatch::WordBoundary;
                let hybrid_base = BaseNameScore::for_hybrid_parent_name(
                    name,
                    is_word_boundary,
                    name_quality_bonus,
                );
                let (parent_depth, parent_hidden) = read.record_parent_meta(record, query_index);
                let score = score_record_with_meta(
                    hybrid_base,
                    record.is_dir,
                    parent_depth,
                    parent_hidden,
                    &rank_ctx,
                    targets_hidden,
                    0,
                );
                top.offer(ord_usize, score, name);
            }
        }

        let chosen: Vec<(usize, f64)> = if use_custom_sort {
            sorted_top
                .finish()
                .into_iter()
                .map(|c| (c.ordinal, 1.0))
                .collect()
        } else {
            top.finish()
                .into_iter()
                .map(|c| (c.ordinal, c.score.to_f64()))
                .collect()
        };
        if chosen.is_empty() {
            return;
        }
        let prepared_kw: Vec<(&str, String)> = plan
            .name_keywords
            .iter()
            .map(|k| (k.as_str(), crate::cjk::fold_str(k)))
            .collect();
        let kw_refs: Vec<(&str, &str)> = prepared_kw
            .iter()
            .map(|(raw, lower)| (*raw, lower.as_str()))
            .collect();

        for (ordinal, score) in chosen {
            let view = read
                .view(crate::compact_index::EntryId(ordinal))
                .expect("valid compact reference");
            let name_folded = crate::cjk::fold_cow(view.name);
            let match_positions = find_positions(view.name, name_folded.as_ref(), &kw_refs);
            let parent = if view.raw_path().is_some() {
                read.record_parent(&records[ordinal])
            } else {
                view.parent
            };
            let parent_match_positions =
                compute_parent_match_positions(parent, &plan.parent_terms);

            results.push(AdvancedSearchResult {
                entry: view.materialize(),
                score,
                match_positions,
                parent_match_positions,
                search_mode: SearchMode::Substring,
            });
        }
        return;
    }

    // 多切分计划（3 词及以上）：每个计划预先在父目录池上完成一次零分配评估，按记录取各计划最高分
    let use_custom_sort = sort != super::SearchSort::Relevance;
    let mut best_by_ordinal: HashMap<usize, (usize, RankScore, &str)> = HashMap::new();
    let mut sorted_top: super::top_k::TopK<'_, u64> = super::top_k::TopK::new(remaining);
    let mut plan_by_ordinal: HashMap<usize, usize> = if use_custom_sort {
        HashMap::with_capacity(remaining)
    } else {
        HashMap::new()
    };

    for (plan_idx, plan) in plans.iter().enumerate() {
        let parent_matches = read.evaluate_parents(HybridParentMatch::Miss, |parent| {
            evaluate_hybrid_parent_raw(parent, &plan.parent_terms, plan.all_parent_ascii)
        });
        if !has_raw_paths
            && !parent_matches
                .iter()
                .any(|&m| m != HybridParentMatch::Miss)
        {
            continue;
        }

        for name_id in 0..query_index.slot_count() as u32 {
            let sig = query_index.signature_at(name_id);
            if sig == NameSignature::default() && !query_index.is_ready(name_id) {
                continue;
            }
            if !sig.contains(plan.required_sig) {
                continue;
            }
            let entries = query_index.entries(name_id);
            if entries.is_empty() {
                continue;
            }
            let Some(name) = read.name_text(name_id) else {
                continue;
            };
            let folded = if name.is_ascii() {
                None
            } else {
                query_index.folded(name_id)
            };
            let Some(name_base) = score_name_rank(
                name,
                folded,
                &plan.name_keywords,
                &plan.name_raw_keywords,
                plan.all_name_ascii,
                plan.name_case_sensitive,
                plan.name_explicit_lock,
            ) else {
                continue;
            };

            let name_quality_bonus =
                ((name_base.tier as i64) * 2_000) + ((name_base.sub_tier as i64) * 200);

            if use_custom_sort {
                for &ordinal in entries {
                    let ord_usize = ordinal as usize;
                    if pure_seen.contains(&ord_usize) {
                        continue;
                    }
                    let Some(record) = records.get(ord_usize) else {
                        continue;
                    };
                    let key = super::sort_key_for_record(record, sort);
                    if let Some(worst) = sorted_top.worst() {
                        if key < worst.score || (key == worst.score && name > worst.name()) {
                            continue;
                        }
                    }
                    if plan_by_ordinal.contains_key(&ord_usize) {
                        continue;
                    }
                    let pmatch = if record.raw_path {
                        evaluate_hybrid_parent_raw(
                            read.record_parent(record),
                            &plan.parent_terms,
                            plan.all_parent_ascii,
                        )
                    } else {
                        parent_matches
                            .get(record.path as usize)
                            .copied()
                            .unwrap_or(HybridParentMatch::Miss)
                    };
                    if pmatch == HybridParentMatch::Miss {
                        continue;
                    }
                    if !passthrough && !filter.matches_record_in(read, record, Some(name)) {
                        continue;
                    }
                    if let Some(view) = read.view(crate::compact_index::EntryId(ord_usize)) {
                        let (inserted, evicted) = sorted_top.offer_path_tracked(ord_usize, key, view);
                        if inserted {
                            if let Some(evicted_ord) = evicted {
                                plan_by_ordinal.remove(&evicted_ord);
                            }
                            plan_by_ordinal.insert(ord_usize, plan_idx);
                        }
                    }
                }
                continue;
            }

            let (head, tail_slice) = bucket_slices(entries);
            for &ordinal in head.iter().chain(tail_slice.iter()) {
                let ord_usize = ordinal as usize;
                if pure_seen.contains(&ord_usize) {
                    continue;
                }
                let Some(record) = records.get(ord_usize) else {
                    continue;
                };
                let pmatch = if record.raw_path {
                    evaluate_hybrid_parent_raw(
                        read.record_parent(record),
                        &plan.parent_terms,
                        plan.all_parent_ascii,
                    )
                } else {
                    parent_matches
                        .get(record.path as usize)
                        .copied()
                        .unwrap_or(HybridParentMatch::Miss)
                };
                if pmatch == HybridParentMatch::Miss {
                    continue;
                }
                if !passthrough && !filter.matches_record_in(read, record, Some(name)) {
                    continue;
                }

                let is_word_boundary = pmatch == HybridParentMatch::WordBoundary;
                let hybrid_base = BaseNameScore::for_hybrid_parent_name(
                    name,
                    is_word_boundary,
                    name_quality_bonus,
                );
                let (parent_depth, parent_hidden) = read.record_parent_meta(record, query_index);
                let score = score_record_with_meta(
                    hybrid_base,
                    record.is_dir,
                    parent_depth,
                    parent_hidden,
                    &rank_ctx,
                    targets_hidden,
                    0,
                );
                match best_by_ordinal.get_mut(&ord_usize) {
                    Some((prev_plan, prev_score, _)) => {
                        if score > *prev_score {
                            *prev_plan = plan_idx;
                            *prev_score = score;
                        }
                    }
                    None => {
                        best_by_ordinal.insert(ord_usize, (plan_idx, score, name));
                    }
                }
            }
        }
    }

    let chosen: Vec<(usize, usize, f64)> = if use_custom_sort {
        if plan_by_ordinal.is_empty() {
            return;
        }
        sorted_top
            .finish()
            .into_iter()
            .filter_map(|c| {
                plan_by_ordinal
                    .get(&c.ordinal)
                    .map(|&plan_idx| (c.ordinal, plan_idx, 1.0))
            })
            .collect()
    } else {
        if best_by_ordinal.is_empty() {
            return;
        }
        let mut top = super::top_k::TopK::new(remaining);
        for (&ordinal, &(_, score, name)) in &best_by_ordinal {
            top.offer(ordinal, score, name);
        }
        top.finish()
            .into_iter()
            .filter_map(|c| {
                best_by_ordinal
                    .get(&c.ordinal)
                    .map(|&(plan_idx, _, _)| (c.ordinal, plan_idx, c.score.to_f64()))
            })
            .collect()
    };

    let prepared_kws: Vec<Vec<(&str, String)>> = plans
        .iter()
        .map(|plan| {
            plan.name_keywords
                .iter()
                .map(|k| (k.as_str(), crate::cjk::fold_str(k)))
                .collect()
        })
        .collect();

    for (ordinal, plan_idx, score) in chosen {
        let plan = &plans[plan_idx];
        let view = read
            .view(crate::compact_index::EntryId(ordinal))
            .expect("valid compact reference");
        let name_folded = crate::cjk::fold_cow(view.name);
        let kw_refs: Vec<(&str, &str)> = prepared_kws[plan_idx]
            .iter()
            .map(|(raw, lower)| (*raw, lower.as_str()))
            .collect();
        let match_positions = find_positions(view.name, name_folded.as_ref(), &kw_refs);
        let parent = if view.raw_path().is_some() {
            read.record_parent(&records[ordinal])
        } else {
            view.parent
        };
        let parent_match_positions =
            compute_parent_match_positions(parent, &plan.parent_terms);

        results.push(AdvancedSearchResult {
            entry: view.materialize(),
            score,
            match_positions,
            parent_match_positions,
            search_mode: SearchMode::Substring,
        });
    }
}

struct PreparedDisjunctionClause<'a> {
    clause_idx: usize,
    is_extension: bool,
    is_wildcard: bool,
    multi_and_wildcard: bool,
    simple_suffix: Option<String>,
    extension: Option<&'a str>,
    ext_matcher: Option<ExtMatcher>,
    ext_ascii: bool,
    keywords: Vec<String>,
    raw_keywords: Vec<String>,
    all_ascii: bool,
    case_sensitive: bool,
    targets_hidden: bool,
    required_sig: NameSignature,
    kw_prepared: Vec<(&'a str, String)>,
    scope_candidates: &'a [PathCandidate],
}

/// 并集检索执行引擎：支持带管道符多子句的单趟签名位运算预筛、上界剪枝与双层复合打分。
pub fn search_disjunction(
    read: &CompactIndex,
    query: &DisjunctionQuery,
    filter: &CompiledRecordFilter,
    max_results: usize,
    explicit_lock: bool,
) -> Vec<AdvancedSearchResult> {
    search_disjunction_with_filters(read, query, filter, None, max_results, explicit_lock)
}

/// 支持按子句独立作用域编译过滤器的并集检索引擎（如 `foo !a | bar !b`、`test_* | *_spec.rs`、`in:src foo | in:tests bar`）。
///
/// 算法核心机制：
/// 1. 单趟 128 位超集签名预筛：多子句签名求或（Any），零堆分配短路快速跳过无关名字；
/// 2. 桶级子句匹配收集与上界剪枝：先行以桶上界淘汰必然落败的名字桶；
/// 3. 双层记录扩展与子句过滤器隔离：仅由通过各自子句过滤器的分支参与计分；
/// 4. 命中子句高亮隔离：仅对通过自身过滤器且实际命中该记录的子句提取高亮字符区间。
pub fn search_disjunction_with_filters(
    read: &CompactIndex,
    query: &DisjunctionQuery,
    filter: &CompiledRecordFilter,
    clause_filters: Option<&[CompiledRecordFilter]>,
    max_results: usize,
    explicit_lock: bool,
) -> Vec<AdvancedSearchResult> {
    search_disjunction_with_filters_and_sort(
        read,
        query,
        filter,
        clause_filters,
        max_results,
        explicit_lock,
        super::SearchSort::Relevance,
    )
}

pub(crate) fn search_disjunction_with_filters_and_sort(
    read: &CompactIndex,
    query: &DisjunctionQuery,
    filter: &CompiledRecordFilter,
    clause_filters: Option<&[CompiledRecordFilter]>,
    max_results: usize,
    explicit_lock: bool,
    sort: super::SearchSort,
) -> Vec<AdvancedSearchResult> {
    if max_results == 0 || query.is_empty() {
        return Vec::new();
    }

    let mut prepared_clauses = Vec::with_capacity(query.clauses.len());
    for (clause_idx, clause) in query.clauses.iter().enumerate() {
        let is_extension = clause.extension.is_some();
        if clause.keywords.is_empty() && !is_extension {
            continue;
        }

        let case_sensitive = clause.is_case_sensitive;
        let ext_str = clause.extension.as_deref();
        let ext_matcher = ext_str.and_then(|ext| ExtMatcher::compile(read, ext));
        let ext_ascii = ext_str.is_none_or(|ext| ext.is_ascii());

        let keywords = clause.keywords.clone();
        let raw_keywords = clause.raw_keywords.clone();
        let all_ascii = keywords.iter().all(|k| k.is_ascii());
        let targets_hidden = keywords_target_hidden(is_extension, &keywords);
        let is_wildcard = clause.is_wildcard;
        let multi_and_wildcard = is_wildcard && keywords.len() > 1;
        let simple_suffix = if is_wildcard && !multi_and_wildcard {
            keywords.first().and_then(|p| {
                p.strip_prefix("*.")
                    .filter(|rest| {
                        !rest.is_empty()
                            && !rest.contains(['*', '?', '[', ']', '{', '}', '\\'])
                            && rest.is_ascii()
                    })
                    .map(str::to_owned)
            })
        } else {
            None
        };

        let kw_prepared: Vec<(&str, String)> = clause
            .keywords
            .iter()
            .map(|k| (k.as_str(), crate::cjk::fold_str(k)))
            .collect();

        prepared_clauses.push(PreparedDisjunctionClause {
            clause_idx,
            is_extension,
            is_wildcard,
            multi_and_wildcard,
            simple_suffix,
            extension: ext_str,
            ext_matcher,
            ext_ascii,
            keywords,
            raw_keywords,
            all_ascii,
            case_sensitive,
            targets_hidden,
            required_sig: clause.required_sig,
            kw_prepared,
            scope_candidates: &clause.scope_candidates,
        });
    }

    if prepared_clauses.is_empty() {
        return Vec::new();
    }

    let use_custom_sort = sort != super::SearchSort::Relevance;
    let query_index = read.query_index();
    let sorted = read.sorted_names();
    let mut top = super::top_k::TopK::new(max_results);
    let mut sorted_top: super::top_k::TopK<'_, u64> = super::top_k::TopK::new(max_results);
    let rank_ctx = RecordRankContext::default();
    let records = read.records();
    let mut ascii_buf = Vec::with_capacity(128);
    let mut matched_clauses: Vec<(usize, Option<BaseNameScore>, Option<&ExtMatcher>)> =
        Vec::with_capacity(prepared_clauses.len());

    for &name_id in &sorted.ids {
        // 1. 单趟 128 位超集签名位运算预筛
        if let Some(sig) = read.name_signature(name_id) {
            let signature_matched = prepared_clauses.iter().any(|c| {
                if c.keywords.is_empty() && !c.is_extension {
                    return false;
                }
                if c.is_extension && c.ext_matcher.is_none() {
                    return false;
                }
                sig.contains(c.required_sig)
            });
            if !signature_matched {
                continue;
            }
        }

        let entries = read.entries_for_name(name_id);
        if entries.is_empty() {
            continue;
        }
        let Some((name, folded)) = read.name_entry(name_id) else {
            continue;
        };

        // 2. 收集命中当前名字的子句及其基分
        matched_clauses.clear();
        for (prep_idx, clause) in prepared_clauses.iter().enumerate() {
            if clause.is_extension {
                if let (Some(ext), Some(ref matcher)) = (clause.extension, &clause.ext_matcher) {
                    if extension_matches(name, folded, ext, clause.ext_ascii) {
                        matched_clauses.push((prep_idx, None, Some(matcher)));
                    }
                }
            } else if clause.is_wildcard {
                if super::wildcard::matches_wildcard_terms(
                    name,
                    folded,
                    &clause.keywords,
                    clause.multi_and_wildcard,
                    clause.simple_suffix.as_deref(),
                    clause.case_sensitive,
                    &mut ascii_buf,
                ) {
                    matched_clauses.push((
                        prep_idx,
                        Some(BaseNameScore::for_tier_only(name, MatchTier::Prefix, 0)),
                        None,
                    ));
                }
            } else if let Some(base) = score_name_rank(
                name,
                folded,
                &clause.keywords,
                &clause.raw_keywords,
                clause.all_ascii,
                clause.case_sensitive,
                explicit_lock,
            ) {
                matched_clauses.push((prep_idx, Some(base), None));
            }
        }

        if matched_clauses.is_empty() {
            continue;
        }

        if use_custom_sort {
            for &ordinal in entries {
                let ord_usize = ordinal as usize;
                let Some(record) = records.get(ord_usize) else {
                    continue;
                };
                let key = super::sort_key_for_record(record, sort);
                if let Some(worst) = sorted_top.worst() {
                    if key < worst.score || (key == worst.score && name > worst.name()) {
                        continue;
                    }
                }
                let mut any_clause_matched = false;
                for &(prep_idx, base_opt, ext_opt) in &matched_clauses {
                    let c = &prepared_clauses[prep_idx];
                    let cf = clause_filters
                        .and_then(|fs| fs.get(c.clause_idx))
                        .unwrap_or(filter);
                    if !cf.is_passthrough() && !cf.matches_record_in(read, record, Some(name)) {
                        continue;
                    }
                    if ext_opt.is_some_and(|m| !record.is_dir && m.matches(record.extension))
                        || base_opt.is_some()
                    {
                        any_clause_matched = true;
                        break;
                    }
                }
                if any_clause_matched {
                    if let Some(view) = read.view(crate::compact_index::EntryId(ord_usize)) {
                        sorted_top.offer_path(ord_usize, key, view);
                    }
                }
            }
            continue;
        }

        let ext_base = matched_clauses
            .iter()
            .any(|(_, _, ext_opt)| ext_opt.is_some())
            .then(|| BaseNameScore::for_exact_fast_path(name));
        let mut bucket_upper: Option<RankScore> = None;
        for &(prep_idx, base_opt, ext_opt) in &matched_clauses {
            let c = &prepared_clauses[prep_idx];
            let cf = clause_filters
                .and_then(|fs| fs.get(c.clause_idx))
                .unwrap_or(filter);
            if ext_opt.is_some() {
                if let Some(base) = ext_base {
                    let ub = base.upper_bound(&rank_ctx, false, false);
                    bucket_upper = Some(bucket_upper.map_or(ub, |cur| cur.max(ub)));
                }
            }
            if let Some(base) = base_opt {
                let ub = base.upper_bound(&rank_ctx, cf.allow_dirs(), c.targets_hidden);
                bucket_upper = Some(bucket_upper.map_or(ub, |cur| cur.max(ub)));
            }
        }
        if let (Some(upper), Some(worst)) = (bucket_upper, top.worst()) {
            if upper < worst.score || (upper == worst.score && name > worst.name()) {
                continue;
            }
        }

        // 3. CSR 条目展开与分句独立记录级守卫
        let (head, tail) = bucket_slices(entries);
        for &ordinal in head.iter().chain(tail.iter()) {
            let Some(record) = records.get(ordinal as usize) else {
                continue;
            };

            let (parent_depth, parent_hidden) = read.record_parent_meta(record, query_index);
            let mut best_record_score: Option<RankScore> = None;

            for &(prep_idx, base_opt, ext_opt) in &matched_clauses {
                let c = &prepared_clauses[prep_idx];
                let cf = clause_filters
                    .and_then(|fs| fs.get(c.clause_idx))
                    .unwrap_or(filter);
                if !cf.is_passthrough() && !cf.matches_record_in(read, record, Some(name)) {
                    continue;
                }

                if let Some(matcher) = ext_opt {
                    if !record.is_dir && matcher.matches(record.extension) {
                        if let Some(base) = ext_base {
                            let s = score_record_with_meta(
                                base,
                                false,
                                parent_depth,
                                parent_hidden,
                                &rank_ctx,
                                false,
                                0,
                            );
                            best_record_score = Some(best_record_score.map_or(s, |cur| cur.max(s)));
                        }
                    }
                }
                if let Some(base) = base_opt {
                    let s = score_record_with_meta(
                        base,
                        record.is_dir,
                        parent_depth,
                        parent_hidden,
                        &rank_ctx,
                        c.targets_hidden,
                        0,
                    );
                    best_record_score = Some(best_record_score.map_or(s, |cur| cur.max(s)));
                }
            }

            if let Some(score) = best_record_score {
                top.offer(ordinal as usize, score, name);
            }
        }
    }

    // 4. 高亮隔离与三态合并
    let candidates: Vec<(usize, f64)> = if use_custom_sort {
        sorted_top
            .finish()
            .into_iter()
            .map(|c| (c.ordinal, 1.0))
            .collect()
    } else {
        top.finish()
            .into_iter()
            .map(|c| (c.ordinal, c.score.to_f64()))
            .collect()
    };
    let mut results = Vec::with_capacity(candidates.len());

    for (ordinal, score) in candidates {
        let Some(view) = read.view(crate::compact_index::EntryId(ordinal)) else {
            continue;
        };
        let name = view.name;
        let name_folded = crate::cjk::fold_cow(name);
        let record = &read.records()[ordinal];
        let folded = read.name_entry(record.name).and_then(|(_, f)| f);

        let mut all_spans = Vec::new();
        let mut parent_spans = Vec::new();
        for clause in &prepared_clauses {
            if clause.is_extension && clause.scope_candidates.is_empty() {
                continue;
            }
            if !clause.is_extension && clause.keywords.is_empty() {
                continue;
            }
            let cf = clause_filters
                .and_then(|fs| fs.get(clause.clause_idx))
                .unwrap_or(filter);
            if !cf.is_passthrough() && !cf.matches_record_in(read, record, Some(name)) {
                continue;
            }
            let matched = if clause.is_extension {
                clause.ext_matcher.as_ref().is_some_and(|m| !record.is_dir && m.matches(record.extension))
            } else if clause.is_wildcard {
                super::wildcard::matches_wildcard_terms(
                    name,
                    folded,
                    &clause.keywords,
                    clause.multi_and_wildcard,
                    clause.simple_suffix.as_deref(),
                    clause.case_sensitive,
                    &mut ascii_buf,
                )
            } else {
                score_name_rank(
                    name,
                    folded,
                    &clause.keywords,
                    &clause.raw_keywords,
                    clause.all_ascii,
                    clause.case_sensitive,
                    explicit_lock,
                )
                .is_some()
            };

            if matched {
                if clause.is_wildcard {
                    all_spans.extend(super::wildcard::find_wildcard_positions(
                        name,
                        &clause.keywords,
                        clause.case_sensitive,
                    ));
                } else if !clause.is_extension {
                    let kw_refs: Vec<(&str, &str)> = clause
                        .kw_prepared
                        .iter()
                        .map(|(raw, lower)| (*raw, lower.as_str()))
                        .collect();
                    let spans = find_positions(name, name_folded.as_ref(), &kw_refs);
                    all_spans.extend(spans);
                }
                if !clause.scope_candidates.is_empty() {
                    let parent = read.record_parent(record);
                    if let Some(pspan) = super::path_search::find_scope_highlight_range(
                        parent,
                        clause.scope_candidates,
                    ) {
                        parent_spans.push(pspan);
                    }
                }
            }
        }

        all_spans.retain(|&(s, e)| {
            s < e
                && e <= name.len()
                && name.is_char_boundary(s)
                && name.is_char_boundary(e)
        });
        let match_positions = merge_highlight_spans(all_spans, name.len());
        let parent_match_positions = if parent_spans.is_empty() {
            Vec::new()
        } else {
            let parent = read.record_parent(record).trim_end_matches('/');
            merge_highlight_spans(parent_spans, parent.len())
        };

        results.push(AdvancedSearchResult {
            entry: view.materialize(),
            score,
            match_positions,
            parent_match_positions,
            search_mode: SearchMode::Substring,
        });
    }

    results
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
        // Prefix（`main.rs`）严格高于 WordBoundary（`my_main.rs`）
        assert!(results[0].score > results[1].score);
        assert!(results[0].score > 0.8);
        assert!(results[1].score > 0.65 && results[1].score < results[0].score);
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
        assert_eq!(bare_dot_names, ["foo.", "noext", "Makefile"]);

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
