use lokii_core::pinyin;

#[allow(dead_code)]
mod compact_index {
    include!("../src/compact_index.rs");

    pub(crate) fn shrink_slots(index: &mut CompactIndex) {
        index.names.shrink_slots();
        index.parents.shrink_slots();
        index.extensions.shrink_slots();
        index.raw_paths.shrink_slots();
    }

    fn table_id<'a>(
        table: &mut std::collections::HashMap<&'a str, u32>,
        strings: &mut Vec<String>,
        text: &'a str,
    ) -> Result<u32, IndexError> {
        match table.entry(text) {
            std::collections::hash_map::Entry::Occupied(entry) => Ok(*entry.get()),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let id = u32::try_from(strings.len()).map_err(|_| IndexError::IdOverflow)?;
                if id == u32::MAX {
                    return Err(IndexError::IdOverflow);
                }
                strings.push(text.into());
                entry.insert(id);
                Ok(id)
            }
        }
    }

    pub(crate) fn build_tables(
        entries: Vec<BorrowedFileEntry<'_>>,
    ) -> Result<CacheIndex, IndexError> {
        let mut names = std::collections::HashMap::new();
        let mut parents = std::collections::HashMap::new();
        let mut extensions = std::collections::HashMap::new();
        let mut raw_paths = std::collections::HashMap::new();
        let mut cache = CacheIndex {
            names: Vec::new(),
            parents: Vec::new(),
            extensions: Vec::new(),
            raw_paths: Vec::new(),
            records: Vec::with_capacity(entries.len()),
        };
        for entry in entries {
            let name = table_id(&mut names, &mut cache.names, entry.name)?;
            let extension = table_id(&mut extensions, &mut cache.extensions, entry.extension)?;
            let parent = entry.path.strip_suffix(entry.name).filter(|parent| {
                !entry.name.is_empty()
                    && !entry.name.contains('/')
                    && (parent.is_empty() || parent.ends_with('/'))
            });
            let raw_path = parent.is_none();
            let path = match parent {
                Some(parent) => table_id(&mut parents, &mut cache.parents, parent)?,
                None => table_id(&mut raw_paths, &mut cache.raw_paths, entry.path)?,
            };
            cache.records.push(CompactRecord {
                name,
                extension,
                path,
                raw_path,
                is_dir: entry.is_dir,
                size: entry.size,
                modified: entry.modified,
                created_at: entry.created_at,
            });
        }
        Ok(cache)
    }

    pub(crate) fn unique_counts(entries: &[BorrowedFileEntry<'_>]) -> [usize; 4] {
        let mut names = HashSet::new();
        let mut parents = HashSet::new();
        let mut extensions = HashSet::new();
        let mut raw_paths = HashSet::new();
        for entry in entries {
            names.insert(entry.name);
            extensions.insert(entry.extension);
            let parent = entry.path.strip_suffix(entry.name).filter(|parent| {
                !entry.name.is_empty()
                    && !entry.name.contains('/')
                    && (parent.is_empty() || parent.ends_with('/'))
            });
            match parent {
                Some(parent) => {
                    parents.insert(parent);
                }
                None => {
                    raw_paths.insert(entry.path);
                }
            }
        }
        [
            names.len(),
            parents.len(),
            extensions.len(),
            raw_paths.len(),
        ]
    }

    pub(crate) fn with_reserved_lookups(
        entries: Vec<BorrowedFileEntry<'_>>,
        capacities: [usize; 4],
    ) -> Result<CompactIndex, IndexError> {
        let mut index = CompactIndex::new();
        index.names.reserve_lookup(capacities[0]);
        index.parents.reserve_lookup(capacities[1]);
        index.extensions.reserve_lookup(capacities[2]);
        index.raw_paths.reserve_lookup(capacities[3]);
        index.records.reserve(entries.len());
        let mut hints = InternHints::default();
        for entry in entries {
            let record = index.acquire_borrowed_with_hints(&entry, &mut hints)?;
            index.records.push(record);
        }
        Ok(index)
    }
}
#[allow(dead_code)]
mod string_pool {
    include!("../src/string_pool.rs");

    impl StringPool {
        pub(crate) fn reserve_lookup(&mut self, count: usize) {
            self.lookup.reserve(count);
        }

        pub(crate) fn shrink_slots(&mut self) {
            self.slots.shrink_to_fit();
        }
    }
}

mod indexer {
    pub use lokii_core::indexer::FileEntry;
}

use compact_index::{BorrowedFileEntry, CompactIndex};
use std::time::Instant;

fn cpu_ms() -> f64 {
    let mut timestamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut timestamp) },
        0
    );
    timestamp.tv_sec as f64 * 1000.0 + timestamp.tv_nsec as f64 / 1_000_000.0
}

fn phase<T>(operation: impl FnOnce() -> T) -> (T, serde_json::Value) {
    let cpu_started = cpu_ms();
    let started = Instant::now();
    let result = operation();
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let cpu_ms = cpu_ms() - cpu_started;
    (
        result,
        serde_json::json!({"wall_ms": wall_ms, "cpu_ms": cpu_ms}),
    )
}

fn main() {
    let path = std::env::args().nth(1).expect("v3 cache path required");
    let rounds = std::env::args()
        .nth(2)
        .map(|value| value.parse::<usize>().unwrap())
        .unwrap_or(5);
    let mode = std::env::args().nth(3);
    let counted = mode.as_deref() == Some("counted");
    let tables = mode.as_deref() == Some("tables");
    let shrink = mode.as_deref() == Some("shrink");
    let hold = std::env::args()
        .nth(4)
        .map(|value| value.parse::<u64>().unwrap())
        .unwrap_or(0);
    let capacities = mode
        .filter(|value| {
            value != "counted" && value != "tables" && value != "shrink" && value != "normal"
        })
        .map(|value| {
            let counts: Vec<usize> = value
                .split(',')
                .map(|count| count.parse().unwrap())
                .collect();
            <[usize; 4]>::try_from(counts).expect("four pool capacities required")
        });
    let synthetic = path.strip_prefix("unique:").map(|value| {
        let count = value.parse::<usize>().unwrap();
        let records: Vec<_> = (0..count).map(|ordinal| {
            let name = format!("file_{ordinal:07}.rs");
            let parent = ordinal / 2;
            indexer::FileEntry { path: format!("/synthetic/developer/project_{parent:07}/src/long_directory_component/{name}"),
                name, extension: "rs".into(), is_dir: false, size: ordinal as u64,
                modified: 1_700_000_000, created_at: 1_600_000_000 }
        }).collect();
        let legacy = records.capacity() * std::mem::size_of::<indexer::FileEntry>()
            + records.iter().map(|entry| entry.name.capacity() + entry.path.capacity() + entry.extension.capacity()).sum::<usize>();
        let mut data = b"LOKI\x03\0\0\0".to_vec();
        data.extend_from_slice(&bitcode::encode(&records));
        (data, legacy)
    });
    for round in 0..rounds {
        let (data, read) = phase(|| match &synthetic {
            Some((data, _)) => data.clone(),
            None => std::fs::read(&path).unwrap(),
        });
        assert_eq!(&data[..8], b"LOKI\x03\0\0\0");
        let (entries, decode) =
            phase(|| bitcode::decode::<Vec<BorrowedFileEntry<'_>>>(&data[8..]).unwrap());
        let (capacities, prepare) = phase(|| {
            if counted {
                Some(compact_index::unique_counts(&entries))
            } else {
                capacities
            }
        });
        let (mut index, intern, build_tables) = if tables {
            let (cache, build) = phase(|| compact_index::build_tables(entries).unwrap());
            let (index, restore) = phase(|| CompactIndex::from_cache(cache).unwrap());
            (index, restore, Some(build))
        } else {
            let (index, intern) = phase(|| match capacities {
                Some(capacities) => {
                    compact_index::with_reserved_lookups(entries, capacities).unwrap()
                }
                None => CompactIndex::from_borrowed_entries(entries).unwrap(),
            });
            (index, intern, None)
        };
        let count = index.len();
        let pools = index.pool_counts();
        let memory_before_shrink = index.memory_stats();
        let (_, shrink_time) = phase(|| {
            if shrink {
                compact_index::shrink_slots(&mut index);
            }
        });
        let memory = index.memory_stats();
        let (_, release_input) = phase(|| drop(data));
        if hold > 0 {
            println!(
                "{}",
                serde_json::json!({"pid":std::process::id(),"entries":count,
                "round":round,"memory":memory,"shrink":shrink_time})
            );
            std::thread::sleep(std::time::Duration::from_secs(hold));
        }
        let (_, release_index) = phase(|| drop(index));
        println!(
            "{}",
            serde_json::json!({"round":round, "entries":count,
            "read":read, "decode":decode, "intern":intern, "release_input":release_input,
            "release_index":release_index, "memory":memory, "pools":pools,
            "reserved_lookups":capacities, "prepare":prepare, "build_tables":build_tables,
            "shrink":shrink_time,"memory_before_shrink":memory_before_shrink,
            "legacy_estimated_bytes":synthetic.as_ref().map(|(_, legacy)| legacy)})
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_shrink_preserves_ids_metadata_and_later_reuse() {
        let originals: Vec<_> = (0..33)
            .map(|ordinal| indexer::FileEntry {
                name: format!("file_{ordinal}.rs"),
                path: format!("/parent_{ordinal}/file_{ordinal}.rs"),
                extension: "rs".into(),
                is_dir: false,
                size: u64::MAX,
                modified: u64::MAX,
                created_at: u64::MAX,
            })
            .collect();
        let mut index = CompactIndex::from_entries(originals.clone()).unwrap();
        let before = index.memory_stats().total_estimated_bytes;
        compact_index::shrink_slots(&mut index);
        assert!(index.memory_stats().total_estimated_bytes < before);
        for (ordinal, original) in originals.iter().enumerate() {
            assert_eq!(
                &index.materialize(compact_index::EntryId(ordinal)).unwrap(),
                original
            );
        }
        index.remove_matching(|view| view.name == "file_0.rs");
        index.push(originals[0].clone()).unwrap();
        index.validate().unwrap();
        assert_eq!(
            index.materialize(compact_index::EntryId(32)).unwrap(),
            originals[0]
        );
    }

    #[test]
    fn counted_reservation_preserves_duplicate_and_raw_records() {
        let originals: Vec<_> = [
            ("main.rs", "/a/main.rs"),
            ("main.rs", "/a/main.rs"),
            ("alias", "/actual"),
            ("", ""),
        ]
        .into_iter()
        .map(|(name, path)| indexer::FileEntry {
            name: name.into(),
            path: path.into(),
            extension: "rs".into(),
            is_dir: false,
            size: u64::MAX,
            modified: u64::MAX,
            created_at: u64::MAX,
        })
        .collect();
        let payload = bitcode::encode(&originals);
        let entries: Vec<BorrowedFileEntry<'_>> = bitcode::decode(&payload).unwrap();
        let counts = compact_index::unique_counts(&entries);
        assert_eq!(counts, [3, 1, 1, 2]);
        let index = compact_index::with_reserved_lookups(entries, counts).unwrap();
        assert_eq!(index.len(), 4);
        index.validate().unwrap();
        for (ordinal, original) in originals.iter().enumerate() {
            assert_eq!(
                &index.materialize(compact_index::EntryId(ordinal)).unwrap(),
                original
            );
        }
    }

    #[test]
    fn batch_tables_preserve_order_duplicates_and_raw_paths() {
        let originals: Vec<_> = [
            ("main.rs", "/a/main.rs"),
            ("main.rs", "/a/main.rs"),
            ("中文", "/中文"),
            ("alias", "/actual"),
            ("/", "/"),
            ("", ""),
        ]
        .into_iter()
        .map(|(name, path)| indexer::FileEntry {
            name: name.into(),
            path: path.into(),
            extension: "".into(),
            is_dir: true,
            size: u64::MAX,
            modified: u64::MAX,
            created_at: u64::MAX,
        })
        .collect();
        let payload = bitcode::encode(&originals);
        let entries = bitcode::decode::<Vec<BorrowedFileEntry<'_>>>(&payload).unwrap();
        let cache = compact_index::build_tables(entries).unwrap();
        let index = CompactIndex::from_cache(cache).unwrap();
        assert_eq!(index.len(), originals.len());
        for (ordinal, original) in originals.iter().enumerate() {
            assert_eq!(
                &index.materialize(compact_index::EntryId(ordinal)).unwrap(),
                original
            );
        }
        index.validate().unwrap();
    }
}
