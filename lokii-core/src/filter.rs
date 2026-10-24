use crate::config::AppConfig;
use crate::path_utils::path_starts_with_ci;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// 规范化前缀：构造期预计算路径层级深度，并统一为小写、带尾随 '/' 的表示，
/// 使热路径的前缀比对只需字节比较，不必为每个候选重新分配字符串。
#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedPrefix {
    raw: PathBuf,
    prefix_str: String,
    depth: usize,
}

impl NormalizedPrefix {
    fn new(path: PathBuf) -> Self {
        let depth = path.components().count();
        let mut s = path.to_string_lossy().to_string();
        #[cfg(target_os = "macos")]
        {
            s = s.to_lowercase();
        }
        if !s.ends_with('/') {
            s.push('/');
        }
        Self {
            raw: path,
            prefix_str: s,
            depth,
        }
    }

    /// 目标路径是否落在此前缀目录下，或与该目录本身相等。
    #[inline(always)]
    fn matches_str(&self, path_str: &str) -> bool {
        let prefix = self.prefix_str.as_str();
        let p_len = prefix.len();
        // 前缀自带尾随 '/'，命中即已对齐组件边界；长度少一时退化为目录自身的精确匹配
        let hit = |s: &str| {
            s.starts_with(prefix)
                || (p_len > 1 && s.len() == p_len - 1 && s == &prefix[..p_len - 1])
        };

        #[cfg(target_os = "macos")]
        {
            let path_bytes = path_str.as_bytes();
            let p_bytes = prefix.as_bytes();
            let hit_ascii = |s: &[u8]| {
                (s.len() >= p_len && s[..p_len].eq_ignore_ascii_case(p_bytes))
                    || (p_len > 1
                        && s.len() == p_len - 1
                        && s.eq_ignore_ascii_case(&p_bytes[..p_len - 1]))
            };
            if hit_ascii(path_bytes) {
                return true;
            }
            // 重音字符的大小写折叠需要 Unicode 语义，仅当两侧含非 ASCII 字节时才付出分配代价
            if !path_bytes.is_ascii() || !p_bytes.is_ascii() {
                return hit(&path_str.to_lowercase());
            }
            false
        }

        #[cfg(not(target_os = "macos"))]
        {
            hit(path_str)
        }
    }
}

/// 匹配 `path` 的最深前缀层级；无任何前缀匹配时为 `None`。
fn deepest_matching_depth(prefixes: &[NormalizedPrefix], path_str: &str) -> Option<usize> {
    prefixes
        .iter()
        .filter(|p| p.matches_str(path_str))
        .map(|p| p.depth)
        .max()
}

/// Compiled filter for fast path exclusion checks.
///
/// All paths are normalized (tilde expanded, absolute) at construction time
/// so the hot path does zero allocation and no canonicalization.
pub struct CompiledFilter {
    /// Normalized include root paths from config `index_paths`.
    include_roots: Vec<NormalizedPrefix>,
    /// Normalized exclude path prefixes from config `exclude_paths`.
    excluded_prefixes: Vec<NormalizedPrefix>,
    /// Directory names to skip (component-level match).
    excluded_names: HashSet<String>,
}

impl CompiledFilter {
    /// Build a compiled filter from `AppConfig`.
    /// Path normalization (tilde expansion, absolute path) happens here.
    pub fn from_config(config: &AppConfig) -> Self {
        let include_roots = config
            .resolve_paths()
            .into_iter()
            .map(NormalizedPrefix::new)
            .collect();
        let excluded_prefixes = config
            .resolve_exclude_paths()
            .into_iter()
            .map(NormalizedPrefix::new)
            .collect();
        let excluded_names: HashSet<String> = config.index.exclude.iter().cloned().collect();

        Self {
            include_roots,
            excluded_prefixes,
            excluded_names,
        }
    }

    /// 是否有排除前缀位于 `path` 之下：用于判断该目录内部是否还藏有需要单独监听的目标。
    pub fn has_excluded_descendant(&self, path: &Path) -> bool {
        self.excluded_prefixes
            .iter()
            .any(|prefix| path_starts_with_ci(&prefix.raw, path))
    }

    /// Return whether any path component after `skip_components` matches an excluded directory name.
    /// Uses component-level matching: `/foo/node_modules/bar` matches `node_modules`,
    /// but `/foo/mynode_modules/bar` does not.
    fn has_excluded_component_after(&self, path: &Path, skip_components: usize) -> bool {
        if self.excluded_names.is_empty() {
            return false;
        }
        path.components().skip(skip_components).any(|c| {
            if let std::path::Component::Normal(name) = c {
                let name_str = name.to_string_lossy();
                self.excluded_names.contains(name_str.as_ref())
            } else {
                false
            }
        })
    }

    /// Fast path: return `true` if this path should be dropped immediately.
    ///
    /// 显式索引根与排除前缀按最长前缀比对：更深的显式根可以穿透上层排除
    /// （如在 ~/Library 下单独索引企微缓存），同深度冲突时排除优先。
    pub fn is_excluded(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();
        let in_depth = deepest_matching_depth(&self.include_roots, &path_str);

        // 配置了索引根却未命中任何根：该路径不属于索引范围
        if !self.include_roots.is_empty() && in_depth.is_none() {
            return true;
        }

        let in_depth = in_depth.unwrap_or(0);

        if let Some(ex_depth) = deepest_matching_depth(&self.excluded_prefixes, &path_str) {
            if ex_depth >= in_depth {
                return true;
            }
        }

        // 黑名单只作用于索引根之下的相对子孙，避免索引根自身叫 build/node_modules 时被误杀
        self.has_excluded_component_after(path, in_depth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, IndexConfig};
    use std::fs;
    use tempfile::TempDir;

    fn make_test_filter(index_paths: Vec<&str>, exclude: Vec<&str>, exclude_paths: Vec<&str>) -> CompiledFilter {
        let mut config = AppConfig::default();
        config.index = IndexConfig {
            index_paths: index_paths.iter().map(|s| s.to_string()).collect(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
            exclude_paths: exclude_paths.iter().map(|s| s.to_string()).collect(),
            include_hidden: true,
            max_results: 500,
        };
        CompiledFilter::from_config(&config)
    }

    #[test]
    fn test_default_config_excludes_library() {
        let config = AppConfig::default();
        let filter = CompiledFilter::from_config(&config);

        let home = dirs::home_dir().unwrap();

        // ~/Library should be excluded by prefix
        let library_cache = home.join("Library/Caches/com.apple.Safari");
        assert!(filter.is_excluded(&library_cache));

        // ~/Documents should NOT be excluded
        let docs = home.join("Documents/report.pdf");
        assert!(!filter.is_excluded(&docs));
    }

    #[test]
    fn test_default_index_is_home() {
        let config = AppConfig::default();
        // Default index root is ~
        assert_eq!(
            config.index.index_paths,
            vec!["~".to_string()],
            "Default index_paths should be ['~']"
        );
        // But ~/Library is excluded by prefix
        assert!(
            config.index.exclude_paths.contains(&"~/Library".to_string()),
            "Default exclude_paths should include ~/Library"
        );
    }

    #[test]
    fn test_path_outside_include_root_is_excluded() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_string_lossy().to_string();
        let filter = make_test_filter(vec![&root], vec![], vec![]);

        let inside = tmp.path().join("foo.txt");
        fs::write(&inside, "data").unwrap();

        assert!(!filter.is_excluded(&inside));
        assert!(filter.is_excluded(Path::new("/tmp/outside.txt")));
    }

    #[test]
    fn test_excluded_prefix_wins_within_include_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_string_lossy().to_string();
        let excluded_dir = tmp.path().join("skip_me");
        fs::create_dir(&excluded_dir).unwrap();
        let excluded_str = excluded_dir.to_string_lossy().to_string();

        let filter = make_test_filter(
            vec![&root],
            vec![],
            vec![&excluded_str],
        );

        let inside = excluded_dir.join("nested/file.txt");
        assert!(filter.is_excluded(&inside));
        assert!(!filter.is_excluded(&tmp.path().join("ok/file.txt")));
    }

    #[test]
    fn test_excluded_component_matches_exact_dir_name_only() {
        let filter = make_test_filter(
            vec!["/tmp"],
            vec!["node_modules", ".git"],
            vec![],
        );

        assert!(filter.is_excluded(Path::new("/tmp/proj/node_modules/pkg/index.js")));
        assert!(filter.is_excluded(Path::new("/tmp/proj/.git/config")));
        // Component-level: only exact directory name match, not substring
        assert!(!filter.is_excluded(Path::new("/tmp/mynode_modules/pkg")));
        assert!(!filter.is_excluded(Path::new("/tmp/proj/src/main.rs")));
    }

    #[test]
    fn test_is_excluded_full_pipeline() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_string_lossy().to_string();
        let node_modules = tmp.path().join("node_modules");
        fs::create_dir(&node_modules).unwrap();

        let cache_dir = tmp.path().join("cache_excluded");
        fs::create_dir(&cache_dir).unwrap();

        let filter = make_test_filter(
            vec![&root],
            vec!["node_modules"],
            vec![&cache_dir.to_string_lossy().to_string()],
        );

        // Outside include root → excluded
        assert!(filter.is_excluded(Path::new("/tmp/outside.txt")));

        // Under excluded prefix → excluded
        let cached_file = cache_dir.join("data.bin");
        assert!(filter.is_excluded(&cached_file));

        // Has excluded component → excluded
        assert!(filter.is_excluded(&node_modules.join("pkg/index.js")));

        // Clean path → not excluded
        let clean = tmp.path().join("src/main.rs");
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(&clean, "fn main() {}").unwrap();
        assert!(!filter.is_excluded(&clean));
    }

    #[test]
    fn test_exclude_paths_with_tilde_expansion() {
        let config = AppConfig::default();
        let filter = CompiledFilter::from_config(&config);

        let home = dirs::home_dir().unwrap();

        // ~/Library should be in excluded prefixes
        let lib = home.join("Library");
        assert!(
            filter.excluded_prefixes.iter().any(|p| p.raw == lib),
            "excluded_prefixes should contain resolved ~/Library"
        );

        // ~/.npm should be in excluded prefixes
        let npm = home.join(".npm");
        assert!(
            filter.excluded_prefixes.iter().any(|p| p.raw == npm),
            "excluded_prefixes should contain resolved ~/.npm"
        );
    }

    #[test]
    fn test_nested_include_pierces_broader_exclude() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_string_lossy().to_string();

        let library = tmp.path().join("Library");
        let wework_cache = library.join("Containers/com.tencent.WeWorkMac/Data/Documents/Files");
        fs::create_dir_all(&wework_cache).unwrap();

        let filter = make_test_filter(
            vec![&root, &wework_cache.to_string_lossy()],
            vec!["node_modules"],
            vec![&library.to_string_lossy()],
        );

        // 企微缓存目录下的文件成功穿透 ~/Library 排除
        let report = wework_cache.join("2026-10/report.pptx");
        assert!(
            !filter.is_excluded(&report),
            "深层显式索引路径应穿透父级排除前缀"
        );

        // ~/Library 下其他未显式穿透的目录依然被排除
        let safari_cache = library.join("Caches/com.apple.Safari/Cache.db");
        assert!(
            filter.is_excluded(&safari_cache),
            "未穿透的父级排除目录文件依然应被排除"
        );
    }

    #[test]
    fn test_same_depth_conflict_prefers_exclude() {
        let tmp = TempDir::new().unwrap();
        let secret = tmp.path().join("Secret");
        fs::create_dir_all(&secret).unwrap();
        let secret_str = secret.to_string_lossy().to_string();

        // 包含和排除配置完全相同的路径（同深度冲突）
        let filter = make_test_filter(
            vec![&secret_str],
            vec![],
            vec![&secret_str],
        );

        let file = secret.join("doc.txt");
        assert!(
            filter.is_excluded(&file),
            "同深度包含与排除冲突时应遵循排除优先"
        );
    }

    #[test]
    fn test_nested_exclude_re_excludes_under_pierced_include() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_string_lossy().to_string();

        let library = tmp.path().join("Library");
        let wework_cache = library.join("Containers/Files");
        let sub_ignored = wework_cache.join("Ignored");
        fs::create_dir_all(&sub_ignored).unwrap();

        let filter = make_test_filter(
            vec![&root, &wework_cache.to_string_lossy()],
            vec![],
            vec![&library.to_string_lossy(), &sub_ignored.to_string_lossy()],
        );

        // wework_cache 放行
        assert!(!filter.is_excluded(&wework_cache.join("good.txt")));
        // 更深层的 sub_ignored 再次排除
        assert!(filter.is_excluded(&sub_ignored.join("bad.txt")));
    }

    #[test]
    fn test_component_exclude_only_applies_after_include_root() {
        let tmp = TempDir::new().unwrap();
        // 显式包含的根自身路径中含有 "build" 关键字
        let build_root = tmp.path().join("my-app/build/artifacts");
        fs::create_dir_all(&build_root).unwrap();

        let filter = make_test_filter(
            vec![&build_root.to_string_lossy()],
            vec!["build", "node_modules"],
            vec![],
        );

        // 显式索引根自身的 "build" 目录名不应被误杀
        let artifact = build_root.join("bundle.js");
        assert!(
            !filter.is_excluded(&artifact),
            "显式索引根自身的黑名单组件名不应导致自身被过滤"
        );

        // 显式索引根内部子孙路径中的黑名单依然生效（方案 A）
        let node_modules_file = build_root.join("node_modules/pkg/index.js");
        assert!(
            filter.is_excluded(&node_modules_file),
            "显式索引根内部子孙的黑名单组件名应继续被过滤"
        );
    }

    #[test]
    fn test_unmatched_path_dropped_when_include_roots_configured() {
        let tmp = TempDir::new().unwrap();
        let my_dir = tmp.path().join("my_dir");
        fs::create_dir_all(&my_dir).unwrap();

        let filter = make_test_filter(
            vec![&my_dir.to_string_lossy()],
            vec![],
            vec![],
        );

        // 外部路径直接被排除
        assert!(filter.is_excluded(Path::new("/var/log/system.log")));
        assert!(filter.is_excluded(Path::new("/etc/hosts")));
    }
}
