use std::ops::Range;

/// 将有序文档编号分块编码的块容量。128 项兼顾随机跳过速度与极低跳表存储开销。
pub const POSTING_BLOCK_SIZE: usize = 128;

/// 写入位流缓冲（小端低位优先）。
#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    bits: u32,
}

impl BitWriter {
    #[inline]
    fn push(&mut self, value: u32, width: u32) {
        if width == 0 {
            return;
        }
        self.acc |= (value as u64) << self.bits;
        self.bits += width;
        while self.bits >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.bits -= 8;
        }
    }

    #[inline]
    fn bit_len(&self) -> u32 {
        (self.out.len() as u32) * 8 + self.bits
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

/// 顺序读取位流缓冲（小端低位优先）。
struct BitReader<'a> {
    bytes: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    #[inline]
    fn new(bytes: &'a [u8], bit_offset: usize) -> Self {
        Self {
            bytes,
            bit_pos: bit_offset,
        }
    }

    #[inline]
    fn read(&mut self, width: u32) -> u32 {
        if width == 0 {
            return 0;
        }
        let byte_idx = self.bit_pos >> 3;
        let bit_shift = self.bit_pos & 7;
        let mut raw = 0u64;
        let avail = self.bytes.len().saturating_sub(byte_idx).min(8);
        if avail == 8 {
            raw = u64::from_le_bytes(self.bytes[byte_idx..byte_idx + 8].try_into().unwrap());
        } else {
            for i in 0..avail {
                raw |= (self.bytes[byte_idx + i] as u64) << (i * 8);
            }
        }
        self.bit_pos += width as usize;
        let mask = if width == 32 {
            u32::MAX as u64
        } else {
            (1u64 << width) - 1
        };
        ((raw >> bit_shift) & mask) as u32
    }
}

#[inline]
fn bit_width(range_max: u32) -> u32 {
    32 - range_max.leading_zeros()
}

/// Binary Interpolative Coding：对有序严格递增序列 `vals`（边界 `[lo, hi]`）进行紧凑编码。
fn encode_ic_slice(vals: &[u32], lo: u32, hi: u32, w: &mut BitWriter) {
    let n = vals.len();
    if n == 0 {
        return;
    }
    let mid = n / 2;
    let m_val = vals[mid];
    let min_mid = lo + mid as u32;
    let max_mid = hi - (n - 1 - mid) as u32;
    let span = max_mid.saturating_sub(min_mid);
    w.push(m_val - min_mid, bit_width(span));

    if mid > 0 {
        encode_ic_slice(&vals[..mid], lo, m_val - 1, w);
    }
    if mid + 1 < n {
        encode_ic_slice(&vals[mid + 1..], m_val + 1, hi, w);
    }
}

fn decode_ic_slice(r: &mut BitReader<'_>, n: usize, lo: u32, hi: u32, out: &mut Vec<u32>) {
    if n == 0 {
        return;
    }
    let mid = n / 2;
    let min_mid = lo + mid as u32;
    let max_mid = hi - (n - 1 - mid) as u32;
    let span = max_mid.saturating_sub(min_mid);
    let m_val = min_mid + r.read(bit_width(span));

    let base_idx = out.len();
    out.resize(base_idx + n, 0);
    out[base_idx + mid] = m_val;

    fn fill_slice(r: &mut BitReader<'_>, dst: &mut [u32], lo: u32, hi: u32) {
        let len = dst.len();
        if len == 0 {
            return;
        }
        let m = len / 2;
        let lo_bound = lo + m as u32;
        let hi_bound = hi - (len - 1 - m) as u32;
        let v = lo_bound + r.read(bit_width(hi_bound.saturating_sub(lo_bound)));
        dst[m] = v;
        if m > 0 {
            fill_slice(r, &mut dst[..m], lo, v - 1);
        }
        if m + 1 < len {
            fill_slice(r, &mut dst[m + 1..], v + 1, hi);
        }
    }

    if mid > 0 {
        fill_slice(r, &mut out[base_idx..base_idx + mid], lo, m_val - 1);
    }
    if mid + 1 < n {
        fill_slice(r, &mut out[base_idx + mid + 1..base_idx + n], m_val + 1, hi);
    }
}

fn write_varint(mut v: u32, out: &mut Vec<u8>) {
    loop {
        let low = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(low);
            break;
        }
        out.push(low | 0x80);
    }
}

#[inline]
fn read_varint(bytes: &[u8], cursor: &mut usize) -> Option<u32> {
    let mut val = 0u32;
    let mut shift = 0u32;
    while *cursor < bytes.len() && shift < 35 {
        let b = bytes[*cursor];
        *cursor += 1;
        val |= ((b & 0x7F) as u32) << shift;
        if (b & 0x80) == 0 {
            return Some(val);
        }
        shift += 7;
    }
    None
}

/// 编码结果标识，与段文件内存储的倒排载荷一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PostingKind {
    /// 稠密位图：文档占比超过 1/8 时直存位图，兼顾 O(1) 位检测与体积极限。
    Bitset = 0,
    /// 单块插值编码（文档数 <= 128）。
    SmallIc = 1,
    /// 分块跳表 + 插值编码（文档数 > 128）。
    BlockIc = 2,
}

impl PostingKind {
    #[inline]
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Bitset),
            1 => Some(Self::SmallIc),
            2 => Some(Self::BlockIc),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedPosting {
    pub kind: PostingKind,
    pub bytes: Vec<u8>,
}

/// 将严格递增的文档 ID 切片压缩为最紧凑的倒排表示。
pub fn encode_posting(sorted_docs: &[u32], ndocs: u32) -> EncodedPosting {
    // 稠密倒排直接用位图（每 8 篇文档 1 字节比存多块 varint + 跳表更省空间，且交集更快）。
    if !sorted_docs.is_empty() && (sorted_docs.len() * 8) > ndocs as usize {
        let byte_len = (ndocs as usize).div_ceil(8);
        let mut bits = vec![0u8; byte_len];
        for &d in sorted_docs {
            if d < ndocs {
                bits[(d >> 3) as usize] |= 1u8 << (d & 7);
            }
        }
        return EncodedPosting {
            kind: PostingKind::Bitset,
            bytes: bits,
        };
    }

    if sorted_docs.len() <= POSTING_BLOCK_SIZE {
        let mut out = Vec::new();
        out.push(sorted_docs.len() as u8);
        if let (Some(&first), Some(&last)) = (sorted_docs.first(), sorted_docs.last()) {
            write_varint(first, &mut out);
            if sorted_docs.len() > 1 {
                write_varint(last - first, &mut out);
            }
            if sorted_docs.len() > 2 {
                let mut bw = BitWriter::default();
                encode_ic_slice(
                    &sorted_docs[1..sorted_docs.len() - 1],
                    first + 1,
                    last - 1,
                    &mut bw,
                );
                out.extend_from_slice(&bw.finish());
            }
        }
        return EncodedPosting {
            kind: PostingKind::SmallIc,
            bytes: out,
        };
    }

    // 分块插值编码：跳表每块固定 12 字节 `(first: u32, last: u32, bit_offset: u32)`，支持 O(log B) 跨块跳跃。
    let mut bw = BitWriter::default();
    let chunks: Vec<&[u32]> = sorted_docs.chunks(POSTING_BLOCK_SIZE).collect();
    let total_docs = sorted_docs.len() as u32;
    let num_blocks = chunks.len() as u32;

    let mut skip_table = Vec::with_capacity(8 + (num_blocks as usize) * 12);
    skip_table.extend_from_slice(&total_docs.to_le_bytes());
    skip_table.extend_from_slice(&num_blocks.to_le_bytes());

    for blk in &chunks {
        let first = blk[0];
        let last = blk[blk.len() - 1];
        let bit_offset = bw.bit_len();
        skip_table.extend_from_slice(&first.to_le_bytes());
        skip_table.extend_from_slice(&last.to_le_bytes());
        skip_table.extend_from_slice(&bit_offset.to_le_bytes());

        if blk.len() > 2 {
            encode_ic_slice(&blk[1..blk.len() - 1], first + 1, last - 1, &mut bw);
        }
    }

    let bit_bytes = bw.finish();
    skip_table.extend_from_slice(&bit_bytes);
    EncodedPosting {
        kind: PostingKind::BlockIc,
        bytes: skip_table,
    }
}

/// 零拷贝只读倒排视图，面向 mmap 段上的直接范围解码与跳表剪枝求交。
#[derive(Clone, Copy)]
pub enum PostingSlice<'a> {
    Bitset { data: &'a [u8], ndocs: u32 },
    SmallIc { data: &'a [u8] },
    BlockIc { data: &'a [u8] },
}

impl<'a> PostingSlice<'a> {
    /// 判断是否可在常数时间内单点成员检测（位图支持 O(1) 位测试）。
    #[inline]
    pub fn contains_bitset(&self, doc_id: u32) -> Option<bool> {
        match self {
            PostingSlice::Bitset { data, ndocs } => {
                if doc_id >= *ndocs {
                    return Some(false);
                }
                let idx = (doc_id >> 3) as usize;
                Some(data.get(idx).is_some_and(|&b| (b & (1u8 << (doc_id & 7))) != 0))
            }
            _ => None,
        }
    }

    /// 返回倒排内文档数量估计（用于查询规划先查最短链）。
    pub fn approx_len(&self) -> usize {
        match self {
            PostingSlice::Bitset { data, .. } => {
                data.iter().map(|b| b.count_ones() as usize).sum()
            }
            PostingSlice::SmallIc { data } => data.first().copied().unwrap_or(0) as usize,
            PostingSlice::BlockIc { data } => {
                if data.len() < 4 {
                    0
                } else {
                    u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize
                }
            }
        }
    }

    /// 解码落在 `doc_range` 内的文档 ID，并追加至 `out`。
    pub fn decode_range(&self, doc_range: Range<u32>, out: &mut Vec<u32>) {
        if doc_range.is_empty() {
            return;
        }
        match self {
            PostingSlice::Bitset { data, ndocs } => {
                let hi = doc_range.end.min(*ndocs);
                let lo = doc_range.start.min(hi);
                if lo >= hi {
                    return;
                }
                let start_word = (lo as usize) / 64;
                let end_word = ((hi as usize) + 63) / 64;
                for word_idx in start_word..end_word {
                    let byte_off = word_idx * 8;
                    if byte_off >= data.len() {
                        break;
                    }
                    let mut w = 0u64;
                    let rem = (data.len() - byte_off).min(8);
                    for i in 0..rem {
                        w |= (data[byte_off + i] as u64) << (i * 8);
                    }
                    if w == 0 {
                        continue;
                    }
                    let base = (word_idx * 64) as u32;
                    while w != 0 {
                        let tz = w.trailing_zeros();
                        let d = base + tz;
                        if d >= lo && d < hi {
                            out.push(d);
                        }
                        w &= w - 1;
                    }
                }
            }
            PostingSlice::SmallIc { data } => {
                let mut scratch = Vec::new();
                decode_small_ic(data, &mut scratch);
                for d in scratch {
                    if d >= doc_range.start && d < doc_range.end {
                        out.push(d);
                    }
                }
            }
            PostingSlice::BlockIc { data } => {
                decode_block_ic_range(data, doc_range, out);
            }
        }
    }

    /// 用当前倒排对已有有序候选集合做就地求交过滤，避免把高频率词表的数万文档全量解码。
    pub fn retain(&self, candidates: &mut Vec<u32>) {
        if candidates.is_empty() {
            return;
        }
        match self {
            PostingSlice::Bitset { data, ndocs } => {
                candidates.retain(|&d| {
                    d < *ndocs
                        && data
                            .get((d >> 3) as usize)
                            .is_some_and(|&b| (b & (1u8 << (d & 7))) != 0)
                });
            }
            PostingSlice::SmallIc { data } => {
                let mut decoded = Vec::new();
                decode_small_ic(data, &mut decoded);
                intersect_sorted(candidates, &decoded);
            }
            PostingSlice::BlockIc { data } => {
                let Some(meta) = BlockMeta::parse(data) else {
                    candidates.clear();
                    return;
                };
                let mut write_idx = 0usize;
                let mut cand_idx = 0usize;
                let mut blk_buf = Vec::with_capacity(POSTING_BLOCK_SIZE);

                while cand_idx < candidates.len() {
                    let target = candidates[cand_idx];
                    // 二分跳跃到第一个 last >= target 的数据块
                    let blk_idx = meta.partition_point_last(target);
                    if blk_idx >= meta.num_blocks {
                        break;
                    }
                    let (first, last, bit_off) = meta.entry(blk_idx);
                    if target < first {
                        // 候选集中小于该块起点的值全部跳过
                        cand_idx += candidates[cand_idx..].partition_point(|&d| d < first);
                        continue;
                    }
                    blk_buf.clear();
                    let elem_count = meta.block_elem_count(blk_idx);
                    decode_single_block(
                        meta.payload,
                        elem_count,
                        first,
                        last,
                        bit_off,
                        &mut blk_buf,
                    );
                    let mut p = 0usize;
                    while cand_idx < candidates.len() && candidates[cand_idx] <= last {
                        let c = candidates[cand_idx];
                        while p < blk_buf.len() && blk_buf[p] < c {
                            p += 1;
                        }
                        if p < blk_buf.len() && blk_buf[p] == c {
                            candidates[write_idx] = c;
                            write_idx += 1;
                        }
                        cand_idx += 1;
                    }
                }
                candidates.truncate(write_idx);
            }
        }
    }
}

fn decode_small_ic(data: &[u8], out: &mut Vec<u32>) {
    if data.is_empty() {
        return;
    }
    let count = data[0] as usize;
    if count == 0 {
        return;
    }
    let mut cursor = 1usize;
    let Some(first) = read_varint(data, &mut cursor) else {
        return;
    };
    if count == 1 {
        out.push(first);
        return;
    }
    let Some(span) = read_varint(data, &mut cursor) else {
        return;
    };
    let last = first + span;
    decode_single_block(&data[cursor..], count, first, last, 0, out);
}

fn decode_single_block(
    bit_payload: &[u8],
    count: usize,
    first: u32,
    last: u32,
    bit_offset: usize,
    out: &mut Vec<u32>,
) {
    if count == 0 {
        return;
    }
    out.push(first);
    if count == 1 {
        return;
    }
    if count > 2 {
        let mut reader = BitReader::new(bit_payload, bit_offset);
        decode_ic_slice(&mut reader, count - 2, first + 1, last - 1, out);
    }
    out.push(last);
}

struct BlockMeta<'a> {
    total_docs: usize,
    num_blocks: usize,
    entries: &'a [u8],
    payload: &'a [u8],
}

impl<'a> BlockMeta<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }
        let total_docs = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
        let num_blocks = u32::from_le_bytes(data[4..8].try_into().ok()?) as usize;
        let table_bytes = num_blocks.checked_mul(12)?;
        if data.len() < 8 + table_bytes {
            return None;
        }
        Some(Self {
            total_docs,
            num_blocks,
            entries: &data[8..8 + table_bytes],
            payload: &data[8 + table_bytes..],
        })
    }

    #[inline]
    fn entry(&self, idx: usize) -> (u32, u32, usize) {
        let off = idx * 12;
        let first = u32::from_le_bytes(self.entries[off..off + 4].try_into().unwrap());
        let last = u32::from_le_bytes(self.entries[off + 4..off + 8].try_into().unwrap());
        let bit_off = u32::from_le_bytes(self.entries[off + 8..off + 12].try_into().unwrap());
        (first, last, bit_off as usize)
    }

    #[inline]
    fn block_elem_count(&self, idx: usize) -> usize {
        if idx + 1 < self.num_blocks {
            POSTING_BLOCK_SIZE
        } else {
            self.total_docs - idx * POSTING_BLOCK_SIZE
        }
    }

    fn partition_point_last(&self, target: u32) -> usize {
        let mut lo = 0usize;
        let mut hi = self.num_blocks;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (_, last, _) = self.entry(mid);
            if last < target {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }
}

fn decode_block_ic_range(data: &[u8], doc_range: Range<u32>, out: &mut Vec<u32>) {
    let Some(meta) = BlockMeta::parse(data) else {
        return;
    };
    let start_blk = meta.partition_point_last(doc_range.start);
    let mut blk_buf = Vec::with_capacity(POSTING_BLOCK_SIZE);
    for blk_idx in start_blk..meta.num_blocks {
        let (first, last, bit_off) = meta.entry(blk_idx);
        if first >= doc_range.end {
            break;
        }
        let elem_count = meta.block_elem_count(blk_idx);
        if first >= doc_range.start && last < doc_range.end {
            decode_single_block(meta.payload, elem_count, first, last, bit_off, out);
        } else {
            blk_buf.clear();
            decode_single_block(meta.payload, elem_count, first, last, bit_off, &mut blk_buf);
            for &d in &blk_buf {
                if d >= doc_range.start && d < doc_range.end {
                    out.push(d);
                }
            }
        }
    }
}

#[inline]
fn intersect_sorted(candidates: &mut Vec<u32>, sorted_other: &[u32]) {
    let mut w = 0usize;
    let mut j = 0usize;
    for i in 0..candidates.len() {
        let c = candidates[i];
        while j < sorted_other.len() && sorted_other[j] < c {
            j += 1;
        }
        if j < sorted_other.len() && sorted_other[j] == c {
            candidates[w] = c;
            w += 1;
        }
    }
    candidates.truncate(w);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice_from<'a>(encoded: &'a EncodedPosting, ndocs: u32) -> PostingSlice<'a> {
        match encoded.kind {
            PostingKind::Bitset => PostingSlice::Bitset {
                data: &encoded.bytes,
                ndocs,
            },
            PostingKind::SmallIc => PostingSlice::SmallIc {
                data: &encoded.bytes,
            },
            PostingKind::BlockIc => PostingSlice::BlockIc {
                data: &encoded.bytes,
            },
        }
    }

    #[test]
    fn roundtrip_small_contiguous_and_dense_bitset() {
        let ndocs = 200u32;

        // 1 & 2 elements
        for list in [vec![0u32], vec![42u32], vec![3u32, 199u32], vec![10, 11, 12, 13, 14]] {
            let enc = encode_posting(&list, ndocs);
            assert_eq!(enc.kind, PostingKind::SmallIc);
            let mut decoded = Vec::new();
            slice_from(&enc, ndocs).decode_range(0..ndocs, &mut decoded);
            assert_eq!(decoded, list);
        }

        // Dense (> 1/8) switches to Bitset
        let dense: Vec<u32> = (0..200).step_by(3).collect();
        let enc = encode_posting(&dense, ndocs);
        assert_eq!(enc.kind, PostingKind::Bitset);
        let mut decoded = Vec::new();
        slice_from(&enc, ndocs).decode_range(10..50, &mut decoded);
        assert_eq!(
            decoded,
            dense.iter().copied().filter(|&d| (10..50).contains(&d)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn roundtrip_and_intersection_across_multiblock_ic() {
        let ndocs = 20_000u32;
        let list_a: Vec<u32> = (0..18_000).filter(|d| d % 11 == 0 || d % 37 == 0).collect();
        let list_b: Vec<u32> = (0..18_000).filter(|d| d % 17 == 0 || d % 37 == 0).collect();

        let enc_a = encode_posting(&list_a, ndocs);
        let enc_b = encode_posting(&list_b, ndocs);
        assert_eq!(enc_a.kind, PostingKind::BlockIc);
        assert_eq!(enc_b.kind, PostingKind::BlockIc);

        let reader_a = slice_from(&enc_a, ndocs);
        let reader_b = slice_from(&enc_b, ndocs);

        let mut all_a = Vec::new();
        reader_a.decode_range(0..ndocs, &mut all_a);
        assert_eq!(all_a, list_a);

        // Sub-range decode
        let mut sub_a = Vec::new();
        reader_a.decode_range(300..900, &mut sub_a);
        assert_eq!(
            sub_a,
            list_a.iter().copied().filter(|&d| (300..900).contains(&d)).collect::<Vec<_>>()
        );

        // Intersection via skip table
        let mut cands = list_a.clone();
        reader_b.retain(&mut cands);
        let expected: Vec<u32> = list_a
            .iter()
            .copied()
            .filter(|d| list_b.binary_search(d).is_ok())
            .collect();
        assert_eq!(cands, expected);
    }
}
