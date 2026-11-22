//! 双层复合排序模型与上界剪枝基础设施。
//!
//! 第一层将文件名级评分（`BaseNameScore`）与记录级上下文（`RecordRankContext`）分离，
//! 使得重名桶内成千上万条记录可先依据 `BaseNameScore::upper_bound` 在唯一名表阶段整体剪枝；
//! 第二层再针对未落败桶内的真实记录计算目录深度、目录/.app 权重与隐藏路径降权，
//! 确保排序精度的同时将全库检索延迟控制在亚毫秒至毫秒量级。

use std::borrow::Cow;

use crate::cjk;
use crate::indexer::MatchScore;
use crate::pinyin::{PinyinMatchTier, score_pinyin_detail};
use crate::string_pool::find_ascii_ignore_case;

/// 主层级步长：相邻两个主匹配层级之间相隔 1,000,000，确保次层级与上下文微调永不越级。
pub const TIER_STRIDE: i64 = 1_000_000;
/// 次层级步长：同一主层级内精细区分前后边界、拼音全拼/简拼等质量梯度。
pub const SUB_TIER_STRIDE: i64 = 100_000;
/// 上下文微调零点偏移：保证组合调整项被限制在 `[0, 99_999]` 非负槽位内。
pub const ADJ_OFFSET: i64 = 40_000;
/// 隐藏文件或隐藏祖先目录惩罚：整体下移 4 个主层级，仅当查询显式瞄准点号项时豁免。
pub const HIDDEN_TIER_PENALTY: i64 = 4 * TIER_STRIDE;

/// 单个重名桶最大扫描记录数：超出后首尾各扫一半，既覆盖早期索引也保留最近变更条目。
pub const MAX_BUCKET_SCAN: usize = 100_000;
const HALF_BUCKET_SCAN: usize = MAX_BUCKET_SCAN / 2;

/// 目录条目加分。
pub const DIR_BONUS: i64 = 600;
/// `.app` 应用包额外加分（叠加 `DIR_BONUS` 合计 +3,800），压过外围小文件与大小写差异。
pub const APP_BUNDLE_BONUS: i64 = 3_200;
/// 父目录每增加一层路径深度的扣分。
pub const DEPTH_PENALTY_STEP: i64 = 320;
/// 计入扣分的最大路径深度上限，防止极端深目录挤占夹逼槽位。
pub const MAX_DEPTH: usize = 32;

/// 路径检索中目标目录直接子项加分。
pub const PATH_BONUS_DIRECT: i64 = 4_000;
/// 路径检索中更深层祖先命中加分。
pub const PATH_BONUS_ANCESTOR: i64 = 1_000;

const MIN_SCORE_F64: f64 = -3_500_000.0;
const MAX_SCORE_F64: f64 = 8_100_000.0;
const EXACT_CASE_BONUS: i64 = 200;

/// 匹配强度主层级（数值越大越靠前）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum MatchTier {
    Fuzzy = 1,
    PinyinWeak = 2,
    PinyinStrong = 3,
    Substring = 4,
    WordBoundary = 5,
    Prefix = 6,
    StemExact = 7,
    FullExact = 8,
}

/// 64 位整数复合排序分（数值越大优先级越高）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RankScore(pub i64);

impl RankScore {
    /// 按统一量纲组装主层级、次层级、夹逼微调分与隐藏路径降权。
    #[inline]
    pub fn assemble(
        effective_tier: MatchTier,
        sub_tier: u8,
        adj: i64,
        hidden_demoted: bool,
    ) -> Self {
        let clamped_adj = adj.clamp(-40_000, 59_999);
        let hidden_shift = if hidden_demoted {
            HIDDEN_TIER_PENALTY
        } else {
            0
        };
        Self(
            (effective_tier as i64) * TIER_STRIDE
                + (sub_tier as i64) * SUB_TIER_STRIDE
                + ADJ_OFFSET
                + clamped_adj
                - hidden_shift,
        )
    }

    /// 投影至三档旧版 `MatchScore`，供对外兼容接口展示大致命中级别。
    #[inline]
    pub fn to_match_score(self) -> MatchScore {
        if self.0 >= (MatchTier::FullExact as i64) * TIER_STRIDE {
            MatchScore::Exact
        } else if self.0 >= (MatchTier::Prefix as i64) * TIER_STRIDE {
            MatchScore::Prefix
        } else {
            MatchScore::Substring
        }
    }

    /// 严格保序线性归一化至 `[1e-6, 1.0]`，使所有主层级、隐藏档位与深度梯度在浮点域保持单调严格区分。
    #[inline]
    pub fn to_f64(self) -> f64 {
        let norm = (self.0 as f64 - MIN_SCORE_F64) / (MAX_SCORE_F64 - MIN_SCORE_F64);
        norm.clamp(1e-6, 1.0)
    }
}

/// 唯一文件名粒度的预评分结果，不依赖具体记录的父路径或目录属性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseNameScore {
    pub tier: MatchTier,
    pub sub_tier: u8,
    pub name_sub: i64,
    pub is_app_bundle_name: bool,
    pub is_name_hidden: bool,
}

impl BaseNameScore {
    /// 仅依赖文件名长度的精确命中基分（用于扩展名精确加速路径及路径目录自身命中）。
    #[inline]
    pub fn for_exact_fast_path(name: &str) -> Self {
        Self::for_tier_only(name, MatchTier::FullExact, 0)
    }

    /// 按固定主/次层级与长度惩罚构造文件名基分（用于空查询筛选模式与纯父路径模式）。
    #[inline]
    pub fn for_tier_only(name: &str, tier: MatchTier, sub_tier: u8) -> Self {
        Self {
            tier,
            sub_tier,
            name_sub: len_penalty(name),
            is_app_bundle_name: is_app_bundle_name(name),
            is_name_hidden: name.starts_with('.'),
        }
    }

    /// 构造模糊匹配文件名基分，保留 nucleo 分数梯度。
    #[inline]
    pub fn for_fuzzy(name: &str, nucleo_score: u32) -> Self {
        let fuzzy_bonus = (nucleo_score.min(2000) as i64) * 10;
        Self {
            tier: MatchTier::Fuzzy,
            sub_tier: 0,
            name_sub: len_penalty(name) + fuzzy_bonus,
            is_app_bundle_name: is_app_bundle_name(name),
            is_name_hidden: name.starts_with('.'),
        }
    }

    /// 构造多词父目录+文件名混合命中的基分：
    /// 处于 `MatchTier::Fuzzy` 内以让位于全部纯文件名非隐藏命中，同时高于 `nucleo` 模糊跳字（`sub_tier = 0`）。
    #[inline]
    pub fn for_hybrid_parent_name(name: &str, parent_word_boundary: bool, name_bonus: i64) -> Self {
        Self {
            tier: MatchTier::Fuzzy,
            sub_tier: if parent_word_boundary { 2 } else { 1 },
            name_sub: len_penalty(name) + name_bonus.clamp(-5_000, 30_000),
            is_app_bundle_name: is_app_bundle_name(name),
            is_name_hidden: name.starts_with('.'),
        }
    }

    /// 计算该唯一名在当前筛选条件下的全局最高可能分（上界），用于扫描记录桶前安全跳过。
    ///
    /// 上界条件证明：
    /// 1. 若 `tier == StemExact && is_app_bundle_name && allow_dirs`，该名下目录项可能晋升为 `FullExact`；
    /// 2. 目录加分、应用包加分与动态上下文加成取允许上限，且路径深度惩罚取最佳深度 0；
    /// 3. 仅以文件名自身点号决定最小隐藏降权，避免误杀未落于隐藏祖先中的条目。
    #[inline]
    pub fn upper_bound(
        self,
        ctx: &RecordRankContext,
        allow_dirs: bool,
        targets_hidden: bool,
    ) -> RankScore {
        self.upper_bound_with_extra(ctx, allow_dirs, targets_hidden, 0)
    }

    /// 带附加路径加分上界的变体，供路径检索中区分直属/祖先加分上限时复用。
    #[inline]
    pub fn upper_bound_with_extra(
        self,
        ctx: &RecordRankContext,
        allow_dirs: bool,
        targets_hidden: bool,
        extra_adj_upper: i64,
    ) -> RankScore {
        let max_tier =
            if self.tier == MatchTier::StemExact && self.is_app_bundle_name && allow_dirs {
                MatchTier::FullExact
            } else {
                self.tier
            };
        let max_dir_adj = if allow_dirs {
            DIR_BONUS
                + if self.is_app_bundle_name {
                    APP_BUNDLE_BONUS
                } else {
                    0
                }
        } else {
            0
        };
        let max_record_adj = max_dir_adj + extra_adj_upper + ctx.max_dynamic_bonus();
        let min_hidden_demoted = self.is_name_hidden && !targets_hidden;
        RankScore::assemble(
            max_tier,
            self.sub_tier,
            self.name_sub + max_record_adj,
            min_hidden_demoted,
        )
    }
}

/// 记录级动态排序上下文（预留给访问频率、时间衰减等动态特性扩展）。
#[derive(Debug, Default, Clone, Copy)]
pub struct RecordRankContext {
    pub _reserved_recent_max_bonus: i64,
}

impl RecordRankContext {
    /// 返回动态信号对单条记录可能贡献的最大加分，参与构建数学完备的桶级剪枝上界。
    #[inline]
    pub fn max_dynamic_bonus(&self) -> i64 {
        self._reserved_recent_max_bonus
    }
}

/// 基于唯一名基分与单条记录的真实目录/父路径属性合成最终排序分。
#[inline]
pub fn score_record(
    base: BaseNameScore,
    is_dir: bool,
    parent: &str,
    ctx: &RecordRankContext,
    targets_hidden: bool,
) -> RankScore {
    score_record_with_extra(base, is_dir, parent, ctx, targets_hidden, 0)
}

/// 在通用记录评分基础上叠加调用方附加调整分（如路径匹配质量加分）。
#[inline]
pub fn score_record_with_extra(
    base: BaseNameScore,
    is_dir: bool,
    parent: &str,
    ctx: &RecordRankContext,
    targets_hidden: bool,
    extra_adj: i64,
) -> RankScore {
    score_record_with_meta(
        base,
        is_dir,
        parent_depth(parent),
        is_parent_hidden(parent.as_bytes()),
        ctx,
        targets_hidden,
        extra_adj,
    )
}

/// 直接消费预计算的 `(parent_depth, parent_hidden)` 元数据合成最终排序分，
/// 供 `QueryIndex::parent_meta` 单字节查表热路径调用，免除父目录字符串解引用与双遍字节扫描。
#[inline]
pub fn score_record_with_meta(
    base: BaseNameScore,
    is_dir: bool,
    parent_depth: usize,
    parent_hidden: bool,
    _ctx: &RecordRankContext,
    targets_hidden: bool,
    extra_adj: i64,
) -> RankScore {
    // 仅当 StemExact 对应目录且为 `.app` 应用包时，提升至 FullExact，
    // 使 `/Applications/Safari.app` 在搜索 "Safari" 时与内层二进制同处于最高层级并借包权重胜出。
    let effective_tier =
        if base.tier == MatchTier::StemExact && is_dir && base.is_app_bundle_name {
            MatchTier::FullExact
        } else {
            base.tier
        };
    let depth_penalty = -((parent_depth.min(MAX_DEPTH)) as i64) * DEPTH_PENALTY_STEP;
    let dir_bonus = if is_dir { DIR_BONUS } else { 0 };
    let app_bonus = if is_dir && base.is_app_bundle_name {
        APP_BUNDLE_BONUS
    } else {
        0
    };
    let record_adj = depth_penalty + dir_bonus + app_bonus + extra_adj;
    let hidden_demoted = !targets_hidden && (base.is_name_hidden || parent_hidden);
    RankScore::assemble(
        effective_tier,
        base.sub_tier,
        base.name_sub + record_adj,
        hidden_demoted,
    )
}

/// 超大重复名桶双端切片：小桶完整返回，大桶首尾各取 50,000 条。
#[inline]
pub fn bucket_slices(entries: &[u32]) -> (&[u32], &[u32]) {
    if entries.len() <= MAX_BUCKET_SCAN {
        (entries, &[])
    } else {
        (
            &entries[..HALF_BUCKET_SCAN],
            &entries[entries.len() - HALF_BUCKET_SCAN..],
        )
    }
}

/// 文件名长度惩罚：鼓励更凝练精确的名字排在同层级长文件名之前。
#[inline]
pub fn len_penalty(name: &str) -> i64 {
    -(name.chars().count().min(200) as i64) * 16
}

/// 零分配检测 `.app` 应用包文件名（排除仅 `".app"` 这种空主干形式）。
#[inline]
pub fn is_app_bundle_name(name: &str) -> bool {
    name.len() > 4 && name.as_bytes()[name.len() - 4..].eq_ignore_ascii_case(b".app")
}

/// 零分配检测父路径任意段是否以 `.` 开头（即位于隐藏目录子树下）。
#[inline]
pub fn is_parent_hidden(parent_bytes: &[u8]) -> bool {
    crate::compact_index::is_parent_hidden(parent_bytes)
}

/// 检测整条记录（文件名或任意祖先目录）是否属于隐藏项。
#[inline]
pub fn is_record_hidden(name: &str, parent: &str) -> bool {
    name.starts_with('.') || is_parent_hidden(parent.as_bytes())
}

/// 判断关键词查询是否显式瞄准点号开头项；扩展名快路径不视为显式瞄准隐藏文件。
#[inline]
pub fn keywords_target_hidden(is_extension_mode: bool, keywords: &[String]) -> bool {
    !is_extension_mode && keywords.iter().any(|k| k.starts_with('.'))
}

/// 零分配统计父路径中的非空目录层级数。
#[inline]
pub fn parent_depth(parent: &str) -> usize {
    crate::compact_index::parent_depth(parent)
}

#[inline]
fn early_match_bonus(first_match_byte: usize) -> i64 {
    (64 - first_match_byte.min(64) as i64) * 8
}

#[inline]
fn is_cjk_char(c: char) -> bool {
    matches!(
        c as u32,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2A6DF
            | 0x3040..=0x30FF
            | 0xAC00..=0xD7AF
    )
}

/// 统一字符边界转移谓词：
/// 覆盖非字母数字分隔符（ASCII 标点、空白及全部中文标点）、驼峰、全大写缩写切分、数字/字母过渡以及中西文切换。
#[inline]
fn is_boundary_pair(prev: char, curr: char, next: Option<char>) -> bool {
    if !prev.is_alphanumeric() || !curr.is_alphanumeric() {
        return true;
    }
    if prev.is_lowercase() && curr.is_uppercase() {
        return true;
    }
    if prev.is_uppercase() && curr.is_uppercase() && next.is_some_and(char::is_lowercase) {
        return true;
    }
    if (prev.is_ascii_digit() && curr.is_ascii_alphabetic())
        || (prev.is_ascii_alphabetic() && curr.is_ascii_digit())
    {
        return true;
    }
    (is_cjk_char(prev) && curr.is_ascii_alphanumeric())
        || (prev.is_ascii_alphanumeric() && is_cjk_char(curr))
}

#[inline]
fn is_start_boundary(s: &str, start: usize) -> bool {
    if start == 0 {
        return true;
    }
    let Some(prev) = s[..start].chars().next_back() else {
        return true;
    };
    let mut rest = s[start..].chars();
    let Some(curr) = rest.next() else {
        return true;
    };
    is_boundary_pair(prev, curr, rest.next())
}

#[inline]
fn is_end_boundary(s: &str, end: usize) -> bool {
    if end >= s.len() {
        return true;
    }
    let Some(last) = s[..end].chars().next_back() else {
        return true;
    };
    let mut rest = s[end..].chars();
    let Some(after) = rest.next() else {
        return true;
    };
    is_boundary_pair(last, after, rest.next())
}

#[inline]
fn valid_stem(s: &str) -> Option<&str> {
    let (stem, ext) = s.rsplit_once('.')?;
    if !stem.is_empty() && !ext.is_empty() && !ext.chars().any(char::is_whitespace) {
        Some(stem)
    } else {
        None
    }
}

/// 在大小写无关/敏感模式下对单个唯一名评估主次层级与名称内细粒度加分。
pub fn score_name_rank(
    orig_name: &str,
    folded_opt: Option<&str>,
    keywords: &[String],
    raw_keywords: &[String],
    all_keywords_ascii: bool,
    case_sensitive: bool,
    explicit_sensitive_lock: bool,
) -> Option<BaseNameScore> {
    if keywords.is_empty() {
        return None;
    }

    // 零分配 ASCII 大小写无关快路径：绝大多数系统文件名纯属 ASCII
    if !case_sensitive && orig_name.is_ascii() {
        if !all_keywords_ascii {
            return None;
        }
        return score_ascii_name(orig_name, keywords, raw_keywords);
    }

    score_general_name(
        orig_name,
        folded_opt,
        keywords,
        raw_keywords,
        all_keywords_ascii,
        case_sensitive,
        explicit_sensitive_lock,
    )
}

#[derive(Clone, Copy)]
struct KeywordHit {
    tier: MatchTier,
    sub_tier: u8,
    start: usize,
    end: usize,
    exact_case: bool,
    is_start_boundary: bool,
    is_end_boundary: bool,
}

#[inline]
fn eval_ascii_keyword(orig_name: &str, kw: &str, raw_kw: &str) -> Option<KeywordHit> {
    let name_bytes = orig_name.as_bytes();
    let kb = kw.as_bytes();
    if orig_name.eq_ignore_ascii_case(kw) {
        return Some(KeywordHit {
            tier: MatchTier::FullExact,
            sub_tier: 0,
            start: 0,
            end: name_bytes.len(),
            exact_case: orig_name == raw_kw,
            is_start_boundary: true,
            is_end_boundary: true,
        });
    }
    if let Some(stem) = valid_stem(orig_name) {
        if stem.eq_ignore_ascii_case(kw) {
            return Some(KeywordHit {
                tier: MatchTier::StemExact,
                sub_tier: 0,
                start: 0,
                end: stem.len(),
                exact_case: stem == raw_kw,
                is_start_boundary: true,
                is_end_boundary: true,
            });
        }
    }
    if name_bytes.len() >= kb.len() && name_bytes[..kb.len()].eq_ignore_ascii_case(kb) {
        let end_ok = is_end_boundary(orig_name, kb.len());
        return Some(KeywordHit {
            tier: MatchTier::Prefix,
            sub_tier: u8::from(end_ok),
            start: 0,
            end: kb.len(),
            exact_case: &orig_name[..kb.len()] == raw_kw,
            is_start_boundary: true,
            is_end_boundary: end_ok,
        });
    }

    let mut best: Option<KeywordHit> = None;
    let mut cursor = 0usize;
    while cursor + kb.len() <= name_bytes.len() {
        let Some(rel) = find_ascii_ignore_case(&name_bytes[cursor..], kb) else {
            break;
        };
        let idx = cursor + rel;
        let end = idx + kb.len();
        let start_ok = is_start_boundary(orig_name, idx);
        let end_ok = start_ok && is_end_boundary(orig_name, end);
        let (cand_tier, cand_sub) = if start_ok {
            (MatchTier::WordBoundary, u8::from(end_ok))
        } else {
            (MatchTier::Substring, 0)
        };
        let cand_exact = &orig_name[idx..end] == raw_kw;
        let replace = match best {
            None => true,
            Some(prev) => (cand_tier, cand_sub) > (prev.tier, prev.sub_tier),
        };
        if replace {
            best = Some(KeywordHit {
                tier: cand_tier,
                sub_tier: cand_sub,
                start: idx,
                end,
                exact_case: cand_exact,
                is_start_boundary: start_ok,
                is_end_boundary: end_ok,
            });
        }
        cursor = idx + 1;
    }
    best
}

#[inline]
fn eval_general_keyword(
    hs: &str,
    cp: &str,
    orig_nfc: &str,
    kw: &str,
    raw_kw: &str,
) -> Option<KeywordHit> {
    let raw_kw_nfc = cjk::normalize_nfc_cow(raw_kw);
    if hs == kw {
        return Some(KeywordHit {
            tier: MatchTier::FullExact,
            sub_tier: 0,
            start: 0,
            end: hs.len(),
            exact_case: orig_nfc == raw_kw_nfc.as_ref(),
            is_start_boundary: true,
            is_end_boundary: true,
        });
    }
    if let Some(stem) = valid_stem(hs) {
        if stem == kw {
            let exact = valid_stem(orig_nfc).is_some_and(|s| s == raw_kw_nfc.as_ref());
            return Some(KeywordHit {
                tier: MatchTier::StemExact,
                sub_tier: 0,
                start: 0,
                end: stem.len(),
                exact_case: exact,
                is_start_boundary: true,
                is_end_boundary: true,
            });
        }
    }
    if hs.starts_with(kw) {
        let (_, cp_end) = map_span(cp, hs, 0, kw.len());
        let end_ok = is_end_boundary(cp, cp_end);
        let exact = orig_nfc.starts_with(raw_kw_nfc.as_ref());
        return Some(KeywordHit {
            tier: MatchTier::Prefix,
            sub_tier: u8::from(end_ok),
            start: 0,
            end: kw.len(),
            exact_case: exact,
            is_start_boundary: true,
            is_end_boundary: end_ok,
        });
    }

    let mut best: Option<KeywordHit> = None;
    let mut cursor = 0usize;
    while cursor + kw.len() <= hs.len() {
        let Some(rel) = hs[cursor..].find(kw) else {
            break;
        };
        let idx = cursor + rel;
        let end = idx + kw.len();
        let (cp_s, cp_e) = map_span(cp, hs, idx, end);
        let start_ok = is_start_boundary(cp, cp_s);
        let end_ok = start_ok && is_end_boundary(cp, cp_e);
        let (cand_tier, cand_sub) = if start_ok {
            (MatchTier::WordBoundary, u8::from(end_ok))
        } else {
            (MatchTier::Substring, 0)
        };
        let (nfc_s, nfc_e) = map_span(orig_nfc, hs, idx, end);
        let cand_exact = orig_nfc
            .get(nfc_s..nfc_e)
            .is_some_and(|slice| slice == raw_kw_nfc.as_ref());
        let replace = match best {
            None => true,
            Some(prev) => (cand_tier, cand_sub) > (prev.tier, prev.sub_tier),
        };
        if replace {
            best = Some(KeywordHit {
                tier: cand_tier,
                sub_tier: cand_sub,
                start: idx,
                end,
                exact_case: cand_exact,
                is_start_boundary: start_ok,
                is_end_boundary: end_ok,
            });
        }
        let step = hs[idx..].chars().next().map_or(1, char::len_utf8);
        cursor = idx + step;
    }
    best
}

/// 零堆分配判定多词命中的字节区间是否完整覆盖文件名主干（stem）中的所有字母/数字/汉字字符。
/// 当所有词元均对齐词边界且合力覆盖整个主干（如 `"user config"` 命中 `"user_config.json"`）时返回 `true`。
#[inline]
fn spans_cover_stem_ascii(name: &str, spans: &[(usize, usize)]) -> bool {
    let stem = valid_stem(name).unwrap_or(name);
    let mut has_alnum = false;
    for (idx, &b) in stem.as_bytes().iter().enumerate() {
        if b.is_ascii_alphanumeric() {
            has_alnum = true;
            if !spans.iter().any(|&(s, e)| idx >= s && idx < e) {
                return false;
            }
        }
    }
    has_alnum
}

#[inline]
fn spans_cover_stem_unicode(hs: &str, spans: &[(usize, usize)]) -> bool {
    let stem = valid_stem(hs).unwrap_or(hs);
    let mut has_alnum = false;
    for (idx, ch) in stem.char_indices() {
        if ch.is_alphanumeric() {
            has_alnum = true;
            if !spans.iter().any(|&(s, e)| idx >= s && idx < e) {
                return false;
            }
        }
    }
    has_alnum
}

/// 聚合多关键词命中特征，实现词序无关的 `Prefix` 晋升与主干完整覆盖次层级加分。
#[inline]
fn aggregate_multi_keyword_hits(
    hits: &[KeywordHit],
    full_stem_covered: bool,
) -> (MatchTier, u8, usize, bool, usize) {
    let mut all_start_boundary = true;
    let mut any_boundary = false;
    let mut any_start_zero = false;
    let mut all_end_boundary = true;
    let mut max_tier = MatchTier::Substring;
    let mut earliest_start = usize::MAX;
    let mut all_exact_case = true;
    let mut boundary_count = 0usize;

    for hit in hits {
        if hit.is_start_boundary {
            any_boundary = true;
            boundary_count += 1;
        } else {
            all_start_boundary = false;
        }
        if !hit.is_end_boundary {
            all_end_boundary = false;
        }
        if hit.start == 0 {
            any_start_zero = true;
        }
        if hit.tier > max_tier {
            max_tier = hit.tier;
        }
        earliest_start = earliest_start.min(hit.start);
        if !hit.exact_case {
            all_exact_case = false;
        }
    }

    let (tier, sub_tier) = if all_start_boundary {
        if max_tier == MatchTier::FullExact {
            (MatchTier::FullExact, 0)
        } else if max_tier == MatchTier::StemExact {
            (MatchTier::StemExact, 0)
        } else if any_start_zero {
            let sub = if full_stem_covered {
                2
            } else if all_end_boundary {
                1
            } else {
                0
            };
            (MatchTier::Prefix, sub)
        } else {
            let sub = if full_stem_covered {
                2
            } else if all_end_boundary {
                1
            } else {
                0
            };
            (MatchTier::WordBoundary, sub)
        }
    } else if any_boundary {
        (MatchTier::WordBoundary, 0)
    } else {
        (MatchTier::Substring, 0)
    };

    (
        tier,
        sub_tier,
        earliest_start,
        all_exact_case,
        boundary_count.saturating_sub(1),
    )
}

fn score_ascii_name(
    orig_name: &str,
    keywords: &[String],
    raw_keywords: &[String],
) -> Option<BaseNameScore> {
    let name_bytes = orig_name.as_bytes();
    for kw in keywords {
        if !crate::string_pool::ascii_contains_ignore_case(name_bytes, kw.as_bytes()) {
            return None;
        }
    }

    let (tier, sub_tier, first_match_byte, exact_case, extra_boundary_count) =
        if keywords.len() == 1 {
            let raw_kw0 = raw_keywords.first().map_or("", String::as_str);
            let hit = eval_ascii_keyword(orig_name, &keywords[0], raw_kw0)?;
            (hit.tier, hit.sub_tier, hit.start, hit.exact_case, 0usize)
        } else {
            let mut stack_hits = [KeywordHit {
                tier: MatchTier::Substring,
                sub_tier: 0,
                start: 0,
                end: 0,
                exact_case: false,
                is_start_boundary: false,
                is_end_boundary: false,
            }; 8];
            let mut stack_spans = [(0usize, 0usize); 8];
            if keywords.len() <= 8 {
                for (i, kw) in keywords.iter().enumerate() {
                    let raw_kw = raw_keywords.get(i).map_or("", String::as_str);
                    let hit = eval_ascii_keyword(orig_name, kw, raw_kw)?;
                    stack_hits[i] = hit;
                    stack_spans[i] = (hit.start, hit.end);
                }
                let hits = &stack_hits[..keywords.len()];
                let all_boundary = hits.iter().all(|h| h.is_start_boundary);
                let full_stem = all_boundary
                    && spans_cover_stem_ascii(orig_name, &stack_spans[..keywords.len()]);
                aggregate_multi_keyword_hits(hits, full_stem)
            } else {
                let mut hits = Vec::with_capacity(keywords.len());
                let mut spans = Vec::with_capacity(keywords.len());
                for (i, kw) in keywords.iter().enumerate() {
                    let raw_kw = raw_keywords.get(i).map_or("", String::as_str);
                    let hit = eval_ascii_keyword(orig_name, kw, raw_kw)?;
                    spans.push((hit.start, hit.end));
                    hits.push(hit);
                }
                let all_boundary = hits.iter().all(|h| h.is_start_boundary);
                let full_stem = all_boundary && spans_cover_stem_ascii(orig_name, &spans);
                aggregate_multi_keyword_hits(&hits, full_stem)
            }
        };

    let name_sub = len_penalty(orig_name)
        + if exact_case { EXACT_CASE_BONUS } else { 0 }
        + early_match_bonus(first_match_byte)
        + (extra_boundary_count.min(5) as i64) * 100;

    Some(BaseNameScore {
        tier,
        sub_tier,
        name_sub,
        is_app_bundle_name: is_app_bundle_name(orig_name),
        is_name_hidden: orig_name.starts_with('.'),
    })
}

fn score_general_name(
    orig_name: &str,
    folded_opt: Option<&str>,
    keywords: &[String],
    raw_keywords: &[String],
    all_keywords_ascii: bool,
    case_sensitive: bool,
    explicit_sensitive_lock: bool,
) -> Option<BaseNameScore> {
    if orig_name.is_ascii() && !all_keywords_ascii {
        return None;
    }

    let haystack: Cow<'_, str> = if case_sensitive {
        if orig_name.is_ascii() {
            Cow::Borrowed(orig_name)
        } else {
            cjk::normalize_and_simplify_cow(orig_name)
        }
    } else if let Some(folded) = folded_opt {
        Cow::Borrowed(folded)
    } else {
        Cow::Borrowed(orig_name)
    };

    let all_literal_match = keywords
        .iter()
        .all(|kw| haystack.contains(kw.as_str()));

    if !all_literal_match {
        // 拼音降级兜底：当名字含非 ASCII 字符、查询中至少包含一个 ASCII 字母（支持纯拼音及汉字+拼音混合），
        // 且未被显式敏感锁强制屏蔽时启用。
        let has_ascii_letter = keywords
            .iter()
            .any(|k| k.bytes().any(|b| b.is_ascii_alphabetic()));
        let allow_pinyin = (!case_sensitive || !explicit_sensitive_lock)
            && has_ascii_letter
            && !orig_name.is_ascii();
        if allow_pinyin {
            if let Some(detail) = score_pinyin_detail(orig_name, keywords) {
                let (tier, sub_tier) = match detail.tier {
                    PinyinMatchTier::PrefixFull => (MatchTier::PinyinStrong, 1),
                    PinyinMatchTier::PrefixMixed => (MatchTier::PinyinStrong, 0),
                    PinyinMatchTier::SubstringFull => (MatchTier::PinyinWeak, 3),
                    PinyinMatchTier::SubstringMixed => (MatchTier::PinyinWeak, 2),
                    PinyinMatchTier::PrefixInitials => (MatchTier::PinyinWeak, 1),
                    PinyinMatchTier::SubstringInitials => (MatchTier::PinyinWeak, 0),
                };
                return Some(BaseNameScore {
                    tier,
                    sub_tier,
                    name_sub: len_penalty(orig_name)
                        + early_match_bonus(detail.first_match_byte_start),
                    is_app_bundle_name: is_app_bundle_name(orig_name),
                    is_name_hidden: orig_name.starts_with('.'),
                });
            }
        }
        return None;
    }

    // 确认字面全部命中后，仅当折叠串确实丢失过大小写信息时，才懒构造大小写保留串用于驼峰与 NFC 原貌校验
    let case_preserved: Cow<'_, str> = if case_sensitive || folded_opt.is_none() {
        Cow::Borrowed(haystack.as_ref())
    } else {
        cjk::normalize_and_simplify_cow(orig_name)
    };
    let orig_nfc: Cow<'_, str> = cjk::normalize_nfc_cow(orig_name);
    let hs = haystack.as_ref();
    let cp = case_preserved.as_ref();

    let (tier, sub_tier, first_match_byte, exact_case, extra_boundary_count) =
        if keywords.len() == 1 {
            let raw_kw0 = raw_keywords.first().map_or("", String::as_str);
            let hit = eval_general_keyword(hs, cp, orig_nfc.as_ref(), &keywords[0], raw_kw0)?;
            (hit.tier, hit.sub_tier, hit.start, hit.exact_case, 0usize)
        } else {
            let mut stack_hits = [KeywordHit {
                tier: MatchTier::Substring,
                sub_tier: 0,
                start: 0,
                end: 0,
                exact_case: false,
                is_start_boundary: false,
                is_end_boundary: false,
            }; 8];
            let mut stack_spans = [(0usize, 0usize); 8];
            if keywords.len() <= 8 {
                for (i, kw) in keywords.iter().enumerate() {
                    let raw_kw = raw_keywords.get(i).map_or("", String::as_str);
                    let hit = eval_general_keyword(hs, cp, orig_nfc.as_ref(), kw, raw_kw)?;
                    stack_hits[i] = hit;
                    stack_spans[i] = (hit.start, hit.end);
                }
                let hits = &stack_hits[..keywords.len()];
                let all_boundary = hits.iter().all(|h| h.is_start_boundary);
                let full_stem = all_boundary
                    && spans_cover_stem_unicode(hs, &stack_spans[..keywords.len()]);
                aggregate_multi_keyword_hits(hits, full_stem)
            } else {
                let mut hits = Vec::with_capacity(keywords.len());
                let mut spans = Vec::with_capacity(keywords.len());
                for (i, kw) in keywords.iter().enumerate() {
                    let raw_kw = raw_keywords.get(i).map_or("", String::as_str);
                    let hit = eval_general_keyword(hs, cp, orig_nfc.as_ref(), kw, raw_kw)?;
                    spans.push((hit.start, hit.end));
                    hits.push(hit);
                }
                let all_boundary = hits.iter().all(|h| h.is_start_boundary);
                let full_stem = all_boundary && spans_cover_stem_unicode(hs, &spans);
                aggregate_multi_keyword_hits(&hits, full_stem)
            }
        };

    let name_sub = len_penalty(orig_name)
        + if exact_case { EXACT_CASE_BONUS } else { 0 }
        + early_match_bonus(first_match_byte)
        + (extra_boundary_count.min(5) as i64) * 100;

    Some(BaseNameScore {
        tier,
        sub_tier,
        name_sub,
        is_app_bundle_name: is_app_bundle_name(orig_name),
        is_name_hidden: orig_name.starts_with('.'),
    })
}

/// 将归一化（或折叠）串中的字节区间映射回等长或源字符串，优先走 O(1) 同偏移快路径。
#[inline]
fn map_span(target: &str, norm: &str, norm_start: usize, norm_end: usize) -> (usize, usize) {
    if target.len() == norm.len()
        && target.is_char_boundary(norm_start)
        && target.is_char_boundary(norm_end)
    {
        return (norm_start, norm_end);
    }
    super::map_norm_span_to_orig(target, norm, norm_start, norm_end).unwrap_or_else(|| {
        (
            norm_start.min(target.len()),
            norm_end.min(target.len()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score_kw(name: &str, query: &str) -> BaseNameScore {
        let folded = crate::cjk::fold_cow(name);
        let folded_opt = if folded.as_ref() == name {
            None
        } else {
            Some(folded.as_ref())
        };
        let keywords: Vec<String> = query.split_whitespace().map(crate::cjk::fold_str).collect();
        let raw_keywords: Vec<String> = query
            .split_whitespace()
            .map(|s| crate::cjk::normalize_nfc_cow(s).into_owned())
            .collect();
        let all_ascii = keywords.iter().all(|k| k.is_ascii());
        score_name_rank(name, folded_opt, &keywords, &raw_keywords, all_ascii, false, false)
            .expect("expected match")
    }

    #[test]
    fn word_boundary_covers_all_cjk_punctuation_and_camelcase_rules() {
        // 全部中文标点分隔符均视为强词边界（前后均是标点或末尾 -> sub_tier = 1）
        let cjk_cases = [
            ("【通知】会议.txt", "通知"),
            ("通知】附件", "通知"),
            ("工作（总结）", "总结"),
            ("工作—总结.txt", "总结"),
            ("一、总结", "总结"),
            ("项目：总结", "总结"),
            ("《算法导论》", "算法导论"),
            ("“重要”文件", "重要"),
            ("文档·备份", "备份"),
            ("「草稿」", "草稿"),
            ("『归档』", "归档"),
            ("〔修订〕", "修订"),
            ("第一，报告", "报告"),
            ("第一章。结论", "结论"),
            ("总览；明细", "明细"),
            ("警告！处理", "处理"),
            ("疑问？解答", "解答"),
            ("区间～摘要", "摘要"),
            ("版本－说明", "说明"),
            ("项目＿笔记", "笔记"),
            ("目录／清单", "清单"),
        ];
        for (name, kw) in cjk_cases {
            let s = score_kw(name, kw);
            assert!(
                s.tier >= MatchTier::WordBoundary,
                "{name:?} matching {kw:?} should be WordBoundary or higher, got {:?}",
                s.tier
            );
            if s.tier == MatchTier::WordBoundary {
                assert_eq!(s.sub_tier, 1, "{name:?} matching {kw:?} should have end boundary");
            }
        }

        let non_end_punct = score_kw("工作—总结报告", "总结");
        assert_eq!((non_end_punct.tier, non_end_punct.sub_tier), (MatchTier::WordBoundary, 0));

        // 驼峰、全大写缩写、数字-字母与中英交界
        assert_eq!((score_kw("getUserName", "user").tier, score_kw("getUserName", "user").sub_tier), (MatchTier::WordBoundary, 1));
        assert_eq!((score_kw("HTMLParser", "parser").tier, score_kw("HTMLParser", "parser").sub_tier), (MatchTier::WordBoundary, 1));
        assert_eq!((score_kw("HTMLParser", "html").tier, score_kw("HTMLParser", "html").sub_tier), (MatchTier::Prefix, 1));
        assert_eq!((score_kw("v2ray", "v2").tier, score_kw("v2ray", "v2").sub_tier), (MatchTier::Prefix, 1));
        assert_eq!((score_kw("v2ray", "ray").tier, score_kw("v2ray", "ray").sub_tier), (MatchTier::WordBoundary, 1));
        assert_eq!((score_kw("中文config", "config").tier, score_kw("中文config", "config").sub_tier), (MatchTier::WordBoundary, 1));
    }

    #[test]
    fn word_boundary_scans_all_occurrences_and_prefers_strongest_match() {
        let non_first = score_kw("fooconfig_config.toml", "config");
        assert_eq!(non_first.tier, MatchTier::WordBoundary);
        assert_eq!(non_first.sub_tier, 1);

        let partial = score_kw("my_configurator", "config");
        assert_eq!(partial.tier, MatchTier::WordBoundary);
        assert_eq!(partial.sub_tier, 0);
        assert!(non_first.sub_tier > partial.sub_tier);
    }

    #[test]
    fn stem_exact_excludes_empty_or_whitespace_extension() {
        assert_eq!(score_kw("README.md", "readme").tier, MatchTier::StemExact);
        assert_eq!(score_kw("foo.tar.gz", "foo.tar").tier, MatchTier::StemExact);
        assert_eq!(score_kw("v1.0 final", "v1").tier, MatchTier::Prefix);
        assert_eq!(score_kw("foo.", "foo").tier, MatchTier::Prefix);
    }

    #[test]
    fn pinyin_tiers_preserve_expected_hierarchy() {
        let pf = score_kw("文件说明.txt", "wenjian");
        let pm = score_kw("文件说明.txt", "wenj");
        let sf = score_kw("说明文件.txt", "wenjian");
        let sm = score_kw("说明文件.txt", "wenj");
        let pi = score_kw("文件说明.txt", "wj");
        let si = score_kw("说明文件.txt", "wj");

        assert_eq!((pf.tier, pf.sub_tier), (MatchTier::PinyinStrong, 1));
        assert_eq!((pm.tier, pm.sub_tier), (MatchTier::PinyinStrong, 0));
        assert_eq!((sf.tier, sf.sub_tier), (MatchTier::PinyinWeak, 3));
        assert_eq!((sm.tier, sm.sub_tier), (MatchTier::PinyinWeak, 2));
        assert_eq!((pi.tier, pi.sub_tier), (MatchTier::PinyinWeak, 1));
        assert_eq!((si.tier, si.sub_tier), (MatchTier::PinyinWeak, 0));
    }

    #[test]
    fn rank_score_to_f64_is_strictly_monotonic_across_all_tiers() {
        let ctx = RecordRankContext::default();
        let samples = [
            RankScore::assemble(MatchTier::FullExact, 0, 10_000, false),
            RankScore::assemble(MatchTier::FullExact, 0, -10_000, false),
            RankScore::assemble(MatchTier::StemExact, 0, 5_000, false),
            RankScore::assemble(MatchTier::Prefix, 1, 0, false),
            RankScore::assemble(MatchTier::Prefix, 0, 0, false),
            RankScore::assemble(MatchTier::WordBoundary, 1, 0, false),
            RankScore::assemble(MatchTier::WordBoundary, 0, 0, false),
            RankScore::assemble(MatchTier::Substring, 0, 100, false),
            RankScore::assemble(MatchTier::FullExact, 0, 0, true), // hidden FullExact (~4.04M)
            RankScore::assemble(MatchTier::PinyinStrong, 1, 0, false),
            RankScore::assemble(MatchTier::PinyinWeak, 3, 0, false),
            RankScore::assemble(MatchTier::PinyinWeak, 0, 0, false),
            RankScore::assemble(MatchTier::Fuzzy, 0, 20_000, false),
            RankScore::assemble(MatchTier::Fuzzy, 0, 0, false),
            RankScore::assemble(MatchTier::Fuzzy, 0, -20_000, true),
        ];
        for pair in samples.windows(2) {
            assert!(pair[0] > pair[1], "{:?} should > {:?}", pair[0], pair[1]);
            assert!(
                pair[0].to_f64() > pair[1].to_f64(),
                "{} should > {} in f64",
                pair[0].to_f64(),
                pair[1].to_f64()
            );
            assert!((1e-6..=1.0).contains(&pair[0].to_f64()));
            assert!((1e-6..=1.0).contains(&pair[1].to_f64()));
        }
        // 验证 upper_bound 始终大于等于真实记录分
        let base = score_kw("Safari.app", "safari");
        let ub = base.upper_bound(&ctx, true, false);
        let actual = score_record(base, true, "/Applications", &ctx, false);
        assert!(actual <= ub);
    }

    #[test]
    fn to_match_score_maps_tiers_consistently() {
        assert_eq!(RankScore::assemble(MatchTier::FullExact, 0, 0, false).to_match_score(), MatchScore::Exact);
        assert_eq!(RankScore::assemble(MatchTier::StemExact, 0, 0, false).to_match_score(), MatchScore::Prefix);
        assert_eq!(RankScore::assemble(MatchTier::Prefix, 1, 0, false).to_match_score(), MatchScore::Prefix);
        assert_eq!(RankScore::assemble(MatchTier::WordBoundary, 1, 0, false).to_match_score(), MatchScore::Substring);
        assert_eq!(RankScore::assemble(MatchTier::Substring, 0, 0, false).to_match_score(), MatchScore::Substring);
        assert_eq!(RankScore::assemble(MatchTier::PinyinStrong, 1, 0, false).to_match_score(), MatchScore::Substring);
        assert_eq!(RankScore::assemble(MatchTier::FullExact, 0, 0, true).to_match_score(), MatchScore::Substring);
    }
}
