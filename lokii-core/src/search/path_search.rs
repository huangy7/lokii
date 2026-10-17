use crate::compact_index::{split_path, EntryId};
use crate::indexer::{score_match, IndexStore, MatchScore};
use crate::search::path_query::PathQuery;
use crate::search::top_k::TopK;
use crate::search::{find_positions, AdvancedSearchResult, SearchMode};

/// 目录实体自身命中时的基准分。
const SCORE_DIR_SELF: u32 = 500;
/// 父目录恰为查询目标目录时的基准分。
const SCORE_DIRECT_PARENT: u32 = 400;
/// 父目录仅为查询目标目录的祖先时的基准分。
const SCORE_ANCESTOR_PARENT: u32 = 300;
/// 文件名精确匹配的基准分。
const SCORE_NAME_EXACT: u32 = 300;
/// 文件名前缀匹配或通配符命中的基准分。
const SCORE_NAME_PREFIX: u32 = 200;
/// 文件名包含匹配的基准分。
const SCORE_NAME_SUBSTRING: u32 = 100;
/// 父目录恰为查询目标目录时的加成。
const PATH_BONUS_DIRECT: u32 = 150;
/// 父目录仅为查询目标目录的祖先时的加成。
const PATH_BONUS_ANCESTOR: u32 = 50;
/// 归一化基准：文件名精确匹配叠加直接父级加成的上限。
const MAX_SCORE: f64 = (SCORE_NAME_EXACT + PATH_BONUS_DIRECT) as f64;

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

/// 在已转小写的父目录上判定约束，同时产出质量层级与高亮区间。
///
/// 绝对路径按前缀命中；相对路径优先取结尾命中的直接父级，其次取中段命中的祖先。
fn match_parent(parent_lower: &str, query: &PathQuery) -> Option<ParentMatch> {
    if let Some(ref scope) = query.scope_prefix {
        if !parent_lower.starts_with(scope.as_str()) {
            return None;
        }
    }

    let Some(ref pat) = query.path_pattern else {
        // 无父目录模式：作用域本身即直接命中；无约束时为零长区间（不高亮）。
        return Some(ParentMatch {
            quality: ParentQuality::Direct,
            span: query
                .scope_prefix
                .as_ref()
                .map_or((0, 0), |scope| (0, scope.len())),
        });
    };

    if query.is_absolute_prefix {
        if !parent_lower.starts_with(pat.as_str()) {
            return None;
        }
        let quality = if parent_lower == pat.as_str() {
            ParentQuality::Direct
        } else {
            ParentQuality::Ancestor
        };
        Some(ParentMatch {
            quality,
            span: (0, pat.len()),
        })
    } else if parent_lower.ends_with(pat.as_str()) {
        Some(ParentMatch {
            quality: ParentQuality::Direct,
            span: (parent_lower.len() - pat.len(), parent_lower.len()),
        })
    } else {
        let start = parent_lower.find(pat.as_str())?;
        Some(ParentMatch {
            quality: ParentQuality::Ancestor,
            span: (start, start + pat.len()),
        })
    }
}

/// 预判单个父目录的约束质量。
pub(crate) fn evaluate_parent_quality(parent_str: &str, query: &PathQuery) -> ParentQuality {
    match match_parent(&parent_str.to_lowercase(), query) {
        Some(matched) => matched.quality,
        None => ParentQuality::None,
    }
}

/// 当前目录实体是否正是查询的目标目录（如 "Desktop/"）。
pub(crate) fn is_target_directory_self(parent: &str, name: &str, query: &PathQuery) -> bool {
    if !query.is_dir_only {
        return false;
    }
    let Some(ref pat) = query.path_pattern else {
        return false;
    };
    let pat_clean = pat.trim_end_matches('/');
    let target_name = split_path(pat_clean).1;
    // 末级目录名必须完全相等（忽略大小写）
    if !name.eq_ignore_ascii_case(target_name) {
        return false;
    }
    let parent_lower = parent.to_lowercase();
    // in: 作用域校验
    if let Some(ref scope) = query.scope_prefix {
        if !parent_lower.starts_with(scope.as_str()) {
            return false;
        }
    }
    // 相对或绝对路径前缀匹配
    if query.is_absolute_prefix {
        format!("{parent}{name}/")
            .to_lowercase()
            .starts_with(pat.as_str())
    } else {
        let (dir_part, _) = split_path(pat_clean);
        dir_part.is_empty() || parent_lower.ends_with(dir_part)
    }
}

/// 计算父目录高亮区间，并剪裁末尾 '/' 以匹配前端展示串。
pub(crate) fn find_parent_highlight_range(
    parent: &str,
    query: &PathQuery,
) -> Option<(usize, usize)> {
    let (start, mut end) = match_parent(&parent.to_lowercase(), query)?.span;

    // 前端展示串不含末尾 '/'，区间若含之会越界并丢失整段高亮，故剪裁。
    while end > start && parent.as_bytes().get(end - 1) == Some(&b'/') {
        end -= 1;
    }
    (start < end).then_some((start, end))
}

/// 执行路径感知流式检索。
pub fn search(
    store: &IndexStore,
    query: &PathQuery,
    max_results: usize,
) -> Vec<AdvancedSearchResult> {
    if max_results == 0 {
        return Vec::new();
    }

    let read = store.read();

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

    let mut top_k = TopK::new(max_results);

    // 阶段 1：单趟扫描记录
    for (ordinal, record) in read.records().iter().enumerate() {
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
            if !entry_ext.eq_ignore_ascii_case(expected_ext) {
                continue;
            }
        }

        // 3. 打分
        let name = read.record_name(record);
        let final_score: u32 = if query.name_terms.is_empty() {
            // 纯目录模式（如 "Desktop/"）
            if is_dir_self {
                SCORE_DIR_SELF
            } else if parent_quality == ParentQuality::Direct {
                SCORE_DIRECT_PARENT
            } else {
                SCORE_ANCESTOR_PARENT
            }
        } else {
            // 带文件名检索（如 "src/main.rs"）
            let name_lower = name.to_lowercase();
            let base_score: u32 = if has_wildcard {
                let all_matched = query.name_terms.iter().all(|term| {
                    if term.contains('*') || term.contains('?') {
                        glob_match::glob_match(term, &name_lower)
                    } else {
                        name_lower.contains(term.as_str())
                    }
                });
                if !all_matched {
                    continue;
                }
                SCORE_NAME_PREFIX
            } else {
                let Some(score) = score_match(&name_lower, &query.name_terms) else {
                    continue;
                };
                match score {
                    MatchScore::Exact => SCORE_NAME_EXACT,
                    MatchScore::Prefix => SCORE_NAME_PREFIX,
                    MatchScore::Substring => SCORE_NAME_SUBSTRING,
                }
            };

            let path_bonus: u32 = match parent_quality {
                ParentQuality::Direct => PATH_BONUS_DIRECT,
                ParentQuality::Ancestor => PATH_BONUS_ANCESTOR,
                ParentQuality::None => 0,
            };

            base_score + path_bonus
        };

        top_k.offer(ordinal, final_score, name);
    }

    // 阶段 2：仅对 Top-K 结果计算高亮区间
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

            let name_lower = name.to_lowercase();
            let match_positions = if is_dir_self && query.name_terms.is_empty() {
                // 目标目录实体自身：高亮其末级目录名
                match query.path_pattern.as_deref() {
                    Some(pat) => {
                        let target_name = split_path(pat.trim_end_matches('/')).1;
                        find_positions(&name_lower, [target_name])
                    }
                    None => Vec::new(),
                }
            } else {
                find_positions(&name_lower, query.name_terms.iter().map(String::as_str))
            };

            AdvancedSearchResult {
                entry: view.materialize(),
                score: (f64::from(candidate.score) / MAX_SCORE).min(1.0),
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
        let results = search(&store, &query, 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "main.rs");
        assert_eq!(results[0].entry.path, "/project/src/main.rs");
        assert_eq!(results[0].parent_match_positions, vec![(9, 12)]); // "src" 长度为 3，不包含末尾斜杠
    }

    #[test]
    fn test_search_parent_filter_no_false_negatives() {
        let store = test_store();
        let query = PathQuery::parse("search/mod.rs").unwrap();
        let results = search(&store, &query, 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.path, "/project/src/search/mod.rs");
    }

    #[test]
    fn test_search_dir_trailing_slash() {
        let store = test_store();
        let query = PathQuery::parse("Desktop/").unwrap();
        let results = search(&store, &query, 10);

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
        let results = search(&store, &query, 10);

        // 仅匹配 /Users/tester/Desktop 及其子项，不匹配 /var/tmp/backup/Desktop
        let paths: Vec<&str> = results.iter().map(|r| r.entry.path.as_str()).collect();
        assert!(paths.contains(&"/Users/tester/Desktop"));
        assert!(!paths.contains(&"/var/tmp/backup/Desktop"));
    }

    #[test]
    fn test_search_multi_word_with_extension() {
        let store = test_store();
        let query = PathQuery::parse("release/app .log").unwrap();
        let results = search(&store, &query, 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.path, "/opt/release/app.log");
    }

    #[test]
    fn test_search_scope_filter() {
        let store = test_store();
        let query = PathQuery::parse("in:/var/log/ app").unwrap();
        let results = search(&store, &query, 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.path, "/var/log/app.log");
    }
}
