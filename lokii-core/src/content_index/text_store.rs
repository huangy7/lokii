use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

const STORE_DATA_MAGIC: &[u8; 8] = b"LKTSTR01";
const STORE_IDX_MAGIC: &[u8; 8] = b"LKTIDX01";
const DEFAULT_CHUNK_BYTES: usize = 64 * 1024;
pub const DEFAULT_LRU_CAP_BYTES: usize = 32 * 1024 * 1024;

const CODEC_RAW: u8 = 0;
const CODEC_LZFSE: u8 = 1;

#[cfg(target_os = "macos")]
const COMPRESSION_LZFSE: u32 = 0x801;

#[cfg(target_os = "macos")]
#[link(name = "compression")]
extern "C" {
    fn compression_encode_buffer(
        dst_buffer: *mut u8,
        dst_size: usize,
        src_buffer: *const u8,
        src_size: usize,
        scratch_buffer: *mut libc::c_void,
        algorithm: u32,
    ) -> usize;

    fn compression_decode_buffer(
        dst_buffer: *mut u8,
        dst_size: usize,
        src_buffer: *const u8,
        src_size: usize,
        scratch_buffer: *mut libc::c_void,
        algorithm: u32,
    ) -> usize;
}

fn compress_chunk_lzfse(src: &[u8]) -> (u8, Vec<u8>) {
    if src.len() < 64 {
        return (CODEC_RAW, src.to_vec());
    }
    #[cfg(target_os = "macos")]
    {
        let mut dst = vec![0u8; src.len()];
        // SAFETY: 输入与输出切片均已按实际长度分配且互不重叠。
        let written = unsafe {
            compression_encode_buffer(
                dst.as_mut_ptr(),
                dst.len(),
                src.as_ptr(),
                src.len(),
                std::ptr::null_mut(),
                COMPRESSION_LZFSE,
            )
        };
        if written > 0 && written < src.len() {
            dst.truncate(written);
            return (CODEC_LZFSE, dst);
        }
    }
    (CODEC_RAW, src.to_vec())
}

fn decompress_chunk_lzfse(codec: u8, src: &[u8], expected_len: usize) -> io::Result<Vec<u8>> {
    match codec {
        CODEC_RAW => {
            if src.len() != expected_len {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "raw chunk size mismatch",
                ));
            }
            Ok(src.to_vec())
        }
        CODEC_LZFSE => {
            #[cfg(target_os = "macos")]
            {
                let mut dst = vec![0u8; expected_len];
                // SAFETY: `dst` 已精确预留 `expected_len` 字节，由 Apple libcompression 解码校验返回实际字节数。
                let decoded = unsafe {
                    compression_decode_buffer(
                        dst.as_mut_ptr(),
                        dst.len(),
                        src.as_ptr(),
                        src.len(),
                        std::ptr::null_mut(),
                        COMPRESSION_LZFSE,
                    )
                };
                if decoded != expected_len {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "lzfse decoded length mismatch",
                    ));
                }
                Ok(dst)
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = src;
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "lzfse codec requires macos",
                ))
            }
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unknown chunk codec",
        )),
    }
}

#[derive(Debug, Clone, bitcode::Encode, bitcode::Decode)]
struct ChunkDesc {
    offset: u64,
    compressed_len: u32,
    raw_len: u32,
    codec: u8,
}

#[derive(Debug, Clone, bitcode::Encode, bitcode::Decode)]
struct StoredTextEntry {
    content_hash: u64,
    total_raw_len: u32,
    chunks: Vec<ChunkDesc>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TextStoreStats {
    pub entry_count: usize,
    pub disk_bytes: u64,
    pub cache_bytes: usize,
    pub cache_cap_bytes: usize,
}

struct LruTextCache {
    cap_bytes: usize,
    used_bytes: usize,
    map: HashMap<u64, Arc<Vec<u8>>>,
    order: VecDeque<u64>,
}

impl LruTextCache {
    fn new(cap_bytes: usize) -> Self {
        Self {
            cap_bytes,
            used_bytes: 0,
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&mut self, hash: u64) -> Option<Arc<Vec<u8>>> {
        let val = self.map.get(&hash)?.clone();
        self.touch(hash);
        Some(val)
    }

    fn insert(&mut self, hash: u64, data: Arc<Vec<u8>>) {
        let item_len = data.len();
        if self.cap_bytes == 0 || item_len > self.cap_bytes {
            return;
        }
        if let Some(prev) = self.map.remove(&hash) {
            self.used_bytes = self.used_bytes.saturating_sub(prev.len());
            self.order.retain(|&h| h != hash);
        }
        while self.used_bytes + item_len > self.cap_bytes {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(removed) = self.map.remove(&oldest) {
                self.used_bytes = self.used_bytes.saturating_sub(removed.len());
            }
        }
        self.used_bytes += item_len;
        self.map.insert(hash, data);
        self.order.push_back(hash);
    }

    fn remove(&mut self, hash: u64) {
        if let Some(removed) = self.map.remove(&hash) {
            self.used_bytes = self.used_bytes.saturating_sub(removed.len());
            self.order.retain(|&h| h != hash);
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.used_bytes = 0;
    }

    fn touch(&mut self, hash: u64) {
        if let Some(pos) = self.order.iter().position(|&h| h == hash) {
            self.order.remove(pos);
        }
        self.order.push_back(hash);
    }
}

struct StoreIndexState {
    entries: HashMap<u64, StoredTextEntry>,
    data_len: u64,
    dirty: bool,
}

/// 分块压缩富文档正文仓：
/// Office/PDF/图片抽取后的纯文本经 64 KiB 分块与 LZFSE 压缩落盘，
/// 配合严格字节上限（默认 32 MiB）的 LRU 解压缓存控制内存峰值。
pub struct TextStore {
    dir: PathBuf,
    data_path: PathBuf,
    idx_path: PathBuf,
    state: RwLock<StoreIndexState>,
    write_lock: Mutex<()>,
    lru: Mutex<LruTextCache>,
}

impl TextStore {
    pub fn open(dir: &Path, max_cache_bytes: usize) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let data_path = dir.join("text_store.dat");
        let idx_path = dir.join("text_store.idx");

        let mut data_len = if data_path.exists() {
            let len = fs::metadata(&data_path)?.len();
            if len >= STORE_DATA_MAGIC.len() as u64 {
                len
            } else {
                init_data_file(&data_path)?
            }
        } else {
            init_data_file(&data_path)?
        };

        let mut entries = load_index_file(&idx_path, data_len).unwrap_or_default();
        if entries.is_empty() && data_len > STORE_DATA_MAGIC.len() as u64 {
            // 若索引损坏但数据区仍大，重建空文件避免 orphaned 空间无限驻留。
            data_len = init_data_file(&data_path)?;
            entries.clear();
        }

        Ok(Self {
            dir: dir.to_path_buf(),
            data_path,
            idx_path,
            state: RwLock::new(StoreIndexState {
                entries,
                data_len,
                dirty: false,
            }),
            write_lock: Mutex::new(()),
            lru: Mutex::new(LruTextCache::new(max_cache_bytes)),
        })
    }

    pub fn contains(&self, content_hash: u64) -> bool {
        self.state.read().entries.contains_key(&content_hash)
    }

    /// 写入（或去重跳过）一份已抽取文本；同时将该原文放入受限 LRU 缓存以备近实时检索。
    pub fn put_text(&self, content_hash: u64, text: &[u8]) -> io::Result<()> {
        if self.contains(content_hash) {
            return Ok(());
        }

        let chunks_raw: Vec<(u8, Vec<u8>, u32)> = if text.is_empty() {
            Vec::new()
        } else {
            text.chunks(DEFAULT_CHUNK_BYTES)
                .map(|c| {
                    let (codec, payload) = compress_chunk_lzfse(c);
                    (codec, payload, c.len() as u32)
                })
                .collect()
        };

        let _guard = self.write_lock.lock();
        if self.state.read().entries.contains_key(&content_hash) {
            return Ok(());
        }

        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&self.data_path)?;
        let mut cursor = file.seek(SeekFrom::End(0))?;
        if cursor < STORE_DATA_MAGIC.len() as u64 {
            file.seek(SeekFrom::Start(0))?;
            file.write_all(STORE_DATA_MAGIC)?;
            cursor = STORE_DATA_MAGIC.len() as u64;
        }

        let mut chunk_descs = Vec::with_capacity(chunks_raw.len());
        {
            let mut w = BufWriter::new(&mut file);
            for (codec, payload, raw_len) in chunks_raw {
                let off = cursor;
                w.write_all(&payload)?;
                cursor += payload.len() as u64;
                chunk_descs.push(ChunkDesc {
                    offset: off,
                    compressed_len: payload.len() as u32,
                    raw_len,
                    codec,
                });
            }
            w.flush()?;
        }

        let total_raw_len = text.len().min(u32::MAX as usize) as u32;
        let entry = StoredTextEntry {
            content_hash,
            total_raw_len,
            chunks: chunk_descs,
        };

        {
            let mut st = self.state.write();
            st.data_len = cursor;
            st.entries.insert(content_hash, entry);
            st.dirty = true;
        }

        self.lru
            .lock()
            .insert(content_hash, Arc::new(text.to_vec()));

        Ok(())
    }

    /// 读取指定 `content_hash` 对应的完整解压后文本（命中 LRU 零磁盘读取）。
    pub fn get_text(&self, content_hash: u64) -> Option<Arc<Vec<u8>>> {
        if let Some(hit) = self.lru.lock().get(content_hash) {
            return Some(hit);
        }

        let entry = {
            let st = self.state.read();
            st.entries.get(&content_hash)?.clone()
        };

        let mut out = Vec::with_capacity(entry.total_raw_len as usize);
        if !entry.chunks.is_empty() {
            let mut file = File::open(&self.data_path).ok()?;
            let mut raw_buf = Vec::new();
            for chunk in &entry.chunks {
                raw_buf.resize(chunk.compressed_len as usize, 0);
                file.seek(SeekFrom::Start(chunk.offset)).ok()?;
                file.read_exact(&mut raw_buf).ok()?;
                let decoded =
                    decompress_chunk_lzfse(chunk.codec, &raw_buf, chunk.raw_len as usize).ok()?;
                out.extend_from_slice(&decoded);
            }
        }

        let arc = Arc::new(out);
        self.lru.lock().insert(content_hash, arc.clone());
        Some(arc)
    }

    pub fn flush(&self) -> io::Result<()> {
        let _guard = self.write_lock.lock();
        let st = self.state.read();
        if !st.dirty {
            return Ok(());
        }
        save_index_file(&self.idx_path, &st.entries)?;
        drop(st);
        self.state.write().dirty = false;
        Ok(())
    }

    /// 垃圾回收：仅保留当前仍在活跃段或覆盖层中引用的富文档文本，原子重写数据仓。
    pub fn compact_keeping(&self, live_hashes: &HashSet<u64>) -> io::Result<usize> {
        let _guard = self.write_lock.lock();
        let old_entries = {
            let st = self.state.read();
            st.entries.clone()
        };
        let removed = old_entries
            .keys()
            .filter(|h| !live_hashes.contains(h))
            .count();
        if removed == 0 {
            return Ok(0);
        }

        let tmp_data = self.dir.join("text_store.dat.tmp");
        let mut src_file = File::open(&self.data_path)?;
        let mut dst_file = BufWriter::new(
            OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp_data)?,
        );
        dst_file.write_all(STORE_DATA_MAGIC)?;
        let mut cursor = STORE_DATA_MAGIC.len() as u64;

        let mut new_entries = HashMap::new();
        let mut copy_buf = Vec::new();

        for (hash, entry) in old_entries {
            if !live_hashes.contains(&hash) {
                self.lru.lock().remove(hash);
                continue;
            }
            let mut new_chunks = Vec::with_capacity(entry.chunks.len());
            for c in &entry.chunks {
                copy_buf.resize(c.compressed_len as usize, 0);
                src_file.seek(SeekFrom::Start(c.offset))?;
                src_file.read_exact(&mut copy_buf)?;
                let new_off = cursor;
                dst_file.write_all(&copy_buf)?;
                cursor += copy_buf.len() as u64;
                new_chunks.push(ChunkDesc {
                    offset: new_off,
                    compressed_len: c.compressed_len,
                    raw_len: c.raw_len,
                    codec: c.codec,
                });
            }
            new_entries.insert(
                hash,
                StoredTextEntry {
                    content_hash: hash,
                    total_raw_len: entry.total_raw_len,
                    chunks: new_chunks,
                },
            );
        }

        dst_file.flush()?;
        drop(dst_file);
        drop(src_file);

        fs::rename(&tmp_data, &self.data_path)?;
        save_index_file(&self.idx_path, &new_entries)?;

        let mut st = self.state.write();
        st.entries = new_entries;
        st.data_len = cursor;
        st.dirty = false;
        Ok(removed)
    }

    pub fn clear_cache(&self) {
        self.lru.lock().clear();
    }

    pub fn stats(&self) -> TextStoreStats {
        let st = self.state.read();
        let lru = self.lru.lock();
        TextStoreStats {
            entry_count: st.entries.len(),
            disk_bytes: st.data_len,
            cache_bytes: lru.used_bytes,
            cache_cap_bytes: lru.cap_bytes,
        }
    }
}

fn init_data_file(data_path: &Path) -> io::Result<u64> {
    let mut f = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(data_path)?;
    f.write_all(STORE_DATA_MAGIC)?;
    f.flush()?;
    Ok(STORE_DATA_MAGIC.len() as u64)
}

#[derive(bitcode::Encode, bitcode::Decode)]
struct PersistedIndex {
    entries: Vec<StoredTextEntry>,
}

fn save_index_file(idx_path: &Path, entries: &HashMap<u64, StoredTextEntry>) -> io::Result<()> {
    let tmp = idx_path.with_extension("idx.tmp");
    let list = PersistedIndex {
        entries: entries.values().cloned().collect(),
    };
    let encoded = bitcode::encode(&list);
    {
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)?;
        f.write_all(STORE_IDX_MAGIC)?;
        f.write_all(&encoded)?;
        f.flush()?;
    }
    fs::rename(&tmp, idx_path)?;
    Ok(())
}

fn load_index_file(idx_path: &Path, data_len: u64) -> Option<HashMap<u64, StoredTextEntry>> {
    let bytes = fs::read(idx_path).ok()?;
    if bytes.len() < STORE_IDX_MAGIC.len() || &bytes[..8] != STORE_IDX_MAGIC {
        return None;
    }
    let decoded: PersistedIndex = bitcode::decode(&bytes[8..]).ok()?;
    let mut map = HashMap::with_capacity(decoded.entries.len());
    for entry in decoded.entries {
        let all_in_bounds = entry
            .chunks
            .iter()
            .all(|c| c.offset.saturating_add(c.compressed_len as u64) <= data_len);
        if all_in_bounds {
            map.insert(entry.content_hash, entry);
        }
    }
    Some(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_index::gram::content_hash;
    use tempfile::TempDir;

    #[test]
    fn text_store_lzfse_roundtrip_and_strict_lru_cap() {
        let tmp = TempDir::new().unwrap();
        // 设置极小 LRU 上限 256 字节以测试严格淘汰行为。
        let store = TextStore::open(tmp.path(), 256).unwrap();

        let doc1 = b"Repeat phrase for LZFSE compression ratio check. ".repeat(40);
        let doc2 = b"Second document exceeding 256 byte cache cap by itself!".repeat(5);
        let doc3 = vec![b'A'; 150];
        let doc4 = vec![b'B'; 150];

        let h1 = content_hash(&doc1);
        let h2 = content_hash(&doc2);
        let h3 = content_hash(&doc3);
        let h4 = content_hash(&doc4);

        store.put_text(h1, &doc1).unwrap();
        store.put_text(h2, &doc2).unwrap();
        store.put_text(h3, &doc3).unwrap();
        assert_eq!(store.stats().cache_bytes, 150);
        // 插入 150B doc4 将迫使 doc3 被 LRU 淘汰，总缓存严格维持在 150 <= 256。
        store.put_text(h4, &doc4).unwrap();
        store.flush().unwrap();

        // LRU 缓存体积严格不得超过设定上限 256 字节。
        let st = store.stats();
        assert_eq!(st.cache_bytes, 150);
        assert!(st.cache_bytes <= 256, "cache_bytes={}", st.cache_bytes);
        assert_eq!(st.entry_count, 4);
        // 数据文件经过 LZFSE 压缩后应显著小于原始重复文本体量。
        assert!(st.disk_bytes < (doc1.len() + doc2.len()) as u64);

        // 重启后从磁盘恢复依然可以准确解压还原全文。
        drop(store);
        let reopened = TextStore::open(tmp.path(), 4096).unwrap();
        assert_eq!(reopened.get_text(h1).unwrap().as_slice(), doc1.as_slice());
        assert_eq!(reopened.get_text(h2).unwrap().as_slice(), doc2.as_slice());
        assert_eq!(reopened.get_text(h3).unwrap().as_slice(), doc3.as_slice());
        assert_eq!(reopened.get_text(h4).unwrap().as_slice(), doc4.as_slice());

        // 仅保留 h3 触发垃圾回收后，磁盘条目缩减为 1。
        let mut live = HashSet::new();
        live.insert(h3);
        let purged = reopened.compact_keeping(&live).unwrap();
        assert_eq!(purged, 3);
        assert!(!reopened.contains(h1));
        assert!(reopened.contains(h3));
    }
}
