use crate::compact_index::EntryId;
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
const MAX_SCORE: f64 = 450.0;

/// 标识父路径匹配的质量层级。
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParentQuality {
    None = 0,
    Ancestor = 1,
    Direct = 2,
}

/// 父路径与查询约束的匹配结果：质量层级与高亮区间同源产出。
#[derive(Clone, Copy)]
struct ParentMatch {
    quality: ParentQuality,
    span: (usize, usize),
}

/// 在已转小写的父目录字符串上执行一次约束判定。
///
/// 质量层级与高亮区间出自同一次判定，避免两处规则各自演化后产生偏差：
/// 绝对路径按前缀命中，相对路径优先取结尾命中的直接父级，其次取中段命中的祖先路径。
fn match_parent(parent_lower: &str, query: &PathQuery) -> Option<ParentMatch> {
    if let Some(ref scope) = query.scope_prefix {
        if !parent_lower.starts_with(scope.as_str()) {
            return None;
        }
    }

    let Some(ref pat) = query.path_pattern else {
        // 无父目录模式：作用域本身即为直接命中；无任何约束时退化为零长区间（不产生高亮）。
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

/// 对单个父目录字符串执行路径约束预判，返回紧凑的单字节质量等级（L1 缓存亲和）。
pub fn evaluate_parent_quality(parent_str: &str, query: &PathQuery) -> ParentQuality {
    match match_parent(&parent_str.to_lowercase(), query) {
        Some(matched) => matched.quality,
        None => ParentQuality::None,
    }
}

/// 判定当前目录实体本身是否恰好即为用户查询的目标目录（如 "Desktop/" 或 "tester/Desktop/"）。
pub fn is_target_directory_self(parent: &str, name: &str, query: &PathQuery) -> bool {
    if !query.is_dir_only {
        return false;
    }
    let Some(ref pat) = query.path_pattern else {
        return false;
    };
    let pat_clean = pat.trim_end_matches('/');
    let target_name = pat_clean
        .rfind('/')
        .map_or(pat_clean, |idx| &pat_clean[idx + 1..]);
    // 快速短路：末级目录名必须完全相等（忽略大小写）
    if !name.eq_ignore_ascii_case(target_name) {
        return false;
    }
    let parent_lower = parent.to_lowercase();
    // 作用域前缀校验 (in:<path>)
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
    } else if let Some(last_slash) = pat_clean.rfind('/') {
        parent_lower.ends_with(&pat[..=last_slash])
    } else {
        true
    }
}

/// 计算父目录字符串中的高亮区间，并自动剪裁末尾斜杠以适配前端展示。
pub fn find_parent_highlight_range(parent: &str, query: &PathQuery) -> Option<(usize, usize)> {
    let (start, mut end) = match_parent(&parent.to_lowercase(), query)?.span;

    // 关键边界守卫：去除区间末尾的 '/'。
    // UI 视图层展示的父路径不含末尾斜杠，若高亮区间包含末尾 '/'，
    // 其 end 索引将溢出 displayString 的实际字节长度，触发安全守卫导致整段高亮丢失。
    // 剪裁后精准覆盖目录名字节范围。
    while end > start && parent.as_bytes().get(end - 1) == Some(&b'/') {
        end -= 1;
    }
    (start < end).then_some((start, end))
}

/// 执行路径感知流式检索。
pub fn search(store: &IndexStore, query: &PathQuery, max_results: usize) -> Vec<AdvancedSearchResult> {
    if max_results == 0 {
        return Vec::new();
    }

    let read = store.read();

    // 阶段 0：父目录池前置预判（Parent Pool Pre-Evaluation）
    // 遍历 parents 字符串池（~3万项），生成仅 30KB 的 L1 缓存常驻状态向量表，
    // 让共享同一父目录的成千上万条记录只承担一次模式判定。
    let parent_status = read.evaluate_parents(ParentQuality::None, |parent| {
        evaluate_parent_quality(parent, query)
    });

    // 循环不变式：通配符判定只依赖查询，与具体记录无关
    let has_wildcard = query
        .name_terms
        .iter()
        .any(|term| term.contains('*') || term.contains('?'));

    let mut top_k = TopK::new(max_results);

    // 阶段 1：单趟流式记录扫描（Single-Pass Streaming Scan）
    for (ordinal, record) in read.records().iter().enumerate() {
        // 1. 获取父路径匹配质量（单指令周期数组查表）
        let parent_quality = if record.raw_path {
            let (parent, _) = read.split_record(record);
            evaluate_parent_quality(parent, query)
        } else {
            parent_status
                .get(record.path as usize)
                .copied()
                .unwrap_or(ParentQuality::None)
        };

        // 纯目录查询时（如 "Desktop/"），检查当前条目是否即为目标目录实体本身
        let is_dir_self = query.is_dir_only && record.is_dir && {
            let (parent, name) = read.split_record(record);
            is_target_directory_self(parent, name, query)
        };

        // 短路守卫：非目标目录本身且父路径不匹配者，立即跳过（99% 数据在此处以 O(1) 返回）
        if !is_dir_self && parent_quality == ParentQuality::None {
            continue;
        }

        // 2. 扩展名快速过滤
        if let Some(ref expected_ext) = query.extension {
            let entry_ext = read.extension(record.extension).unwrap_or("");
            if !entry_ext.eq_ignore_ascii_case(expected_ext) {
                continue;
            }
        }

        // 3. 计算最终打分（按需获取文件名）
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
            // 带文件名检索（如 "src/main.rs"、"src/*.rs"、"build release/app"）
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

    // 阶段 2：仅对 Top-K 候选结果实例化并计算精准高亮区间
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
                        let pat_clean = pat.trim_end_matches('/');
                        let target_name = pat_clean
                            .rfind('/')
                            .map_or(pat_clean, |idx| &pat_clean[idx + 1..]);
                        find_positions(&name_lower, [target_name])
                    }
                    None => Vec::new(),
                }
            } else if query.extension.is_some() && query.name_terms.is_empty() {
                Vec::new()
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
