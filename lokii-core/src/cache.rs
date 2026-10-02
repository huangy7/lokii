use crate::indexer::FileEntry;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const CACHE_MAGIC: &[u8; 4] = b"LOKI";
const CACHE_VERSION: u32 = 3;

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
    VersionMismatch { found: u32, expected: u32 },
    /// Failed to decode the cache payload.
    Decode(bitcode::Error),
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheError::Io(e) => write!(f, "cache I/O error: {e}"),
            CacheError::TooSmall => write!(f, "cache file too small for valid header"),
            CacheError::BadMagic => write!(f, "cache file has invalid magic bytes"),
            CacheError::VersionMismatch { found, expected } => {
                write!(f, "cache version mismatch: found {found}, expected {expected}")
            }
            CacheError::Decode(e) => write!(f, "cache decode error: {e}"),
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
/// File format: [4 bytes "LOKI"] [4 bytes version u32 LE] [N bytes bitcode payload]
///
/// Uses atomic write: writes to a .tmp file first, then renames to final path
/// to prevent corruption if the process crashes mid-write.
pub fn save_cache(entries: &[FileEntry], path: &Path) -> Result<(), CacheError> {
    // Create parent directories if needed
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let encoded = bitcode::encode(entries);

    let tmp_path = path.with_extension("tmp");
    let mut file = fs::File::create(&tmp_path)?;
    file.write_all(CACHE_MAGIC)?;
    file.write_all(&CACHE_VERSION.to_le_bytes())?;
    file.write_all(&encoded)?;
    file.flush()?;

    // Atomic rename
    fs::rename(&tmp_path, path)?;

    Ok(())
}

/// Load file entries from a cache file, validating the version header.
///
/// Returns errors for: missing file, too small, bad magic, version mismatch, corrupt payload.
/// Never panics on invalid input.
pub fn load_cache(path: &Path) -> Result<Vec<FileEntry>, CacheError> {
    let data = fs::read(path)?;

    if data.len() < 8 {
        return Err(CacheError::TooSmall);
    }

    if &data[0..4] != CACHE_MAGIC {
        return Err(CacheError::BadMagic);
    }

    let version = u32::from_le_bytes(data[4..8].try_into().unwrap());
    if version != CACHE_VERSION {
        return Err(CacheError::VersionMismatch {
            found: version,
            expected: CACHE_VERSION,
        });
    }

    bitcode::decode(&data[8..]).map_err(CacheError::Decode)
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

        let tmp_path = path.with_extension("tmp");
        assert!(!tmp_path.exists(), ".tmp file should not remain after save");
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
}
