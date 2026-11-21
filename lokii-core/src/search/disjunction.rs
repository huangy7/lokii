use crate::search::filter::{is_dotfile_query, parse_query_structured, ParsedQuery, QueryFilter};
use crate::search::{detect_mode_from_tokens, resolve_case_sensitive_tokens, CaseSensitivity};
use crate::string_pool::NameSignature;

/// 表示并集检索中的单个子句元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrClause {
    /// 词法切分提取出的原始子句查询字符串
    pub raw_query: String,
    /// 该子句独立判定的大小写敏感状态
    pub is_case_sensitive: bool,
    /// 候选文件扩展名（小写，不含前导点号）；`Some` 即代表扩展名匹配子句
    pub extension: Option<String>,
    /// 参与匹配与高亮计算的关键词集合（大小写已按敏感态规范化）
    pub keywords: Vec<String>,
    /// 保留原始大小写（经 NFC 规范化）的关键词集合，供排序精确大小写加分判定使用
    pub(crate) raw_keywords: Vec<String>,
    /// 基于子句关键词计算出的 128 位超集必需签名掩码，用于索引预筛选
    pub(crate) required_sig: NameSignature,
    /// 该子句作用域内的内联过滤条件与否定排除约束（如 `type:` / `!term` / `!.ext` / `!dir/`）
    pub filter: QueryFilter,
}

/// 并集检索查询模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisjunctionQuery {
    pub clauses: Vec<OrClause>,
}

impl DisjunctionQuery {
    /// 基于已完成结构化词法分析的单个子句构建 `OrClause`（供 `parse` 与调用方预解析未知扩展回退共用）。
    pub fn build_clause(raw: String, parsed: ParsedQuery, global_case: Option<bool>) -> OrClause {
        let case_setting = match global_case {
            Some(true) => CaseSensitivity::Sensitive,
            Some(false) => CaseSensitivity::Insensitive,
            None => CaseSensitivity::Auto,
        };

        let (is_extension, extension) =
            if parsed.positive_tokens.len() == 1 && !parsed.positive_tokens[0].quoted {
                parse_clause_extension(&parsed.positive_tokens[0].text)
            } else {
                (false, None)
            };

        let search_mode = detect_mode_from_tokens(&parsed.positive_tokens);
        let is_case_sensitive =
            resolve_case_sensitive_tokens(&parsed.positive_tokens, case_setting, search_mode);

        let mut keywords = Vec::new();
        let mut raw_keywords = Vec::new();
        if !is_extension {
            for token in &parsed.positive_tokens {
                let clean = token.text.as_str();
                if clean.is_empty() {
                    continue;
                }
                raw_keywords.push(crate::cjk::normalize_nfc_cow(clean).into_owned());
                keywords.push(if is_case_sensitive {
                    crate::cjk::normalize_and_simplify_cow(clean).into_owned()
                } else {
                    crate::cjk::fold_str(clean)
                });
            }
        }

        let required_sig = if is_extension {
            extension
                .as_deref()
                .map_or_else(NameSignature::default, extension_signature)
        } else {
            let mut sig = NameSignature::default();
            for kw in &keywords {
                sig = sig.union(NameSignature::from_substring_query(kw));
            }
            sig
        };

        OrClause {
            raw_query: raw,
            is_case_sensitive,
            extension,
            keywords,
            raw_keywords,
            required_sig,
            filter: parsed.filter,
        }
    }

    /// 解析输入查询字符串并构建子句模型。
    ///
    /// `global_case` 为 `None` 时，每个子句基于自身正向词元独立执行 Smart Case 启发式判定；
    /// 为 `Some(true)` 或 `Some(false)` 时，所有子句继承全局显式锁定的大小写敏感策略。
    pub fn parse(query: &str, global_case: Option<bool>) -> Self {
        let parts = split_query_pipes(query);
        let mut clauses = Vec::with_capacity(parts.len());

        for raw in parts {
            let parsed = parse_query_structured(&raw);
            clauses.push(Self::build_clause(raw, parsed, global_case));
        }

        Self { clauses }
    }

    /// 判定并集查询是否为空。
    pub fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }
}

/// 解析词元是否为纯扩展名匹配子句。
///
/// 当词元形如 `*.ext` 或以 `.` 开头（且经点文件规则判断非隐藏配置文件）时，返回 `(true, Some(ext))`；
/// 否则返回 `(false, None)`。
pub fn parse_clause_extension(term: &str) -> (bool, Option<String>) {
    let trimmed = term.trim();
    if let Some(ext) = trimmed.strip_prefix("*.") {
        if !ext.contains(' ') && !ext.contains('/') {
            return (true, Some(crate::cjk::fold_str(ext)));
        }
    }
    if trimmed.starts_with('.')
        && !is_dotfile_query(trimmed)
        && !trimmed.contains(' ')
        && !trimmed.contains('/')
    {
        return (true, Some(crate::cjk::fold_str(&trimmed[1..])));
    }
    (false, None)
}

/// 构建扩展名子句的 128 位预筛签名（点号 + 扩展名）。
pub(crate) fn extension_signature(extension: &str) -> NameSignature {
    if extension.is_empty() {
        NameSignature::default()
    } else {
        NameSignature::from_substring_query(".")
            .union(NameSignature::from_substring_query(extension))
    }
}

/// 判定文件名后缀是否命中目标扩展名。空扩展名视为命中。
pub(crate) fn extension_matches(
    name: &str,
    folded: Option<&str>,
    extension: &str,
    ext_ascii: bool,
) -> bool {
    if extension.is_empty() {
        return true;
    }
    if let Some(f) = folded {
        f.rsplit_once('.').is_some_and(|(_, ext)| ext == extension)
    } else if ext_ascii {
        name.rsplit_once('.')
            .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case(extension))
    } else {
        name.rsplit_once('.')
            .is_some_and(|(_, ext)| super::filter::ext_matches_folded(ext, extension))
    }
}

/// 扫描查询串中位于成对双引号 `"..."` 保护区之外且未被反斜杠转义的顶层管道符 `|`，
/// 按字节偏移依次回调 `on_pipe`；当回调返回 `true` 时提前终止扫描。
///
/// 仅当未转义 `"` 在后续切片中存在匹配的未转义闭合 `"` 时才跳过引号内部；
/// 未闭合的孤立 `"` 与词法状态机一致退回普通字面字符，不屏蔽后续 `|` 切分。
fn for_each_top_level_pipe(query: &str, mut on_pipe: impl FnMut(usize) -> bool) {
    let mut i = 0usize;
    let len = query.len();
    let mut backslash_count = 0usize;
    let mut has_closing_quote = true;

    while i < len {
        let ch = query[i..].chars().next().unwrap();
        let next_i = i + ch.len_utf8();

        if ch == '\\' {
            backslash_count += 1;
            i = next_i;
            continue;
        }

        let is_escaped = !backslash_count.is_multiple_of(2);
        backslash_count = 0;

        if ch == '"' && !is_escaped {
            if has_closing_quote {
                if let Some(close_idx) = super::filter::find_closing_quote(query, next_i) {
                    i = close_idx + 1;
                    continue;
                }
                has_closing_quote = false;
            }
            i = next_i;
            continue;
        }

        if ch == '|' && !is_escaped && on_pipe(i) {
            return;
        }

        i = next_i;
    }
}

/// 检测查询字符串中是否包含处于成对双引号保护区之外且未被转义的管道符 `|`。
///
/// 遵循极速短路策略：
/// 1. 若字符串不包含任何 `|` 字符，快速返回 `false`；
/// 2. 仅追踪存在闭合双引号的成对 `"` 保护区（单引号 `'` 与未闭合双引号均视为普通字面字符），成对引号内的管道符被保护忽略；
/// 3. 统计反斜杠 `\` 连续数量：奇数反斜杠表示转义（不视作有效管道符），偶数反斜杠表示反斜杠自转义（有效管道符）；
/// 4. 一旦扫描到处于保护区外且未转义的 `|`，立即短路返回 `true`。
pub fn has_unescaped_pipe(query: &str) -> bool {
    if !query.contains('|') {
        return false;
    }

    let mut found = false;
    for_each_top_level_pipe(query, |_| {
        found = true;
        true
    });
    found
}

/// 根据管道符 `|` 将并集检索语句切分为多个子句。
///
/// 状态机机制：
/// 1. 仅跟踪存在闭合双引号的成对 `"` 保护区（单引号 `'` 与未闭合双引号均视为普通字面字符），成对双引号内的管道符不被拆分；
/// 2. 统计管道符前驱连续反斜杠数量：奇数反斜杠表示转义管道符（不拆分），偶数反斜杠表示字面量反斜杠（执行拆分）；
/// 3. 对切分后的各子句执行首尾空白剔除，忽略空子句。
pub fn split_query_pipes(query: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut part_start = 0usize;

    for_each_top_level_pipe(query, |pipe_idx| {
        let trimmed = query[part_start..pipe_idx].trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
        part_start = pipe_idx + 1;
        false
    });

    let trimmed = query[part_start..].trim();
    if !trimmed.is_empty() {
        parts.push(trimmed.to_string());
    }

    parts
}

/// 将被转义的管道符 `\|` 还原为字面量字符 `|`。
///
/// 仅当管道符前驱为奇数个连续反斜杠时，消耗前驱的最后一个转义反斜杠并将管道符视作普通字符；
/// 偶数个反斜杠则按自转义规则完整保留。
pub fn unescape_literal_pipes(term: &str) -> String {
    let mut result = String::with_capacity(term.len());
    let mut backslash_count = 0;

    for c in term.chars() {
        if c == '\\' {
            backslash_count += 1;
        } else if c == '|' {
            if backslash_count % 2 == 1 {
                for _ in 0..(backslash_count - 1) {
                    result.push('\\');
                }
                result.push('|');
            } else {
                for _ in 0..backslash_count {
                    result.push('\\');
                }
                result.push('|');
            }
            backslash_count = 0;
        } else {
            for _ in 0..backslash_count {
                result.push('\\');
            }
            backslash_count = 0;
            result.push(c);
        }
    }

    for _ in 0..backslash_count {
        result.push('\\');
    }

    result
}

/// 合并高亮区间，实现包含、重叠、相邻三态合并，并对越界区间执行边界守卫裁剪。
pub fn merge_highlight_spans(
    spans: Vec<(usize, usize)>,
    text_len: usize,
) -> Vec<(usize, usize)> {
    if spans.is_empty() || text_len == 0 {
        return Vec::new();
    }

    // 过滤空区间与倒置区间，并将区间裁剪至 [0, text_len] 边界内
    let mut valid_spans: Vec<(usize, usize)> = spans
        .into_iter()
        .filter_map(|(start, end)| {
            let s = start.min(text_len);
            let e = end.min(text_len);
            if s < e {
                Some((s, e))
            } else {
                None
            }
        })
        .collect();

    if valid_spans.is_empty() {
        return Vec::new();
    }

    // 优先按起始位置升序排序，起始位置相同时按结束位置升序
    valid_spans.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let mut merged = Vec::with_capacity(valid_spans.len());
    let mut current = valid_spans[0];

    for next in valid_spans.into_iter().skip(1) {
        if next.0 <= current.1 {
            // 包含、重叠或相邻态：直接将结束位置延伸至二者最大值
            current.1 = current.1.max(next.1);
        } else {
            // 离散断开：提交当前已合并区间，开启新区间
            merged.push(current);
            current = next;
        }
    }
    merged.push(current);

    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_clause_extension_variants() {
        assert_eq!(
            parse_clause_extension("*.png"),
            (true, Some("png".to_string()))
        );
        assert_eq!(
            parse_clause_extension("*.PNG"),
            (true, Some("png".to_string()))
        );
        assert_eq!(
            parse_clause_extension(".jpg"),
            (true, Some("jpg".to_string()))
        );
        assert_eq!(parse_clause_extension(".gitignore"), (false, None));
        assert_eq!(parse_clause_extension("report.pdf"), (false, None));
        assert_eq!(parse_clause_extension("plain"), (false, None));
        assert_eq!(
            parse_clause_extension("*."),
            (true, Some("".to_string()))
        );
    }

    #[test]
    fn test_disjunction_query_smart_case_independence() {
        let q = DisjunctionQuery::parse("Report | summary", None);
        assert_eq!(q.clauses.len(), 2);
        // 第一子句包含大写字母，独立推导为区分大小写
        assert!(q.clauses[0].is_case_sensitive);
        assert_eq!(q.clauses[0].keywords, vec!["Report"]);

        // 第二子句全小写，独立推导为忽略大小写
        assert!(!q.clauses[1].is_case_sensitive);
        assert_eq!(q.clauses[1].keywords, vec!["summary"]);

        // 必需签名掩码包含对应小写子串
        let sig0 = NameSignature::from_substring_query("report");
        assert!(q.clauses[0].required_sig.contains(sig0));

        let sig1 = NameSignature::from_substring_query("summary");
        assert!(q.clauses[1].required_sig.contains(sig1));
    }

    #[test]
    fn test_disjunction_query_global_case_override() {
        let q_sensitive = DisjunctionQuery::parse("report | summary", Some(true));
        assert!(q_sensitive.clauses[0].is_case_sensitive);
        assert!(q_sensitive.clauses[1].is_case_sensitive);

        let q_insensitive = DisjunctionQuery::parse("Report | Summary", Some(false));
        assert!(!q_insensitive.clauses[0].is_case_sensitive);
        assert!(!q_insensitive.clauses[1].is_case_sensitive);
    }

    #[test]
    fn test_merge_highlight_spans_edge_cases() {
        // 空列表与 0 长度
        assert_eq!(merge_highlight_spans(vec![], 10), vec![]);
        assert_eq!(merge_highlight_spans(vec![(0, 5)], 0), vec![]);

        // 越界区间截断
        assert_eq!(
            merge_highlight_spans(vec![(8, 15)], 10),
            vec![(8, 10)]
        );

        // 完全越界丢弃
        assert_eq!(merge_highlight_spans(vec![(12, 15)], 10), vec![]);

        // 倒置区间与空区间丢弃
        assert_eq!(
            merge_highlight_spans(vec![(5, 5), (6, 4), (0, 3)], 10),
            vec![(0, 3)]
        );

        // 乱序输入正确合并
        assert_eq!(
            merge_highlight_spans(vec![(5, 8), (0, 3), (2, 6)], 10),
            vec![(0, 8)]
        );
    }

    #[test]
    fn test_has_unescaped_pipe_variants() {
        assert!(!has_unescaped_pipe("main"));
        assert!(has_unescaped_pipe("main | test"));
        assert!(has_unescaped_pipe("main |"));
        assert!(has_unescaped_pipe("|"));
        assert!(has_unescaped_pipe("||"));
        assert!(!has_unescaped_pipe("main \\| test"));
        assert!(has_unescaped_pipe("main \\\\| test"));
        assert!(!has_unescaped_pipe("\"main | test\""));
        assert!(has_unescaped_pipe("'main | test'"));
        assert!(has_unescaped_pipe("John's | Bob's"));
        assert!(has_unescaped_pipe(r#""unclosed | foo"#));
        assert!(!has_unescaped_pipe(r#""a|b""#));
    }

    #[test]
    fn test_unclosed_quote_splits_pipe_consistently_with_tokenize() {
        assert_eq!(split_query_pipes(r#""a|b""#), vec![r#""a|b""#]);
        assert_eq!(
            split_query_pipes(r#""unclosed | foo"#),
            vec![r#""unclosed"#, "foo"]
        );
        assert_eq!(
            split_query_pipes(r#""a|b" | "unclosed | c"#),
            vec![r#""a|b""#, r#""unclosed"#, "c"]
        );
        assert_eq!(
            split_query_pipes(r#""a | \"b\" | c""#),
            vec![r#""a | \"b\" | c""#]
        );
        assert_eq!(
            split_query_pipes(r#""a | b\\" | c"#),
            vec![r#""a | b\\""#, "c"]
        );
        assert_eq!(split_query_pipes(r#"\"a | b"#), vec![r#"\"a"#, "b"]);

        let q = DisjunctionQuery::parse(r#""unclosed | foo"#, None);
        assert_eq!(q.clauses.len(), 2);
        assert_eq!(q.clauses[0].keywords, vec!["\"unclosed"]);
        assert_eq!(q.clauses[1].keywords, vec!["foo"]);
    }
}
