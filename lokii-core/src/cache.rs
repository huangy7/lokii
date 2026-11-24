use crate::indexer::FileEntry;

use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

const CACHE_MAGIC: &[u8; 4] = b"LOKI";
/// Version of the legacy `Vec<FileEntry>` payload.
const CACHE_VERSION: u32 = 3;
/// Version of the compact index payload (shared string tables plus records).
const CACHE_VERSION_COMPACT: u32 = 4;
/// Compact payload preceded by a 16-byte [`Checkpoint`] (event id 0 = none).
const CACHE_VERSION_CHECKPOINT: u32 = 5;

/// What the cached snapshot is known to reflect: every FSEvents event up to
/// `event_id`, scanned under the config/permission state hashed into
/// `fingerprint`. Replaying from it is only valid while the fingerprint matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub event_id: u64,
    pub fingerprint: u64,
}

/// Errors that can occur during cache operations.
#[derive(Debug)]
pub enum CacheError {
    /// I/O error (e.g., missing file, permission denied).
    Io(std::io::Error),
    /// Cache file is too small to contain a valid header.
    TooSmall,
    /// Cache file does not start with the expected magic bytes.
    BadMagic,
    /// Cache file version does not match the expected version.
    VersionMismatch {
        found: u32,
        expected: u32,
    },
    /// Failed to decode the cache payload.
    Decode(bitcode::Error),
    InvalidIndex(crate::compact_index::IndexError),
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheError::Io(e) => write!(f, "cache I/O error: {e}"),
            CacheError::TooSmall => write!(f, "cache file too small for valid header"),
            CacheError::BadMagic => write!(f, "cache file has invalid magic bytes"),
            CacheError::VersionMismatch { found, expected } => {
                write!(
                    f,
                    "cache version mismatch: found {found}, expected {expected}"
                )
            }
            CacheError::Decode(e) => write!(f, "cache decode error: {e}"),
            CacheError::InvalidIndex(error) => write!(f, "invalid cached index: {error}"),
        }
    }
}

impl std::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CacheError::Io(e) => Some(e),
            CacheError::Decode(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for CacheError {
    fn from(e: std::io::Error) -> Self {
        CacheError::Io(e)
    }
}

/// Return the default cache file path: ~/.config/lokii/index.cache
pub fn cache_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".config")
        .join("lokii")
        .join("index.cache")
}

/// Save file entries to a cache file with a version header.
///
/// Uses atomic write: writes to a .tmp file first, then renames to final path
/// to prevent corruption if the process crashes mid-write.
pub fn save_cache(entries: &[FileEntry], path: &Path) -> Result<(), CacheError> {
    write_cache(CACHE_VERSION, &[&bitcode::encode(entries)], path)
}

/// Load file entries from a cache file, validating the version header.
///
/// Returns errors for: missing file, too small, bad magic, version mismatch, corrupt payload.
/// Never panics on invalid input.
pub fn load_cache(path: &Path) -> Result<Vec<FileEntry>, CacheError> {
    let (version, data) = read_cache(path)?;
    if version != CACHE_VERSION {
        return Err(CacheError::VersionMismatch {
            found: version,
            expected: CACHE_VERSION,
        });
    }
    bitcode::decode(&data[8..]).map_err(CacheError::Decode)
}

/// 仅在内存中将紧凑索引编码为缓存字节流，便于调用方尽快释放索引读锁后再执行磁盘写入与 `sync_data()`。
pub fn encode_compact(index: &crate::compact_index::CompactIndex) -> Vec<u8> {
    index.encode_cache()
}

/// Resolves a stable lock key even when leaf parent directories do not exist yet,
/// by canonicalizing the nearest existing ancestor and re-appending missing components.
fn canonical_lock_key(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(path)
    } else {
        path.to_path_buf()
    };

    let mut pending_components = Vec::new();
    let mut cursor = abs.as_path();
    loop {
        if let Ok(canon) = fs::canonicalize(cursor) {
            let mut key = canon;
            for comp in pending_components.into_iter().rev() {
                key.push(comp);
            }
            return key;
        }
        match (cursor.parent(), cursor.file_name()) {
            (Some(parent), Some(name)) if parent != cursor => {
                pending_components.push(name.to_os_string());
                cursor = parent;
            }
            _ => return abs,
        }
    }
}

/// Serialize concurrent saves targeting the exact same cache path while allowing
/// distinct shard files to flush and `sync_data()` in parallel.
pub(crate) fn path_write_lock(path: &Path) -> Arc<Mutex<()>> {
    static PATH_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

    let key = canonical_lock_key(path);

    let map = PATH_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock();
    if guard.len() > 256 {
        guard.retain(|_, lock| Arc::strong_count(lock) > 1);
    }
    Arc::clone(
        guard
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
}

pub(crate) fn save_encoded_compact_unlocked(
    encoded: &[u8],
    checkpoint: Option<Checkpoint>,
    path: &Path,
) -> Result<(), CacheError> {
    let checkpoint = checkpoint.unwrap_or(Checkpoint {
        event_id: 0,
        fingerprint: 0,
    });
    // 检查点与编码结果分段写入文件，不再拼成一份连续负载，省掉一次整份复制。
    write_cache_unlocked(
        CACHE_VERSION_CHECKPOINT,
        &[
            &checkpoint.event_id.to_le_bytes(),
            &checkpoint.fingerprint.to_le_bytes(),
            encoded,
        ],
        path,
    )
}

/// 将已编码的紧凑索引字节流与可选回放检查点原子写入磁盘。
pub fn save_encoded_compact(
    encoded: &[u8],
    checkpoint: Option<Checkpoint>,
    path: &Path,
) -> Result<(), CacheError> {
    let lock = path_write_lock(path);
    let _guard = lock.lock();
    save_encoded_compact_unlocked(encoded, checkpoint, path)
}

pub fn save_compact(
    index: &crate::compact_index::CompactIndex,
    checkpoint: Option<Checkpoint>,
    path: &Path,
) -> Result<(), CacheError> {
    // Hold the per-path lock across encoding and atomic rename so concurrent callers
    // saving the same cache file cannot commit snapshots out of order.
    let lock = path_write_lock(path);
    let _guard = lock.lock();
    let encoded = encode_compact(index);
    save_encoded_compact_unlocked(&encoded, checkpoint, path)
}

pub fn load_compact(
    path: &Path,
) -> Result<(crate::compact_index::CompactIndex, Option<Checkpoint>), CacheError> {
    let (version, data) = read_cache(path)?;
    let result = match version {
        CACHE_VERSION => {
            let entries: Vec<crate::compact_index::BorrowedFileEntry<'_>> =
                bitcode::decode(&data[8..]).map_err(CacheError::Decode)?;
            crate::compact_index::CompactIndex::from_borrowed_entries(entries)
                .map(|index| (index, None))
                .map_err(CacheError::InvalidIndex)
        }
        CACHE_VERSION_COMPACT => {
            let cache: crate::compact_index::BorrowedCacheIndex<'_> =
                bitcode::decode(&data[8..]).map_err(CacheError::Decode)?;
            crate::compact_index::CompactIndex::from_borrowed_cache(cache)
                .map(|index| (index, None))
                .map_err(CacheError::InvalidIndex)
        }
        CACHE_VERSION_CHECKPOINT => {
            if data.len() < 24 {
                return Err(CacheError::TooSmall);
            }
            let word = |at: usize| u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
            let checkpoint = Checkpoint {
                event_id: word(8),
                fingerprint: word(16),
            };
            let cache: crate::compact_index::BorrowedCacheIndex<'_> =
                bitcode::decode(&data[24..]).map_err(CacheError::Decode)?;
            crate::compact_index::CompactIndex::from_borrowed_cache(cache)
                .map(|index| (index, (checkpoint.event_id != 0).then_some(checkpoint)))
                .map_err(CacheError::InvalidIndex)
        }
        found => Err(CacheError::VersionMismatch {
            found,
            expected: CACHE_VERSION_CHECKPOINT,
        }),
    };
    // 零拷贝解码进入字符串池后立即释放原始文件字节缓冲，压低热启动峰值内存
    drop(data);
    result
}

fn parse_temp_pid(file_name: &str) -> Option<u32> {
    let (prefix, rest) = file_name.rsplit_once(".tmp.")?;
    if prefix.is_empty() || rest.is_empty() {
        return None;
    }
    let pid_part = match rest.split_once('.') {
        Some((pid_str, seq_str)) => {
            if seq_str.is_empty() || !seq_str.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            pid_str
        }
        None => rest,
    };
    if pid_part.is_empty() || !pid_part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid_part.parse::<u32>().ok().filter(|&pid| pid > 0)
}

#[cfg(unix)]
fn is_process_missing(pid: u32) -> bool {
    if pid == std::process::id() {
        return false;
    }
    let Ok(native_pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    let rc = unsafe { libc::kill(native_pid, 0) };
    rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(not(unix))]
fn is_process_missing(_pid: u32) -> bool {
    false
}

/// Remove orphaned `<name>.tmp.<pid>` / `<name>.tmp.<pid>.<seq>` staging files in `dir`
/// left behind by crashed processes whose PID is no longer alive.
pub fn cleanup_stale_temp_files(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let Some(pid) = parse_temp_pid(name_str) else {
            continue;
        };
        if is_process_missing(pid) && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn temporary_cache_path(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static CACHE_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

    let seq = CACHE_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("cache");
    path.with_file_name(format!("{file_name}.tmp.{}.{seq}", std::process::id()))
}

/// Write `[4 bytes "LOKI"] [4 bytes version u32 LE] [payload parts...]` atomically.
fn write_cache(version: u32, payload: &[&[u8]], path: &Path) -> Result<(), CacheError> {
    let lock = path_write_lock(path);
    let _guard = lock.lock();
    write_cache_unlocked(version, payload, path)
}

fn write_cache_unlocked(version: u32, payload: &[&[u8]], path: &Path) -> Result<(), CacheError> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let temporary = temporary_cache_path(path);
    let write_res = (|| -> Result<(), CacheError> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(CACHE_MAGIC)?;
        file.write_all(&version.to_le_bytes())?;
        for part in payload {
            file.write_all(part)?;
        }
        file.flush()?;
        // 先落盘再改名：断电后要么是旧缓存，要么是完整的新缓存。
        file.sync_data()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if write_res.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_res
}

/// Read a cache file, returning its version and raw contents (header included).
fn read_cache(path: &Path) -> Result<(u32, Vec<u8>), CacheError> {
    let data = fs::read(path)?;
    if data.len() < 8 {
        return Err(CacheError::TooSmall);
    }
    if &data[..4] != CACHE_MAGIC {
        return Err(CacheError::BadMagic);
    }
    Ok((u32::from_le_bytes(data[4..8].try_into().unwrap()), data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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

    fn temp_cache_path(dir: &TempDir) -> PathBuf {
        dir.path().join("index.cache")
    }

    #[test]
    fn test_save_creates_file() {
        let tmp = TempDir::new().unwrap();
        let path = temp_cache_path(&tmp);
        let entries = make_entries(3);

        save_cache(&entries, &path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn interrupted_update_keeps_previous_cache_readable() {
        let directory = TempDir::new().unwrap();
        let path = temp_cache_path(&directory);
        let previous = make_entries(3);
        save_cache(&previous, &path).unwrap();
        let crashed_tmp = directory.path().join("index.cache.tmp.999999.0");
        fs::write(&crashed_tmp, b"LOKI\x04\x00").unwrap();

        let recovered = load_cache(&path).unwrap();
        assert_eq!(recovered.len(), previous.len());
        assert_eq!(recovered[0].path, previous[0].path);

        save_cache(&make_entries(5), &path).unwrap();
        assert_eq!(load_cache(&path).unwrap().len(), 5);

        let extra_tmps: Vec<_> = fs::read_dir(directory.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name.contains(".tmp.") && name != "index.cache.tmp.999999.0"
            })
            .collect();
        assert!(extra_tmps.is_empty(), "new save must not leave temp files: {extra_tmps:?}");
    }

    #[test]
    fn test_save_header() {
        let tmp = TempDir::new().unwrap();
        let path = temp_cache_path(&tmp);
        save_cache(&make_entries(1), &path).unwrap();

        let data = fs::read(&path).unwrap();
        assert_eq!(&data[0..4], b"LOKI");
        assert_eq!(&data[4..8], &3u32.to_le_bytes());
    }

    #[test]
    fn test_save_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nested").join("deep").join("index.cache");

        save_cache(&make_entries(1), &path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_save_atomic_no_tmp_left() {
        let tmp = TempDir::new().unwrap();
        let path = temp_cache_path(&tmp);

        save_cache(&make_entries(1), &path).unwrap();

        let leftovers: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), ".tmp file should not remain after save: {leftovers:?}");
    }

    #[test]
    fn test_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let path = temp_cache_path(&tmp);
        let entries = make_entries(5);

        save_cache(&entries, &path).unwrap();
        let loaded = load_cache(&path).unwrap();

        assert_eq!(entries, loaded);
    }

    #[test]
    fn test_load_roundtrip_1000() {
        let tmp = TempDir::new().unwrap();
        let path = temp_cache_path(&tmp);
        let entries = make_entries(1000);

        save_cache(&entries, &path).unwrap();
        let loaded = load_cache(&path).unwrap();

        assert_eq!(entries, loaded);
    }

    #[test]
    fn test_load_bad_magic() {
        let tmp = TempDir::new().unwrap();
        let path = temp_cache_path(&tmp);

        let mut data = Vec::new();
        data.extend_from_slice(b"NOPE");
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&[0; 16]);
        fs::write(&path, &data).unwrap();

        let err = load_cache(&path).unwrap_err();
        assert!(matches!(err, CacheError::BadMagic));
    }

    #[test]
    fn test_load_version_mismatch() {
        let tmp = TempDir::new().unwrap();
        let path = temp_cache_path(&tmp);

        let mut data = Vec::new();
        data.extend_from_slice(b"LOKI");
        data.extend_from_slice(&99u32.to_le_bytes());
        data.extend_from_slice(&[0; 16]);
        fs::write(&path, &data).unwrap();

        let err = load_cache(&path).unwrap_err();
        match err {
            CacheError::VersionMismatch { found, expected } => {
                assert_eq!(found, 99);
                assert_eq!(expected, 3);
            }
            other => panic!("Expected VersionMismatch, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_too_small() {
        let tmp = TempDir::new().unwrap();
        let path = temp_cache_path(&tmp);

        fs::write(&path, &[0u8; 4]).unwrap();

        let err = load_cache(&path).unwrap_err();
        assert!(matches!(err, CacheError::TooSmall));
    }

    #[test]
    fn test_load_corrupt_payload() {
        let tmp = TempDir::new().unwrap();
        let path = temp_cache_path(&tmp);

        let mut data = Vec::new();
        data.extend_from_slice(b"LOKI");
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(b"this is definitely not valid bitcode data!!");
        fs::write(&path, &data).unwrap();

        let err = load_cache(&path).unwrap_err();
        assert!(matches!(err, CacheError::Decode(_)));
    }

    #[test]
    fn test_load_missing_file() {
        let err = load_cache(Path::new("/nonexistent/path/cache.bin")).unwrap_err();
        assert!(matches!(err, CacheError::Io(_)));
    }

    #[test]
    fn test_cache_path_default() {
        let path = cache_path();
        let path_str = path.to_string_lossy();
        assert!(
            path_str.ends_with(".config/lokii/index.cache"),
            "cache_path should end with .config/lokii/index.cache, got: {path_str}"
        );
    }

    #[test]
    fn test_concurrent_save_compact_uses_unique_temp_files() {
        let tmp = TempDir::new().unwrap();
        let shared_path = tmp.path().join("shared.cache");
        let compact = crate::compact_index::CompactIndex::from_entries(make_entries(25)).unwrap();
        let checkpoint = Some(Checkpoint {
            event_id: 99,
            fingerprint: 1234,
        });

        std::thread::scope(|scope| {
            for t in 0..6 {
                let compact_ref = &compact;
                let shared_ref = &shared_path;
                let distinct_path = tmp.path().join(format!("shard_{t}.cache"));
                scope.spawn(move || {
                    for _ in 0..4 {
                        save_compact(compact_ref, checkpoint, shared_ref).unwrap();
                        save_compact(compact_ref, checkpoint, &distinct_path).unwrap();
                    }
                });
            }
        });

        let (decoded_shared, cp) = load_compact(&shared_path).unwrap();
        assert_eq!(decoded_shared.len(), 25);
        assert_eq!(cp, checkpoint);
        for t in 0..6 {
            let (decoded, _) = load_compact(&tmp.path().join(format!("shard_{t}.cache"))).unwrap();
            assert_eq!(decoded.len(), 25);
        }
        let leftovers: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leftover cache temp files: {leftovers:?}");
    }

    #[test]
    fn cleanup_stale_temp_files_removes_dead_pid_but_keeps_live_and_real_files() {
        let tmp = TempDir::new().unwrap();
        let dead_tmp_seq = tmp.path().join("index.cache.tmp.999999999.0");
        let dead_tmp_plain = tmp.path().join("manifest.json.tmp.999999998");
        let live_tmp = tmp
            .path()
            .join(format!("index.cache.tmp.{}.1", std::process::id()));
        let real_cache = tmp.path().join("index.cache");
        let real_cfg = tmp.path().join("config.toml");

        fs::write(&dead_tmp_seq, b"dead1").unwrap();
        fs::write(&dead_tmp_plain, b"dead2").unwrap();
        fs::write(&live_tmp, b"live").unwrap();
        fs::write(&real_cache, b"LOKI").unwrap();
        fs::write(&real_cfg, b"[general]").unwrap();

        let removed = cleanup_stale_temp_files(tmp.path());
        assert_eq!(removed, 2);
        assert!(!dead_tmp_seq.exists());
        assert!(!dead_tmp_plain.exists());
        assert!(live_tmp.exists(), "must preserve temp file owned by live process");
        assert!(real_cache.exists());
        assert!(real_cfg.exists());
    }

    #[test]
    fn path_write_lock_returns_same_mutex_before_and_after_parent_creation() {
        let tmp = TempDir::new().unwrap();
        let uncreated_subdir = tmp.path().join("nested").join("volumes");
        let cache_path = uncreated_subdir.join("vol_a.cache");

        let lock_before = path_write_lock(&cache_path);
        fs::create_dir_all(&uncreated_subdir).unwrap();
        let lock_after = path_write_lock(&cache_path);
        assert!(
            Arc::ptr_eq(&lock_before, &lock_after),
            "per-path lock must remain identical before and after parent directory creation"
        );
    }
}
