use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use memchr::memmem;
use rayon::prelude::*;
use regex::bytes::RegexBuilder;
use regex_syntax::hir::{Class, Hir, HirKind};
use regex_syntax::ParserBuilder;

use super::gram::{
    decode_plain_text, fold_ascii, fold_ascii_slice, is_symbol_defined_at, literal_gram_plan,
    query_short_hashes, GramPlan,
};
use super::overlay::DirtyOverlay;
use super::scope::{is_dataless, ContentSourceKind, NoMaterializeGuard};
use super::segment::{DictEntry, Segment, SegmentSet};
use super::text_store::TextStore;

/// 单行展示最大字节窗口（超过时围绕首个命中截断并对齐 UTF-8 字符边界）。
const MAX_LINE_SNIPPET_BYTES: usize = 240;

/// 实盘校验并行分批大小（每批结束后检查取消令牌与时间预算）。
const VERIFY_BATCH_SIZE: usize = 64;

/// 内容检索语法模式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentQueryMode {
    Literal {
        pattern: String,
        case_insensitive: bool,
        symbol_only: bool,
    },
    Regex {
        pattern: String,
        case_insensitive: bool,
    },
}

/// 内容检索运行选项。
#[derive(Debug, Clone)]
pub struct ContentQueryOptions {
    pub max_results: usize,
    pub max_matches_per_file: usize,
    pub budget_ms: u64,
    pub max_candidates: usize,
    pub min_size_bytes: Option<u64>,
    pub max_size_bytes: Option<u64>,
}

impl Default for ContentQueryOptions {
    fn default() -> Self {
        Self {
            max_results: 200,
            max_matches_per_file: 5,
            budget_ms: 250,
            max_candidates: 25_000,
            min_size_bytes: None,
            max_size_bytes: None,
        }
    }
}

/// 相对于 `ContentLineMatch.line_text` 的 UTF-8 字节偏移高亮区间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentMatchRange {
    pub start: u32,
    pub end: u32,
}

/// 单行命中摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentLineMatch {
    pub line_number: u32,
    pub column: u32,
    pub line_text: String,
    pub match_ranges: Vec<ContentMatchRange>,
    pub is_symbol_def: bool,
}

/// 单文件命中详情。
#[derive(Debug, Clone)]
pub struct ContentSearchHit {
    pub path: String,
    pub source_kind: ContentSourceKind,
    pub score: i32,
    pub line_matches: Vec<ContentLineMatch>,
    pub total_matches_in_file: u32,
    pub size_bytes: u64,
    pub modified_secs: i64,
}

/// 内容检索完整返回结构。
#[derive(Debug, Default)]
pub struct ContentSearchResponse {
    pub hits: Vec<ContentSearchHit>,
    pub total_candidates_matched: usize,
    pub candidates_verified: usize,
    pub truncated_by_budget: bool,
    pub cancelled: bool,
    pub elapsed_ms: u64,
}

/// 候选调度描述符（锁外实盘验证使用，不持有任何索引锁）。
#[derive(Debug, Clone)]
pub struct CandidateDoc {
    pub path: String,
    pub source_kind: ContentSourceKind,
    pub content_hash: u64,
    pub doc_rank: u16,
    pub modified_secs: i64,
    pub size_bytes: u64,
    pub from_dirty: bool,
}

/// 编译后的检索执行计划（包含倒排计划、Bloom 校验哈希与实盘精确匹配器）。
pub struct CompiledContentQuery {
    pub mode: ContentQueryMode,
    pub gram_plan: GramPlan,
    pub short_hashes: Vec<u32>,
    matcher: VerifiedMatcher,
}

enum VerifiedMatcher {
    LiteralExact {
        needle: Vec<u8>,
        symbol_only: bool,
    },
    LiteralAsciiFolded {
        folded: Vec<u8>,
        rare_idx: usize,
        rare_b1: u8,
        rare_b2: u8,
        symbol_only: bool,
    },
    Regex(regex::bytes::Regex),
}

impl CompiledContentQuery {
    pub fn compile(mode: ContentQueryMode) -> Result<Self, String> {
        match &mode {
            ContentQueryMode::Literal {
                pattern,
                case_insensitive,
                symbol_only,
            } => {
                if pattern.is_empty() {
                    return Err("empty literal pattern".to_string());
                }
                let gram_plan = literal_gram_plan(pattern.as_bytes(), *symbol_only);
                let short_hashes = query_short_hashes(pattern.as_bytes());
                let matcher = if !*case_insensitive {
                    VerifiedMatcher::LiteralExact {
                        needle: pattern.as_bytes().to_vec(),
                        symbol_only: *symbol_only,
                    }
                } else if pattern.is_ascii() {
                    let folded = fold_ascii_slice(pattern.as_bytes());
                    let rare_idx = pick_rarest_byte_idx(&folded);
                    let rb = folded[rare_idx];
                    let rb_upper = if rb.is_ascii_lowercase() {
                        rb.to_ascii_uppercase()
                    } else {
                        rb
                    };
                    VerifiedMatcher::LiteralAsciiFolded {
                        folded,
                        rare_idx,
                        rare_b1: rb,
                        rare_b2: rb_upper,
                        symbol_only: *symbol_only,
                    }
                } else {
                    let re = RegexBuilder::new(&regex::escape(pattern))
                        .case_insensitive(true)
                        .build()
                        .map_err(|e| e.to_string())?;
                    VerifiedMatcher::Regex(re)
                };
                Ok(Self {
                    mode,
                    gram_plan,
                    short_hashes,
                    matcher,
                })
            }
            ContentQueryMode::Regex {
                pattern,
                case_insensitive,
            } => {
                if pattern.is_empty() {
                    return Err("empty regex pattern".to_string());
                }
                let hir = ParserBuilder::new()
                    .case_insensitive(*case_insensitive)
                    .build()
                    .parse(pattern)
                    .map_err(|e| e.to_string())?;
                let mut short_literals: Vec<Vec<u8>> = Vec::new();
                let gram_plan = simplify_plan(hir_to_gram_plan(&hir, &mut short_literals));
                let mut short_hashes = Vec::new();
                for lit in &short_literals {
                    short_hashes.extend(query_short_hashes(lit));
                }
                short_hashes.sort_unstable();
                short_hashes.dedup();

                let re = RegexBuilder::new(pattern)
                    .case_insensitive(*case_insensitive)
                    .build()
                    .map_err(|e| e.to_string())?;
                Ok(Self {
                    mode,
                    gram_plan,
                    short_hashes,
                    matcher: VerifiedMatcher::Regex(re),
                })
            }
        }
    }

    pub fn verify_bytes(
        &self,
        haystack: &[u8],
        max_matches_per_file: usize,
    ) -> (Vec<ContentLineMatch>, u32, bool) {
        let mut raw_matches: Vec<(usize, usize)> = Vec::new();
        let symbol_only = matches!(
            self.matcher,
            VerifiedMatcher::LiteralExact {
                symbol_only: true,
                ..
            } | VerifiedMatcher::LiteralAsciiFolded {
                symbol_only: true,
                ..
            }
        );

        match &self.matcher {
            VerifiedMatcher::LiteralExact { needle, symbol_only } => {
                for start in memmem::find_iter(haystack, needle) {
                    if *symbol_only && !is_symbol_defined_at(haystack, start, needle.len()) {
                        continue;
                    }
                    raw_matches.push((start, start + needle.len()));
                }
            }
            VerifiedMatcher::LiteralAsciiFolded {
                folded,
                rare_idx,
                rare_b1,
                rare_b2,
                symbol_only,
            } => {
                let pat_len = folded.len();
                let mut search_from = 0usize;
                while search_from < haystack.len() {
                    let hit = if *rare_b1 == *rare_b2 {
                        memchr::memchr(*rare_b1, &haystack[search_from..])
                    } else {
                        memchr::memchr2(*rare_b1, *rare_b2, &haystack[search_from..])
                    };
                    let Some(rel) = hit else {
                        break;
                    };
                    let abs_pos = search_from + rel;
                    if abs_pos >= *rare_idx {
                        let start = abs_pos - *rare_idx;
                        let end = start + pat_len;
                        if end <= haystack.len()
                            && eq_ascii_folded(&haystack[start..end], folded)
                            && (!*symbol_only || is_symbol_defined_at(haystack, start, pat_len))
                        {
                            raw_matches.push((start, end));
                            search_from = end;
                            continue;
                        }
                    }
                    search_from = abs_pos + 1;
                }
            }
            VerifiedMatcher::Regex(re) => {
                for m in re.find_iter(haystack) {
                    if m.end() > m.start() {
                        raw_matches.push((m.start(), m.end()));
                    }
                    // 对单大文件限制全量扫描上限，防止极短通配正则产出百万命中。
                    if raw_matches.len() >= 1024 {
                        break;
                    }
                }
            }
        }

        if raw_matches.is_empty() {
            return (Vec::new(), 0, false);
        }

        let total = raw_matches.len() as u32;
        let has_symbol_def = if symbol_only {
            true
        } else {
            // 即使普通查询也附带标注符号定义位置以提升排序权重。
            raw_matches
                .iter()
                .take(8)
                .any(|&(s, e)| is_symbol_defined_at(haystack, s, e - s))
        };

        let line_matches = build_line_matches(haystack, &raw_matches, max_matches_per_file, symbol_only);
        (line_matches, total, has_symbol_def)
    }
}

/// 按倒排与 Bloom 计划从覆盖层与分段索引收集候选列表（高优先级 `Dirty` 与高 `doc_rank` 排前）。
pub fn collect_candidates<F>(
    query: &CompiledContentQuery,
    segments: &SegmentSet,
    overlay: &DirtyOverlay,
    mut path_filter: F,
    max_candidates: usize,
) -> Vec<CandidateDoc>
where
    F: FnMut(&str) -> bool,
{
    let mut out = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();

    // 1. 优先级最高：内存 `Dirty` 覆盖层中的最新编辑文档。
    let dirty_ids = match eval_plan_overlay(overlay, &query.gram_plan) {
        Some(ids) => ids,
        None => overlay.live_docs().map(|d| d.doc_id).collect(),
    };

    for did in dirty_ids {
        if !overlay.bloom_contains_all(did, &query.short_hashes) {
            continue;
        }
        let Some(doc) = overlay.doc(did) else {
            continue;
        };
        if !path_filter(&doc.input.path) {
            continue;
        }
        seen_paths.insert(doc.input.path.clone());
        out.push(CandidateDoc {
            path: doc.input.path.clone(),
            source_kind: doc.input.source_kind,
            content_hash: doc.input.content_hash,
            doc_rank: doc.input.doc_rank,
            modified_secs: doc.input.mtime_secs,
            size_bytes: doc.input.size_bytes,
            from_dirty: true,
        });
    }

    // 2. 从各 `mmap` 段收集未死亡且未被覆盖层替代的候选文档。
    for (seg, dead) in &segments.segments {
        if dead.live_count() == 0 {
            continue;
        }
        let cid_opt = eval_plan_segment(seg, &query.gram_plan);
        let cids: Vec<u32> = match cid_opt {
            Some(v) => v,
            None => (0..seg.n_contents()).collect(),
        };

        for cid in cids {
            if !seg.content_bloom_contains_all(cid, &query.short_hashes) {
                continue;
            }
            let Some(cmeta) = seg.content_meta(cid) else {
                continue;
            };
            let doc_ids = seg.doc_ids_for_content(cid);
            for doc_id in doc_ids {
                if dead.is_dead(doc_id) {
                    continue;
                }
                let Some(path) = seg.doc_path(doc_id) else {
                    continue;
                };
                if overlay.is_path_superseded(&path) || !path_filter(&path) {
                    continue;
                }
                if !seen_paths.insert(path.clone()) {
                    continue;
                }
                let Some(meta) = seg.doc_meta(doc_id) else {
                    continue;
                };
                out.push(CandidateDoc {
                    path,
                    source_kind: meta.source_kind,
                    content_hash: cmeta.content_hash,
                    doc_rank: meta.doc_rank,
                    modified_secs: meta.mtime_secs,
                    size_bytes: meta.size_bytes,
                    from_dirty: false,
                });
            }
        }
    }

    // 优先级排序：`Dirty` > 高 `doc_rank` > 更新修改时间 > 更短路径。
    out.sort_unstable_by(|a, b| {
        b.from_dirty
            .cmp(&a.from_dirty)
            .then_with(|| b.doc_rank.cmp(&a.doc_rank))
            .then_with(|| b.modified_secs.cmp(&a.modified_secs))
            .then_with(|| a.path.len().cmp(&b.path.len()))
    });

    if out.len() > max_candidates {
        out.truncate(max_candidates);
    }
    out
}

/// 在时间预算与原子取消令牌控制下，无锁并行读取磁盘纯文本或 Tier B 压缩正文仓完成精确校验。
pub fn verify_candidates(
    query: &CompiledContentQuery,
    candidates: &[CandidateDoc],
    text_store: Option<&TextStore>,
    options: &ContentQueryOptions,
    cancel: &AtomicBool,
) -> ContentSearchResponse {
    let start = Instant::now();
    let total_candidates_matched = candidates.len();
    let mut hits: Vec<ContentSearchHit> = Vec::new();
    let mut candidates_verified = 0usize;
    let mut truncated_by_budget = false;
    let mut cancelled = false;

    for chunk in candidates.chunks(VERIFY_BATCH_SIZE) {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        if start.elapsed().as_millis() as u64 >= options.budget_ms {
            truncated_by_budget = true;
            break;
        }

        let batch_results: Vec<Option<ContentSearchHit>> = chunk
            .par_iter()
            .map(|cand| {
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
                if let Some(min_sz) = options.min_size_bytes {
                    if cand.size_bytes < min_sz {
                        return None;
                    }
                }
                if let Some(max_sz) = options.max_size_bytes {
                    if cand.size_bytes > max_sz {
                        return None;
                    }
                }
                let _guard = NoMaterializeGuard::enter();
                verify_single_candidate(query, cand, text_store, options.max_matches_per_file)
            })
            .collect();

        candidates_verified += chunk.len();
        for res in batch_results.into_iter().flatten() {
            hits.push(res);
            if hits.len() >= options.max_results {
                break;
            }
        }
        if hits.len() >= options.max_results {
            break;
        }
    }

    // 最终结果按符号命中权重、`from_dirty`/rank 质量与文件命中频次排序。
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| b.modified_secs.cmp(&a.modified_secs))
            .then_with(|| a.path.cmp(&b.path))
    });
    if hits.len() > options.max_results {
        hits.truncate(options.max_results);
    }

    ContentSearchResponse {
        hits,
        total_candidates_matched,
        candidates_verified,
        truncated_by_budget,
        cancelled,
        elapsed_ms: start.elapsed().as_millis() as u64,
    }
}

fn verify_single_candidate(
    query: &CompiledContentQuery,
    cand: &CandidateDoc,
    text_store: Option<&TextStore>,
    max_matches_per_file: usize,
) -> Option<ContentSearchHit> {
    let (line_matches, total_matches, has_sym_def) = match cand.source_kind {
        ContentSourceKind::PlainText => {
            let p = Path::new(&cand.path);
            let meta = fs::symlink_metadata(p).ok()?;
            if is_dataless(&meta) || !meta.file_type().is_file() {
                return None;
            }
            let raw = fs::read(p).ok()?;
            let decoded = decode_plain_text(&raw)?;
            query.verify_bytes(decoded.as_ref(), max_matches_per_file)
        }
        ContentSourceKind::OfficeXml | ContentSourceKind::Pdf | ContentSourceKind::Image => {
            let store = text_store?;
            let text_arc = store.get_text(cand.content_hash)?;
            query.verify_bytes(text_arc.as_slice(), max_matches_per_file)
        }
    };

    if line_matches.is_empty() {
        return None;
    }

    let mut score = (cand.doc_rank as i32) * 10 + (total_matches.min(50) as i32) * 20;
    if has_sym_def {
        score += 5000;
    }
    if cand.from_dirty {
        score += 200;
    }

    Some(ContentSearchHit {
        path: cand.path.clone(),
        source_kind: cand.source_kind,
        score,
        line_matches,
        total_matches_in_file: total_matches,
        size_bytes: cand.size_bytes,
        modified_secs: cand.modified_secs,
    })
}

/// 按行聚合所有命中偏移，对超长行以首个命中为中心截取 UTF-8 安全窗口并重算相对高亮区间。
fn build_line_matches(
    haystack: &[u8],
    matches: &[(usize, usize)],
    max_lines: usize,
    symbol_only: bool,
) -> Vec<ContentLineMatch> {
    let mut out = Vec::new();
    let mut cur_line_num = 1u32;
    let mut prev_pos = 0usize;

    let mut idx = 0usize;
    while idx < matches.len() && out.len() < max_lines {
        let (m_start, _m_end) = matches[idx];
        cur_line_num += bytecount_newlines(&haystack[prev_pos..m_start]);
        prev_pos = m_start;

        let line_start = memchr::memrchr(b'\n', &haystack[..m_start]).map_or(0, |p| p + 1);
        let mut line_end = memchr::memchr(b'\n', &haystack[m_start..])
            .map_or(haystack.len(), |p| m_start + p);
        if line_end > line_start && haystack[line_end - 1] == b'\r' {
            line_end -= 1;
        }

        // 归并落入同一行的其余命中。
        let mut line_hits = Vec::new();
        while idx < matches.len() && matches[idx].0 < line_end {
            if matches[idx].1 <= line_end {
                line_hits.push(matches[idx]);
            } else {
                line_hits.push((matches[idx].0, line_end));
            }
            idx += 1;
        }
        if line_hits.is_empty() {
            idx += 1;
            continue;
        }

        let (win_start, win_end) = if line_end - line_start <= MAX_LINE_SNIPPET_BYTES {
            (line_start, line_end)
        } else {
            let center = line_hits[0].0;
            let raw_start = center.saturating_sub(MAX_LINE_SNIPPET_BYTES / 3).max(line_start);
            let raw_end = (raw_start + MAX_LINE_SNIPPET_BYTES).min(line_end);
            let s = align_utf8_floor(haystack, raw_start);
            let e = align_utf8_ceil(haystack, raw_end).min(line_end);
            (s, e)
        };

        let slice = &haystack[win_start..win_end];
        let line_text = String::from_utf8_lossy(slice).into_owned();
        let ranges: Vec<ContentMatchRange> = line_hits
            .iter()
            .filter_map(|&(s, e)| {
                if e <= win_start || s >= win_end {
                    return None;
                }
                let rs = s.saturating_sub(win_start) as u32;
                let re = e.min(win_end).saturating_sub(win_start) as u32;
                Some(ContentMatchRange { start: rs, end: re })
            })
            .collect();

        let column = (line_hits[0].0.saturating_sub(line_start) + 1) as u32;
        let sym = if symbol_only {
            true
        } else {
            is_symbol_defined_at(haystack, line_hits[0].0, line_hits[0].1 - line_hits[0].0)
        };

        out.push(ContentLineMatch {
            line_number: cur_line_num,
            column,
            line_text,
            match_ranges: ranges,
            is_symbol_def: sym,
        });
    }

    out
}

#[inline]
fn bytecount_newlines(slice: &[u8]) -> u32 {
    memchr::memchr_iter(b'\n', slice).count() as u32
}

#[inline]
fn align_utf8_floor(buf: &[u8], mut pos: usize) -> usize {
    pos = pos.min(buf.len());
    while pos > 0 && (buf[pos] & 0b1100_0000) == 0b1000_0000 {
        pos -= 1;
    }
    pos
}

#[inline]
fn align_utf8_ceil(buf: &[u8], mut pos: usize) -> usize {
    pos = pos.min(buf.len());
    while pos < buf.len() && (buf[pos] & 0b1100_0000) == 0b1000_0000 {
        pos += 1;
    }
    pos
}

#[inline]
fn eq_ascii_folded(a: &[u8], b_folded: &[u8]) -> bool {
    a.len() == b_folded.len()
        && a.iter()
            .zip(b_folded.iter())
            .all(|(&x, &y)| fold_ascii(x) == y)
}

fn pick_rarest_byte_idx(folded: &[u8]) -> usize {
    fn rank(b: u8) -> u8 {
        match b {
            b' ' | b'\t' | b'\n' | b'/' | b'.' => 0,
            b'e' | b't' | b'a' | b'i' | b'o' | b'n' | b'r' | b's' | b'l' => 1,
            b'c' | b'd' | b'u' | b'p' | b'm' | b'h' | b'g' => 2,
            b'_' | b'-' | b':' | b'(' | b')' => 3,
            b'0'..=b'9' => 4,
            _ => 5,
        }
    }
    folded
        .iter()
        .enumerate()
        .max_by_key(|&(idx, &b)| (rank(b), idx))
        .map_or(0, |(idx, _)| idx)
}

fn eval_plan_segment(seg: &Segment, plan: &GramPlan) -> Option<Vec<u32>> {
    match plan {
        GramPlan::All => None,
        GramPlan::Key(k) => match seg.lookup_key(*k) {
            None => Some(Vec::new()),
            Some(entry) => {
                let mut out = Vec::new();
                seg.posting_slice(&entry)
                    .decode_range(0..seg.n_contents(), &mut out);
                Some(out)
            }
        },
        GramPlan::PrefixRange { start, end } => {
            let range_end = end.saturating_add(1);
            let entries = seg.lookup_key_range(*start..range_end);
            if entries.is_empty() {
                return Some(Vec::new());
            }
            let mut acc = Vec::new();
            let mut buf = Vec::new();
            for entry in entries {
                buf.clear();
                seg.posting_slice(&entry)
                    .decode_range(0..seg.n_contents(), &mut buf);
                acc.extend_from_slice(&buf);
            }
            acc.sort_unstable();
            acc.dedup();
            Some(acc)
        }
        GramPlan::And(children) => {
            let mut direct_entries: Vec<DictEntry> = Vec::new();
            let mut complex: Vec<&GramPlan> = Vec::new();

            for child in children {
                match child {
                    GramPlan::All => {}
                    GramPlan::Key(k) => match seg.lookup_key(*k) {
                        Some(entry) => direct_entries.push(entry),
                        None => return Some(Vec::new()),
                    },
                    other => complex.push(other),
                }
            }

            if direct_entries.is_empty() && complex.is_empty() {
                return None;
            }

            // 按文档频率升序交集，尽早消减候选项。
            direct_entries.sort_unstable_by_key(|e| e.doc_freq);
            let mut acc_opt: Option<Vec<u32>> = None;

            if !direct_entries.is_empty() {
                let mut acc = Vec::with_capacity(direct_entries[0].doc_freq as usize);
                seg.posting_slice(&direct_entries[0])
                    .decode_range(0..seg.n_contents(), &mut acc);
                for entry in &direct_entries[1..] {
                    if acc.is_empty() {
                        return Some(Vec::new());
                    }
                    seg.posting_slice(entry).retain(&mut acc);
                }
                acc_opt = Some(acc);
            }

            for child in complex {
                if matches!(acc_opt, Some(ref v) if v.is_empty()) {
                    return Some(Vec::new());
                }
                let Some(sub_vec) = eval_plan_segment(seg, child) else {
                    continue;
                };
                match acc_opt {
                    None => acc_opt = Some(sub_vec),
                    Some(ref mut acc) => intersect_sorted_in_place(acc, &sub_vec),
                }
            }
            acc_opt
        }
        GramPlan::Or(children) => {
            let mut merged = Vec::new();
            for child in children {
                match eval_plan_segment(seg, child) {
                    None => return None,
                    Some(sub) => merged.extend(sub),
                }
            }
            merged.sort_unstable();
            merged.dedup();
            Some(merged)
        }
    }
}

fn eval_plan_overlay(overlay: &DirtyOverlay, plan: &GramPlan) -> Option<Vec<u32>> {
    match plan {
        GramPlan::All => None,
        GramPlan::Key(k) => Some(overlay.lookup_key(*k)),
        GramPlan::PrefixRange { start, end } => {
            Some(overlay.lookup_key_range(*start..end.saturating_add(1)))
        }
        GramPlan::And(children) => {
            let mut acc_opt: Option<Vec<u32>> = None;
            for child in children {
                let Some(sub) = eval_plan_overlay(overlay, child) else {
                    continue;
                };
                if sub.is_empty() {
                    return Some(Vec::new());
                }
                match acc_opt {
                    None => acc_opt = Some(sub),
                    Some(ref mut acc) => intersect_sorted_in_place(acc, &sub),
                }
            }
            acc_opt
        }
        GramPlan::Or(children) => {
            let mut merged = Vec::new();
            for child in children {
                match eval_plan_overlay(overlay, child) {
                    None => return None,
                    Some(sub) => merged.extend(sub),
                }
            }
            merged.sort_unstable();
            merged.dedup();
            Some(merged)
        }
    }
}

fn intersect_sorted_in_place(acc: &mut Vec<u32>, rhs: &[u32]) {
    if acc.is_empty() || rhs.is_empty() {
        acc.clear();
        return;
    }
    let mut write = 0usize;
    let mut j = 0usize;
    for i in 0..acc.len() {
        let val = acc[i];
        while j < rhs.len() && rhs[j] < val {
            j += 1;
        }
        if j < rhs.len() && rhs[j] == val {
            acc[write] = val;
            write += 1;
        }
    }
    acc.truncate(write);
}

fn hir_to_gram_plan(hir: &Hir, bloom_literals: &mut Vec<Vec<u8>>) -> GramPlan {
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => GramPlan::All,
        HirKind::Literal(lit) => {
            if lit.0.len() >= 5 {
                bloom_literals.push(lit.0.to_vec());
            }
            literal_gram_plan(&lit.0, false)
        }
        HirKind::Class(cls) => {
            let _ = extract_folded_ascii_class(cls);
            GramPlan::All
        }
        HirKind::Capture(cap) => hir_to_gram_plan(&cap.sub, bloom_literals),
        HirKind::Repetition(rep) => {
            if rep.min >= 1 {
                hir_to_gram_plan(&rep.sub, bloom_literals)
            } else {
                GramPlan::All
            }
        }
        HirKind::Concat(parts) => {
            // 将相邻字面量以及等价 ASCII 大小写折叠单字符类拼接为连续文本再提三元组，兼容 `(?i)...`。
            let mut plans: Vec<GramPlan> = Vec::new();
            let mut run: Vec<u8> = Vec::new();

            let flush_run = |run: &mut Vec<u8>,
                             plans: &mut Vec<GramPlan>,
                             bloom_literals: &mut Vec<Vec<u8>>| {
                if !run.is_empty() {
                    if run.len() >= 5 {
                        bloom_literals.push(run.clone());
                    }
                    let p = literal_gram_plan(run, false);
                    if p != GramPlan::All {
                        plans.push(p);
                    }
                    run.clear();
                }
            };

            for part in parts {
                match part.kind() {
                    HirKind::Literal(lit) => {
                        for &b in lit.0.iter() {
                            run.push(fold_ascii(b));
                        }
                    }
                    HirKind::Class(cls) => {
                        if let Some(folded_b) = extract_folded_ascii_class(cls) {
                            run.push(folded_b);
                        } else {
                            flush_run(&mut run, &mut plans, bloom_literals);
                        }
                    }
                    _ => {
                        flush_run(&mut run, &mut plans, bloom_literals);
                        let sub_plan = hir_to_gram_plan(part, bloom_literals);
                        if sub_plan != GramPlan::All {
                            plans.push(sub_plan);
                        }
                    }
                }
            }
            flush_run(&mut run, &mut plans, bloom_literals);
            if plans.is_empty() {
                GramPlan::All
            } else {
                GramPlan::And(plans)
            }
        }
        HirKind::Alternation(branches) => {
            // 分支不能跨分支强加相同 Bloom，避免误淘汰有效分支。
            let mut dummy = Vec::new();
            let sub_plans: Vec<GramPlan> = branches
                .iter()
                .map(|b| hir_to_gram_plan(b, &mut dummy))
                .collect();
            if sub_plans.iter().any(|p| *p == GramPlan::All) || sub_plans.is_empty() {
                GramPlan::All
            } else {
                GramPlan::Or(sub_plans)
            }
        }
    }
}

fn extract_folded_ascii_class(cls: &Class) -> Option<u8> {
    match cls {
        Class::Unicode(u) => {
            let mut target: Option<u8> = None;
            for r in u.iter() {
                let s = r.start();
                let e = r.end();
                if !s.is_ascii() || !e.is_ascii() || (e as u32 - s as u32) > 0 {
                    return None;
                }
                let folded = fold_ascii(s as u8);
                match target {
                    None => target = Some(folded),
                    Some(prev) if prev == folded => {}
                    _ => return None,
                }
            }
            target
        }
        Class::Bytes(b) => {
            let mut target: Option<u8> = None;
            for r in b.iter() {
                if r.start() != r.end() {
                    return None;
                }
                let folded = fold_ascii(r.start());
                match target {
                    None => target = Some(folded),
                    Some(prev) if prev == folded => {}
                    _ => return None,
                }
            }
            target
        }
    }
}

fn simplify_plan(plan: GramPlan) -> GramPlan {
    match plan {
        GramPlan::And(items) => {
            let mut flat = Vec::new();
            for item in items {
                match simplify_plan(item) {
                    GramPlan::All => {}
                    GramPlan::And(sub) => flat.extend(sub),
                    other => flat.push(other),
                }
            }
            if flat.is_empty() {
                GramPlan::All
            } else if flat.len() == 1 {
                flat.pop().unwrap()
            } else {
                GramPlan::And(flat)
            }
        }
        GramPlan::Or(items) => {
            let mut flat = Vec::new();
            for item in items {
                match simplify_plan(item) {
                    GramPlan::All => return GramPlan::All,
                    GramPlan::Or(sub) => flat.extend(sub),
                    other => flat.push(other),
                }
            }
            if flat.is_empty() {
                GramPlan::All
            } else if flat.len() == 1 {
                flat.pop().unwrap()
            } else {
                GramPlan::Or(flat)
            }
        }
        other => other,
    }
}

/// 辅助计算单文件的默认检索优先级评分（源码优先，其次文档，再次数据文件，路径越浅略高）。
pub fn compute_doc_rank(path: &str, kind: ContentSourceKind) -> u16 {
    let base: u16 = match kind {
        ContentSourceKind::PlainText => {
            let ext = path.rsplit('.').next().map(|s| s.to_ascii_lowercase());
            match ext.as_deref() {
                Some("rs" | "swift" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "c" | "cpp" | "h" | "hpp" | "java" | "kt") => 420,
                Some("md" | "mdx" | "txt" | "toml" | "yaml" | "yml") => 320,
                Some("json" | "xml" | "csv" | "tsv") => 180,
                _ => 280,
            }
        }
        ContentSourceKind::OfficeXml => 240,
        ContentSourceKind::Pdf => 220,
        ContentSourceKind::Image => 140,
    };
    let depth = path.bytes().filter(|&b| b == b'/').count().min(40) as u16;
    base + (40 - depth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_index::segment::{SegmentDocInput, SegmentSet};
    use std::io::Write;
    use tempfile::TempDir;

    fn write_test_file(dir: &Path, rel: &str, body: &[u8]) -> String {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(body).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn literal_regex_symbol_path_filter_and_line_ranges() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("files");
        let seg_dir = tmp.path().join("segs");

        let p1 = write_test_file(
            &src_dir,
            "src/engine.rs",
            b"use std::fmt;\nfn alpha_engine(x: i32) -> i32 {\n    x + 1\n}\n",
        );
        let p2 = write_test_file(
            &src_dir,
            "src/caller.rs",
            b"fn run() {\n    let v = alpha_engine(42);\n    // \xe4\xb8\xad\xe6\x96\x87\xe6\xa3\x80\xe7\xb4\xa2\xe5\xbc\x95\xe6\x93\x8e\n}\n",
        );
        let p3 = write_test_file(
            &src_dir,
            "docs/notes.md",
            b"Alpha Engine design doc\nRegex token ABC-9001 here\n",
        );

        let mut segments = SegmentSet::open_dir(&seg_dir).unwrap();
        segments
            .add_tier0(vec![
                SegmentDocInput::from_decoded_text(
                    p1.clone(),
                    100,
                    60,
                    ContentSourceKind::PlainText,
                    440,
                    0,
                    &fs::read(&p1).unwrap(),
                    true,
                ),
                SegmentDocInput::from_decoded_text(
                    p2.clone(),
                    100,
                    60,
                    ContentSourceKind::PlainText,
                    440,
                    0,
                    &fs::read(&p2).unwrap(),
                    true,
                ),
            ])
            .unwrap();

        let mut overlay = DirtyOverlay::default();
        overlay.upsert_doc(
            SegmentDocInput::from_decoded_text(
                p3.clone(),
                200,
                50,
                ContentSourceKind::PlainText,
                340,
                0,
                &fs::read(&p3).unwrap(),
                true,
            ),
            Some(&mut segments),
        );

        let cancel = AtomicBool::new(false);
        let opts = ContentQueryOptions::default();

        // 1. 符号定义查询 `sym:alpha_engine` 应仅命中定义处 p1，不命中调用处 p2。
        let sym_q = CompiledContentQuery::compile(ContentQueryMode::Literal {
            pattern: "alpha_engine".to_string(),
            case_insensitive: false,
            symbol_only: true,
        })
        .unwrap();
        let sym_cands = collect_candidates(&sym_q, &segments, &overlay, |_| true, 100);
        let sym_resp = verify_candidates(&sym_q, &sym_cands, None, &opts, &cancel);
        assert_eq!(sym_resp.hits.len(), 1);
        assert_eq!(sym_resp.hits[0].path, p1);
        assert_eq!(sym_resp.hits[0].line_matches[0].line_number, 2);
        assert!(sym_resp.hits[0].line_matches[0].is_symbol_def);

        // 2. 正则 + 路径过滤 + UTF-8 字节高亮偏移校验（含大小写折叠与中文测试）。
        let re_q = CompiledContentQuery::compile(ContentQueryMode::Regex {
            pattern: r"abc-\d{4}".to_string(),
            case_insensitive: true,
        })
        .unwrap();
        assert_ne!(re_q.gram_plan, GramPlan::All);
        let re_cands = collect_candidates(
            &re_q,
            &segments,
            &overlay,
            |path| path.ends_with(".md"),
            100,
        );
        let re_resp = verify_candidates(&re_q, &re_cands, None, &opts, &cancel);
        assert_eq!(re_resp.hits.len(), 1);
        let lm = &re_resp.hits[0].line_matches[0];
        assert_eq!(lm.line_number, 2);
        let r = lm.match_ranges[0];
        assert_eq!(&lm.line_text[r.start as usize..r.end as usize], "ABC-9001");

        // 3. 汉字字面量搜索命中 p2 第 3 行。
        let cjk_q = CompiledContentQuery::compile(ContentQueryMode::Literal {
            pattern: "中文检索".to_string(),
            case_insensitive: true,
            symbol_only: false,
        })
        .unwrap();
        let cjk_cands = collect_candidates(&cjk_q, &segments, &overlay, |_| true, 100);
        let cjk_resp = verify_candidates(&cjk_q, &cjk_cands, None, &opts, &cancel);
        assert_eq!(cjk_resp.hits.len(), 1);
        assert_eq!(cjk_resp.hits[0].path, p2);
        assert_eq!(cjk_resp.hits[0].line_matches[0].line_number, 3);
    }

    #[test]
    fn cancellation_and_zero_budget_stop_verification_early() {
        let tmp = TempDir::new().unwrap();
        let p = write_test_file(tmp.path(), "a.rs", b"fn cancel_token_test() {}\n");
        let cand = vec![CandidateDoc {
            path: p,
            source_kind: ContentSourceKind::PlainText,
            content_hash: 1,
            doc_rank: 400,
            modified_secs: 10,
            size_bytes: 26,
            from_dirty: true,
        }];
        let q = CompiledContentQuery::compile(ContentQueryMode::Literal {
            pattern: "cancel_token_test".to_string(),
            case_insensitive: true,
            symbol_only: false,
        })
        .unwrap();

        let cancel = AtomicBool::new(true);
        let resp = verify_candidates(&q, &cand, None, &ContentQueryOptions::default(), &cancel);
        assert!(resp.cancelled);
        assert_eq!(resp.candidates_verified, 0);

        let cancel_ok = AtomicBool::new(false);
        let zero_budget = ContentQueryOptions {
            budget_ms: 0,
            ..Default::default()
        };
        let resp2 = verify_candidates(&q, &cand, None, &zero_budget, &cancel_ok);
        assert!(resp2.truncated_by_budget);
        assert_eq!(resp2.candidates_verified, 0);
    }
}
