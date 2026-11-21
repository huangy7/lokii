use std::borrow::Cow;
use icu_normalizer::ComposingNormalizerBorrowed;
use crate::cjk_data::TRAD_TO_SIMP;

pub const MIN_TRAD_CHAR: char = '\u{346F}';
pub const MAX_TRAD_CHAR: char = '\u{30EDE}';

/// 编译期静态嵌入的 Unicode NFC 合成归一化器，用于消除跨文件系统（如 macOS APFS NFD 与用户输入 NFC）的表示差异。
const NFC: ComposingNormalizerBorrowed<'static> = ComposingNormalizerBorrowed::new_nfc();

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

/// 保持大小写的纯 Unicode NFC 归一化入口。
/// 纯 ASCII 或原本已符合 NFC 规范的字符串直接零拷贝借用原切片。
#[inline]
pub fn normalize_nfc_cow(s: &str) -> Cow<'_, str> {
    if s.is_ascii() {
        Cow::Borrowed(s)
    } else {
        NFC.normalize(s)
    }
}

/// 保持大小写的同时完成 Unicode NFC 合成归一化与汉字繁简转换。
/// 先合成 NFC 再做繁简映射，可覆盖 CJK 兼容汉字区（如 U+F900..U+FAFF）合成为标准表意文字后的简体映射。
pub fn normalize_and_simplify_cow(s: &str) -> Cow<'_, str> {
    if s.is_ascii() {
        return Cow::Borrowed(s);
    }
    let nfc = NFC.normalize(s);
    if nfc.chars().all(|c| to_simplified_char(c) == c) {
        nfc
    } else {
        Cow::Owned(nfc.chars().map(to_simplified_char).collect())
    }
}

/// 将字符串统一转换为 NFC 合规且简体化的形式。
/// 绝大多数文件名是纯 ASCII 或已规范化，先判定可实现零堆分配借用，避免百万级索引下无谓内存分配。
pub fn to_simplified_str(s: &str) -> Cow<'_, str> {
    normalize_and_simplify_cow(s)
}

/// 快速判定字符串是否需要执行大小写折叠、繁体转换或 Unicode NFC 合成。
/// 对纯 ASCII 仅扫描大写字节，对非 ASCII 先检查 NFC 状态并跳过无大小写差异的 CJK 统一表意文字。
#[inline]
pub fn needs_fold(s: &str) -> bool {
    if s.is_ascii() {
        return s.bytes().any(|b| b.is_ascii_uppercase());
    }
    if !NFC.is_normalized(s) {
        return true;
    }
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
/// 若字符串已满足 NFC、全小写且不含繁体字，返回 Cow::Borrowed，实现真正的零堆分配。
pub fn fold_cow<'a>(s: &'a str) -> Cow<'a, str> {
    if !needs_fold(s) {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(fold_str(s))
    }
}

/// 统一的查询串与索引名归一化折叠器：
///
/// 1. 非 ASCII 输入先经 Unicode NFC 合成归一化（覆盖 NFD 变音符与组合韩文字母）；
/// 2. CJK 繁体字转换为规范简体字；
/// 3. ASCII 与非 ASCII 字母统一小写化，并在必要时再次保证输出处于 NFC 状态。
///
/// 确保检索查询与索引折叠镜像在单字及组合字符簇级别保持严格等价。
pub fn fold_str(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    let nfc = NFC.normalize(s);
    let mut result = String::with_capacity(nfc.len());
    for c in nfc.chars() {
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
    if !NFC.is_normalized(&result) {
        NFC.normalize(&result).into_owned()
    } else {
        result
    }
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

    #[test]
    fn test_nfc_normalization_and_cow_fast_paths() {
        let ascii = "src/main.rs";
        assert!(matches!(normalize_nfc_cow(ascii), Cow::Borrowed(_)));
        assert!(matches!(normalize_and_simplify_cow(ascii), Cow::Borrowed(_)));
        assert!(matches!(fold_cow(ascii), Cow::Borrowed(_)));
        assert!(!needs_fold("already_lower.rs"));
        assert!(needs_fold("MixedCase.rs"));

        let nfd_cafe = "Cafe\u{0301}";
        let nfc_cafe = "Café";
        assert!(needs_fold("cafe\u{0301}"));
        assert!(!needs_fold("café"));
        assert_eq!(normalize_nfc_cow(nfd_cafe).as_ref(), nfc_cafe);
        assert!(matches!(normalize_nfc_cow(nfc_cafe), Cow::Borrowed(_)));
        assert_eq!(normalize_and_simplify_cow("測Cafe\u{0301}").as_ref(), "测Café");
        assert_eq!(fold_cow(nfd_cafe).as_ref(), "café");
        assert_eq!(fold_str("\u{1112}\u{1161}\u{11AB}\u{1100}\u{1173}\u{11AF}"), "한글");
        // 幂等性验证
        let once = fold_str("測İCafe\u{0301}");
        assert_eq!(fold_str(&once), once);
        assert!(!needs_fold(&once));
    }
}
