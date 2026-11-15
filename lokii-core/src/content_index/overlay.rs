use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::ops::Range;

use super::gram::bloom_contains_all;
use super::scope::ContentSourceKind;
use super::segment::{SegmentDocInput, SegmentSet};

/// 触发 `Dirty` 覆盖层自动刷盘生成 Tier-0 段的文档数阈值。
pub const DEFAULT_DIRTY_FLUSH_DOCS: usize = 256;

/// 内存增量覆盖层中的活跃变更文档。
#[derive(Debug, Clone)]
pub struct DirtyDoc {
    pub doc_id: u32,
    pub input: SegmentDocInput,
    pub dead: bool,
}

/// 内存增量覆盖层：接收 FSEvents 创建/修改/删除批次后即时生效，无需等待异步归并写段即可检索最新文件。
pub struct DirtyOverlay {
    docs: Vec<DirtyDoc>,
    path_to_id: HashMap<String, u32>,
    superseded_paths: HashSet<String>,
    inverted: BTreeMap<u32, Vec<u32>>,
    live_count: usize,
    flush_threshold: usize,
}

impl Default for DirtyOverlay {
    fn default() -> Self {
        Self::new(DEFAULT_DIRTY_FLUSH_DOCS)
    }
}

impl DirtyOverlay {
    pub fn new(flush_threshold: usize) -> Self {
        Self {
            docs: Vec::new(),
            path_to_id: HashMap::new(),
            superseded_paths: HashSet::new(),
            inverted: BTreeMap::new(),
            live_count: 0,
            flush_threshold: flush_threshold.max(1),
        }
    }

    #[inline]
    pub fn live_count(&self) -> usize {
        self.live_count
    }

    #[inline]
    pub fn total_slots(&self) -> usize {
        self.docs.len()
    }

    #[inline]
    pub fn needs_flush(&self) -> bool {
        self.live_count >= self.flush_threshold
    }

    /// 检查给定路径是否在覆盖层中存在更新版本或已被删除（查询端可据此屏蔽旧段记录）。
    #[inline]
    pub fn is_path_superseded(&self, path: &str) -> bool {
        self.superseded_paths.contains(path)
    }

    /// 标记路径删除；若之前在 `Dirty` 中存在活跃条目一并置死，并同步标记磁盘段墓碑。
    pub fn mark_deleted(&mut self, path: &str, segments: Option<&mut SegmentSet>) {
        if let Some(&old_id) = self.path_to_id.get(path) {
            if let Some(slot) = self.docs.get_mut(old_id as usize) {
                if !slot.dead {
                    slot.dead = true;
                    self.live_count = self.live_count.saturating_sub(1);
                }
            }
            self.path_to_id.remove(path);
        }
        self.superseded_paths.insert(path.to_string());
        if let Some(seg_set) = segments {
            seg_set.mark_path_dead(path);
        }
    }

    /// 写入或更新一篇文档；旧版本（包括覆盖层内与底层段内）立即失效。
    pub fn upsert_doc(&mut self, input: SegmentDocInput, segments: Option<&mut SegmentSet>) {
        let path = input.path.clone();
        if let Some(&old_id) = self.path_to_id.get(&path) {
            if let Some(slot) = self.docs.get_mut(old_id as usize) {
                if !slot.dead {
                    slot.dead = true;
                    self.live_count = self.live_count.saturating_sub(1);
                }
            }
        }
        self.superseded_paths.insert(path.clone());
        if let Some(seg_set) = segments {
            seg_set.mark_path_dead(&path);
        }

        let new_id = self.docs.len() as u32;
        for &k in &input.keys {
            self.inverted.entry(k).or_default().push(new_id);
        }
        self.path_to_id.insert(path, new_id);
        self.docs.push(DirtyDoc {
            doc_id: new_id,
            input,
            dead: false,
        });
        self.live_count += 1;
    }

    pub fn doc(&self, dirty_doc_id: u32) -> Option<&DirtyDoc> {
        self.docs
            .get(dirty_doc_id as usize)
            .filter(|d| !d.dead)
    }

    pub fn live_docs(&self) -> impl Iterator<Item = &DirtyDoc> {
        self.docs.iter().filter(|d| !d.dead)
    }

    pub fn find_live_meta(&self, path: &str) -> Option<(i64, u64, ContentSourceKind)> {
        let &id = self.path_to_id.get(path)?;
        let d = self.doc(id)?;
        Some((d.input.mtime_secs, d.input.size_bytes, d.input.source_kind))
    }

    /// 精确键检索，返回仍然存活的有序 `dirty_doc_id` 列表。
    pub fn lookup_key(&self, key: u32) -> Vec<u32> {
        let Some(raw) = self.inverted.get(&key) else {
            return Vec::new();
        };
        let mut out: Vec<u32> = raw
            .iter()
            .copied()
            .filter(|&id| self.docs.get(id as usize).is_some_and(|d| !d.dead))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// 键区间检索（用于文末 2 字节哨兵扫描）。
    pub fn lookup_key_range(&self, range: Range<u32>) -> Vec<u32> {
        if range.start >= range.end {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (_, ids) in self.inverted.range(range) {
            for &id in ids {
                if self.docs.get(id as usize).is_some_and(|d| !d.dead) {
                    out.push(id);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn bloom_contains_all(&self, dirty_doc_id: u32, short_hashes: &[u32]) -> bool {
        if short_hashes.is_empty() {
            return true;
        }
        let Some(doc) = self.doc(dirty_doc_id) else {
            return false;
        };
        bloom_contains_all(&doc.input.bloom, short_hashes)
    }

    /// 将当前覆盖层中的活跃变更批写入新的 Tier-0 段，持久化旧段墓碑并触发分级归并，随后清空覆盖层。
    pub fn flush_to_segments(&mut self, segments: &mut SegmentSet) -> io::Result<usize> {
        for sup_path in &self.superseded_paths {
            segments.mark_path_dead(sup_path);
        }

        let inputs: Vec<SegmentDocInput> = self
            .docs
            .iter()
            .filter(|d| !d.dead)
            .map(|d| d.input.clone())
            .collect();
        let flushed = inputs.len();

        if !inputs.is_empty() {
            segments.add_tier0(inputs)?;
        }
        segments.flush_dead_bitmaps()?;
        let _ = segments.compact_if_needed()?;

        self.docs.clear();
        self.path_to_id.clear();
        self.superseded_paths.clear();
        self.inverted.clear();
        self.live_count = 0;

        Ok(flushed)
    }
}

/// 候选文件元数据快照（由 `CompactIndex` 或目录扫描供给内容索引做差异对齐）。
#[derive(Debug, Clone)]
pub struct FileSnapshot {
    pub path: String,
    pub mtime_secs: i64,
    pub size_bytes: u64,
    pub source_kind: ContentSourceKind,
    pub doc_rank: u16,
}

#[derive(Debug, Default)]
pub struct SyncDiff {
    pub to_index: Vec<FileSnapshot>,
    pub to_remove: Vec<String>,
    pub unchanged: usize,
}

/// 基于 `(mtime_secs, size_bytes, source_kind)` 对比现存索引与最新文件清单，只重读发生变更的文件。
pub fn plan_sync_diff(
    current_files: &[FileSnapshot],
    segments: &SegmentSet,
    overlay: &DirtyOverlay,
) -> SyncDiff {
    let mut existing: HashMap<String, (i64, u64, ContentSourceKind)> = HashMap::new();

    for (seg, dead) in &segments.segments {
        for doc_id in 0..seg.ndocs() {
            if dead.is_dead(doc_id) {
                continue;
            }
            let Some(path) = seg.doc_path(doc_id) else {
                continue;
            };
            if overlay.is_path_superseded(&path) {
                continue;
            }
            if let Some(meta) = seg.doc_meta(doc_id) {
                existing.insert(path, (meta.mtime_secs, meta.size_bytes, meta.source_kind));
            }
        }
    }

    for dirty in overlay.live_docs() {
        existing.insert(
            dirty.input.path.clone(),
            (
                dirty.input.mtime_secs,
                dirty.input.size_bytes,
                dirty.input.source_kind,
            ),
        );
    }

    let mut diff = SyncDiff::default();
    let mut seen: HashSet<&str> = HashSet::with_capacity(current_files.len());

    for f in current_files {
        seen.insert(f.path.as_str());
        match existing.get(&f.path) {
            Some(&(mtime, size, kind))
                if mtime == f.mtime_secs && size == f.size_bytes && kind == f.source_kind =>
            {
                diff.unchanged += 1;
            }
            _ => {
                diff.to_index.push(f.clone());
            }
        }
    }

    for path in existing.keys() {
        if !seen.contains(path.as_str()) {
            diff.to_remove.push(path.clone());
        }
    }

    diff
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_index::gram::{fold_ascii_slice, trigrams_folded};
    use tempfile::TempDir;

    fn make_input(path: &str, body: &[u8], mtime: i64) -> SegmentDocInput {
        SegmentDocInput::from_decoded_text(
            path.to_string(),
            mtime,
            body.len() as u64,
            ContentSourceKind::PlainText,
            400,
            0,
            body,
            true,
        )
    }

    #[test]
    fn dirty_overlay_supersedes_segment_updates_and_deletes_immediately() {
        let tmp = TempDir::new().unwrap();
        let mut segments = SegmentSet::open_dir(tmp.path()).unwrap();
        segments
            .add_tier0(vec![
                make_input("/proj/a.rs", b"fn old_alpha_token() {}\n", 10),
                make_input("/proj/b.rs", b"fn keep_bravo_token() {}\n", 10),
            ])
            .unwrap();

        let mut overlay = DirtyOverlay::default();

        // 修改 a.rs 与删除 b.rs 后，在尚未刷盘写段时即应在覆盖层与墓碑层实时生效。
        overlay.upsert_doc(
            make_input("/proj/a.rs", b"fn fresh_omega_token() {}\n", 20),
            Some(&mut segments),
        );
        overlay.mark_deleted("/proj/b.rs", Some(&mut segments));

        assert!(overlay.is_path_superseded("/proj/a.rs"));
        assert!(overlay.is_path_superseded("/proj/b.rs"));
        assert_eq!(segments.total_live_docs(), 0);
        assert_eq!(overlay.live_count(), 1);

        let omega_key = trigrams_folded(&fold_ascii_slice(b"fresh_omega_token"))[0];
        let hits = overlay.lookup_key(omega_key);
        assert_eq!(hits.len(), 1);
        assert_eq!(
            overlay.doc(hits[0]).unwrap().input.path.as_str(),
            "/proj/a.rs"
        );

        // 刷盘后覆盖层清空，新段仅存最新存活的 a.rs。
        let flushed = overlay.flush_to_segments(&mut segments).unwrap();
        assert_eq!(flushed, 1);
        assert_eq!(overlay.live_count(), 0);
        assert_eq!(segments.total_live_docs(), 1);
        assert_eq!(segments.segments.len(), 1);
        let seg = &segments.segments[0].0;
        assert!(seg.find_doc_by_path("/proj/a.rs").is_some());
        assert!(seg.find_doc_by_path("/proj/b.rs").is_none());
    }

    #[test]
    fn plan_sync_diff_detects_new_modified_deleted_and_unchanged() {
        let tmp = TempDir::new().unwrap();
        let mut segments = SegmentSet::open_dir(tmp.path()).unwrap();
        segments
            .add_tier0(vec![
                make_input("/p/unchanged.rs", b"fn a() {}\n", 100),
                make_input("/p/modified.rs", b"fn old() {}\n", 100),
                make_input("/p/deleted.rs", b"fn gone() {}\n", 100),
            ])
            .unwrap();
        let mut overlay = DirtyOverlay::default();
        overlay.upsert_doc(
            make_input("/p/dirty_same.rs", b"fn d() {}\n", 500),
            Some(&mut segments),
        );

        let snapshots = vec![
            FileSnapshot {
                path: "/p/unchanged.rs".to_string(),
                mtime_secs: 100,
                size_bytes: 10,
                source_kind: ContentSourceKind::PlainText,
                doc_rank: 400,
            },
            FileSnapshot {
                path: "/p/modified.rs".to_string(),
                mtime_secs: 200,
                size_bytes: 12,
                source_kind: ContentSourceKind::PlainText,
                doc_rank: 400,
            },
            FileSnapshot {
                path: "/p/added.rs".to_string(),
                mtime_secs: 300,
                size_bytes: 15,
                source_kind: ContentSourceKind::PlainText,
                doc_rank: 400,
            },
            FileSnapshot {
                path: "/p/dirty_same.rs".to_string(),
                mtime_secs: 500,
                size_bytes: 10,
                source_kind: ContentSourceKind::PlainText,
                doc_rank: 400,
            },
        ];

        let diff = plan_sync_diff(&snapshots, &segments, &overlay);
        assert_eq!(diff.unchanged, 2);
        assert_eq!(diff.to_remove, vec!["/p/deleted.rs".to_string()]);
        let to_index_paths: Vec<&str> = diff.to_index.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(to_index_paths, vec!["/p/modified.rs", "/p/added.rs"]);
    }
}
