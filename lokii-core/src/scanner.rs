use crate::indexer::FileEntry;
use crate::path_utils::{path_starts_with_ci, paths_equal_ci};
use crate::permissions::has_full_disk_access;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// 没有完全磁盘访问权限时，macOS 会弹窗询问的受保护路径。
/// 直接跳过这些路径，避免触发系统权限对话框。
pub(crate) fn protected_paths() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return vec![];
    };
    let lib = home.join("Library");
    let base_paths = vec![
        // ~/Library 下的受保护目录
        lib.join("Application Support"),
        lib.join("Caches"),
        lib.join("Containers"),
        lib.join("Group Containers"),
        lib.join("HTTPStorages"),
        lib.join("Application Scripts"),
        lib.join("Metadata"),
        lib.join("Cookies"),
        lib.join("IdentityServices"),
        lib.join("Sharing"),
        lib.join("Mail"),
        lib.join("Messages"),
        lib.join("Safari"),
        lib.join("Calendars"),
        lib.join("Reminders"),
        lib.join("HomeKit"),
        lib.join("Photos"),
        lib.join("Music"),
        lib.join("Media"),
        lib.join("MediaLibrary"),
        lib.join("PersonalizationPortrait"),
        lib.join("Suggestions"),
        // 用户主目录下的受保护目录
        home.join("Desktop"),
        home.join("Downloads"),
        home.join("Documents"),
        home.join("Pictures"),
        home.join("Movies"),
        home.join("Music"),
        home.join("iTunes"),
        // 系统目录（有些可能触发弹窗，有些是为防止误扫）
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Library/Assets"),
        PathBuf::from("/Library/Application Support/com.apple.TCC"),
    ];

    // macOS 可能返回 Data Volume 真实路径（/System/Volumes/Data/...），
    // 为避免 /Users/... 过滤失效，这里补充等价别名路径。
    with_macos_data_volume_aliases(base_paths)
}

fn with_macos_data_volume_aliases(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let users_prefix = Path::new("/Users");
    let lib_prefix = Path::new("/Library");
    let users_data_prefix = Path::new("/System/Volumes/Data/Users");
    let lib_data_prefix = Path::new("/System/Volumes/Data/Library");

    let mut out = Vec::with_capacity(paths.len() * 2);
    for p in paths {
        out.push(p.clone());

        if let Ok(rest) = p.strip_prefix(users_prefix) {
            out.push(users_data_prefix.join(rest));
            continue;
        }

        if let Ok(rest) = p.strip_prefix(lib_prefix) {
            out.push(lib_data_prefix.join(rest));
        }
    }

    out.sort();
    out.dedup();
    out
}

pub(crate) fn is_protected_or_descendant(path: &Path, protected: &[PathBuf]) -> bool {
    protected
        .iter()
        .any(|p| paths_equal_ci(path, p) || path_starts_with_ci(path, p))
}

#[cfg(test)]
fn dummy_filter() -> std::sync::Arc<crate::filter::CompiledFilter> { let mut cfg = crate::config::AppConfig::default(); cfg.index.index_paths = vec![]; std::sync::Arc::new(crate::filter::CompiledFilter::from_config(&cfg)) }

pub(crate) fn contains_protected_descendant(path: &Path, protected: &[PathBuf]) -> bool {
    protected.iter().any(|p| path_starts_with_ci(p, path))
}

/// Scan directories in parallel using jwalk, applying exclusion filters
/// during traversal (not post-filter) for performance.
///
/// # Arguments
/// - `paths`: Root directories to scan
/// - `exclude`: Directory names to skip (e.g., "node_modules", ".git")
/// - `include_hidden`: Whether to include hidden files/dirs (starting with ".")
/// - `progress`: Atomic counter incremented for each file processed
///
/// # Notes
/// - Exclusion is applied during traversal via `process_read_dir` callback,
///   preventing descent into excluded directories (per D-08)
/// - Permission-denied entries are silently skipped
/// - Must NOT be called from within a Rayon pool (jwalk uses Rayon internally)
pub fn scan_directories(
    paths: &[PathBuf],
    filter: Arc<crate::filter::CompiledFilter>,
    include_hidden: bool,
    progress: Arc<AtomicUsize>,
) -> Vec<FileEntry> {
    let mut all_entries = Vec::new();

    // 没有 FDA 时跳过受保护路径，避免触发系统权限弹窗
    let skip_paths: Vec<PathBuf> = if has_full_disk_access() {
        vec![]
    } else {
        protected_paths()
    };

    for root in paths {
        if !root.exists() {
            continue;
        }

        if is_protected_or_descendant(root, &skip_paths) {
            continue;
        }

        let filter_clone = filter.clone();
        let skip_paths_clone = skip_paths.clone();

        let walker = jwalk::WalkDir::new(root)
            .skip_hidden(false) // We handle hidden file filtering ourselves in process_read_dir
            .process_read_dir(move |_depth, _path, _state, children| {
                children.retain(|entry_result| {
                    entry_result
                        .as_ref()
                        .map(|entry| {
                            let full_path = entry.path();
                            // 没有 FDA 时跳过受保护路径（不论是目录还是文件）
                            if is_protected_or_descendant(&full_path, &skip_paths_clone) {
                                return false;
                            }

                            let name = entry.file_name.to_string_lossy();

                            // Skip hidden files/dirs if configured
                            if !include_hidden && name.starts_with('.') {
                                return false;
                            }

                            // Use CompiledFilter to exclude paths and directories
                            if filter_clone.is_excluded(&full_path) {
                                return false;
                            }

                            true
                        })
                        .unwrap_or(false) // Filter out Err entries (permission denied)
                });
            });

        for entry in walker {
            if let Ok(entry) = entry {
                if let Ok(metadata) = entry.metadata() {
                    let name_str = entry.file_name.to_string_lossy();
                    let extension = if metadata.is_dir() {
                        String::new()
                    } else {
                        std::path::Path::new(name_str.as_ref())
                            .extension()
                            .map(|ext| ext.to_string_lossy().to_lowercase())
                            .unwrap_or_default()
                    };
                    all_entries.push(FileEntry {
                        name: name_str.into_owned(),
                        path: entry.path().to_string_lossy().into_owned(),
                        is_dir: metadata.is_dir(),
                        size: metadata.len(),
                        modified: metadata
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                        created_at: metadata
                            .created()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                        extension,
                    });
                    progress.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    all_entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::Builder;

    fn make_progress() -> Arc<AtomicUsize> {
        Arc::new(AtomicUsize::new(0))
    }

    fn make_temp_dir() -> tempfile::TempDir {
        Builder::new()
            .prefix("lokii_test_")
            .tempdir()
            .unwrap()
    }

    #[test]
    fn test_scan_basic() {
        let tmp = make_temp_dir();
        fs::write(tmp.path().join("file1.txt"), "hello").unwrap();
        fs::write(tmp.path().join("file2.rs"), "fn main() {}").unwrap();
        fs::write(tmp.path().join("file3.md"), "# README").unwrap();

        let progress = make_progress();
        let entries = scan_directories(
            &[tmp.path().to_path_buf()],
            dummy_filter(),
            true, // include hidden so we don't filter anything
            progress.clone(),
        );

        // Should have the root dir + 3 files = 4, or just 3 files
        // jwalk includes the root directory entry
        assert!(entries.len() >= 3, "Expected at least 3 entries, got {}", entries.len());

        // Verify entries have non-empty names and paths
        for entry in &entries {
            assert!(!entry.name.is_empty());
            assert!(!entry.path.is_empty());
        }

        // Verify specific files exist
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"file1.txt"));
        assert!(names.contains(&"file2.rs"));
        assert!(names.contains(&"file3.md"));
    }

    #[test]
    fn test_scan_exclude_dir() {
        let tmp = make_temp_dir();
        fs::create_dir_all(tmp.path().join("node_modules/pkg")).unwrap();
        fs::write(tmp.path().join("node_modules/pkg/index.js"), "module.exports = {}").unwrap();
        fs::write(tmp.path().join("app.rs"), "fn main() {}").unwrap();

        let progress = make_progress();
        let entries = scan_directories(
            &[tmp.path().to_path_buf()],
            dummy_filter(),
            true,
            progress,
        );

        // No entry should contain "node_modules" in its path
        for entry in &entries {
            assert!(
                !entry.path.contains("node_modules"),
                "Found excluded directory in results: {}",
                entry.path
            );
        }

        // app.rs should be present
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"app.rs"));
    }

    #[test]
    fn test_scan_hidden_files_excluded() {
        let tmp = make_temp_dir();
        fs::write(tmp.path().join(".hidden"), "secret").unwrap();
        fs::write(tmp.path().join("visible"), "public").unwrap();

        let progress = make_progress();
        let entries = scan_directories(
            &[tmp.path().to_path_buf()],
            dummy_filter(),
            false, // exclude hidden
            progress,
        );

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"visible"), "visible file should be present");
        assert!(!names.contains(&".hidden"), ".hidden file should be excluded");
    }

    #[test]
    fn test_scan_hidden_files_included() {
        let tmp = make_temp_dir();
        fs::write(tmp.path().join(".hidden"), "secret").unwrap();
        fs::write(tmp.path().join("visible"), "public").unwrap();

        let progress = make_progress();
        let entries = scan_directories(
            &[tmp.path().to_path_buf()],
            dummy_filter(),
            true, // include hidden
            progress,
        );

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"visible"), "visible file should be present");
        assert!(names.contains(&".hidden"), ".hidden file should be included");
    }

    #[test]
    fn test_scan_progress_counter() {
        let tmp = make_temp_dir();
        fs::write(tmp.path().join("a.txt"), "a").unwrap();
        fs::write(tmp.path().join("b.txt"), "b").unwrap();

        let progress = make_progress();
        let _entries = scan_directories(
            &[tmp.path().to_path_buf()],
            dummy_filter(),
            true,
            progress.clone(),
        );

        assert!(
            progress.load(Ordering::Relaxed) > 0,
            "Progress counter should be > 0"
        );
    }

    #[test]
    fn test_scan_nonexistent_path() {
        let progress = make_progress();
        let entries = scan_directories(
            &[PathBuf::from("/nonexistent/path/that/does/not/exist")],
            dummy_filter(),
            true,
            progress,
        );
        assert!(entries.is_empty(), "Scanning nonexistent path should return empty");
    }

    #[test]
    fn test_scan_file_entry_correctness() {
        let tmp = make_temp_dir();
        let subdir = tmp.path().join("subdir");
        fs::create_dir(&subdir).unwrap();
        fs::write(subdir.join("test.txt"), "content").unwrap();

        let progress = make_progress();
        let entries = scan_directories(
            &[tmp.path().to_path_buf()],
            dummy_filter(),
            true,
            progress,
        );

        // Find the test.txt entry
        let test_entry = entries.iter().find(|e| e.name == "test.txt");
        assert!(test_entry.is_some(), "test.txt should be in results");
        let test_entry = test_entry.unwrap();
        assert!(!test_entry.is_dir);
        assert_eq!(test_entry.size, 7); // "content" is 7 bytes
        assert!(test_entry.modified > 0);
        assert!(test_entry.path.ends_with("test.txt"));
        assert_eq!(test_entry.extension, "txt");

        // Find the subdir entry
        let dir_entry = entries.iter().find(|e| e.name == "subdir");
        assert!(dir_entry.is_some(), "subdir should be in results");
        assert!(dir_entry.unwrap().is_dir);
        assert_eq!(dir_entry.unwrap().extension, "");
    }

    #[test]
    fn test_scan_extension_extraction() {
        let tmp = make_temp_dir();
        fs::write(tmp.path().join("test.rs"), "fn main() {}").unwrap();
        fs::write(tmp.path().join("report.PDF"), "pdf content").unwrap();
        fs::write(tmp.path().join("Makefile"), "all:").unwrap();
        fs::create_dir(tmp.path().join("mydir")).unwrap();

        let progress = make_progress();
        let entries = scan_directories(
            &[tmp.path().to_path_buf()],
            dummy_filter(),
            true,
            progress,
        );

        let rs_entry = entries.iter().find(|e| e.name == "test.rs").unwrap();
        assert_eq!(rs_entry.extension, "rs");

        let pdf_entry = entries.iter().find(|e| e.name == "report.PDF").unwrap();
        assert_eq!(pdf_entry.extension, "pdf"); // lowercased

        let makefile_entry = entries.iter().find(|e| e.name == "Makefile").unwrap();
        assert_eq!(makefile_entry.extension, ""); // no extension

        let dir_entry = entries.iter().find(|e| e.name == "mydir").unwrap();
        assert_eq!(dir_entry.extension, ""); // directories have no extension
    }
}
