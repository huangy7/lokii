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
    pub(crate) name: u32,
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

pub(crate) struct SortedNames {
    pub(crate) ids: Vec<u32>,
    membership: Vec<u64>,
}

pub(crate) struct QueryIndex {
    /// 每个名字池槽位对应的记录下标列表（CSR 扁平存储）。
    name_entries_off: Vec<u32>,
    name_entries: Vec<u32>,
    /// 按名字池槽位索引的签名；槽位复用时会清空，下次刷新按需重算。
    signatures: Vec<crate::string_pool::NameSignature>,
    /// `signatures` 的有效位；签名本身可能全零，不能拿零值当“未计算”。
    signature_ready: Vec<u64>,
    /// 等分查询（通配/正则/扩展名）需要的字典序名字表；按需构建，变更后失效。
    sorted: std::sync::Mutex<Option<std::sync::Arc<SortedNames>>>,
    /// 仅缓存“小写折叠后与原串不同”的非 ASCII 名字；CJK 小写恒等，不再复制一份。
    folded_non_ascii: std::collections::HashMap<u32, Box<str>>,
}

impl Default for QueryIndex {
    fn default() -> Self {
        Self {
            name_entries_off: Vec::new(),
            name_entries: Vec::new(),
            signatures: Vec::new(),
            signature_ready: Vec::new(),
            sorted: std::sync::Mutex::new(None),
            folded_non_ascii: std::collections::HashMap::new(),
        }
    }
}

impl QueryIndex {
    fn build(records: &[CompactRecord], names: &crate::string_pool::StringPool) -> Self {
        let mut index = Self::default();
        index.refresh(records, names);
        index
    }

    pub(crate) fn slot_count(&self) -> usize {
        self.signatures.len()
    }

    fn has_entries(&self, name_id: u32) -> bool {
        let slot = name_id as usize;
        match (
            self.name_entries_off.get(slot),
            self.name_entries_off.get(slot + 1),
        ) {
            (Some(start), Some(end)) => start < end,
            _ => false,
        }
    }

    fn set_ready(&mut self, name_id: u32) {
        let slot = name_id as usize;
        let word = slot / 64;
        if word >= self.signature_ready.len() {
            self.signature_ready.resize(word + 1, 0);
        }
        self.signature_ready[word] |= 1u64 << (slot % 64);
    }

    pub(crate) fn folded(&self, name_id: u32) -> Option<&str> {
        self.folded_non_ascii.get(&name_id).map(|text| text.as_ref())
    }

    pub(crate) fn signature(&self, name_id: u32) -> Option<crate::string_pool::NameSignature> {
        if self.is_ready(name_id) {
            self.signatures.get(name_id as usize).copied()
        } else {
            None
        }
    }

    /// 热路径使用：直接读槽位签名。调用方需先用 `is_ready` 兜住全零签名，
    /// 或者确认该槽位一定有条目（构建/刷新保证存活槽位已计算）。
    pub(crate) fn signature_at(&self, name_id: u32) -> crate::string_pool::NameSignature {
        self.signatures
            .get(name_id as usize)
            .copied()
            .unwrap_or_default()
    }

    pub(crate) fn is_ready(&self, name_id: u32) -> bool {
        let slot = name_id as usize;
        self.signature_ready
            .get(slot / 64)
            .is_some_and(|word| word & (1u64 << (slot % 64)) != 0)
    }

    pub(crate) fn entries(&self, name_id: u32) -> &[u32] {
        let slot = name_id as usize;
        let (Some(start), Some(end)) = (
            self.name_entries_off.get(slot),
            self.name_entries_off.get(slot + 1),
        ) else {
            return &[];
        };
        &self.name_entries[*start as usize..*end as usize]
    }

    fn set_name(&mut self, name_id: u32, name: &str) {
        let folded = if name.is_ascii() {
            None
        } else {
            let lower = name.to_lowercase();
            (lower != name).then(|| lower.into_boxed_str())
        };
        self.signatures[name_id as usize] =
            crate::string_pool::NameSignature::from_name(name, folded.as_deref());
        self.set_ready(name_id);
        match folded {
            Some(folded) => {
                self.folded_non_ascii.insert(name_id, folded);
            }
            None => {
                self.folded_non_ascii.remove(&name_id);
            }
        }
    }

    fn clear_name(&mut self, name_id: u32) {
        let slot = name_id as usize;
        if let Some(signature) = self.signatures.get_mut(slot) {
            *signature = crate::string_pool::NameSignature::default();
        }
        let word = slot / 64;
        if let Some(word) = self.signature_ready.get_mut(word) {
            *word &= !(1u64 << (slot % 64));
        }
        self.folded_non_ascii.remove(&name_id);
        self.invalidate_sorted();
    }

    fn invalidate_sorted(&self) {
        let mut sorted = self
            .sorted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *sorted = None;
    }

    /// 纯新增时把新名字并进已排序表，避免为一次写入重排全部唯一名。
    /// 删除/改名/槽位复用会让排序表失效（`clear_name`），此时直接跳过，等下一次等分查询重建。
    fn merge_new_sorted_names(&mut self, names: &crate::string_pool::StringPool) {
        let mut sorted = self
            .sorted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(existing) = sorted.take() else {
            return;
        };

        let mut new_ids: Vec<u32> = (0..self.slot_count() as u32)
            .filter(|name_id| {
                let slot = *name_id as usize;
                self.has_entries(*name_id)
                    && existing
                        .membership
                        .get(slot / 64)
                        .is_none_or(|word| word & (1u64 << (slot % 64)) == 0)
            })
            .collect();
        if new_ids.is_empty() {
            *sorted = Some(existing);
            return;
        }

        new_ids.sort_unstable_by(|&left, &right| {
            let left_name = names.get(left).unwrap_or("");
            let right_name = names.get(right).unwrap_or("");
            left_name.cmp(right_name).then(left.cmp(&right))
        });

        let mut ids = Vec::with_capacity(existing.ids.len() + new_ids.len());
        let (mut old, mut new) = (0usize, 0usize);
        while old < existing.ids.len() || new < new_ids.len() {
            let take_old = match (existing.ids.get(old), new_ids.get(new)) {
                (Some(&a), Some(&b)) => names.get(a).unwrap_or("") <= names.get(b).unwrap_or(""),
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if take_old {
                ids.push(existing.ids[old]);
                old += 1;
            } else {
                ids.push(new_ids[new]);
                new += 1;
            }
        }

        let mut membership = existing.membership.clone();
        let words = self.slot_count().div_ceil(64);
        if membership.len() < words {
            membership.resize(words, 0);
        }
        for &name_id in &new_ids {
            let slot = name_id as usize;
            membership[slot / 64] |= 1u64 << (slot % 64);
        }
        *sorted = Some(std::sync::Arc::new(SortedNames { ids, membership }));
    }

    /// 只重建 O(records) 的 CSR，并补算新增名字的签名；不再每次变更都排序全部唯一名。
    fn refresh(&mut self, records: &[CompactRecord], names: &crate::string_pool::StringPool) {
        let slots = names.slot_len();
        let mut counts = vec![0u32; slots];
        for record in records {
            if let Some(count) = counts.get_mut(record.name as usize) {
                *count += 1;
            }
        }

        self.name_entries_off.clear();
        self.name_entries_off.reserve(slots + 1);
        self.name_entries_off.push(0);
        let mut total = 0u32;
        for &count in &counts {
            total += count;
            self.name_entries_off.push(total);
        }

        self.name_entries.clear();
        if self.name_entries.capacity() < records.len() {
            self.name_entries.reserve_exact(records.len());
        }
        self.name_entries.resize(records.len(), 0);
        let mut cursors = self.name_entries_off[..slots].to_vec();
        for (ordinal, record) in records.iter().enumerate() {
            if let Some(cursor) = cursors.get_mut(record.name as usize) {
                self.name_entries[*cursor as usize] = ordinal as u32;
                *cursor += 1;
            }
        }

        self.signatures
            .resize(slots, crate::string_pool::NameSignature::default());
        self.signature_ready.resize(slots.div_ceil(64), 0);

        for name_id in 0..slots {
            let name_id = name_id as u32;
            if !self.has_entries(name_id) || self.is_ready(name_id) {
                continue;
            }
            if let Some(name) = names.get(name_id) {
                self.set_name(name_id, name);
            }
        }

        // 槽位释放时已经清掉折叠串；这里不再每次刷新遍历整张表，
        // 否则中文名字占比高时会把一次增量更新拖成 O(非 ASCII 名字数)。
        self.merge_new_sorted_names(names);
    }

    fn estimated_bytes(&self) -> usize {
        let folded_bytes: usize = self
            .folded_non_ascii
            .values()
            .map(|text| text.len() + std::mem::size_of::<Box<str>>() + 16)
            .sum();
        let sorted_bytes = self
            .sorted
            .lock()
            .ok()
            .and_then(|sorted| {
                sorted.as_ref().map(|names| {
                    names.ids.capacity() * 4 + names.membership.capacity() * 8
                })
            })
            .unwrap_or(0);
        self.name_entries_off.capacity() * 4
            + self.name_entries.capacity() * 4
            + self.signatures.capacity() * std::mem::size_of::<crate::string_pool::NameSignature>()
            + self.signature_ready.capacity() * 8
            + folded_bytes
            + sorted_bytes
    }
}

pub struct CompactIndex {
    records: Vec<CompactRecord>,
    names: StringPool,
    parents: StringPool,
    extensions: StringPool,
    raw_paths: StringPool,
    query_index: std::sync::OnceLock<QueryIndex>,
}

impl Default for CompactIndex {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            names: StringPool::default(),
            parents: StringPool::default(),
            extensions: StringPool::default(),
            raw_paths: StringPool::default(),
            query_index: std::sync::OnceLock::new(),
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct IndexMemoryStats {
    pub records_bytes: usize,
    pub pools_estimated_bytes: usize,
    /// 查询侧 CSR、签名与折叠串的估算值；这部分此前不在统计口径内。
    pub query_index_bytes: usize,
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
        // 槽位可能随后被新名字复用；旧签名/折叠串必须先失效，
        // 否则下一次查询会把新名字当成旧名字的缓存。
        if self.names.ref_count(record.name) == 0 {
            if let Some(index) = self.query_index.get_mut() {
                index.clear_name(record.name);
            }
        }
    }

    /// Release every record staged for a batch that is being abandoned.
    fn release_all(&mut self, records: impl IntoIterator<Item = CompactRecord>) {
        for record in records {
            self.release(record);
        }
    }

    fn refresh_query_index(&mut self) {
        let Self {
            query_index,
            records,
            names,
            ..
        } = self;
        if let Some(index) = query_index.get_mut() {
            index.refresh(records, names);
        }
    }

    fn ensure_query_index(&self) -> &QueryIndex {
        self.query_index
            .get_or_init(|| QueryIndex::build(&self.records, &self.names))
    }

    #[inline]
    pub(crate) fn sorted_names(&self) -> std::sync::Arc<SortedNames> {
        let index = self.ensure_query_index();
        let mut sorted = index
            .sorted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(names) = sorted.as_ref() {
            return std::sync::Arc::clone(names);
        }
        let mut ids: Vec<u32> = (0..index.slot_count() as u32)
            .filter(|name_id| index.has_entries(*name_id))
            .collect();
        ids.sort_unstable_by(|&left, &right| {
            let left_name = self.names.get(left).unwrap_or("");
            let right_name = self.names.get(right).unwrap_or("");
            left_name.cmp(right_name).then(left.cmp(&right))
        });
        let mut membership = vec![0u64; index.slot_count().div_ceil(64)];
        for &name_id in &ids {
            let slot = name_id as usize;
            membership[slot / 64] |= 1u64 << (slot % 64);
        }
        let names = std::sync::Arc::new(SortedNames { ids, membership });
        *sorted = Some(std::sync::Arc::clone(&names));
        names
    }

    #[inline]
    pub(crate) fn query_index(&self) -> &QueryIndex {
        self.ensure_query_index()
    }

    #[inline]
    pub(crate) fn name_text(&self, name_id: u32) -> Option<&str> {
        self.names.get(name_id)
    }

    #[inline]
    pub(crate) fn name_signature(&self, name_id: u32) -> Option<crate::string_pool::NameSignature> {
        self.ensure_query_index().signature(name_id)
    }

    /// 在后台线程预热查询侧索引，避免用户第一次按键为构建 CSR/签名付费。
    pub fn warm_query_index(&self) {
        let _ = self.ensure_query_index();
        let _ = self.sorted_names();
    }

    #[inline]
    pub(crate) fn name_entry(&self, name_id: u32) -> Option<(&str, Option<&str>)> {
        let name = self.names.get(name_id)?;
        let folded = if name.is_ascii() {
            None
        } else {
            self.ensure_query_index()
                .folded_non_ascii
                .get(&name_id)
                .map(|text| text.as_ref())
        };
        Some((name, folded))
    }

    #[inline]
    pub(crate) fn entries_for_name(&self, name_id: u32) -> &[u32] {
        self.ensure_query_index().entries(name_id)
    }

    pub fn push(&mut self, entry: FileEntry) -> Result<(), IndexError> {
        let record = self.acquire(&entry)?;
        self.records.push(record);
        self.refresh_query_index();
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

    pub fn remove_matching(&mut self, predicate: impl FnMut(EntryView<'_>) -> bool) -> usize {
        let removed = self.remove_without_refresh(predicate);
        self.refresh_query_index();
        removed
    }

    fn remove_without_refresh(
        &mut self,
        mut predicate: impl FnMut(EntryView<'_>) -> bool,
    ) -> usize {
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
        self.refresh_query_index();
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
        let removed = self.remove_without_refresh(predicate);
        self.records.reserve_exact(staged.len());
        self.records.extend(staged);
        self.refresh_query_index();
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
        self.refresh_query_index();
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
        // 统计本批次 additions 对每个 parent 槽位的引用数；只有当池中总引用数大于本批次引用数时，
        // 说明旧记录列表 `self.records` 中才可能存在使用该 parent 的条目。
        let mut batch_parent_counts: Vec<u32> = Vec::new();
        for record in &additions {
            if !record.raw_path {
                let p_idx = record.path as usize;
                if p_idx >= batch_parent_counts.len() {
                    batch_parent_counts.resize(p_idx + 1, 0);
                }
                batch_parent_counts[p_idx] += 1;
            }
        }
        let mut upsert_parents = Vec::new();
        let mut upsert_ids = HashSet::new();
        for record in &additions {
            if !record.raw_path {
                let p_idx = record.path as usize;
                let parent_in_existing =
                    self.parents.ref_count(record.path) > batch_parent_counts[p_idx] as usize;
                let name_in_existing = self.names.ref_count(record.name) > 1;
                if parent_in_existing && name_in_existing {
                    if p_idx >= upsert_parents.len() {
                        upsert_parents.resize(p_idx + 1, false);
                    }
                    upsert_parents[p_idx] = true;
                    upsert_ids.insert((record.path, record.name));
                }
            }
        }
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

        let removed = if removes.is_empty()
            && raw_upserts.is_empty()
            && raw_keys.is_empty()
            && upsert_keys.is_empty()
            && upsert_ids.is_empty()
        {
            0
        } else {
            self.remove_records_matching(|index, record| {
                if !record.raw_path
                    && marked(&upsert_parents, record.path)
                    && upsert_ids.contains(&(record.path, record.name))
                {
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
            })
        };
        let added = additions.len();
        self.records.reserve_exact(added);
        self.records.extend(additions);
        self.refresh_query_index();
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
        self.remove_without_refresh(|view| !current.contains(&view.path_key()));
        self.refresh_query_index();
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
        let query_index_bytes = self
            .query_index
            .get()
            .map_or(0, |index| index.estimated_bytes());
        IndexMemoryStats {
            records_bytes,
            pools_estimated_bytes,
            query_index_bytes,
            total_estimated_bytes: records_bytes + pools_estimated_bytes + query_index_bytes,
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
        let mut names = vec![0u32; cache.names.len()];
        let mut parents = vec![0u32; cache.parents.len()];
        let mut extensions = vec![0u32; cache.extensions.len()];
        let mut raw_paths = vec![0u32; cache.raw_paths.len()];
        for record in &cache.records {
            let n = names
                .get_mut(record.name as usize)
                .ok_or(IndexError::InvalidReference)?;
            *n = n.checked_add(1).ok_or(IndexError::ReferenceOverflow)?;
            let e = extensions
                .get_mut(record.extension as usize)
                .ok_or(IndexError::InvalidReference)?;
            *e = e.checked_add(1).ok_or(IndexError::ReferenceOverflow)?;
            let paths = if record.raw_path {
                &mut raw_paths
            } else {
                &mut parents
            };
            let p = paths
                .get_mut(record.path as usize)
                .ok_or(IndexError::InvalidReference)?;
            *p = p.checked_add(1).ok_or(IndexError::ReferenceOverflow)?;
        }
        Ok(Self {
            records: cache.records,
            names: StringPool::from_cache(cache.names, names)?,
            parents: StringPool::from_cache(cache.parents, parents)?,
            extensions: StringPool::from_cache(cache.extensions, extensions)?,
            raw_paths: StringPool::from_cache(cache.raw_paths, raw_paths)?,
            query_index: std::sync::OnceLock::new(),
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
