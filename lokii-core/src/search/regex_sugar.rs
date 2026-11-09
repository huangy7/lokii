/// 正则语法糖解析产物
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegexSugar {
    /// 剥离定界符后的纯正则表达式文本
    pub pattern: String,
    /// 是否携带尾部 `i` 标志（不区分大小写）
    pub case_insensitive: bool,
}

/// 判定字符是否为正则表达式元字符
#[inline]
pub fn is_regex_metachar(c: char) -> bool {
    matches!(
        c,
        '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\' | '.'
    )
}

/// 扫描并解析行内正则语法糖
///
/// 具备四重守卫判定机制：
/// 1. 长度防御与闭合斜杠逆向定位：输入长度不超过 1024 字节，校验首部与末端闭合定界符的反斜杠转义奇偶性；
/// 2. 标志符合法性检验：目前支持 `i`/`I`（大小写不敏感），任何多余未知字符均判定为路径或非法标志；
/// 3. 中间未转义斜杠排斥：若主体内容中包含未转义的斜杠，判定为多级目录路径，阻断误触；
/// 4. 根路径与纯无元字符保护：针对无元字符且无标志的单段输入（如 `/test/` 或系统根目录），优先交由路径检索分发。
pub fn parse_regex_sugar(input: &str) -> Option<RegexSugar> {
    let trimmed = input.trim();
    // 长度防御截断：超过 1024 字节直接忽略语法糖
    if trimmed.len() > 1024 || !trimmed.starts_with('/') || trimmed.len() < 2 {
        return None;
    }

    // 1. 逆向寻找未转义的末端闭合斜杠
    let bytes = trimmed.as_bytes();
    let mut closing_slash_idx = None;
    let mut flags_start_idx = None;

    // 从尾部向前扫描寻找闭合斜杠与后续标志
    for i in (1..bytes.len()).rev() {
        if bytes[i] == b'/' {
            // 校验反斜杠转义（统计斜杠前连续反斜杠数量）
            let mut backslash_count = 0;
            for j in (0..i).rev() {
                if bytes[j] == b'\\' {
                    backslash_count += 1;
                } else {
                    break;
                }
            }
            if backslash_count % 2 == 0 {
                closing_slash_idx = Some(i);
                flags_start_idx = Some(i + 1);
                break;
            }
        }
    }

    let close_idx = closing_slash_idx?;
    let flags_idx = flags_start_idx?;
    let flags_str = &trimmed[flags_idx..];

    // 2. 标志符合法性校验（目前严格只支持 'i'/'I'，其余任何多余字符均视为路径或非法标志）
    let mut case_insensitive = false;
    for ch in flags_str.chars() {
        match ch {
            'i' | 'I' => case_insensitive = true,
            _ => return None, // 包含非法标志或路径后缀（如 /usr/bin），阻断正则推导
        }
    }

    let inner = &trimmed[1..close_idx];
    if inner.is_empty() {
        return None; // "//" 空正则不激活
    }

    // 3. 校验内部是否包含未转义的中间斜杠（多级路径排斥）
    let mut in_escape = false;
    for b in inner.bytes() {
        if in_escape {
            in_escape = false;
            continue;
        }
        if b == b'\\' {
            in_escape = true;
            continue;
        }
        if b == b'/' {
            return None; // 存在未转义的中间斜杠，判定为绝对路径
        }
    }

    // 4. 根路径与纯无元字符保护
    let has_meta = inner.chars().any(is_regex_metachar);
    if !has_meta && !case_insensitive {
        return None; // 无元字符且无标志，交由路径检索器处理
    }

    Some(RegexSugar {
        pattern: inner.to_string(),
        case_insensitive,
    })
}
