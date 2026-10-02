use crate::cache;
use crate::config::AppConfig;
use crate::filter::CompiledFilter;
use crate::indexer::{FileEntry, IndexStore};
use crate::metrics::WatcherMetrics;
use crate::path_utils::path_starts_with_ci;
use crate::permissions::has_full_disk_access;
use crate::scanner;
use notify::event::{ModifyKind, RenameMode};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Check if a path's filename starts with '.' (hidden file).
pub fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().starts_with('.'))
        .unwrap_or(false)
}

/// Create a FileEntry from a filesystem path by reading metadata.
/// Returns None if the path doesn't exist or metadata can't be read.
pub fn file_entry_from_path(path: &Path) -> Option<FileEntry> {
    let metadata = std::fs::metadata(path).ok()?;
    let name_str = path.file_name()?.to_string_lossy();
    let extension = if metadata.is_dir() {
        String::new()
    } else {
        std::path::Path::new(name_str.as_ref())
            .extension()
            .map(|ext| ext.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    Some(FileEntry {
        name: name_str.into_owned(),
        path: path.to_string_lossy().into_owned(),
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
    })
}

/// Classified events after deduplication. Each path appears at most once.
#[derive(Debug, Default)]
pub struct EventBatch {
    pub creates: Vec<PathBuf>,
    pub removes: Vec<PathBuf>,
}

/// Classify raw notify events into creates and removes.
/// Each path may have multiple event kinds accumulated during the debounce window.
/// Uses priority-based decision per path:
///   1. RenameMode::From → remove (FSEvents unpaired rename = file gone)
///   2. RenameMode::To → create (FSEvents unpaired rename = file appeared)
///   3. Create → create
///   4. Remove → remove
///   5. Fallback: check path.exists() on disk
///
/// RenameMode::From takes highest priority because FSEvents often sends only
/// the "From" half of a rename (the "To" may land in a different debounce window
/// or be lost entirely). Treating an unpaired From as a remove ensures the stale
/// entry is cleaned up; the reconciliation timer or a later event will re-add it
/// if the file still exists under a new name.
pub fn classify_events(raw: HashMap<PathBuf, Vec<EventKind>>) -> EventBatch {
    let mut batch = EventBatch::default();
    for (path, kinds) in raw {
        let has_rename_from = kinds.iter().any(|k| {
            matches!(k, EventKind::Modify(ModifyKind::Name(RenameMode::From)))
        });
        let has_rename_to = kinds.iter().any(|k| {
            matches!(k, EventKind::Modify(ModifyKind::Name(RenameMode::To)))
        });
        let has_create = kinds.iter().any(|k| matches!(k, EventKind::Create(_)));
        let has_remove = kinds.iter().any(|k| matches!(k, EventKind::Remove(_)));

        if has_rename_from {
            batch.removes.push(path);
        } else if has_rename_to {
            batch.creates.push(path);
        } else if has_create {
            batch.creates.push(path);
        } else if has_remove {
            batch.removes.push(path);
        } else {
            // FSEvents on macOS may deliver new file events as Modify(Metadata)
            // or other non-Create kinds. Check disk to determine intent.
            if path.exists() {
                batch.creates.push(path);
            } else {
                batch.removes.push(path);
            }
        }
    }
    batch
}

/// Detect directory renames by cross-referencing removes with creates.
/// A remove is considered a directory rename if:
/// 1. The index has children with that path as prefix (proves it was a known directory)
/// 2. A create exists in the same parent directory that is a directory on disk
/// Returns pairs of (old_path_string, new_path_string).
fn detect_dir_renames(
    creates: &[PathBuf],
    removes: &[PathBuf],
    store: &IndexStore,
) -> Vec<(String, String)> {
    let mut renames = Vec::new();

    // Find creates that are directories on disk
    let dir_creates: Vec<&PathBuf> = creates.iter().filter(|p| p.is_dir()).collect();

    if dir_creates.is_empty() {
        return renames;
    }

    // Check which removes had children in the index (were directories we knew about)
    let entries = store.entries();
    let dir_removes: Vec<&PathBuf> = removes
        .iter()
        .filter(|r| {
            let prefix = format!("{}/", r.to_string_lossy());
            entries.iter().any(|e| e.path.starts_with(&prefix))
        })
        .collect();
    drop(entries); // Drop read lock before any write lock (Pitfall 3)

    // Match removes to creates by same parent directory
    let mut used_creates: HashSet<usize> = HashSet::new();
    for old_dir in &dir_removes {
        let old_parent = old_dir.parent();
        for (idx, new_dir) in dir_creates.iter().enumerate() {
            if !used_creates.contains(&idx) && new_dir.parent() == old_parent {
                renames.push((
                    old_dir.to_string_lossy().into_owned(),
                    new_dir.to_string_lossy().into_owned(),
                ));
                used_creates.insert(idx);
                break; // One match per old_dir
            }
        }
    }

    renames
}

/// Bulk path prefix rewrite for directory renames using entries_mut().
/// Updates the directory entry itself and all children whose path starts with old_prefix + "/".
/// Uses "/" boundary guard to avoid /foo/bar matching /foo/barista (Pitfall 4).
/// Returns the number of entries updated.
fn apply_dir_rename(store: &IndexStore, old_prefix: &str, new_prefix: &str) -> usize {
    let search_prefix = format!("{}/", old_prefix);
    let mut entries = store.entries_mut();
    let mut count = 0;

    for entry in entries.iter_mut() {
        // Update the directory entry itself
        if entry.path == old_prefix {
            entry.path = new_prefix.to_string();
            if let Some(name) = Path::new(new_prefix).file_name() {
                entry.name = name.to_string_lossy().into_owned();
            }
            count += 1;
        }
        // Update child entries (use "/" suffix to avoid /foo/bar matching /foo/barista)
        else if entry.path.starts_with(&search_prefix) {
            entry.path = format!("{}/{}", new_prefix, &entry.path[search_prefix.len()..]);
            count += 1;
        }
    }

    count
}

/// Process a classified event batch: build FileEntry structs OUTSIDE the lock,
/// then apply incremental updates to the IndexStore (per D-07).
/// Uses upsert semantics for creates: removes existing entries with the same path
/// before adding, to avoid duplicates when FSEvents delivers modify events for
/// files already in the index.
/// Detects directory renames and bulk-rewrites child paths before normal processing.
/// Returns the number of entries added and removed.
pub fn process_event_batch(
    mut batch: EventBatch,
    store: &IndexStore,
    filter: &CompiledFilter,
    include_hidden: bool,
) -> (usize, usize) {
    // --- Directory rename detection (before normal create/remove processing) ---
    let dir_renames = detect_dir_renames(&batch.creates, &batch.removes, store);

    for (old_path, new_path) in &dir_renames {
        // Bulk-rewrite child paths in the index
        apply_dir_rename(store, old_path, new_path);

        // Remove matched pairs from batch to avoid double-processing
        batch.removes.retain(|p| p.to_string_lossy() != *old_path);
        batch.creates.retain(|p| p.to_string_lossy() != *new_path);

        // Check if new path should be excluded — if so, remove all rewritten entries
        if filter.is_excluded(Path::new(new_path)) {
            let remove_prefix = format!("{}/", new_path);
            let paths_to_remove: HashSet<String> = {
                let entries = store.entries();
                entries
                    .iter()
                    .filter(|e| e.path == *new_path || e.path.starts_with(&remove_prefix))
                    .map(|e| e.path.clone())
                    .collect()
            };
            if !paths_to_remove.is_empty() {
                store.remove_paths(&paths_to_remove);
            }
        }
    }

    // Filter and build new entries OUTSIDE the lock (per D-07)
    let new_entries: Vec<FileEntry> = batch
        .creates
        .iter()
        .filter(|p| !filter.is_excluded(p))
        .filter(|p| include_hidden || !is_hidden(p))
        .filter_map(|p| file_entry_from_path(p))
        .collect();

    // Build removal set: explicit removes + paths being upserted (to avoid duplicates)
    let mut remove_set: HashSet<String> = batch
        .removes
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    // Cascade: for each remove path, also remove all child entries in the index.
    // This handles the case where a directory rename's detect_dir_renames() pairing
    // fails (unpaired From event), so the directory remove correctly cleans up
    // all descendant entries instead of leaving them as orphans.
    {
        let entries = store.entries();
        let mut cascade_paths: Vec<String> = Vec::new();
        for remove_path in &remove_set.clone() {
            let prefix = format!("{}/", remove_path);
            for entry in entries.iter() {
                if entry.path.starts_with(&prefix) {
                    cascade_paths.push(entry.path.clone());
                }
            }
        }
        drop(entries);
        for p in cascade_paths {
            remove_set.insert(p);
        }
    }

    // Add create paths to removal set for upsert semantics
    for entry in &new_entries {
        remove_set.insert(entry.path.clone());
    }

    let added = new_entries.len();

    // Count and perform actual removals
    let removed = if remove_set.is_empty() {
        0
    } else {
        let current = store.entries();
        let count = current.iter().filter(|e| remove_set.contains(&e.path)).count();
        drop(current); // Drop read lock before write (Pitfall 5)
        if count > 0 {
            store.remove_paths(&remove_set);
        }
        count
    };

    if !new_entries.is_empty() {
        store.add_entries(new_entries);
    }

    (added, removed)
}

// ── Dirty Queue: Coalescing ────────────────────────────────────────────────

const DEFAULT_DEBOUNCE_MS: u64 = 500;

/// A queue that coalesces file events before they enter the debounce/processing pipeline.
///
/// Coalescing rules:
/// - Same path appearing multiple times → merged (existing HashMap behaviour)
/// - If a directory path is dirty, child paths are skipped (directory rescan covers them)
/// - If queue exceeds max_batch, flushed immediately to prevent unbounded growth
struct DirtyQueue {
    paths: HashMap<PathBuf, Vec<EventKind>>,
    dir_paths: HashSet<PathBuf>,
}

impl DirtyQueue {
    fn new() -> Self {
        Self {
            paths: HashMap::new(),
            dir_paths: HashSet::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    #[allow(dead_code)]
    fn len(&self) -> usize {
        self.paths.len()
    }

    /// Insert a path + event kind into the queue, applying coalescing rules.
    ///
    /// Returns true if the entry was accepted (not coalesced away).
    fn insert(&mut self, path: PathBuf, kind: EventKind) -> bool {
        // Check if any parent directory is already dirty → coalesce (skip child)
        for ancestor in path.ancestors().skip(1) {
            if self.dir_paths.contains(ancestor) {
                return false; // Parent dir is dirty, will be rescanned
            }
        }

        let is_dir = matches!(kind, EventKind::Create(notify::event::CreateKind::Folder))
            || path.is_dir();

        if is_dir {
            self.dir_paths.insert(path.clone());
            // Remove any existing children of this directory (now covered by dir rescan)
            let prefix = format!("{}/", path.to_string_lossy());
            self.paths.retain(|p, _| {
                !p.starts_with(&prefix) && p != &path
            });
            self.dir_paths.retain(|p| {
                p == &path || !p.starts_with(&prefix)
            });
        }

        self.paths.entry(path).or_default().push(kind);
        true
    }

    /// Take all accumulated events, leaving the queue empty.
    fn take(&mut self) -> HashMap<PathBuf, Vec<EventKind>> {
        self.dir_paths.clear();
        std::mem::take(&mut self.paths)
    }
}

// ── Safe Watch Paths ───────────────────────────────────────────────────────

fn collect_safe_watch_paths(
    path: &Path,
    filter: &CompiledFilter,
    protected: &[PathBuf],
    out: &mut Vec<PathBuf>,
) {
    if !path.exists() {
        return;
    }

    if scanner::is_protected_or_descendant(path, protected) {
        return;
    }

    if filter.is_under_excluded_prefix(path) {
        return;
    }

    // Check if this directory contains any protected or excluded descendants
    let has_protected = scanner::contains_protected_descendant(path, protected);
    let has_excluded = filter
        .excluded_prefixes()
        .iter()
        .any(|p| path_starts_with_ci(p, path));

    if has_protected || has_excluded {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                collect_safe_watch_paths(&entry.path(), filter, protected, out);
            }
        }
        return;
    }

    out.push(path.to_path_buf());
}

fn safe_watch_paths(paths: &[PathBuf], filter: &CompiledFilter, protected: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        collect_safe_watch_paths(path, filter, protected, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

/// Start the file watcher on a dedicated thread.
/// Returns a channel sender that can be used to signal shutdown (drop to stop).
///
/// The watcher thread:
/// 1. Creates a RecommendedWatcher (FSEvents on macOS) per D-01
/// 2. Watches all resolved config paths recursively per D-02
/// 3. Runs a debounce loop with DirtyQueue coalescing
/// 4. Processes batched events and updates the IndexStore per D-03
/// 5. Calls the on_batch callback after each processed batch
pub fn start_watcher<F>(
    store: IndexStore,
    config: AppConfig,
    metrics: Arc<WatcherMetrics>,
    on_batch: F,
) -> Result<(), notify::Error>
where
    F: Fn(usize) + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Event>();
    let paths = config.resolve_paths();

    let filter = Arc::new(CompiledFilter::from_config(&config));

    let has_fda = has_full_disk_access();
    let protected = if has_fda {
        vec![]
    } else {
        scanner::protected_paths()
    };
    let watch_paths = if has_fda {
        paths.clone()
    } else {
        safe_watch_paths(&paths, &filter, &protected)
    };

    let mut watcher = RecommendedWatcher::new(
        {
            let metrics = Arc::clone(&metrics);
            let filter = Arc::clone(&filter);
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    metrics.events_received.fetch_add(1, Ordering::Relaxed);

                    // Level 1: fast filter at callback top level.
                    // Excluded events never enter the channel or debounce queue.
                    if event.paths.iter().all(|p| filter.is_excluded(p)) {
                        metrics.events_dropped_excluded.fetch_add(1, Ordering::Relaxed);
                        return;
                    }

                    metrics.events_enqueued.fetch_add(1, Ordering::Relaxed);
                    let _ = tx.send(event);
                }
            }
        },
        Config::default(),
    )?;

    for path in &watch_paths {
        watcher.watch(path, RecursiveMode::Recursive).ok();
    }

    let include_hidden = config.index.include_hidden;

    std::thread::spawn(move || {
        let _watcher = watcher; // Keep watcher alive (Pitfall 3)
        let mut dirty = DirtyQueue::new();
        let mut first_event_time: Option<Instant> = None;
        const MAX_DEBOUNCE_MS: u64 = 2000;
        const MAX_BATCH_SIZE: usize = 10000;

        loop {
            let timeout = if let Some(first) = first_event_time {
                let elapsed = first.elapsed();
                let max = Duration::from_millis(MAX_DEBOUNCE_MS);
                if elapsed >= max {
                    Duration::from_millis(0)
                } else {
                    Duration::from_millis(DEFAULT_DEBOUNCE_MS).min(max - elapsed)
                }
            } else {
                Duration::from_millis(DEFAULT_DEBOUNCE_MS)
            };

            let mut should_flush = false;

            match rx.recv_timeout(timeout) {
                Ok(event) => {
                    if first_event_time.is_none() {
                        first_event_time = Some(Instant::now());
                    }

                    let mut handled_as_both = false;
                    // Handle Rename(Both) specially: paths[0]=old, paths[1]=new
                    if matches!(
                        event.kind,
                        EventKind::Modify(ModifyKind::Name(RenameMode::Both))
                    ) {
                        if event.paths.len() >= 2 {
                            let old_path = event.paths[0].clone();
                            if has_fda || !scanner::is_protected_or_descendant(&old_path, &protected)
                            {
                                dirty.insert(
                                    old_path,
                                    EventKind::Modify(ModifyKind::Name(RenameMode::From)),
                                );
                            }

                            let new_path = event.paths[1].clone();
                            if has_fda || !scanner::is_protected_or_descendant(&new_path, &protected)
                            {
                                dirty.insert(
                                    new_path,
                                    EventKind::Modify(ModifyKind::Name(RenameMode::To)),
                                );
                            }
                            handled_as_both = true;
                        }
                    }
                    
                    if !handled_as_both {
                        // Accumulate all event kinds per path (with coalescing)
                        for path in event.paths {
                            if has_fda || !scanner::is_protected_or_descendant(&path, &protected) {
                                dirty.insert(path, event.kind.clone());
                            }
                        }
                    }

                    if dirty.len() >= MAX_BATCH_SIZE {
                        should_flush = true;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    should_flush = true;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }

            if should_flush {
                if !dirty.is_empty() {
                    let raw = dirty.take();
                    let batch = classify_events(raw);
                    let (_added, _removed) =
                        process_event_batch(batch, &store, &filter, include_hidden);
                    metrics.batches_processed.fetch_add(1, Ordering::Relaxed);
                    let new_total = store.len();
                    on_batch(new_total);
                }
                first_event_time = None;
            }
        }
    });

    Ok(())
}

/// Start a periodic reconciliation timer on a dedicated thread.
/// First scan at 5 minutes after startup, then every 6 hours.
/// Reconciliation: full rescan of configured roots only, diff against current index.
/// Calls on_start() before each scan and on_done(new_total) after each scan.
pub fn start_reconciliation_timer<S, D>(
    store: IndexStore,
    config: AppConfig,
    on_start: S,
    on_done: D,
) where
    S: Fn() + Send + 'static,
    D: Fn(usize) + Send + 'static,
{
    std::thread::spawn(move || {
        // Initial delay: 5 minutes
        std::thread::sleep(Duration::from_secs(5 * 60));

        loop {
            on_start();

            let paths = config.resolve_paths();
            let progress = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let filter = Arc::new(CompiledFilter::from_config(&config));
            let fresh = scanner::scan_directories(
                &paths,
                filter,
                config.index.include_hidden,
                progress,
            );

            // Build diff (per D-09)
            let fresh_set: HashSet<String> = fresh.iter().map(|e| e.path.clone()).collect();
            let current = store.entries();
            let current_set: HashSet<String> = current.iter().map(|e| e.path.clone()).collect();

            let added: Vec<FileEntry> = fresh
                .into_iter()
                .filter(|e| !current_set.contains(&e.path))
                .collect();
            let removed: HashSet<String> = current_set
                .into_iter()
                .filter(|p| !fresh_set.contains(p))
                .collect();

            drop(current); // Release read lock before write (Pitfall 5)

            if !added.is_empty() || !removed.is_empty() {
                if !removed.is_empty() {
                    store.remove_paths(&removed);
                }
                if !added.is_empty() {
                    store.add_entries(added);
                }
            }

            // Save cache after reconciliation
            let cache_file = AppConfig::config_dir().join("index.cache");
            let entries = store.entries();
            let _ = cache::save_cache(&entries, &cache_file);
            drop(entries);

            let new_total = store.len();
            on_done(new_total);

            // Wait 6 hours until next reconciliation
            std::thread::sleep(Duration::from_secs(6 * 60 * 60));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, RemoveKind};
    use std::fs;
    use tempfile::TempDir;

    
    // --- is_hidden tests ---

    #[test]
    fn test_is_hidden_true() {
        assert!(is_hidden(Path::new("/Users/test/.hidden")));
    }

    #[test]
    fn test_is_hidden_false() {
        assert!(!is_hidden(Path::new("/Users/test/visible")));
    }

    #[test]
    fn test_is_hidden_dot_dir() {
        assert!(is_hidden(Path::new("/Users/test/.config")));
    }

    // --- file_entry_from_path tests ---

    #[test]
    fn test_file_entry_from_path_existing_file() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.txt");
        fs::write(&file_path, "hello world").unwrap();

        let entry = file_entry_from_path(&file_path);
        assert!(entry.is_some());
        let entry = entry.unwrap();
        assert_eq!(entry.name, "test.txt");
        assert!(!entry.is_dir);
        assert_eq!(entry.size, 11); // "hello world" is 11 bytes
        assert!(entry.modified > 0);
        assert!(entry.path.ends_with("test.txt"));
    }

    #[test]
    fn test_file_entry_from_path_nonexistent() {
        let entry = file_entry_from_path(Path::new("/nonexistent/path/abc.txt"));
        assert!(entry.is_none());
    }

    #[test]
    fn test_file_entry_from_path_directory() {
        let tmp = TempDir::new().unwrap();
        let dir_path = tmp.path().join("subdir");
        fs::create_dir(&dir_path).unwrap();

        let entry = file_entry_from_path(&dir_path);
        assert!(entry.is_some());
        let entry = entry.unwrap();
        assert_eq!(entry.name, "subdir");
        assert!(entry.is_dir);
    }

    // --- classify_events tests ---

    #[test]
    fn test_classify_creates() {
        let mut raw = HashMap::new();
        raw.insert(
            PathBuf::from("/tmp/new.txt"),
            vec![EventKind::Create(CreateKind::File)],
        );
        let batch = classify_events(raw);
        assert_eq!(batch.creates.len(), 1);
        assert_eq!(batch.removes.len(), 0);
    }

    #[test]
    fn test_classify_removes() {
        let mut raw = HashMap::new();
        raw.insert(
            PathBuf::from("/tmp/old.txt"),
            vec![EventKind::Remove(RemoveKind::File)],
        );
        let batch = classify_events(raw);
        assert_eq!(batch.creates.len(), 0);
        assert_eq!(batch.removes.len(), 1);
    }

    #[test]
    fn test_classify_rename_from_to() {
        let mut raw = HashMap::new();
        raw.insert(
            PathBuf::from("/tmp/old_name.txt"),
            vec![EventKind::Modify(ModifyKind::Name(RenameMode::From))],
        );
        raw.insert(
            PathBuf::from("/tmp/new_name.txt"),
            vec![EventKind::Modify(ModifyKind::Name(RenameMode::To))],
        );
        let batch = classify_events(raw);
        assert_eq!(batch.removes.len(), 1);
        assert_eq!(batch.creates.len(), 1);
    }

    #[test]
    fn test_classify_modify_metadata_existing_file() {
        // FSEvents on macOS may deliver file creation as Modify(Metadata(Any)).
        // If the path exists on disk, it should be classified as a create.
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("new_via_modify.txt");
        fs::write(&file_path, "data").unwrap();

        let mut raw = HashMap::new();
        raw.insert(
            file_path,
            vec![EventKind::Modify(ModifyKind::Any)],
        );
        let batch = classify_events(raw);
        assert_eq!(batch.creates.len(), 1);
        assert_eq!(batch.removes.len(), 0);
    }

    #[test]
    fn test_classify_modify_metadata_nonexistent_file() {
        // If the path does NOT exist on disk, a Modify event should be treated as a remove.
        let mut raw = HashMap::new();
        raw.insert(
            PathBuf::from("/nonexistent/path/ghost.txt"),
            vec![EventKind::Modify(ModifyKind::Any)],
        );
        let batch = classify_events(raw);
        assert_eq!(batch.creates.len(), 0);
        assert_eq!(batch.removes.len(), 1);
    }

    #[test]
    fn test_process_event_batch_upsert_no_duplicates() {
        // When a file already exists in the index and a create event arrives,
        // upsert semantics should update (not duplicate) the entry.
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("existing.txt");
        fs::write(&file_path, "original").unwrap();

        let store = IndexStore::new();
        store.add_entries(vec![FileEntry {
            name: "existing.txt".to_string(),
            path: file_path.to_string_lossy().into_owned(),
            is_dir: false,
            size: 8,
            modified: 1700000000,
            created_at: 0,
            extension: "txt".to_string(),
        }]);
        assert_eq!(store.len(), 1);

        // Simulate a modify event that triggers upsert
        let batch = EventBatch {
            creates: vec![file_path],
            removes: vec![],
        };
        let (added, removed) = process_event_batch(batch, &store, &dummy_filter(), true);
        assert_eq!(added, 1);
        assert_eq!(removed, 1); // old entry removed via upsert
        assert_eq!(store.len(), 1); // no duplicate
    }

    // --- process_event_batch tests ---

    #[test]
    fn test_process_event_batch_creates() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("new_file.txt");
        fs::write(&file_path, "data").unwrap();

        let store = IndexStore::new();
        let batch = EventBatch {
            creates: vec![file_path],
            removes: vec![],
        };

        let (added, removed) = process_event_batch(batch, &store, &dummy_filter(), true);
        assert_eq!(added, 1);
        assert_eq!(removed, 0);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn test_process_event_batch_removes() {
        let store = IndexStore::new();
        store.add_entries(vec![FileEntry {
            name: "old.txt".to_string(),
            path: "/tmp/old.txt".to_string(),
            is_dir: false,
            size: 100,
            modified: 1700000000,
            created_at: 0,
            extension: "txt".to_string(),
        }]);
        assert_eq!(store.len(), 1);

        let batch = EventBatch {
            creates: vec![],
            removes: vec![PathBuf::from("/tmp/old.txt")],
        };

        let (added, removed) = process_event_batch(batch, &store, &dummy_filter(), true);
        assert_eq!(added, 0);
        assert_eq!(removed, 1);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_process_event_batch_filters_excluded() {
        let tmp = TempDir::new().unwrap();
        let nm_dir = tmp.path().join("node_modules");
        fs::create_dir(&nm_dir).unwrap();
        let file_path = nm_dir.join("pkg.js");
        fs::write(&file_path, "module.exports = {}").unwrap();

        let store = IndexStore::new();
        let batch = EventBatch {
            creates: vec![file_path],
            removes: vec![],
        };

        let (added, _removed) =
            process_event_batch(batch, &store, &dummy_filter(), true);
        assert_eq!(added, 0);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_process_event_batch_filters_hidden() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join(".hidden_file");
        fs::write(&file_path, "secret").unwrap();

        let store = IndexStore::new();
        let batch = EventBatch {
            creates: vec![file_path],
            removes: vec![],
        };

        // include_hidden = false should filter out hidden files
        let (added, _removed) = process_event_batch(batch, &store, &dummy_filter(), false);
        assert_eq!(added, 0);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_process_event_batch_includes_hidden_when_configured() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join(".hidden_file");
        fs::write(&file_path, "secret").unwrap();

        let store = IndexStore::new();
        let batch = EventBatch {
            creates: vec![file_path],
            removes: vec![],
        };

        // include_hidden = true should keep hidden files
        let (added, _removed) = process_event_batch(batch, &store, &dummy_filter(), true);
        assert_eq!(added, 1);
        assert_eq!(store.len(), 1);
    }

    // --- directory rename tests ---

    /// Helper to create a FileEntry for tests (mirrors indexer::tests::make_entry pattern)
    fn dummy_filter() -> crate::filter::CompiledFilter { let mut cfg = crate::config::AppConfig::default(); cfg.index.index_paths = vec![]; crate::filter::CompiledFilter::from_config(&cfg) }

    fn make_entry(name: &str, path: &str, is_dir: bool) -> FileEntry {
        let extension = if is_dir {
            String::new()
        } else {
            std::path::Path::new(name)
                .extension()
                .map(|ext| ext.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        };
        FileEntry {
            name: name.to_string(),
            path: path.to_string(),
            is_dir,
            size: 1024,
            modified: 1700000000,
            created_at: 0,
            extension,
        }
    }

    #[test]
    fn test_dir_rename_updates_child_paths() {
        // Use TempDir as parent so old and new dirs share the same parent (like real renames)
        let tmp = TempDir::new().unwrap();
        let old_dir = tmp.path().join("mydir");
        let new_dir = tmp.path().join("newdir");
        // Create new dir on disk (file_entry_from_path needs it); old dir doesn't need to exist
        fs::create_dir(&new_dir).unwrap();

        let old_prefix = old_dir.to_string_lossy().to_string();
        let new_prefix = new_dir.to_string_lossy().to_string();

        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("mydir", &old_prefix, true),
            make_entry("a.txt", &format!("{}/a.txt", old_prefix), false),
            make_entry("b.txt", &format!("{}/sub/b.txt", old_prefix), false),
        ]);

        let batch = EventBatch {
            removes: vec![old_dir],
            creates: vec![new_dir],
        };

        process_event_batch(batch, &store, &dummy_filter(), true);

        let entries = store.entries();
        assert!(
            entries.iter().any(|e| e.path == new_prefix),
            "Directory entry should have new path"
        );
        assert!(
            entries.iter().any(|e| e.path == format!("{}/a.txt", new_prefix)),
            "Child a.txt should have new prefix"
        );
        assert!(
            entries.iter().any(|e| e.path == format!("{}/sub/b.txt", new_prefix)),
            "Child b.txt should have new prefix"
        );
        assert!(
            !entries.iter().any(|e| e.path.contains("mydir")),
            "No entries should contain old 'mydir' prefix"
        );
    }

    #[test]
    fn test_dir_rename_no_stale_entries() {
        let tmp = TempDir::new().unwrap();
        let old_dir = tmp.path().join("mydir");
        let new_dir = tmp.path().join("newdir");
        fs::create_dir(&new_dir).unwrap();

        let old_prefix = old_dir.to_string_lossy().to_string();

        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("mydir", &old_prefix, true),
            make_entry("a.txt", &format!("{}/a.txt", old_prefix), false),
            make_entry("b.txt", &format!("{}/sub/b.txt", old_prefix), false),
        ]);

        let batch = EventBatch {
            removes: vec![old_dir],
            creates: vec![new_dir],
        };

        process_event_batch(batch, &store, &dummy_filter(), true);

        // 3 entries in, 3 entries out (rewritten in-place, not removed+re-added)
        assert_eq!(store.len(), 3, "Entry count should remain 3 after rename");

        let entries = store.entries();
        assert!(
            !entries.iter().any(|e| e.path.contains("mydir")),
            "No stale entries with old prefix"
        );
    }

    #[test]
    fn test_dir_rename_nested() {
        // Deep nesting: parent/a/b/c/file.txt — rename parent/a to parent/z
        let tmp = TempDir::new().unwrap();
        let old_dir = tmp.path().join("a");
        let new_dir = tmp.path().join("z");
        fs::create_dir(&new_dir).unwrap();

        let old_prefix = old_dir.to_string_lossy().to_string();
        let new_prefix = new_dir.to_string_lossy().to_string();

        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("a", &old_prefix, true),
            make_entry("b", &format!("{}/b", old_prefix), true),
            make_entry("c", &format!("{}/b/c", old_prefix), true),
            make_entry("file.txt", &format!("{}/b/c/file.txt", old_prefix), false),
        ]);

        let batch = EventBatch {
            removes: vec![old_dir],
            creates: vec![new_dir],
        };

        process_event_batch(batch, &store, &dummy_filter(), true);

        let entries = store.entries();
        assert!(
            entries.iter().any(|e| e.path == format!("{}/b/c/file.txt", new_prefix)),
            "Deeply nested file should have updated path"
        );
        assert!(
            !entries.iter().any(|e| e.path.contains("/a/")),
            "No entries should contain old /a/ prefix"
        );
    }

    #[test]
    fn test_dir_rename_to_excluded_removes_children() {
        // Rename dir to "node_modules" (excluded) — all children should be removed
        let tmp = TempDir::new().unwrap();
        let old_dir = tmp.path().join("mydir");
        let new_dir = tmp.path().join("node_modules");
        fs::create_dir(&new_dir).unwrap();

        let old_prefix = old_dir.to_string_lossy().to_string();

        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("mydir", &old_prefix, true),
            make_entry("a.txt", &format!("{}/a.txt", old_prefix), false),
            make_entry("b.txt", &format!("{}/b.txt", old_prefix), false),
        ]);

        let batch = EventBatch {
            removes: vec![old_dir],
            creates: vec![new_dir],
        };

        process_event_batch(batch, &store, &dummy_filter(), true);

        assert_eq!(
            store.len(),
            0,
            "All entries should be removed when renamed to excluded directory"
        );
    }

    #[test]
    fn test_prefix_boundary_no_false_match() {
        // parent/bar + child, AND parent/barista/y.txt — rename parent/bar to parent/baz
        // barista entry must NOT be affected
        let tmp = TempDir::new().unwrap();
        let old_dir = tmp.path().join("bar");
        let new_dir = tmp.path().join("baz");
        fs::create_dir(&new_dir).unwrap();

        let old_prefix = old_dir.to_string_lossy().to_string();
        let new_prefix = new_dir.to_string_lossy().to_string();
        let barista_path = format!("{}/y.txt", tmp.path().join("barista").to_string_lossy());

        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("bar", &old_prefix, true),
            make_entry("x.txt", &format!("{}/x.txt", old_prefix), false),
            make_entry("y.txt", &barista_path, false),
        ]);

        let batch = EventBatch {
            removes: vec![old_dir],
            creates: vec![new_dir],
        };

        process_event_batch(batch, &store, &dummy_filter(), true);

        let entries = store.entries();

        // bar entries updated
        assert!(
            entries.iter().any(|e| e.path == new_prefix),
            "bar dir should be renamed to baz"
        );
        assert!(
            entries.iter().any(|e| e.path == format!("{}/x.txt", new_prefix)),
            "bar/x.txt should be renamed"
        );

        // barista entry unchanged
        assert!(
            entries.iter().any(|e| e.path == barista_path),
            "barista/y.txt should be unaffected"
        );
    }

    #[test]
    fn test_file_rename_still_works() {
        // Simple file rename — existing behavior must be preserved
        let tmp = TempDir::new().unwrap();
        let new_file = tmp.path().join("new.txt");
        fs::write(&new_file, "content").unwrap();

        let store = IndexStore::new();
        store.add_entries(vec![make_entry("old.txt", "/tmp/old.txt", false)]);

        let batch = EventBatch {
            removes: vec![PathBuf::from("/tmp/old.txt")],
            creates: vec![new_file.clone()],
        };

        process_event_batch(batch, &store, &dummy_filter(), true);

        let entries = store.entries();
        let new_path = new_file.to_string_lossy().to_string();
        assert!(
            entries.iter().any(|e| e.path == new_path),
            "New file should be in index"
        );
        assert!(
            !entries.iter().any(|e| e.path == "/tmp/old.txt"),
            "Old file should be removed from index"
        );
    }

    // --- New tests for FSEvents rename fix ---

    #[test]
    fn test_cascade_remove_directory_children() {
        // When a directory is removed, all children in the index should also be removed.
        // This handles the case where detect_dir_renames() pairing fails.
        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("mydir", "/tmp/mydir", true),
            make_entry("a.txt", "/tmp/mydir/a.txt", false),
            make_entry("sub", "/tmp/mydir/sub", true),
            make_entry("b.txt", "/tmp/mydir/sub/b.txt", false),
            make_entry("other.txt", "/tmp/other.txt", false),
        ]);
        assert_eq!(store.len(), 5);

        let batch = EventBatch {
            creates: vec![],
            removes: vec![PathBuf::from("/tmp/mydir")],
        };

        let (_added, removed) = process_event_batch(batch, &store, &dummy_filter(), true);
        // Should remove mydir + 3 children = 4 total
        assert_eq!(removed, 4);
        assert_eq!(store.len(), 1);
        let entries = store.entries();
        assert_eq!(entries[0].path, "/tmp/other.txt");
    }

    #[test]
    fn test_unpaired_rename_from_cleans_up() {
        // FSEvents sends only RenameMode::From without a matching To.
        // The old path and its children should be fully removed from the index.
        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("proj", "/tmp/proj", true),
            make_entry("main.rs", "/tmp/proj/main.rs", false),
        ]);

        let mut raw = HashMap::new();
        raw.insert(
            PathBuf::from("/tmp/proj"),
            vec![EventKind::Modify(ModifyKind::Name(RenameMode::From))],
        );
        let batch = classify_events(raw);

        // Should classify as remove
        assert_eq!(batch.removes.len(), 1);
        assert_eq!(batch.creates.len(), 0);

        // Processing should cascade-remove the child too
        let (_added, removed) = process_event_batch(batch, &store, &dummy_filter(), true);
        assert_eq!(removed, 2);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_classify_multi_events_rename_from_wins() {
        // Same path gets both Modify(Any) and RenameMode::From in one debounce window.
        // RenameMode::From should take priority → remove.
        let mut raw = HashMap::new();
        raw.insert(
            PathBuf::from("/tmp/moved.txt"),
            vec![
                EventKind::Modify(ModifyKind::Any),
                EventKind::Modify(ModifyKind::Name(RenameMode::From)),
            ],
        );
        let batch = classify_events(raw);
        assert_eq!(batch.removes.len(), 1);
        assert_eq!(batch.creates.len(), 0);
    }

    #[test]
    fn test_classify_multi_events_rename_to_wins_over_create() {
        // Same path gets both Create and RenameMode::To.
        // RenameMode::To has higher priority → still create.
        let mut raw = HashMap::new();
        raw.insert(
            PathBuf::from("/tmp/arrived.txt"),
            vec![
                EventKind::Create(CreateKind::File),
                EventKind::Modify(ModifyKind::Name(RenameMode::To)),
            ],
        );
        let batch = classify_events(raw);
        assert_eq!(batch.creates.len(), 1);
        assert_eq!(batch.removes.len(), 0);
    }

    #[test]
    fn test_classify_rename_from_overrides_create() {
        // Same path gets Create then RenameMode::From (file created then immediately moved away).
        // RenameMode::From should win → remove.
        let mut raw = HashMap::new();
        raw.insert(
            PathBuf::from("/tmp/transient.txt"),
            vec![
                EventKind::Create(CreateKind::File),
                EventKind::Modify(ModifyKind::Name(RenameMode::From)),
            ],
        );
        let batch = classify_events(raw);
        assert_eq!(batch.removes.len(), 1);
        assert_eq!(batch.creates.len(), 0);
    }

    #[test]
    fn test_cascade_remove_respects_prefix_boundary() {
        // /tmp/bar removal should NOT cascade to /tmp/barista/file.txt
        let store = IndexStore::new();
        store.add_entries(vec![
            make_entry("bar", "/tmp/bar", true),
            make_entry("x.txt", "/tmp/bar/x.txt", false),
            make_entry("y.txt", "/tmp/barista/y.txt", false),
        ]);

        let batch = EventBatch {
            creates: vec![],
            removes: vec![PathBuf::from("/tmp/bar")],
        };

        process_event_batch(batch, &store, &dummy_filter(), true);
        assert_eq!(store.len(), 1);
        let entries = store.entries();
        assert_eq!(entries[0].path, "/tmp/barista/y.txt");
    }

    // --- DirtyQueue coalescing tests ---

    #[test]
    fn test_dirty_queue_insert_basic() {
        let mut q = DirtyQueue::new();
        assert!(q.insert(PathBuf::from("/tmp/a.txt"), EventKind::Create(CreateKind::File)));
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn test_dirty_queue_coalesce_same_path() {
        let mut q = DirtyQueue::new();
        q.insert(PathBuf::from("/tmp/a.txt"), EventKind::Create(CreateKind::File));
        q.insert(PathBuf::from("/tmp/a.txt"), EventKind::Modify(ModifyKind::Any));
        // Same path should merge (still 1 entry, but with multiple event kinds)
        assert_eq!(q.len(), 1);
        let raw = q.take();
        assert_eq!(raw.get(&PathBuf::from("/tmp/a.txt")).unwrap().len(), 2);
    }

    #[test]
    fn test_dirty_queue_coalesce_child_under_dirty_dir() {
        let mut q = DirtyQueue::new();
        // Insert a directory event first
        q.insert(
            PathBuf::from("/tmp/mydir"),
            EventKind::Create(CreateKind::Folder),
        );
        // Child file under dirty dir should be coalesced (skipped)
        assert!(!q.insert(
            PathBuf::from("/tmp/mydir/a.txt"),
            EventKind::Create(CreateKind::File),
        ));
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn test_dirty_queue_dir_clears_existing_children() {
        let mut q = DirtyQueue::new();
        // Insert child first
        q.insert(PathBuf::from("/tmp/mydir/a.txt"), EventKind::Create(CreateKind::File));
        assert_eq!(q.len(), 1);
        // Then insert parent dir → should remove child
        q.insert(
            PathBuf::from("/tmp/mydir"),
            EventKind::Create(CreateKind::Folder),
        );
        assert_eq!(q.len(), 1);
        let raw = q.take();
        assert!(raw.contains_key(&PathBuf::from("/tmp/mydir")));
        assert!(!raw.contains_key(&PathBuf::from("/tmp/mydir/a.txt")));
    }

    #[test]
    fn test_dirty_queue_no_coalesce_sibling() {
        let mut q = DirtyQueue::new();
        q.insert(PathBuf::from("/tmp/mydir"), EventKind::Create(CreateKind::Folder));
        // Sibling dir (different parent) should NOT be coalesced
        assert!(q.insert(
            PathBuf::from("/tmp/other"),
            EventKind::Create(CreateKind::Folder),
        ));
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn test_dirty_queue_take_clears() {
        let mut q = DirtyQueue::new();
        q.insert(PathBuf::from("/tmp/a.txt"), EventKind::Create(CreateKind::File));
        assert_eq!(q.len(), 1);
        let raw = q.take();
        assert_eq!(raw.len(), 1);
        assert!(q.is_empty());
    }

    #[test]
    fn test_dirty_queue_max_batch_flush_semantics() {
        let mut q = DirtyQueue::new();
        q.insert(PathBuf::from("/tmp/a.txt"), EventKind::Create(CreateKind::File));
        q.insert(PathBuf::from("/tmp/b.txt"), EventKind::Create(CreateKind::File));
        q.insert(PathBuf::from("/tmp/c.txt"), EventKind::Create(CreateKind::File));
        assert_eq!(q.len(), 3);
        // Next insert triggers no auto-flush (caller decides),
        // but we can verify via len
        q.insert(PathBuf::from("/tmp/d.txt"), EventKind::Create(CreateKind::File));
        assert_eq!(q.len(), 4);
    }
}
