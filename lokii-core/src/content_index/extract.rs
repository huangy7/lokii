use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use super::scope::MAX_RICH_FILE_BYTES;

/// Office ZIP 容器允许的最大单条目解压后体积（64 MiB）与全包累计解压上限（256 MiB），用于阻断解压炸弹。
pub const MAX_ZIP_ENTRY_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_ZIP_DECOMPRESSED_BYTES: usize = 256 * 1024 * 1024;

#[cfg(target_os = "macos")]
const COMPRESSION_ZLIB: u32 = 0x205;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractError {
    Io(String),
    TooLarge,
    InvalidArchive(&'static str),
    DecompressionFailed,
    ZipBombLimitExceeded,
    UnsupportedFormat,
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::TooLarge => write!(f, "archive exceeds max size"),
            Self::InvalidArchive(msg) => write!(f, "invalid archive: {msg}"),
            Self::DecompressionFailed => write!(f, "deflate decompression failed"),
            Self::ZipBombLimitExceeded => write!(f, "decompression size limit exceeded"),
            Self::UnsupportedFormat => write!(f, "unsupported document format"),
        }
    }
}

impl std::error::Error for ExtractError {}

/// 使用 macOS `libcompression` (`COMPRESSION_ZLIB`) 执行裸 DEFLATE (RFC 1951) 压缩（测试与工具辅助）。
#[cfg(target_os = "macos")]
pub fn deflate_raw(src: &[u8]) -> Option<Vec<u8>> {
    if src.is_empty() {
        return Some(Vec::new());
    }
    let mut dst = vec![0u8; src.len().saturating_add(64)];
    // SAFETY: 输入与输出缓冲各自独立持有合法字节边界。
    let written = unsafe {
        compression_encode_buffer(
            dst.as_mut_ptr(),
            dst.len(),
            src.as_ptr(),
            src.len(),
            std::ptr::null_mut(),
            COMPRESSION_ZLIB,
        )
    };
    if written == 0 {
        return None;
    }
    dst.truncate(written);
    Some(dst)
}

/// 执行裸 DEFLATE 解压并严格校验期望输出上限，防止恶意构造压缩炸弹耗尽内存。
pub fn inflate_raw_deflate(
    compressed: &[u8],
    expected_uncompressed: usize,
    cap_bytes: usize,
) -> Result<Vec<u8>, ExtractError> {
    if expected_uncompressed > cap_bytes {
        return Err(ExtractError::ZipBombLimitExceeded);
    }
    if expected_uncompressed == 0 {
        return Ok(Vec::new());
    }
    #[cfg(target_os = "macos")]
    {
        let mut dst = vec![0u8; expected_uncompressed];
        // SAFETY: `dst` 已分配精确 `expected_uncompressed` 字节，由系统解压缩接口填充并返回解压长度。
        let out_len = unsafe {
            compression_decode_buffer(
                dst.as_mut_ptr(),
                dst.len(),
                compressed.as_ptr(),
                compressed.len(),
                std::ptr::null_mut(),
                COMPRESSION_ZLIB,
            )
        };
        if out_len == 0 {
            return Err(ExtractError::DecompressionFailed);
        }
        dst.truncate(out_len);
        Ok(dst)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = compressed;
        Err(ExtractError::DecompressionFailed)
    }
}

struct ZipEntryMeta {
    name: String,
    method: u16,
    compressed_size: usize,
    uncompressed_size: usize,
    local_header_offset: usize,
}

/// 内存态只读 ZIP 目录解析器（仅读取 Central Directory 后按需提取，不直接在易变化外接盘文件上做 `mmap`）。
struct ZipPackage<'a> {
    data: &'a [u8],
    entries: HashMap<String, ZipEntryMeta>,
    total_decompressed: usize,
}

impl<'a> ZipPackage<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, ExtractError> {
        let eocd_off = find_eocd(data).ok_or(ExtractError::InvalidArchive("missing EOCD"))?;
        let cd_size = read_u32(data, eocd_off + 12) as usize;
        let cd_offset = read_u32(data, eocd_off + 16) as usize;
        let total_entries = read_u16(data, eocd_off + 10) as usize;

        if cd_offset.saturating_add(cd_size) > eocd_off {
            return Err(ExtractError::InvalidArchive("invalid central directory bounds"));
        }

        let mut entries = HashMap::with_capacity(total_entries.min(4096));
        let mut cursor = cd_offset;
        let cd_end = cd_offset + cd_size;

        while cursor + 46 <= cd_end {
            if &data[cursor..cursor + 4] != b"PK\x01\x02" {
                return Err(ExtractError::InvalidArchive("bad central header signature"));
            }
            let method = read_u16(data, cursor + 10);
            let comp_size = read_u32(data, cursor + 20) as usize;
            let uncomp_size = read_u32(data, cursor + 24) as usize;
            let fname_len = read_u16(data, cursor + 28) as usize;
            let extra_len = read_u16(data, cursor + 30) as usize;
            let comment_len = read_u16(data, cursor + 32) as usize;
            let local_off = read_u32(data, cursor + 42) as usize;

            let name_start = cursor + 46;
            let name_end = name_start + fname_len;
            if name_end + extra_len + comment_len > cd_end {
                return Err(ExtractError::InvalidArchive("truncated central entry"));
            }
            let name = String::from_utf8_lossy(&data[name_start..name_end]).into_owned();
            entries.insert(
                name.clone(),
                ZipEntryMeta {
                    name,
                    method,
                    compressed_size: comp_size,
                    uncompressed_size: uncomp_size,
                    local_header_offset: local_off,
                },
            );
            cursor = name_end + extra_len + comment_len;
        }

        Ok(Self {
            data,
            entries,
            total_decompressed: 0,
        })
    }

    fn read_entry(&mut self, name: &str) -> Result<Option<Vec<u8>>, ExtractError> {
        let Some(meta) = self.entries.get(name) else {
            return Ok(None);
        };
        if meta.uncompressed_size > MAX_ZIP_ENTRY_BYTES {
            return Err(ExtractError::ZipBombLimitExceeded);
        }
        if self.total_decompressed.saturating_add(meta.uncompressed_size)
            > MAX_ZIP_DECOMPRESSED_BYTES
        {
            return Err(ExtractError::ZipBombLimitExceeded);
        }

        let lh = meta.local_header_offset;
        if lh + 30 > self.data.len() || &self.data[lh..lh + 4] != b"PK\x03\x04" {
            return Err(ExtractError::InvalidArchive("bad local file header"));
        }
        let l_fname = read_u16(self.data, lh + 26) as usize;
        let l_extra = read_u16(self.data, lh + 28) as usize;
        let payload_start = lh + 30 + l_fname + l_extra;
        let payload_end = payload_start.saturating_add(meta.compressed_size);
        if payload_end > self.data.len() {
            return Err(ExtractError::InvalidArchive("truncated entry payload"));
        }
        let payload = &self.data[payload_start..payload_end];

        let out = match meta.method {
            0 => payload.to_vec(),
            8 => inflate_raw_deflate(payload, meta.uncompressed_size, MAX_ZIP_ENTRY_BYTES)?,
            _ => return Err(ExtractError::UnsupportedFormat),
        };
        self.total_decompressed += out.len();
        Ok(Some(out))
    }

    fn matching_names<F>(&self, mut pred: F) -> Vec<String>
    where
        F: FnMut(&str) -> bool,
    {
        let mut names: Vec<String> = self
            .entries
            .values()
            .filter(|e| pred(&e.name))
            .map(|e| e.name.clone())
            .collect();
        names.sort_by(|a, b| natural_cmp(a, b));
        names
    }
}

fn find_eocd(data: &[u8]) -> Option<usize> {
    if data.len() < 22 {
        return None;
    }
    let min_pos = data.len().saturating_sub(22 + 65_535);
    let mut i = data.len() - 22;
    loop {
        if &data[i..i + 4] == b"PK\x05\x06" {
            return Some(i);
        }
        if i <= min_pos {
            break;
        }
        i -= 1;
    }
    None
}

/// 面向富文档（`.docx` / `.xlsx` / `.pptx`）的受限读入与纯 Rust 正文提取入口。
pub fn extract_office_xml_file(path: &Path) -> Result<String, ExtractError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let mut file = File::open(path).map_err(|e| ExtractError::Io(e.to_string()))?;
    let meta = file.metadata().map_err(|e| ExtractError::Io(e.to_string()))?;
    if meta.len() > MAX_RICH_FILE_BYTES {
        return Err(ExtractError::TooLarge);
    }
    let mut buf = Vec::with_capacity(meta.len() as usize);
    file.read_to_end(&mut buf)
        .map_err(|e| ExtractError::Io(e.to_string()))?;
    extract_office_xml_bytes(&ext, &buf)
}

pub fn extract_office_xml_bytes(ext_lower: &str, zip_bytes: &[u8]) -> Result<String, ExtractError> {
    match ext_lower {
        "docx" => extract_docx_bytes(zip_bytes),
        "xlsx" => extract_xlsx_bytes(zip_bytes),
        "pptx" => extract_pptx_bytes(zip_bytes),
        _ => Err(ExtractError::UnsupportedFormat),
    }
}

pub fn extract_docx_bytes(zip_bytes: &[u8]) -> Result<String, ExtractError> {
    let mut pkg = ZipPackage::parse(zip_bytes)?;
    let mut targets = pkg.matching_names(|n| {
        n == "word/document.xml"
            || (n.starts_with("word/header") && n.ends_with(".xml"))
            || (n.starts_with("word/footer") && n.ends_with(".xml"))
            || n == "word/footnotes.xml"
            || n == "word/comments.xml"
    });
    // 确保主正文始终位于最前。
    if let Some(pos) = targets.iter().position(|n| n == "word/document.xml") {
        let main = targets.remove(pos);
        targets.insert(0, main);
    }

    let mut out = String::new();
    for name in targets {
        if let Some(xml) = pkg.read_entry(&name)? {
            extract_wordprocessing_xml(&xml, &mut out);
        }
    }
    Ok(out.trim_end().to_string())
}

pub fn extract_xlsx_bytes(zip_bytes: &[u8]) -> Result<String, ExtractError> {
    let mut pkg = ZipPackage::parse(zip_bytes)?;
    let sst = match pkg.read_entry("xl/sharedStrings.xml")? {
        Some(bytes) => parse_xlsx_shared_strings(&bytes),
        None => Vec::new(),
    };
    let sheets = pkg.matching_names(|n| n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml"));
    let mut out = String::new();
    for sheet in sheets {
        if let Some(xml) = pkg.read_entry(&sheet)? {
            extract_xlsx_sheet_xml(&xml, &sst, &mut out);
        }
    }
    Ok(out.trim_end().to_string())
}

pub fn extract_pptx_bytes(zip_bytes: &[u8]) -> Result<String, ExtractError> {
    let mut pkg = ZipPackage::parse(zip_bytes)?;
    let slides = pkg.matching_names(|n| {
        (n.starts_with("ppt/slides/slide") || n.starts_with("ppt/notesSlides/notesSlide"))
            && n.ends_with(".xml")
    });
    let mut out = String::new();
    for slide in slides {
        if let Some(xml) = pkg.read_entry(&slide)? {
            extract_drawing_ml_xml(&xml, &mut out);
        }
    }
    Ok(out.trim_end().to_string())
}

fn extract_wordprocessing_xml(xml: &[u8], out: &mut String) {
    let mut i = 0usize;
    let mut in_text = false;
    let mut raw_buf = String::new();

    while i < xml.len() {
        if xml[i] == b'<' {
            let Some(rel_end) = xml[i..].iter().position(|&b| b == b'>') else {
                break;
            };
            let tag = std::str::from_utf8(&xml[i + 1..i + rel_end]).unwrap_or("");
            let name = tag_name(tag);
            let is_closing = tag.starts_with('/');
            if name == "w:t" || name == "w:delText" {
                in_text = !is_closing && !tag.ends_with('/');
                if !in_text && !raw_buf.is_empty() {
                    unescape_xml_into(&raw_buf, out);
                    raw_buf.clear();
                }
            } else if is_closing && name == "w:p" {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            } else if name == "w:br" || name == "w:cr" {
                out.push('\n');
            } else if name == "w:tab" {
                out.push('\t');
            }
            i += rel_end + 1;
        } else {
            let next_tag = xml[i..].iter().position(|&b| b == b'<').unwrap_or(xml.len() - i);
            if in_text {
                raw_buf.push_str(&String::from_utf8_lossy(&xml[i..i + next_tag]));
            }
            i += next_tag;
        }
    }
    if !raw_buf.is_empty() {
        unescape_xml_into(&raw_buf, out);
    }
}

fn extract_drawing_ml_xml(xml: &[u8], out: &mut String) {
    let mut i = 0usize;
    let mut in_text = false;
    let mut raw_buf = String::new();

    while i < xml.len() {
        if xml[i] == b'<' {
            let Some(rel_end) = xml[i..].iter().position(|&b| b == b'>') else {
                break;
            };
            let tag = std::str::from_utf8(&xml[i + 1..i + rel_end]).unwrap_or("");
            let name = tag_name(tag);
            let is_closing = tag.starts_with('/');
            if name == "a:t" {
                in_text = !is_closing && !tag.ends_with('/');
                if !in_text && !raw_buf.is_empty() {
                    unescape_xml_into(&raw_buf, out);
                    raw_buf.clear();
                }
            } else if is_closing && name == "a:p" {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            } else if name == "a:br" {
                out.push('\n');
            }
            i += rel_end + 1;
        } else {
            let next_tag = xml[i..].iter().position(|&b| b == b'<').unwrap_or(xml.len() - i);
            if in_text {
                raw_buf.push_str(&String::from_utf8_lossy(&xml[i..i + next_tag]));
            }
            i += next_tag;
        }
    }
    if !raw_buf.is_empty() {
        unescape_xml_into(&raw_buf, out);
    }
}

fn parse_xlsx_shared_strings(xml: &[u8]) -> Vec<String> {
    let mut sst = Vec::new();
    let mut cur_item = String::new();
    let mut raw_text = String::new();
    let mut in_si = false;
    let mut in_t = false;
    let mut i = 0usize;

    while i < xml.len() {
        if xml[i] == b'<' {
            let Some(rel_end) = xml[i..].iter().position(|&b| b == b'>') else {
                break;
            };
            let tag = std::str::from_utf8(&xml[i + 1..i + rel_end]).unwrap_or("");
            let name = tag_name(tag);
            let is_closing = tag.starts_with('/');
            if name == "si" {
                if is_closing {
                    sst.push(std::mem::take(&mut cur_item));
                    in_si = false;
                } else {
                    cur_item.clear();
                    in_si = !tag.ends_with('/');
                }
            } else if in_si && name == "t" {
                if !is_closing && !tag.ends_with('/') {
                    in_t = true;
                    raw_text.clear();
                } else if is_closing && in_t {
                    unescape_xml_into(&raw_text, &mut cur_item);
                    raw_text.clear();
                    in_t = false;
                }
            }
            i += rel_end + 1;
        } else {
            let next_tag = xml[i..].iter().position(|&b| b == b'<').unwrap_or(xml.len() - i);
            if in_t {
                raw_text.push_str(&String::from_utf8_lossy(&xml[i..i + next_tag]));
            }
            i += next_tag;
        }
    }
    sst
}

fn extract_xlsx_sheet_xml(xml: &[u8], sst: &[String], out: &mut String) {
    let mut i = 0usize;
    let mut cell_is_shared = false;
    let mut cell_is_inline_str = false;
    let mut in_v = false;
    let mut in_t = false;
    let mut val_buf = String::new();
    let mut row_has_cell = false;

    while i < xml.len() {
        if xml[i] == b'<' {
            let Some(rel_end) = xml[i..].iter().position(|&b| b == b'>') else {
                break;
            };
            let tag = std::str::from_utf8(&xml[i + 1..i + rel_end]).unwrap_or("");
            let name = tag_name(tag);
            let is_closing = tag.starts_with('/');
            match name {
                "row" => {
                    if !is_closing {
                        row_has_cell = false;
                    } else if row_has_cell {
                        out.push('\n');
                    }
                }
                "c" => {
                    if !is_closing {
                        cell_is_shared = tag.contains("t=\"s\"") || tag.contains("t='s'");
                        cell_is_inline_str = tag.contains("t=\"str\"") || tag.contains("t='str'");
                    } else {
                        cell_is_shared = false;
                        cell_is_inline_str = false;
                    }
                }
                "v" => {
                    if !is_closing && !tag.ends_with('/') {
                        in_v = true;
                        val_buf.clear();
                    } else if is_closing && in_v {
                        in_v = false;
                        if row_has_cell {
                            out.push('\t');
                        }
                        if cell_is_shared {
                            if let Ok(idx) = val_buf.trim().parse::<usize>() {
                                if let Some(s) = sst.get(idx) {
                                    out.push_str(s);
                                    row_has_cell = true;
                                }
                            }
                        } else if cell_is_inline_str {
                            unescape_xml_into(&val_buf, out);
                            row_has_cell = true;
                        } else if !val_buf.trim().is_empty() {
                            out.push_str(val_buf.trim());
                            row_has_cell = true;
                        }
                    }
                }
                "t" => {
                    if !is_closing && !tag.ends_with('/') {
                        in_t = true;
                        val_buf.clear();
                    } else if is_closing && in_t {
                        in_t = false;
                        if row_has_cell {
                            out.push('\t');
                        }
                        unescape_xml_into(&val_buf, out);
                        row_has_cell = true;
                    }
                }
                _ => {}
            }
            i += rel_end + 1;
        } else {
            let next_tag = xml[i..].iter().position(|&b| b == b'<').unwrap_or(xml.len() - i);
            if in_v || in_t {
                val_buf.push_str(&String::from_utf8_lossy(&xml[i..i + next_tag]));
            }
            i += next_tag;
        }
    }
}

/// 剥离 HTML 标签并解码实体，跳过 `<script>` / `<style>` 与注释块，适用于 HTML 摘要清洗。
pub fn strip_html_to_text(html: &[u8]) -> String {
    let text = String::from_utf8_lossy(html);
    let mut out = String::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut skip_until_tag: Option<&str> = None;

    while i < bytes.len() {
        if bytes[i] == b'<' {
            if bytes[i..].starts_with(b"<!--") {
                if let Some(end) = find_subslice(&bytes[i + 4..], b"-->") {
                    i = i + 4 + end + 3;
                    continue;
                }
                break;
            }
            let Some(rel_end) = bytes[i..].iter().position(|&b| b == b'>') else {
                break;
            };
            let tag_inner = &text[i + 1..i + rel_end];
            let lower_name = tag_name(tag_inner).to_ascii_lowercase();
            let is_closing = tag_inner.starts_with('/');

            if let Some(target) = skip_until_tag {
                if is_closing && lower_name == target {
                    skip_until_tag = None;
                }
                i += rel_end + 1;
                continue;
            }
            if !is_closing && !tag_inner.ends_with('/') {
                if lower_name == "script" {
                    skip_until_tag = Some("script");
                } else if lower_name == "style" {
                    skip_until_tag = Some("style");
                }
            }
            if matches!(
                lower_name.as_str(),
                "p" | "div" | "br" | "tr" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            ) && !out.is_empty()
                && !out.ends_with('\n')
            {
                out.push('\n');
            }
            i += rel_end + 1;
        } else {
            let next_tag = bytes[i..].iter().position(|&b| b == b'<').unwrap_or(bytes.len() - i);
            if skip_until_tag.is_none() {
                unescape_xml_into(&text[i..i + next_tag], &mut out);
            }
            i += next_tag;
        }
    }
    out.trim().to_string()
}

/// 还原 JSON 字符串中的 `\uXXXX`（含 UTF-16 代理对），使转义形式的中日韩或符号文本直接可搜。
pub fn unescape_json_unicode(input: &str) -> String {
    if !input.contains("\\u") {
        return input.to_string();
    }
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if i + 6 <= bytes.len() && bytes[i] == b'\\' && bytes[i + 1] == b'u' {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 2..i + 6]) {
                if let Ok(code1) = u16::from_str_radix(hex, 16) {
                    if (0xD800..=0xDBFF).contains(&code1)
                        && i + 12 <= bytes.len()
                        && bytes[i + 6] == b'\\'
                        && bytes[i + 7] == b'u'
                    {
                        if let Ok(hex2) = std::str::from_utf8(&bytes[i + 8..i + 12]) {
                            if let Ok(code2) = u16::from_str_radix(hex2, 16) {
                                if (0xDC00..=0xDFFF).contains(&code2) {
                                    let cp = 0x10000
                                        + (((code1 as u32 - 0xD800) << 10)
                                            | (code2 as u32 - 0xDC00));
                                    if let Some(ch) = char::from_u32(cp) {
                                        out.push(ch);
                                        i += 12;
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                    if let Some(ch) = char::from_u32(code1 as u32) {
                        out.push(ch);
                        i += 6;
                        continue;
                    }
                }
            }
        }
        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn tag_name(raw_tag: &str) -> &str {
    let trimmed = raw_tag.trim().trim_start_matches('/').trim_end_matches('/');
    trimmed
        .split(|c: char| c.is_ascii_whitespace())
        .next()
        .unwrap_or("")
}

fn unescape_xml_into(s: &str, out: &mut String) {
    if !s.contains('&') {
        out.push_str(s);
        return;
    }
    let mut i = 0usize;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if let Some(rel_semi) = bytes[i..].iter().take(12).position(|&b| b == b';') {
                let ent = &s[i + 1..i + rel_semi];
                let decoded = match ent {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                        u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32)
                    }
                    _ if ent.starts_with('#') => {
                        ent[1..].parse::<u32>().ok().and_then(char::from_u32)
                    }
                    _ => None,
                };
                if let Some(ch) = decoded {
                    out.push(ch);
                    i += rel_semi + 1;
                    continue;
                }
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let mut ia = 0usize;
    let mut ib = 0usize;
    while ia < ab.len() && ib < bb.len() {
        if ab[ia].is_ascii_digit() && bb[ib].is_ascii_digit() {
            let start_a = ia;
            while ia < ab.len() && ab[ia].is_ascii_digit() {
                ia += 1;
            }
            let start_b = ib;
            while ib < bb.len() && bb[ib].is_ascii_digit() {
                ib += 1;
            }
            let na: u64 = std::str::from_utf8(&ab[start_a..ia])
                .unwrap_or("0")
                .parse()
                .unwrap_or(0);
            let nb: u64 = std::str::from_utf8(&bb[start_b..ib])
                .unwrap_or("0")
                .parse()
                .unwrap_or(0);
            if na != nb {
                return na.cmp(&nb);
            }
        } else {
            if ab[ia] != bb[ib] {
                return ab[ia].cmp(&bb[ib]);
            }
            ia += 1;
            ib += 1;
        }
    }
    ab.len().cmp(&bb.len())
}

#[inline]
fn read_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

#[inline]
fn read_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造标准无第三方依赖的最小测试用 ZIP 打包器，支持 Stored 与 DEFLATE(Method 8)。
    fn build_test_zip(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();

        for (name, raw, use_deflate) in entries {
            let local_off = out.len() as u32;
            let (method, payload) = if *use_deflate {
                #[cfg(target_os = "macos")]
                {
                    (8u16, deflate_raw(raw).unwrap())
                }
                #[cfg(not(target_os = "macos"))]
                {
                    (0u16, raw.to_vec())
                }
            } else {
                (0u16, raw.to_vec())
            };

            let name_bytes = name.as_bytes();
            // Local file header
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name_bytes);
            out.extend_from_slice(&payload);

            // Central directory header
            central.extend_from_slice(b"PK\x01\x02");
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&method.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            central.extend_from_slice(&(raw.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&local_off.to_le_bytes());
            central.extend_from_slice(name_bytes);
        }

        let cd_off = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);

        // End of central directory
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn extracts_docx_xlsx_and_pptx_deflated_archives() {
        let docx_xml = br#"<w:document><w:body><w:p><w:r><w:t>Hello &amp; World</w:t></w:r></w:p><w:p><w:r><w:t>Second Line &#20013;&#25991;</w:t></w:r></w:p></w:body></w:document>"#;
        let docx = build_test_zip(&[("word/document.xml", docx_xml, true)]);
        assert_eq!(
            extract_docx_bytes(&docx).unwrap(),
            "Hello & World\nSecond Line 中文"
        );

        let sst = br#"<sst><si><t>Quarterly Revenue</t></si><si><t>Tokyo</t></si></sst>"#;
        let sheet = br#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1"><v>420</v></c></row></sheetData></worksheet>"#;
        let xlsx = build_test_zip(&[
            ("xl/sharedStrings.xml", sst, true),
            ("xl/worksheets/sheet1.xml", sheet, true),
        ]);
        assert_eq!(
            extract_xlsx_bytes(&xlsx).unwrap(),
            "Quarterly Revenue\tTokyo\t420"
        );

        let slide2 = br#"<p:sld><p:cSld><p:spTree><a:p><a:r><a:t>Slide Two</a:t></a:r></a:p></p:spTree></p:cSld></p:sld>"#;
        let slide10 = br#"<p:sld><p:cSld><p:spTree><a:p><a:r><a:t>Slide Ten</a:t></a:r></a:p></p:spTree></p:cSld></p:sld>"#;
        let pptx = build_test_zip(&[
            ("ppt/slides/slide10.xml", slide10, true),
            ("ppt/slides/slide2.xml", slide2, true),
        ]);
        assert_eq!(extract_pptx_bytes(&pptx).unwrap(), "Slide Two\nSlide Ten");
    }

    #[test]
    fn html_stripping_and_json_unicode_unescaping() {
        let html = b"<html><head><style>.a{color:red}</style></head><body><h1>Title</h1><script>var secret=1;</script><p>Hi &amp; Bye</p></body></html>";
        assert_eq!(strip_html_to_text(html), "Title\nHi & Bye");

        let json_raw = r#"{"msg":"\u4e2d\u6587\u641c\u7d22 \u0026 emoji_ok \ud83d\ude00"}"#;
        assert_eq!(unescape_json_unicode(json_raw), "{\"msg\":\"中文搜索 & emoji_ok \u{1F600}\"}");
    }
}
