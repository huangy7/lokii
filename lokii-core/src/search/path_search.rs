use crate::compact_index::{split_path, CompactIndex, EntryId};
use crate::indexer::IndexStore;
use crate::search::path_query::{PathCandidate, PathQuery};
use crate::search::ranking::{
    self, BaseNameScore, MatchTier, RankScore, RecordRankContext, PATH_BONUS_ANCESTOR,
    PATH_BONUS_DIRECT,
};
use crate::search::top_k::TopK;
use crate::search::{find_positions, AdvancedSearchResult, CompiledRecordFilter, SearchMode};
use crate::string_pool::NameSignature;

#[derive(Clone, Copy)]
enum NameScoreSlot {
    Unscored,
    Miss,
    Hit(BaseNameScore),
}

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
pub(crate) fn match_scope_candidates(
    parent_folded: &str,
    scope_candidates: &[PathCandidate],
) -> Option<(usize, usize)> {
    for cand in scope_candidates {
        let pat = cand.pattern.as_str();
        if cand.is_absolute {
            if parent_folded.starts_with(pat) {
                return Some((0, pat.len()));
            }
        } else if parent_folded.starts_with(pat) {
            return Some((0, pat.len()));
        } else {
            let needle = format!("/{pat}");
            if let Some(pos) = parent_folded.find(&needle) {
                let start = pos + 1;
                return Some((start, start + pat.len()));
            }
        }
    }
    None
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
            let boundary_suffix = format!("/{pat}");
            if parent_folded.as_ref() == pat || parent_folded.ends_with(&boundary_suffix) {
                (
                    ParentQuality::Direct,
                    parent_folded.len() - pat.len(),
                    parent_folded.len(),
                )
            } else if parent_folded.starts_with(pat) {
                (ParentQuality::Ancestor, 0, pat.len())
            } else if let Some(pos) = parent_folded.find(&boundary_suffix) {
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
pub(crate) fn is_target_directory_self(
    parent: &str,
    name: &str,
    query: &PathQuery,
) -> bool {
    if !query.is_dir_only || query.path_candidates.is_empty() {
        return false;
    }
    let name_folded = crate::cjk::fold_cow(name);
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
        let target_name = split_path(pat_clean).1;
        // 目录名匹配恒定大小写无关且 NFC 等价
        if name_folded.as_ref() != target_name {
            continue;
        }
        if cand.is_absolute {
            let full = format!("{parent_folded}{name_folded}/");
            if full.starts_with(pat) {
                return true;
            }
        } else {
            let (dir_part, _) = split_path(pat_clean);
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
    if max_results == 0 {
        return Vec::new();
    }

    // 阶段 0：父目录池预判
    // 每个父目录只判定一次，状态表约 30KB，常驻 L1 缓存。
    let parent_status = read.evaluate_parents(ParentQuality::None, |parent| {
        evaluate_parent_quality(parent, query)
    });

    // 通配符判定只依赖查询，提到循环外。
    let has_wildcard = query
        .name_terms
        .iter()
        .any(|term| term.contains('*') || term.contains('?'));
    let all_terms_ascii = query.name_terms.iter().all(|t| t.is_ascii());

    let mut top_k = TopK::new(max_results);
    let mut ascii_buf = Vec::with_capacity(128);
    let rank_ctx = RecordRankContext::default();
    let targets_hidden = path_query_targets_hidden(query);

    // 宽路径下大量记录共用同一批文件名：每个唯一名只判定一次并缓存 `BaseNameScore`；
    // 原始路径记录（`record.raw_path == true`）不在名字池中，继续走现场拆解与非缓存分支。
    let query_index = read.query_index();
    let required_signature = query.name_terms.iter().fold(NameSignature::default(), |sig, term| {
        sig.union(if term.contains('*') || term.contains('?') {
            crate::search::wildcard::required_signature_for_glob(term)
        } else {
            NameSignature::from_substring_query(term)
        })
    });
    let mut name_scores = if query.name_terms.is_empty() {
        Vec::new()
    } else {
        vec![NameScoreSlot::Unscored; query_index.slot_count()]
    };

    // 阶段 1：单趟扫描记录
    for (ordinal, record) in read.records().iter().enumerate() {
        if !filter.matches_record_in(read, record, None) {
            continue;
        }

        // 1. 查表取父路径匹配质量
        let parent_quality = if record.raw_path {
            let (parent, _) = read.split_record(record);
            evaluate_parent_quality(parent, query)
        } else {
            parent_status
                .get(record.path as usize)
                .copied()
                .unwrap_or(ParentQuality::None)
        };

        // 纯目录查询：判断条目本身是否即目标目录
        let is_dir_self = query.is_dir_only && record.is_dir && {
            let (parent, name) = read.split_record(record);
            is_target_directory_self(parent, name, query)
        };

        // 非目标目录且父路径不匹配者立即跳过（绝大多数记录在此返回）
        if !is_dir_self && parent_quality == ParentQuality::None {
            continue;
        }

        // 2. 扩展名过滤
        if let Some(ref expected_ext) = query.extension {
            let entry_ext = read.extension(record.extension).unwrap_or("");
            if !super::filter::ext_matches_folded(entry_ext, expected_ext) {
                continue;
            }
        }

        // 3. 名字判定（池化记录查/写缓存，raw_path 记录保持非缓存）与记录级上下文打分
        let memo_slot = (!record.raw_path).then_some(record.name as usize);
        let remembered = memo_slot
            .and_then(|slot| name_scores.get(slot).copied())
            .unwrap_or(NameScoreSlot::Unscored);
        if matches!(remembered, NameScoreSlot::Miss) {
            continue;
        }
        let raw_folded;
        let (name, folded_opt) = if record.raw_path {
            let n = read.record_name(record);
            if !n.is_ascii() && crate::cjk::needs_fold(n) {
                raw_folded = Some(crate::cjk::fold_str(n));
                (n, raw_folded.as_deref())
            } else {
                (n, None)
            }
        } else {
            match read.name_entry(record.name) {
                Some((n, f)) => (n, f),
                None => ("", None),
            }
        };
        let parent = read.record_parent(record);
        let final_score: RankScore = if query.name_terms.is_empty() {
            // 纯目录模式（如 "Desktop/"）：直属子项取 Prefix 而非 StemExact，避免 .app 子包反向越过目录自身
            let base = if is_dir_self {
                BaseNameScore::for_exact_fast_path(name)
            } else if parent_quality == ParentQuality::Direct {
                BaseNameScore::for_tier_only(name, MatchTier::Prefix, 0)
            } else {
                BaseNameScore::for_tier_only(name, MatchTier::Substring, 0)
            };
            ranking::score_record(base, record.is_dir, parent, &rank_ctx, targets_hidden)
        } else {
            // 带文件名检索（如 "src/main.rs"）
            let base_score = match remembered {
                NameScoreSlot::Hit(base) => base,
                NameScoreSlot::Unscored | NameScoreSlot::Miss => {
                    let pruned = !record.raw_path
                        && query_index
                            .signature(record.name)
                            .is_some_and(|signature| !signature.contains(required_signature));
                    let scored = if pruned {
                        None
                    } else {
                        score_name(
                            name,
                            folded_opt,
                            query,
                            has_wildcard,
                            all_terms_ascii,
                            &mut ascii_buf,
                            case_sensitive,
                            explicit_lock,
                        )
                    };
                    if let Some(memo) = memo_slot.and_then(|slot| name_scores.get_mut(slot)) {
                        *memo = scored.map_or(NameScoreSlot::Miss, NameScoreSlot::Hit);
                    }
                    let Some(base) = scored else {
                        continue;
                    };
                    base
                }
            };

            let path_bonus = match parent_quality {
                ParentQuality::Direct => PATH_BONUS_DIRECT,
                ParentQuality::Ancestor => PATH_BONUS_ANCESTOR,
                ParentQuality::None => 0,
            };
            ranking::score_record_with_extra(
                base_score,
                record.is_dir,
                parent,
                &rank_ctx,
                targets_hidden,
                path_bonus,
            )
        };

        top_k.offer(ordinal, final_score, name);
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

    let candidates = top_k.finish();
    candidates
        .into_iter()
        .map(|candidate| {
            let view = read
                .view(EntryId(candidate.ordinal))
                .expect("valid compact reference");
            let (parent, name) = read.split_record(&read.records()[candidate.ordinal]);

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

            let score = if query.name_terms.is_empty()
                && is_dir_self
                && (targets_hidden || !ranking::is_record_hidden(name, parent))
            {
                1.0
            } else {
                candidate.score.to_f64()
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
