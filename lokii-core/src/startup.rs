use crate::cache;
use crate::config::AppConfig;
use crate::indexer::IndexStore;
use crate::path_utils::{path_starts_with_ci, paths_equal_ci};
use crate::scanner;
use crate::scope_diff::PrimaryScopeSnapshot;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

/// Result of index initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupMode {
    /// Loaded from cache, user can search immediately; the index service
    /// catches up from `checkpoint` (or runs incremental diff / verification).
    HotStart {
        cached_count: usize,
        /// Replay point stored with the cache, if trusted for resume or incremental diff.
        checkpoint: Option<cache::Checkpoint>,
        /// Structural scope snapshot decoded from v6 cache or claimed from v5 cache.
        saved_snapshot: Option<PrimaryScopeSnapshot>,
        /// Optional one-shot directory rescans triggered by v5 -> v6 upgrade claim (e.g. non-shallow `.app` bundles).
        upgrade_rescans: Vec<PathBuf>,
        /// True when a v5 cache was claimed and should be persisted as v6 on next save.
        needs_v6_persist: bool,
    },
    /// No cache available, performed full scan.
    ColdStart {
        scanned_count: usize,
        /// Event id read before the scan: replaying from it covers changes made meanwhile.
        event_id: u64,
    },
    Failed(crate::compact_index::IndexError),
}

/// Error returned when index initialization fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitError {
    Index(crate::compact_index::IndexError),
    Io(String),
}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InitError::Index(e) => write!(f, "Index error during init: {e}"),
            InitError::Io(e) => write!(f, "IO error during init: {e}"),
        }
    }
}

impl std::error::Error for InitError {}

impl From<crate::compact_index::IndexError> for InitError {
    fn from(err: crate::compact_index::IndexError) -> Self {
        InitError::Index(err)
    }
}

/// Capture a checkpoint for a snapshot about to be scanned or replayed: the
/// event id is read first, so anything that changes during the work is
/// replayed again next time rather than lost.
#[cfg(test)]
fn checkpoint(config: &AppConfig) -> cache::Checkpoint {
    let event_id = crate::fsevents::current_event_id();
    cache::Checkpoint {
        event_id,
        fingerprint: scope_fingerprint(config),
    }
}

/// Hash of everything that decides which paths get indexed: roots, exclusion
/// rules, hidden files and Full Disk Access.
#[cfg(test)]
fn scope_fingerprint(config: &AppConfig) -> u64 {
    scope_fingerprint_with_fda(config, crate::permissions::has_full_disk_access())
}

/// 复用已探测的 `has_fda` 状态计算全量索引范围指纹（仅保留给旧单域测试校验）。
#[cfg(test)]
pub(crate) fn scope_fingerprint_with_fda(config: &AppConfig, has_fda: bool) -> u64 {
    use std::hash::{Hash, Hasher};
    // A different binary may hash differently; that only costs one full scan.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    config.resolve_paths().hash(&mut hasher);
    config.resolve_exclude_paths().hash(&mut hasher);
    config.index.exclude.hash(&mut hasher);
    config.index.include_hidden.hash(&mut hasher);
    config.index.exclude_sensitive.hash(&mut hasher);
    config.index.sensitive_names.hash(&mut hasher);
    config.index.index_apps.hash(&mut hasher);
    has_fda.hash(&mut hasher);
    hasher.finish()
}

/// Calculate deterministic 64-bit FNV-1a scope fingerprint for the primary local index domain.
pub fn primary_scope_fingerprint(
    config: &AppConfig,
    primary_roots: &[std::path::PathBuf],
    excluded_mount_roots: &[std::path::PathBuf],
) -> u64 {
    primary_scope_fingerprint_with_fda(
        config,
        primary_roots,
        excluded_mount_roots,
        crate::permissions::has_full_disk_access(),
    )
}

/// 复用已探测的 `has_fda` 状态计算确定性 FNV-1a 主本地索引域指纹。
pub(crate) fn primary_scope_fingerprint_with_fda(
    config: &AppConfig,
    primary_roots: &[std::path::PathBuf],
    excluded_mount_roots: &[std::path::PathBuf],
    has_fda: bool,
) -> u64 {
    PrimaryScopeSnapshot::from_inputs(config, primary_roots, excluded_mount_roots, has_fda)
        .fingerprint_fnv1a()
}

/// 兼容 v5 缓存平滑升级认领的旧 `DefaultHasher` 指纹算法（仅用于首次读取 v5 缓存时的校验）。
pub(crate) fn legacy_primary_scope_fingerprint_with_fda(
    config: &AppConfig,
    primary_roots: &[std::path::PathBuf],
    excluded_mount_roots: &[std::path::PathBuf],
    has_fda: bool,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    primary_roots.hash(&mut hasher);
    config.resolve_exclude_paths().hash(&mut hasher);
    if !excluded_mount_roots.is_empty() {
        excluded_mount_roots.hash(&mut hasher);
    }
    config.index.exclude.hash(&mut hasher);
    config.index.include_hidden.hash(&mut hasher);
    config.index.exclude_sensitive.hash(&mut hasher);
    config.index.sensitive_names.hash(&mut hasher);
    config.index.index_apps.hash(&mut hasher);
    has_fda.hash(&mut hasher);
    hasher.finish()
}

/// 升级认领 v5 -> v6 且 `index_apps == true` 时，收集内存中位于非浅层根下的已有 `.app` 目录路径，
/// 交由后台 `rescans` 补齐旧版全局浅层跳过的包内子项（如 `Wrapper/`），维持与从零全扫 100% 等价。
fn collect_v5_non_shallow_app_dirs(
    store: &IndexStore,
    snapshot: &PrimaryScopeSnapshot,
) -> Vec<PathBuf> {
    let mut app_dirs: Vec<PathBuf> = Vec::new();
    let guard = store.read();
    for (_, view) in guard.iter() {
        if !view.is_dir {
            continue;
        }
        let is_app = view
            .name
            .rsplit_once('.')
            .is_some_and(|(stem, ext)| !stem.is_empty() && ext.eq_ignore_ascii_case("app"));
        if !is_app {
            continue;
        }
        let path_str = view.materialize().path;
        let path = Path::new(&path_str);
        let deepest = snapshot
            .primary_roots
            .iter()
            .filter(|r| path_starts_with_ci(path, Path::new(r)))
            .max_by_key(|r| Path::new(r).components().count());
        let under_shallow = deepest.is_some_and(|r| {
            snapshot
                .shallow_app_roots
                .iter()
                .any(|s| paths_equal_ci(Path::new(r), Path::new(s)))
        });
        if !under_shallow && path.exists() {
            app_dirs.push(path.to_path_buf());
        }
    }
    app_dirs
}

/// Save `index` with the replay checkpoint it reflects.
#[cfg(test)]
fn save_index(
    index: &crate::compact_index::CompactIndex,
    checkpoint: cache::Checkpoint,
    cache_file: &Path,
) {
    if let Err(error) = cache::save_compact(index, Some(checkpoint), cache_file) {
        eprintln!("[Lokii] index cache save failed: {error}");
    }
}

/// 将已在短读锁内完成内存编码的索引字节流连同检查点与结构化快照原子落盘为 v6 缓存。
pub fn save_encoded_index_with_snapshot(
    encoded: &[u8],
    checkpoint: cache::Checkpoint,
    snapshot: &PrimaryScopeSnapshot,
    cache_file: &Path,
) {
    if let Err(error) =
        cache::save_encoded_compact_with_snapshot(encoded, checkpoint, snapshot, cache_file)
    {
        eprintln!("[Lokii] index cache save failed: {error}");
    }
}

/// 将已在短读锁内完成内存编码的索引字节流连同检查点落盘（v5 兼容入口）。
pub fn save_encoded_index(encoded: &[u8], checkpoint: cache::Checkpoint, cache_file: &Path) {
    if let Err(error) = cache::save_encoded_compact(encoded, Some(checkpoint), cache_file) {
        eprintln!("[Lokii] index cache save failed: {error}");
    }
}

/// Initialize the index: try cache first, fall back to full scan (legacy single-domain path, test-only).
#[cfg(test)]
fn initialize_index(
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
                saved_snapshot: None,
                upgrade_rescans: Vec::new(),
                needs_v6_persist: false,
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
            // 先将新索引发布进 store 立即可搜，再在读锁外执行落盘与 sync_data()
            store.replace_compact(fresh);
            let encoded = {
                let guard = store.read();
                cache::encode_compact(&guard)
            };
            let _ = cache::save_encoded_compact(&encoded, Some(checkpoint), cache_file);
            StartupMode::ColdStart {
                scanned_count: count,
                event_id: checkpoint.event_id,
            }
        }
    }
}

/// Initialize the primary index: try cache first, fall back to full scan.
pub fn initialize_primary_index(
    store: &IndexStore,
    config: &AppConfig,
    primary_roots: &[std::path::PathBuf],
    excluded_mount_roots: &[std::path::PathBuf],
    on_progress: &mut dyn FnMut(&str, u64),
    cache_path: &Path,
) -> Result<StartupMode, InitError> {
    initialize_primary_index_with_progress(
        store,
        config,
        primary_roots,
        excluded_mount_roots,
        Arc::new(AtomicUsize::new(0)),
        on_progress,
        cache_path,
    )
}

/// Initialize the primary index while updating a shared atomic scan counter so
/// callers can poll live progress during cold-start scans.
pub fn initialize_primary_index_with_progress(
    store: &IndexStore,
    config: &AppConfig,
    primary_roots: &[std::path::PathBuf],
    excluded_mount_roots: &[std::path::PathBuf],
    progress: Arc<AtomicUsize>,
    on_progress: &mut dyn FnMut(&str, u64),
    cache_path: &Path,
) -> Result<StartupMode, InitError> {
    let (mode, cold_persist) = initialize_primary_index_deferred_persist(
        store,
        config,
        primary_roots,
        excluded_mount_roots,
        progress,
        on_progress,
        cache_path,
    )?;
    if let Some((checkpoint, snapshot)) = cold_persist {
        let encoded = {
            let guard = store.read();
            cache::encode_compact(&guard)
        };
        let _ = cache::save_encoded_compact_with_snapshot(
            &encoded,
            checkpoint,
            &snapshot,
            cache_path,
        );
    }
    Ok(mode)
}

/// 初始化主索引：冷启动扫盘完成后立即调用 `store.replace_compact(fresh)` 发布索引并返回待落盘的
/// `(Checkpoint, PrimaryScopeSnapshot)`，使调用方得以先向 UI 派发 `on_index_ready` 解锁交互，随后在无锁状态下原子落盘 v6 缓存。
pub(crate) fn initialize_primary_index_deferred_persist(
    store: &IndexStore,
    config: &AppConfig,
    primary_roots: &[std::path::PathBuf],
    excluded_mount_roots: &[std::path::PathBuf],
    progress: Arc<AtomicUsize>,
    on_progress: &mut dyn FnMut(&str, u64),
    cache_path: &Path,
) -> Result<(StartupMode, Option<(cache::Checkpoint, PrimaryScopeSnapshot)>), InitError> {
    let has_fda = crate::permissions::has_full_disk_access();
    let current_snapshot = PrimaryScopeSnapshot::from_inputs(
        config,
        primary_roots,
        excluded_mount_roots,
        has_fda,
    );

    if primary_roots.is_empty() {
        store.replace_compact(crate::compact_index::CompactIndex::new());
        if cache_path.exists() {
            if let Ok((entries, _, _)) = cache::load_compact_with_snapshot(cache_path) {
                if !entries.is_empty() {
                    let empty_index = crate::compact_index::CompactIndex::new();
                    let cp = cache::Checkpoint {
                        event_id: crate::fsevents::current_event_id(),
                        fingerprint: current_snapshot.fingerprint_fnv1a(),
                    };
                    let _ = cache::save_compact_with_snapshot(
                        &empty_index,
                        cp,
                        &current_snapshot,
                        cache_path,
                    );
                }
            }
        }
        on_progress("ready", 0);
        return Ok((
            StartupMode::HotStart {
                cached_count: 0,
                checkpoint: None,
                saved_snapshot: Some(current_snapshot),
                upgrade_rescans: Vec::new(),
                needs_v6_persist: false,
            },
            None,
        ));
    }

    match cache::load_compact_with_snapshot(cache_path) {
        Ok((entries, saved_cp, Some(saved_snapshot))) => {
            let count = entries.len();
            progress.store(count, std::sync::atomic::Ordering::Relaxed);
            store.replace_compact(entries);
            on_progress("cached", count as u64);
            Ok((
                StartupMode::HotStart {
                    cached_count: count,
                    checkpoint: saved_cp,
                    saved_snapshot: Some(saved_snapshot),
                    upgrade_rescans: Vec::new(),
                    needs_v6_persist: false,
                },
                None,
            ))
        }
        Ok((entries, saved_cp, None)) => {
            let count = entries.len();
            progress.store(count, std::sync::atomic::Ordering::Relaxed);
            store.replace_compact(entries);
            on_progress("cached", count as u64);

            // v5 缓存平滑升级认领：使用旧 DefaultHasher 指纹校验，匹配则免全量重扫并升级落盘为 v6
            let legacy_fp = legacy_primary_scope_fingerprint_with_fda(
                config,
                primary_roots,
                excluded_mount_roots,
                has_fda,
            );
            let claimed = saved_cp.filter(|cp| cp.fingerprint == legacy_fp);
            if let Some(cp) = claimed {
                let upgrade_rescans = if config.index.index_apps {
                    collect_v5_non_shallow_app_dirs(store, &current_snapshot)
                } else {
                    Vec::new()
                };
                let claimed_cp = cache::Checkpoint {
                    event_id: cp.event_id,
                    fingerprint: current_snapshot.fingerprint_fnv1a(),
                };
                Ok((
                    StartupMode::HotStart {
                        cached_count: count,
                        checkpoint: Some(claimed_cp),
                        saved_snapshot: Some(current_snapshot),
                        upgrade_rescans,
                        needs_v6_persist: true,
                    },
                    None,
                ))
            } else {
                Ok((
                    StartupMode::HotStart {
                        cached_count: count,
                        checkpoint: None,
                        saved_snapshot: None,
                        upgrade_rescans: Vec::new(),
                        needs_v6_persist: true,
                    },
                    None,
                ))
            }
        }
        Err(_) => {
            let current_event_id = crate::fsevents::current_event_id();
            let checkpoint = cache::Checkpoint {
                event_id: current_event_id,
                fingerprint: current_snapshot.fingerprint_fnv1a(),
            };
            let filter = Arc::new(crate::filter::CompiledFilter::from_primary_config(
                config,
                primary_roots,
                excluded_mount_roots,
            ));
            let fresh = match scanner::scan_compact(
                primary_roots,
                filter,
                config.index.include_hidden,
                progress,
            ) {
                Ok(fresh) => fresh,
                Err(error) => return Err(InitError::Index(error)),
            };
            let count = fresh.len();
            on_progress("scanned", count as u64);
            store.replace_compact(fresh);
            Ok((
                StartupMode::ColdStart {
                    scanned_count: count,
                    event_id: checkpoint.event_id,
                },
                Some((checkpoint, current_snapshot)),
            ))
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
                checkpoint: Some(cache::Checkpoint { event_id: 99, .. }),
                ..
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

    #[test]
    fn test_legacy_primary_scope_fingerprint_matches_legacy_when_no_mounted_excluded() {
        let config = AppConfig::default();
        let legacy_fp = scope_fingerprint(&config);
        let primary_fp = legacy_primary_scope_fingerprint_with_fda(
            &config,
            &config.resolve_paths(),
            &[],
            crate::permissions::has_full_disk_access(),
        );
        assert_eq!(primary_fp, legacy_fp);
    }

    #[test]
    fn v5_cache_upgrade_claims_matching_legacy_fingerprint_and_queues_non_shallow_app_backfill() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("home");
        let app_dir = root.join("MyTool.app");
        let wrapper_dir = app_dir.join("Wrapper");
        fs::create_dir_all(&wrapper_dir).unwrap();
        fs::write(wrapper_dir.join("bundle.bin"), "bin").unwrap();

        let mut cfg = make_test_config(&root);
        cfg.index.index_apps = true;
        let primary_roots = vec![root.clone()];
        let has_fda = crate::permissions::has_full_disk_access();
        let legacy_fp = legacy_primary_scope_fingerprint_with_fda(&cfg, &primary_roots, &[], has_fda);

        let v5_entries = vec![
            FileEntry {
                name: "MyTool.app".into(),
                path: app_dir.to_string_lossy().into_owned(),
                is_dir: true,
                size: 0,
                modified: 0,
                created_at: 0,
                extension: String::new(),
            },
        ];
        let compact = crate::compact_index::CompactIndex::from_entries(v5_entries).unwrap();
        let cache_file = tmp.path().join("index.cache");
        cache::save_compact(
            &compact,
            Some(cache::Checkpoint {
                event_id: 77,
                fingerprint: legacy_fp,
            }),
            &cache_file,
        )
        .unwrap();

        let store = IndexStore::new();
        let mut on_progress = |_: &str, _: u64| {};
        let mode = initialize_primary_index(
            &store,
            &cfg,
            &primary_roots,
            &[],
            &mut on_progress,
            &cache_file,
        )
        .unwrap();

        match mode {
            StartupMode::HotStart {
                cached_count,
                checkpoint,
                saved_snapshot,
                upgrade_rescans,
                needs_v6_persist,
            } => {
                assert_eq!(cached_count, 1);
                assert_eq!(checkpoint.map(|c| c.event_id), Some(77));
                assert!(saved_snapshot.is_some());
                assert!(needs_v6_persist);
                assert_eq!(upgrade_rescans, vec![app_dir]);
            }
            other => panic!("expected claimed HotStart, got {other:?}"),
        }
    }

    #[test]
    fn test_primary_scope_fingerprint_changes_on_roots() {
        let cfg = AppConfig::default();
        let roots_a = vec![std::path::PathBuf::from("/Users/dev")];
        let roots_b = vec![std::path::PathBuf::from("/Users/dev2")];
        let fp_a = primary_scope_fingerprint(&cfg, &roots_a, &[]);
        let fp_b = primary_scope_fingerprint(&cfg, &roots_b, &[]);
        assert_ne!(fp_a, fp_b);
    }

    #[test]
    fn test_initialize_primary_index_empty_roots_skips_scan() {
        let store = IndexStore::new();
        let cfg = AppConfig::default();
        let tmp = TempDir::new().unwrap();
        let cache_file = tmp.path().join("primary.cache");
        let mut progress_count = 0u64;
        let mut on_progress = |_phase: &str, count: u64| {
            progress_count = count;
        };
        let res = initialize_primary_index(&store, &cfg, &[], &[], &mut on_progress, &cache_file);
        assert!(res.is_ok());
        assert_eq!(store.len(), 0);
        assert_eq!(progress_count, 0);
    }

    #[test]
    fn test_initialize_primary_index_with_progress_updates_atomic_counter() {
        let store = IndexStore::new();
        let tmp = TempDir::new().unwrap();
        let scan_root = tmp.path().join("scan");
        fs::create_dir_all(&scan_root).unwrap();
        fs::write(scan_root.join("a.txt"), "a").unwrap();
        fs::write(scan_root.join("b.txt"), "b").unwrap();

        let cfg = make_test_config(&scan_root);
        let cache_file = tmp.path().join("primary.cache");
        let shared_progress = Arc::new(AtomicUsize::new(0));
        let mut phases = Vec::new();
        let mut on_progress = |phase: &str, count: u64| {
            phases.push((phase.to_string(), count));
        };

        let mode = initialize_primary_index_with_progress(
            &store,
            &cfg,
            &[scan_root],
            &[],
            Arc::clone(&shared_progress),
            &mut on_progress,
            &cache_file,
        )
        .unwrap();

        assert!(matches!(
            mode,
            StartupMode::ColdStart {
                scanned_count: 3,
                ..
            }
        ));
        assert_eq!(
            shared_progress.load(std::sync::atomic::Ordering::Relaxed),
            3
        );
        assert_eq!(phases, vec![("scanned".to_string(), 3)]);
    }
}
