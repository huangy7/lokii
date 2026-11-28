use crate::compact_index::{split_path, CompactIndex, EntryId};
use crate::indexer::IndexStore;
use crate::search::path_query::{PathCandidate, PathQuery};
use crate::search::ranking::{
    self, BaseNameScore, MatchTier, RecordRankContext, PATH_BONUS_ANCESTOR, PATH_BONUS_DIRECT,
};
use crate::search::top_k::TopK;
use crate::search::{find_positions, AdvancedSearchResult, CompiledRecordFilter, SearchMode};
use crate::string_pool::NameSignature;

#[inline]
fn any_segment_starts_with_dot(s: &str) -> bool {
    s.split(['/', '\\']).any(|seg| seg.starts_with('.'))
}

#[inline]
pub(crate) fn path_query_targets_hidden(query: &PathQuery) -> bool {
    query.name_terms.iter().any(|t| t.starts_with('.'))
        || query
            .path_pattern
            .as_deref()
            .is_some_and(any_segment_starts_with_dot)
        || query
            .scope_prefix
            .as_deref()
            .is_some_and(any_segment_starts_with_dot)
        || query
            .path_candidates
            .iter()
            .any(|c| any_segment_starts_with_dot(&c.pattern))
        || query
            .scope_candidates
            .iter()
            .any(|c| any_segment_starts_with_dot(&c.pattern))
}

/// 标识父路径匹配的质量层级。
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParentQuality {
    None = 0,
    Ancestor = 1,
    Direct = 2,
}

/// 父路径匹配结果：质量层级与高亮区间同源产出。
#[derive(Clone, Copy)]
struct ParentMatch {
    quality: ParentQuality,
    span: (usize, usize),
}

/// 匹配 `in:` 作用域候选：绝对路径走前缀匹配，相对路径严格要求段边界命中（禁止 `/mydownloads/` 误命中）。
/// 直接借用预拼接的 `cand.slashed_pattern`（`"/{pat}"`），消除父目录池评估期间的数万次 `format!` 堆分配。
pub(crate) fn match_scope_candidates(
    parent_folded: &str,
    scope_candidates: &[PathCandidate],
) -> Option<(usize, usize)> {
    for cand in scope_candidates {
        let pat = cand.pattern.as_str();
        if cand.is_absolute || parent_folded.starts_with(pat) {
            if parent_folded.starts_with(pat) {
                return Some((0, pat.len()));
            }
        } else if let Some(pos) = parent_folded.find(cand.slashed_pattern.as_str()) {
            let start = pos + 1;
            return Some((start, start + pat.len()));
        }
    }
    None
}

/// 针对 `in:<scope>` 作用域计算父路径高亮区间（并剪裁尾随 `/` 以匹配展示串），
/// 供正则、模糊、并集等正交复用 `in:` 作用域的检索模式展示父路径命中段。
pub(crate) fn find_scope_highlight_range(
    parent: &str,
    scope_candidates: &[PathCandidate],
) -> Option<(usize, usize)> {
    if scope_candidates.is_empty() || parent.is_empty() {
        return None;
    }
    let parent_folded = crate::cjk::fold_cow(parent);
    let (norm_start, norm_end) = match_scope_candidates(&parent_folded, scope_candidates)?;
    let (start, mut end) =
        super::map_norm_span_to_orig(parent, &parent_folded, norm_start, norm_end)?;
    while end > start && parent.as_bytes().get(end - 1) == Some(&b'/') {
        end -= 1;
    }
    (start < end).then_some((start, end))
}

/// 在父目录上判定约束，同时产出质量层级与高亮区间。
///
/// 绝对路径按前缀命中；相对路径优先取结尾命中的直接父级，其次取中段命中的祖先。
/// 高亮字节区间严格使用最终命中的候选模式字节长度，避免别名长度差异造成 UTF-8 区间越界。
/// 依据 macOS APFS 特性与 Smart Case 设计，父目录恒定保持大小写无关及 Unicode NFC 等价匹配。
fn match_parent(parent: &str, query: &PathQuery) -> Option<ParentMatch> {
    let parent_folded = crate::cjk::fold_cow(parent);

    let scope_norm_span = if !query.scope_candidates.is_empty() {
        Some(match_scope_candidates(&parent_folded, &query.scope_candidates)?)
    } else {
        None
    };

    if query.path_candidates.is_empty() {
        // 无父目录模式：作用域本身即直接命中；无约束时为零长区间（不高亮）。
        let span = scope_norm_span
            .and_then(|(s, e)| super::map_norm_span_to_orig(parent, &parent_folded, s, e))
            .unwrap_or((0, 0));
        return Some(ParentMatch {
            quality: ParentQuality::Direct,
            span,
        });
    }

    let mut best: Option<(ParentQuality, usize, usize)> = None;
    for cand in &query.path_candidates {
        let pat = cand.pattern.as_str();
        let matched = if cand.is_absolute {
            if !parent_folded.starts_with(pat) {
                continue;
            }
            let quality = if parent_folded.as_ref() == pat {
                ParentQuality::Direct
            } else {
                ParentQuality::Ancestor
            };
            (quality, 0, pat.len())
        } else if cand.require_segment_boundary {
            let boundary_suffix = cand.slashed_pattern.as_str();
            if parent_folded.as_ref() == pat || parent_folded.ends_with(boundary_suffix) {
                (
                    ParentQuality::Direct,
                    parent_folded.len() - pat.len(),
                    parent_folded.len(),
                )
            } else if parent_folded.starts_with(pat) {
                (ParentQuality::Ancestor, 0, pat.len())
            } else if let Some(pos) = parent_folded.find(boundary_suffix) {
                let start = pos + 1;
                (ParentQuality::Ancestor, start, start + pat.len())
            } else {
                continue;
            }
        } else if parent_folded.ends_with(pat) {
            (
                ParentQuality::Direct,
                parent_folded.len() - pat.len(),
                parent_folded.len(),
            )
        } else if let Some(start) = parent_folded.find(pat) {
            (ParentQuality::Ancestor, start, start + pat.len())
        } else {
            continue;
        };

        let replace = match best {
            None => true,
            Some((prev_q, _, _)) => (matched.0 as u8) > (prev_q as u8),
        };
        if replace {
            best = Some(matched);
            if matched.0 == ParentQuality::Direct {
                break;
            }
        }
    }

    let (quality, norm_start, norm_end) = best?;
    let span = super::map_norm_span_to_orig(parent, &parent_folded, norm_start, norm_end)?;
    Some(ParentMatch { quality, span })
}

/// 预判单个父目录的约束质量。
pub(crate) fn evaluate_parent_quality(
    parent_str: &str,
    query: &PathQuery,
) -> ParentQuality {
    match match_parent(parent_str, query) {
        Some(matched) => matched.quality,
        None => ParentQuality::None,
    }
}

/// 当前目录实体是否正是查询的目标目录（如 "Desktop/" 或 "桌面/"）。
/// 先比对末级目录名再延迟折叠父路径，过滤 99.9%+ 无关目录并消除绝对路径判断的 `format!` 堆分配。
pub(crate) fn is_target_directory_self(
    parent: &str,
    name: &str,
    query: &PathQuery,
) -> bool {
    if !query.is_dir_only || query.path_candidates.is_empty() {
        return false;
    }
    let name_folded = crate::cjk::fold_cow(name);
    let any_name_match = query.path_candidates.iter().any(|cand| {
        let pat_clean = cand.pattern.trim_end_matches('/');
        split_path(pat_clean).1 == name_folded.as_ref()
    });
    if !any_name_match {
        return false;
    }

    let parent_folded = crate::cjk::fold_cow(parent);

    // in: 作用域校验
    if !query.scope_candidates.is_empty()
        && match_scope_candidates(&parent_folded, &query.scope_candidates).is_none()
    {
        return false;
    }

    for cand in &query.path_candidates {
        let pat = cand.pattern.as_str();
        let pat_clean = pat.trim_end_matches('/');
        let (dir_part, target_name) = split_path(pat_clean);
        // 目录名匹配恒定大小写无关且 NFC 等价
        if name_folded.as_ref() != target_name {
            continue;
        }
        if cand.is_absolute {
            if parent_folded.len() >= pat.len() {
                if parent_folded.starts_with(pat) {
                    return true;
                }
            } else if let Some(rest) = pat.strip_prefix(parent_folded.as_ref()) {
                if rest == name_folded.as_ref() || rest.strip_suffix('/') == Some(name_folded.as_ref()) {
                    return true;
                }
            }
        } else {
            if dir_part.is_empty() {
                return true;
            }
            if cand.require_segment_boundary {
                // 多段相对别名须在路径段边界匹配，避免 "my/桌面/" 误命中 ".../dummy/Desktop"
                if parent_folded
                    .strip_suffix(dir_part)
                    .is_some_and(|prefix| prefix.is_empty() || prefix.ends_with('/'))
                {
                    return true;
                }
            } else if parent_folded.ends_with(dir_part) {
                return true;
            }
        }
    }
    false
}

/// 计算父目录高亮区间，并剪裁末尾 '/' 以匹配前端展示串。
pub(crate) fn find_parent_highlight_range(
    parent: &str,
    query: &PathQuery,
) -> Option<(usize, usize)> {
    let (start, mut end) = match_parent(parent, query)?.span;

    // 前端展示串不含末尾 '/'，区间若含之会越界并丢失整段高亮，故剪裁。
    while end > start && parent.as_bytes().get(end - 1) == Some(&b'/') {
        end -= 1;
    }
    (start < end).then_some((start, end))
}

/// 对单个文件名打分，返回文件名级复合排序基分；未命中返回 `None`。
#[allow(clippy::too_many_arguments)]
pub(crate) fn score_name(
    name: &str,
    folded: Option<&str>,
    query: &PathQuery,
    has_wildcard: bool,
    all_terms_ascii: bool,
    ascii_buf: &mut Vec<u8>,
    case_sensitive: bool,
    explicit_lock: bool,
) -> Option<BaseNameScore> {
    if has_wildcard {
        let name_lower_cow: std::borrow::Cow<'_, str> = if case_sensitive {
            crate::cjk::normalize_and_simplify_cow(name)
        } else if let Some(f) = folded {
            std::borrow::Cow::Borrowed(f)
        } else if name.is_ascii() {
            ascii_buf.clear();
            ascii_buf.extend(name.bytes().map(|b| b.to_ascii_lowercase()));
            std::borrow::Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(ascii_buf) })
        } else {
            crate::cjk::fold_cow(name)
        };
        let all_matched = query.name_terms.iter().all(|term| {
            if term.contains('*') || term.contains('?') {
                glob_match::glob_match(term, &name_lower_cow)
            } else {
                name_lower_cow.contains(term.as_str())
            }
        });
        return all_matched.then(|| BaseNameScore::for_tier_only(name, MatchTier::Prefix, 0));
    }
    ranking::score_name_rank(
        name,
        folded,
        &query.name_terms,
        &query.raw_name_terms,
        all_terms_ascii,
        case_sensitive,
        explicit_lock,
    )
}

/// 执行路径感知流式检索。
pub fn search(
    store: &IndexStore,
    query: &PathQuery,
    filter: &CompiledRecordFilter,
    max_results: usize,
) -> Vec<AdvancedSearchResult> {
    if max_results == 0 {
        return Vec::new();
    }
    let read = store.read();
    search_index(&read, query, filter, max_results, false, false)
}

/// 接收统一快照读锁的路径检索实现，杜绝锁重入与快照位掩码漂移。
pub fn search_index(
    read: &CompactIndex,
    query: &PathQuery,
    filter: &CompiledRecordFilter,
    max_results: usize,
    case_sensitive: bool,
    explicit_lock: bool,
) -> Vec<AdvancedSearchResult> {
    search_index_with_sort(
        read,
        query,
        filter,
        max_results,
        case_sensitive,
        explicit_lock,
        super::SearchSort::Relevance,
    )
}

pub(crate) fn search_index_with_sort(
    read: &CompactIndex,
    query: &PathQuery,
    filter: &CompiledRecordFilter,
    max_results: usize,
    case_sensitive: bool,
    explicit_lock: bool,
    sort: super::SearchSort,
) -> Vec<AdvancedSearchResult> {
    if max_results == 0 {
        return Vec::new();
    }

    // 预编译扩展名槽位匹配器：若查询指定扩展名但索引池无对应槽位，直接短路返回空
    let ext_matcher = match query.extension.as_deref() {
        Some(ext) => match super::substring::ExtMatcher::compile(read, ext) {
            Some(m) => Some(m),
            None => return Vec::new(),
        },
        None => None,
    };

    // 阶段 0：父目录池预判
    // 每个父目录只判定一次，状态表常驻 L1 缓存。
    let parent_status = read.evaluate_parents(ParentQuality::None, |parent| {
        evaluate_parent_quality(parent, query)
    });

    let has_raw_paths = read.pool_counts().3 > 0;
    let any_parent_hit = parent_status.iter().any(|&q| q != ParentQuality::None);
    if !any_parent_hit && !query.is_dir_only && !has_raw_paths {
        return Vec::new();
    }

    // 通配符判定只依赖查询，提到循环外。
    let has_wildcard = query
        .name_terms
        .iter()
        .any(|term| term.contains('*') || term.contains('?'));
    let all_terms_ascii = query.name_terms.iter().all(|t| t.is_ascii());

    let use_custom_sort = sort != super::SearchSort::Relevance;
    let mut top_k = TopK::new(max_results);
    let mut sorted_top_k: TopK<'_, u64> = TopK::new(max_results);
    let mut ascii_buf = Vec::with_capacity(128);
    let rank_ctx = RecordRankContext::default();
    let targets_hidden = path_query_targets_hidden(query);
    let passthrough = filter.is_passthrough();
    let allow_dirs = filter.allow_dirs();
    let query_index = read.query_index();
    let records = read.records();

    if !query.name_terms.is_empty() {
        // 阶段 1A：带文件名路径检索（如 "src/main.rs"、"in:Desktop report"）走 QueryIndex CSR 倒排 + 128 位签名 + 上界剪枝，
        // 跳过 95%+ 无关文件名并免除全表百万记录扫描；raw_path 记录随后单独补扫保证语义 100% 完备。
        let required_signature = query.name_terms.iter().fold(NameSignature::default(), |sig, term| {
            sig.union(if term.contains('*') || term.contains('?') {
                crate::search::wildcard::required_signature_for_glob(term)
            } else {
                NameSignature::from_substring_query(term)
            })
        });

        for name_id in 0..query_index.slot_count() as u32 {
            let sig = query_index.signature_at(name_id);
            if sig == NameSignature::default() && !query_index.is_ready(name_id) {
                continue;
            }
            if !sig.contains(required_signature) {
                continue;
            }
            let entries = query_index.entries(name_id);
            if entries.is_empty() {
                continue;
            }
            let Some(name) = read.name_text(name_id) else {
                continue;
            };
            let folded_opt = if name.is_ascii() {
                None
            } else {
                query_index.folded(name_id)
            };
            let Some(base_score) = score_name(
                name,
                folded_opt,
                query,
                has_wildcard,
                all_terms_ascii,
                &mut ascii_buf,
                case_sensitive,
                explicit_lock,
            ) else {
                continue;
            };

            if use_custom_sort {
                for &ordinal in entries {
                    let ord_usize = ordinal as usize;
                    let Some(record) = records.get(ord_usize) else {
                        continue;
                    };
                    if record.raw_path {
                        continue;
                    }
                    let key = super::sort_key_for_record(record, sort);
                    if let Some(worst) = sorted_top_k.worst() {
                        if key < worst.score || (key == worst.score && name > worst.name()) {
                            continue;
                        }
                    }
                    let parent_quality = parent_status
                        .get(record.path as usize)
                        .copied()
                        .unwrap_or(ParentQuality::None);
                    let is_dir_self = parent_quality == ParentQuality::None
                        && query.is_dir_only
                        && record.is_dir
                        && is_target_directory_self(read.record_parent(record), name, query);
                    if parent_quality == ParentQuality::None && !is_dir_self {
                        continue;
                    }
                    if let Some(ref matcher) = ext_matcher {
                        if !matcher.matches(record.extension) {
                            continue;
                        }
                    }
                    if !passthrough && !filter.matches_record_in(read, record, Some(name)) {
                        continue;
                    }
                    if let Some(view) = read.view(EntryId(ord_usize)) {
                        sorted_top_k.offer_path(ord_usize, key, view);
                    }
                }
                continue;
            }

            let upper = base_score.upper_bound_with_extra(
                &rank_ctx,
                allow_dirs,
                targets_hidden,
                PATH_BONUS_DIRECT,
            );
            if let Some(worst) = top_k.worst() {
                if upper < worst.score || (upper == worst.score && name > worst.name()) {
                    continue;
                }
            }

            let (head, tail) = ranking::bucket_slices(entries);
            for &ordinal in head.iter().chain(tail.iter()) {
                let ord_usize = ordinal as usize;
                let Some(record) = records.get(ord_usize) else {
                    continue;
                };
                if record.raw_path {
                    continue;
                }
                let parent_quality = parent_status
                    .get(record.path as usize)
                    .copied()
                    .unwrap_or(ParentQuality::None);
                let is_dir_self = parent_quality == ParentQuality::None
                    && query.is_dir_only
                    && record.is_dir
                    && is_target_directory_self(read.record_parent(record), name, query);
                if parent_quality == ParentQuality::None && !is_dir_self {
                    continue;
                }
                if let Some(ref matcher) = ext_matcher {
                    if !matcher.matches(record.extension) {
                        continue;
                    }
                }
                if !passthrough && !filter.matches_record_in(read, record, Some(name)) {
                    continue;
                }

                let path_bonus = match parent_quality {
                    ParentQuality::Direct => PATH_BONUS_DIRECT,
                    ParentQuality::Ancestor => PATH_BONUS_ANCESTOR,
                    ParentQuality::None => 0,
                };
                let (parent_depth, parent_hidden) = read.record_parent_meta(record, query_index);
                let final_score = ranking::score_record_with_meta(
                    base_score,
                    record.is_dir,
                    parent_depth,
                    parent_hidden,
                    &rank_ctx,
                    targets_hidden,
                    path_bonus,
                );
                top_k.offer(ord_usize, final_score, name);
            }
        }

        // 仅当索引中存在非池化原始路径记录（如 alias -> /actual）时补扫，按真实路径末段评估
        if has_raw_paths {
            for (ordinal, record) in records.iter().enumerate() {
                if !record.raw_path {
                    continue;
                }
                let (parent, name) = read.split_record(record);
                if use_custom_sort {
                    let key = super::sort_key_for_record(record, sort);
                    if let Some(worst) = sorted_top_k.worst() {
                        if key < worst.score || (key == worst.score && name > worst.name()) {
                            continue;
                        }
                    }
                }
                let parent_quality = evaluate_parent_quality(parent, query);
                let is_dir_self = query.is_dir_only
                    && record.is_dir
                    && is_target_directory_self(parent, name, query);
                if !is_dir_self && parent_quality == ParentQuality::None {
                    continue;
                }
                if let Some(ref matcher) = ext_matcher {
                    if !matcher.matches(record.extension) {
                        continue;
                    }
                }
                if !passthrough && !filter.matches_record_in(read, record, Some(name)) {
                    continue;
                }
                let raw_folded = (!name.is_ascii() && crate::cjk::needs_fold(name))
                    .then(|| crate::cjk::fold_str(name));
                let Some(base_score) = score_name(
                    name,
                    raw_folded.as_deref(),
                    query,
                    has_wildcard,
                    all_terms_ascii,
                    &mut ascii_buf,
                    case_sensitive,
                    explicit_lock,
                ) else {
                    continue;
                };
                if use_custom_sort {
                    let key = super::sort_key_for_record(record, sort);
                    if let Some(view) = read.view(EntryId(ordinal)) {
                        sorted_top_k.offer_path(ordinal, key, view);
                    }
                } else {
                    let path_bonus = match parent_quality {
                        ParentQuality::Direct => PATH_BONUS_DIRECT,
                        ParentQuality::Ancestor => PATH_BONUS_ANCESTOR,
                        ParentQuality::None => 0,
                    };
                    let final_score = ranking::score_record_with_extra(
                        base_score,
                        record.is_dir,
                        parent,
                        &rank_ctx,
                        targets_hidden,
                        path_bonus,
                    );
                    top_k.offer(ordinal, final_score, name);
                }
            }
        }
    } else {
        // 阶段 1B：纯目录列举模式（如 "Desktop/"、"in:src"）：先用父目录状态表快速淘汰 99%+ 非目标记录，再执行过滤器与单字节元数据评分
        for (ordinal, record) in records.iter().enumerate() {
            if use_custom_sort {
                let key = super::sort_key_for_record(record, sort);
                if let Some(worst) = sorted_top_k.worst() {
                    if key < worst.score {
                        continue;
                    }
                }
            }
            let parent_quality = if record.raw_path {
                let (parent, _) = read.split_record(record);
                evaluate_parent_quality(parent, query)
            } else {
                parent_status
                    .get(record.path as usize)
                    .copied()
                    .unwrap_or(ParentQuality::None)
            };

            let is_dir_self = query.is_dir_only && record.is_dir && {
                let (parent, name) = read.split_record(record);
                is_target_directory_self(parent, name, query)
            };

            if !is_dir_self && parent_quality == ParentQuality::None {
                continue;
            }
            if let Some(ref matcher) = ext_matcher {
                if !matcher.matches(record.extension) {
                    continue;
                }
            }
            let name = read.record_name(record);
            if !passthrough && !filter.matches_record_in(read, record, Some(name)) {
                continue;
            }

            if use_custom_sort {
                let key = super::sort_key_for_record(record, sort);
                if let Some(view) = read.view(EntryId(ordinal)) {
                    sorted_top_k.offer_path(ordinal, key, view);
                }
            } else {
                // 纯目录模式（如 "Desktop/"）：直属子项取 Prefix 而非 StemExact，避免 .app 子包反向越过目录自身
                let base = if is_dir_self {
                    BaseNameScore::for_exact_fast_path(name)
                } else if parent_quality == ParentQuality::Direct {
                    BaseNameScore::for_tier_only(name, MatchTier::Prefix, 0)
                } else {
                    BaseNameScore::for_tier_only(name, MatchTier::Substring, 0)
                };
                let (parent_depth, parent_hidden) = read.record_parent_meta(record, query_index);
                let final_score = ranking::score_record_with_meta(
                    base,
                    record.is_dir,
                    parent_depth,
                    parent_hidden,
                    &rank_ctx,
                    targets_hidden,
                    0,
                );
                top_k.offer(ordinal, final_score, name);
            }
        }
    }

    // 阶段 2：仅对 Top-K 结果计算高亮区间
    let kw_prepared: Vec<(&str, String)> = query
        .name_terms
        .iter()
        .map(|t| (t.as_str(), crate::cjk::fold_str(t)))
        .collect();
    let kw_refs: Vec<(&str, &str)> = kw_prepared
        .iter()
        .map(|(raw, lower)| (*raw, lower.as_str()))
        .collect();
    let target_names: Vec<(&str, String)> = query
        .path_candidates
        .iter()
        .map(|cand| {
            let target = split_path(cand.pattern.trim_end_matches('/')).1;
            (target, crate::cjk::fold_str(target))
        })
        .collect();

    let chosen: Vec<(usize, f64)> = if use_custom_sort {
        sorted_top_k
            .finish()
            .into_iter()
            .map(|c| (c.ordinal, 1.0))
            .collect()
    } else {
        top_k
            .finish()
            .into_iter()
            .map(|c| (c.ordinal, c.score.to_f64()))
            .collect()
    };
    chosen
        .into_iter()
        .map(|(ordinal, base_f64)| {
            let view = read
                .view(EntryId(ordinal))
                .expect("valid compact reference");
            let (parent, name) = read.split_record(&read.records()[ordinal]);

            let is_dir_self = view.is_dir && is_target_directory_self(parent, name, query);
            let parent_match_positions = find_parent_highlight_range(parent, query)
                .into_iter()
                .collect();

            let name_folded = crate::cjk::fold_cow(name);
            let match_positions = if is_dir_self && query.name_terms.is_empty() {
                // 目标目录实体自身：优先匹配与真实目录名一致的候选别名并高亮其末级目录名
                match target_names
                    .iter()
                    .find(|(_, tl)| tl.as_str() == name_folded.as_ref())
                    .or_else(|| target_names.first())
                {
                    Some((target, target_lower)) => {
                        find_positions(view.name, &name_folded, &[(*target, target_lower.as_str())])
                    }
                    None => Vec::new(),
                }
            } else {
                find_positions(view.name, &name_folded, &kw_refs)
            };

            let score = if use_custom_sort {
                1.0
            } else if query.name_terms.is_empty()
                && is_dir_self
                && (targets_hidden || !ranking::is_record_hidden(name, parent))
            {
                1.0
            } else {
                base_f64
            };

            AdvancedSearchResult {
                entry: view.materialize(),
                score,
                match_positions,
                parent_match_positions,
                search_mode: SearchMode::Substring,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::{FileEntry, IndexStore};

    /// 同名记录分属不同目录：判定结果按唯一名复用后，命中与未命中都不能串到别的记录上。
    #[test]
    fn shared_names_keep_per_record_results() {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("main.rs", "/p/src/main.rs", false),
            make_entry("main.rs", "/p/lib/main.rs", false),
            make_entry("main.rs", "/q/src/main.rs", false),
            make_entry("other.rs", "/p/src/other.rs", false),
            make_entry("Main.RS", "/p/src/deep/Main.RS", false),
            make_entry("中文笔记.txt", "/p/src/中文笔记.txt", false),
            // 名字与路径末段不一致的原始路径记录：按路径末段搜索，不走按名缓存。
            make_entry("alias", "/p/src/alias", false),
            make_entry("alias", "/p/src/actual", false),
        ]);
        let run = |query: &str| {
            let query = PathQuery::parse(query).unwrap();
            let mut paths: Vec<_> = search(&store, &query, &CompiledRecordFilter::default(), 500)
                .into_iter()
                .map(|result| result.entry.path)
                .collect();
            paths.sort();
            paths
        };

        assert_eq!(
            run("src/main"),
            ["/p/src/deep/Main.RS", "/p/src/main.rs", "/q/src/main.rs"]
        );
        assert_eq!(run("lib/main"), ["/p/lib/main.rs"]);
        assert_eq!(run("src/ma*.rs"), ["/p/src/deep/Main.RS", "/p/src/main.rs", "/q/src/main.rs"]);
        assert_eq!(run("src/zhongwen"), ["/p/src/中文笔记.txt"]);
        assert_eq!(run("src/actual"), ["/p/src/actual"]);
        assert!(run("src/missing").is_empty());
    }

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

    fn test_store() -> IndexStore {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("main.rs", "/project/src/main.rs", false),
            make_entry("mod.rs", "/project/src/search/mod.rs", false),
            make_entry("mod.rs", "/project/src/watcher/mod.rs", false),
            make_entry("mod.rs", "/other/repo/src/mod.rs", false),
            make_entry("Desktop", "/Users/tester/Desktop", true),
            make_entry("Desktop", "/var/tmp/backup/Desktop", true),
            make_entry("notes.txt", "/Users/tester/Desktop/notes.txt", false),
            make_entry("deep.txt", "/Users/tester/Desktop/sub/deep.txt", false),
            make_entry("app.log", "/var/log/app.log", false),
            make_entry("app.log", "/opt/release/app.log", false),
        ]);
        store
    }

    #[test]
    fn test_parent_highlight_trailing_slash_trimmed() {
        let query = PathQuery::parse("src/main.rs").unwrap();
        let range = find_parent_highlight_range("/Users/tester/project/src/", &query);
        assert_eq!(range, Some((22, 25))); // 对应 "src"，不包含末尾斜杠
    }

    #[test]
    fn test_search_relative_path() {
        let store = test_store();
        let query = PathQuery::parse("src/main.rs").unwrap();
        let filter = CompiledRecordFilter::default();
        let results = search(&store, &query, &filter, 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "main.rs");
        assert_eq!(results[0].entry.path, "/project/src/main.rs");
        assert_eq!(results[0].parent_match_positions, vec![(9, 12)]); // "src" 长度为 3，不包含末尾斜杠
    }

    #[test]
    fn test_search_parent_filter_no_false_negatives() {
        let store = test_store();
        let query = PathQuery::parse("search/mod.rs").unwrap();
        let filter = CompiledRecordFilter::default();
        let results = search(&store, &query, &filter, 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.path, "/project/src/search/mod.rs");
    }

    #[test]
    fn test_search_dir_trailing_slash() {
        let store = test_store();
        let query = PathQuery::parse("Desktop/").unwrap();
        let filter = CompiledRecordFilter::default();
        let results = search(&store, &query, &filter, 10);

        // 应命中两个同名 Desktop 目录实体本身以及 Desktop 下的所有子孙文件
        assert_eq!(results.len(), 4);
        let paths: Vec<&str> = results.iter().map(|r| r.entry.path.as_str()).collect();
        assert!(paths.contains(&"/Users/tester/Desktop"));
        assert!(paths.contains(&"/var/tmp/backup/Desktop"));
        assert!(paths.contains(&"/Users/tester/Desktop/notes.txt"));
        assert!(paths.contains(&"/Users/tester/Desktop/sub/deep.txt"));

        // 目标目录本身得到最高分 500
        assert_eq!(results[0].score, 1.0);
    }

    #[test]
    fn test_search_dir_with_parent_prefix_no_false_positives() {
        let store = test_store();
        let query = PathQuery::parse("tester/Desktop/").unwrap();
        let filter = CompiledRecordFilter::default();
        let results = search(&store, &query, &filter, 10);

        // 仅匹配 /Users/tester/Desktop 及其子项，不匹配 /var/tmp/backup/Desktop
        let paths: Vec<&str> = results.iter().map(|r| r.entry.path.as_str()).collect();
        assert!(paths.contains(&"/Users/tester/Desktop"));
        assert!(!paths.contains(&"/var/tmp/backup/Desktop"));
    }

    #[test]
    fn test_search_multi_word_with_extension() {
        let store = test_store();
        let query = PathQuery::parse("release/app .log").unwrap();
        let filter = CompiledRecordFilter::default();
        let results = search(&store, &query, &filter, 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.path, "/opt/release/app.log");
    }

    #[test]
    fn test_search_scope_filter() {
        let store = test_store();
        let query = PathQuery::parse("in:/var/log/ app").unwrap();
        let filter = CompiledRecordFilter::default();
        let results = search(&store, &query, &filter, 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.path, "/var/log/app.log");
    }

    #[test]
    fn test_path_search_with_compiled_filter() {
        let store = test_store();
        let read = store.read();
        let query_filter = super::super::QueryFilter {
            file_type: super::super::FileType::Document,
            custom_ext: None,
            ..Default::default()
        };
        let filter = CompiledRecordFilter::compile(&read, &query_filter);
        drop(read);

        // Desktop/ 下包含目录 Desktop、notes.txt、deep.txt。Document 过滤下仅保留 notes.txt 与 deep.txt
        let query = PathQuery::parse("Desktop/").unwrap();
        let results = search(&store, &query, &filter, 10);
        assert_eq!(results.len(), 2);
        let names: Vec<&str> = results.iter().map(|r| r.entry.name.as_str()).collect();
        assert!(names.contains(&"notes.txt"));
        assert!(names.contains(&"deep.txt"));
    }

    #[test]
    fn test_cn_alias_parent_match_and_relative_in_scope_boundary() {
        let store = IndexStore::new();
        store.replace(vec![
            FileEntry {
                name: "Desktop".to_string(),
                path: "/Users/u/Desktop".to_string(),
                is_dir: true,
                size: 0,
                modified: 0,
                created_at: 0,
                extension: String::new(),
            },
            FileEntry {
                name: "周报.docx".to_string(),
                path: "/Users/u/Desktop/周报.docx".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "docx".to_string(),
            },
            FileEntry {
                name: "report.pdf".to_string(),
                path: "/Users/u/Downloads/report.pdf".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "pdf".to_string(),
            },
            FileEntry {
                name: "report.pdf".to_string(),
                path: "/Users/u/mydownloads/report.pdf".to_string(),
                is_dir: false,
                size: 100,
                modified: 0,
                created_at: 0,
                extension: "pdf".to_string(),
            },
        ]);
        let read = store.read();
        let filter = CompiledRecordFilter::compile(&read, &crate::search::filter::QueryFilter::default());
        drop(read);

        let q_alias = PathQuery::parse("桌面/周报").unwrap();
        let res_alias = search(&store, &q_alias, &filter, 10);
        assert_eq!(res_alias.len(), 1);
        assert_eq!(res_alias[0].entry.path, "/Users/u/Desktop/周报.docx");
        assert_eq!(res_alias[0].parent_match_positions, vec![(9, 16)]);

        let q_dir_self = PathQuery::parse("桌面/").unwrap();
        let res_self = search(&store, &q_dir_self, &filter, 10);
        let paths: Vec<&str> = res_self.iter().map(|r| r.entry.path.as_str()).collect();
        assert!(paths.contains(&"/Users/u/Desktop"));
        assert!(paths.contains(&"/Users/u/Desktop/周报.docx"));

        let q_scope = PathQuery::parse("in:下载 report").unwrap();
        let res_scope = search(&store, &q_scope, &filter, 10);
        let scope_paths: Vec<&str> = res_scope.iter().map(|r| r.entry.path.as_str()).collect();
        assert_eq!(scope_paths, vec!["/Users/u/Downloads/report.pdf"]);
    }
}
