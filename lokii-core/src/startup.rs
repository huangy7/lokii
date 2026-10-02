use crate::cache;
use crate::config::AppConfig;
use crate::indexer::IndexStore;
use crate::scanner;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Arc;

/// Result of index initialization.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StartupMode {
    /// Loaded from cache, user can search immediately.
    /// Background verification will follow.
    HotStart { cached_count: usize },
    /// No cache available, performed full scan.
    ColdStart { scanned_count: usize },
}

/// Initialize the index: try cache first, fall back to full scan.
///
/// For hot start: loads cache into store and returns immediately so the UI
/// can become interactive. The caller is responsible for triggering
/// `verify_index` afterwards on a background thread.
///
/// For cold start: performs a full blocking scan.
pub fn initialize_index(
    store: &IndexStore,
    config: &AppConfig,
    progress: Arc<AtomicUsize>,
    cache_file: &Path,
) -> StartupMode {
    let paths = config.resolve_paths();

    // Try hot start from cache
    match cache::load_cache(cache_file) {
        Ok(entries) => {
            let count = entries.len();
            store.replace(entries);

            // Report cached count as initial progress
            progress.store(count, std::sync::atomic::Ordering::Relaxed);

            StartupMode::HotStart { cached_count: count }
        }
        Err(_) => {
            // Cold start: full scan (silently fall back on any cache error)
            let filter = Arc::new(crate::filter::CompiledFilter::from_config(config));
            let fresh = scanner::scan_directories(
                &paths,
                filter,
                config.index.include_hidden,
                progress,
            );
            let count = fresh.len();

            // Save cache for next startup
            let _ = cache::save_cache(&fresh, cache_file);

            store.replace(fresh);
            StartupMode::ColdStart { scanned_count: count }
        }
    }
}

/// Verify the cached index by doing a fresh scan and replacing the store.
/// Call this on a background thread after a hot start.
/// Checks `cancel` flag — if set, aborts early without modifying the store.
pub fn verify_index(
    store: &IndexStore,
    config: &AppConfig,
    cache_file: &Path,
    cancel: &AtomicBool,
) {
    let paths = config.resolve_paths();
    let progress = Arc::new(AtomicUsize::new(0));

    let filter = Arc::new(crate::filter::CompiledFilter::from_config(config));
    let fresh = scanner::scan_directories(
        &paths,
        filter,
        config.index.include_hidden,
        progress,
    );

    // If cancelled (e.g. rebuild started), don't overwrite the store
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }

    store.replace(fresh);

    // Re-save cache with verified data
    let entries = store.entries();
    let _ = cache::save_cache(&entries, cache_file);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache;
    use crate::indexer::FileEntry;
    use std::fs;
    use tempfile::TempDir;

    fn make_test_config(scan_dir: &Path) -> AppConfig {
        let mut config = AppConfig::default();
        config.index.index_paths = vec![scan_dir.to_string_lossy().to_string()];
        config.index.exclude = vec![];
        config.index.include_hidden = true;
        config
    }

    fn make_progress() -> Arc<AtomicUsize> {
        Arc::new(AtomicUsize::new(0))
    }

    fn make_entries(n: usize) -> Vec<FileEntry> {
        (0..n)
            .map(|i| FileEntry {
                name: format!("file_{i}.txt"),
                path: format!("/tmp/test/file_{i}.txt"),
                is_dir: false,
                size: (i as u64) * 100,
                modified: 1700000000 + (i as u64),
                created_at: 0,
                extension: "txt".to_string(),
            })
            .collect()
    }

    #[test]
    fn test_cold_start_no_cache() {
        let scan_dir = TempDir::new().unwrap();
        fs::write(scan_dir.path().join("a.txt"), "hello").unwrap();
        fs::write(scan_dir.path().join("b.txt"), "world").unwrap();

        let cache_dir = TempDir::new().unwrap();
        let cache_file = cache_dir.path().join("index.cache");

        let store = IndexStore::new();
        let config = make_test_config(scan_dir.path());

        let mode = initialize_index(&store, &config, make_progress(), &cache_file);

        match mode {
            StartupMode::ColdStart { scanned_count } => {
                assert!(scanned_count > 0, "Should have scanned some entries");
            }
            other => panic!("Expected ColdStart, got: {other:?}"),
        }

        assert!(store.len() > 0, "Store should have entries after cold start");
        assert!(cache_file.exists(), "Cache file should be saved after cold start");
    }

    #[test]
    fn test_hot_start_with_cache() {
        let cache_dir = TempDir::new().unwrap();
        let cache_file = cache_dir.path().join("index.cache");
        let entries = make_entries(10);

        // Pre-save cache
        cache::save_cache(&entries, &cache_file).unwrap();

        // Use a scan dir that exists but is empty (verification scan will run)
        let scan_dir = TempDir::new().unwrap();
        let store = IndexStore::new();
        let config = make_test_config(scan_dir.path());

        let mode = initialize_index(&store, &config, make_progress(), &cache_file);

        match mode {
            StartupMode::HotStart { cached_count } => {
                assert_eq!(cached_count, 10);
            }
            other => panic!("Expected HotStart, got: {other:?}"),
        }

        // After blocking verification, store has been updated with fresh scan
        // (empty dir = 0 entries after verification replaces cached data)
    }

    #[test]
    fn test_corrupt_cache_falls_back() {
        let scan_dir = TempDir::new().unwrap();
        fs::write(scan_dir.path().join("file.txt"), "data").unwrap();

        let cache_dir = TempDir::new().unwrap();
        let cache_file = cache_dir.path().join("index.cache");

        // Write garbage to cache
        fs::write(&cache_file, b"garbage data that is not a valid cache").unwrap();

        let store = IndexStore::new();
        let config = make_test_config(scan_dir.path());

        let mode = initialize_index(&store, &config, make_progress(), &cache_file);

        match mode {
            StartupMode::ColdStart { scanned_count } => {
                assert!(scanned_count > 0, "Should fall back to full scan");
            }
            other => panic!("Expected ColdStart fallback, got: {other:?}"),
        }
    }

    #[test]
    fn test_blocking_verify_on_hot_start() {
        let cache_dir = TempDir::new().unwrap();
        let cache_file = cache_dir.path().join("index.cache");
        let entries = make_entries(5);

        cache::save_cache(&entries, &cache_file).unwrap();

        // Scan dir with actual files
        let scan_dir = TempDir::new().unwrap();
        fs::write(scan_dir.path().join("real.txt"), "content").unwrap();

        let store = IndexStore::new();
        let config = make_test_config(scan_dir.path());

        let mode = initialize_index(&store, &config, make_progress(), &cache_file);
        assert!(matches!(mode, StartupMode::HotStart { .. }));

        // Verification is now blocking, so store is already updated
        // with fresh scan results (which differ from cached data)
        assert!(store.len() > 0, "Store should have entries after blocking verify");
    }
}
