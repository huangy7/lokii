use crate::content_index::verify::ContentQueryMode;
use crate::search::filter::{parse_query_filter, QueryFilter};
use crate::search::path_query::PathQuery;
use crate::search::CaseSensitivity;

/// 从用户搜索串拆分出的内容检索指令与正交文件过滤条件。
#[derive(Debug, Clone)]
pub struct ParsedContentQuery {
    pub mode: ContentQueryMode,
    pub path_filter: ContentPathPredicate,
    pub query_filter: QueryFilter,
    pub remaining_path_query: String,
    pub is_case_sensitive: bool,
}

/// 内容检索候选路径与过滤条件的统一谓词，严格兼容 `in:` / `/path` / `ext:` / `type:` / `size:`。
#[derive(Debug, Clone)]
pub struct ContentPathPredicate {
    pub query_filter: QueryFilter,
    pub path_query: Option<PathQuery>,
    pub raw_terms: Vec<String>,
}

impl ContentPathPredicate {
    pub fn matches_path(&self, path: &str) -> bool {
        if !self.query_filter.matches_file_path(path, None) {
            return false;
        }
        let path_lower = path.to_lowercase();
        let (parent, name) = match path.rsplit_once('/') {
            Some((p, n)) => (p, n),
            None => ("", path),
        };
        let parent_lower = format!("{}/", parent.to_lowercase());
        let name_lower = name.to_lowercase();

        if let Some(ref pq) = self.path_query {
            if pq.is_dir_only {
                return false;
            }
            if let Some(ref scope) = pq.scope_prefix {
                if scope.starts_with('/') {
                    if !parent_lower.starts_with(scope.as_str()) {
                        return false;
                    }
                } else {
                    let needle = format!("/{scope}");
                    if !parent_lower.contains(&needle) {
                        return false;
                    }
                }
            }
            if let Some(ref pat) = pq.path_pattern {
                if pq.is_absolute_prefix {
                    if !parent_lower.starts_with(pat.as_str()) {
                        return false;
                    }
                } else if !parent_lower.contains(pat.as_str()) {
                    return false;
                }
            }
            if let Some(ref ext) = pq.extension {
                let file_ext = name_lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
                if file_ext != ext.as_str() {
                    return false;
                }
            }
            for term in &pq.name_terms {
                if !name_lower.contains(&term.to_lowercase()) {
                    return false;
                }
            }
            return true;
        }

        for term in &self.raw_terms {
            if !path_lower.contains(&term.to_lowercase()) {
                return false;
            }
        }
        true
    }
}

/// 快速判断原始输入是否显式包含内容检索词元（无 `:` 时 O(1) 零分配短路退出）。
#[inline]
pub fn has_content_directive(input: &str) -> bool {
    if !input.contains(':') {
        return false;
    }
    tokenize_preserving_quotes(input).iter().any(|tok| {
        let lower = tok.to_ascii_lowercase();
        lower.starts_with("content:")
            || lower.starts_with("grep:")
            || lower.starts_with("c:")
            || lower.starts_with("cregex:")
            || lower.starts_with("sym:")
    })
}

/// 解析内容检索指令；若未激活 `force_content` 且输入不含内容指令则返回 `None`，确保主路径零额外损耗。
pub fn parse_content_query(
    raw_input: &str,
    case_sensitivity: CaseSensitivity,
    force_content: bool,
    force_regex: bool,
    force_symbol: bool,
) -> Option<ParsedContentQuery> {
    let trimmed = raw_input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let tokens = tokenize_preserving_quotes(trimmed);
    let mut explicit_pattern: Option<(String, bool, bool)> = None; // (pattern, is_regex, is_sym)
    let mut non_directive_tokens: Vec<String> = Vec::new();

    for tok in tokens {
        let lower = tok.to_ascii_lowercase();
        if let Some(rest) = strip_prefix_ci(&tok, &lower, "content:")
            .or_else(|| strip_prefix_ci(&tok, &lower, "grep:"))
            .or_else(|| strip_prefix_ci(&tok, &lower, "c:"))
        {
            let pat = unquote(rest);
            if !pat.is_empty() {
                explicit_pattern = Some((pat.to_string(), false, force_symbol));
            }
            continue;
        }
        if let Some(rest) = strip_prefix_ci(&tok, &lower, "cregex:") {
            let pat = unquote(rest);
            if !pat.is_empty() {
                explicit_pattern = Some((pat.to_string(), true, false));
            }
            continue;
        }
        if let Some(rest) = strip_prefix_ci(&tok, &lower, "sym:") {
            let pat = unquote(rest);
            if !pat.is_empty() {
                explicit_pattern = Some((pat.to_string(), false, true));
            }
            continue;
        }
        non_directive_tokens.push(tok);
    }

    let (pattern, is_regex, symbol_only, remaining_path_query, query_filter) =
        if let Some((pat, re, sym)) = explicit_pattern {
            let (remaining, qf) = if non_directive_tokens.is_empty() {
                (String::new(), QueryFilter::default())
            } else {
                // 前置占位词确保独立 `.ext` 过滤语境与正常多词查询一致（仍豁免 `.gitignore` 等知名点文件）。
                let (after, qf) =
                    parse_query_filter(&format!("__lokii_kw__ {}", non_directive_tokens.join(" ")));
                let stripped = after
                    .split_whitespace()
                    .filter(|&w| w != "__lokii_kw__")
                    .collect::<Vec<_>>()
                    .join(" ");
                (stripped, qf)
            };
            (pat, re, sym, remaining, qf)
        } else if force_content {
            // 全内容模式下，先复用 `parse_query_filter` 剥离 `type:`/`ext:`/`size:` 与合法 `.ext`，其余路径限定语保留至文件名谓词。
            let combined = non_directive_tokens.join(" ");
            let (after_filter, query_filter) = parse_query_filter(&combined);
            let mut path_only_parts = Vec::new();
            let mut body_parts = Vec::new();
            for part in tokenize_preserving_quotes(&after_filter) {
                if part.starts_with("in:") || (part.contains('/') && part.ends_with('/')) {
                    path_only_parts.push(part);
                } else {
                    body_parts.push(part);
                }
            }
            let pat = body_parts.join(" ");
            if pat.is_empty() {
                return None;
            }
            (pat, force_regex, force_symbol, path_only_parts.join(" "), query_filter)
        } else {
            return None;
        };

    let has_upper = pattern.chars().any(|c| c.is_uppercase());
    let is_case_sensitive = match case_sensitivity {
        CaseSensitivity::Sensitive => true,
        CaseSensitivity::Insensitive => false,
        CaseSensitivity::Auto => has_upper,
    };

    let path_query = PathQuery::parse_with_case(&remaining_path_query, false);
    let raw_terms: Vec<String> = if path_query.is_none() {
        remaining_path_query
            .split_whitespace()
            .map(ToString::to_string)
            .collect()
    } else {
        Vec::new()
    };

    let mode = if is_regex {
        ContentQueryMode::Regex {
            pattern,
            case_insensitive: !is_case_sensitive,
        }
    } else {
        ContentQueryMode::Literal {
            pattern,
            case_insensitive: !is_case_sensitive,
            symbol_only,
        }
    };

    Some(ParsedContentQuery {
        mode,
        path_filter: ContentPathPredicate {
            query_filter: query_filter.clone(),
            path_query,
            raw_terms,
        },
        query_filter,
        remaining_path_query,
        is_case_sensitive,
    })
}

fn strip_prefix_ci<'a>(orig: &'a str, lower: &str, prefix: &str) -> Option<&'a str> {
    if lower.starts_with(prefix) && orig.is_char_boundary(prefix.len()) {
        Some(&orig[prefix.len()..])
    } else {
        None
    }
}

fn unquote(s: &str) -> &str {
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn tokenize_preserving_quotes(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;

    for ch in input.chars() {
        match quote {
            Some(q) if ch == q => {
                cur.push(ch);
                quote = None;
            }
            Some(_) => cur.push(ch),
            None if ch.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                cur.push(ch);
            }
            None => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_check_ignores_normal_filename_queries() {
        assert!(!has_content_directive("main.rs"));
        assert!(!has_content_directive("in:src ext:rs hybrid"));
        assert!(has_content_directive("content:hello"));
        assert!(has_content_directive("sym:ContentIndex ext:rs"));
        assert!(has_content_directive("cregex:fn\\s+search"));
    }

    #[test]
    fn parses_quoted_content_sym_and_orthogonal_filters() {
        let parsed = parse_content_query(
            r#"content:"hybrid search" ext:rs in:lokii-core size:>10"#,
            CaseSensitivity::Auto,
            false,
            false,
            false,
        )
        .unwrap();

        assert_eq!(
            parsed.mode,
            ContentQueryMode::Literal {
                pattern: "hybrid search".to_string(),
                case_insensitive: true,
                symbol_only: false,
            }
        );
        assert_eq!(parsed.query_filter.custom_exts, vec!["rs".to_string()]);
        assert_eq!(parsed.query_filter.size_filter.min_bytes, Some(10));
        assert!(parsed
            .path_filter
            .matches_path("/Users/me/lokii-core/src/main.rs"));
        assert!(!parsed
            .path_filter
            .matches_path("/Users/me/other/src/main.rs"));
        assert!(!parsed
            .path_filter
            .matches_path("/Users/me/lokii-core/src/main.ts"));
    }

    #[test]
    fn directive_with_standalone_dot_extension_filters_extension_uniformly() {
        let parsed =
            parse_content_query("content:hello .rs", CaseSensitivity::Auto, false, false, false)
                .unwrap();
        assert_eq!(parsed.query_filter.custom_exts, vec!["rs".to_string()]);
        assert!(parsed.path_filter.matches_path("/tmp/src/a.rs"));
        assert!(!parsed.path_filter.matches_path("/tmp/src/a.rs.bak"));

        let dotfile_parsed = parse_content_query(
            "content:target .gitignore",
            CaseSensitivity::Auto,
            false,
            false,
            false,
        )
        .unwrap();
        assert!(dotfile_parsed.query_filter.custom_exts.is_empty());
        assert!(dotfile_parsed.path_filter.matches_path("/repo/.gitignore"));
    }
}
