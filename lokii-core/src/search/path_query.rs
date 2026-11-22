use crate::compact_index::split_path;
use super::filter::QueryToken;

/// 中英标准系统目录双向别名表（全量使用 fold_str 后的简体小写形式）。
const CN_EN_DIR_ALIASES: &[(&str, &str)] = &[
    ("桌面", "desktop"),
    ("下载", "downloads"),
    ("文稿", "documents"),
    ("图片", "pictures"),
    ("音乐", "music"),
    ("影片", "movies"),
    ("应用程序", "applications"),
    ("废纸篓", "trash"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathCandidate {
    pub pattern: String,
    /// 预拼接前导斜杠的模式（`"/{pattern}"`），供段边界匹配热路径直接借用，免除每次 `format!` 堆分配。
    pub slashed_pattern: String,
    pub is_absolute: bool,
    pub require_segment_boundary: bool,
}

impl PathCandidate {
    pub(crate) fn new(pattern: String, is_absolute: bool, require_segment_boundary: bool) -> Self {
        let slashed_pattern = if is_absolute {
            String::new()
        } else {
            format!("/{pattern}")
        };
        Self {
            pattern,
            slashed_pattern,
            is_absolute,
            require_segment_boundary,
        }
    }

    pub(crate) fn set_pattern(&mut self, pattern: String) {
        self.slashed_pattern = if self.is_absolute {
            String::new()
        } else {
            format!("/{pattern}")
        };
        self.pattern = pattern;
    }
}

/// 路径感知查询：父目录约束与终端文件名词元分离。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathQuery {
    /// 父路径匹配模式（小写），如 "src/"。
    pub path_pattern: Option<String>,
    /// 文件名关键词（已按大小写设置折叠或规范化）。
    pub name_terms: Vec<String>,
    /// 保留原始大小写（经 NFC 规范化）的文件名关键词，供精确大小写加分判定使用。
    pub(crate) raw_name_terms: Vec<String>,
    /// 扩展名约束（小写，不含点号）。
    pub extension: Option<String>,
    /// 查询以 '/' 结尾：仅匹配目录本身及其下级。
    pub is_dir_only: bool,
    /// 查询以 '/' 开头：从根路径匹配。
    pub is_absolute_prefix: bool,
    /// `in:` 作用域前缀（已展开 '~'，以 '/' 结尾）。
    pub scope_prefix: Option<String>,
    /// 保留原始大小写的 NFC 目录路径（经 `~` 展开与斜杠合并），供未索引绝对目录 `read_dir` 兜底使用。
    pub(crate) raw_dir_pattern: Option<String>,
    /// 展开别名后的父路径候选列表。
    pub(crate) path_candidates: Vec<PathCandidate>,
    /// 展开别名后的 `in:` 作用域候选列表。
    pub(crate) scope_candidates: Vec<PathCandidate>,
}

impl PathQuery {
    /// 查询是否含路径语法（经粘贴路径清洗后含 '/' 或 `in:`）。
    #[cfg(test)]
    pub(crate) fn is_path_query(query: &str) -> bool {
        let tokens = super::filter::tokenize_query(query);
        Self::is_path_query_tokens(&tokens)
    }

    /// 判断结构化正向词元序列是否触发路径感知查询（带引号词元视为纯字面文件名，不触发路径模式）。
    pub(crate) fn is_path_query_tokens(tokens: &[QueryToken]) -> bool {
        tokens
            .iter()
            .any(|t| !t.negated && !t.quoted && (t.text.contains('/') || t.text.starts_with("in:")))
    }

    /// 解析查询；无路径语法时返回 None。
    pub fn parse(query: &str) -> Option<Self> {
        Self::parse_with_case(query, false)
    }

    /// 接收大小写敏感配置解析路径感知查询。
    pub fn parse_with_case(query: &str, case_sensitive: bool) -> Option<Self> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return None;
        }
        let tokens = super::filter::tokenize_query(trimmed);
        Self::parse_from_tokens(&tokens, case_sensitive)
    }

    /// 基于结构化正向词元解析路径感知查询：带引号词元不参与 `in:` / `.ext` / `/` 路径拆分。
    pub(crate) fn parse_from_tokens(tokens: &[QueryToken], case_sensitive: bool) -> Option<Self> {
        if tokens.is_empty() || !Self::is_path_query_tokens(tokens) {
            return None;
        }

        let mut scope_prefix = None;
        let mut path_pattern = None;
        let mut raw_dir_pattern = None;
        let mut path_candidates: Vec<PathCandidate> = Vec::new();
        let mut scope_candidates: Vec<PathCandidate> = Vec::new();
        let mut name_terms = Vec::new();
        let mut raw_name_terms = Vec::new();
        let mut extension = None;
        let mut is_dir_only = false;
        let mut is_absolute_prefix = false;

        for token in tokens {
            if token.negated {
                continue;
            }
            let word = token.text.as_str();
            if word.is_empty() {
                continue;
            }

            if !token.quoted {
                // 1. 作用域过滤器: in:<path>
                if let Some(scope_raw) = word.strip_prefix("in:") {
                    let mut cands = build_dir_candidates(scope_raw, true);
                    if let Some(first) = cands.first() {
                        scope_prefix = Some(first.pattern.clone());
                    }
                    push_unique_candidates(&mut scope_candidates, &mut cands);
                    continue;
                }

                // 2. 独立扩展名: .rs, .toml（排除隐藏点文件，如 .env、.gitignore）
                if word.starts_with('.') && !word.contains('/') && word.len() > 1 {
                    if crate::search::filter::is_dotfile_query(word) {
                        raw_name_terms.push(crate::cjk::normalize_nfc_cow(word).into_owned());
                        name_terms.push(if case_sensitive {
                            crate::cjk::normalize_and_simplify_cow(word).into_owned()
                        } else {
                            crate::cjk::fold_str(word)
                        });
                    } else {
                        extension = Some(crate::cjk::fold_str(&word[1..]));
                    }
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
                        // 以 '/' 结尾（父目录约束始终小写并完成 NFC 归一化；原始大小写另存以供兜底读盘）
                        is_dir_only = true;
                        raw_dir_pattern =
                            Some(crate::cjk::normalize_nfc_cow(&collapsed).into_owned());
                        let mut cands = build_dir_candidates(word, false);
                        if let Some(first) = cands.first() {
                            path_pattern = Some(first.pattern.clone());
                        }
                        push_unique_candidates(&mut path_candidates, &mut cands);
                    } else {
                        // 中间含 '/'：拆成目录与文件名（父目录约束始终小写与 NFC 归一化，仅文件名按需区分大小写）
                        let (word_dir, _) = split_path(word);
                        let (dir_part, name_part) = split_path(&collapsed);
                        if !dir_part.is_empty() {
                            let mut cands = build_dir_candidates(word_dir, false);
                            if let Some(first) = cands.first() {
                                path_pattern = Some(first.pattern.clone());
                            }
                            push_unique_candidates(&mut path_candidates, &mut cands);
                            if !name_part.is_empty() {
                                raw_name_terms
                                    .push(crate::cjk::normalize_nfc_cow(name_part).into_owned());
                                name_terms.push(if case_sensitive {
                                    crate::cjk::normalize_and_simplify_cow(name_part).into_owned()
                                } else {
                                    crate::cjk::fold_str(name_part)
                                });
                            }
                        }
                    }
                    continue;
                }
            }

            // 4. 普通文件名词元（或被双引号保护的字面词元）
            raw_name_terms.push(crate::cjk::normalize_nfc_cow(word).into_owned());
            name_terms.push(if case_sensitive {
                crate::cjk::normalize_and_simplify_cow(word).into_owned()
            } else {
                crate::cjk::fold_str(word)
            });
        }

        Some(Self {
            path_pattern,
            name_terms,
            raw_name_terms,
            extension,
            is_dir_only,
            is_absolute_prefix,
            scope_prefix,
            raw_dir_pattern,
            path_candidates,
            scope_candidates,
        })
    }

    /// 查询中是否存在有效的父路径匹配约束。
    pub fn has_path(&self) -> bool {
        self.path_pattern.is_some()
            || !self.path_candidates.is_empty()
            || self.scope_prefix.is_some()
            || !self.scope_candidates.is_empty()
    }

    /// 若分片存在挂载路径漂移（`configured_path != active_path`），
    /// 将 `scope_prefix` 或绝对路径 `path_pattern` 中的历史配置前缀重写为当前生效前缀。
    ///
    /// 当配置路径与生效路径位于卷内子目录且共享相同尾部路径段（如 `/Volumes/Share/Projects` 与
    /// `/Volumes/Share-1/Projects`）时，剥离双方相同的尾部路径段以推导出卷挂载根别名
    /// （双方均至少保留 2 个非空路径段，如 `/Volumes/<Name>`），从而同时兼容卷根级 `in:` 作用域与
    /// 子路径绝对查询；并严格维持前缀边界安全（如 `/Volumes/Share` 绝不误匹配 `/Volumes/ShareExtra`）
    /// 与大小写不敏感匹配。
    pub fn rewrite_drift_alias(&self, configured_path: &str, active_path: &str) -> Self {
        let cfg_clean = collapse_consecutive_slashes(configured_path.trim_end_matches('/'));
        let act_clean = collapse_consecutive_slashes(active_path.trim_end_matches('/'));

        if cfg_clean.is_empty() || act_clean.is_empty() || eq_ignore_case(&cfg_clean, &act_clean) {
            return self.clone();
        }

        let mut cfg_segments: Vec<&str> = cfg_clean.split('/').filter(|s| !s.is_empty()).collect();
        let mut act_segments: Vec<&str> = act_clean.split('/').filter(|s| !s.is_empty()).collect();
        if cfg_segments.is_empty() || act_segments.is_empty() {
            return self.clone();
        }

        while cfg_segments.len() > 2 && act_segments.len() > 2 {
            let cfg_tail = cfg_segments.last().copied().unwrap();
            let act_tail = act_segments.last().copied().unwrap();
            if !eq_ignore_case(cfg_tail, act_tail) {
                break;
            }
            cfg_segments.pop();
            act_segments.pop();
        }

        let cfg_prefix = format_segments_prefix(&cfg_clean, &cfg_segments);
        let act_prefix = format_segments_prefix(&act_clean, &act_segments);
        if cfg_prefix == act_prefix {
            return self.clone();
        }

        let mut rewritten = self.clone();

        for candidate in rewritten
            .scope_candidates
            .iter_mut()
            .chain(rewritten.path_candidates.iter_mut())
            .filter(|candidate| candidate.is_absolute)
        {
            if let Some(updated) = candidate.pattern.strip_prefix(&cfg_prefix) {
                let new_pat = format!("{act_prefix}{updated}");
                candidate.set_pattern(new_pat);
            }
        }
        rewritten.scope_prefix = rewritten
            .scope_candidates
            .first()
            .map(|c| c.pattern.clone());
        rewritten.path_pattern = rewritten.path_candidates.first().map(|c| c.pattern.clone());
        if let Some(raw) = rewritten.raw_dir_pattern.as_mut() {
            let folded = crate::cjk::fold_cow(raw);
            if folded.starts_with(&cfg_prefix) {
                if let Some((_, end)) =
                    super::map_norm_span_to_orig(raw, &folded, 0, cfg_prefix.len())
                {
                    *raw = format!("/{}/{}", act_segments.join("/"), &raw[end..]);
                }
            }
        }

        rewritten
    }
}

/// 从结构化正向词元中提取所有未加引号的 `in:<scope>` 作用域候选（含 `~` 与中英系统目录别名展开），
/// 供正则、模糊、并集等非 `PathQuery` 引擎共享编译为父目录掩码。
pub(crate) fn build_scope_candidates_from_tokens(tokens: &[QueryToken]) -> Vec<PathCandidate> {
    let mut scope_candidates = Vec::new();
    for token in tokens {
        if token.negated || token.quoted {
            continue;
        }
        if let Some(scope_raw) = token.text.strip_prefix("in:") {
            let mut cands = build_dir_candidates(scope_raw, true);
            push_unique_candidates(&mut scope_candidates, &mut cands);
        }
    }
    scope_candidates
}

/// 针对挂载路径漂移分片重写一组独立的 `in:` 作用域候选前缀（与 `PathQuery::rewrite_drift_alias` 保持同一边界安全规则）。
pub(crate) fn rewrite_scope_candidates_drift_alias(
    candidates: &[PathCandidate],
    configured_path: &str,
    active_path: &str,
) -> Vec<PathCandidate> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let cfg_clean = collapse_consecutive_slashes(configured_path.trim_end_matches('/'));
    let act_clean = collapse_consecutive_slashes(active_path.trim_end_matches('/'));
    if cfg_clean.is_empty() || act_clean.is_empty() || eq_ignore_case(&cfg_clean, &act_clean) {
        return candidates.to_vec();
    }
    let mut cfg_segments: Vec<&str> = cfg_clean.split('/').filter(|s| !s.is_empty()).collect();
    let mut act_segments: Vec<&str> = act_clean.split('/').filter(|s| !s.is_empty()).collect();
    if cfg_segments.is_empty() || act_segments.is_empty() {
        return candidates.to_vec();
    }
    while cfg_segments.len() > 2 && act_segments.len() > 2 {
        let cfg_tail = cfg_segments.last().copied().unwrap();
        let act_tail = act_segments.last().copied().unwrap();
        if !eq_ignore_case(cfg_tail, act_tail) {
            break;
        }
        cfg_segments.pop();
        act_segments.pop();
    }
    let cfg_prefix = format_segments_prefix(&cfg_clean, &cfg_segments);
    let act_prefix = format_segments_prefix(&act_clean, &act_segments);
    if cfg_prefix == act_prefix {
        return candidates.to_vec();
    }
    let mut rewritten = candidates.to_vec();
    for candidate in rewritten.iter_mut().filter(|c| c.is_absolute) {
        if let Some(updated) = candidate.pattern.strip_prefix(&cfg_prefix) {
            let new_pat = format!("{act_prefix}{updated}");
            candidate.set_pattern(new_pat);
        }
    }
    rewritten
}

fn eq_ignore_case(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        a.eq_ignore_ascii_case(b)
    } else {
        crate::cjk::fold_cow(a) == crate::cjk::fold_cow(b)
    }
}

fn format_segments_prefix(clean: &str, segments: &[&str]) -> String {
    let joined = segments.join("/");
    if clean.starts_with('/') {
        crate::cjk::fold_str(&format!("/{joined}/"))
    } else {
        crate::cjk::fold_str(&format!("{joined}/"))
    }
}

fn lookup_dir_alias(seg_folded: &str) -> Option<&'static str> {
    if seg_folded == "应用程式" {
        return Some("applications");
    }
    for &(cn, en) in CN_EN_DIR_ALIASES {
        if seg_folded == cn {
            return Some(en);
        }
        if seg_folded == en {
            return Some(cn);
        }
    }
    None
}

fn push_unique_candidates(dst: &mut Vec<PathCandidate>, src: &mut Vec<PathCandidate>) {
    for cand in src.drain(..) {
        if !dst.iter().any(|existing| existing == &cand) {
            dst.push(cand);
        }
    }
}

/// 逐段展开标准目录别名（中文简繁体 <-> 英文标准目录名），上限 8 组防止多段组合爆炸。
fn expand_dir_segment_aliases(folded_dir: &str, is_absolute: bool) -> Vec<PathCandidate> {
    let had_leading = folded_dir.starts_with('/');
    let had_trailing = folded_dir.ends_with('/');
    let core = folded_dir.trim_matches('/');
    let mut seg_lists: Vec<Vec<String>> = vec![if core.is_empty() {
        Vec::new()
    } else {
        core.split('/').map(|s| s.to_string()).collect()
    }];

    let seg_count = seg_lists[0].len();
    for idx in 0..seg_count {
        let current_seg = seg_lists[0][idx].clone();
        if let Some(alt) = lookup_dir_alias(&current_seg) {
            let existing_len = seg_lists.len();
            for j in 0..existing_len {
                if seg_lists.len() >= 8 {
                    break;
                }
                let mut cloned = seg_lists[j].clone();
                cloned[idx] = alt.to_string();
                seg_lists.push(cloned);
            }
        }
    }

    let mut out = Vec::with_capacity(seg_lists.len());
    for (variant_idx, list) in seg_lists.into_iter().enumerate() {
        let mut pat = String::new();
        if had_leading {
            pat.push('/');
        }
        pat.push_str(&list.join("/"));
        if had_trailing && !pat.ends_with('/') {
            pat.push('/');
        }
        // 经别名换写生成的候选强制按完整目录段边界匹配，避免 "downloads/" 误命中 "mydownloads/"
        let require_segment_boundary = variant_idx > 0;
        if !out
            .iter()
            .any(|c: &PathCandidate| c.pattern == pat && c.is_absolute == is_absolute)
        {
            out.push(PathCandidate::new(pat, is_absolute, require_segment_boundary));
        }
    }
    out
}

/// 构建路径或 `in:` 目录候选列表：
/// 1) 识别路径首部 `个人 <-> ~`（同时产出 `$HOME` 绝对候选与相对 `个人/` 候选）；
/// 2) 为每个基础候选展开标准系统目录中英双向别名。
fn build_dir_candidates(raw_dir: &str, ensure_trailing_slash: bool) -> Vec<PathCandidate> {
    let normalized_raw = collapse_consecutive_slashes(raw_dir);
    let ensure_slash = |mut s: String| {
        if ensure_trailing_slash && !s.ends_with('/') {
            s.push('/');
        }
        s
    };

    let folded_raw = crate::cjk::fold_str(&normalized_raw);
    let mut base_entries: Vec<(String, bool)> = Vec::new();

    if folded_raw == "个人" || folded_raw.starts_with("个人/") {
        let suffix = folded_raw.strip_prefix("个人").unwrap_or("");
        let tilde_input = format!("~{suffix}");
        let home_expanded = collapse_consecutive_slashes(&expand_tilde(&tilde_input));
        let home_is_abs = home_expanded.starts_with('/');
        let home_folded = crate::cjk::fold_str(&ensure_slash(home_expanded));
        base_entries.push((home_folded, home_is_abs));
        base_entries.push((ensure_slash(folded_raw), false));
    } else {
        let expanded = collapse_consecutive_slashes(&expand_tilde(&normalized_raw));
        let dir_with_slash = ensure_slash(expanded);
        let is_abs = dir_with_slash.starts_with('/');
        let folded = crate::cjk::fold_str(&dir_with_slash);
        base_entries.push((folded, is_abs));
    }

    let mut result = Vec::new();
    for (dir_folded, is_abs) in base_entries {
        let mut expanded = expand_dir_segment_aliases(&dir_folded, is_abs);
        push_unique_candidates(&mut result, &mut expanded);
    }

    result
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

    #[test]
    fn test_parse_with_case_dir_and_name() {
        let q = PathQuery::parse_with_case("Src/AppDelegate.swift", true).expect("should parse");
        assert_eq!(q.path_pattern.as_deref(), Some("src/"));
        assert_eq!(q.name_terms, vec!["AppDelegate.swift"]);

        let q2 = PathQuery::parse_with_case("Src/AppDelegate.swift", false).expect("should parse");
        assert_eq!(q2.path_pattern.as_deref(), Some("src/"));
        assert_eq!(q2.name_terms, vec!["appdelegate.swift"]);
    }

    #[test]
    fn test_parse_pure_cn_filename_is_not_path_query() {
        // 纯中文标准目录名不带 '/' 时不应误判为路径查询
        assert!(PathQuery::parse("桌面").is_none());
    }

    #[test]
    fn test_rewrite_drift_alias_subpath_mount_root_and_boundaries() {
        let cfg = "/Volumes/Share/Projects";
        let act = "/Volumes/Share-1/Projects";

        // 1. 卷根级 in: 作用域改写
        let q_root = PathQuery::parse("in:/Volumes/Share doc.txt").unwrap();
        let rw_root = q_root.rewrite_drift_alias(cfg, act);
        assert_eq!(rw_root.scope_prefix.as_deref(), Some("/volumes/share-1/"));

        // 2. 子路径绝对查询改写
        let q_sub = PathQuery::parse("/Volumes/Share/Projects/doc.txt").unwrap();
        let rw_sub = q_sub.rewrite_drift_alias(cfg, act);
        assert_eq!(rw_sub.path_pattern.as_deref(), Some("/volumes/share-1/projects/"));

        // 3. 兄弟前缀边界守卫：/Volumes/ShareExtra 不得被误改写
        let q_sibling = PathQuery::parse("in:/Volumes/ShareExtra doc.txt").unwrap();
        let rw_sibling = q_sibling.rewrite_drift_alias(cfg, act);
        assert_eq!(
            rw_sibling.scope_prefix.as_deref(),
            Some("/volumes/shareextra/")
        );

        let q_sibling_abs = PathQuery::parse("/Volumes/ShareExtra/Projects/doc.txt").unwrap();
        let rw_sibling_abs = q_sibling_abs.rewrite_drift_alias(cfg, act);
        assert_eq!(
            rw_sibling_abs.path_pattern.as_deref(),
            Some("/volumes/shareextra/projects/")
        );

        // 4. 至少保留 2 级路径段：/Volumes/A 与 /Other/A 不得剥离出只剩 1 级段的 /Volumes
        let q_two_seg = PathQuery::parse("in:/Volumes/ doc.txt").unwrap();
        let rw_two_seg = q_two_seg.rewrite_drift_alias("/Volumes/A", "/Other/A");
        assert_eq!(rw_two_seg.scope_prefix.as_deref(), Some("/volumes/"));
    }

    #[test]
    fn test_rewrite_drift_alias_non_ascii_and_unicode_case() {
        let q = PathQuery::parse("in:/Volumes/中文卷 报告").unwrap();
        let rw = q.rewrite_drift_alias("/Volumes/中文卷", "/Volumes/中文卷-1");
        assert_eq!(rw.scope_prefix.as_deref(), Some("/volumes/中文卷-1/"));

        // 带大小写扩展字符（如 'İ'）的目录名安全改写，不发生字节越界切片
        let q_tr = PathQuery::parse("/Volumes/İSTANBUL/sub/doc.txt").unwrap();
        let rw_tr = q_tr.rewrite_drift_alias("/Volumes/İSTANBUL", "/Volumes/İSTANBUL-1");
        assert_eq!(
            rw_tr.path_pattern.as_deref(),
            Some("/volumes/i\u{307}stanbul-1/sub/")
        );
    }
}
