use std::borrow::Cow;

use fsevent_sys::core_foundation as cf;

/// 短序列布隆过滤器的窗口长度（字节）。
/// 在 24-bit 三元组交集基础上追加 5 字节短语窗口，可拦截绝大多数跨行偶然凑齐三元组的误召回，显著减少实盘校验 I/O。
pub const SHORT_GRAM_LEN: usize = 5;

/// 布隆过滤器为每个去重短语分配的位预算。3 bits 在 2 个哈希函数下可将单一缺失短语误放行率压低至约 24%。
pub const BLOOM_BITS_PER_GRAM: usize = 3;

/// 单文件默认最大文本大小（1 MiB）。
pub const MAX_PLAIN_FILE_BYTES: u64 = 1 << 20;

/// 代码仓库内常规文本文件放宽至 10 MiB，兼容大型源码生成物或单文件模块。
pub const MAX_REPO_FILE_BYTES: u64 = 10 << 20;

/// 索引键最高位标记：第 24 位为 1 表示该三元组来自代码符号定义位置，用于 `sym:` 检索加速。
pub const SYM_KEY_BIT: u32 = 1 << 24;

/// 触发长行压缩/混淆文本拦截的最小样本长度。
const LONG_LINE_MIN_BYTES: usize = 4096;

/// 平均行长超过该阈值的文件视为前端 bundle 或压缩文本，仅保留文件名可搜，跳过正文倒排以防词表膨胀。
const LONG_LINE_MAX_AVG_BYTES: usize = 1000;

/// CoreFoundation GB18030 编码常量（来自 macOS SDK CFStringEncodingExt.h）。
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const CF_STRING_ENCODING_GB_18030_2000: u32 = 0x0632;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithBytes(
        alloc: cf::CFRef,
        bytes: *const u8,
        num_bytes: cf::CFIndex,
        encoding: u32,
        is_external_representation: u8,
    ) -> cf::CFRef;
    fn CFStringGetLength(the_string: cf::CFRef) -> cf::CFIndex;
    fn CFStringGetBytes(
        the_string: cf::CFRef,
        range: CFRange,
        encoding: u32,
        loss_byte: u8,
        is_external_representation: u8,
        buffer: *mut u8,
        max_buf_len: cf::CFIndex,
        used_buf_len: *mut cf::CFIndex,
    ) -> cf::CFIndex;
    fn CFRelease(cf: cf::CFRef);
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CFRange {
    location: cf::CFIndex,
    length: cf::CFIndex,
}

/// 分支无关的 ASCII 小写折叠，保持非 ASCII 字节原样以维持 UTF-8 多字节序列完整性。
#[inline(always)]
pub fn fold_ascii(byte: u8) -> u8 {
    byte | (((byte.wrapping_sub(b'A') < 26) as u8) << 5)
}

/// 计算折叠后的全切片副本。
#[inline]
pub fn fold_ascii_slice(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(|&b| fold_ascii(b)).collect()
}

/// 64 位非加密内容摘要，用于多副本文件与多工作树下的内容级去重。
pub fn content_hash(bytes: &[u8]) -> u64 {
    #[inline(always)]
    fn mix(x: u64, y: u64) -> u64 {
        let m = (x as u128).wrapping_mul(y as u128);
        (m as u64) ^ ((m >> 64) as u64)
    }

    const K: [u64; 4] = [
        0x9E37_79B9_7F4A_7C15,
        0xC2B2_AE3D_27D4_EB4F,
        0x1656_67B1_9E37_79F9,
        0x85EB_CA77_C2B2_AE63,
    ];
    let read_u64 = |w: &[u8], i: usize| u64::from_le_bytes(w[i * 8..i * 8 + 8].try_into().unwrap());
    let len = bytes.len() as u64;
    let mut s = [K[0] ^ len, K[1], K[2], K[3] ^ len.rotate_left(32)];

    let mut step = |w: &[u8]| {
        let x = [
            read_u64(w, 0),
            read_u64(w, 1),
            read_u64(w, 2),
            read_u64(w, 3),
        ];
        s = [
            mix(x[0] ^ K[0], x[1] ^ s[0]),
            mix(x[1] ^ K[1], x[0] ^ s[1]),
            mix(x[2] ^ K[2], x[3] ^ s[2]),
            mix(x[3] ^ K[3], x[2] ^ s[3]),
        ];
    };

    let mut chunks = bytes.chunks_exact(32);
    for w in &mut chunks {
        step(w);
    }
    let rem = chunks.remainder();
    let mut last = [0u8; 32];
    last[..rem.len()].copy_from_slice(rem);
    step(&last);

    mix(mix(s[0] ^ K[1], s[2] ^ K[2]) ^ s[1], s[3] ^ K[0])
}

/// 对一段已折叠的 5 字节短窗口计算哈希，用于构建与探测内容布隆过滤器。
#[inline]
pub fn short_gram_hash(folded_window: u64) -> u32 {
    let x = folded_window
        .wrapping_add(1)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    ((x ^ (x >> 32)) & 0x00FF_FFFF) as u32
}

/// 根据每篇去重内容的短语数量与位预算填充布隆过滤器字节。
pub fn build_bloom_bytes(short_hashes: &[u32]) -> Vec<u8> {
    if short_hashes.is_empty() {
        return Vec::new();
    }
    let total_bits = (short_hashes.len() * BLOOM_BITS_PER_GRAM).max(64).next_multiple_of(8);
    let mut out = vec![0u8; total_bits / 8];
    let modulus = total_bits as u32;
    for &h in short_hashes {
        let (b0, b1) = bloom_positions(h, modulus);
        out[b0 >> 3] |= 1u8 << (b0 & 7);
        out[b1 >> 3] |= 1u8 << (b1 & 7);
    }
    out
}

/// 探测内容布隆过滤器是否可能包含全部查询短窗口。
#[inline]
pub fn bloom_contains_all(bloom: &[u8], short_hashes: &[u32]) -> bool {
    if short_hashes.is_empty() {
        return true;
    }
    if bloom.is_empty() {
        return false;
    }
    let modulus = (bloom.len() * 8) as u32;
    for &h in short_hashes {
        let (b0, b1) = bloom_positions(h, modulus);
        if (bloom[b0 >> 3] & (1u8 << (b0 & 7))) == 0 || (bloom[b1 >> 3] & (1u8 << (b1 & 7))) == 0 {
            return false;
        }
    }
    true
}

#[inline(always)]
fn bloom_positions(hash: u32, total_bits: u32) -> (usize, usize) {
    let h1 = hash.wrapping_mul(0x9E37_79B1);
    let h2 = h1.rotate_left(15).wrapping_mul(0x85EB_CA77);
    ((h1 % total_bits) as usize, (h2 % total_bits) as usize)
}

/// 判定切片是否表现为压缩/单行打包代码（平均行长过高）。
#[inline]
pub fn has_long_lines(buf: &[u8]) -> bool {
    if buf.len() < LONG_LINE_MIN_BYTES {
        return false;
    }
    let min_newlines = (buf.len() / (LONG_LINE_MAX_AVG_BYTES + 1)).max(1);
    memchr::memchr_iter(b'\n', buf).nth(min_newlines - 1).is_none()
}

/// 校验缓冲区是否适合作为纯文本处理（不含 NUL 字节且非单行压缩产物）。
#[inline]
pub fn is_text_buffer(buf: &[u8]) -> bool {
    memchr::memchr(0, buf).is_none() && !has_long_lines(buf)
}

/// 检查非 UTF-8 文本在转码前是否混杂过多二进制控制字符。
fn has_binary_control_bytes(raw: &[u8]) -> bool {
    if memchr::memchr(0, raw).is_some() {
        return true;
    }
    let sample = &raw[..raw.len().min(4096)];
    let ctrl = sample
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C | 0x1B))
        .count();
    ctrl * 64 > sample.len().max(1)
}

/// 将原始字节按 UTF-16 BOM / UTF-8 / GB18030 顺序归一化为合法 UTF-8 文本，同时拦截二进制文件与单行混淆资源。
pub fn decode_plain_text<'a>(raw: &'a [u8]) -> Option<Cow<'a, [u8]>> {
    if raw.is_empty() {
        return Some(Cow::Borrowed(raw));
    }

    // 1. UTF-16 BOM（包含双字节 0x00，必须优先于 NUL 检查识别）
    if raw.len() >= 2 && (raw.starts_with(&[0xFF, 0xFE]) || raw.starts_with(&[0xFE, 0xFF])) {
        let is_le = raw[0] == 0xFF;
        let payload = &raw[2..];
        if payload.len() % 2 != 0 {
            return None;
        }
        let units: Vec<u16> = payload
            .chunks_exact(2)
            .map(|c| {
                if is_le {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    u16::from_be_bytes([c[0], c[1]])
                }
            })
            .collect();
        let text = String::from_utf16_lossy(&units);
        let bytes = text.into_bytes();
        return (!has_long_lines(&bytes)).then_some(Cow::Owned(bytes));
    }

    // 2. UTF-8 快路径（含 UTF-8 BOM 剥离）
    let utf8_slice = raw.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(raw);
    if std::str::from_utf8(utf8_slice).is_ok() {
        return is_text_buffer(utf8_slice).then_some(Cow::Borrowed(utf8_slice));
    }

    // 3. Windows 常见中文文本编码（GB18030 / GBK）兜底转码
    if has_binary_control_bytes(raw) {
        return None;
    }
    let decoded = decode_gb18030_to_utf8(raw)?;
    is_text_buffer(&decoded).then_some(Cow::Owned(decoded))
}

fn decode_gb18030_to_utf8(raw: &[u8]) -> Option<Vec<u8>> {
    // SAFETY: CoreFoundation 字符串接口接受只读字节指针与长度，失败返回空指针后即刻退出，成功路径配对释放引用。
    unsafe {
        let cf_str = CFStringCreateWithBytes(
            std::ptr::null_mut(),
            raw.as_ptr(),
            raw.len() as cf::CFIndex,
            CF_STRING_ENCODING_GB_18030_2000,
            0,
        );
        if cf_str.is_null() {
            return None;
        }
        let len = CFStringGetLength(cf_str);
        let range = CFRange {
            location: 0,
            length: len,
        };
        let mut needed: cf::CFIndex = 0;
        let converted = CFStringGetBytes(
            cf_str,
            range,
            CF_STRING_ENCODING_UTF8,
            0,
            0,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
        if converted != len || needed < 0 {
            CFRelease(cf_str);
            return None;
        }
        let mut out = vec![0u8; needed as usize];
        let mut written: cf::CFIndex = 0;
        let ok = CFStringGetBytes(
            cf_str,
            range,
            CF_STRING_ENCODING_UTF8,
            0,
            0,
            out.as_mut_ptr(),
            needed,
            &mut written,
        );
        CFRelease(cf_str);
        if ok != len {
            return None;
        }
        out.truncate(written.max(0) as usize);
        Some(out)
    }
}

/// 从归一化 UTF-8 正文中抽取出用于倒排索引的去重键集合。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ExtractedGrams {
    /// 排序去重后的索引键：普通 24-bit 三元组（含文末 2 字节哨兵）及带 `SYM_KEY_BIT` 的符号定义三元组。
    pub keys: Vec<u32>,
    /// 5 字节短窗口去重哈希，供生成紧凑布隆过滤器使用。
    pub short_hashes: Vec<u32>,
}

/// 单次扫描提取正文的折叠三元组、文末 2 字节哨兵、符号定义键与 5 字节短窗口哈希。
pub fn extract_grams(buf: &[u8], index_symbols: bool) -> ExtractedGrams {
    let n = buf.len();
    if n < 2 {
        return ExtractedGrams::default();
    }

    let mut keys = Vec::with_capacity((n / 4).clamp(16, 4096));
    let mut short_hashes = Vec::with_capacity((n / 4).clamp(16, 4096));

    // 文末 2 字节补零编码为哨兵键，令双字节 ASCII 查询能直接锁定倒排范围 [p, p | 0xFF]。
    let eof_pair = ((fold_ascii(buf[n - 2]) as u32) << 16) | ((fold_ascii(buf[n - 1]) as u32) << 8);
    keys.push(eof_pair);

    let mut window = ((fold_ascii(buf[0]) as u64) << 8) | (fold_ascii(buf[1]) as u64);
    for (idx, &b) in buf[2..].iter().enumerate() {
        let abs_idx = idx + 2;
        window = (window << 8) | (fold_ascii(b) as u64);
        keys.push((window & 0x00FF_FFFF) as u32);
        if abs_idx + 1 >= SHORT_GRAM_LEN {
            short_hashes.push(short_gram_hash(window & 0xFF_FFFF_FFFF));
        }
    }

    if index_symbols {
        extract_symbol_definition_keys(buf, &mut keys);
    }

    keys.sort_unstable();
    keys.dedup();
    short_hashes.sort_unstable();
    short_hashes.dedup();

    ExtractedGrams { keys, short_hashes }
}

/// 常见编程语言与接口定义关键字前缀，用于在索引期额外记录符号定义位置的三元组。
const SYMBOL_PREFIXES: &[&[u8]] = &[
    b"fn ",
    b"pub fn ",
    b"async fn ",
    b"pub async fn ",
    b"struct ",
    b"pub struct ",
    b"enum ",
    b"pub enum ",
    b"trait ",
    b"pub trait ",
    b"impl ",
    b"type ",
    b"pub type ",
    b"const ",
    b"pub const ",
    b"mod ",
    b"pub mod ",
    b"func ",
    b"class ",
    b"protocol ",
    b"interface ",
    b"def ",
    b"async def ",
    b"export function ",
    b"export async function ",
    b"export class ",
    b"export interface ",
    b"export type ",
    b"export const ",
    b"function ",
];

fn extract_symbol_definition_keys(buf: &[u8], out_keys: &mut Vec<u32>) {
    for line in buf.split(|&b| b == b'\n') {
        let trimmed = trim_ascii_start_ws(line);
        if let Some(ident) = symbol_identifier_on_line(trimmed) {
            if ident.len() >= 3 {
                for tri in trigrams_folded(ident) {
                    out_keys.push(SYM_KEY_BIT | tri);
                }
            } else if ident.len() == 2 {
                let pair = ((fold_ascii(ident[0]) as u32) << 16)
                    | ((fold_ascii(ident[1]) as u32) << 8);
                out_keys.push(SYM_KEY_BIT | pair);
            }
        }
    }
}

#[inline]
fn trim_ascii_start_ws(mut s: &[u8]) -> &[u8] {
    while let Some((&b, rest)) = s.split_first() {
        if matches!(b, b' ' | b'\t' | b'\r') {
            s = rest;
        } else {
            break;
        }
    }
    s
}

#[inline]
pub fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn symbol_identifier_on_line(trimmed_line: &[u8]) -> Option<&[u8]> {
    for prefix in SYMBOL_PREFIXES {
        if let Some(mut rest) = trimmed_line.strip_prefix(*prefix) {
            // Go 方法接收者: func (r *Receiver) Method(...)
            if *prefix == b"func " && rest.starts_with(b"(") {
                if let Some(close) = memchr::memchr(b')', rest) {
                    rest = trim_ascii_start_ws(&rest[close + 1..]);
                }
            }
            // Rust `impl<T> Trait for Type` / `impl Type` 仅取紧随其后的主标识符
            if *prefix == b"impl " && rest.starts_with(b"<") {
                if let Some(close) = memchr::memchr(b'>', rest) {
                    rest = trim_ascii_start_ws(&rest[close + 1..]);
                }
            }
            let rest = trim_ascii_start_ws(rest);
            let end = rest
                .iter()
                .position(|&b| !is_ident_byte(b))
                .unwrap_or(rest.len());
            if end > 0 {
                return Some(&rest[..end]);
            }
        }
    }
    None
}

/// 判定正文中 `byte_offset..byte_offset+len` 是否处于标识符定义的词边界上（用于 `sym:` 精确核验）。
pub fn is_symbol_defined_at(haystack: &[u8], byte_offset: usize, len: usize) -> bool {
    let line_start = memchr::memrchr(b'\n', &haystack[..byte_offset]).map_or(0, |p| p + 1);
    let line_end = memchr::memchr(b'\n', &haystack[byte_offset + len..])
        .map_or(haystack.len(), |p| byte_offset + len + p);
    let trimmed = trim_ascii_start_ws(&haystack[line_start..line_end]);
    let Some(ident) = symbol_identifier_on_line(trimmed) else {
        return false;
    };
    let ident_start = ident.as_ptr() as usize - haystack.as_ptr() as usize;
    byte_offset == ident_start && len == ident.len()
}

/// 对查询串提取去重后的 ASCII 折叠 24-bit 三元组。
pub fn trigrams_folded(bytes: &[u8]) -> Vec<u32> {
    if bytes.len() < 3 {
        return Vec::new();
    }
    let mut out: Vec<u32> = bytes
        .windows(3)
        .map(|w| {
            ((fold_ascii(w[0]) as u32) << 16)
                | ((fold_ascii(w[1]) as u32) << 8)
                | (fold_ascii(w[2]) as u32)
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// 对查询串提取去重后的 5 字节短窗口哈希。
pub fn query_short_hashes(bytes: &[u8]) -> Vec<u32> {
    if bytes.len() < SHORT_GRAM_LEN {
        return Vec::new();
    }
    let mut out: Vec<u32> = bytes
        .windows(SHORT_GRAM_LEN)
        .map(|w| {
            let mut v = 0u64;
            for &b in w {
                v = (v << 8) | (fold_ascii(b) as u64);
            }
            short_gram_hash(v)
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// 倒排查询抽象语法树，支持字面量、符号与正则表达式的索引前筛。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GramPlan {
    /// 无需（或无法）通过倒排过滤，交由实盘扫描与外部过滤条件收敛。
    All,
    /// 单一倒排键（如 24-bit 三元组或带符号标志的键）。
    Key(u32),
    /// 2 字节 ASCII 范围查询：`[prefix, prefix | 0xFF]` 任一键出现即命中。
    PrefixRange { start: u32, end: u32 },
    /// 全部子计划必须同时满足。
    And(Vec<GramPlan>),
    /// 任一子计划满足即保留候选。
    Or(Vec<GramPlan>),
}

/// 为字面量（或符号名）构造倒排前筛计划。
pub fn literal_gram_plan(pattern: &[u8], symbol_only: bool) -> GramPlan {
    let tag = if symbol_only { SYM_KEY_BIT } else { 0 };
    match pattern {
        [] | [_] => GramPlan::All,
        &[a, b] if a < 0x80 && b < 0x80 => {
            let base = tag | ((fold_ascii(a) as u32) << 16) | ((fold_ascii(b) as u32) << 8);
            GramPlan::PrefixRange {
                start: base,
                end: base | 0xFF,
            }
        }
        &[_, _] => GramPlan::All,
        _ => {
            let tris = trigrams_folded(pattern);
            if tris.is_empty() {
                GramPlan::All
            } else if tris.len() == 1 {
                GramPlan::Key(tag | tris[0])
            } else {
                GramPlan::And(tris.into_iter().map(|t| GramPlan::Key(tag | t)).collect())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_fold_and_short_bloom_have_zero_false_negatives() {
        let doc = b"pub fn process_content_index(query: &str) {\n    // HelloWorld\n}\n";
        let grams = extract_grams(doc, true);
        let bloom = build_bloom_bytes(&grams.short_hashes);

        let present = query_short_hashes(b"HELLOWORLD");
        assert!(!present.is_empty());
        assert!(bloom_contains_all(&bloom, &present));

        let present_sym = literal_gram_plan(b"process_content_index", true);
        if let GramPlan::And(parts) = present_sym {
            for part in parts {
                if let GramPlan::Key(k) = part {
                    assert!(grams.keys.contains(&k), "missing symbol key {k:#x}");
                }
            }
        } else {
            panic!("expected And plan for long symbol");
        }
    }

    #[test]
    fn cjk_single_char_produces_single_trigram_and_eof_pair_works() {
        // UTF-8 中单个汉字占 3 字节，恰好构成一个完整三元组
        let plan = literal_gram_plan("中".as_bytes(), false);
        assert!(matches!(plan, GramPlan::Key(_)));

        let doc = b"hi";
        let grams = extract_grams(doc, false);
        assert_eq!(grams.keys, vec![((b'h' as u32) << 16) | ((b'i' as u32) << 8)]);

        let two_ascii = literal_gram_plan(b"HI", false);
        assert_eq!(
            two_ascii,
            GramPlan::PrefixRange {
                start: 0x0068_6900,
                end: 0x0068_69FF
            }
        );
        // 双字节非 ASCII 保持全量回退语义，不误杀多字节边界
        assert_eq!(literal_gram_plan(&[0xE4, 0xB8], false), GramPlan::All);
    }

    #[test]
    fn decode_plain_text_handles_utf8_utf16_binary_and_long_lines() {
        let utf8 = "hello 世界\nline2".as_bytes();
        assert_eq!(decode_plain_text(utf8).as_deref(), Some(utf8));

        let mut utf16_le = vec![0xFFu8, 0xFE];
        for unit in "你好Lokii".encode_utf16() {
            utf16_le.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(
            decode_plain_text(&utf16_le).as_deref(),
            Some("你好Lokii".as_bytes())
        );

        let binary = b"ELF\x00\x01\x02";
        assert!(decode_plain_text(binary).is_none());

        let minified = vec![b'a'; 5000];
        assert!(decode_plain_text(&minified).is_none());
    }

    #[test]
    fn decode_gb18030_chinese_text() {
        // "中文" 在 GBK/GB18030 中的编码为 D6 D0 CE C4
        let raw = b"name=\xD6\xD0\xCE\xC4\n";
        let decoded = decode_plain_text(raw).expect("GB18030 text should decode cleanly");
        assert_eq!(decoded.as_ref(), "name=中文\n".as_bytes());
    }

    #[test]
    fn symbol_boundary_check_distinguishes_definition_from_callsite() {
        let src = b"fn calculate_score(v: u32) {}\nlet x = calculate_score(1);\n";
        assert!(is_symbol_defined_at(src, 3, "calculate_score".len()));
        assert!(!is_symbol_defined_at(src, 38, "calculate_score".len()));
    }
}
