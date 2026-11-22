use std::cmp::Reverse;

use crate::indexer::MatchScore;
use crate::pinyin_data;

const STATE_UNREACHABLE: u8 = 0;
const STATE_SINGLE_INITIAL: u8 = 1;
const STATE_MIXED: u8 = 2;
const STATE_TRAILING_PREFIX: u8 = 3;
const STATE_ALL_FULL: u8 = 4;
const STATE_START: u8 = 5;

const STACK_DP_CAP: usize = 65;

/// 拼音匹配的精细分层等级（按排序权重升序排列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PinyinMatchTier {
    SubstringInitials,
    PrefixInitials,
    SubstringMixed,
    SubstringFull,
    PrefixMixed,
    PrefixFull,
}

/// 拼音动态规划匹配的结构化结果，供打分排序与高亮定位共享。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PinyinMatchDetail {
    pub tier: PinyinMatchTier,
    pub is_prefix: bool,
    pub first_match_byte_start: usize,
}

#[derive(Clone, Copy)]
enum SyllableMatchKind {
    SingleInitial,
    MultiPrefix,
    Full,
}

enum Token {
    Cjk {
        simp_char: char,
        variants: &'static [&'static str],
        byte_start: usize,
        byte_end: usize,
    },
    AsciiLiteral {
        byte_lower: u8,
        byte_start: usize,
        byte_end: usize,
    },
    NonAsciiLiteral {
        ch: char,
        byte_start: usize,
        byte_end: usize,
    },
}

impl Token {
    #[inline]
    fn byte_start(&self) -> usize {
        match *self {
            Self::Cjk { byte_start, .. }
            | Self::AsciiLiteral { byte_start, .. }
            | Self::NonAsciiLiteral { byte_start, .. } => byte_start,
        }
    }

    #[inline]
    fn byte_end(&self) -> usize {
        match *self {
            Self::Cjk { byte_end, .. }
            | Self::AsciiLiteral { byte_end, .. }
            | Self::NonAsciiLiteral { byte_end, .. } => byte_end,
        }
    }
}

#[derive(Clone, Copy)]
struct DpKeywordMatch {
    state: u8,
    start_tok: usize,
    end_tok: usize,
    byte_start: usize,
    byte_end: usize,
}

impl DpKeywordMatch {
    #[inline]
    fn rank_key(&self) -> (bool, u8, u8, Reverse<usize>, Reverse<usize>) {
        let consumes_cjk = self.state != STATE_START;
        let is_prefix = self.start_tok == 0;
        let tier_rank = if consumes_cjk {
            state_to_tier(self.state, is_prefix) as u8
        } else {
            u8::from(is_prefix)
        };
        (
            consumes_cjk,
            tier_rank,
            self.state,
            Reverse(self.start_tok),
            Reverse(self.end_tok - self.start_tok),
        )
    }
}

struct DpScratch {
    cur: Vec<u8>,
    next: Vec<u8>,
    kw_lower: Vec<u8>,
}

impl DpScratch {
    fn new() -> Self {
        Self {
            cur: Vec::new(),
            next: Vec::new(),
            kw_lower: Vec::new(),
        }
    }
}

/// 线程局部拼音分词与 DP 复用缓冲区：消除高频检索时对每个非 ASCII 文件名重复在堆上分配 `Vec<Token>`。
struct PinyinScratch {
    tokens: Vec<Token>,
    dp: DpScratch,
    kw_chars: Vec<char>,
}

impl PinyinScratch {
    fn new() -> Self {
        Self {
            tokens: Vec::with_capacity(64),
            dp: DpScratch::new(),
            kw_chars: Vec::with_capacity(32),
        }
    }
}

thread_local! {
    static PINYIN_SCRATCH: std::cell::RefCell<PinyinScratch> =
        std::cell::RefCell::new(PinyinScratch::new());
}

/// 返回单个汉字的全部小写 ASCII 拼音读音切片（首项为主读音）。
///
/// 先通过 `to_simplified_char` 统一繁简编码，确保繁体与简体文件名产出完全一致的读音集合；
/// 同步过滤 `PINYIN_SYLLABLES` 中少数非小写 ASCII 的占位条目，避免非合法音节进入 DP 引擎。
#[inline]
pub fn char_pinyin_variants(c: char) -> &'static [&'static str] {
    let simp = crate::cjk::to_simplified_char(c);
    if let Ok(idx) = pinyin_data::POLYPHONE_TABLE.binary_search_by_key(&simp, |&(ch, _)| ch) {
        return pinyin_data::POLYPHONE_TABLE[idx].1;
    }
    let u = simp as u32;
    if (pinyin_data::CJK_START..=pinyin_data::CJK_END).contains(&u) {
        let idx = pinyin_data::PINYIN_TABLE[(u - pinyin_data::CJK_START) as usize] as usize;
        let syllable = pinyin_data::PINYIN_SYLLABLES[idx];
        if !syllable.is_empty() && syllable.bytes().all(|b| b.is_ascii_lowercase()) {
            return &pinyin_data::PINYIN_SYLLABLES[idx..=idx];
        }
    }
    &[]
}

/// 返回单个汉字的主拼音音节与首字母。
#[inline]
pub fn char_pinyin(c: char) -> Option<(&'static str, char)> {
    let primary = *char_pinyin_variants(c).first()?;
    let initial = primary.chars().next()?;
    Some((primary, initial))
}

/// 判断音节中位置 1 的 `'u'` 是否对应韵母 `ü`（允许与查询中的 `'v'` 等价匹配）。
///
/// 仅放行 `lu`、`lue`、`nu`、`nue` 与 `ju`/`qu`/`xu`/`yu` 开头的音节，
/// 严格排除 `luan`/`luo`/`lun`、`nuan`/`nuo`/`nun`、`liu`/`lou` 等非 `ü` 音节误召回。
#[inline]
pub fn is_v_u_syllable(syl: &str) -> bool {
    matches!(syl, "lu" | "lue" | "nu" | "nue")
        || syl.starts_with("ju")
        || syl.starts_with("qu")
        || syl.starts_with("xu")
        || syl.starts_with("yu")
}

/// 快速判定文件名是否包含 CJK 统一表意文字或繁体汉字。
#[inline]
pub fn has_cjk_characters(name: &str) -> bool {
    name.chars().any(|c| {
        let u = c as u32;
        (pinyin_data::CJK_START..=pinyin_data::CJK_END).contains(&u)
            || (0x3400..=0x4DBF).contains(&u)
            || crate::cjk::is_trad_char(c)
    })
}

#[inline]
fn state_to_tier(state: u8, is_prefix: bool) -> PinyinMatchTier {
    match (is_prefix, state) {
        (true, STATE_ALL_FULL) => PinyinMatchTier::PrefixFull,
        (true, STATE_SINGLE_INITIAL) => PinyinMatchTier::PrefixInitials,
        (true, _) => PinyinMatchTier::PrefixMixed,
        (false, STATE_ALL_FULL) => PinyinMatchTier::SubstringFull,
        (false, STATE_SINGLE_INITIAL) => PinyinMatchTier::SubstringInitials,
        (false, _) => PinyinMatchTier::SubstringMixed,
    }
}

#[inline]
fn transition_state(prev: u8, kind: SyllableMatchKind) -> u8 {
    match prev {
        STATE_START => match kind {
            SyllableMatchKind::Full => STATE_ALL_FULL,
            SyllableMatchKind::MultiPrefix => STATE_TRAILING_PREFIX,
            SyllableMatchKind::SingleInitial => STATE_SINGLE_INITIAL,
        },
        STATE_ALL_FULL => match kind {
            SyllableMatchKind::Full => STATE_ALL_FULL,
            SyllableMatchKind::MultiPrefix | SyllableMatchKind::SingleInitial => {
                STATE_TRAILING_PREFIX
            }
        },
        STATE_SINGLE_INITIAL => match kind {
            SyllableMatchKind::SingleInitial => STATE_SINGLE_INITIAL,
            SyllableMatchKind::MultiPrefix | SyllableMatchKind::Full => STATE_MIXED,
        },
        STATE_TRAILING_PREFIX | STATE_MIXED => STATE_MIXED,
        _ => STATE_UNREACHABLE,
    }
}

fn tokenize_into(name: &str, tokens: &mut Vec<Token>) -> bool {
    tokens.clear();
    if name.is_ascii() {
        return false;
    }
    let mut has_cjk = false;
    for (byte_start, ch) in name.char_indices() {
        let byte_end = byte_start + ch.len_utf8();
        if ch.is_ascii() {
            tokens.push(Token::AsciiLiteral {
                byte_lower: (ch as u8).to_ascii_lowercase(),
                byte_start,
                byte_end,
            });
        } else {
            let simp = crate::cjk::to_simplified_char(ch);
            let variants = char_pinyin_variants(simp);
            if !variants.is_empty() {
                has_cjk = true;
                tokens.push(Token::Cjk {
                    simp_char: simp,
                    variants,
                    byte_start,
                    byte_end,
                });
            } else {
                let folded = simp.to_lowercase().next().unwrap_or(simp);
                tokens.push(Token::NonAsciiLiteral {
                    ch: folded,
                    byte_start,
                    byte_end,
                });
            }
        }
    }
    has_cjk
}

#[inline]
fn can_start_at_ascii(token: &Token, first_lower: u8) -> bool {
    match token {
        Token::AsciiLiteral { byte_lower, .. } => *byte_lower == first_lower,
        Token::Cjk { variants, .. } => variants
            .iter()
            .any(|syl| syl.as_bytes().first() == Some(&first_lower)),
        Token::NonAsciiLiteral { .. } => false,
    }
}

#[inline]
fn can_start_at_char(token: &Token, first_ch: char) -> bool {
    match token {
        Token::AsciiLiteral { byte_lower, .. } => first_ch == (*byte_lower as char),
        Token::NonAsciiLiteral { ch, .. } => first_ch == *ch,
        Token::Cjk {
            simp_char,
            variants,
            ..
        } => {
            first_ch == *simp_char
                || (first_ch.is_ascii_lowercase()
                    && variants
                        .iter()
                        .any(|syl| syl.as_bytes().first() == Some(&(first_ch as u8))))
        }
    }
}

fn dp_match_keyword(
    tokens: &[Token],
    kw: &str,
    scratch: &mut DpScratch,
    kw_chars_buf: &mut Vec<char>,
) -> Option<DpKeywordMatch> {
    if kw.is_ascii() {
        dp_match_keyword_ascii(tokens, kw, scratch)
    } else {
        dp_match_keyword_unicode(tokens, kw, scratch, kw_chars_buf)
    }
}

fn dp_match_keyword_ascii(
    tokens: &[Token],
    kw: &str,
    scratch: &mut DpScratch,
) -> Option<DpKeywordMatch> {
    let kw_len = kw.len();
    if kw_len == 0 || tokens.is_empty() {
        return None;
    }

    let mut stack_kw = [0u8; STACK_DP_CAP];
    let mut stack_cur = [0u8; STACK_DP_CAP];
    let mut stack_next = [0u8; STACK_DP_CAP];

    let dp_len = kw_len + 1;
    let (kw_lower, cur_dp, next_dp): (&[u8], &mut [u8], &mut [u8]) = if dp_len <= STACK_DP_CAP {
        for (dst, &b) in stack_kw[..kw_len].iter_mut().zip(kw.as_bytes()) {
            *dst = b.to_ascii_lowercase();
        }
        (&stack_kw[..kw_len], &mut stack_cur[..dp_len], &mut stack_next[..dp_len])
    } else {
        scratch.kw_lower.clear();
        scratch
            .kw_lower
            .extend(kw.as_bytes().iter().map(|b| b.to_ascii_lowercase()));
        scratch.cur.resize(dp_len, 0);
        scratch.next.resize(dp_len, 0);
        (
            scratch.kw_lower.as_slice(),
            &mut scratch.cur[..dp_len],
            &mut scratch.next[..dp_len],
        )
    };

    let first_lower = kw_lower[0];
    let mut best: Option<DpKeywordMatch> = None;

    for start_tok in 0..tokens.len() {
        // 单个 token 最多消费 6 字节全拼（如 zhuang/shuang/chuang），剩余 token 不足时提前中止
        if (tokens.len() - start_tok).saturating_mul(6) < kw_len {
            break;
        }
        if !can_start_at_ascii(&tokens[start_tok], first_lower) {
            continue;
        }

        cur_dp.fill(STATE_UNREACHABLE);
        cur_dp[0] = STATE_START;
        let mut active_lo = 0usize;
        let mut active_hi = 0usize;

        for (offset, token) in tokens[start_tok..].iter().enumerate() {
            let tok_idx = start_tok + offset;
            if matches!(token, Token::NonAsciiLiteral { .. }) {
                break;
            }

            next_dp.fill(STATE_UNREACHABLE);
            let mut next_lo = usize::MAX;
            let mut next_hi = 0usize;

            match token {
                Token::AsciiLiteral { byte_lower, .. } => {
                    for j in active_lo..=active_hi {
                        let st = cur_dp[j];
                        if st == STATE_UNREACHABLE || *byte_lower != kw_lower[j] {
                            continue;
                        }
                        let nj = j + 1;
                        if st > next_dp[nj] {
                            next_dp[nj] = st;
                        }
                        next_lo = next_lo.min(nj);
                        next_hi = next_hi.max(nj);
                    }
                }
                Token::Cjk { variants, .. } => {
                    for j in active_lo..=active_hi {
                        let st = cur_dp[j];
                        if st == STATE_UNREACHABLE {
                            continue;
                        }
                        let remaining = kw_len - j;
                        let target_first = kw_lower[j];
                        for &syl in *variants {
                            let syl_bytes = syl.as_bytes();
                            if syl_bytes[0] != target_first {
                                continue;
                            }
                            let allow_vu = is_v_u_syllable(syl);
                            let max_k = syl_bytes.len().min(remaining);

                            let kind1 = if syl_bytes.len() == 1 {
                                SyllableMatchKind::Full
                            } else {
                                SyllableMatchKind::SingleInitial
                            };
                            let st1 = transition_state(st, kind1);
                            let nj1 = j + 1;
                            if st1 > next_dp[nj1] {
                                next_dp[nj1] = st1;
                            }
                            next_lo = next_lo.min(nj1);
                            next_hi = next_hi.max(nj1);

                            for k in 2..=max_k {
                                let idx = k - 1;
                                let sb = syl_bytes[idx];
                                let kb = kw_lower[j + idx];
                                let byte_ok =
                                    sb == kb || (allow_vu && idx == 1 && sb == b'u' && kb == b'v');
                                if !byte_ok {
                                    break;
                                }
                                let kind = if k == syl_bytes.len() {
                                    SyllableMatchKind::Full
                                } else {
                                    SyllableMatchKind::MultiPrefix
                                };
                                let stk = transition_state(st, kind);
                                let nj = j + k;
                                if stk > next_dp[nj] {
                                    next_dp[nj] = stk;
                                }
                                next_lo = next_lo.min(nj);
                                next_hi = next_hi.max(nj);
                            }
                        }
                    }
                }
                Token::NonAsciiLiteral { .. } => unreachable!(),
            }

            let end_state = next_dp[kw_len];
            if end_state != STATE_UNREACHABLE {
                let candidate = DpKeywordMatch {
                    state: end_state,
                    start_tok,
                    end_tok: tok_idx + 1,
                    byte_start: tokens[start_tok].byte_start(),
                    byte_end: token.byte_end(),
                };
                if best.is_none_or(|prev| candidate.rank_key() > prev.rank_key()) {
                    best = Some(candidate);
                }
            }

            if next_lo >= kw_len {
                break;
            }
            cur_dp.copy_from_slice(next_dp);
            active_lo = next_lo;
            active_hi = next_hi.min(kw_len - 1);
        }
    }

    best
}

/// 支持汉字与拼音混合推进的字符级动态规划（如单词 `"工作hb"` 或多词中的 `"2026年"`、`"财务"`）：
/// 汉字字面命中保持状态不变（不置为拼音态），仅 ASCII 字母命中 CJK 音节时才推进拼音状态，
/// 保证纯汉字查询不会误入拼音通道。
fn dp_match_keyword_unicode(
    tokens: &[Token],
    kw: &str,
    scratch: &mut DpScratch,
    kw_chars_buf: &mut Vec<char>,
) -> Option<DpKeywordMatch> {
    if kw.is_empty() || tokens.is_empty() {
        return None;
    }

    let kw_nfc = crate::cjk::normalize_nfc_cow(kw);
    kw_chars_buf.clear();
    for ch in kw_nfc.chars() {
        if ch.is_ascii() {
            kw_chars_buf.push(ch.to_ascii_lowercase());
        } else {
            let simp = crate::cjk::to_simplified_char(ch);
            for lc in simp.to_lowercase() {
                kw_chars_buf.push(lc);
            }
        }
    }
    let kw_chars = kw_chars_buf.as_slice();
    let kw_len = kw_chars.len();
    if kw_len == 0 {
        return None;
    }

    let mut stack_cur = [0u8; STACK_DP_CAP];
    let mut stack_next = [0u8; STACK_DP_CAP];
    let dp_len = kw_len + 1;
    let (cur_dp, next_dp): (&mut [u8], &mut [u8]) = if dp_len <= STACK_DP_CAP {
        (&mut stack_cur[..dp_len], &mut stack_next[..dp_len])
    } else {
        scratch.cur.resize(dp_len, 0);
        scratch.next.resize(dp_len, 0);
        (&mut scratch.cur[..dp_len], &mut scratch.next[..dp_len])
    };

    let first_ch = kw_chars[0];
    let mut best: Option<DpKeywordMatch> = None;

    for start_tok in 0..tokens.len() {
        if (tokens.len() - start_tok).saturating_mul(6) < kw_len {
            break;
        }
        if !can_start_at_char(&tokens[start_tok], first_ch) {
            continue;
        }

        cur_dp.fill(STATE_UNREACHABLE);
        cur_dp[0] = STATE_START;
        let mut active_lo = 0usize;
        let mut active_hi = 0usize;

        for (offset, token) in tokens[start_tok..].iter().enumerate() {
            let tok_idx = start_tok + offset;

            next_dp.fill(STATE_UNREACHABLE);
            let mut next_lo = usize::MAX;
            let mut next_hi = 0usize;

            match token {
                Token::AsciiLiteral { byte_lower, .. } => {
                    let tok_ch = *byte_lower as char;
                    for j in active_lo..=active_hi {
                        let st = cur_dp[j];
                        if st == STATE_UNREACHABLE || kw_chars[j] != tok_ch {
                            continue;
                        }
                        let nj = j + 1;
                        if st > next_dp[nj] {
                            next_dp[nj] = st;
                        }
                        next_lo = next_lo.min(nj);
                        next_hi = next_hi.max(nj);
                    }
                }
                Token::NonAsciiLiteral { ch, .. } => {
                    for j in active_lo..=active_hi {
                        let st = cur_dp[j];
                        if st == STATE_UNREACHABLE || kw_chars[j] != *ch {
                            continue;
                        }
                        let nj = j + 1;
                        if st > next_dp[nj] {
                            next_dp[nj] = st;
                        }
                        next_lo = next_lo.min(nj);
                        next_hi = next_hi.max(nj);
                    }
                }
                Token::Cjk {
                    simp_char,
                    variants,
                    ..
                } => {
                    for j in active_lo..=active_hi {
                        let st = cur_dp[j];
                        if st == STATE_UNREACHABLE {
                            continue;
                        }
                        let target_ch = kw_chars[j];
                        if target_ch == *simp_char {
                            let nj = j + 1;
                            if st > next_dp[nj] {
                                next_dp[nj] = st;
                            }
                            next_lo = next_lo.min(nj);
                            next_hi = next_hi.max(nj);
                        }
                        if !target_ch.is_ascii_lowercase() {
                            continue;
                        }
                        let remaining = kw_len - j;
                        let target_first = target_ch as u8;
                        for &syl in *variants {
                            let syl_bytes = syl.as_bytes();
                            if syl_bytes[0] != target_first {
                                continue;
                            }
                            let allow_vu = is_v_u_syllable(syl);
                            let max_k = syl_bytes.len().min(remaining);

                            let kind1 = if syl_bytes.len() == 1 {
                                SyllableMatchKind::Full
                            } else {
                                SyllableMatchKind::SingleInitial
                            };
                            let st1 = transition_state(st, kind1);
                            let nj1 = j + 1;
                            if st1 > next_dp[nj1] {
                                next_dp[nj1] = st1;
                            }
                            next_lo = next_lo.min(nj1);
                            next_hi = next_hi.max(nj1);

                            for k in 2..=max_k {
                                let idx = k - 1;
                                let kc = kw_chars[j + idx];
                                if !kc.is_ascii() {
                                    break;
                                }
                                let sb = syl_bytes[idx];
                                let kb = kc as u8;
                                let byte_ok =
                                    sb == kb || (allow_vu && idx == 1 && sb == b'u' && kb == b'v');
                                if !byte_ok {
                                    break;
                                }
                                let kind = if k == syl_bytes.len() {
                                    SyllableMatchKind::Full
                                } else {
                                    SyllableMatchKind::MultiPrefix
                                };
                                let stk = transition_state(st, kind);
                                let nj = j + k;
                                if stk > next_dp[nj] {
                                    next_dp[nj] = stk;
                                }
                                next_lo = next_lo.min(nj);
                                next_hi = next_hi.max(nj);
                            }
                        }
                    }
                }
            }

            let end_state = next_dp[kw_len];
            if end_state != STATE_UNREACHABLE {
                let candidate = DpKeywordMatch {
                    state: end_state,
                    start_tok,
                    end_tok: tok_idx + 1,
                    byte_start: tokens[start_tok].byte_start(),
                    byte_end: token.byte_end(),
                };
                if best.is_none_or(|prev| candidate.rank_key() > prev.rank_key()) {
                    best = Some(candidate);
                }
            }

            if next_lo >= kw_len {
                break;
            }
            cur_dp.copy_from_slice(next_dp);
            active_lo = next_lo;
            active_hi = next_hi.min(kw_len - 1);
        }
    }

    best
}

/// 音节前缀动态规划拼音打分，返回精细的匹配层级与起始位置特征。
///
/// 支持纯 ASCII 拼音、多词汉字+拼音混合（如 `"2026年 cwysb"`、`"财务 ysb"`）以及单词内汉字+拼音混合（如 `"工作hb"`）；
/// 整体查询必须至少在一个词元内通过拼音音节命中至少 1 个 CJK 汉字（`any_cjk == true`），
/// 确保纯字面 ASCII 或纯汉字查询不会误走拼音通道。
pub(crate) fn score_pinyin_detail(name: &str, keywords: &[String]) -> Option<PinyinMatchDetail> {
    if keywords.is_empty()
        || name.is_ascii()
        || !keywords
            .iter()
            .any(|k| k.bytes().any(|b| b.is_ascii_alphabetic()))
    {
        return None;
    }

    PINYIN_SCRATCH.with(|cell| {
        let mut scratch = cell.borrow_mut();
        let PinyinScratch {
            tokens,
            dp,
            kw_chars,
        } = &mut *scratch;
        if !tokenize_into(name, tokens) {
            return None;
        }

        let mut any_non_empty = false;
        let mut any_cjk = false;
        let mut any_prefix = false;
        let mut earliest_byte_start = usize::MAX;
        let mut all_cjk_full = true;
        let mut all_cjk_initials = true;

        for kw in keywords {
            if kw.is_empty() {
                continue;
            }
            any_non_empty = true;
            let matched = dp_match_keyword(tokens, kw.as_str(), dp, kw_chars)?;
            if matched.start_tok == 0 {
                any_prefix = true;
            }
            earliest_byte_start = earliest_byte_start.min(matched.byte_start);
            if matched.state != STATE_START {
                any_cjk = true;
                if matched.state != STATE_ALL_FULL {
                    all_cjk_full = false;
                }
                if matched.state != STATE_SINGLE_INITIAL {
                    all_cjk_initials = false;
                }
            }
        }

        if !any_non_empty || !any_cjk {
            return None;
        }

        let is_prefix = any_prefix;
        let tier = match (is_prefix, all_cjk_full, all_cjk_initials) {
            (true, true, _) => PinyinMatchTier::PrefixFull,
            (true, false, true) => PinyinMatchTier::PrefixInitials,
            (true, false, false) => PinyinMatchTier::PrefixMixed,
            (false, true, _) => PinyinMatchTier::SubstringFull,
            (false, false, true) => PinyinMatchTier::SubstringInitials,
            (false, false, false) => PinyinMatchTier::SubstringMixed,
        };

        Some(PinyinMatchDetail {
            tier,
            is_prefix,
            first_match_byte_start: earliest_byte_start,
        })
    })
}

/// 拼音转写打分，保持对既有上游打分调用方的 `MatchScore` 兼容契约。
pub(crate) fn score_pinyin_match(name: &str, keywords: &[String]) -> Option<MatchScore> {
    score_pinyin_detail(name, keywords).map(|detail| {
        if detail.is_prefix {
            MatchScore::Prefix
        } else {
            MatchScore::Substring
        }
    })
}

/// 仅对进入 Top-K 的命中结果计算拼音对应原始文件名的 UTF-8 字节区间，供界面高亮。
pub(crate) fn find_pinyin_positions<S: AsRef<str>>(name: &str, keywords: &[S]) -> Vec<(usize, usize)> {
    if keywords.is_empty()
        || name.is_ascii()
        || !keywords
            .iter()
            .any(|k| k.as_ref().bytes().any(|b| b.is_ascii_alphabetic()))
    {
        return Vec::new();
    }

    PINYIN_SCRATCH.with(|cell| {
        let mut scratch = cell.borrow_mut();
        let PinyinScratch {
            tokens,
            dp,
            kw_chars,
        } = &mut *scratch;
        if !tokenize_into(name, tokens) {
            return Vec::new();
        }

        let mut positions = Vec::new();
        let mut any_cjk = false;

        for kw in keywords {
            let kw_str = kw.as_ref();
            if kw_str.is_empty() {
                continue;
            }
            if let Some(matched) = dp_match_keyword(tokens, kw_str, dp, kw_chars) {
                if matched.state != STATE_START {
                    any_cjk = true;
                }
                positions.push((matched.byte_start, matched.byte_end));
            }
        }

        if !any_cjk {
            return Vec::new();
        }

        positions.sort_by_key(|&(s, e)| (s, e));
        positions.dedup();
        positions
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::string_pool::find_ascii_ignore_case;

    #[test]
    fn test_find_ascii_ignore_case() {
        assert_eq!(find_ascii_ignore_case(b"hello world", b""), Some(0));
        assert_eq!(find_ascii_ignore_case(b"hi", b"hello"), None);
        assert_eq!(find_ascii_ignore_case(b"gongzuohuibao", b"GONGZUO"), Some(0));
        assert_eq!(find_ascii_ignore_case(b"gongzuohuibao", b"Zuo"), Some(4));
        assert_eq!(find_ascii_ignore_case(b"gzhb", b"GZ"), Some(0));
        assert_eq!(find_ascii_ignore_case(b"gzhb", b"HB"), Some(2));
        assert_eq!(find_ascii_ignore_case(b"gzhb", b"xyz"), None);
    }

    #[test]
    fn test_score_pinyin_match_case_insensitivity() {
        let kw_upper = vec!["GZHB".to_string()];
        assert_eq!(
            score_pinyin_match("工作汇报.docx", &kw_upper),
            Some(MatchScore::Prefix)
        );

        let kw_mixed = vec!["GongZuo".to_string()];
        assert_eq!(
            score_pinyin_match("工作汇报.docx", &kw_mixed),
            Some(MatchScore::Prefix)
        );

        let kw_empty: Vec<String> = vec![];
        assert_eq!(score_pinyin_match("工作汇报.docx", &kw_empty), None);

        let kw_non_ascii = vec!["中文".to_string()];
        assert_eq!(score_pinyin_match("工作汇报.docx", &kw_non_ascii), None);
    }

    #[test]
    fn test_find_pinyin_positions_case_insensitivity() {
        let kw = vec!["GZHB".to_string()];
        let pos = find_pinyin_positions("工作汇报.docx", &kw);
        assert!(!pos.is_empty());
        // 工作汇报 四个汉字共 12 字节（每个汉字 3 字节）
        assert_eq!(pos, vec![(0, 12)]);
    }

    #[test]
    fn test_dp_syllable_matching_polyphone_and_vu_behavior() {
        // 混合简全拼
        assert_eq!(
            score_pinyin_match("截图表.xlsx", &["jietb".to_string()]),
            Some(MatchScore::Prefix)
        );
        assert_eq!(
            score_pinyin_match("2026年财务预算表.xlsx", &["2026ncw".to_string()]),
            Some(MatchScore::Prefix)
        );
        assert_eq!(
            score_pinyin_match("2026年财务预算表.xlsx", &["2026".to_string(), "cwysb".to_string()]),
            Some(MatchScore::Prefix)
        );

        // 音节边界防护与首选范围
        assert_eq!(score_pinyin_match("工作汇报.docx", &["ngzu".to_string()]), None);
        assert_eq!(score_pinyin_match("文件上传.zip", &["enjian".to_string()]), None);
        assert_eq!(score_pinyin_match("西安.txt", &["ian".to_string()]), None);
        assert_eq!(find_pinyin_positions("高奥.txt", &["ga"]), vec![(0, 3)]);

        // v/u 等价边界
        assert_eq!(
            score_pinyin_match("绿色.txt", &["lvse".to_string()]),
            Some(MatchScore::Prefix)
        );
        assert_eq!(
            score_pinyin_match("觉得.txt", &["jve".to_string()]),
            Some(MatchScore::Prefix)
        );
        assert_eq!(score_pinyin_match("乱码.txt", &["lv".to_string()]), None);

        // 多音字与繁简一致
        assert_eq!(
            score_pinyin_match("重新整理.md", &["chongxin".to_string()]),
            Some(MatchScore::Prefix)
        );
        assert_eq!(
            score_pinyin_match("乾坤.txt", &["qiankun".to_string()]),
            Some(MatchScore::Prefix)
        );
        assert_eq!(
            score_pinyin_match("音樂.mp3", &["yinyue".to_string()]),
            Some(MatchScore::Prefix)
        );
    }

    #[test]
    fn test_polyphone_table_invariants() {
        let table = pinyin_data::POLYPHONE_TABLE;
        assert!(!table.is_empty());

        // 1. 严格按 char 码点升序，无重复键
        for pair in table.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "POLYPHONE_TABLE must be strictly sorted without duplicates: {:?} >= {:?}",
                pair[0].0,
                pair[1].0
            );
        }

        // 2. 每个键均已规范简体化、至少 2 个小写 ASCII 读音，且包含 PINYIN_TABLE 主读音
        for &(ch, variants) in table {
            assert_eq!(
                crate::cjk::to_simplified_char(ch),
                ch,
                "POLYPHONE_TABLE key {ch:?} must be simplified"
            );
            assert!(
                variants.len() >= 2,
                "POLYPHONE_TABLE entry {ch:?} must have at least 2 variants"
            );
            for &syl in variants {
                assert!(
                    !syl.is_empty() && syl.bytes().all(|b| b.is_ascii_lowercase()),
                    "POLYPHONE_TABLE entry {ch:?} contains non-ASCII-lowercase syllable {syl:?}"
                );
            }
            let unique: std::collections::HashSet<&str> = variants.iter().copied().collect();
            assert_eq!(
                unique.len(),
                variants.len(),
                "POLYPHONE_TABLE entry {ch:?} has duplicate readings: {variants:?}"
            );

            let u = ch as u32;
            assert!((pinyin_data::CJK_START..=pinyin_data::CJK_END).contains(&u));
            let base_idx =
                pinyin_data::PINYIN_TABLE[(u - pinyin_data::CJK_START) as usize] as usize;
            let base_syl = pinyin_data::PINYIN_SYLLABLES[base_idx];
            assert!(
                variants.contains(&base_syl),
                "POLYPHONE_TABLE entry {ch:?} missing primary reading {base_syl:?} in {variants:?}"
            );
            assert_eq!(
                variants[0], base_syl,
                "POLYPHONE_TABLE entry {ch:?} should list primary reading {base_syl:?} first"
            );
        }
    }

    #[test]
    fn test_char_pinyin_variants_filter_and_trad_simp() {
        for bad in ['兙', '兡', '嗧', '桛', '烪', '瓧', '瓰', '瓱', '瓼', '甅'] {
            assert!(
                char_pinyin_variants(bad).is_empty(),
                "non-ASCII syllable placeholder {bad:?} must be filtered"
            );
            assert_eq!(char_pinyin(bad), None);
        }

        for (trad, simp) in [('長', '长'), ('樂', '乐'), ('乾', '干'), ('開', '开'), ('發', '发')] {
            assert_eq!(char_pinyin_variants(trad), char_pinyin_variants(simp));
            assert_eq!(char_pinyin(trad), char_pinyin(simp));
        }
    }

    #[test]
    fn test_is_v_u_syllable_boundaries() {
        for ok in [
            "lu", "lue", "nu", "nue", "ju", "juan", "jue", "jun", "qu", "quan", "que", "qun",
            "xu", "xuan", "xue", "xun", "yu", "yuan", "yue", "yun",
        ] {
            assert!(is_v_u_syllable(ok), "{ok:?} should allow u/v equivalence");
        }

        for bad in [
            "luan", "luo", "lun", "liu", "lou", "nuan", "nuo", "nun", "niu", "nou", "jiu", "qiu",
            "xiu", "you", "lv", "u", "v", "",
        ] {
            assert!(!is_v_u_syllable(bad), "{bad:?} must not allow u/v equivalence");
        }
    }

    #[test]
    fn test_pinyin_match_tier_ordering_and_detail() {
        assert!(PinyinMatchTier::SubstringInitials < PinyinMatchTier::PrefixInitials);
        assert!(PinyinMatchTier::PrefixInitials < PinyinMatchTier::SubstringMixed);
        assert!(PinyinMatchTier::SubstringMixed < PinyinMatchTier::SubstringFull);
        assert!(PinyinMatchTier::SubstringFull < PinyinMatchTier::PrefixMixed);
        assert!(PinyinMatchTier::PrefixMixed < PinyinMatchTier::PrefixFull);

        assert_eq!(
            score_pinyin_detail("工作汇报.docx", &["gzhb".to_string()]).map(|d| d.tier),
            Some(PinyinMatchTier::PrefixInitials)
        );
        assert_eq!(
            score_pinyin_detail("2026年工作汇报.docx", &["gzhb".to_string()]).map(|d| d.tier),
            Some(PinyinMatchTier::SubstringInitials)
        );
        assert_eq!(
            score_pinyin_detail("工作汇报.docx", &["gongzuo".to_string()]).map(|d| d.tier),
            Some(PinyinMatchTier::PrefixFull)
        );
        assert_eq!(
            score_pinyin_detail("2026年工作汇报.docx", &["huibao".to_string()]).map(|d| d.tier),
            Some(PinyinMatchTier::SubstringFull)
        );
        assert_eq!(
            score_pinyin_detail("截图表.xlsx", &["jietb".to_string()]).map(|d| d.tier),
            Some(PinyinMatchTier::PrefixMixed)
        );
        assert_eq!(
            score_pinyin_detail("2026年截图表.xlsx", &["jietb".to_string()]).map(|d| d.tier),
            Some(PinyinMatchTier::SubstringMixed)
        );
        assert_eq!(
            score_pinyin_detail("工作汇报.docx", &["gongzuo".to_string(), "hb".to_string()])
                .map(|d| d.tier),
            Some(PinyinMatchTier::PrefixMixed)
        );
        assert_eq!(
            score_pinyin_detail(
                "工作汇报.docx",
                &["gongzuo".to_string(), "huibao".to_string()]
            )
            .map(|d| d.tier),
            Some(PinyinMatchTier::PrefixFull)
        );

        // 混合纯字面词 + 拼音词（如 "2026 cwysb"）合法命中，而纯字面查询返回 None
        let mixed = score_pinyin_detail(
            "2026年财务预算表.xlsx",
            &["2026".to_string(), "cwysb".to_string()],
        );
        assert_eq!(
            mixed.map(|d| (d.tier, d.is_prefix)),
            Some((PinyinMatchTier::PrefixInitials, true))
        );
        assert_eq!(
            score_pinyin_detail("2026年财务预算表.xlsx", &["2026".to_string()]),
            None
        );

        // 超过栈容量 (64 bytes) 的长拼音查询回退至堆复用缓冲区
        let long_name = "工作汇报工作汇报工作汇报工作汇报工作汇报.docx";
        let long_kw = "gongzuohuibao".repeat(5); // 65 bytes > 64
        assert_eq!(
            score_pinyin_detail(long_name, &[long_kw]).map(|d| d.tier),
            Some(PinyinMatchTier::PrefixFull)
        );
    }
}
