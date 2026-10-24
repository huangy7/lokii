use regex::RegexBuilder;

use super::{AdvancedSearchResult, SearchMode};
use crate::indexer::IndexStore;
use crate::string_pool::NameSignature;

/// 从正则中提取“所有命中都必然包含”的 ASCII 字面量片段，生成签名超集掩码。
///
/// 这里刻意只处理可证明安全的子集：普通字母/数字/下划线/中划线、转义标点，
/// 以及 `.`、`^`、`$` 这类不产生字符的锚点。分组、字符类、花括号量词、分支
/// 一律放弃预筛——它们的字面量并不一定是命中名字的连续子串，强行要求会静默漏结果。
/// 量词会撤销前一个字符：`ab?` 的 `b`、`foo*` 的最后一个 `o` 都不是必然出现的，
/// 连同相邻 bigram 一起丢弃，宁可少剪枝也不能漏召回。
fn required_signature_for_regex(pattern: &str) -> NameSignature {
    let bytes = pattern.as_bytes();
    let mut sig = NameSignature::default();
    let mut run = String::new();

    fn flush(run: &mut String, sig: &mut NameSignature) {
        if !run.is_empty() {
            *sig = sig.union(NameSignature::from_substring_query(run));
            run.clear();
        }
    }

    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            // 分组、字符类、花括号量词、分支：无法廉价证明“必然子串”。
            b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'|' => {
                return NameSignature::default();
            }
            // 转义：只有“转义标点”仍是固定字面量（\.、\+ 等）；
            // \d、\w、\xNN、\p{..} 这类字符类/编码转义必须整体放弃预筛。
            b'\\' => {
                let Some(&next) = bytes.get(i + 1) else {
                    return NameSignature::default();
                };
                if next.is_ascii_alphanumeric() || !next.is_ascii() {
                    return NameSignature::default();
                }
                flush(&mut run, &mut sig);
                let literal = (next as char).to_string();
                sig = sig.union(NameSignature::from_substring_query(&literal));
                i += 1;
            }
            // 量词：前一个字符不是必然出现（* / ?），或与前一字符的相邻关系不成立（+）。
            b'*' | b'+' | b'?' => {
                run.pop();
                flush(&mut run, &mut sig);
            }
            // 零宽锚点与任意字符：只结束当前字面量，不产生必需位。
            b'.' | b'^' | b'$' => flush(&mut run, &mut sig),
            _ if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' => {
                run.push(b.to_ascii_lowercase() as char);
            }
            _ => flush(&mut run, &mut sig),
        }
        i += 1;
    }
    flush(&mut run, &mut sig);
    sig
}

/// Regex search using the `regex` crate.
///
/// Queries are expected to start with "regex:" prefix (stripped before compilation).
/// Case-insensitive matching per R1.3. Invalid regex returns an error string
/// instead of panicking (per D-07). A size_limit caps pathological patterns.
pub fn search(
    store: &IndexStore,
    query: &str,
    max_results: usize,
) -> (Vec<AdvancedSearchResult>, Option<String>) {
    let pattern = query.trim().strip_prefix("regex:").unwrap_or(query.trim());
    if pattern.is_empty() {
        return (Vec::new(), None);
    }
    let regex = match RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(10_000_000)
        .build()
    {
        Ok(regex) => regex,
        Err(error) => return (Vec::new(), Some(format!("Invalid regex: {error}"))),
    };
    let req = required_signature_for_regex(pattern);
    let read = store.read();
    let mut top = super::top_k::TopK::new(max_results);
    let sorted = read.sorted_names();
    for &name_id in &sorted.ids {
        if let Some(sig) = read.name_signature(name_id) {
            if !sig.contains(req) {
                continue;
            }
        }
        let Some((name, _)) = read.name_entry(name_id) else {
            continue;
        };
        if regex.is_match(name) {
            for &ordinal in read.entries_for_name(name_id).iter().take(max_results) {
                top.offer(ordinal as usize, 1, name);
                if top.is_full() {
                    break;
                }
            }
            if top.is_full() {
                break;
            }
        }
    }
    let results = top
        .finish()
        .into_iter()
    .map(|candidate| {
        let view = read
            .view(crate::compact_index::EntryId(candidate.ordinal))
            .unwrap();
        let matched = regex.find(view.name).unwrap();
        AdvancedSearchResult {
            entry: view.materialize(),
            score: 1.0,
            match_positions: vec![(matched.start(), matched.end())],
            parent_match_positions: Vec::new(),
            search_mode: SearchMode::Regex,
        }
    })
    .collect();
    (results, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::{FileEntry, IndexStore};

    fn make_entry(name: &str, path: &str) -> FileEntry {
        let extension = std::path::Path::new(name)
            .extension()
            .map(|ext| ext.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        FileEntry {
            name: name.to_string(),
            path: path.to_string(),
            is_dir: false,
            size: 1024,
            modified: 1700000000,
            created_at: 0,
            extension,
        }
    }

    fn test_store() -> IndexStore {
        let store = IndexStore::new();
        store.replace(vec![
            make_entry("main.rs", "/main.rs"),
            make_entry("my_main.rs", "/my_main.rs"),
            make_entry("main.py", "/main.py"),
            make_entry("readme.md", "/readme.md"),
        ]);
        store
    }

    #[test]
    fn test_regex_caret_main() {
        let store = test_store();
        let (results, error) = search(&store, "regex:^main", 500);
        assert!(error.is_none());
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].entry.name, "main.py");
        assert_eq!(results[1].entry.name, "main.rs");
    }

    #[test]
    fn test_regex_rs_extension() {
        let store = test_store();
        let (results, error) = search(&store, r"regex:\.rs$", 500);
        assert!(error.is_none());
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].entry.name, "main.rs");
        assert_eq!(results[1].entry.name, "my_main.rs");
    }

    #[test]
    fn test_regex_case_insensitive() {
        let store = test_store();
        let (results, error) = search(&store, "regex:MAIN", 500);
        assert!(error.is_none());
        // All entries with "main" in name
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_invalid_regex_returns_error() {
        let store = test_store();
        let (results, error) = search(&store, "regex:[invalid", 500);
        assert!(results.is_empty());
        assert!(error.is_some());
        assert!(error.unwrap().contains("Invalid regex:"));
    }

    #[test]
    fn test_match_positions_contains_regex_range() {
        let store = test_store();
        let (results, _) = search(&store, "regex:^main", 500);
        assert!(!results.is_empty());
        // "main.py" -> match at (0, 4) for "main"
        let main_py = results.iter().find(|r| r.entry.name == "main.py").unwrap();
        assert_eq!(main_py.match_positions, vec![(0, 4)]);
    }

    #[test]
    fn test_results_sorted_by_name() {
        let store = test_store();
        let (results, _) = search(&store, "regex:main", 500);
        let names: Vec<&str> = results.iter().map(|r| r.entry.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn test_empty_pattern_after_prefix() {
        let store = test_store();
        let (results, error) = search(&store, "regex:", 500);
        assert!(results.is_empty());
        assert!(error.is_none());
    }

    #[test]
    fn test_search_mode_is_regex() {
        let store = test_store();
        let (results, _) = search(&store, "regex:main", 500);
        for r in &results {
            assert_eq!(r.search_mode, SearchMode::Regex);
        }
    }

    #[test]
    fn regex_prefilter_keeps_quantified_grouped_and_escaped_literals() {
        let store = IndexStore::new();
        store.replace(
            ["ac", "aa", "fo", "bar", "A.txt"]
                .into_iter()
                .map(|name| make_entry(name, &format!("/{name}")))
                .collect(),
        );

        for (pattern, expected) in [
            ("regex:ab{0,1}c", "ac"),
            ("regex:(fo)?bar", "bar"),
            ("regex:\\x41", "A.txt"),
            ("regex:a{2}", "aa"),
            ("regex:foo*", "fo"),
        ] {
            let (results, error) = search(&store, pattern, 100);
            assert!(error.is_none(), "{pattern}: {error:?}");
            assert!(
                results.iter().any(|result| result.entry.name == expected),
                "{pattern} must match {expected}, got {:?}",
                results.iter().map(|r| r.entry.name.as_str()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn regex_prefilter_never_drops_a_match() {
        use regex::RegexBuilder;

        let names = [
            "ac", "aa", "fo", "bar", "A.txt", "foo", "a", "ab", "abc", "a-b", "fao",
        ];
        let patterns = [
            "ab{0,1}c",
            "(fo)?bar",
            r"\x41",
            "a{2}",
            "foo*",
            "fo*",
            "ab?",
            "[a-c]+",
            "^foo",
            "foo$",
            "foo|bar",
            "a.b",
            r"a\+b",
            r"\d+",
            r"[\]]",
            "f.o",
            ".*",
        ];

        for pattern in patterns {
            let regex = RegexBuilder::new(pattern)
                .case_insensitive(true)
                .build()
                .unwrap();
            let required = required_signature_for_regex(pattern);
            for name in names {
                if !regex.is_match(name) {
                    continue;
                }
                let signature = NameSignature::from_name(name, None);
                assert!(
                    signature.contains(required),
                    "prefilter for {pattern:?} must keep matching name {name:?}"
                );
            }
        }
    }

    #[test]
    fn test_max_results_truncation() {
        let store = test_store();
        let (results, _) = search(&store, "regex:main", 1);
        assert_eq!(results.len(), 1);
    }
}
