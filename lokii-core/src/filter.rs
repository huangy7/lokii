use crate::config::AppConfig;
use crate::path_utils::path_starts_with_ci;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Compiled filter for fast path exclusion checks.
///
/// All paths are normalized (tilde expanded, absolute) at construction time
/// so the hot path does zero allocation and no canonicalization.
pub struct CompiledFilter {
    /// Normalized include root paths from config `index_paths`.
    include_roots: Vec<PathBuf>,
    /// Normalized exclude path prefixes from config `exclude_paths`.
    excluded_prefixes: Vec<PathBuf>,
    /// Directory names to skip (component-level match).
    excluded_names: HashSet<String>,
}

impl CompiledFilter {
    /// Build a compiled filter from `AppConfig`.
    /// Path normalization (tilde expansion, absolute path) happens here.
    pub fn from_config(config: &AppConfig) -> Self {
        let include_roots = config.resolve_paths();
        let excluded_prefixes = config.resolve_exclude_paths();
        let excluded_names: HashSet<String> = config.index.exclude.iter().cloned().collect();

        Self {
            include_roots,
            excluded_prefixes,
            excluded_names,
        }
    }

    /// Return whether `path` is under at least one include root.
    pub fn is_under_include_root(&self, path: &Path) -> bool {
        if self.include_roots.is_empty() {
            return true; // No roots configured = accept everything
        }
        self.include_roots
            .iter()
            .any(|root| path_starts_with_ci(path, root))
    }

    /// Return a reference to the normalized excluded path prefixes.
    pub fn excluded_prefixes(&self) -> &[PathBuf] {
        &self.excluded_prefixes
    }

    /// Return whether `path` falls under any excluded path prefix.
    pub fn is_under_excluded_prefix(&self, path: &Path) -> bool {
        self.excluded_prefixes
            .iter()
            .any(|prefix| path_starts_with_ci(path, prefix))
    }

    /// Return whether any path component matches an excluded directory name.
    /// Uses component-level matching: `/foo/node_modules/bar` matches `node_modules`,
    /// but `/foo/mynode_modules/bar` does not.
    pub fn has_excluded_component(&self, path: &Path) -> bool {
        path.components().any(|c| {
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
    /// Checks in order from cheapest to most expensive:
    /// 1. Not under any include root → drop
    /// 2. Under an excluded path prefix → drop
    /// 3. Has an excluded directory name component → drop
    pub fn is_excluded(&self, path: &Path) -> bool {
        if !self.is_under_include_root(path) {
            return true;
        }
        if self.is_under_excluded_prefix(path) {
            return true;
        }
        if self.has_excluded_component(path) {
            return true;
        }
        false
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
        assert!(
            filter.is_under_excluded_prefix(&library_cache),
            "~/Library/Caches should be under excluded prefix"
        );
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
    fn test_is_under_include_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_string_lossy().to_string();
        let filter = make_test_filter(vec![&root], vec![], vec![]);

        let inside = tmp.path().join("foo.txt");
        fs::write(&inside, "data").unwrap();

        assert!(filter.is_under_include_root(&inside));
        assert!(!filter.is_under_include_root(Path::new("/tmp/outside.txt")));
    }

    #[test]
    fn test_is_under_excluded_prefix() {
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
        assert!(filter.is_under_excluded_prefix(&inside));
        assert!(!filter.is_under_excluded_prefix(&tmp.path().join("ok/file.txt")));
    }

    #[test]
    fn test_has_excluded_component() {
        let filter = make_test_filter(
            vec!["/tmp"],
            vec!["node_modules", ".git"],
            vec![],
        );

        assert!(filter.has_excluded_component(Path::new("/tmp/proj/node_modules/pkg/index.js")));
        assert!(filter.has_excluded_component(Path::new("/tmp/proj/.git/config")));
        // Component-level: only exact directory name match, not substring
        assert!(!filter.has_excluded_component(Path::new("/tmp/mynode_modules/pkg")));
        assert!(!filter.has_excluded_component(Path::new("/tmp/proj/src/main.rs")));
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
            filter.excluded_prefixes.contains(&lib),
            "excluded_prefixes should contain resolved ~/Library"
        );

        // ~/.npm should be in excluded prefixes
        let npm = home.join(".npm");
        assert!(
            filter.excluded_prefixes.contains(&npm),
            "excluded_prefixes should contain resolved ~/.npm"
        );
    }
}
