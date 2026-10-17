use crate::compact_index::split_path;

/// 路径感知查询：父目录约束与终端文件名词元分离。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathQuery {
    /// 父路径匹配模式（小写），如 "src/"。
    pub path_pattern: Option<String>,
    /// 文件名关键词（小写）。
    pub name_terms: Vec<String>,
    /// 扩展名约束（小写，不含点号）。
    pub extension: Option<String>,
    /// 查询以 '/' 结尾：仅匹配目录本身及其下级。
    pub is_dir_only: bool,
    /// 查询以 '/' 开头：从根路径匹配。
    pub is_absolute_prefix: bool,
    /// `in:` 作用域前缀（已展开 '~'，以 '/' 结尾）。
    pub scope_prefix: Option<String>,
}

impl PathQuery {
    /// 查询是否含路径语法（'/' 或 `in:`）。
    pub(crate) fn is_path_query(query: &str) -> bool {
        let trimmed = query.trim();
        trimmed.contains('/') || trimmed.starts_with("in:") || trimmed.contains(" in:")
    }

    /// 解析查询；无路径语法时返回 None。
    pub fn parse(query: &str) -> Option<Self> {
        let trimmed = query.trim();
        if trimmed.is_empty() || !Self::is_path_query(trimmed) {
            return None;
        }

        let mut scope_prefix = None;
        let mut path_pattern = None;
        let mut name_terms = Vec::new();
        let mut extension = None;
        let mut is_dir_only = false;
        let mut is_absolute_prefix = false;

        for word in trimmed.split_whitespace() {
            // 1. 作用域过滤器: in:<path>
            if let Some(scope_raw) = word.strip_prefix("in:") {
                let mut scope = collapse_consecutive_slashes(&expand_tilde(scope_raw));
                if !scope.ends_with('/') {
                    scope.push('/');
                }
                scope_prefix = Some(scope.to_lowercase());
                continue;
            }

            // 2. 独立扩展名: .rs, .toml
            if word.starts_with('.') && !word.contains('/') && word.len() > 1 {
                extension = Some(word[1..].to_lowercase());
                continue;
            }

            // 3. 含 '/' 的词元
            if word.contains('/') {
                let expanded = expand_tilde(word);
                let collapsed = collapse_consecutive_slashes(&expanded);

                if collapsed.starts_with('/') {
                    is_absolute_prefix = true;
                }

                if collapsed.ends_with('/') {
                    // 以 '/' 结尾
                    is_dir_only = true;
                    path_pattern = Some(collapsed.to_lowercase());
                } else {
                    // 中间含 '/'：拆成目录与文件名
                    let (dir_part, name_part) = split_path(&collapsed);
                    if !dir_part.is_empty() {
                        path_pattern = Some(dir_part.to_lowercase());
                        if !name_part.is_empty() {
                            name_terms.push(name_part.to_lowercase());
                        }
                    }
                }
                continue;
            }

            // 4. 普通文件名词元
            name_terms.push(word.to_lowercase());
        }

        Some(Self {
            path_pattern,
            name_terms,
            extension,
            is_dir_only,
            is_absolute_prefix,
            scope_prefix,
        })
    }

    /// 查询中是否存在有效的父路径匹配约束。
    pub fn has_path(&self) -> bool {
        self.path_pattern.is_some() || self.scope_prefix.is_some()
    }
}

/// 规约连续斜杠（如 "///foo///bar//" -> "/foo/bar/"）。
pub(crate) fn collapse_consecutive_slashes(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    let mut prev_slash = false;
    for ch in path.chars() {
        if ch == '/' {
            if !prev_slash {
                result.push(ch);
                prev_slash = true;
            }
        } else {
            result.push(ch);
            prev_slash = false;
        }
    }
    result
}

/// 展开路径开头的 '~' 为用户家目录。
pub(crate) fn expand_tilde(path: &str) -> String {
    if path == "~" {
        dirs::home_dir()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|| "~".to_string())
    } else if let Some(stripped) = path.strip_prefix("~/") {
        match dirs::home_dir() {
            Some(home) => {
                let home_str = home.to_string_lossy();
                let trimmed = home_str.trim_end_matches('/');
                format!("{trimmed}/{stripped}")
            }
            None => path.to_string(),
        }
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_path_query_detection() {
        assert!(!PathQuery::is_path_query("main.rs"));
        assert!(!PathQuery::is_path_query("cargo build"));
        assert!(PathQuery::is_path_query("src/main.rs"));
        assert!(PathQuery::is_path_query("/Users/huangy"));
        assert!(PathQuery::is_path_query("Desktop/"));
        assert!(PathQuery::is_path_query("in:~/Developer app"));
    }

    #[test]
    fn test_parse_simple_path() {
        let q = PathQuery::parse("src/main.rs").expect("should parse");
        assert_eq!(q.path_pattern.as_deref(), Some("src/"));
        assert_eq!(q.name_terms, vec!["main.rs"]);
        assert!(q.has_path());
        assert!(!q.is_dir_only);
        assert!(!q.is_absolute_prefix);
        assert!(q.scope_prefix.is_none());
    }

    #[test]
    fn test_parse_directory_trailing_slash() {
        let q = PathQuery::parse("Desktop/").expect("should parse");
        assert_eq!(q.path_pattern.as_deref(), Some("desktop/"));
        assert!(q.name_terms.is_empty());
        assert!(q.has_path());
        assert!(q.is_dir_only);
        assert!(!q.is_absolute_prefix);
    }

    #[test]
    fn test_parse_absolute_path() {
        let q = PathQuery::parse("/usr/local/bin").expect("should parse");
        assert_eq!(q.path_pattern.as_deref(), Some("/usr/local/"));
        assert_eq!(q.name_terms, vec!["bin"]);
        assert!(q.has_path());
        assert!(q.is_absolute_prefix);
        assert!(!q.is_dir_only);
    }

    #[test]
    fn test_parse_multiple_slashes_collapsed() {
        let q = PathQuery::parse("///var///log///syslog").expect("should parse");
        assert_eq!(q.path_pattern.as_deref(), Some("/var/log/"));
        assert_eq!(q.name_terms, vec!["syslog"]);
        assert!(q.has_path());
        assert!(q.is_absolute_prefix);
    }

    #[test]
    fn test_parse_multi_word_with_extension() {
        let q = PathQuery::parse("build release/app .log").expect("should parse");
        assert_eq!(q.path_pattern.as_deref(), Some("release/"));
        assert_eq!(q.name_terms, vec!["build", "app"]);
        assert_eq!(q.extension.as_deref(), Some("log"));
        assert!(q.has_path());
    }

    #[test]
    fn test_parse_scope_filter() {
        let q = PathQuery::parse("in:/tmp/logs/ server.log").expect("should parse");
        assert_eq!(q.scope_prefix.as_deref(), Some("/tmp/logs/"));
        assert_eq!(q.name_terms, vec!["server.log"]);
        assert!(q.path_pattern.is_none());
        assert!(q.has_path());
    }

    #[test]
    fn test_parse_non_path_returns_none() {
        assert!(PathQuery::parse("main.rs").is_none());
        assert!(PathQuery::parse("hello world").is_none());
        assert!(PathQuery::parse("").is_none());
    }

    #[test]
    fn test_collapse_consecutive_slashes() {
        assert_eq!(collapse_consecutive_slashes("a///b//c/"), "a/b/c/");
        assert_eq!(collapse_consecutive_slashes("////"), "/");
        assert_eq!(collapse_consecutive_slashes("plain"), "plain");
    }
}
