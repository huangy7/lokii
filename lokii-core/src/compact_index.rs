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
pub struct CompactRecord {
    pub(crate) name: u32,
    /// 父目录池槽位；原始路径记录则为路径池槽位。
    pub(crate) path: u32,
    pub(crate) extension: u32,
    pub(crate) raw_path: bool,
    pub(crate) is_dir: bool,
    pub(crate) size: u64,
    pub(crate) modified: u64,
    pub(crate) created_at: u64,
}

impl CompactRecord {
    #[cfg(test)]
    pub(crate) fn from_raw_parts(is_dir: bool, extension: u32) -> Self {
        Self {
            name: 0,
            path: 0,
            extension,
            raw_path: false,
            is_dir,
            size: 0,
            modified: 0,
            created_at: 0,
        }
    }
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
    /// 基于外部实时扫描到的元数据字段构造只读条目视图，供未索引目录兜底列举复用过滤器。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_parts(
        name: &'a str,
        parent: &'a str,
        extension: &'a str,
        is_dir: bool,
        size: u64,
        modified: u64,
        created_at: u64,
    ) -> Self {
        Self {
            name,
            parent,
            extension,
            is_dir,
            size,
            modified,
            created_at,
            raw_path: None,
        }
    }

    #[inline]
    pub(crate) fn raw_path(&self) -> Option<&'a str> {
        self.raw_path
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
}

#[derive(Default)]
struct CsrTable {
    /// Base CSR 构建时覆盖的 `records` 前缀长度（`0..base_records_len`）。
    base_records_len: usize,
    /// 是否存在发生在 `0..base_records_len` 范围内的原地改名操作。
    has_base_renames: bool,
    /// 每个名字池槽位对应的记录下标列表（CSR 扁平存储）。
    name_entries_off: Vec<u32>,
    name_entries: Vec<u32>,
    /// 增量追加或原地改名后覆盖的 `name_id -> ordinals` 升序列表；
    /// 命中此表的 `name_id` 直接返回切片，避免增量写入时重建全量 Base CSR。
    overflow_entries: std::collections::HashMap<u32, Vec<u32>>,
}

impl CsrTable {
    #[inline]
    fn base_slice(&self, name_id: u32) -> &[u32] {
        let slot = name_id as usize;
        let (Some(&start), Some(&end)) = (
            self.name_entries_off.get(slot),
            self.name_entries_off.get(slot + 1),
        ) else {
            return &[];
        };
        &self.name_entries[start as usize..end as usize]
    }

    #[inline]
    fn entries(&self, name_id: u32) -> &[u32] {
        if !self.overflow_entries.is_empty() {
            if let Some(list) = self.overflow_entries.get(&name_id) {
                return list.as_slice();
            }
        }
        self.base_slice(name_id)
    }

    /// 原地复用 `name_entries_off` 完成计数、前缀和与游标写入，零临时 buffer 分配。
    fn rebuild(&mut self, records: &[CompactRecord], slots: usize) {
        self.overflow_entries.clear();
        self.has_base_renames = false;
        self.base_records_len = records.len();

        self.name_entries_off.clear();
        self.name_entries_off.resize(slots + 2, 0);
        for record in records {
            if let Some(slot) = self.name_entries_off.get_mut(record.name as usize + 2) {
                *slot += 1;
            }
        }

        let mut total = 0u32;
        for slot in &mut self.name_entries_off[1..=slots + 1] {
            total += *slot;
            *slot = total;
        }

        self.name_entries.clear();
        if self.name_entries.capacity() < records.len() {
            self.name_entries
                .reserve_exact(records.len() - self.name_entries.capacity());
        }
        self.name_entries.resize(records.len(), 0);
        for (ordinal, record) in records.iter().enumerate() {
            if let Some(cursor) = self.name_entries_off.get_mut(record.name as usize + 1) {
                self.name_entries[*cursor as usize] = ordinal as u32;
                *cursor += 1;
            }
        }
        self.name_entries_off.truncate(slots + 1);
    }

    fn append_record(&mut self, name_id: u32, ordinal: u32) {
        if let Some(list) = self.overflow_entries.get_mut(&name_id) {
            list.push(ordinal);
            return;
        }
        let base = self.base_slice(name_id);
        let mut list = Vec::with_capacity(base.len() + 1);
        list.extend_from_slice(base);
        list.push(ordinal);
        self.overflow_entries.insert(name_id, list);
    }

    fn replace_record_name(&mut self, ordinal: u32, old_name: u32, new_name: u32) {
        if old_name == new_name {
            return;
        }
        if (ordinal as usize) < self.base_records_len {
            self.has_base_renames = true;
        }
        if !self.overflow_entries.contains_key(&old_name) {
            let base = self.base_slice(old_name).to_vec();
            self.overflow_entries.insert(old_name, base);
        }
        if let Some(old_list) = self.overflow_entries.get_mut(&old_name) {
            if let Ok(pos) = old_list.binary_search(&ordinal) {
                old_list.remove(pos);
            }
        }
        if !self.overflow_entries.contains_key(&new_name) {
            let base = self.base_slice(new_name).to_vec();
            self.overflow_entries.insert(new_name, base);
        }
        if let Some(new_list) = self.overflow_entries.get_mut(&new_name) {
            if let Err(pos) = new_list.binary_search(&ordinal) {
                new_list.insert(pos, ordinal);
            }
        }
    }

    fn rebuild_overflow(&mut self, records: &[CompactRecord]) {
        self.overflow_entries.clear();
        for (offset, record) in records[self.base_records_len..].iter().enumerate() {
            let ordinal = (self.base_records_len + offset) as u32;
            self.append_record(record.name, ordinal);
        }
    }

    fn clear_name(&mut self, name_id: u32) {
        if self.base_slice(name_id).is_empty() {
            self.overflow_entries.remove(&name_id);
        } else {
            self.overflow_entries.insert(name_id, Vec::new());
        }
    }

    fn estimated_bytes(&self) -> usize {
        let overflow_bytes: usize = self
            .overflow_entries
            .values()
            .map(|list| list.capacity() * 4 + std::mem::size_of::<Vec<u32>>() + 8)
            .sum();
        self.name_entries_off.capacity() * 4 + self.name_entries.capacity() * 4 + overflow_bytes
    }
}

/// `QueryIndex::parent_meta` 单字节位布局：
/// - bits 0..=5 (`0x3F`): 父目录路径深度（截断至 32，与排序深度上限 `ranking::MAX_DEPTH` 对齐）
/// - bit  6     (`0x40`): 槽位有效位（`0` 表示尚未计算或槽位已被释放）
/// - bit  7     (`0x80`): 父目录是否存在任意以 `.` 开头的隐藏段
const PARENT_META_DEPTH_MASK: u8 = 0x3F;
const PARENT_META_MAX_DEPTH: usize = 32;
const PARENT_META_VALID: u8 = 0x40;
const PARENT_META_HIDDEN: u8 = 0x80;

/// 零分配统计父路径中的非空目录层级数。
#[inline]
pub(crate) fn parent_depth(parent: &str) -> usize {
    let mut count = 0usize;
    let mut in_seg = false;
    for &b in parent.as_bytes() {
        if b == b'/' || b == b'\\' {
            in_seg = false;
        } else if !in_seg {
            in_seg = true;
            count += 1;
        }
    }
    count
}

/// 零分配检测父路径任意段是否以 `.` 开头（即位于隐藏目录子树下）。
#[inline]
pub(crate) fn is_parent_hidden(parent_bytes: &[u8]) -> bool {
    parent_bytes.first() == Some(&b'.') || parent_bytes.windows(2).any(|w| w == b"/.")
}

#[inline]
fn encode_parent_meta(parent: &str) -> u8 {
    let depth = parent_depth(parent).min(PARENT_META_MAX_DEPTH) as u8;
    let hidden = if is_parent_hidden(parent.as_bytes()) {
        PARENT_META_HIDDEN
    } else {
        0
    };
    PARENT_META_VALID | hidden | (depth & PARENT_META_DEPTH_MASK)
}

pub(crate) struct QueryIndex {
    /// 按需构建并支持增量溢出的 CSR 反表。
    csr: std::sync::OnceLock<CsrTable>,
    /// 当发生中间记录删除使 CSR 失效时，回收 `CsrTable` 以在下一次查询时零分配复用容量。
    csr_scratch: std::sync::Mutex<Option<CsrTable>>,
    /// 按名字池槽位索引的签名；槽位复用时会清空，下次新增按需重算。
    signatures: Vec<crate::string_pool::NameSignature>,
    /// `signatures` 的有效位；签名本身可能全零，不能拿零值当“未计算”。
    signature_ready: Vec<u64>,
    /// 按父目录池槽位缓存的单字节元数据（深度 + 隐藏标记 + 有效位），
    /// 让重名桶展开时的 `score_record` 退化为 O(1) 字节查表，免除父路径字符串解引用与双遍扫描。
    parent_meta: Vec<u8>,
    /// 等分查询（通配/正则/扩展名）需要的字典序名字表；按需构建，变更后失效。
    sorted: std::sync::Mutex<Option<std::sync::Arc<SortedNames>>>,
    /// 仅缓存“小写折叠后与原串不同”的非 ASCII 名字；CJK 小写恒等，不再复制一份。
    folded_non_ascii: std::collections::HashMap<u32, Box<str>>,
}

impl Default for QueryIndex {
    fn default() -> Self {
        Self {
            csr: std::sync::OnceLock::new(),
            csr_scratch: std::sync::Mutex::new(None),
            signatures: Vec::new(),
            signature_ready: Vec::new(),
            parent_meta: Vec::new(),
            sorted: std::sync::Mutex::new(None),
            folded_non_ascii: std::collections::HashMap::new(),
        }
    }
}

impl QueryIndex {
    fn build(
        records: &[CompactRecord],
        names: &crate::string_pool::StringPool,
        parents: &crate::string_pool::StringPool,
    ) -> Self {
        let mut index = Self::default();
        let slots = names.slot_len();
        index
            .signatures
            .resize(slots, crate::string_pool::NameSignature::default());
        index.signature_ready.resize(slots.div_ceil(64), 0);

        for name_id in 0..slots as u32 {
            if let Some(name) = names.get(name_id) {
                index.set_name(name_id, name);
            }
        }

        let parent_slots = parents.slot_len();
        index.parent_meta.resize(parent_slots, 0);
        for parent_id in 0..parent_slots as u32 {
            if let Some(parent) = parents.get(parent_id) {
                index.parent_meta[parent_id as usize] = encode_parent_meta(parent);
            }
        }

        let mut table = CsrTable::default();
        table.rebuild(records, slots);
        let _ = index.csr.set(table);
        index
    }

    #[inline]
    fn ensure_csr(&self, records: &[CompactRecord], slots: usize) -> &CsrTable {
        self.csr.get_or_init(|| {
            let mut table = self
                .csr_scratch
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
                .unwrap_or_default();
            table.rebuild(records, slots);
            table
        })
    }

    fn invalidate_csr(&mut self) {
        if let Some(mut table) = self.csr.take() {
            table.overflow_entries.clear();
            table.has_base_renames = false;
            *self
                .csr_scratch
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(table);
        }
    }

    pub(crate) fn slot_count(&self) -> usize {
        self.signatures.len()
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

    #[inline]
    pub(crate) fn entries(&self, name_id: u32) -> &[u32] {
        self.csr.get().map_or(&[], |table| table.entries(name_id))
    }

    #[inline]
    pub(crate) fn parent_meta(&self, parent_id: u32) -> Option<(usize, bool)> {
        let meta = *self.parent_meta.get(parent_id as usize)?;
        if (meta & PARENT_META_VALID) == 0 {
            return None;
        }
        Some((
            (meta & PARENT_META_DEPTH_MASK) as usize,
            (meta & PARENT_META_HIDDEN) != 0,
        ))
    }

    fn set_parent(&mut self, parent_id: u32, parent: &str) {
        let slot = parent_id as usize;
        if slot >= self.parent_meta.len() {
            self.parent_meta.resize(slot + 1, 0);
        }
        self.parent_meta[slot] = encode_parent_meta(parent);
    }

    fn clear_parent(&mut self, parent_id: u32) {
        if let Some(meta) = self.parent_meta.get_mut(parent_id as usize) {
            *meta = 0;
        }
    }

    fn register_parent(&mut self, parent_id: u32, parents: &crate::string_pool::StringPool) {
        let slot = parent_id as usize;
        if self
            .parent_meta
            .get(slot)
            .is_some_and(|&meta| (meta & PARENT_META_VALID) != 0)
        {
            return;
        }
        if let Some(parent) = parents.get(parent_id) {
            self.set_parent(parent_id, parent);
        }
    }

    /// 挂载点前缀原地重映射后按父目录池重算 `parent_meta`，保留签名、CSR 与折叠表免全量重建。
    fn refresh_all_parents(&mut self, parents: &crate::string_pool::StringPool) {
        let parent_slots = parents.slot_len();
        self.parent_meta.clear();
        self.parent_meta.resize(parent_slots, 0);
        for parent_id in 0..parent_slots as u32 {
            if let Some(parent) = parents.get(parent_id) {
                self.parent_meta[parent_id as usize] = encode_parent_meta(parent);
            }
        }
    }

    fn set_name(&mut self, name_id: u32, name: &str) {
        let folded = if name.is_ascii() || !crate::cjk::needs_fold(name) {
            None
        } else {
            Some(crate::cjk::fold_str(name).into_boxed_str())
        };
        let slot = name_id as usize;
        if slot >= self.signatures.len() {
            self.signatures
                .resize(slot + 1, crate::string_pool::NameSignature::default());
        }
        self.signatures[slot] =
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
        if let Some(csr) = self.csr.get_mut() {
            csr.clear_name(name_id);
        }
        self.invalidate_sorted();
    }

    fn invalidate_sorted(&self) {
        let mut sorted = self
            .sorted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *sorted = None;
    }

    /// 仅为本批次新注册的记录补算签名与父目录元数据；若出现新唯一名则以 O(1) 惰性失效字典序名字表，
    /// 避免在独占写锁内拷贝并归并数十万条 `SortedNames`，推迟到通配符/正则查询按需重建。
    fn register_records(
        &mut self,
        records: &[CompactRecord],
        names: &crate::string_pool::StringPool,
        parents: &crate::string_pool::StringPool,
    ) {
        let slots = names.slot_len();
        if self.signatures.len() < slots {
            self.signatures
                .resize(slots, crate::string_pool::NameSignature::default());
        }
        let words = slots.div_ceil(64);
        if self.signature_ready.len() < words {
            self.signature_ready.resize(words, 0);
        }
        let parent_slots = parents.slot_len();
        if self.parent_meta.len() < parent_slots {
            self.parent_meta.resize(parent_slots, 0);
        }

        let mut has_new_name = false;
        for record in records {
            let name_id = record.name;
            if !self.is_ready(name_id) {
                if let Some(name) = names.get(name_id) {
                    self.set_name(name_id, name);
                    has_new_name = true;
                }
            }
            if !record.raw_path {
                self.register_parent(record.path, parents);
            }
        }

        if has_new_name {
            self.invalidate_sorted();
        }
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
            .and_then(|sorted| sorted.as_ref().map(|names| names.ids.capacity() * 4))
            .unwrap_or(0);
        let csr_bytes = self.csr.get().map_or_else(
            || {
                self.csr_scratch
                    .lock()
                    .ok()
                    .and_then(|scratch| scratch.as_ref().map(CsrTable::estimated_bytes))
                    .unwrap_or(0)
            },
            CsrTable::estimated_bytes,
        );
        csr_bytes
            + self.signatures.capacity() * std::mem::size_of::<crate::string_pool::NameSignature>()
            + self.signature_ready.capacity() * 8
            + self.parent_meta.capacity()
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

/// 拥有型缓存载荷结构：供线格式一致性单元测试与 `examples/migration_profile` 基准工具使用，
/// 生产热启动路径已全面切换为零拷贝 [`BorrowedCacheIndex`]。
#[allow(dead_code)]
#[derive(bitcode::Encode, bitcode::Decode)]
pub(crate) struct CacheIndex {
    names: Vec<String>,
    parents: Vec<String>,
    extensions: Vec<String>,
    raw_paths: Vec<String>,
    records: Vec<CompactRecord>,
}

/// 与 [`CacheIndex`] 字段顺序一致、线格式相同；直接借用池内文本，存盘时不再为每个串单独分配一份。
#[derive(bitcode::Encode)]
struct CacheIndexRef<'a> {
    names: Vec<&'a str>,
    parents: Vec<&'a str>,
    extensions: Vec<&'a str>,
    raw_paths: Vec<&'a str>,
    records: Vec<CompactRecord>,
}

/// 紧凑池无空洞时的四表借用视图：配合 `bitcode::encode(&self.records)` 拼接，
/// 与 [`CacheIndex`] 保持逐字节相同的线格式，同时避免克隆整个 `Vec<CompactRecord>`。
#[derive(bitcode::Encode)]
struct CacheStringsRef<'a> {
    names: Vec<&'a str>,
    parents: Vec<&'a str>,
    extensions: Vec<&'a str>,
    raw_paths: Vec<&'a str>,
}

/// 与 [`CacheIndex`] 线格式完全一致的零拷贝解码视图：直接借用原始文件缓冲中的字符串切片，
/// 交由 [`StringPool::from_cache`] 一步转入 `Box<str>`，消除热启动期数十万次临时 `String` 堆分配。
#[derive(bitcode::Decode)]
pub(crate) struct BorrowedCacheIndex<'a> {
    names: Vec<&'a str>,
    parents: Vec<&'a str>,
    extensions: Vec<&'a str>,
    raw_paths: Vec<&'a str>,
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
struct CompiledPathMatcher {
    subtree_parents: Vec<bool>,
    exact_parents: Vec<bool>,
    exact_ids: HashSet<(u32, u32)>,
    raw_matches: Vec<bool>,
}

impl CompiledPathMatcher {
    #[inline]
    fn is_empty(&self) -> bool {
        self.subtree_parents.is_empty()
            && self.exact_ids.is_empty()
            && self.raw_matches.is_empty()
    }

    #[inline]
    fn matches(&self, record: &CompactRecord) -> bool {
        if record.raw_path {
            marked(&self.raw_matches, record.path)
        } else {
            marked(&self.subtree_parents, record.path)
                || (marked(&self.exact_parents, record.path)
                    && self.exact_ids.contains(&(record.path, record.name)))
        }
    }
}

#[derive(Default)]
pub(crate) struct InternHints {
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
            if self.parents.ref_count(record.path) == 0 {
                if let Some(index) = self.query_index.get_mut() {
                    index.clear_parent(record.path);
                }
            }
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

    fn on_records_appended(&mut self, base_ordinal: usize, added: &[CompactRecord]) {
        if added.is_empty() {
            return;
        }
        let Self {
            query_index,
            names,
            parents,
            ..
        } = self;
        if let Some(index) = query_index.get_mut() {
            index.register_records(added, names, parents);
            if let Some(csr) = index.csr.get_mut() {
                for (offset, record) in added.iter().enumerate() {
                    csr.append_record(record.name, (base_ordinal + offset) as u32);
                }
                if csr.overflow_entries.len() > 8192 {
                    index.invalidate_csr();
                }
            }
        }
    }

    fn on_records_removed(&mut self, first_removed: usize) {
        let Self {
            query_index,
            records,
            ..
        } = self;
        if let Some(index) = query_index.get_mut() {
            if let Some(csr) = index.csr.get_mut() {
                if !csr.has_base_renames && first_removed >= csr.base_records_len {
                    csr.rebuild_overflow(records);
                } else {
                    index.invalidate_csr();
                }
            }
        }
    }

    fn ensure_query_index(&self) -> &QueryIndex {
        let index = self
            .query_index
            .get_or_init(|| QueryIndex::build(&self.records, &self.names, &self.parents));
        index.ensure_csr(&self.records, self.names.slot_len());
        index
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
            .filter(|&name_id| self.names.get(name_id).is_some())
            .collect();
        ids.sort_unstable_by(|&left, &right| {
            let left_name = self.names.get(left).unwrap_or("");
            let right_name = self.names.get(right).unwrap_or("");
            left_name.cmp(right_name).then(left.cmp(&right))
        });
        let names = std::sync::Arc::new(SortedNames { ids });
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
        let base_ordinal = self.records.len();
        self.records.push(record);
        self.on_records_appended(base_ordinal, std::slice::from_ref(&record));
        Ok(())
    }

    /// 流式借用写入单条记录，复用同目录 InternHints 避免构建中间 FileEntry 堆分配与重复父目录哈希查找。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn push_borrowed_with_hints(
        &mut self,
        name: &str,
        path: &str,
        is_dir: bool,
        size: u64,
        modified: u64,
        created_at: u64,
        extension: &str,
        hints: &mut InternHints,
    ) -> Result<(), IndexError> {
        let entry = BorrowedFileEntry {
            name,
            path,
            is_dir,
            size,
            modified,
            created_at,
            extension,
        };
        let record = self.acquire_borrowed_with_hints(&entry, hints)?;
        let base_ordinal = self.records.len();
        self.records.push(record);
        self.on_records_appended(base_ordinal, std::slice::from_ref(&record));
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

    pub fn records(&self) -> &[CompactRecord] {
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
    pub fn record_name(&self, record: &CompactRecord) -> &str {
        if record.raw_path {
            self.raw_paths
                .get(record.path)
                .map_or("", |p| split_path(p).1)
        } else {
            self.names.get(record.name).unwrap_or("")
        }
    }

    /// 仅解析父目录字符串，池化记录直接查父目录池，避免冗余路径拼接或二次文件名查找。
    #[inline]
    pub fn record_parent(&self, record: &CompactRecord) -> &str {
        if record.raw_path {
            self.raw_paths
                .get(record.path)
                .map_or("", |p| split_path(p).0)
        } else {
            self.parents.get(record.path).unwrap_or("")
        }
    }

    /// O(1) 获取记录父目录的 `(depth, is_hidden)` 排序元数据：
    /// 池化记录直接读取 `QueryIndex::parent_meta` 单字节表，原始路径或未命中槽位安全回退现场计算。
    #[inline]
    pub(crate) fn record_parent_meta(
        &self,
        record: &CompactRecord,
        query_index: &QueryIndex,
    ) -> (usize, bool) {
        if !record.raw_path {
            if let Some(meta) = query_index.parent_meta(record.path) {
                return meta;
            }
        }
        let parent = self.record_parent(record);
        (parent_depth(parent), is_parent_hidden(parent.as_bytes()))
    }

    /// 查找扩展名在池中的槽位编号。
    /// 纯 ASCII 走哈希槽直查；非 ASCII 若字面未命中，则按 Unicode NFC 与大小写折叠回退匹配（如 `.café` 命中 NFD 槽位）。
    pub fn extension_id(&self, ext: &str) -> Option<u32> {
        if let Some(id) = self.extensions.find(ext) {
            return Some(id);
        }
        if ext.is_ascii() {
            return None;
        }
        let target = crate::cjk::fold_cow(ext);
        self.extensions
            .find_matching(|slot| !slot.is_ascii() && crate::cjk::fold_cow(slot) == target)
    }

    /// 评估扩展名池中的每个槽位是否满足谓词条件，产出与池容量等长的布尔位图。
    pub fn evaluate_extensions(&self, predicate: impl FnMut(&str) -> bool) -> Vec<bool> {
        self.extensions.matching_ids(predicate)
    }

    pub(crate) fn evaluate_parents<T: Clone>(
        &self,
        default: T,
        mapper: impl FnMut(&str) -> T,
    ) -> Vec<T> {
        self.parents.evaluate_ids(default, mapper)
    }

    /// 评估名字池中的每个槽位是否满足谓词条件，产出与名字池等长的布尔位图。
    pub(crate) fn evaluate_name_mask(&self, predicate: impl FnMut(&str) -> bool) -> Vec<bool> {
        self.names.matching_ids(predicate)
    }

    /// 评估父目录池中的每个槽位是否满足谓词条件，产出与父目录池等长的布尔位图。
    pub(crate) fn evaluate_parent_mask(&self, predicate: impl FnMut(&str) -> bool) -> Vec<bool> {
        self.parents.matching_ids(predicate)
    }

    /// 评估原始路径池中的每个槽位是否满足谓词条件，产出与原始路径池等长的布尔位图。
    pub(crate) fn evaluate_raw_path_mask(&self, predicate: impl FnMut(&str) -> bool) -> Vec<bool> {
        self.raw_paths.matching_ids(predicate)
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
        let (removed, first_removed) = self.remove_without_refresh(predicate);
        if let Some(first_removed) = first_removed {
            self.on_records_removed(first_removed);
        }
        removed
    }

    /// 按精确路径集合 O(1) 查表删除记录，避免对百万记录逐条执行 `paths.iter().any(...)`。
    pub fn remove_exact_paths(&mut self, paths: &HashSet<String>) -> usize {
        if paths.is_empty() {
            return 0;
        }
        let mut exact_parents: Vec<bool> = Vec::new();
        let mut exact_ids: HashSet<(u32, u32)> = HashSet::with_capacity(paths.len());
        for path in paths {
            let (parent, name) = split_path(path);
            if !name.is_empty() {
                if let (Some(pid), Some(nid)) = (self.parents.find(parent), self.names.find(name)) {
                    let p_idx = pid as usize;
                    if p_idx >= exact_parents.len() {
                        exact_parents.resize(p_idx + 1, false);
                    }
                    exact_parents[p_idx] = true;
                    exact_ids.insert((pid, nid));
                }
            }
        }
        let raw_matches = if self.raw_paths.len() > 0 {
            self.raw_paths.matching_ids(|raw| paths.contains(raw))
        } else {
            Vec::new()
        };
        if exact_ids.is_empty() && !raw_matches.iter().any(|&matched| matched) {
            return 0;
        }
        let (removed, first_removed) = self.remove_records_matching(|_, record| {
            if record.raw_path {
                marked(&raw_matches, record.path)
            } else {
                marked(&exact_parents, record.path) && exact_ids.contains(&(record.path, record.name))
            }
        });
        if let Some(first_removed) = first_removed {
            self.on_records_removed(first_removed);
        }
        removed
    }

    fn remove_without_refresh(
        &mut self,
        mut predicate: impl FnMut(EntryView<'_>) -> bool,
    ) -> (usize, Option<usize>) {
        self.remove_records_matching(|index, record| predicate(index.record_view(record).unwrap()))
    }

    fn remove_records_matching(
        &mut self,
        mut predicate: impl FnMut(&Self, &CompactRecord) -> bool,
    ) -> (usize, Option<usize>) {
        let mut retained = 0;
        let mut first_removed = None;
        let count = self.records.len();
        for ordinal in 0..count {
            let record = self.records[ordinal];
            if predicate(self, &record) {
                if first_removed.is_none() {
                    first_removed = Some(ordinal);
                }
                self.release(record);
            } else {
                self.records[retained] = record;
                retained += 1;
            }
        }
        self.records.truncate(retained);
        (count - retained, first_removed)
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
        let base_ordinal = self.records.len();
        self.on_records_appended(base_ordinal, &staged);
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
        let (removed, first_removed) = self.remove_without_refresh(predicate);
        if let Some(first_removed) = first_removed {
            self.on_records_removed(first_removed);
        }
        let base_ordinal = self.records.len();
        self.on_records_appended(base_ordinal, &staged);
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
            if previous.name != record.name {
                if let Some(index) = self.query_index.get_mut() {
                    if let Some(csr) = index.csr.get_mut() {
                        csr.replace_record_name(ordinal as u32, previous.name, record.name);
                    }
                }
            }
            self.release(previous);
            let parent_changed =
                !record.raw_path && (previous.raw_path || previous.path != record.path);
            if previous.name != record.name || parent_changed {
                let Self {
                    query_index,
                    names,
                    parents,
                    ..
                } = self;
                if let Some(index) = query_index.get_mut() {
                    if previous.name != record.name {
                        index.register_records(std::slice::from_ref(&record), names, parents);
                    } else if parent_changed {
                        index.register_parent(record.path, parents);
                    }
                }
            }
        }
        Ok(count)
    }

    /// 将属于 `old_root` 挂载域的所有条目原子重锚定至 `new_root`。
    ///
    /// 步骤包括：
    /// 1. 规范化路径并构建带尾随斜杠的目录边界前缀；
    /// 2. 重映射 `parents` 与 `raw_paths` 字符串池并重建哈希表与冲突链；
    /// 3. 若已构建 `QueryIndex`，原地刷新 `parent_meta` 并增量更新挂载根条目的名称与 CSR，保留全部签名免全量重建；
    /// 4. 若在极罕见碰撞场景下无法原地完成，无损回退至全量物化重构，确保 100% 满足校验不变量。
    pub fn rebase_mount_root(&mut self, old_root: &str, new_root: &str) -> Result<usize, IndexError> {
        let old_clean = old_root.trim_end_matches('/');
        let new_clean = new_root.trim_end_matches('/');
        if old_clean == new_clean {
            return Ok(0);
        }

        let res = self.rebase_in_place(old_clean, new_clean);
        match res {
            Ok(count) => {
                if self.validate().is_ok() {
                    return Ok(count);
                }
            }
            Err(_) => {}
        }

        self.rebase_fallback(old_clean, new_clean)
    }

    fn rebase_in_place(&mut self, old_clean: &str, new_clean: &str) -> Result<usize, IndexError> {
        let case_insensitive = cfg!(target_os = "macos");
        let old_prefix = format!("{old_clean}/");
        let new_prefix = format!("{new_clean}/");

        // 先处理体量极小的 raw_paths 再处理 parents，降低碰撞回退时的部分变更概率
        let _raw_remapped = self
            .raw_paths
            .remap_prefix(old_clean, new_clean, case_insensitive)?;
        let parents_remapped = self
            .parents
            .remap_prefix(&old_prefix, &new_prefix, case_insensitive)?;

        // 保留已预热的 QueryIndex（签名/CSR/折叠表），仅按重映射后的父目录池刷新 parent_meta 单字节表
        if parents_remapped > 0 {
            let Self {
                query_index,
                parents,
                ..
            } = self;
            if let Some(index) = query_index.get_mut() {
                index.refresh_all_parents(parents);
            }
        }

        let (old_parent, old_leaf) = split_path(old_clean);
        let (new_parent, new_leaf) = split_path(new_clean);

        let is_match = |a: &str, b: &str| {
            if case_insensitive {
                a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase()
            } else {
                a == b
            }
        };

        let parent_is_new_subtree = self
            .parents
            .matching_ids(|parent| parent.starts_with(&new_prefix));
        let raw_is_new = if self.raw_paths.len() > 0 {
            self.raw_paths
                .matching_ids(|raw| raw == new_clean || raw.starts_with(&new_prefix))
        } else {
            Vec::new()
        };
        let old_parent_ids = self
            .parents
            .matching_ids(|parent| is_match(parent, old_parent));
        let old_name_ids = self.names.matching_ids(|name| is_match(name, old_leaf));
        let has_root_candidate =
            old_parent_ids.iter().any(|&matched| matched) && old_name_ids.iter().any(|&matched| matched);
        let existing_new_parent_id = self.parents.find(new_parent);
        let existing_new_name_id = self.names.find(new_leaf);

        let mut count = 0usize;
        for ord in 0..self.records.len() {
            let mut record = self.records[ord];
            if record.raw_path {
                if marked(&raw_is_new, record.path) {
                    count += 1;
                }
                continue;
            }

            if has_root_candidate
                && marked(&old_parent_ids, record.path)
                && marked(&old_name_ids, record.name)
            {
                let prev_name = record.name;
                let prev_parent = record.path;
                let name_changed = old_leaf != new_leaf;
                let parent_changed = old_parent != new_parent;
                if name_changed {
                    let new_name = self.names.intern(new_leaf)?;
                    record.name = new_name;
                    self.records[ord].name = new_name;
                    if let Some(index) = self.query_index.get_mut() {
                        if let Some(csr) = index.csr.get_mut() {
                            csr.replace_record_name(ord as u32, prev_name, new_name);
                        }
                    }
                    self.names.release(prev_name);
                    if self.names.ref_count(prev_name) == 0 {
                        if let Some(index) = self.query_index.get_mut() {
                            index.clear_name(prev_name);
                        }
                    }
                }
                if parent_changed {
                    let new_p = self.parents.intern(new_parent)?;
                    record.path = new_p;
                    self.records[ord].path = new_p;
                    self.parents.release(prev_parent);
                    if self.parents.ref_count(prev_parent) == 0 {
                        if let Some(index) = self.query_index.get_mut() {
                            index.clear_parent(prev_parent);
                        }
                    }
                }
                if name_changed || parent_changed {
                    let Self {
                        query_index,
                        names,
                        parents,
                        ..
                    } = self;
                    if let Some(index) = query_index.get_mut() {
                        if name_changed {
                            index.register_records(std::slice::from_ref(&record), names, parents);
                        } else if parent_changed {
                            index.register_parent(record.path, parents);
                        }
                    }
                }
                if self.parents.get(record.path) == Some(new_parent)
                    && self.names.get(record.name) == Some(new_leaf)
                {
                    count += 1;
                }
                continue;
            }

            if marked(&parent_is_new_subtree, record.path)
                || (existing_new_parent_id == Some(record.path)
                    && existing_new_name_id == Some(record.name))
            {
                count += 1;
            }
        }

        Ok(count)
    }

    fn rebase_fallback(&mut self, old_clean: &str, new_clean: &str) -> Result<usize, IndexError> {
        let old_prefix = format!("{old_clean}/");
        let new_prefix = format!("{new_clean}/");
        let mut entries = Vec::with_capacity(self.records.len());
        let old_path = std::path::Path::new(old_clean);
        let old_comps: Vec<String> = old_path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect();
        for (_id, view) in self.iter() {
            let mut entry = view.materialize();
            let entry_path = std::path::Path::new(&entry.path);
            if view.path_equals(old_clean)
                || (cfg!(target_os = "macos")
                    && (entry.path.eq_ignore_ascii_case(old_clean)
                        || entry_path
                            .components()
                            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
                            .eq(old_comps.iter().cloned())))
            {
                entry.path = new_clean.to_string();
                if let Some(name) = std::path::Path::new(new_clean).file_name() {
                    entry.name = name.to_string_lossy().into_owned();
                }
            } else if let Some(suffix) = entry.path.strip_prefix(&old_prefix) {
                entry.path = format!("{new_clean}/{suffix}");
            } else if cfg!(target_os = "macos")
                && entry
                    .path
                    .as_bytes()
                    .get(..old_prefix.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(old_prefix.as_bytes()))
            {
                // 字节前缀已与有效 UTF-8 串 old_prefix 按 ASCII 忽略大小写完全匹配，
                // 因此 old_prefix.len() 必然落在 UTF-8 字符边界上，不会触发切片越界 panic。
                let suffix = &entry.path[old_prefix.len()..];
                entry.path = format!("{new_clean}/{suffix}");
            } else if cfg!(target_os = "macos") {
                let entry_comps: Vec<_> = entry_path.components().collect();
                if entry_comps.len() > old_comps.len()
                    && entry_comps
                        .iter()
                        .take(old_comps.len())
                        .zip(old_comps.iter())
                        .all(|(c, target)| c.as_os_str().to_string_lossy().to_lowercase() == *target)
                {
                    let mut rebuilt = std::path::PathBuf::from(new_clean);
                    for comp in entry_comps.into_iter().skip(old_comps.len()) {
                        rebuilt.push(comp.as_os_str());
                    }
                    entry.path = rebuilt.to_string_lossy().into_owned();
                }
            }
            entries.push(entry);
        }
        *self = Self::from_entries(entries)?;

        let mut count = 0;
        for ord in 0..self.records.len() {
            let view = self.view(EntryId(ord)).ok_or(IndexError::InvalidReference)?;
            if view.path_equals(new_clean) || view.path_starts_with(&new_prefix) {
                count += 1;
            }
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

    /// 将一组待删除或待排除的路径预编译为 O(1) 查表结构：
    /// - `subtree_parents` 仅标记严格位于某路径子树下（`parent.starts_with("{path}/")`）的父目录；
    /// - `exact_ids` 将单路径精确匹配预解析为 `(parent_id, name_id)` 整数对，消除同目录存活文件的字符串比对。
    fn compile_path_matcher(&self, paths: &[String]) -> CompiledPathMatcher {
        if paths.is_empty() {
            return CompiledPathMatcher::default();
        }
        let prefixes: Vec<String> = paths.iter().map(|path| format!("{path}/")).collect();
        let any_trailing_slash = paths.iter().any(|path| path.ends_with('/'));
        let prefix_set: HashSet<&str> = prefixes.iter().map(String::as_str).collect();
        let subtree_parents = self.parents.matching_ids(|parent| {
            if any_trailing_slash {
                prefixes.iter().any(|prefix| parent.starts_with(prefix))
            } else {
                touches_path(parent, &prefix_set)
            }
        });
        let has_subtree = subtree_parents.iter().any(|&matched| matched);

        let mut exact_parents: Vec<bool> = Vec::new();
        let mut exact_ids: HashSet<(u32, u32)> = HashSet::with_capacity(paths.len());
        for path in paths {
            let (parent, name) = split_path(path);
            if !name.is_empty() {
                if let (Some(pid), Some(nid)) = (self.parents.find(parent), self.names.find(name)) {
                    let p_idx = pid as usize;
                    if p_idx >= exact_parents.len() {
                        exact_parents.resize(p_idx + 1, false);
                    }
                    exact_parents[p_idx] = true;
                    exact_ids.insert((pid, nid));
                }
            }
        }

        let raw_matches = if self.raw_paths.len() > 0 {
            let exact_set: HashSet<&str> = paths.iter().map(String::as_str).collect();
            self.raw_paths.matching_ids(|raw| {
                exact_set.contains(raw)
                    || if any_trailing_slash {
                        prefixes.iter().any(|prefix| raw.starts_with(prefix))
                    } else {
                        touches_path(raw, &prefix_set)
                    }
            })
        } else {
            Vec::new()
        };
        let has_raw = raw_matches.iter().any(|&matched| matched);

        CompiledPathMatcher {
            subtree_parents: if has_subtree { subtree_parents } else { Vec::new() },
            exact_parents,
            exact_ids,
            raw_matches: if has_raw { raw_matches } else { Vec::new() },
        }
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
            let mut rename_hints = InternHints::default();
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
                    match self.acquire_borrowed_with_hints(
                        &BorrowedFileEntry::from(&entry),
                        &mut rename_hints,
                    ) {
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
        let mut create_hints = InternHints::default();
        for entry in &creates {
            match self.acquire_borrowed_with_hints(
                &BorrowedFileEntry::from(entry),
                &mut create_hints,
            ) {
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
            if previous.name != replacement.name {
                if let Some(index) = self.query_index.get_mut() {
                    if let Some(csr) = index.csr.get_mut() {
                        csr.replace_record_name(ordinal as u32, previous.name, replacement.name);
                    }
                }
            }
            self.release(previous);
            let parent_changed = !replacement.raw_path
                && (previous.raw_path || previous.path != replacement.path);
            if previous.name != replacement.name || parent_changed {
                let Self {
                    query_index,
                    names,
                    parents,
                    ..
                } = self;
                if let Some(index) = query_index.get_mut() {
                    if previous.name != replacement.name {
                        index.register_records(std::slice::from_ref(&replacement), names, parents);
                    } else if parent_changed {
                        index.register_parent(replacement.path, parents);
                    }
                }
            }
        }

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
        let mut upsert_parents: Vec<bool> = Vec::new();
        let mut upsert_ids: HashSet<(u32, u32)> = HashSet::new();
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
            } else if let Some(raw) = self.raw_paths.get(record.path) {
                // 原始路径新增若与既有池化记录同路径，直接预解析为 (parent_id, name_id) 并入 O(1) 查表
                let (parent, name) = split_path(raw);
                if !name.is_empty() {
                    if let (Some(pid), Some(nid)) =
                        (self.parents.find(parent), self.names.find(name))
                    {
                        let p_idx = pid as usize;
                        if p_idx >= upsert_parents.len() {
                            upsert_parents.resize(p_idx + 1, false);
                        }
                        upsert_parents[p_idx] = true;
                        upsert_ids.insert((pid, nid));
                    }
                }
            }
        }

        let raw_upsert_mask = if self.raw_paths.len() > 0 && !additions.is_empty() {
            let mut raw_exact: HashSet<u32> = HashSet::new();
            let mut addition_keys: HashSet<(&str, &str)> = HashSet::with_capacity(additions.len());
            for record in &additions {
                if record.raw_path {
                    raw_exact.insert(record.path);
                    if let Some(raw) = self.raw_paths.get(record.path) {
                        addition_keys.insert(split_path(raw));
                    }
                } else if let (Some(parent), Some(name)) =
                    (self.parents.get(record.path), self.names.get(record.name))
                {
                    addition_keys.insert((parent, name));
                }
            }
            let mask: Vec<bool> = (0..self.raw_paths.slot_len())
                .map(|slot| {
                    let id = slot as u32;
                    raw_exact.contains(&id)
                        || self
                            .raw_paths
                            .get(id)
                            .is_some_and(|raw| addition_keys.contains(&split_path(raw)))
                })
                .collect();
            if mask.iter().any(|&matched| matched) {
                mask
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let excluded_matcher = self.compile_path_matcher(excluded);
        let remove_matcher = self.compile_path_matcher(removes);
        let has_excluded = !excluded_matcher.is_empty();
        let has_removes = !remove_matcher.is_empty();
        let has_upserts = !upsert_ids.is_empty() || !raw_upsert_mask.is_empty();

        // 单遍扫描同时完成 excluded 剔除、removes 删除与 upsert 覆盖，彻底免除同目录存活文件的字符串比对；
        // excluded 优先匹配且不计入返回的 removed 计数，保持与原有统计口径严格一致。
        let mut first_removed: Option<usize> = None;
        let mut removed = 0usize;
        if has_excluded || has_removes || has_upserts {
            let mut retained = 0usize;
            let count = self.records.len();
            for ordinal in 0..count {
                let record = self.records[ordinal];
                let drop_excluded = has_excluded && excluded_matcher.matches(&record);
                let drop_removed = !drop_excluded
                    && ((has_upserts
                        && if record.raw_path {
                            marked(&raw_upsert_mask, record.path)
                        } else {
                            marked(&upsert_parents, record.path)
                                && upsert_ids.contains(&(record.path, record.name))
                        })
                        || (has_removes && remove_matcher.matches(&record)));
                if drop_excluded || drop_removed {
                    if first_removed.is_none() {
                        first_removed = Some(ordinal);
                    }
                    if drop_removed {
                        removed += 1;
                    }
                    self.release(record);
                } else {
                    self.records[retained] = record;
                    retained += 1;
                }
            }
            self.records.truncate(retained);
        }

        if let Some(first_removed) = first_removed {
            self.on_records_removed(first_removed);
        }
        let added = additions.len();
        let base_ordinal = self.records.len();
        self.on_records_appended(base_ordinal, &additions);
        self.records.reserve_exact(added);
        self.records.extend(additions);
        Ok((added, removed))
    }

    /// 索引里是否有以 `dir` 为父目录的条目。
    pub fn has_children(&self, dir: &str) -> bool {
        self.parents.find(&format!("{dir}/")).is_some()
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

    /// 编码为缓存负载，读取侧用 [`BorrowedCacheIndex`] 或 [`CacheIndex`] 解码。
    ///
    /// 当四个字符串池均无空洞时直接借用 `&self.records` 序列化，免去 36MB 级记录数组克隆与 ID 重映射表分配。
    pub(crate) fn encode_cache(&self) -> Vec<u8> {
        if self.names.is_dense()
            && self.parents.is_dense()
            && self.extensions.is_dense()
            && self.raw_paths.is_dense()
        {
            let mut encoded = bitcode::encode(&CacheStringsRef {
                names: self.names.cache_strings_dense(),
                parents: self.parents.cache_strings_dense(),
                extensions: self.extensions.cache_strings_dense(),
                raw_paths: self.raw_paths.cache_strings_dense(),
            });
            encoded.extend_from_slice(&bitcode::encode(&self.records));
            return encoded;
        }
        let (names, name_ids) = self.names.cache_strings();
        let (parents, parent_ids) = self.parents.cache_strings();
        let (extensions, extension_ids) = self.extensions.cache_strings();
        let (raw_paths, raw_ids) = self.raw_paths.cache_strings();
        let records: Vec<CompactRecord> = self
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
        bitcode::encode(&CacheIndexRef {
            names,
            parents,
            extensions,
            raw_paths,
            records,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn from_cache(cache: CacheIndex) -> Result<Self, IndexError> {
        Self::from_cache_parts(
            cache.names,
            cache.parents,
            cache.extensions,
            cache.raw_paths,
            cache.records,
        )
    }

    pub(crate) fn from_borrowed_cache(cache: BorrowedCacheIndex<'_>) -> Result<Self, IndexError> {
        Self::from_cache_parts(
            cache.names,
            cache.parents,
            cache.extensions,
            cache.raw_paths,
            cache.records,
        )
    }

    fn from_cache_parts<S>(
        cache_names: Vec<S>,
        cache_parents: Vec<S>,
        cache_extensions: Vec<S>,
        cache_raw_paths: Vec<S>,
        records: Vec<CompactRecord>,
    ) -> Result<Self, IndexError>
    where
        S: AsRef<str> + Into<Box<str>>,
    {
        for record in &records {
            if !record.raw_path {
                let name = cache_names
                    .get(record.name as usize)
                    .ok_or(IndexError::InvalidReference)?
                    .as_ref();
                let parent = cache_parents
                    .get(record.path as usize)
                    .ok_or(IndexError::InvalidReference)?
                    .as_ref();
                if name.is_empty()
                    || name.contains('/')
                    || (!parent.is_empty() && !parent.ends_with('/'))
                {
                    return Err(IndexError::InvalidReference);
                }
            }
        }
        let mut names = vec![0u32; cache_names.len()];
        let mut parents = vec![0u32; cache_parents.len()];
        let mut extensions = vec![0u32; cache_extensions.len()];
        let mut raw_paths = vec![0u32; cache_raw_paths.len()];
        for record in &records {
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
            records,
            names: StringPool::from_cache(cache_names, names)?,
            parents: StringPool::from_cache(cache_parents, parents)?,
            extensions: StringPool::from_cache(cache_extensions, extensions)?,
            raw_paths: StringPool::from_cache(cache_raw_paths, raw_paths)?,
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

    // 经编码再按 CacheIndex 解码：顺带守住借用编码与读取侧的线格式一致。
    fn cache_fixture() -> CacheIndex {
        let encoded = CompactIndex::from_entries([FileEntry {
            name: "main.rs".into(),
            path: "/src/main.rs".into(),
            extension: "rs".into(),
            is_dir: false,
            size: u64::MAX,
            modified: u64::MAX,
            created_at: u64::MAX,
        }])
        .unwrap()
        .encode_cache();
        bitcode::decode(&encoded).unwrap()
    }

    #[test]
    fn borrowed_cache_encoding_matches_owned() {
        let encoded = CompactIndex::from_entries([
            FileEntry {
                name: "main.rs".into(),
                path: "/src/main.rs".into(),
                extension: "rs".into(),
                is_dir: false,
                size: 1,
                modified: 2,
                created_at: 3,
            },
            FileEntry {
                name: "src".into(),
                path: "/src".into(),
                extension: String::new(),
                is_dir: true,
                size: 0,
                modified: 0,
                created_at: 0,
            },
            FileEntry {
                name: "报告\u{1F4C4}.txt".into(),
                path: "/文档/报告\u{1F4C4}.txt".into(),
                extension: "txt".into(),
                is_dir: false,
                size: 4,
                modified: 5,
                created_at: 6,
            },
        ])
        .unwrap()
        .encode_cache();
        let owned: CacheIndex = bitcode::decode(&encoded).unwrap();
        assert_eq!(bitcode::encode(&owned), encoded);
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

    #[test]
    fn rebase_mount_root_handles_root_entry_subpaths_and_cache_roundtrip() {
        let entries = vec![
            FileEntry {
                name: "Share".into(),
                path: "/Volumes/Share".into(),
                is_dir: true,
                size: 0,
                modified: 10,
                created_at: 1,
                extension: "".into(),
            },
            FileEntry {
                name: "a.txt".into(),
                path: "/Volumes/Share/a.txt".into(),
                is_dir: false,
                size: 5,
                modified: 20,
                created_at: 2,
                extension: "txt".into(),
            },
            FileEntry {
                name: "b.rs".into(),
                path: "/Volumes/Share/sub/b.rs".into(),
                is_dir: false,
                size: 9,
                modified: 30,
                created_at: 3,
                extension: "rs".into(),
            },
        ];
        let mut index = CompactIndex::from_entries(entries).unwrap();
        index.rebase_mount_root("/Volumes/Share", "/Volumes/Share-1").unwrap();
        let encoded = index.encode_cache();
        let cache: CacheIndex = bitcode::decode(&encoded).unwrap();
        let reloaded = CompactIndex::from_cache(cache).unwrap();
        let paths: Vec<String> = (0..reloaded.len())
            .map(|i| reloaded.view(EntryId(i)).unwrap().materialize().path)
            .collect();
        assert!(paths.contains(&"/Volumes/Share-1".to_string()));
        assert!(paths.contains(&"/Volumes/Share-1/a.txt".to_string()));
        assert!(paths.contains(&"/Volumes/Share-1/sub/b.rs".to_string()));
        let hit_paths: Vec<String> = crate::search::path_search::search_index(
            &reloaded,
            &crate::search::path_query::PathQuery::parse_with_case("in:/Volumes/Share-1 b.rs", false).unwrap(),
            &crate::search::CompiledRecordFilter::default(),
            10,
            false,
            false,
        )
        .into_iter()
        .map(|r| r.entry.path)
        .collect();
        assert_eq!(hit_paths, vec!["/Volumes/Share-1/sub/b.rs"]);
    }

    #[test]
    fn rebase_mount_root_reverse_and_boundary_safety() {
        let entries = vec![
            FileEntry {
                name: "Share".into(),
                path: "/Volumes/Share".into(),
                is_dir: true,
                size: 0,
                modified: 10,
                created_at: 1,
                extension: "".into(),
            },
            FileEntry {
                name: "a.txt".into(),
                path: "/Volumes/Share/a.txt".into(),
                is_dir: false,
                size: 5,
                modified: 20,
                created_at: 2,
                extension: "txt".into(),
            },
            FileEntry {
                name: "b.rs".into(),
                path: "/Volumes/Share/sub/b.rs".into(),
                is_dir: false,
                size: 9,
                modified: 30,
                created_at: 3,
                extension: "rs".into(),
            },
            FileEntry {
                name: "other.txt".into(),
                path: "/Volumes/ShareExtra/other.txt".into(),
                is_dir: false,
                size: 3,
                modified: 40,
                created_at: 4,
                extension: "txt".into(),
            },
        ];
        let mut index = CompactIndex::from_entries(entries).unwrap();
        let count = index.rebase_mount_root("/Volumes/Share", "/Volumes/Share-1").unwrap();
        assert_eq!(count, 3);

        // Boundary check: /Volumes/ShareExtra/other.txt must NOT be modified
        let paths: Vec<String> = (0..index.len())
            .map(|i| index.view(EntryId(i)).unwrap().materialize().path)
            .collect();
        assert!(paths.contains(&"/Volumes/ShareExtra/other.txt".to_string()));
        assert!(!paths.contains(&"/Volumes/Share-1Extra/other.txt".to_string()));
        assert!(paths.contains(&"/Volumes/Share-1".to_string()));
        assert!(paths.contains(&"/Volumes/Share-1/a.txt".to_string()));
        assert!(paths.contains(&"/Volumes/Share-1/sub/b.rs".to_string()));

        // Invariants must strictly hold
        index.validate().unwrap();

        // Reverse rebase back to /Volumes/Share
        let reverse_count = index.rebase_mount_root("/Volumes/Share-1", "/Volumes/Share").unwrap();
        assert_eq!(reverse_count, 3);

        let restored_paths: Vec<String> = (0..index.len())
            .map(|i| index.view(EntryId(i)).unwrap().materialize().path)
            .collect();
        assert!(restored_paths.contains(&"/Volumes/Share".to_string()));
        assert!(restored_paths.contains(&"/Volumes/Share/a.txt".to_string()));
        assert!(restored_paths.contains(&"/Volumes/Share/sub/b.rs".to_string()));
        assert!(restored_paths.contains(&"/Volumes/ShareExtra/other.txt".to_string()));

        index.validate().unwrap();
        let encoded = index.encode_cache();
        let cache: CacheIndex = bitcode::decode(&encoded).unwrap();
        let reloaded = CompactIndex::from_cache(cache).unwrap();
        assert_eq!(reloaded.len(), 4);
    }

    #[test]
    fn rebase_fallback_accurate_count_on_rebuilt_records() {
        let entries = vec![
            FileEntry {
                name: "Share".into(),
                path: "/Volumes/Share".into(),
                is_dir: true,
                size: 0,
                modified: 10,
                created_at: 1,
                extension: "".into(),
            },
            FileEntry {
                name: "a.txt".into(),
                path: "/Volumes/Share/a.txt".into(),
                is_dir: false,
                size: 5,
                modified: 20,
                created_at: 2,
                extension: "txt".into(),
            },
            FileEntry {
                name: "other.txt".into(),
                path: "/Volumes/Other/other.txt".into(),
                is_dir: false,
                size: 3,
                modified: 40,
                created_at: 4,
                extension: "txt".into(),
            },
        ];
        let mut index = CompactIndex::from_entries(entries).unwrap();
        let count = index.rebase_fallback("/Volumes/Share", "/Volumes/Share-1").unwrap();
        assert_eq!(count, 2);
        assert_eq!(index.len(), 3);
        index.validate().unwrap();

        let paths: Vec<String> = (0..index.len())
            .map(|i| index.view(EntryId(i)).unwrap().materialize().path)
            .collect();
        assert!(paths.contains(&"/Volumes/Share-1".to_string()));
        assert!(paths.contains(&"/Volumes/Share-1/a.txt".to_string()));
        assert!(paths.contains(&"/Volumes/Other/other.txt".to_string()));
    }

    #[test]
    fn rebase_fallback_handles_multibyte_utf8_at_prefix_length_without_panic() {
        let entries = vec![
            FileEntry {
                name: "a.txt".into(),
                path: "/Volumes/Disk/a.txt".into(),
                is_dir: false,
                size: 5,
                modified: 20,
                created_at: 2,
                extension: "txt".into(),
            },
            FileEntry {
                name: "b.txt".into(),
                // "/Volumes/Disk/" 长度为 14 字节，此处第 14 字节正好落在 '享' 的多字节 UTF-8 编码内部
                path: "/Volumes/共享盘/b.txt".into(),
                is_dir: false,
                size: 8,
                modified: 30,
                created_at: 3,
                extension: "txt".into(),
            },
        ];
        let mut index = CompactIndex::from_entries(entries).unwrap();
        let count = index.rebase_fallback("/Volumes/Disk", "/Volumes/Disk-1").unwrap();
        assert_eq!(count, 1);
        index.validate().unwrap();
        let paths: Vec<String> = (0..index.len())
            .map(|i| index.view(EntryId(i)).unwrap().materialize().path)
            .collect();
        assert!(paths.contains(&"/Volumes/Disk-1/a.txt".to_string()));
        assert!(paths.contains(&"/Volumes/共享盘/b.txt".to_string()));
    }

    #[test]
    fn borrowed_cache_decode_borrows_strings_and_roundtrips_dense_and_sparse() {
        let originals = vec![
            FileEntry {
                name: "main.rs".into(),
                path: "/src/main.rs".into(),
                extension: "rs".into(),
                is_dir: false,
                size: 10,
                modified: 100,
                created_at: 50,
            },
            FileEntry {
                name: "temp.log".into(),
                path: "/tmp/temp.log".into(),
                extension: "log".into(),
                is_dir: false,
                size: 20,
                modified: 200,
                created_at: 60,
            },
            FileEntry {
                name: "中文文档.md".into(),
                path: "/文档/中文文档.md".into(),
                extension: "md".into(),
                is_dir: false,
                size: 30,
                modified: 300,
                created_at: 70,
            },
        ];

        // 1. 紧凑无空洞编码路径（零 records 克隆）+ 零拷贝 BorrowedCacheIndex 解码
        let mut index = CompactIndex::from_entries(originals.clone()).unwrap();
        let dense_encoded = index.encode_cache();
        let dense_reloaded = {
            let borrowed: BorrowedCacheIndex<'_> = bitcode::decode(&dense_encoded).unwrap();
            let start = dense_encoded.as_ptr() as usize;
            let end = start + dense_encoded.len();
            for text in borrowed
                .names
                .iter()
                .chain(&borrowed.parents)
                .chain(&borrowed.extensions)
            {
                if !text.is_empty() {
                    let ptr = text.as_ptr() as usize;
                    assert!((start..end).contains(&ptr));
                    assert!(ptr + text.len() <= end);
                }
            }
            CompactIndex::from_borrowed_cache(borrowed).unwrap()
        };
        dense_reloaded.validate().unwrap();
        assert_eq!(dense_reloaded.len(), originals.len());
        for (i, orig) in originals.iter().enumerate() {
            assert_eq!(&dense_reloaded.materialize(EntryId(i)).unwrap(), orig);
        }

        // 2. 删除中间条目产生池空洞后，稀疏重映射路径编码同样与 CacheIndex 线格式一致
        index.remove_matching(|view| view.name == "temp.log");
        assert!(!index.names.is_dense());
        let sparse_encoded = index.encode_cache();
        let owned: CacheIndex = bitcode::decode(&sparse_encoded).unwrap();
        assert_eq!(bitcode::encode(&owned), sparse_encoded);
        let borrowed: BorrowedCacheIndex<'_> = bitcode::decode(&sparse_encoded).unwrap();
        let sparse_reloaded = CompactIndex::from_borrowed_cache(borrowed).unwrap();
        sparse_reloaded.validate().unwrap();
        assert_eq!(sparse_reloaded.len(), 2);
        assert_eq!(sparse_reloaded.materialize(EntryId(0)).unwrap(), originals[0]);
        assert_eq!(sparse_reloaded.materialize(EntryId(1)).unwrap(), originals[2]);
    }

    #[test]
    fn rebase_mount_root_retains_warmed_query_index_and_updates_parent_meta() {
        let entries = vec![
            FileEntry {
                name: ".HiddenShare".into(),
                path: "/Volumes/.HiddenShare".into(),
                is_dir: true,
                size: 0,
                modified: 10,
                created_at: 1,
                extension: "".into(),
            },
            FileEntry {
                name: "report.pdf".into(),
                path: "/Volumes/.HiddenShare/docs/report.pdf".into(),
                is_dir: false,
                size: 42,
                modified: 20,
                created_at: 2,
                extension: "pdf".into(),
            },
        ];
        let mut index = CompactIndex::from_entries(entries).unwrap();
        index.warm_query_index();
        assert!(index.query_index.get().is_some());

        // 从隐藏 3 层路径重映射到非隐藏 4 层路径，同时改变深度、隐藏标记与挂载根名称
        let count = index
            .rebase_mount_root("/Volumes/.HiddenShare", "/Volumes/Share-1/Nested")
            .unwrap();
        assert_eq!(count, 2);
        assert!(
            index.query_index.get().is_some(),
            "in-place rebase must preserve warmed QueryIndex instead of resetting OnceLock"
        );
        index.validate().unwrap();

        let qi = index.query_index();
        let report_record = index.records()[1];
        let (depth, is_hidden) = index.record_parent_meta(&report_record, qi);
        assert_eq!(depth, 4, "parent /Volumes/Share-1/Nested/docs/ has depth 4");
        assert!(!is_hidden, "rebased parent is no longer hidden");

        // 挂载根记录的新名称应可经 CSR 与签名命中，旧名称已被清理
        let new_root_id = index.names.find("Nested").expect("new root name interned");
        assert_eq!(qi.entries(new_root_id), &[0]);
        assert_eq!(index.names.find(".HiddenShare"), None);
    }

    #[test]
    fn apply_event_batch_exact_sibling_removal_excluded_and_upsert_semantics() {
        let mut entries: Vec<FileEntry> = (0..50)
            .map(|i| FileEntry {
                name: format!("sibling_{i:02}.txt"),
                path: format!("/dir/sibling_{i:02}.txt"),
                is_dir: false,
                size: i as u64,
                modified: 100,
                created_at: 10,
                extension: "txt".into(),
            })
            .collect();
        entries.push(FileEntry {
            name: "drop.rs".into(),
            path: "/excl/drop.rs".into(),
            is_dir: false,
            size: 1,
            modified: 100,
            created_at: 10,
            extension: "rs".into(),
        });
        entries.push(FileEntry {
            name: "raw_alias".into(),
            path: "/dir/raw_target".into(),
            is_dir: false,
            size: 1,
            modified: 100,
            created_at: 10,
            extension: "".into(),
        });

        let mut index = CompactIndex::from_entries(entries).unwrap();
        index.warm_query_index();

        let (added, removed) = index
            .apply_event_batch(
                &[],
                &["/excl".into()],
                &["/dir/sibling_05.txt".into()],
                vec![
                    FileEntry {
                        name: "sibling_10.txt".into(),
                        path: "/dir/sibling_10.txt".into(),
                        is_dir: false,
                        size: 999,
                        modified: 200,
                        created_at: 10,
                        extension: "txt".into(),
                    },
                    FileEntry {
                        name: "raw_target".into(),
                        path: "/dir/raw_target".into(),
                        is_dir: false,
                        size: 777,
                        modified: 200,
                        created_at: 10,
                        extension: "".into(),
                    },
                    FileEntry {
                        name: "brand_new.rs".into(),
                        path: "/dir/brand_new.rs".into(),
                        is_dir: false,
                        size: 12,
                        modified: 200,
                        created_at: 10,
                        extension: "rs".into(),
                    },
                ],
            )
            .unwrap();

        // excluded 剔除 1 条不计入 removed；removes 删 1 条 + upserts 覆写 2 条 = removed 3；creates 3 条
        assert_eq!((added, removed), (3, 3));
        assert_eq!(index.len(), 51);
        index.validate().unwrap();

        // 新增唯一名触发 SortedNames 惰性失效（不阻塞写锁），待下次 sorted_names() 按需重建
        {
            let qi = index.query_index.get().unwrap();
            assert!(
                qi.sorted.lock().unwrap().is_none(),
                "new unique name should lazily invalidate SortedNames under write lock"
            );
        }
        let rebuilt = index.sorted_names();
        let new_id = index.names.find("brand_new.rs").unwrap();
        assert!(rebuilt.ids.contains(&new_id));
    }
}
