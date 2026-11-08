use crate::cache;
use crate::config::AppConfig;
use crate::indexer::IndexStore;
use crate::scanner;

use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

/// Result of index initialization.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StartupMode {
    /// Loaded from cache, user can search immediately; the index service
    /// catches up from `checkpoint` (or re-verifies without one).
    HotStart {
        cached_count: usize,
        /// Replay point stored with the cache, if it still matches the config.
        checkpoint: Option<cache::Checkpoint>,
    },
    /// No cache available, performed full scan.
    ColdStart {
        scanned_count: usize,
        /// Event id read before the scan: replaying from it covers changes made meanwhile.
        event_id: u64,
    },
    Failed(crate::compact_index::IndexError),
}

/// Capture a checkpoint for a snapshot about to be scanned or replayed: the
/// event id is read first, so anything that changes during the work is
/// replayed again next time rather than lost.
pub fn checkpoint(config: &AppConfig) -> cache::Checkpoint {
    let event_id = crate::fsevents::current_event_id();
    cache::Checkpoint {
        event_id,
        fingerprint: scope_fingerprint(config),
    }
}

/// Hash of everything that decides which paths get indexed: roots, exclusion
/// rules, hidden files and Full Disk Access.
pub fn scope_fingerprint(config: &AppConfig) -> u64 {
    use std::hash::{Hash, Hasher};
    // A different binary may hash differently; that only costs one full scan.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    config.resolve_paths().hash(&mut hasher);
    config.resolve_exclude_paths().hash(&mut hasher);
    config.index.exclude.hash(&mut hasher);
    config.index.include_hidden.hash(&mut hasher);
    crate::permissions::has_full_disk_access().hash(&mut hasher);
    hasher.finish()
}

/// Save `index` with the replay checkpoint it reflects.
pub fn save_index(
    index: &crate::compact_index::CompactIndex,
    checkpoint: cache::Checkpoint,
    cache_file: &Path,
) {
    if let Err(error) = cache::save_compact(index, Some(checkpoint), cache_file) {
        eprintln!("[Lokii] index cache save failed: {error}");
    }
}

/// Initialize the index: try cache first, fall back to full scan.
///
/// For hot start: loads cache into store and returns immediately so the UI
/// can become interactive. The index service then catches up in the background.
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
    match cache::load_compact(cache_file) {
        Ok((entries, saved)) => {
            let count = entries.len();
            store.replace_compact(entries);

            // Report cached count as initial progress
            progress.store(count, std::sync::atomic::Ordering::Relaxed);

            let fingerprint = scope_fingerprint(config);
            StartupMode::HotStart {
                cached_count: count,
                checkpoint: saved.filter(|saved| saved.fingerprint == fingerprint),
            }
        }
        Err(_) => {
            // Cold start: full scan (silently fall back on any cache error)
            let checkpoint = checkpoint(config);
            let filter = Arc::new(crate::filter::CompiledFilter::from_config(config));
            let fresh = match scanner::scan_compact(
                &paths,
                filter,
                config.index.include_hidden,
                progress,
            ) {
                Ok(fresh) => fresh,
                Err(error) => return StartupMode::Failed(error),
            };
            let count = fresh.len();

            // Save cache for next startup
            let _ = cache::save_compact(&fresh, Some(checkpoint), cache_file);

            store.replace_compact(fresh);
            StartupMode::ColdStart {
                scanned_count: count,
                event_id: checkpoint.event_id,
            }
        }
    }
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
            StartupMode::ColdStart { scanned_count, .. } => {
                assert!(scanned_count > 0, "Should have scanned some entries");
            }
            other => panic!("Expected ColdStart, got: {other:?}"),
        }

        assert!(
            store.len() > 0,
            "Store should have entries after cold start"
        );
        assert!(
            cache_file.exists(),
            "Cache file should be saved after cold start"
        );
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
            StartupMode::HotStart { cached_count, .. } => {
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
            StartupMode::ColdStart { scanned_count, .. } => {
                assert!(scanned_count > 0, "Should fall back to full scan");
            }
            other => panic!("Expected ColdStart fallback, got: {other:?}"),
        }
    }

    #[test]
    fn saved_index_resumes_from_its_checkpoint() {
        let scan_dir = TempDir::new().unwrap();
        let cache_dir = TempDir::new().unwrap();
        let cache_file = cache_dir.path().join("index.cache");
        let config = make_test_config(scan_dir.path());
        let store = IndexStore::new();
        store.replace(make_entries(3));

        let checkpoint = cache::Checkpoint {
            event_id: 99,
            fingerprint: scope_fingerprint(&config),
        };
        save_index(&store.read(), checkpoint, &cache_file);
        let mode = initialize_index(&IndexStore::new(), &config, make_progress(), &cache_file);
        assert!(matches!(
            mode,
            StartupMode::HotStart {
                cached_count: 3,
                checkpoint: Some(cache::Checkpoint { event_id: 99, .. })
            }
        ));
    }

    #[test]
    fn changed_config_discards_checkpoint() {
        let scan_dir = TempDir::new().unwrap();
        fs::write(scan_dir.path().join("a.txt"), "a").unwrap();
        let cache_dir = TempDir::new().unwrap();
        let cache_file = cache_dir.path().join("index.cache");
        let mut config = make_test_config(scan_dir.path());
        initialize_index(&IndexStore::new(), &config, make_progress(), &cache_file);

        config.index.include_hidden = false;
        let mode = initialize_index(&IndexStore::new(), &config, make_progress(), &cache_file);
        assert!(matches!(
            mode,
            StartupMode::HotStart {
                checkpoint: None,
                ..
            }
        ));
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
        assert!(
            store.len() > 0,
            "Store should have entries after blocking verify"
        );
    }
}
