use std::collections::HashSet;
use std::fmt;

use crate::indexer::FileEntry;
use crate::string_pool::StringPool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexError {
    IdOverflow,
    InvalidReference,
    ReferenceOverflow,
}

impl fmt::Display for IndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "compact index error: {self:?}")
    }
}

impl std::error::Error for IndexError {}

pub(crate) fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(ordinal) => path.split_at(ordinal + 1),
        None => ("", path),
    }
}

/// Rewrite `entry` for a rename of `old` to `new`.
///
/// `prefix` must be `old` followed by `/`. The renamed entry itself moves to
/// `new`; descendants keep their suffix and are reparented under `new`.
fn rewrite_rename(entry: &mut FileEntry, old: &str, new: &str, prefix: &str) {
    if entry.path == old {
        entry.path = new.to_owned();
        if let Some(name) = std::path::Path::new(new).file_name() {
            entry.name = name.to_string_lossy().into_owned();
        }
    } else if let Some(suffix) = entry.path.strip_prefix(prefix) {
        entry.path = format!("{new}/{suffix}");
    }
}

/// Whether `path` is one of `names` or sits below one of the `/`-terminated
/// prefixes in `names`.
///
/// Only `/`-boundary prefixes can match, so walking the path's own component
/// boundaries costs O(depth) hash lookups instead of one comparison per name.
fn touches_path(path: &str, names: &HashSet<&str>) -> bool {
    if names.contains(path) {
        return true;
    }
    let mut end = path.len();
    while end > 0 {
        let trimmed = path[..end].trim_end_matches('/');
        match trimmed.rfind('/') {
            Some(ordinal) => {
                end = ordinal + 1;
                if names.contains(&path[..end]) {
                    return true;
                }
            }
            None => break,
        }
    }
    false
}

/// Whether `affected_masks` marked the pool entry `ordinal`. Empty masks mark
/// nothing, so a batch that carried no paths never has to build them.
fn marked(mask: &[bool], ordinal: u32) -> bool {
    mask.get(ordinal as usize).copied().unwrap_or(false)
}

#[derive(Clone, Copy, bitcode::Encode, bitcode::Decode)]
pub(crate) struct CompactRecord {
    name: u32,
    /// 父目录池槽位；原始路径记录则为路径池槽位。
    pub(crate) path: u32,
    pub(crate) extension: u32,
    pub(crate) raw_path: bool,
    pub(crate) is_dir: bool,
    size: u64,
    modified: u64,
    created_at: u64,
}

#[derive(Clone, Copy)]
pub struct EntryView<'a> {
    pub name: &'a str,
    pub parent: &'a str,
    pub extension: &'a str,
    pub is_dir: bool,
    pub size: u64,
    pub modified: u64,
    pub created_at: u64,
    raw_path: Option<&'a str>,
}

impl<'a> EntryView<'a> {
    fn path_key(&self) -> (&'a str, &'a str) {
        match self.raw_path {
            Some(raw) => split_path(raw),
            None => (self.parent, self.name),
        }
    }
    pub fn path_into(&self, output: &mut String) {
        output.clear();
        if let Some(path) = self.raw_path {
            output.push_str(path);
        } else {
            output.push_str(self.parent);
            output.push_str(self.name);
        }
    }

    pub fn path_equals(&self, path: &str) -> bool {
        match self.raw_path {
            Some(raw) => raw == path,
            None => path.strip_prefix(self.parent) == Some(self.name),
        }
    }

    pub fn path_starts_with(&self, prefix: &str) -> bool {
        match self.raw_path {
            Some(raw) => raw.starts_with(prefix),
            None if prefix.len() <= self.parent.len() => self.parent.starts_with(prefix),
            None => prefix
                .strip_prefix(self.parent)
                .is_some_and(|suffix| self.name.starts_with(suffix)),
        }
    }

    pub fn materialize(&self) -> FileEntry {
        let mut path = String::new();
        self.path_into(&mut path);
        FileEntry {
            name: self.name.into(),
            path,
            is_dir: self.is_dir,
            size: self.size,
            modified: self.modified,
            created_at: self.created_at,
            extension: self.extension.into(),
        }
    }
}

#[derive(Default)]
pub struct CompactIndex {
    records: Vec<CompactRecord>,
    names: StringPool,
    parents: StringPool,
    extensions: StringPool,
    raw_paths: StringPool,
}

#[derive(Debug, serde::Serialize)]
pub struct IndexMemoryStats {
    pub records_bytes: usize,
    pub pools_estimated_bytes: usize,
    pub total_estimated_bytes: usize,
}

#[derive(bitcode::Encode, bitcode::Decode)]
pub(crate) struct CacheIndex {
    names: Vec<String>,
    parents: Vec<String>,
    extensions: Vec<String>,
    raw_paths: Vec<String>,
    records: Vec<CompactRecord>,
}

#[derive(bitcode::Decode)]
pub(crate) struct BorrowedFileEntry<'a> {
    name: &'a str,
    path: &'a str,
    is_dir: bool,
    size: u64,
    modified: u64,
    created_at: u64,
    extension: &'a str,
}

impl<'a> From<&'a FileEntry> for BorrowedFileEntry<'a> {
    fn from(entry: &'a FileEntry) -> Self {
        Self {
            name: &entry.name,
            path: &entry.path,
            is_dir: entry.is_dir,
            size: entry.size,
            modified: entry.modified,
            created_at: entry.created_at,
            extension: &entry.extension,
        }
    }
}

#[derive(Default)]
struct InternHints {
    name: Option<u32>,
    extension: Option<u32>,
    parent: Option<u32>,
    raw_path: Option<u32>,
}

impl CompactIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_entries(entries: impl IntoIterator<Item = FileEntry>) -> Result<Self, IndexError> {
        let mut index = Self::new();
        let entries = entries.into_iter();
        index.records.reserve(entries.size_hint().0);
        for entry in entries {
            index.push(entry)?;
        }
        Ok(index)
    }

    fn acquire(&mut self, entry: &FileEntry) -> Result<CompactRecord, IndexError> {
        self.acquire_borrowed(&BorrowedFileEntry::from(entry))
    }

    pub(crate) fn from_borrowed_entries<'a>(
        entries: impl IntoIterator<Item = BorrowedFileEntry<'a>>,
    ) -> Result<Self, IndexError> {
        let mut index = Self::new();
        let entries = entries.into_iter();
        index.records.reserve(entries.size_hint().0);
        let mut hints = InternHints::default();
        for entry in entries {
            let record = index.acquire_borrowed_with_hints(&entry, &mut hints)?;
            index.records.push(record);
        }
        Ok(index)
    }

    fn acquire_borrowed(
        &mut self,
        entry: &BorrowedFileEntry<'_>,
    ) -> Result<CompactRecord, IndexError> {
        self.acquire_borrowed_with_hints(entry, &mut InternHints::default())
    }

    fn acquire_borrowed_with_hints(
        &mut self,
        entry: &BorrowedFileEntry<'_>,
        hints: &mut InternHints,
    ) -> Result<CompactRecord, IndexError> {
        let name = self.names.intern_with_hint(entry.name, &mut hints.name)?;
        let extension = match self
            .extensions
            .intern_with_hint(entry.extension, &mut hints.extension)
        {
            Ok(id) => id,
            Err(error) => {
                self.names.release(name);
                return Err(error);
            }
        };
        let parent = entry.path.strip_suffix(entry.name).filter(|parent| {
            !entry.name.is_empty()
                && !entry.name.contains('/')
                && (parent.is_empty() || parent.ends_with('/'))
        });
        let raw_path = parent.is_none();
        let path = match parent {
            Some(parent) => self.parents.intern_with_hint(parent, &mut hints.parent),
            None => self
                .raw_paths
                .intern_with_hint(entry.path, &mut hints.raw_path),
        };
        let path = match path {
            Ok(id) => id,
            Err(error) => {
                self.names.release(name);
                self.extensions.release(extension);
                return Err(error);
            }
        };
        Ok(CompactRecord {
            name,
            path,
            extension,
            raw_path,
            is_dir: entry.is_dir,
            size: entry.size,
            modified: entry.modified,
            created_at: entry.created_at,
        })
    }

    fn release(&mut self, record: CompactRecord) {
        self.names.release(record.name);
        self.extensions.release(record.extension);
        if record.raw_path {
            self.raw_paths.release(record.path);
        } else {
            self.parents.release(record.path);
        }
    }

    /// Release every record staged for a batch that is being abandoned.
    fn release_all(&mut self, records: impl IntoIterator<Item = CompactRecord>) {
        for record in records {
            self.release(record);
        }
    }

    pub fn push(&mut self, entry: FileEntry) -> Result<(), IndexError> {
        let record = self.acquire(&entry)?;
        self.records.push(record);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn record_view(&self, record: &CompactRecord) -> Option<EntryView<'_>> {
        let path = if record.raw_path {
            self.raw_paths.get(record.path)?
        } else {
            self.parents.get(record.path)?
        };
        Some(EntryView {
            name: self.names.get(record.name)?,
            parent: if record.raw_path { "" } else { path },
            raw_path: record.raw_path.then_some(path),
            extension: self.extensions.get(record.extension)?,
            is_dir: record.is_dir,
            size: record.size,
            modified: record.modified,
            created_at: record.created_at,
        })
    }

    pub fn view(&self, id: EntryId) -> Option<EntryView<'_>> {
        self.record_view(self.records.get(id.0)?)
    }

    pub fn iter(&self) -> impl Iterator<Item = (EntryId, EntryView<'_>)> {
        self.records.iter().enumerate().map(|(ordinal, record)| {
            (
                EntryId(ordinal),
                self.record_view(record).expect("valid compact reference"),
            )
        })
    }

    pub fn names(&self) -> impl Iterator<Item = (EntryId, &str)> {
        self.records.iter().enumerate().map(|(ordinal, record)| {
            (
                EntryId(ordinal),
                self.names.get(record.name).expect("valid name reference"),
            )
        })
    }

    pub(crate) fn records(&self) -> &[CompactRecord] {
        &self.records
    }

    /// 解析记录的父目录与文件名：原始路径记录现场按 '/' 切分，池化记录直接取池内字符串。
    /// 与 `EntryView::path_key` 同规则，但按记录惰性求值，避免流式扫描时构建整条视图。
    pub(crate) fn split_record(&self, record: &CompactRecord) -> (&str, &str) {
        if record.raw_path {
            self.raw_paths.get(record.path).map_or(("", ""), split_path)
        } else {
            (
                self.parents.get(record.path).unwrap_or(""),
                self.names.get(record.name).unwrap_or(""),
            )
        }
    }

    /// 仅解析文件名，省去不需要时的父目录池查表。
    pub(crate) fn record_name(&self, record: &CompactRecord) -> &str {
        if record.raw_path {
            self.raw_paths
                .get(record.path)
                .map_or("", |p| split_path(p).1)
        } else {
            self.names.get(record.name).unwrap_or("")
        }
    }

    pub(crate) fn extension(&self, id: u32) -> Option<&str> {
        self.extensions.get(id)
    }

    pub(crate) fn evaluate_parents<T: Clone>(
        &self,
        default: T,
        mapper: impl FnMut(&str) -> T,
    ) -> Vec<T> {
        self.parents.evaluate_ids(default, mapper)
    }

    pub fn materialize(&self, id: EntryId) -> Result<FileEntry, IndexError> {
        Ok(self
            .view(id)
            .ok_or(IndexError::InvalidReference)?
            .materialize())
    }

    pub fn path_into(&self, id: EntryId, output: &mut String) -> Result<(), IndexError> {
        self.view(id)
            .ok_or(IndexError::InvalidReference)?
            .path_into(output);
        Ok(())
    }

    pub fn remove_matching(&mut self, mut predicate: impl FnMut(EntryView<'_>) -> bool) -> usize {
        self.remove_records_matching(|index, record| predicate(index.record_view(record).unwrap()))
    }

    fn remove_records_matching(
        &mut self,
        mut predicate: impl FnMut(&Self, &CompactRecord) -> bool,
    ) -> usize {
        let mut retained = 0;
        let count = self.records.len();
        for ordinal in 0..count {
            let record = self.records[ordinal];
            if predicate(self, &record) {
                self.release(record);
            } else {
                self.records[retained] = record;
                retained += 1;
            }
        }
        self.records.truncate(retained);
        count - retained
    }

    pub fn add_entries(&mut self, entries: Vec<FileEntry>) -> Result<(), IndexError> {
        let mut staged = Vec::with_capacity(entries.len());
        self.records.reserve_exact(entries.len());
        for entry in entries {
            match self.acquire(&entry) {
                Ok(record) => staged.push(record),
                Err(error) => {
                    self.release_all(staged);
                    return Err(error);
                }
            }
        }
        self.records.extend(staged);
        Ok(())
    }

    pub fn upsert_batch(
        &mut self,
        entries: Vec<FileEntry>,
        predicate: impl FnMut(EntryView<'_>) -> bool,
    ) -> Result<usize, IndexError> {
        let mut staged = Vec::with_capacity(entries.len());
        for entry in entries {
            match self.acquire(&entry) {
                Ok(record) => staged.push(record),
                Err(error) => {
                    self.release_all(staged);
                    return Err(error);
                }
            }
        }
        let removed = self.remove_matching(predicate);
        self.records.reserve_exact(staged.len());
        self.records.extend(staged);
        Ok(removed)
    }

    pub fn rename_prefix(&mut self, old: &str, new: &str) -> Result<usize, IndexError> {
        let prefix = format!("{old}/");
        let (old_parent, old_name) = split_path(old);
        let affected = self
            .parents
            .matching_ids(|parent| parent.starts_with(&prefix) || parent == old_parent);
        let raw_affected = self
            .raw_paths
            .matching_ids(|path| path == old || path.starts_with(&prefix));
        let mut staged = Vec::new();
        for ordinal in 0..self.records.len() {
            let record = self.records[ordinal];
            if record.raw_path {
                if !raw_affected[record.path as usize] {
                    continue;
                }
            } else {
                if !affected[record.path as usize] {
                    continue;
                }
                let parent = self.parents.get(record.path).unwrap();
                if parent == old_parent && self.names.get(record.name) != Some(old_name) {
                    continue;
                }
            }
            let view = self.view(EntryId(ordinal)).unwrap();
            if !view.path_equals(old) && !view.path_starts_with(&prefix) {
                continue;
            }
            let mut entry = view.materialize();
            rewrite_rename(&mut entry, old, new, &prefix);
            match self.acquire(&entry) {
                Ok(record) => staged.push((ordinal, record)),
                Err(error) => {
                    self.release_all(staged.into_iter().map(|(_, record)| record));
                    return Err(error);
                }
            }
        }
        let count = staged.len();
        for (ordinal, record) in staged {
            let previous = std::mem::replace(&mut self.records[ordinal], record);
            self.release(previous);
        }
        Ok(count)
    }

    /// Bitmasks over the parent and raw-path pools marking every entry a batch
    /// of `paths` can touch: each path itself, or anything below it.
    ///
    /// Records reach the pools by reference, so the loops that walk the record
    /// list pay one array read each instead of one comparison per record per
    /// path. The saving tracks how far the pools deduplicate — on a tree where
    /// every file sits in a directory of its own the pools are as long as the
    /// record list, and the walk is no cheaper than comparing directly.
    ///
    /// An empty `paths` yields empty masks, which `marked` reads as "nothing is
    /// touched": walking both pools to build all-false masks would cost more
    /// than the loop the masks exist to speed up.
    fn affected_masks(&self, paths: &[&str]) -> (Vec<bool>, Vec<bool>) {
        if paths.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let prefixes: Vec<String> = paths.iter().map(|path| format!("{path}/")).collect();
        let mut names: HashSet<&str> = HashSet::with_capacity(2 * paths.len());
        let mut exact_parents: HashSet<&str> = HashSet::with_capacity(paths.len());
        for &path in paths {
            names.insert(path);
            exact_parents.insert(split_path(path).0);
        }
        names.extend(prefixes.iter().map(String::as_str));
        let parents = self
            .parents
            .matching_ids(|parent| exact_parents.contains(parent) || touches_path(parent, &names));
        let raw = self
            .raw_paths
            .matching_ids(|path| touches_path(path, &names));
        (parents, raw)
    }

    pub fn apply_event_batch(
        &mut self,
        renames: &[(String, String)],
        excluded: &[String],
        removes: &[String],
        creates: Vec<FileEntry>,
    ) -> Result<(usize, usize), IndexError> {
        let mut replacements = Vec::new();
        let rename_prefixes: Vec<_> = renames.iter().map(|(old, _)| format!("{old}/")).collect();
        if !renames.is_empty() {
            let old_paths: Vec<&str> = renames.iter().map(|(old, _)| old.as_str()).collect();
            let (affected_parents, affected_raw_paths) = self.affected_masks(&old_paths);
            for ordinal in 0..self.records.len() {
                let record = self.records[ordinal];
                let affected = if record.raw_path {
                    marked(&affected_raw_paths, record.path)
                } else {
                    marked(&affected_parents, record.path)
                };
                if !affected {
                    continue;
                }
                let view = self.view(EntryId(ordinal)).unwrap();
                let mut changed = None;
                for ((old, new), prefix) in renames.iter().zip(&rename_prefixes) {
                    let mut entry = match changed.take() {
                        Some(entry) => entry,
                        None if view.path_equals(old) || view.path_starts_with(prefix) => {
                            view.materialize()
                        }
                        None => continue,
                    };
                    rewrite_rename(&mut entry, old, new, prefix);
                    changed = Some(entry);
                }
                if let Some(entry) = changed {
                    match self.acquire(&entry) {
                        Ok(record) => replacements.push((ordinal, record)),
                        Err(error) => {
                            self.release_all(replacements.into_iter().map(|(_, record)| record));
                            return Err(error);
                        }
                    }
                }
            }
        }
        let mut additions = Vec::with_capacity(creates.len());
        for entry in creates {
            match self.acquire(&entry) {
                Ok(record) => additions.push(record),
                Err(error) => {
                    self.release_all(replacements.into_iter().map(|(_, record)| record));
                    self.release_all(additions);
                    return Err(error);
                }
            }
        }
        for (ordinal, replacement) in replacements {
            let previous = std::mem::replace(&mut self.records[ordinal], replacement);
            self.release(previous);
        }
        if !excluded.is_empty() {
            let excluded_prefixes: Vec<_> =
                excluded.iter().map(|path| format!("{path}/")).collect();
            let excluded_paths: Vec<&str> = excluded.iter().map(String::as_str).collect();
            let (affected_parents, affected_raw_paths) = self.affected_masks(&excluded_paths);
            self.remove_records_matching(|index, record| {
                let affected = if record.raw_path {
                    marked(&affected_raw_paths, record.path)
                } else {
                    marked(&affected_parents, record.path)
                };
                if !affected {
                    return false;
                }
                let view = index.record_view(record).unwrap();
                excluded.iter().any(|path| view.path_equals(path))
                    || excluded_prefixes
                        .iter()
                        .any(|prefix| view.path_starts_with(prefix))
            });
        }
        let prefixes: Vec<_> = removes.iter().map(|path| format!("{path}/")).collect();
        let upsert_ids: HashSet<_> = additions
            .iter()
            .filter(|record| !record.raw_path)
            .map(|record| (record.path, record.name))
            .collect();
        let raw_upserts: HashSet<_> = additions
            .iter()
            .filter(|record| record.raw_path)
            .map(|record| record.path)
            .collect();
        let raw_keys: HashSet<_> = additions
            .iter()
            .filter(|record| record.raw_path)
            .map(|record| split_path(self.raw_paths.get(record.path).unwrap()))
            .collect();
        let raw_keys: Vec<_> = raw_keys
            .into_iter()
            .map(|(parent, name)| (parent.to_owned(), name.to_owned()))
            .collect();
        let upsert_keys: Vec<_> = additions
            .iter()
            .filter(|_| self.raw_paths.len() > 0)
            .map(|record| {
                let view = self.record_view(record).unwrap();
                let (parent, name) = view.path_key();
                (parent.to_owned(), name.to_owned())
            })
            .collect();
        let remove_paths: Vec<&str> = removes.iter().map(String::as_str).collect();
        let (affected_parents, affected_raw_paths) = self.affected_masks(&remove_paths);
        let removed = self.remove_records_matching(|index, record| {
            if !record.raw_path && upsert_ids.contains(&(record.path, record.name)) {
                return true;
            }
            if record.raw_path && raw_upserts.contains(&record.path) {
                return true;
            }
            // Only a record the masks mark can match `removes`, but a key match
            // lands on a parent rather than a path, so a side still holding key
            // work has to fall through to the walk.
            let skip = if record.raw_path {
                upsert_keys.is_empty() && !marked(&affected_raw_paths, record.path)
            } else {
                raw_keys.is_empty() && !marked(&affected_parents, record.path)
            };
            if skip {
                return false;
            }
            let view = index.record_view(record).unwrap();
            removes.iter().any(|path| view.path_equals(path))
                || prefixes.iter().any(|prefix| view.path_starts_with(prefix))
                || if record.raw_path {
                    let (parent, name) = view.path_key();
                    upsert_keys.iter().any(|(expected_parent, expected_name)| {
                        expected_parent == parent && expected_name == name
                    })
                } else {
                    let (parent, name) = view.path_key();
                    raw_keys.iter().any(|(expected_parent, expected_name)| {
                        expected_parent == parent && expected_name == name
                    })
                }
        });
        let added = additions.len();
        self.records.reserve_exact(added);
        self.records.extend(additions);
        Ok((added, removed))
    }

    pub fn reconcile(&mut self, fresh: &Self) -> Result<(), IndexError> {
        let existing: HashSet<_> = self.iter().map(|(_, view)| view.path_key()).collect();
        let added: Vec<_> = fresh
            .iter()
            .filter(|(_, view)| !existing.contains(&view.path_key()))
            .map(|(id, _)| id)
            .collect();
        drop(existing);
        let mut staged = Vec::with_capacity(added.len());
        for id in added {
            match self.acquire(&fresh.materialize(id)?) {
                Ok(record) => staged.push(record),
                Err(error) => {
                    self.release_all(staged);
                    return Err(error);
                }
            }
        }
        let current: HashSet<_> = fresh.iter().map(|(_, view)| view.path_key()).collect();
        self.records.reserve_exact(staged.len());
        self.records.extend(staged);
        self.remove_matching(|view| !current.contains(&view.path_key()));
        Ok(())
    }

    pub fn pool_counts(&self) -> (usize, usize, usize, usize) {
        (
            self.names.len(),
            self.parents.len(),
            self.extensions.len(),
            self.raw_paths.len(),
        )
    }

    pub fn memory_stats(&self) -> IndexMemoryStats {
        let records_bytes = self.records.capacity() * std::mem::size_of::<CompactRecord>();
        let pools_estimated_bytes = self.names.estimated_bytes()
            + self.parents.estimated_bytes()
            + self.extensions.estimated_bytes()
            + self.raw_paths.estimated_bytes();
        IndexMemoryStats {
            records_bytes,
            pools_estimated_bytes,
            total_estimated_bytes: records_bytes + pools_estimated_bytes,
        }
    }

    pub fn validate(&self) -> Result<(), IndexError> {
        let mut names = self.names.reference_counts();
        let mut parents = self.parents.reference_counts();
        let mut extensions = self.extensions.reference_counts();
        let mut raw_paths = self.raw_paths.reference_counts();
        for record in &self.records {
            self.record_view(record)
                .ok_or(IndexError::InvalidReference)?;
            names[record.name as usize] += 1;
            extensions[record.extension as usize] += 1;
            if record.raw_path {
                raw_paths[record.path as usize] += 1;
            } else {
                parents[record.path as usize] += 1;
            }
        }
        self.names.validate_references(&names)?;
        self.parents.validate_references(&parents)?;
        self.extensions.validate_references(&extensions)?;
        self.raw_paths.validate_references(&raw_paths)
    }

    pub(crate) fn to_cache(&self) -> CacheIndex {
        let (names, name_ids) = self.names.cache_strings();
        let (parents, parent_ids) = self.parents.cache_strings();
        let (extensions, extension_ids) = self.extensions.cache_strings();
        let (raw_paths, raw_ids) = self.raw_paths.cache_strings();
        let records = self
            .records
            .iter()
            .map(|record| CompactRecord {
                name: name_ids[record.name as usize],
                extension: extension_ids[record.extension as usize],
                path: if record.raw_path {
                    raw_ids[record.path as usize]
                } else {
                    parent_ids[record.path as usize]
                },
                ..*record
            })
            .collect();
        CacheIndex {
            names,
            parents,
            extensions,
            raw_paths,
            records,
        }
    }

    pub(crate) fn from_cache(cache: CacheIndex) -> Result<Self, IndexError> {
        for record in &cache.records {
            if !record.raw_path {
                let name = cache
                    .names
                    .get(record.name as usize)
                    .ok_or(IndexError::InvalidReference)?;
                let parent = cache
                    .parents
                    .get(record.path as usize)
                    .ok_or(IndexError::InvalidReference)?;
                if name.is_empty()
                    || name.contains('/')
                    || (!parent.is_empty() && !parent.ends_with('/'))
                {
                    return Err(IndexError::InvalidReference);
                }
            }
        }
        let mut names = vec![0usize; cache.names.len()];
        let mut parents = vec![0usize; cache.parents.len()];
        let mut extensions = vec![0usize; cache.extensions.len()];
        let mut raw_paths = vec![0usize; cache.raw_paths.len()];
        for record in &cache.records {
            *names
                .get_mut(record.name as usize)
                .ok_or(IndexError::InvalidReference)? += 1;
            *extensions
                .get_mut(record.extension as usize)
                .ok_or(IndexError::InvalidReference)? += 1;
            let paths = if record.raw_path {
                &mut raw_paths
            } else {
                &mut parents
            };
            *paths
                .get_mut(record.path as usize)
                .ok_or(IndexError::InvalidReference)? += 1;
        }
        Ok(Self {
            records: cache.records,
            names: StringPool::from_cache(cache.names, names)?,
            parents: StringPool::from_cache(cache.parents, parents)?,
            extensions: StringPool::from_cache(cache.extensions, extensions)?,
            raw_paths: StringPool::from_cache(cache.raw_paths, raw_paths)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_cache_decode_borrows_strings_and_compact_outlives_payload() {
        let originals = vec![
            FileEntry {
                name: "中文.rs".into(),
                path: "/space dir/中文.rs".into(),
                extension: "rs".into(),
                is_dir: false,
                size: u64::MAX,
                modified: u64::MAX,
                created_at: u64::MAX,
            },
            FileEntry {
                name: "alias".into(),
                path: "/actual".into(),
                extension: "".into(),
                is_dir: true,
                size: 0,
                modified: 0,
                created_at: 0,
            },
        ];
        let compact = {
            let payload = bitcode::encode(&originals);
            let decoded: Vec<BorrowedFileEntry<'_>> = bitcode::decode(&payload).unwrap();
            let payload_start = payload.as_ptr() as usize;
            let payload_end = payload_start + payload.len();
            for record in &decoded {
                for text in [record.name, record.path, record.extension] {
                    if !text.is_empty() {
                        assert!((payload_start..payload_end).contains(&(text.as_ptr() as usize)));
                        assert!(text.as_ptr() as usize + text.len() <= payload_end);
                    }
                }
            }
            CompactIndex::from_borrowed_entries(decoded).unwrap()
        };
        for (ordinal, original) in originals.iter().enumerate() {
            assert_eq!(&compact.materialize(EntryId(ordinal)).unwrap(), original);
        }
        compact.validate().unwrap();
    }

    fn cache_fixture() -> CacheIndex {
        CompactIndex::from_entries([FileEntry {
            name: "main.rs".into(),
            path: "/src/main.rs".into(),
            extension: "rs".into(),
            is_dir: false,
            size: u64::MAX,
            modified: u64::MAX,
            created_at: u64::MAX,
        }])
        .unwrap()
        .to_cache()
    }

    #[test]
    fn cache_rejects_each_out_of_range_reference() {
        for field in 0..4 {
            let mut cache = cache_fixture();
            match field {
                0 => cache.records[0].name = u32::MAX,
                1 => cache.records[0].extension = u32::MAX,
                2 => cache.records[0].path = u32::MAX,
                _ => cache.records[0].raw_path = true,
            }
            assert!(matches!(
                CompactIndex::from_cache(cache),
                Err(IndexError::InvalidReference)
            ));
        }
    }

    #[test]
    fn cache_rejects_duplicate_and_unused_strings() {
        let mut duplicate = cache_fixture();
        duplicate.names.push(duplicate.names[0].clone());
        let mut second = duplicate.records[0];
        second.name = 1;
        duplicate.records.push(second);
        assert!(matches!(
            CompactIndex::from_cache(duplicate),
            Err(IndexError::InvalidReference)
        ));

        let mut unused = cache_fixture();
        unused.parents.push("/unused/".into());
        assert!(matches!(
            CompactIndex::from_cache(unused),
            Err(IndexError::InvalidReference)
        ));
    }

    #[test]
    fn cache_rejects_invalid_split_path_representation() {
        for field in 0..3 {
            let mut cache = cache_fixture();
            match field {
                0 => cache.parents[0] = "/src".into(),
                1 => cache.names[0] = "sub/main.rs".into(),
                _ => cache.names[0].clear(),
            }
            assert!(matches!(
                CompactIndex::from_cache(cache),
                Err(IndexError::InvalidReference)
            ));
        }
    }
}
