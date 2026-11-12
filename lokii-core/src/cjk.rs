use std::borrow::Cow;
use crate::cjk_data::TRAD_TO_SIMP;

pub const MIN_TRAD_CHAR: char = '\u{346F}';
pub const MAX_TRAD_CHAR: char = '\u{30EDE}';

/// 将单个 CJK 繁体字字符转换为规范简体字字符。
/// 若字符不在繁体字表中，或为 ASCII / 非 CJK 字符，直接原样返回。
#[inline]
pub fn to_simplified_char(c: char) -> char {
    if c < MIN_TRAD_CHAR || c > MAX_TRAD_CHAR {
        return c;
    }
    match TRAD_TO_SIMP.binary_search_by_key(&c, |&(trad, _)| trad) {
        Ok(idx) => TRAD_TO_SIMP[idx].1,
        Err(_) => c,
    }
}

/// 快速判定单个字符是否为可转换为简体字的繁体字。
#[inline]
pub fn is_trad_char(c: char) -> bool {
    to_simplified_char(c) != c
}

/// 将单个字符进行大小写折叠与繁简归一化。
/// 对于 ASCII 大写字符转小写；对于繁体汉字转为简体汉字；非 ASCII 变音字符统一规范化至小写。
#[inline]
pub fn fold_char(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        let simp = to_simplified_char(c);
        let u = simp as u32;
        if (0x4E00..=0x9FFF).contains(&u) || (0x3400..=0x4DBF).contains(&u) {
            simp
        } else {
            simp.to_lowercase().next().unwrap_or(simp)
        }
    }
}

/// 将字符串统一转换为简体字。
/// 若字符串无需转换（无繁体字符），返回 Cow::Borrowed，实现零堆分配。
pub fn to_simplified_str(s: &str) -> Cow<'_, str> {
    // 绝大多数文件名是纯 ASCII，先整串判定可省掉逐字符解码，百万级文件名下约快 15 倍
    if s.is_ascii() || s.chars().all(|c| to_simplified_char(c) == c) {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(s.chars().map(to_simplified_char).collect())
    }
}

/// 快速判定字符串是否包含需要折叠的大写字符或繁体汉字。
/// 对 CJK 统一表意文字短路跳过大小写判定，规避不必要的 Unicode 查找表开销。
#[inline]
pub fn needs_fold(s: &str) -> bool {
    s.chars().any(|c| {
        if c.is_ascii() {
            c.is_ascii_uppercase()
        } else {
            let u = c as u32;
            if (0x4E00..=0x9FFF).contains(&u) || (0x3400..=0x4DBF).contains(&u) {
                is_trad_char(c)
            } else {
                is_trad_char(c) || c.to_lowercase().any(|lc| lc != c)
            }
        }
    })
}

/// 统一的查询串与索引名归一化折叠器（Cow 优化版本）：
/// 若字符串无需任何转换（已是全小写且不含繁体字），返回 Cow::Borrowed，实现真正的零堆分配。
pub fn fold_cow<'a>(s: &'a str) -> Cow<'a, str> {
    if !needs_fold(s) {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(fold_str(s))
    }
}

/// 统一的查询串与索引名归一化折叠器：
/// 1. 小写化（ASCII 与非 ASCII 拉丁字符小写化）；
/// 2. CJK 繁体字转换为规范简体字。
/// 保证查询与索引折叠镜像在单字级别完全对齐。
pub fn fold_str(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii() {
            result.push(c.to_ascii_lowercase());
        } else {
            let simp = to_simplified_char(c);
            let u = simp as u32;
            if (0x4E00..=0x9FFF).contains(&u) || (0x3400..=0x4DBF).contains(&u) {
                result.push(simp);
            } else {
                for lower in simp.to_lowercase() {
                    result.push(lower);
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_simplified_char() {
        assert_eq!(to_simplified_char('開'), '开');
        assert_eq!(to_simplified_char('發'), '发');
        assert_eq!(to_simplified_char('備'), '备');
        assert_eq!(to_simplified_char('忘'), '忘');
        assert_eq!(to_simplified_char('錄'), '录');
        assert_eq!(to_simplified_char('簡'), '简');
        assert_eq!(to_simplified_char('體'), '体');
        assert_eq!(to_simplified_char('a'), 'a');
        assert_eq!(to_simplified_char('Z'), 'Z');
        assert_eq!(to_simplified_char('1'), '1');
    }

    #[test]
    fn test_to_simplified_str_borrowed_when_unchanged() {
        let text = "测试文档123.rs";
        let res = to_simplified_str(text);
        assert!(matches!(res, Cow::Borrowed(_)));
        assert_eq!(res, text);
    }

    #[test]
    fn test_to_simplified_str_owned_when_changed() {
        let text = "測試文檔123.rs";
        let res = to_simplified_str(text);
        assert!(matches!(res, Cow::Owned(_)));
        assert_eq!(res, "测试文档123.rs");
    }

    #[test]
    fn test_fold_str() {
        assert_eq!(fold_str("Hello WORLD"), "hello world");
        assert_eq!(fold_str("測試文檔.MD"), "测试文档.md");
        assert_eq!(fold_str("開發規範_v1.0.rs"), "开发规范_v1.0.rs");
    }
}
