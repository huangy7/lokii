use crate::indexer::MatchScore;
use crate::pinyin_data;
use crate::string_pool::find_ascii_ignore_case;

#[inline]
pub fn char_pinyin(c: char) -> Option<(&'static str, char)> {
    let simp = crate::cjk::to_simplified_char(c);
    let u = simp as u32;
    if (pinyin_data::CJK_START..=pinyin_data::CJK_END).contains(&u) {
        let idx = pinyin_data::PINYIN_TABLE[(u - pinyin_data::CJK_START) as usize] as usize;
        let syllable = pinyin_data::PINYIN_SYLLABLES[idx];
        let initial = syllable.chars().next()?;
        Some((syllable, initial))
    } else {
        None
    }
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

/// 拼音转写打分：零额外的堆分配，不构建字节区间映射表。
/// 内部比对采用 ASCII 大小写无关匹配，避免热路径构建 lower_keywords。
/// 仅当查询关键词均为 ASCII 且目标文件名包含 CJK 字符时触发。
pub(crate) fn score_pinyin_match(name: &str, keywords: &[String]) -> Option<MatchScore> {
    if keywords.is_empty() || !keywords.iter().all(|k| k.is_ascii()) {
        return None;
    }
    if !has_cjk_characters(name) {
        return None;
    }

    let mut initials = String::with_capacity(name.len());
    let mut full_pinyin = String::with_capacity(name.len() * 3);

    for c in name.chars() {
        if let Some((syllable, initial)) = char_pinyin(c) {
            initials.push(initial);
            full_pinyin.push_str(syllable);
        } else {
            let lower = c.to_ascii_lowercase();
            initials.push(lower);
            full_pinyin.push(lower);
        }
    }

    let mut is_prefix = false;
    for (i, kw) in keywords.iter().enumerate() {
        if kw.is_empty() {
            continue;
        }
        let kw_str = kw.as_str();
        let full_idx = find_ascii_ignore_case(full_pinyin.as_bytes(), kw_str.as_bytes());
        let init_idx = if full_idx.is_some() {
            None
        } else {
            find_ascii_ignore_case(initials.as_bytes(), kw_str.as_bytes())
        };

        if full_idx.is_none() && init_idx.is_none() {
            return None;
        }

        if i == 0 && (full_idx == Some(0) || init_idx == Some(0)) {
            is_prefix = true;
        }
    }

    Some(if is_prefix {
        MatchScore::Prefix
    } else {
        MatchScore::Substring
    })
}

/// 仅对进入 Top-K 的命中结果计算拼音对应原始文件名的字节区间，供界面高亮。
/// 严格保证字节偏移与原始 UTF-8 字符范围 1:1 对齐，杜绝因多字节字符导致的越界崩溃。
pub(crate) fn find_pinyin_positions<S: AsRef<str>>(name: &str, keywords: &[S]) -> Vec<(usize, usize)> {
    if keywords.is_empty() || !has_cjk_characters(name) {
        return Vec::new();
    }

    let mut initials = String::with_capacity(name.len());
    let mut initials_byte_ranges = Vec::with_capacity(name.len());
    let mut full_pinyin = String::with_capacity(name.len() * 3);
    let mut pinyin_char_byte_ranges = Vec::with_capacity(name.len() * 3);

    let mut byte_offset = 0;
    for c in name.chars() {
        let char_len = c.len_utf8();
        let range = (byte_offset, byte_offset + char_len);
        byte_offset += char_len;

        if let Some((syllable, initial)) = char_pinyin(c) {
            initials.push(initial);
            initials_byte_ranges.push(range);

            for _ in 0..syllable.len() {
                pinyin_char_byte_ranges.push(range);
            }
            full_pinyin.push_str(syllable);
        } else {
            let lower = c.to_ascii_lowercase();
            // 非 CJK 字符可能为多字节（如中文全角标点【】、分隔符·等）
            // str::find 返回字节偏移量，byte_ranges 必须为该字符所占的每个字节各推入一个映射项
            for _ in 0..char_len {
                initials_byte_ranges.push(range);
                pinyin_char_byte_ranges.push(range);
            }
            initials.push(lower);
            full_pinyin.push(lower);
        }
    }

    debug_assert_eq!(initials.len(), initials_byte_ranges.len());
    debug_assert_eq!(full_pinyin.len(), pinyin_char_byte_ranges.len());

    let mut positions = Vec::new();
    for kw in keywords {
        let kw_str = kw.as_ref();
        if kw_str.is_empty() {
            continue;
        }
        if let Some(start) = find_ascii_ignore_case(full_pinyin.as_bytes(), kw_str.as_bytes()) {
            let end = start + kw_str.len();
            if end > 0 && end <= pinyin_char_byte_ranges.len() {
                let orig_start = pinyin_char_byte_ranges[start].0;
                let orig_end = pinyin_char_byte_ranges[end - 1].1;
                positions.push((orig_start, orig_end));
            }
        } else if let Some(start) = find_ascii_ignore_case(initials.as_bytes(), kw_str.as_bytes()) {
            let end = start + kw_str.len();
            if end > 0 && end <= initials_byte_ranges.len() {
                let orig_start = initials_byte_ranges[start].0;
                let orig_end = initials_byte_ranges[end - 1].1;
                positions.push((orig_start, orig_end));
            }
        }
    }

    positions.sort_by_key(|&(s, e)| (s, e));
    positions.dedup();
    positions
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
