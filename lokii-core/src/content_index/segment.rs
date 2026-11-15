use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use super::gram::{bloom_contains_all, build_bloom_bytes, content_hash, extract_grams};
use super::posting::{encode_posting, EncodedPosting, PostingKind, PostingSlice};
use super::scope::{release_allocator_memory, ContentSourceKind};

const SEGMENT_MAGIC: &[u8; 8] = b"LKCSEG01";
const SEGMENT_VERSION: u32 = 1;
const HEADER_SIZE: usize = 112;
const DOC_META_SIZE: usize = 24;
const CONTENT_META_SIZE: usize = 24;
const DICT_ENTRY_SIZE: usize = 24;
const PATH_RESTART_INTERVAL: usize = 16;
const COARSE_DICT_INTERVAL: usize = 32;

const DEAD_MAGIC: &[u8; 8] = b"LKDED001";

/// 分级归并触发阈值：同一层级达到 8 段触发归并；墓碑比例超过 1/16 触发压缩重整。
pub const TIER_MERGE_FANOUT: usize = 8;
pub const COMPACT_DEAD_NUMERATOR: usize = 1;
pub const COMPACT_DEAD_DENOMINATOR: usize = 16;

#[derive(Debug)]
struct MmapRegion {
    ptr: *const u8,
    len: usize,
}

// SAFETY: 映射区全程只读 (`PROT_READ`) 且对应不可变文件，跨线程只读共享与最终 `munmap` 均安全。
unsafe impl Send for MmapRegion {}
unsafe impl Sync for MmapRegion {}

impl MmapRegion {
    fn map_read_only(file: &File, len: usize) -> io::Result<Self> {
        if len == 0 {
            return Ok(Self {
                ptr: std::ptr::null(),
                len: 0,
            });
        }
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // SAFETY: 已核实文件总长度 >= len > 0，`PROT_READ | MAP_PRIVATE` 不修改映射区内容，返回值检查 `MAP_FAILED`。
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ,
                    libc::MAP_PRIVATE,
                    file.as_raw_fd(),
                    0,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                ptr: ptr as *const u8,
                len,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = file;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "mmap only enabled on unix",
            ))
        }
    }

    #[inline]
    fn as_slice(&self) -> &[u8] {
        if self.len == 0 || self.ptr.is_null() {
            &[]
        } else {
            // SAFETY: `self.ptr..self.ptr+self.len` 由成功的 `mmap` 保证有效且生命周期绑定 `Self`。
            unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
        }
    }
}

impl Drop for MmapRegion {
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.len > 0 && !self.ptr.is_null() {
            // SAFETY: 配对释放由 `MmapRegion::map_read_only` 成功创建的映射区。
            unsafe {
                let _ = libc::munmap(self.ptr as *mut libc::c_void, self.len);
            }
        }
    }
}

/// 建段输入文档描述符（既可用于直接从正文提取，也可用于富文档已抽取正文）。
#[derive(Debug, Clone)]
pub struct SegmentDocInput {
    pub path: String,
    pub mtime_secs: i64,
    pub size_bytes: u64,
    pub source_kind: ContentSourceKind,
    pub doc_rank: u16,
    pub content_hash: u64,
    pub bloom: Vec<u8>,
    pub decoded_bytes: u32,
    pub aux_meta: u32,
    pub keys: Vec<u32>,
}

impl SegmentDocInput {
    /// 从归一化后的 UTF-8 文本构建建段输入，自动计算 `content_hash`、三元组与 `Bloom` 过滤器。
    pub fn from_decoded_text(
        path: String,
        mtime_secs: i64,
        size_bytes: u64,
        source_kind: ContentSourceKind,
        doc_rank: u16,
        aux_meta: u32,
        text: &[u8],
        index_symbols: bool,
    ) -> Self {
        let hash = content_hash(text);
        let extracted = extract_grams(text, index_symbols);
        let bloom = build_bloom_bytes(&extracted.short_hashes);
        Self {
            path,
            mtime_secs,
            size_bytes,
            source_kind,
            doc_rank,
            content_hash: hash,
            bloom,
            decoded_bytes: text.len().min(u32::MAX as usize) as u32,
            aux_meta,
            keys: extracted.keys,
        }
    }
}

/// 段内单文档元数据视图。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocMeta {
    pub content_id: u32,
    pub source_kind: ContentSourceKind,
    pub doc_rank: u16,
    pub mtime_secs: i64,
    pub size_bytes: u64,
}

/// 段内唯一内容元数据视图（经 `content_hash` 去重）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentMeta {
    pub content_hash: u64,
    pub bloom_offset: u32,
    pub bloom_len: u32,
    pub decoded_bytes: u32,
    pub aux_meta: u32,
}

/// 词典条目元数据。
#[derive(Debug, Clone, Copy)]
pub struct DictEntry {
    pub key: u32,
    pub kind: PostingKind,
    pub posting_offset: u64,
    pub posting_len: u32,
    pub doc_freq: u32,
}

#[derive(Debug, Clone)]
struct SectionLayout {
    ndocs: u32,
    n_contents: u32,
    n_keys: u32,
    docs_meta_off: u64,
    paths_index_off: u64,
    paths_data_off: u64,
    contents_meta_off: u64,
    content_docs_index_off: u64,
    content_docs_data_off: u64,
    bloom_blob_off: u64,
    dict_coarse_off: u64,
    dict_entries_off: u64,
    postings_off: u64,
}

/// 不可变内存映射倒排段 (`LKCSEG01`)。
/// 通过只读 `mmap` 零拷贝访问词典、位图与前缀压缩路径，并按 `content_hash` 在段内去重。
pub struct Segment {
    pub id: u64,
    pub tier: u8,
    pub file_path: PathBuf,
    region: MmapRegion,
    layout: SectionLayout,
}

impl Segment {
    /// 构建新段并原子落盘后以 `mmap` 打开。
    pub fn build_to_path(
        id: u64,
        tier: u8,
        dest_path: &Path,
        inputs: Vec<SegmentDocInput>,
    ) -> io::Result<Self> {
        write_segment_file(dest_path, inputs)?;
        Self::open(id, tier, dest_path)
    }

    /// 打开既有 `.lkcseg` 段文件并校验头部边界。
    pub fn open(id: u64, tier: u8, file_path: &Path) -> io::Result<Self> {
        let file = File::open(file_path)?;
        let len = file.metadata()?.len() as usize;
        if len < HEADER_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "segment shorter than header",
            ));
        }
        let region = MmapRegion::map_read_only(&file, len)?;
        let data = region.as_slice();
        if &data[0..8] != SEGMENT_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid segment magic",
            ));
        }
        let version = read_u32(data, 8);
        if version != SEGMENT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported segment version {version}"),
            ));
        }
        let ndocs = read_u32(data, 12);
        let n_contents = read_u32(data, 16);
        let n_keys = read_u32(data, 20);

        let layout = SectionLayout {
            ndocs,
            n_contents,
            n_keys,
            docs_meta_off: read_u64(data, 32),
            paths_index_off: read_u64(data, 40),
            paths_data_off: read_u64(data, 48),
            contents_meta_off: read_u64(data, 56),
            content_docs_index_off: read_u64(data, 64),
            content_docs_data_off: read_u64(data, 72),
            bloom_blob_off: read_u64(data, 80),
            dict_coarse_off: read_u64(data, 88),
            dict_entries_off: read_u64(data, 96),
            postings_off: read_u64(data, 104),
        };
        validate_layout(&layout, len)?;

        Ok(Self {
            id,
            tier,
            file_path: file_path.to_path_buf(),
            region,
            layout,
        })
    }

    #[inline]
    pub fn ndocs(&self) -> u32 {
        self.layout.ndocs
    }

    #[inline]
    pub fn n_contents(&self) -> u32 {
        self.layout.n_contents
    }

    #[inline]
    pub fn n_keys(&self) -> u32 {
        self.layout.n_keys
    }

    #[inline]
    pub fn mapped_bytes(&self) -> usize {
        self.region.len
    }

    /// 读取指定 `doc_id` 的元数据。
    pub fn doc_meta(&self, doc_id: u32) -> Option<DocMeta> {
        if doc_id >= self.layout.ndocs {
            return None;
        }
        let data = self.region.as_slice();
        let off = self.layout.docs_meta_off as usize + (doc_id as usize) * DOC_META_SIZE;
        let content_id = read_u32(data, off);
        let source_kind = match data[off + 4] {
            0 => ContentSourceKind::PlainText,
            1 => ContentSourceKind::OfficeXml,
            2 => ContentSourceKind::Pdf,
            3 => ContentSourceKind::Image,
            _ => ContentSourceKind::PlainText,
        };
        let doc_rank = read_u16(data, off + 6);
        let mtime_secs = read_i64(data, off + 8);
        let size_bytes = read_u64(data, off + 16);
        Some(DocMeta {
            content_id,
            source_kind,
            doc_rank,
            mtime_secs,
            size_bytes,
        })
    }

    /// 解码指定 `doc_id` 的路径（基于 16 条重启点的前向前缀解压）。
    pub fn doc_path(&self, doc_id: u32) -> Option<String> {
        if doc_id >= self.layout.ndocs {
            return None;
        }
        let data = self.region.as_slice();
        let block = (doc_id as usize) / PATH_RESTART_INTERVAL;
        let in_block = (doc_id as usize) % PATH_RESTART_INTERVAL;
        let rel_block_off = read_u64(
            data,
            self.layout.paths_index_off as usize + block * 8,
        ) as usize;
        let mut cursor = self.layout.paths_data_off as usize + rel_block_off;
        let mut buf: Vec<u8> = Vec::with_capacity(128);
        for _step in 0..=in_block {
            let prefix_len = read_u16(data, cursor) as usize;
            let suffix_len = read_u16(data, cursor + 2) as usize;
            cursor += 4;
            buf.truncate(prefix_len);
            buf.extend_from_slice(&data[cursor..cursor + suffix_len]);
            cursor += suffix_len;
        }
        Some(String::from_utf8_lossy(&buf).into_owned())
    }

    /// 二分查找有序路径表（路径在段构建时已按字节序排列），实现 `O(log N)` 删除/修改精确定位。
    pub fn find_doc_by_path(&self, target_path: &str) -> Option<u32> {
        let mut lo = 0u32;
        let mut hi = self.layout.ndocs;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mid_path = self.doc_path(mid)?;
            match mid_path.as_str().cmp(target_path) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    /// 获取唯一内容记录元数据。
    pub fn content_meta(&self, content_id: u32) -> Option<ContentMeta> {
        if content_id >= self.layout.n_contents {
            return None;
        }
        let data = self.region.as_slice();
        let off = self.layout.contents_meta_off as usize + (content_id as usize) * CONTENT_META_SIZE;
        Some(ContentMeta {
            content_hash: read_u64(data, off),
            bloom_offset: read_u32(data, off + 8),
            bloom_len: read_u32(data, off + 12),
            decoded_bytes: read_u32(data, off + 16),
            aux_meta: read_u32(data, off + 20),
        })
    }

    /// 迭代指定 `content_id` 关联的全部 `doc_id`。
    pub fn doc_ids_for_content(&self, content_id: u32) -> Vec<u32> {
        if content_id >= self.layout.n_contents {
            return Vec::new();
        }
        let data = self.region.as_slice();
        let idx_base = self.layout.content_docs_index_off as usize;
        let start = read_u32(data, idx_base + (content_id as usize) * 4) as usize;
        let end = read_u32(data, idx_base + ((content_id as usize) + 1) * 4) as usize;
        let data_base = self.layout.content_docs_data_off as usize;
        let mut out = Vec::with_capacity(end.saturating_sub(start));
        for i in start..end {
            out.push(read_u32(data, data_base + i * 4));
        }
        out
    }

    /// 判断 `content_id` 的短语布隆过滤器是否满足全部查询哈希。
    pub fn content_bloom_contains_all(&self, content_id: u32, short_hashes: &[u32]) -> bool {
        if short_hashes.is_empty() {
            return true;
        }
        let Some(meta) = self.content_meta(content_id) else {
            return false;
        };
        if meta.bloom_len == 0 {
            return false;
        }
        let start = self.layout.bloom_blob_off as usize + meta.bloom_offset as usize;
        let end = start + meta.bloom_len as usize;
        let bloom = &self.region.as_slice()[start..end];
        bloom_contains_all(bloom, short_hashes)
    }

    /// 在段词典中查询单个 `key`（利用 32-key 粗粒度表缩小局部二分窗口）。
    pub fn lookup_key(&self, key: u32) -> Option<DictEntry> {
        let n = self.layout.n_keys as usize;
        if n == 0 {
            return None;
        }
        let data = self.region.as_slice();
        let n_coarse = (n + COARSE_DICT_INTERVAL - 1) / COARSE_DICT_INTERVAL;
        let coarse_off = self.layout.dict_coarse_off as usize;

        // 找到最后一个 coarse_key <= key 的块。
        let mut c_lo = 0usize;
        let mut c_hi = n_coarse;
        while c_lo < c_hi {
            let mid = c_lo + (c_hi - c_lo) / 2;
            let k = read_u32(data, coarse_off + mid * 4);
            if k <= key {
                c_lo = mid + 1;
            } else {
                c_hi = mid;
            }
        }
        if c_lo == 0 {
            return None;
        }
        let block = c_lo - 1;
        let mut lo = block * COARSE_DICT_INTERVAL;
        let mut hi = ((block + 1) * COARSE_DICT_INTERVAL).min(n);

        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let entry = self.dict_entry_at(mid);
            match entry.key.cmp(&key) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(entry),
            }
        }
        None
    }

    /// 遍历键处于 `[range.start, range.end)` 内的所有词典条目（用于文末 2 字节前缀扫描）。
    pub fn lookup_key_range(&self, range: Range<u32>) -> Vec<DictEntry> {
        let n = self.layout.n_keys as usize;
        if n == 0 || range.start >= range.end {
            return Vec::new();
        }
        let mut lo = 0usize;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.dict_entry_at(mid).key < range.start {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let mut out = Vec::new();
        for idx in lo..n {
            let e = self.dict_entry_at(idx);
            if e.key >= range.end {
                break;
            }
            out.push(e);
        }
        out
    }

    pub fn posting_slice(&self, entry: &DictEntry) -> PostingSlice<'_> {
        let start = self.layout.postings_off as usize + entry.posting_offset as usize;
        let end = start + entry.posting_len as usize;
        let bytes = &self.region.as_slice()[start..end];
        match entry.kind {
            PostingKind::Bitset => PostingSlice::Bitset {
                data: bytes,
                ndocs: self.layout.n_contents,
            },
            PostingKind::SmallIc => PostingSlice::SmallIc { data: bytes },
            PostingKind::BlockIc => PostingSlice::BlockIc { data: bytes },
        }
    }

    fn dict_entry_at(&self, idx: usize) -> DictEntry {
        let data = self.region.as_slice();
        let off = self.layout.dict_entries_off as usize + idx * DICT_ENTRY_SIZE;
        let key = read_u32(data, off);
        let kind = PostingKind::from_u8(data[off + 4]).unwrap_or(PostingKind::SmallIc);
        let posting_offset = read_u64(data, off + 8);
        let posting_len = read_u32(data, off + 16);
        let doc_freq = read_u32(data, off + 20);
        DictEntry {
            key,
            kind,
            posting_offset,
            posting_len,
            doc_freq,
        }
    }
}

/// 每个段对应的 `.dead` 墓碑位图，记录已被删除或新版本覆盖的 `doc_id`。
#[derive(Debug, Clone)]
pub struct DeadBitmap {
    ndocs: u32,
    words: Vec<u64>,
    dead_count: u32,
    dirty: bool,
}

impl DeadBitmap {
    pub fn empty(ndocs: u32) -> Self {
        let n_words = ((ndocs as usize) + 63) / 64;
        Self {
            ndocs,
            words: vec![0u64; n_words],
            dead_count: 0,
            dirty: false,
        }
    }

    pub fn load_or_empty(dead_path: &Path, ndocs: u32) -> io::Result<Self> {
        if !dead_path.exists() {
            return Ok(Self::empty(ndocs));
        }
        let bytes = fs::read(dead_path)?;
        if bytes.len() < 16 || &bytes[0..8] != DEAD_MAGIC {
            return Ok(Self::empty(ndocs));
        }
        let stored_ndocs = read_u32(&bytes, 8);
        if stored_ndocs != ndocs {
            return Ok(Self::empty(ndocs));
        }
        let n_words = ((ndocs as usize) + 63) / 64;
        if bytes.len() < 16 + n_words * 8 {
            return Ok(Self::empty(ndocs));
        }
        let mut words = Vec::with_capacity(n_words);
        let mut dead_count = 0u32;
        for i in 0..n_words {
            let mut w = read_u64(&bytes, 16 + i * 8);
            // 掩码剔除尾字中超出 ndocs 的高位杂散比特。
            if i + 1 == n_words {
                let rem = (ndocs as usize) % 64;
                if rem != 0 {
                    w &= (1u64 << rem) - 1;
                }
            }
            dead_count += w.count_ones();
            words.push(w);
        }
        Ok(Self {
            ndocs,
            words,
            dead_count,
            dirty: false,
        })
    }

    #[inline]
    pub fn is_dead(&self, doc_id: u32) -> bool {
        if doc_id >= self.ndocs {
            return true;
        }
        let word = (doc_id as usize) >> 6;
        let bit = doc_id & 63;
        ((self.words[word] >> bit) & 1) != 0
    }

    pub fn mark_dead(&mut self, doc_id: u32) -> bool {
        if doc_id >= self.ndocs {
            return false;
        }
        let word = (doc_id as usize) >> 6;
        let bit = doc_id & 63;
        let mask = 1u64 << bit;
        if (self.words[word] & mask) == 0 {
            self.words[word] |= mask;
            self.dead_count += 1;
            self.dirty = true;
            true
        } else {
            false
        }
    }

    #[inline]
    pub fn dead_count(&self) -> u32 {
        self.dead_count
    }

    #[inline]
    pub fn ndocs(&self) -> u32 {
        self.ndocs
    }

    #[inline]
    pub fn live_count(&self) -> u32 {
        self.ndocs.saturating_sub(self.dead_count)
    }

    #[inline]
    pub fn exceeds_compact_threshold(&self) -> bool {
        if self.ndocs == 0 {
            return false;
        }
        (self.dead_count as usize) * COMPACT_DEAD_DENOMINATOR
            > (self.ndocs as usize) * COMPACT_DEAD_NUMERATOR
            || self.dead_count == self.ndocs
    }

    pub fn save_if_dirty(&mut self, dead_path: &Path) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if self.dead_count == 0 {
            if dead_path.exists() {
                let _ = fs::remove_file(dead_path);
            }
            self.dirty = false;
            return Ok(());
        }
        if let Some(parent) = dead_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = dead_path.with_extension("dead.tmp");
        {
            let file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp)?;
            let mut w = BufWriter::new(file);
            w.write_all(DEAD_MAGIC)?;
            w.write_all(&self.ndocs.to_le_bytes())?;
            w.write_all(&self.dead_count.to_le_bytes())?;
            for &word in &self.words {
                w.write_all(&word.to_le_bytes())?;
            }
            w.flush()?;
        }
        fs::rename(&tmp, dead_path)?;
        self.dirty = false;
        Ok(())
    }
}

pub fn dead_path_for(seg_path: &Path) -> PathBuf {
    seg_path.with_extension("dead")
}

pub fn segment_filename(id: u64, tier: u8) -> String {
    format!("seg_{id:08}_t{tier}.lkcseg")
}

pub fn parse_segment_filename(name: &str) -> Option<(u64, u8)> {
    let stem = name.strip_prefix("seg_")?.strip_suffix(".lkcseg")?;
    let (id_str, tier_str) = stem.split_once("_t")?;
    let id = id_str.parse::<u64>().ok()?;
    let tier = tier_str.parse::<u8>().ok()?;
    Some((id, tier))
}

/// 持有全部已打开段与墓碑的运行时集合，并管理 Tier-of-8 归并及墓碑压缩。
pub struct SegmentSet {
    pub dir: PathBuf,
    pub segments: Vec<(Segment, DeadBitmap)>,
    next_id: u64,
}

impl SegmentSet {
    pub fn open_dir(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let mut found: Vec<(u64, u8, PathBuf)> = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Some((id, tier)) = parse_segment_filename(name) {
                found.push((id, tier, path));
            }
        }
        found.sort_by_key(|&(id, tier, _)| (tier, id));
        let mut segments = Vec::with_capacity(found.len());
        let mut max_id = 0u64;
        for (id, tier, path) in found {
            max_id = max_id.max(id);
            match Segment::open(id, tier, &path) {
                Ok(seg) => {
                    let dead = DeadBitmap::load_or_empty(&dead_path_for(&path), seg.ndocs())?;
                    segments.push((seg, dead));
                }
                Err(_) => {
                    // 损坏遗留半成品段清理，避免阻塞启动。
                    let _ = fs::remove_file(&path);
                    let _ = fs::remove_file(dead_path_for(&path));
                }
            }
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            segments,
            next_id: max_id + 1,
        })
    }

    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// 推送一段由外部输入构建好的 Tier-0 段并纳入管理。
    pub fn add_tier0(&mut self, inputs: Vec<SegmentDocInput>) -> io::Result<()> {
        if inputs.is_empty() {
            return Ok(());
        }
        let id = self.alloc_id();
        let path = self.dir.join(segment_filename(id, 0));
        let seg = Segment::build_to_path(id, 0, &path, inputs)?;
        let dead = DeadBitmap::empty(seg.ndocs());
        self.segments.push((seg, dead));
        let _ = release_allocator_memory();
        Ok(())
    }

    /// 在所有现有段中按精确路径查找并标记死亡（用于文件删除或被新版内容覆盖）。
    pub fn mark_path_dead(&mut self, path: &str) -> bool {
        let mut any = false;
        for (seg, dead) in &mut self.segments {
            if let Some(doc_id) = seg.find_doc_by_path(path) {
                if dead.mark_dead(doc_id) {
                    any = true;
                }
            }
        }
        any
    }

    /// 将所有尚未刷盘的 `.dead` 墓碑增量原子持久化。
    pub fn flush_dead_bitmaps(&mut self) -> io::Result<()> {
        for (seg, dead) in &mut self.segments {
            let dp = dead_path_for(&seg.file_path);
            dead.save_if_dirty(&dp)?;
        }
        Ok(())
    }

    pub fn total_live_docs(&self) -> usize {
        self.segments
            .iter()
            .map(|(_, dead)| dead.live_count() as usize)
            .sum()
    }

    pub fn total_unique_contents(&self) -> usize {
        self.segments
            .iter()
            .map(|(seg, _)| seg.n_contents() as usize)
            .sum()
    }

    pub fn total_mapped_bytes(&self) -> usize {
        self.segments
            .iter()
            .map(|(seg, _)| seg.mapped_bytes())
            .sum()
    }

    /// 执行必要的多段分层归并与高墓碑率真空压缩。返回触发压缩/归并的次数。
    pub fn compact_if_needed(&mut self) -> io::Result<usize> {
        self.flush_dead_bitmaps()?;
        let mut rounds = 0usize;

        loop {
            // 1. 先剔除全死段。
            let mut drop_indices = Vec::new();
            for (idx, (_seg, dead)) in self.segments.iter().enumerate() {
                if dead.live_count() == 0 {
                    drop_indices.push(idx);
                }
            }
            if !drop_indices.is_empty() {
                for idx in drop_indices.into_iter().rev() {
                    let (seg, _dead) = self.segments.remove(idx);
                    let path = seg.file_path.clone();
                    drop(seg);
                    let _ = fs::remove_file(&path);
                    let _ = fs::remove_file(dead_path_for(&path));
                }
                rounds += 1;
                continue;
            }

            // 2. 真空压缩墓碑超过 1/16 的段。
            if let Some(idx) = self
                .segments
                .iter()
                .position(|(_, dead)| dead.exceeds_compact_threshold())
            {
                let target_tier = self.segments[idx].0.tier;
                self.merge_indices(&[idx], target_tier)?;
                rounds += 1;
                continue;
            }

            // 3. 同一 tier 满 8 段触发向下归并至 tier + 1。
            let mut by_tier: BTreeMap<u8, Vec<usize>> = BTreeMap::new();
            for (idx, (seg, _)) in self.segments.iter().enumerate() {
                by_tier.entry(seg.tier).or_default().push(idx);
            }
            let merge_candidate = by_tier
                .into_iter()
                .find(|(_, indices)| indices.len() >= TIER_MERGE_FANOUT);
            if let Some((tier, mut indices)) = merge_candidate {
                indices.truncate(TIER_MERGE_FANOUT);
                let next_tier = tier.saturating_add(1);
                self.merge_indices(&indices, next_tier)?;
                rounds += 1;
                continue;
            }

            break;
        }

        if rounds > 0 {
            let _ = release_allocator_memory();
        }
        Ok(rounds)
    }

    fn merge_indices(&mut self, indices: &[usize], new_tier: u8) -> io::Result<()> {
        let new_id = self.alloc_id();
        let new_path = self.dir.join(segment_filename(new_id, new_tier));
        let pairs: Vec<(&Segment, &DeadBitmap)> = indices
            .iter()
            .map(|&i| (&self.segments[i].0, &self.segments[i].1))
            .collect();
        let merged_seg = merge_segments(new_id, new_tier, &new_path, &pairs)?;
        let merged_dead = DeadBitmap::empty(merged_seg.ndocs());

        // 从大到小移除旧段并删除文件。
        let mut sorted_idx = indices.to_vec();
        sorted_idx.sort_unstable_by(|a, b| b.cmp(a));
        let mut old_paths = Vec::with_capacity(sorted_idx.len());
        for idx in sorted_idx {
            let (old_seg, _) = self.segments.remove(idx);
            old_paths.push(old_seg.file_path.clone());
            drop(old_seg);
        }
        for old_p in old_paths {
            let _ = fs::remove_file(&old_p);
            let _ = fs::remove_file(dead_path_for(&old_p));
        }

        self.segments.push((merged_seg, merged_dead));
        Ok(())
    }
}

/// 纯索引层多段归并（不重读磁盘源文件）：
/// 收集全部存活文档与对应 `content_hash` 映射，跨段重建内容去重与有序倒排 postings，随后原子写段。
pub fn merge_segments(
    new_id: u64,
    new_tier: u8,
    dest_path: &Path,
    sources: &[(&Segment, &DeadBitmap)],
) -> io::Result<Segment> {
    #[derive(Clone)]
    struct LiveDocRecord {
        path: String,
        meta: DocMeta,
        old_seg_idx: usize,
    }

    struct LiveContentRecord {
        content_hash: u64,
        bloom: Vec<u8>,
        decoded_bytes: u32,
        aux_meta: u32,
    }

    let mut live_docs: Vec<LiveDocRecord> = Vec::new();
    // 每个 `(old_seg_idx, old_content_id) -> new_content_id` 映射，利用 `content_hash` 跨段去重。
    let mut old_to_new_cid: Vec<Vec<Option<u32>>> = sources
        .iter()
        .map(|(seg, _)| vec![None; seg.n_contents() as usize])
        .collect();
    let mut hash_to_new_cid: HashMap<u64, u32> = HashMap::new();
    let mut contents: Vec<LiveContentRecord> = Vec::new();

    for (seg_idx, (seg, dead)) in sources.iter().enumerate() {
        for doc_id in 0..seg.ndocs() {
            if dead.is_dead(doc_id) {
                continue;
            }
            let Some(meta) = seg.doc_meta(doc_id) else {
                continue;
            };
            let Some(path) = seg.doc_path(doc_id) else {
                continue;
            };
            let old_cid = meta.content_id;
            let new_cid = if let Some(cid) = old_to_new_cid[seg_idx][old_cid as usize] {
                cid
            } else {
                let cmeta = seg.content_meta(old_cid).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "missing content meta")
                })?;
                if let Some(&existing_cid) = hash_to_new_cid.get(&cmeta.content_hash) {
                    old_to_new_cid[seg_idx][old_cid as usize] = Some(existing_cid);
                    existing_cid
                } else {
                    let start = seg.layout.bloom_blob_off as usize + cmeta.bloom_offset as usize;
                    let end = start + cmeta.bloom_len as usize;
                    let bloom = seg.region.as_slice()[start..end].to_vec();
                    let assigned = contents.len() as u32;
                    contents.push(LiveContentRecord {
                        content_hash: cmeta.content_hash,
                        bloom,
                        decoded_bytes: cmeta.decoded_bytes,
                        aux_meta: cmeta.aux_meta,
                    });
                    hash_to_new_cid.insert(cmeta.content_hash, assigned);
                    old_to_new_cid[seg_idx][old_cid as usize] = Some(assigned);
                    assigned
                }
            };

            live_docs.push(LiveDocRecord {
                path,
                meta: DocMeta {
                    content_id: new_cid,
                    ..meta
                },
                old_seg_idx: seg_idx,
            });
        }
    }

    // 路径升序排列以保持前向前缀压缩与二分查重性质。
    live_docs.sort_by(|a, b| a.path.cmp(&b.path));
    let _ = live_docs.iter().map(|d| d.old_seg_idx).next();

    // 按 key 从各旧段顺序映射 postings 到 `new_content_id` 并去重。
    let mut key_to_cids: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (seg_idx, (seg, _)) in sources.iter().enumerate() {
        let mut decoded_buf = Vec::new();
        for k_idx in 0..seg.n_keys() as usize {
            let entry = seg.dict_entry_at(k_idx);
            let slice = seg.posting_slice(&entry);
            decoded_buf.clear();
            slice.decode_range(0..seg.n_contents(), &mut decoded_buf);
            let target = key_to_cids.entry(entry.key).or_default();
            for &old_cid in &decoded_buf {
                if let Some(new_cid) = old_to_new_cid[seg_idx][old_cid as usize] {
                    target.push(new_cid);
                }
            }
        }
    }

    // 重建每篇文档对应的轻量 `SegmentDocInput` 供共享编码器落盘。
    // 由于 `key_to_cids` 已经覆盖全量内容键，我们直接用共享低层编码器序列化，避免反向拆 key。
    let content_entries: Vec<BuiltContent> = contents
        .into_iter()
        .map(|c| BuiltContent {
            content_hash: c.content_hash,
            bloom: c.bloom,
            decoded_bytes: c.decoded_bytes,
            aux_meta: c.aux_meta,
        })
        .collect();
    let doc_entries: Vec<BuiltDoc> = live_docs
        .into_iter()
        .map(|d| BuiltDoc {
            path: d.path,
            content_id: d.meta.content_id,
            source_kind: d.meta.source_kind,
            doc_rank: d.meta.doc_rank,
            mtime_secs: d.meta.mtime_secs,
            size_bytes: d.meta.size_bytes,
        })
        .collect();

    let n_contents = content_entries.len() as u32;
    let postings: Vec<(u32, u32, EncodedPosting)> = key_to_cids
        .into_par_iter()
        .filter_map(|(key, mut cids)| {
            cids.sort_unstable();
            cids.dedup();
            if cids.is_empty() {
                None
            } else {
                let freq = cids.len() as u32;
                let enc = encode_posting(&cids, n_contents);
                Some((key, freq, enc))
            }
        })
        .collect();

    write_built_segment(dest_path, doc_entries, content_entries, postings)?;
    Segment::open(new_id, new_tier, dest_path)
}

struct BuiltDoc {
    path: String,
    content_id: u32,
    source_kind: ContentSourceKind,
    doc_rank: u16,
    mtime_secs: i64,
    size_bytes: u64,
}

struct BuiltContent {
    content_hash: u64,
    bloom: Vec<u8>,
    decoded_bytes: u32,
    aux_meta: u32,
}

fn write_segment_file(dest_path: &Path, mut inputs: Vec<SegmentDocInput>) -> io::Result<()> {
    inputs.sort_by(|a, b| a.path.cmp(&b.path));

    let mut hash_to_cid: HashMap<u64, u32> = HashMap::new();
    let mut content_entries: Vec<BuiltContent> = Vec::new();
    let mut doc_entries: Vec<BuiltDoc> = Vec::with_capacity(inputs.len());
    let mut key_to_cids: BTreeMap<u32, Vec<u32>> = BTreeMap::new();

    for doc in inputs {
        let (cid, is_new_content) = if let Some(&existing) = hash_to_cid.get(&doc.content_hash) {
            (existing, false)
        } else {
            let cid = content_entries.len() as u32;
            hash_to_cid.insert(doc.content_hash, cid);
            content_entries.push(BuiltContent {
                content_hash: doc.content_hash,
                bloom: doc.bloom,
                decoded_bytes: doc.decoded_bytes,
                aux_meta: doc.aux_meta,
            });
            (cid, true)
        };

        if is_new_content {
            for &key in &doc.keys {
                key_to_cids.entry(key).or_default().push(cid);
            }
        }

        doc_entries.push(BuiltDoc {
            path: doc.path,
            content_id: cid,
            source_kind: doc.source_kind,
            doc_rank: doc.doc_rank,
            mtime_secs: doc.mtime_secs,
            size_bytes: doc.size_bytes,
        });
    }

    let n_contents = content_entries.len() as u32;
    let mut raw_postings: Vec<(u32, Vec<u32>)> = key_to_cids.into_iter().collect();
    let postings: Vec<(u32, u32, EncodedPosting)> = raw_postings
        .par_iter_mut()
        .map(|(key, cids)| {
            cids.sort_unstable();
            cids.dedup();
            let freq = cids.len() as u32;
            let enc = encode_posting(cids, n_contents);
            (*key, freq, enc)
        })
        .collect();

    write_built_segment(dest_path, doc_entries, content_entries, postings)
}

fn write_built_segment(
    dest_path: &Path,
    doc_entries: Vec<BuiltDoc>,
    content_entries: Vec<BuiltContent>,
    mut postings: Vec<(u32, u32, EncodedPosting)>,
) -> io::Result<()> {
    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent)?;
    }
    postings.sort_by_key(|(k, _, _)| *k);

    let ndocs = doc_entries.len() as u32;
    let n_contents = content_entries.len() as u32;
    let n_keys = postings.len() as u32;

    // 1. 前向路径压缩编码。
    let n_blocks = ((doc_entries.len()) + PATH_RESTART_INTERVAL - 1) / PATH_RESTART_INTERVAL;
    let mut paths_index_bytes = Vec::with_capacity(n_blocks * 8);
    let mut paths_data_bytes = Vec::new();
    let mut prev_path: &[u8] = &[];

    for (idx, doc) in doc_entries.iter().enumerate() {
        let cur = doc.path.as_bytes();
        let is_restart = (idx % PATH_RESTART_INTERVAL) == 0;
        if is_restart {
            let off = paths_data_bytes.len() as u64;
            paths_index_bytes.extend_from_slice(&off.to_le_bytes());
        }
        let prefix_len = if is_restart {
            0
        } else {
            common_prefix_len(prev_path, cur).min(u16::MAX as usize)
        };
        let suffix = &cur[prefix_len..];
        let suffix_len = suffix.len().min(u16::MAX as usize);
        paths_data_bytes.extend_from_slice(&(prefix_len as u16).to_le_bytes());
        paths_data_bytes.extend_from_slice(&(suffix_len as u16).to_le_bytes());
        paths_data_bytes.extend_from_slice(&suffix[..suffix_len]);
        prev_path = cur;
    }

    // 2. 文档元数据 + `content_id -> [doc_id]` CSR 表。
    let mut docs_meta_bytes = Vec::with_capacity(doc_entries.len() * DOC_META_SIZE);
    let mut cid_to_docs: Vec<Vec<u32>> = vec![Vec::new(); content_entries.len()];
    for (doc_id, doc) in doc_entries.iter().enumerate() {
        docs_meta_bytes.extend_from_slice(&doc.content_id.to_le_bytes());
        docs_meta_bytes.push(doc.source_kind as u8);
        docs_meta_bytes.push(0);
        docs_meta_bytes.extend_from_slice(&doc.doc_rank.to_le_bytes());
        docs_meta_bytes.extend_from_slice(&doc.mtime_secs.to_le_bytes());
        docs_meta_bytes.extend_from_slice(&doc.size_bytes.to_le_bytes());
        if let Some(bucket) = cid_to_docs.get_mut(doc.content_id as usize) {
            bucket.push(doc_id as u32);
        }
    }

    let mut content_docs_index_bytes = Vec::with_capacity((content_entries.len() + 1) * 4);
    let mut content_docs_data_bytes = Vec::with_capacity(doc_entries.len() * 4);
    let mut csr_cursor = 0u32;
    for docs in &cid_to_docs {
        content_docs_index_bytes.extend_from_slice(&csr_cursor.to_le_bytes());
        for &d in docs {
            content_docs_data_bytes.extend_from_slice(&d.to_le_bytes());
        }
        csr_cursor += docs.len() as u32;
    }
    content_docs_index_bytes.extend_from_slice(&csr_cursor.to_le_bytes());

    // 3. 唯一内容元数据 + Bloom blob。
    let mut contents_meta_bytes = Vec::with_capacity(content_entries.len() * CONTENT_META_SIZE);
    let mut bloom_blob_bytes = Vec::new();
    for c in &content_entries {
        let bloom_off = bloom_blob_bytes.len() as u32;
        let bloom_len = c.bloom.len() as u32;
        bloom_blob_bytes.extend_from_slice(&c.bloom);
        contents_meta_bytes.extend_from_slice(&c.content_hash.to_le_bytes());
        contents_meta_bytes.extend_from_slice(&bloom_off.to_le_bytes());
        contents_meta_bytes.extend_from_slice(&bloom_len.to_le_bytes());
        contents_meta_bytes.extend_from_slice(&c.decoded_bytes.to_le_bytes());
        contents_meta_bytes.extend_from_slice(&c.aux_meta.to_le_bytes());
    }

    // 4. 词典粗索引 + 定长词典条目 + Postings blob。
    let n_coarse = (postings.len() + COARSE_DICT_INTERVAL - 1) / COARSE_DICT_INTERVAL;
    let mut dict_coarse_bytes = Vec::with_capacity(n_coarse * 4);
    let mut dict_entries_bytes = Vec::with_capacity(postings.len() * DICT_ENTRY_SIZE);
    let mut postings_bytes = Vec::new();

    for (idx, (key, doc_freq, enc)) in postings.iter().enumerate() {
        if (idx % COARSE_DICT_INTERVAL) == 0 {
            dict_coarse_bytes.extend_from_slice(&key.to_le_bytes());
        }
        let posting_off = postings_bytes.len() as u64;
        let posting_len = enc.bytes.len() as u32;
        postings_bytes.extend_from_slice(&enc.bytes);

        dict_entries_bytes.extend_from_slice(&key.to_le_bytes());
        dict_entries_bytes.push(enc.kind as u8);
        dict_entries_bytes.extend_from_slice(&[0u8; 3]);
        dict_entries_bytes.extend_from_slice(&posting_off.to_le_bytes());
        dict_entries_bytes.extend_from_slice(&posting_len.to_le_bytes());
        dict_entries_bytes.extend_from_slice(&doc_freq.to_le_bytes());
    }

    // 计算各个分区精确字节偏移。
    let mut cursor = HEADER_SIZE as u64;
    let docs_meta_off = cursor;
    cursor += docs_meta_bytes.len() as u64;
    let paths_index_off = cursor;
    cursor += paths_index_bytes.len() as u64;
    let paths_data_off = cursor;
    cursor += paths_data_bytes.len() as u64;
    let contents_meta_off = cursor;
    cursor += contents_meta_bytes.len() as u64;
    let content_docs_index_off = cursor;
    cursor += content_docs_index_bytes.len() as u64;
    let content_docs_data_off = cursor;
    cursor += content_docs_data_bytes.len() as u64;
    let bloom_blob_off = cursor;
    cursor += bloom_blob_bytes.len() as u64;
    let dict_coarse_off = cursor;
    cursor += dict_coarse_bytes.len() as u64;
    let dict_entries_off = cursor;
    cursor += dict_entries_bytes.len() as u64;
    let postings_off = cursor;

    let tmp_path = dest_path.with_extension("lkcseg.tmp");
    {
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp_path)?;
        let mut w = BufWriter::new(file);
        w.write_all(SEGMENT_MAGIC)?;
        w.write_all(&SEGMENT_VERSION.to_le_bytes())?;
        w.write_all(&ndocs.to_le_bytes())?;
        w.write_all(&n_contents.to_le_bytes())?;
        w.write_all(&n_keys.to_le_bytes())?;
        w.write_all(&0u32.to_le_bytes())?;
        w.write_all(&0u32.to_le_bytes())?;

        w.write_all(&docs_meta_off.to_le_bytes())?;
        w.write_all(&paths_index_off.to_le_bytes())?;
        w.write_all(&paths_data_off.to_le_bytes())?;
        w.write_all(&contents_meta_off.to_le_bytes())?;
        w.write_all(&content_docs_index_off.to_le_bytes())?;
        w.write_all(&content_docs_data_off.to_le_bytes())?;
        w.write_all(&bloom_blob_off.to_le_bytes())?;
        w.write_all(&dict_coarse_off.to_le_bytes())?;
        w.write_all(&dict_entries_off.to_le_bytes())?;
        w.write_all(&postings_off.to_le_bytes())?;

        w.write_all(&docs_meta_bytes)?;
        w.write_all(&paths_index_bytes)?;
        w.write_all(&paths_data_bytes)?;
        w.write_all(&contents_meta_bytes)?;
        w.write_all(&content_docs_index_bytes)?;
        w.write_all(&content_docs_data_bytes)?;
        w.write_all(&bloom_blob_bytes)?;
        w.write_all(&dict_coarse_bytes)?;
        w.write_all(&dict_entries_bytes)?;
        w.write_all(&postings_bytes)?;
        w.flush()?;
    }
    fs::rename(&tmp_path, dest_path)?;
    Ok(())
}

fn validate_layout(l: &SectionLayout, total_len: usize) -> io::Result<()> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "corrupt segment layout");
    let min_end = l
        .postings_off
        .max(l.dict_entries_off + (l.n_keys as u64) * (DICT_ENTRY_SIZE as u64));
    if min_end > total_len as u64 {
        return Err(invalid());
    }
    if l.docs_meta_off + (l.ndocs as u64) * (DOC_META_SIZE as u64) > l.paths_index_off {
        return Err(invalid());
    }
    if l.contents_meta_off + (l.n_contents as u64) * (CONTENT_META_SIZE as u64)
        > l.content_docs_index_off
    {
        return Err(invalid());
    }
    Ok(())
}

#[inline]
fn common_prefix_len(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

#[inline]
fn read_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

#[inline]
fn read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

#[inline]
fn read_u64(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
        buf[off + 4],
        buf[off + 5],
        buf[off + 6],
        buf[off + 7],
    ])
}

#[inline]
fn read_i64(buf: &[u8], off: usize) -> i64 {
    i64::from_le_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
        buf[off + 4],
        buf[off + 5],
        buf[off + 6],
        buf[off + 7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_index::gram::{fold_ascii_slice, query_short_hashes, trigrams_folded};
    use tempfile::TempDir;

    fn make_doc(path: &str, text: &[u8], mtime: i64) -> SegmentDocInput {
        SegmentDocInput::from_decoded_text(
            path.to_string(),
            mtime,
            text.len() as u64,
            ContentSourceKind::PlainText,
            400,
            0,
            text,
            true,
        )
    }

    #[test]
    fn segment_roundtrip_content_dedup_and_dead_bitmap() {
        let tmp = TempDir::new().unwrap();
        let seg_path = tmp.path().join("seg_00000001_t0.lkcseg");

        let shared_code = b"pub fn hybrid_search(query: &str) -> Vec<u32> { vec![1, 2, 3] }\n";
        let other_code = b"// localized Chinese comment \xe4\xb8\xad\xe6\x96\x87\xe6\xa3\x80\xe7\xb4\xa2\nfn other() {}\n";

        let inputs = vec![
            make_doc("/repo/worktree_b/src/search.rs", shared_code, 100),
            make_doc("/repo/worktree_a/src/search.rs", shared_code, 100),
            make_doc("/repo/other.rs", other_code, 200),
        ];

        let seg = Segment::build_to_path(1, 0, &seg_path, inputs).unwrap();
        assert_eq!(seg.ndocs(), 3);
        // 两份完全相同正文的文件去重为 1 条唯一内容，总数由 3 压缩为 2。
        assert_eq!(seg.n_contents(), 2);

        let doc_a = seg.find_doc_by_path("/repo/worktree_a/src/search.rs").unwrap();
        let doc_b = seg.find_doc_by_path("/repo/worktree_b/src/search.rs").unwrap();
        assert_eq!(seg.doc_path(doc_a).as_deref(), Some("/repo/worktree_a/src/search.rs"));
        assert_eq!(
            seg.doc_meta(doc_a).unwrap().content_id,
            seg.doc_meta(doc_b).unwrap().content_id
        );

        let folded = fold_ascii_slice(b"HYBRID_SEARCH");
        let tri = trigrams_folded(&folded)[0];
        let entry = seg.lookup_key(tri).unwrap();
        let mut cids = Vec::new();
        seg.posting_slice(&entry).decode_range(0..seg.n_contents(), &mut cids);
        assert_eq!(cids.len(), 1);
        let mapped_docs = seg.doc_ids_for_content(cids[0]);
        assert_eq!(mapped_docs.len(), 2);

        let shorts = query_short_hashes(&folded);
        assert!(seg.content_bloom_contains_all(cids[0], &shorts));

        // 墓碑原子落盘与重加载
        let dp = dead_path_for(&seg_path);
        let mut dead = DeadBitmap::empty(seg.ndocs());
        assert!(dead.mark_dead(doc_a));
        dead.save_if_dirty(&dp).unwrap();
        let reloaded = DeadBitmap::load_or_empty(&dp, seg.ndocs()).unwrap();
        assert!(reloaded.is_dead(doc_a));
        assert!(!reloaded.is_dead(doc_b));
        assert_eq!(reloaded.live_count(), 2);
    }

    #[test]
    fn segment_set_compacts_dead_above_sixteenth_and_tier_of_eight() {
        let tmp = TempDir::new().unwrap();
        let mut set = SegmentSet::open_dir(tmp.path()).unwrap();

        // 1. 超过 1/16 死亡阈值触发真空压缩。
        let mut docs = Vec::new();
        for i in 0..20 {
            let body = format!("pub fn fn_{i}() {{ let token_{i} = \"val_{i}\"; }}\n");
            docs.push(make_doc(&format!("/src/f{i:02}.rs"), body.as_bytes(), 10));
        }
        set.add_tier0(docs).unwrap();
        assert!(set.mark_path_dead("/src/f00.rs"));
        assert!(set.mark_path_dead("/src/f01.rs")); // 2 / 20 = 1/10 > 1/16
        let rounds = set.compact_if_needed().unwrap();
        assert!(rounds >= 1);
        assert_eq!(set.total_live_docs(), 18);
        assert_eq!(set.segments[0].1.dead_count(), 0);
        assert!(set.segments[0].0.find_doc_by_path("/src/f00.rs").is_none());

        // 2. 凑齐 8 个 Tier-0 段触发 8 合 1 升层至 Tier-1，同时跨段内容去重保持有效。
        for batch in 0..8 {
            let text = format!("fn common_token() {{}} fn batch_{batch}() {{}}\n");
            set.add_tier0(vec![make_doc(
                &format!("/batch/b_{batch}.rs"),
                text.as_bytes(),
                20,
            )])
            .unwrap();
        }
        let merge_rounds = set.compact_if_needed().unwrap();
        assert!(merge_rounds >= 1);
        assert!(set.segments.iter().any(|(s, _)| s.tier == 1));
        assert_eq!(set.total_live_docs(), 18 + 8);
    }
}
