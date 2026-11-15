pub mod extract;
pub mod gram;
pub mod overlay;
pub mod posting;
pub mod scope;
pub mod segment;
pub mod text_store;
pub mod verify;

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use parking_lot::RwLock;
use rayon::prelude::*;

use extract::extract_office_xml_file;
use gram::{content_hash, decode_plain_text};
use overlay::{plan_sync_diff, DirtyOverlay, FileSnapshot, DEFAULT_DIRTY_FLUSH_DOCS};
use scope::{
    is_dataless, release_allocator_memory, ContentSourceKind, NoMaterializeGuard, ScopeConfig,
    ScopeDecision, ScopeEvaluator,
};
use segment::{SegmentDocInput, SegmentSet};
use text_store::{TextStore, DEFAULT_LRU_CAP_BYTES};
use verify::{
    collect_candidates, compute_doc_rank, verify_candidates, CompiledContentQuery, ContentLineMatch,
    ContentQueryMode, ContentQueryOptions, ContentSearchResponse,
};

/// 外部富文档文本抽取器钩子（例如低优先级隔离辅助进程提供的 PDFKit / Vision OCR 提取）。
pub trait RichTextExtractor: Send + Sync {
    fn extract_text(&self, path: &Path, kind: ContentSourceKind) -> Option<(String, u32)>;
}

/// 内容索引运行时状态汇总。
#[derive(Debug, Default, Clone)]
pub struct ContentIndexStats {
    pub live_docs: usize,
    pub unique_contents: usize,
    pub segment_count: usize,
    pub dirty_docs: usize,
    pub mapped_bytes: usize,
    pub text_store_entries: usize,
    pub text_store_disk_bytes: u64,
    pub lru_cache_bytes: usize,
    pub lru_cache_cap_bytes: usize,
    pub is_indexing: bool,
}

/// Lokii 混合内容检索子系统核心协调器：
/// 统筹磁盘 `mmap` 分段倒排、内存 `Dirty` 覆盖层、压缩 Tier B 正文仓、`.gitignore`/iCloud 守卫与并行预算校验。
pub struct ContentIndex {
    data_dir: PathBuf,
    segments: RwLock<SegmentSet>,
    overlay: RwLock<DirtyOverlay>,
    text_store: TextStore,
    scope_config: RwLock<ScopeConfig>,
    rich_extractor: RwLock<Option<Arc<dyn RichTextExtractor>>>,
    is_indexing: AtomicBool,
}

impl ContentIndex {
    pub fn open(
        data_dir: &Path,
        scope_config: ScopeConfig,
        max_lru_bytes: usize,
    ) -> io::Result<Self> {
        fs::create_dir_all(data_dir)?;
        let seg_dir = data_dir.join("segments");
        let text_dir = data_dir.join("text_store");

        let segments = SegmentSet::open_dir(&seg_dir)?;
        let text_store = TextStore::open(&text_dir, max_lru_bytes)?;

        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            segments: RwLock::new(segments),
            overlay: RwLock::new(DirtyOverlay::new(DEFAULT_DIRTY_FLUSH_DOCS)),
            text_store,
            scope_config: RwLock::new(scope_config),
            rich_extractor: RwLock::new(None),
            is_indexing: AtomicBool::new(false),
        })
    }

    pub fn open_default(data_dir: &Path) -> io::Result<Self> {
        Self::open(data_dir, ScopeConfig::default(), DEFAULT_LRU_CAP_BYTES)
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn set_rich_extractor(&self, extractor: Option<Arc<dyn RichTextExtractor>>) {
        *self.rich_extractor.write() = extractor;
    }

    pub fn set_scope_config(&self, config: ScopeConfig) {
        *self.scope_config.write() = config;
    }

    pub fn scope_config(&self) -> ScopeConfig {
        self.scope_config.read().clone()
    }

    pub fn clear_lru_cache(&self) {
        self.text_store.clear_cache();
    }

    /// 评估单个文件路径并抽取出适合写入段或覆盖层的文档输入。
    fn build_input_for_path(
        &self,
        path: &Path,
        eval: &mut ScopeEvaluator,
    ) -> Option<SegmentDocInput> {
        let _guard = NoMaterializeGuard::enter();
        let meta = fs::symlink_metadata(path).ok()?;
        let kind = match eval.evaluate_path_with_meta(path, &meta) {
            ScopeDecision::Include(k) => k,
            ScopeDecision::Exclude(_) => return None,
        };
        let mtime_secs = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let size_bytes = meta.len();
        let path_str = path.to_string_lossy().into_owned();
        let rank = compute_doc_rank(&path_str, kind);

        self.extract_and_prepare(&path_str, path, kind, mtime_secs, size_bytes, rank)
    }

    fn extract_and_prepare(
        &self,
        path_str: &str,
        path: &Path,
        kind: ContentSourceKind,
        mtime_secs: i64,
        size_bytes: u64,
        rank: u16,
    ) -> Option<SegmentDocInput> {
        match kind {
            ContentSourceKind::PlainText => {
                let meta = fs::symlink_metadata(path).ok()?;
                if is_dataless(&meta) || !meta.file_type().is_file() {
                    return None;
                }
                let raw = fs::read(path).ok()?;
                let decoded = decode_plain_text(&raw)?;
                Some(SegmentDocInput::from_decoded_text(
                    path_str.to_string(),
                    mtime_secs,
                    size_bytes,
                    ContentSourceKind::PlainText,
                    rank,
                    0,
                    decoded.as_ref(),
                    true,
                ))
            }
            ContentSourceKind::OfficeXml => {
                let text = extract_office_xml_file(path).ok()?;
                if text.is_empty() {
                    return None;
                }
                let bytes = text.as_bytes();
                let hash = content_hash(bytes);
                let _ = self.text_store.put_text(hash, bytes);
                Some(SegmentDocInput::from_decoded_text(
                    path_str.to_string(),
                    mtime_secs,
                    size_bytes,
                    ContentSourceKind::OfficeXml,
                    rank,
                    0,
                    bytes,
                    false,
                ))
            }
            ContentSourceKind::Pdf | ContentSourceKind::Image => {
                let extractor = self.rich_extractor.read().clone()?;
                let (text, page_count) = extractor.extract_text(path, kind)?;
                if text.is_empty() {
                    return None;
                }
                let bytes = text.as_bytes();
                let hash = content_hash(bytes);
                let _ = self.text_store.put_text(hash, bytes);
                Some(SegmentDocInput::from_decoded_text(
                    path_str.to_string(),
                    mtime_secs,
                    size_bytes,
                    kind,
                    rank,
                    page_count,
                    bytes,
                    false,
                ))
            }
        }
    }

    /// 即时索引或更新单个文件至 `Dirty` 覆盖层，并在达到阈值时自动下刷为 Tier-0 段。
    pub fn upsert_file(&self, path: &Path) -> bool {
        let cfg = self.scope_config();
        let mut eval = ScopeEvaluator::new(cfg);
        let Some(input) = self.build_input_for_path(path, &mut eval) else {
            return false;
        };
        let should_flush = {
            let mut segs = self.segments.write();
            let mut ov = self.overlay.write();
            ov.upsert_doc(input, Some(&mut segs));
            ov.needs_flush()
        };
        if should_flush {
            let _ = self.flush();
        }
        true
    }

    /// 供隔离辅助进程/外部 OCR 结果直接注入已完成提取的 PDF 或图片纯文本正文。
    pub fn ingest_rich_text(
        &self,
        path: &Path,
        kind: ContentSourceKind,
        mtime_secs: i64,
        size_bytes: u64,
        decoded_utf8: &[u8],
        aux_meta: u32,
    ) -> io::Result<()> {
        let path_str = path.to_string_lossy().into_owned();
        let hash = content_hash(decoded_utf8);
        self.text_store.put_text(hash, decoded_utf8)?;
        let rank = compute_doc_rank(&path_str, kind);
        let input = SegmentDocInput::from_decoded_text(
            path_str,
            mtime_secs,
            size_bytes,
            kind,
            rank,
            aux_meta,
            decoded_utf8,
            false,
        );
        let should_flush = {
            let mut segs = self.segments.write();
            let mut ov = self.overlay.write();
            ov.upsert_doc(input, Some(&mut segs));
            ov.needs_flush()
        };
        if should_flush {
            let _ = self.flush();
        }
        Ok(())
    }

    /// 接收 FSEvents 增量批次：先淘汰删除与改名旧路径，再并行读取并写入新增/变更文档至 `Dirty` 覆盖层。
    pub fn apply_watcher_delta(
        &self,
        created_or_modified: &[PathBuf],
        removed: &[String],
        renames: &[(String, String)],
    ) -> usize {
        {
            let mut segs = self.segments.write();
            let mut ov = self.overlay.write();
            for p in removed {
                ov.mark_deleted(p, Some(&mut segs));
            }
            for (old_p, _) in renames {
                ov.mark_deleted(old_p, Some(&mut segs));
            }
        }

        let mut pending_paths: Vec<PathBuf> = created_or_modified.to_vec();
        for (_, new_p) in renames {
            pending_paths.push(PathBuf::from(new_p));
        }
        if pending_paths.is_empty() {
            return 0;
        }

        let cfg = self.scope_config();
        let inputs: Vec<SegmentDocInput> = {
            let mut eval = ScopeEvaluator::new(cfg);
            let accepted: Vec<(PathBuf, ContentSourceKind, i64, u64, u16)> = pending_paths
                .into_iter()
                .filter_map(|p| {
                    let meta = fs::symlink_metadata(&p).ok()?;
                    let kind = match eval.evaluate_path_with_meta(&p, &meta) {
                        ScopeDecision::Include(k) => k,
                        ScopeDecision::Exclude(_) => return None,
                    };
                    let mtime = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    let size = meta.len();
                    let rank = compute_doc_rank(&p.to_string_lossy(), kind);
                    Some((p, kind, mtime, size, rank))
                })
                .collect();

            accepted
                .into_par_iter()
                .filter_map(|(p, kind, mtime, size, rank)| {
                    let _guard = NoMaterializeGuard::enter();
                    let pstr = p.to_string_lossy().into_owned();
                    self.extract_and_prepare(&pstr, &p, kind, mtime, size, rank)
                })
                .collect()
        };

        let added = inputs.len();
        if added > 0 {
            let should_flush = {
                let mut segs = self.segments.write();
                let mut ov = self.overlay.write();
                for input in inputs {
                    ov.upsert_doc(input, Some(&mut segs));
                }
                ov.needs_flush()
            };
            if should_flush {
                let _ = self.flush();
            }
        }
        added
    }

    /// 基于全盘/作用域候选路径清单与 `(mtime, size, kind)` 执行无重扫差异全量/增量对账。
    pub fn sync_from_snapshots<I>(&self, paths: I) -> io::Result<usize>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        self.is_indexing.store(true, Ordering::SeqCst);
        let res = self.sync_inner(paths);
        self.is_indexing.store(false, Ordering::SeqCst);
        res
    }

    fn sync_inner<I>(&self, paths: I) -> io::Result<usize>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let cfg = self.scope_config();
        let mut eval = ScopeEvaluator::new(cfg);
        let mut snapshots = Vec::new();
        for p in paths {
            let Ok(meta) = fs::symlink_metadata(&p) else {
                continue;
            };
            let ScopeDecision::Include(kind) = eval.evaluate_path_with_meta(&p, &meta) else {
                continue;
            };
            let mtime_secs = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let pstr = p.to_string_lossy().into_owned();
            let doc_rank = compute_doc_rank(&pstr, kind);
            snapshots.push(FileSnapshot {
                path: pstr,
                mtime_secs,
                size_bytes: meta.len(),
                source_kind: kind,
                doc_rank,
            });
        }

        let diff = {
            let segs = self.segments.read();
            let ov = self.overlay.read();
            plan_sync_diff(&snapshots, &segs, &ov)
        };

        if !diff.to_remove.is_empty() {
            let mut segs = self.segments.write();
            let mut ov = self.overlay.write();
            for p in &diff.to_remove {
                ov.mark_deleted(p, Some(&mut segs));
            }
        }

        let indexed_count = if !diff.to_index.is_empty() {
            let prepared: Vec<SegmentDocInput> = diff
                .to_index
                .into_par_iter()
                .filter_map(|snap| {
                    let _guard = NoMaterializeGuard::enter();
                    self.extract_and_prepare(
                        &snap.path,
                        Path::new(&snap.path),
                        snap.source_kind,
                        snap.mtime_secs,
                        snap.size_bytes,
                        snap.doc_rank,
                    )
                })
                .collect();

            let count = prepared.len();
            if count > 0 {
                let mut segs = self.segments.write();
                let mut ov = self.overlay.write();
                for item in &prepared {
                    ov.mark_deleted(&item.path, Some(&mut segs));
                }
                if count >= DEFAULT_DIRTY_FLUSH_DOCS {
                    segs.add_tier0(prepared)?;
                } else {
                    for item in prepared {
                        ov.upsert_doc(item, Some(&mut segs));
                    }
                }
            }
            count
        } else {
            0
        };

        self.flush()?;
        let _ = release_allocator_memory();
        Ok(indexed_count)
    }

    /// 刷盘 `Dirty` 覆盖层至段、持久化墓碑与富文档索引，执行 Tier-of-8 归并并回收不再被引用的正文仓垃圾。
    pub fn flush(&self) -> io::Result<usize> {
        let flushed = {
            let mut segs = self.segments.write();
            let mut ov = self.overlay.write();
            ov.flush_to_segments(&mut segs)?
        };

        // 收集所有存活富文档 `content_hash` 以定期回收已删除 PDF/Office 的历史正文。
        let live_rich_hashes = {
            let segs = self.segments.read();
            let ov = self.overlay.read();
            let mut hashes = HashSet::new();
            for (seg, dead) in &segs.segments {
                for doc_id in 0..seg.ndocs() {
                    if dead.is_dead(doc_id) {
                        continue;
                    }
                    if let Some(meta) = seg.doc_meta(doc_id) {
                        if meta.source_kind != ContentSourceKind::PlainText {
                            if let Some(cm) = seg.content_meta(meta.content_id) {
                                hashes.insert(cm.content_hash);
                            }
                        }
                    }
                }
            }
            for d in ov.live_docs() {
                if d.input.source_kind != ContentSourceKind::PlainText {
                    hashes.insert(d.input.content_hash);
                }
            }
            hashes
        };

        let _ = self.text_store.flush();
        if self.text_store.stats().entry_count > live_rich_hashes.len() + 32 {
            let _ = self.text_store.compact_keeping(&live_rich_hashes);
        }
        let _ = release_allocator_memory();
        Ok(flushed)
    }

    /// 执行两段锁分离的并行全文检索：短暂持读锁完成倒排筛选与候选排序，随后彻底释放锁并发读取磁盘验证。
    pub fn search<F>(
        &self,
        mode: ContentQueryMode,
        path_filter: F,
        options: &ContentQueryOptions,
        cancel: &AtomicBool,
    ) -> Result<ContentSearchResponse, String>
    where
        F: FnMut(&str) -> bool,
    {
        let query = CompiledContentQuery::compile(mode)?;
        if cancel.load(Ordering::Relaxed) {
            return Ok(ContentSearchResponse {
                cancelled: true,
                ..Default::default()
            });
        }

        let candidates = {
            let segs = self.segments.read();
            let ov = self.overlay.read();
            collect_candidates(&query, &segs, &ov, path_filter, options.max_candidates)
        };

        Ok(verify_candidates(
            &query,
            &candidates,
            Some(&self.text_store),
            options,
            cancel,
        ))
    }

    /// 针对单个高亮查看路径按需提取完整行命中明细（兼容纯文本与 Tier B 压缩富文档）。
    pub fn get_file_snippets(
        &self,
        path: &str,
        mode: ContentQueryMode,
        max_matches: usize,
    ) -> Result<Vec<ContentLineMatch>, String> {
        let _guard = NoMaterializeGuard::enter();
        let query = CompiledContentQuery::compile(mode)?;
        let kind_and_hash = {
            let ov = self.overlay.read();
            let mut found = None;
            for d in ov.live_docs() {
                if d.input.path == path {
                    found = Some((d.input.source_kind, d.input.content_hash));
                    break;
                }
            }
            if found.is_none() && !ov.is_path_superseded(path) {
                let segs = self.segments.read();
                for (seg, dead) in &segs.segments {
                    if let Some(doc_id) = seg.find_doc_by_path(path) {
                        if !dead.is_dead(doc_id) {
                            if let Some(dm) = seg.doc_meta(doc_id) {
                                if let Some(cm) = seg.content_meta(dm.content_id) {
                                    found = Some((dm.source_kind, cm.content_hash));
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            found
        };

        let (kind, chash) = kind_and_hash.unwrap_or((ContentSourceKind::PlainText, 0));
        match kind {
            ContentSourceKind::PlainText => {
                let p = Path::new(path);
                let meta = fs::symlink_metadata(p).map_err(|e| e.to_string())?;
                if is_dataless(&meta) {
                    return Ok(Vec::new());
                }
                let raw = fs::read(p).map_err(|e| e.to_string())?;
                let Some(decoded) = decode_plain_text(&raw) else {
                    return Ok(Vec::new());
                };
                Ok(query.verify_bytes(decoded.as_ref(), max_matches).0)
            }
            ContentSourceKind::OfficeXml | ContentSourceKind::Pdf | ContentSourceKind::Image => {
                let Some(text_arc) = self.text_store.get_text(chash) else {
                    return Ok(Vec::new());
                };
                Ok(query.verify_bytes(text_arc.as_slice(), max_matches).0)
            }
        }
    }

    /// 筛选尚未建立内容索引（或已过期）的文件路径，供后台外部 PDF/OCR 辅助进程分批提取。
    pub fn filter_stale_paths(&self, candidates: &[(String, i64, u64)], limit: usize) -> Vec<String> {
        let segs = self.segments.read();
        let ov = self.overlay.read();
        let mut out = Vec::new();
        for (path, mtime, size) in candidates {
            if out.len() >= limit {
                break;
            }
            if let Some((om, os, _)) = ov.find_live_meta(path) {
                if om == *mtime && os == *size {
                    continue;
                }
            } else if !ov.is_path_superseded(path) {
                let mut matched = false;
                for (seg, dead) in &segs.segments {
                    if let Some(doc_id) = seg.find_doc_by_path(path) {
                        if !dead.is_dead(doc_id) {
                            if let Some(dm) = seg.doc_meta(doc_id) {
                                if dm.mtime_secs == *mtime && dm.size_bytes == *size {
                                    matched = true;
                                }
                            }
                        }
                        break;
                    }
                }
                if matched {
                    continue;
                }
            }
            out.push(path.clone());
        }
        out
    }

    pub fn stats(&self) -> ContentIndexStats {
        let segs = self.segments.read();
        let ov = self.overlay.read();
        let ts = self.text_store.stats();
        ContentIndexStats {
            live_docs: segs.total_live_docs() + ov.live_count(),
            unique_contents: segs.total_unique_contents(),
            segment_count: segs.segments.len(),
            dirty_docs: ov.live_count(),
            mapped_bytes: segs.total_mapped_bytes(),
            text_store_entries: ts.entry_count,
            text_store_disk_bytes: ts.disk_bytes,
            lru_cache_bytes: ts.cache_bytes,
            lru_cache_cap_bytes: ts.cache_cap_bytes,
            is_indexing: self.is_indexing.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use extract::deflate_raw;
    use std::io::Write;
    use tempfile::TempDir;

    fn build_minimal_docx(text: &str) -> Vec<u8> {
        let xml = format!(
            r#"<w:document><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"#
        );
        let raw = xml.as_bytes();
        let name = b"word/document.xml";
        #[cfg(target_os = "macos")]
        let (method, payload) = (8u16, deflate_raw(raw).unwrap());
        #[cfg(not(target_os = "macos"))]
        let (method, payload) = (0u16, raw.to_vec());

        let mut out = Vec::new();
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&payload);

        let cd_off = out.len() as u32;
        let mut cd = Vec::new();
        cd.extend_from_slice(b"PK\x01\x02");
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&method.to_le_bytes());
        cd.extend_from_slice(&0u32.to_le_bytes());
        cd.extend_from_slice(&0u32.to_le_bytes());
        cd.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u32.to_le_bytes());
        cd.extend_from_slice(&0u32.to_le_bytes());
        cd.extend_from_slice(name);
        out.extend_from_slice(&cd);

        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn end_to_end_sync_watcher_office_xml_and_ingested_pdf() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("idx_data");
        let work_dir = tmp.path().join("workspace");
        fs::create_dir_all(&work_dir).unwrap();

        let rust_file = work_dir.join("lib.rs");
        fs::write(&rust_file, b"pub fn compute_matrix() -> usize { 42 }\n").unwrap();

        let docx_file = work_dir.join("report.docx");
        {
            let mut f = fs::File::create(&docx_file).unwrap();
            f.write_all(&build_minimal_docx("Annual Revenuecompute_matrix Blueprint")).unwrap();
        }

        let pdf_file = work_dir.join("spec.pdf");
        fs::write(&pdf_file, b"%PDF-1.4\n").unwrap();

        let idx = ContentIndex::open(
            &data_dir,
            ScopeConfig {
                global_gitignore_path: None,
                ..Default::default()
            },
            4 * 1024 * 1024,
        )
        .unwrap();

        let n = idx
            .sync_from_snapshots([rust_file.clone(), docx_file.clone()])
            .unwrap();
        assert_eq!(n, 2);

        // 外部辅助进程抽取的 PDF 正文进入 LZFSE 压缩仓 + 分段倒排
        idx.ingest_rich_text(
            &pdf_file,
            ContentSourceKind::Pdf,
            300,
            9,
            b"Architecture Spec\nContains compute_matrix algorithm page 3",
            3,
        )
        .unwrap();

        let cancel = AtomicBool::new(false);
        let resp = idx
            .search(
                ContentQueryMode::Literal {
                    pattern: "compute_matrix".to_string(),
                    case_insensitive: true,
                    symbol_only: false,
                },
                |_| true,
                &ContentQueryOptions::default(),
                &cancel,
            )
            .unwrap();
        assert_eq!(resp.hits.len(), 3);

        // FSEvents 增量更新：修改 rust_file 并新增 rename_dest.rs
        fs::write(&rust_file, b"pub fn renamed_matrix() {}\n").unwrap();
        let moved_file = work_dir.join("moved.rs");
        fs::write(&moved_file, b"pub fn compute_matrix() {}\n").unwrap();
        idx.apply_watcher_delta(
            &[rust_file.clone()],
            &[],
            &[(
                docx_file.to_string_lossy().into_owned(),
                moved_file.to_string_lossy().into_owned(),
            )],
        );

        let after = idx
            .search(
                ContentQueryMode::Literal {
                    pattern: "compute_matrix".to_string(),
                    case_insensitive: true,
                    symbol_only: true,
                },
                |_| true,
                &ContentQueryOptions::default(),
                &cancel,
            )
            .unwrap();
        assert_eq!(after.hits.len(), 1);
        assert_eq!(after.hits[0].path, moved_file.to_string_lossy());

        // 片段按需提取可用于 PDF 与 PlainText
        let pdf_snips = idx
            .get_file_snippets(
                &pdf_file.to_string_lossy(),
                ContentQueryMode::Literal {
                    pattern: "algorithm".to_string(),
                    case_insensitive: true,
                    symbol_only: false,
                },
                5,
            )
            .unwrap();
        assert_eq!(pdf_snips.len(), 1);
        assert_eq!(pdf_snips[0].line_number, 2);
    }
}
