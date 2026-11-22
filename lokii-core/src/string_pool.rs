use crate::compact_index::IndexError;

/// 为每个唯一名计算的 128 位超集特征签名（2 × u64）。
///
/// 布局：
/// - `lo` bits 0..25  (26b): ASCII 字母 a..z 是否出现（大小写折叠）
/// - `lo` bits 26..35 (10b): 数字 0..9 是否出现
/// - `lo` bits 36..50 (15b): 常用符号是否存在
/// - `lo` bit  51     (1b) : 是否包含非 ASCII 字符
/// - `lo` bit  52     (1b) : 是否包含汉字
/// - `lo` bit  53     (1b) : 是否包含日文假名
/// - `lo` bit  54     (1b) : 是否包含韩文谚文
/// - `lo` bit  55     (1b) : 是否包含连续相同 ASCII 字母（如 ll, ss, tt）
/// - `lo` bits 56..63 (8b) : 相邻 ASCII 字母数字 2-gram 哈希桶低 8 位
/// - `hi` bits 0..25  (26b): 词首/边界首字母桶 a..z（首字符、分隔符后字符、驼峰大写）
/// - `hi` bits 26..63 (38b): 相邻 ASCII 字母数字 2-gram 哈希桶高 38 位（共 46 个 2-gram 桶）
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct NameSignature {
    lo: u64,
    hi: u64,
}

impl NameSignature {
    #[inline]
    pub fn from_name(name: &str, folded_non_ascii: Option<&str>) -> Self {
        let folded_cow;
        let target = match folded_non_ascii {
            Some(folded) => folded,
            None if name.is_ascii() => name,
            None => {
                folded_cow = crate::cjk::fold_cow(name);
                folded_cow.as_ref()
            }
        };
        let bytes = target.as_bytes();
        let mut lo = 0u64;
        let mut hi = 0u64;
        let mut prev_folded: u8 = 0;
        let mut prev_is_alnum = false;
        let mut prev_was_upper = false;
        let mut at_boundary = true;

        for (idx, &b) in bytes.iter().enumerate() {
            if b >= 0x80 {
                lo |= 1u64 << 51;
                prev_is_alnum = false;
                at_boundary = true;
                continue;
            }
            let is_upper = b.is_ascii_uppercase();
            let folded = b.to_ascii_lowercase();
            let is_alpha = folded.is_ascii_lowercase();
            let is_digit = folded.is_ascii_digit();

            if is_alpha {
                let bit = (folded - b'a') as u32;
                lo |= 1u64 << bit;
                if at_boundary || (is_upper && !prev_was_upper && idx > 0) {
                    hi |= 1u64 << bit;
                }
                if prev_is_alnum && prev_folded == folded {
                    lo |= 1u64 << 55;
                }
            } else if is_digit {
                let bit = 26 + (folded - b'0') as u32;
                lo |= 1u64 << bit;
            } else {
                if let Some(sym_bit) = symbol_bit(b) {
                    lo |= 1u64 << sym_bit;
                }
            }

            let is_alnum = is_alpha || is_digit;
            if is_alnum && prev_is_alnum {
                let bucket = bigram_bucket(prev_folded, folded);
                if bucket < 8 {
                    lo |= 1u64 << (56 + bucket);
                } else {
                    hi |= 1u64 << (26 + (bucket - 8));
                }
            }

            prev_folded = folded;
            prev_is_alnum = is_alnum;
            prev_was_upper = is_upper;
            at_boundary = matches!(b, b'.' | b'_' | b'-' | b' ' | b'/' | b'\\');
        }

        if (lo & (1u64 << 51)) != 0 {
            for ch in target.chars() {
                if !ch.is_ascii() {
                    apply_non_ascii_signature_bits(ch, true, &mut lo, &mut hi);
                }
            }
        }

        Self { lo, hi }
    }

    /// 根据模糊查询串生成必需签名位（超集过滤掩码）。
    ///
    /// ASCII 字符按字母与数字位收拢；汉字额外要求汉字标记位并收拢其拼音字母位，
    /// 使非 ASCII 查询也能在预筛阶段剪枝，而不是让每个唯一名都进入评分。
    /// 模糊匹配是子序列语义，查询可能被拆成多个空白分隔的子句，
    /// 因此这里只要求字符出现位，不要求符号位、相邻二元组与词首位。
    #[inline]
    pub fn from_fuzzy_query(query: &str) -> Self {
        let norm = crate::cjk::normalize_and_simplify_cow(query);
        let mut lo = 0u64;
        let mut unused_hi = 0u64;
        for ch in norm.chars() {
            if ch.is_ascii() {
                if let Some(bit) = ascii_alnum_bit(ch as u8) {
                    lo |= 1u64 << bit;
                }
            } else {
                apply_non_ascii_signature_bits(ch, false, &mut lo, &mut unused_hi);
            }
        }
        Self { lo, hi: 0 }
    }

    /// 根据查询子串片段生成必需签名位（超集过滤掩码）。
    ///
    /// 非 ASCII 查询先做 NFC 合成与折叠再构建签名，保证与索引端 NFD/NFC 归一化口径保持一致。
    /// 任何包含 `needle` 的文件名，其 `NameSignature` 必然包含返回掩码中的所有 1 位。
    #[inline]
    pub fn from_substring_query(needle_lower: &str) -> Self {
        let folded_cow = crate::cjk::fold_cow(needle_lower);
        let target = folded_cow.as_ref();
        let bytes = target.as_bytes();
        let mut lo = 0u64;
        let mut hi = 0u64;
        let mut prev_folded: u8 = 0;
        let mut prev_is_alnum = false;

        for &b in bytes {
            if b >= 0x80 {
                lo |= 1u64 << 51;
                prev_is_alnum = false;
                continue;
            }
            let folded = b.to_ascii_lowercase();
            let is_alpha = folded.is_ascii_lowercase();
            let is_digit = folded.is_ascii_digit();

            if is_alpha {
                lo |= 1u64 << (folded - b'a');
                if prev_is_alnum && prev_folded == folded {
                    lo |= 1u64 << 55;
                }
            } else if is_digit {
                lo |= 1u64 << (26 + (folded - b'0'));
            } else if let Some(sym_bit) = symbol_bit(b) {
                lo |= 1u64 << sym_bit;
            }

            let is_alnum = is_alpha || is_digit;
            if is_alnum && prev_is_alnum {
                let bucket = bigram_bucket(prev_folded, folded);
                if bucket < 8 {
                    lo |= 1u64 << (56 + bucket);
                } else {
                    hi |= 1u64 << (26 + (bucket - 8));
                }
            }

            prev_folded = folded;
            prev_is_alnum = is_alnum;
        }

        if (lo & (1u64 << 51)) != 0 {
            for ch in target.chars() {
                if !ch.is_ascii() {
                    apply_non_ascii_signature_bits(ch, true, &mut lo, &mut hi);
                }
            }
        }

        Self { lo, hi }
    }

    #[inline]
    pub fn union(self, other: Self) -> Self {
        Self {
            lo: self.lo | other.lo,
            hi: self.hi | other.hi,
        }
    }

    #[inline]
    pub fn contains(self, required: Self) -> bool {
        if (self.lo & required.lo) == required.lo && (self.hi & required.hi) == required.hi {
            return true;
        }
        // 若目标包含 CJK 文字且查询包含拉丁字母，放宽二元组与连字强约束，
        // 校验基础字符（字母、数字、符号及脚本标记位）是否完备存在。
        if (self.lo & (1u64 << 52)) != 0 {
            const LETTERS_MASK: u64 = 0x3FFFFFFu64; // bits 0..25 (a..z)
            const CHARS_MASK: u64 = (1u64 << 55) - 1; // bits 0..54 (基础字符与脚本位)
            let req_letters = required.lo & LETTERS_MASK;
            if req_letters != 0 {
                let req_chars = required.lo & CHARS_MASK;
                if (self.lo & req_chars) == req_chars {
                    return true;
                }
            }
        }
        false
    }
}

/// 为非 ASCII 字符设置统一的签名位，保证建库端与各检索模式查询端保持同一脚本与变音映射口径。
#[inline]
fn apply_non_ascii_signature_bits(
    ch: char,
    include_cjk_initial_hi: bool,
    lo: &mut u64,
    hi: &mut u64,
) {
    let u = ch as u32;
    if (0x4E00..=0x9FFF).contains(&u) || (0x3400..=0x4DBF).contains(&u) {
        *lo |= 1u64 << 52;
        // 遍历繁简归一化后的全部合法读音变体，补充多音字字母位、边界首字母及精确 ü(v)/u 签名位；
        // 严禁在 ASCII 字节路径交叉置 u/v，避免影响纯英文词项的预筛精度。
        for &syllable in crate::pinyin::char_pinyin_variants(ch) {
            for b in syllable.bytes() {
                if b.is_ascii_lowercase() {
                    *lo |= 1u64 << (b - b'a');
                }
            }
            if let Some(&first) = syllable.as_bytes().first() {
                if first.is_ascii_lowercase() {
                    let bit = (first - b'a') as u32;
                    *lo |= 1u64 << bit;
                    if include_cjk_initial_hi {
                        *hi |= 1u64 << bit;
                    }
                }
            }
            if crate::pinyin::is_v_u_syllable(syllable) {
                *lo |= 1u64 << (b'v' - b'a');
            }
        }
    } else if (0x3040..=0x30FF).contains(&u) {
        *lo |= 1u64 << 53;
    } else if (0xAC00..=0xD7AF).contains(&u) || (0x1100..=0x11FF).contains(&u) {
        *lo |= 1u64 << 54;
    } else if u >= 0x80 {
        // 将带重音/变音符的拉丁扩展字符折叠映射至对应的 ASCII 基础字母/数字单字 lo 位，
        // 严禁写入 hi 或二元组位，确保模糊去重音检索与 NFC/NFD 归一化检索均满足超集不变式。
        let norm = nucleo_matcher::chars::normalize(ch);
        if norm.is_ascii() {
            if let Some(bit) = ascii_alnum_bit(norm as u8) {
                *lo |= 1u64 << bit;
            }
        }
    }
}

/// ASCII 字母/数字映射到签名位；其余字节不参与字母数字位。
#[inline]
fn ascii_alnum_bit(b: u8) -> Option<u32> {
    let lower = b.to_ascii_lowercase();
    if lower.is_ascii_lowercase() {
        Some((lower - b'a') as u32)
    } else if lower.is_ascii_digit() {
        Some(26 + (lower - b'0') as u32)
    } else {
        None
    }
}

#[inline]
fn symbol_bit(b: u8) -> Option<u32> {
    match b {
        b'.' => Some(36),
        b'-' => Some(37),
        b'_' => Some(38),
        b' ' => Some(39),
        b'/' => Some(40),
        b'\\' => Some(41),
        b'@' => Some(42),
        b'#' => Some(43),
        b'(' => Some(44),
        b')' => Some(45),
        b'[' => Some(46),
        b']' => Some(47),
        b'{' => Some(48),
        b'}' => Some(49),
        b'+' => Some(50),
        _ => None,
    }
}

/// 将相邻两个 ASCII 字母/数字映射到 0..46 的确定性哈希桶。
#[inline]
fn bigram_bucket(c0: u8, c1: u8) -> u32 {
    let h = (c0 as u32)
        .wrapping_mul(131)
        .wrapping_add(c1 as u32)
        .wrapping_mul(0x9E37_79B1);
    (h >> 24) % 46
}

/// 64 位无二次哈希直通 Hasher：用于 `StringPool::lookup`（键本身已是经充分雪崩混洗的 `u64` 哈希值），
/// 避免 `HashMap` 对 `u64` 键重复运行 SipHash。
#[derive(Default)]
struct IdentityU64Hasher(u64);

impl std::hash::Hasher for IdentityU64Hasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut acc = self.0;
        for &b in bytes {
            acc = acc.wrapping_mul(0x100_0000_01b3) ^ (b as u64);
        }
        self.0 = acc;
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
}

type IdentityBuildHasher = std::hash::BuildHasherDefault<IdentityU64Hasher>;

/// 针对文件名与路径字符串的 64 位高速哈希（8 字节分块 + SplitMix64 终结混洗）：
/// 确保低位桶索引与高 7 位 SIMD 控制标签均充分雪崩，供 `IdentityU64Hasher` 直通查表。
#[inline]
fn hash_str_64(text: &str) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut h = 0x9e37_79b9_7f4a_7c15u64 ^ (text.len() as u64).wrapping_mul(K);
    let bytes = text.as_bytes();
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks {
        let word = u64::from_ne_bytes(chunk.try_into().unwrap());
        h = (h.rotate_left(5) ^ word).wrapping_mul(K);
    }
    let rem = chunks.remainder();
    if !rem.is_empty() {
        let mut tail = 0u64;
        for (i, &b) in rem.iter().enumerate() {
            tail |= (b as u64) << (i * 8);
        }
        h = (h.rotate_left(5) ^ tail).wrapping_mul(K);
    }
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

struct PoolEntry {
    text: Box<str>,
    references: u32,
    next: u32,
}

#[derive(Default)]
pub(crate) struct StringPool {
    slots: Vec<Option<PoolEntry>>,
    lookup: std::collections::HashMap<u64, u32, IdentityBuildHasher>,
    free: Vec<u32>,
    live: usize,
}

impl StringPool {
    pub(crate) fn from_cache<S>(
        strings: Vec<S>,
        references: Vec<u32>,
    ) -> Result<Self, IndexError>
    where
        S: AsRef<str> + Into<Box<str>>,
    {
        if strings.len() != references.len() {
            return Err(IndexError::InvalidReference);
        }
        let mut pool = Self::default();
        pool.slots.reserve(strings.len());
        pool.lookup.reserve(strings.len());
        for (ordinal, (text, references)) in strings.into_iter().zip(references).enumerate() {
            if references == 0 {
                return Err(IndexError::InvalidReference);
            }
            let id = u32::try_from(ordinal).map_err(|_| IndexError::IdOverflow)?;
            if id == u32::MAX {
                return Err(IndexError::IdOverflow);
            }
            let text_ref = text.as_ref();
            let hash = pool.hash(text_ref);
            let next = pool.lookup.get(&hash).copied().unwrap_or(u32::MAX);
            let mut current = next;
            while current != u32::MAX {
                let slot = pool.slots[current as usize]
                    .as_ref()
                    .ok_or(IndexError::InvalidReference)?;
                if slot.text.as_ref() == text_ref {
                    return Err(IndexError::InvalidReference);
                }
                current = slot.next;
            }
            pool.slots.push(Some(PoolEntry {
                text: text.into(),
                references,
                next,
            }));
            pool.lookup.insert(hash, id);
            pool.live += 1;
        }
        Ok(pool)
    }

    #[inline]
    fn hash(&self, text: &str) -> u64 {
        hash_str_64(text)
    }

    pub(crate) fn intern(&mut self, text: &str) -> Result<u32, IndexError> {
        self.intern_hashed(text, self.hash(text))
    }

    pub(crate) fn intern_with_hint(
        &mut self,
        text: &str,
        hint: &mut Option<u32>,
    ) -> Result<u32, IndexError> {
        if let Some(id) = *hint {
            if let Some(Some(slot)) = self.slots.get_mut(id as usize) {
                if slot.text.as_ref() == text {
                    slot.references = slot
                        .references
                        .checked_add(1)
                        .ok_or(IndexError::ReferenceOverflow)?;
                    return Ok(id);
                }
            }
        }
        let id = self.intern(text)?;
        *hint = Some(id);
        Ok(id)
    }

    fn intern_hashed(&mut self, text: &str, hash: u64) -> Result<u32, IndexError> {
        use std::collections::hash_map::Entry;
        let lookup = self.lookup.entry(hash);
        let next = match &lookup {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(_) => u32::MAX,
        };
        let mut current = next;
        while current != u32::MAX {
            let slot = self.slots[current as usize]
                .as_mut()
                .ok_or(IndexError::InvalidReference)?;
            if slot.text.as_ref() == text {
                slot.references = slot
                    .references
                    .checked_add(1)
                    .ok_or(IndexError::ReferenceOverflow)?;
                return Ok(current);
            }
            current = slot.next;
        }
        let id = match self.free.last() {
            Some(&id) => id,
            None => {
                let id = u32::try_from(self.slots.len()).map_err(|_| IndexError::IdOverflow)?;
                if id == u32::MAX {
                    return Err(IndexError::IdOverflow);
                }
                id
            }
        };
        let slot = Some(PoolEntry {
            text: text.into(),
            references: 1,
            next,
        });
        if self.free.pop().is_some() {
            self.slots[id as usize] = slot;
        } else {
            self.slots.push(slot);
        }
        match lookup {
            Entry::Occupied(mut entry) => {
                entry.insert(id);
            }
            Entry::Vacant(entry) => {
                entry.insert(id);
            }
        }
        self.live += 1;
        Ok(id)
    }

    #[inline]
    pub(crate) fn get(&self, id: u32) -> Option<&str> {
        self.slots
            .get(id as usize)?
            .as_ref()
            .map(|slot| slot.text.as_ref())
    }

    #[inline]
    pub(crate) fn ref_count(&self, id: u32) -> usize {
        self.slots
            .get(id as usize)
            .and_then(|s| s.as_ref())
            .map_or(0, |s| s.references as usize)
    }

    pub(crate) fn slot_len(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn find(&self, text: &str) -> Option<u32> {
        let hash = self.hash(text);
        let mut current = *self.lookup.get(&hash)?;
        while current != u32::MAX {
            let slot = self.slots.get(current as usize)?.as_ref()?;
            if slot.text.as_ref() == text {
                return Some(current);
            }
            current = slot.next;
        }
        None
    }

    pub(crate) fn find_matching(&self, mut predicate: impl FnMut(&str) -> bool) -> Option<u32> {
        self.slots.iter().enumerate().find_map(|(idx, slot)| {
            let text = slot.as_ref()?.text.as_ref();
            predicate(text).then_some(idx as u32)
        })
    }

    pub(crate) fn release(&mut self, id: u32) {
        let slot = self.slots[id as usize]
            .as_mut()
            .expect("live pool reference");
        if slot.references > 1 {
            slot.references -= 1;
            return;
        }
        let hash = self.hash(self.get(id).expect("live pool reference"));
        self.release_hashed(id, hash);
    }

    pub(crate) fn matching_ids(&self, predicate: impl FnMut(&str) -> bool) -> Vec<bool> {
        self.evaluate_ids(false, predicate)
    }

    pub(crate) fn evaluate_ids<T: Clone>(
        &self,
        default: T,
        mut mapper: impl FnMut(&str) -> T,
    ) -> Vec<T> {
        self.slots
            .iter()
            .map(|slot| match slot {
                Some(slot) => mapper(&slot.text),
                None => default.clone(),
            })
            .collect()
    }

    fn release_hashed(&mut self, id: u32, hash: u64) {
        let slot = self.slots[id as usize]
            .as_mut()
            .expect("live pool reference");
        slot.references -= 1;
        if slot.references != 0 {
            return;
        }
        let next = slot.next;
        let head = *self
            .lookup
            .get(&hash)
            .expect("pool lookup contains live entry");
        if head == id {
            if next == u32::MAX {
                self.lookup.remove(&hash);
            } else {
                self.lookup.insert(hash, next);
            }
        } else {
            let mut current = head;
            loop {
                let slot = self.slots[current as usize].as_mut().unwrap();
                if slot.next == id {
                    slot.next = next;
                    break;
                }
                current = slot.next;
            }
        }
        self.slots[id as usize] = None;
        self.free.push(id);
        self.live -= 1;
    }

    pub(crate) fn len(&self) -> usize {
        self.live
    }

    pub(crate) fn estimated_bytes(&self) -> usize {
        let buckets = if self.lookup.capacity() == 0 {
            0
        } else {
            self.lookup
                .capacity()
                .saturating_mul(8)
                .div_ceil(7)
                .next_power_of_two()
        };
        self.slots.capacity() * std::mem::size_of::<Option<PoolEntry>>()
            + self.free.capacity() * std::mem::size_of::<u32>()
            + buckets * (std::mem::size_of::<(u64, u32)>() + 1)
            + self
                .slots
                .iter()
                .flatten()
                .map(|slot| slot.text.len())
                .sum::<usize>()
    }

    pub(crate) fn validate_references(&self, references: &[u32]) -> Result<(), IndexError> {
        if references.len() != self.slots.len() {
            return Err(IndexError::InvalidReference);
        }
        let mut live = 0;
        for (ordinal, slot) in self.slots.iter().enumerate() {
            match slot {
                Some(slot) if slot.references == references[ordinal] && references[ordinal] > 0 => {
                    let mut current = self
                        .lookup
                        .get(&self.hash(&slot.text))
                        .copied()
                        .unwrap_or(u32::MAX);
                    while current != u32::MAX && current as usize != ordinal {
                        current = self.slots[current as usize]
                            .as_ref()
                            .ok_or(IndexError::InvalidReference)?
                            .next;
                    }
                    if current == u32::MAX {
                        return Err(IndexError::InvalidReference);
                    }
                    live += 1;
                }
                None if references[ordinal] == 0 => {}
                _ => return Err(IndexError::InvalidReference),
            }
        }
        if live != self.live {
            return Err(IndexError::InvalidReference);
        }
        Ok(())
    }

    pub(crate) fn reference_counts(&self) -> Vec<u32> {
        vec![0; self.slots.len()]
    }

    /// 池内槽位是否完全紧凑无空洞（每个槽位 ID 即其序列化序号，存盘时可免重映射直接借用 records）。
    #[inline]
    pub(crate) fn is_dense(&self) -> bool {
        self.free.is_empty() && self.live == self.slots.len()
    }

    pub(crate) fn cache_strings_dense(&self) -> Vec<&str> {
        debug_assert!(self.is_dense());
        let mut strings = Vec::with_capacity(self.live);
        for slot in &self.slots {
            if let Some(slot) = slot {
                strings.push(&*slot.text);
            }
        }
        strings
    }

    pub(crate) fn cache_strings(&self) -> (Vec<&str>, Vec<u32>) {
        let mut strings = Vec::with_capacity(self.live);
        let mut mapping = vec![u32::MAX; self.slots.len()];
        for (ordinal, slot) in self.slots.iter().enumerate() {
            if let Some(slot) = slot {
                mapping[ordinal] = u32::try_from(strings.len()).expect("pool ID fits u32");
                strings.push(&*slot.text);
            }
        }
        (strings, mapping)
    }

    /// 重映射池中匹配指定前缀的字符串，并重建哈希表与冲突链。
    ///
    /// 遵循目录边界语义：
    /// - 若 `old_prefix` 以 '/' 结尾，匹配所有以此前缀开头的字符串；
    /// - 若 `old_prefix` 不以 '/' 结尾，匹配全等或后接 '/' 的目录前缀（避免误伤如 ShareExtra）。
    /// 重构后重建每个活跃槽位的哈希值与 `next` 链表，若存在重串碰撞则返回错误且保持池状态未修改。
    pub(crate) fn remap_prefix(
        &mut self,
        old_prefix: &str,
        new_prefix: &str,
        case_insensitive: bool,
    ) -> Result<usize, IndexError> {
        if old_prefix == new_prefix {
            return Ok(0);
        }

        let mut staged_replacements: Vec<Option<Box<str>>> = vec![None; self.slots.len()];
        let mut count = 0;

        for (id, slot) in self.slots.iter().enumerate() {
            if let Some(entry) = slot {
                if let Some(remapped) =
                    match_and_remap_prefix(&entry.text, old_prefix, new_prefix, case_insensitive)
                {
                    if remapped.as_str() != entry.text.as_ref() {
                        staged_replacements[id] = Some(remapped.into_boxed_str());
                        count += 1;
                    }
                }
            }
        }

        if count == 0 {
            return Ok(0);
        }

        let mut new_lookup = std::collections::HashMap::with_capacity_and_hasher(
            self.live,
            IdentityBuildHasher::default(),
        );
        let mut new_next = vec![u32::MAX; self.slots.len()];

        for (id, slot) in self.slots.iter().enumerate() {
            if slot.is_some() {
                let text: &str = if let Some(ref remapped) = staged_replacements[id] {
                    remapped.as_ref()
                } else {
                    &slot.as_ref().unwrap().text
                };

                let hash = self.hash(text);
                let prev_head = new_lookup.get(&hash).copied().unwrap_or(u32::MAX);
                let mut current = prev_head;
                while current != u32::MAX {
                    let curr_text: &str =
                        if let Some(ref remapped) = staged_replacements[current as usize] {
                            remapped.as_ref()
                        } else {
                            &self.slots[current as usize].as_ref().unwrap().text
                        };
                    if curr_text == text {
                        return Err(IndexError::InvalidReference);
                    }
                    current = new_next[current as usize];
                }

                new_next[id] = prev_head;
                new_lookup.insert(hash, id as u32);
            }
        }

        for (id, slot) in self.slots.iter_mut().enumerate() {
            if let Some(entry) = slot {
                if let Some(remapped) = staged_replacements[id].take() {
                    entry.text = remapped;
                }
                entry.next = new_next[id];
            }
        }
        self.lookup = new_lookup;

        Ok(count)
    }
}

fn match_and_remap_prefix(
    text: &str,
    old_prefix: &str,
    new_prefix: &str,
    case_insensitive: bool,
) -> Option<String> {
    if old_prefix.is_empty() {
        return None;
    }

    let match_prefix_ci = |haystack: &str, needle: &str| -> Option<usize> {
        if haystack.len() < needle.len() {
            return None;
        }
        if haystack.is_ascii() && needle.is_ascii() {
            if haystack.as_bytes()[..needle.len()].eq_ignore_ascii_case(needle.as_bytes()) {
                return Some(needle.len());
            }
            return None;
        }
        let mut h_chars = haystack.chars();
        let mut n_chars = needle.chars();
        let mut matched_bytes = 0;
        while let Some(nc) = n_chars.next() {
            let Some(hc) = h_chars.next() else { return None; };
            if nc == hc || nc.to_lowercase().eq(hc.to_lowercase()) {
                matched_bytes += hc.len_utf8();
            } else {
                return None;
            }
        }
        Some(matched_bytes)
    };

    if old_prefix.ends_with('/') {
        let matched = if case_insensitive {
            match_prefix_ci(text, old_prefix)
        } else if text.starts_with(old_prefix) {
            Some(old_prefix.len())
        } else {
            None
        };
        matched.map(|bytes| {
            let suffix = &text[bytes..];
            format!("{new_prefix}{suffix}")
        })
    } else {
        let is_exact = if case_insensitive {
            if text.len() == old_prefix.len() {
                if text.is_ascii() && old_prefix.is_ascii() {
                    text.eq_ignore_ascii_case(old_prefix)
                } else {
                    text.to_lowercase() == old_prefix.to_lowercase()
                }
            } else {
                false
            }
        } else {
            text == old_prefix
        };
        if is_exact {
            return Some(new_prefix.to_string());
        }

        let matched = if case_insensitive {
            match_prefix_ci(text, old_prefix)
        } else if text.starts_with(old_prefix) {
            Some(old_prefix.len())
        } else {
            None
        };
        if let Some(bytes) = matched {
            if text[bytes..].starts_with('/') {
                let suffix = &text[bytes..];
                let prefix_trimmed = new_prefix.trim_end_matches('/');
                return Some(format!("{prefix_trimmed}{suffix}"));
            }
        }
        None
    }
}

/// 零分配大小写无关比较与子串查找辅助函数。
#[inline]
pub(crate) fn find_ascii_ignore_case(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let n_len = needle.len();
    if n_len == 0 {
        return Some(0);
    }
    if haystack.len() < n_len {
        return None;
    }
    let first = needle[0].to_ascii_lowercase();
    (0..=haystack.len() - n_len).find(|&i| {
        haystack[i].to_ascii_lowercase() == first
            && haystack[i..i + n_len].eq_ignore_ascii_case(needle)
    })
}

#[inline]
pub(crate) fn ascii_contains_ignore_case(haystack: &[u8], needle: &[u8]) -> bool {
    find_ascii_ignore_case(haystack, needle).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_superset_invariant_holds_for_substrings() {
        let samples = [
            "main.rs",
            "CompactIndex.rs",
            "SearchResultsViewController.swift",
            "2026-10-24-core-engine-v3-design.md",
            "中文文档_测试_001.txt",
            "café_report_v2.pdf",
            "İstanbul.txt",
            "node_modules",
        ];
        for name in samples {
            let lower = name.to_lowercase();
            let folded = (!name.is_ascii()).then_some(lower.as_str());
            let sig = NameSignature::from_name(name, folded);
            let chars: Vec<char> = lower.chars().collect();
            for start in 0..chars.len() {
                for end in start + 1..=chars.len() {
                    let sub: String = chars[start..end].iter().collect();
                    let req = NameSignature::from_substring_query(&sub);
                    assert!(
                        sig.contains(req),
                        "signature for '{name}' must contain query '{sub}'"
                    );
                    if name.is_ascii() {
                        assert!(
                            ascii_contains_ignore_case(name.as_bytes(), sub.as_bytes()),
                            "ascii_contains_ignore_case must match '{sub}' in '{name}'"
                        );
                    }
                }
            }
            if name.is_ascii() {
                assert!(ascii_contains_ignore_case(name.as_bytes(), b""));
                assert!(!ascii_contains_ignore_case(name.as_bytes(), b"z"));
                assert!(!ascii_contains_ignore_case(name.as_bytes(), b"zz"));
            }
        }
    }

    #[test]
    fn hinted_intern_preserves_references_and_rejects_overflow() {
        let mut pool = StringPool::default();
        let mut hint = None;
        let first = pool.intern_with_hint("rs", &mut hint).unwrap();
        assert_eq!(pool.intern_with_hint("rs", &mut hint).unwrap(), first);
        pool.release(first);
        assert_eq!(pool.get(first), Some("rs"));
        pool.slots[first as usize].as_mut().unwrap().references = u32::MAX;
        assert_eq!(
            pool.intern_with_hint("rs", &mut hint),
            Err(IndexError::ReferenceOverflow)
        );
        assert_eq!(
            pool.slots[first as usize].as_ref().unwrap().references,
            u32::MAX
        );
    }

    #[test]
    fn hinted_intern_handles_different_strings_and_reused_slots() {
        let mut pool = StringPool::default();
        let mut hint = None;
        let first = pool.intern_with_hint("first", &mut hint).unwrap();
        pool.release(first);
        let second = pool.intern("second").unwrap();
        assert_eq!(second, first);
        let restored = pool.intern_with_hint("first", &mut hint).unwrap();
        assert_ne!(restored, second);
        assert_eq!(pool.get(restored), Some("first"));
        assert_eq!(pool.get(second), Some("second"));
    }

    #[test]
    fn shared_reference_and_slot_reuse() {
        let mut pool = StringPool::default();
        let id = pool.intern("rs").unwrap();
        assert_eq!(pool.intern("rs").unwrap(), id);
        pool.release(id);
        assert_eq!(pool.get(id), Some("rs"));
        pool.release(id);
        assert_eq!(pool.get(id), None);
        assert_eq!(pool.intern("txt").unwrap(), id);
    }

    #[test]
    fn reference_overflow_does_not_modify_pool() {
        let mut pool = StringPool::default();
        let id = pool.intern("rs").unwrap();
        pool.slots[id as usize].as_mut().unwrap().references = u32::MAX;
        assert!(matches!(
            pool.intern("rs"),
            Err(IndexError::ReferenceOverflow)
        ));
        assert_eq!(
            pool.slots[id as usize].as_ref().unwrap().references,
            u32::MAX
        );
    }

    #[test]
    fn hash_collisions_keep_distinct_names_and_reclaim_chains() {
        let mut pool = StringPool::default();
        let first = pool.intern_hashed("first", 7).unwrap();
        let second = pool.intern_hashed("second", 7).unwrap();
        assert_ne!(first, second);
        assert_eq!(pool.intern_hashed("first", 7).unwrap(), first);
        pool.release_hashed(first, 7);
        pool.release_hashed(second, 7);
        assert_eq!(pool.get(first), Some("first"));
        pool.release_hashed(first, 7);
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn name_signature_superset_invariant_for_unicode_normalization() {
        let exact_pairs = [
            ("cafe\u{0301}.txt", "café"),
            ("café.txt", "cafe\u{0301}"),
            ("\u{1112}\u{1161}\u{11AB}\u{1100}\u{1173}\u{11AF}.txt", "한글"),
            ("한글.txt", "\u{1112}\u{1161}\u{11AB}\u{1100}\u{1173}\u{11AF}"),
            ("開發規範.md", "开发"),
        ];

        for (name, query) in exact_pairs {
            let name_sig = NameSignature::from_name(name, None);
            let sub_req = NameSignature::from_substring_query(query);
            assert!(
                name_sig.contains(sub_req),
                "substring signature superset violated for name={name:?} query={query:?}"
            );
            let fuzzy_req = NameSignature::from_fuzzy_query(query);
            assert!(
                name_sig.contains(fuzzy_req),
                "fuzzy signature superset violated for name={name:?} query={query:?}"
            );
        }

        // 模糊搜索中纯 ASCII "cafe" 必须能匹配 NFD "cafe\u{0301}.txt" 与 NFC "café.txt"
        for name in ["cafe\u{0301}.txt", "café.txt"] {
            let name_sig = NameSignature::from_name(name, None);
            let fuzzy_req = NameSignature::from_fuzzy_query("cafe");
            assert!(
                name_sig.contains(fuzzy_req),
                "fuzzy de-accenting superset violated for name={name:?}"
            );
        }
    }

    #[test]
    fn name_signature_superset_invariant_for_polyphones_and_vu() {
        let polyphone_and_vu_pairs = [
            ("重量.xlsx", "zhongliang"),
            ("重新整理.md", "chongxin"),
            ("银行对账单.pdf", "yinhang"),
            ("行事历.ics", "xingshi"),
            ("長度说明.txt", "changdu"),
            ("长度说明.txt", "changdu"),
            ("调试日志.log", "tiaoshi"),
            ("地图.png", "ditu"),
            ("音樂.mp3", "yinyue"),
            ("音乐.mp3", "yinyue"),
            ("乾坤.txt", "qiankun"),
            ("干坤.txt", "qiankun"),
            ("绿色.txt", "lvse"),
            ("女儿.png", "nver"),
            ("略过.txt", "lve"),
            ("觉得.txt", "jve"),
            ("概率统计.pdf", "gailv"),
            ("2026年财务预算表.xlsx", "2026ncw"),
        ];

        for (name, query) in polyphone_and_vu_pairs {
            let name_sig = NameSignature::from_name(name, None);
            let sub_req = NameSignature::from_substring_query(query);
            assert!(
                name_sig.contains(sub_req),
                "substring signature superset violated for name={name:?} query={query:?}"
            );
            let fuzzy_req = NameSignature::from_fuzzy_query(query);
            assert!(
                name_sig.contains(fuzzy_req),
                "fuzzy signature superset violated for name={name:?} query={query:?}"
            );
        }

        // 纯 ASCII 对照组：严禁在 ASCII 字节路径交叉置 u/v 位，防止 "dev" 与 "use" 误匹配
        let dev_sig = NameSignature::from_name("dev.rs", None);
        let use_sig = NameSignature::from_name("use.rs", None);
        let dev_sub = NameSignature::from_substring_query("dev");
        let use_sub = NameSignature::from_substring_query("use");
        let dev_fuzzy = NameSignature::from_fuzzy_query("dev");
        let use_fuzzy = NameSignature::from_fuzzy_query("use");

        assert!(dev_sig.contains(dev_sub));
        assert!(use_sig.contains(use_sub));
        assert!(!use_sig.contains(dev_sub));
        assert!(!dev_sig.contains(use_sub));
        assert!(!use_sig.contains(dev_fuzzy));
        assert!(!dev_sig.contains(use_fuzzy));

        // 排除非 ü 系 u 音节误带 v 位
        let luan_sig = NameSignature::from_name("乱码.txt", None);
        assert!(!luan_sig.contains(NameSignature::from_substring_query("lv")));
    }

    #[test]
    fn remap_prefix_exact_boundary_and_rebuild_lookup() {
        let mut pool = StringPool::default();
        let p1 = pool.intern("/Volumes/Share/").unwrap();
        let p2 = pool.intern("/Volumes/Share/sub/").unwrap();
        let p3 = pool.intern("/Volumes/ShareExtra/").unwrap();

        let count = pool
            .remap_prefix("/Volumes/Share/", "/Volumes/Share-1/", false)
            .unwrap();
        assert_eq!(count, 2);

        assert_eq!(pool.get(p1), Some("/Volumes/Share-1/"));
        assert_eq!(pool.get(p2), Some("/Volumes/Share-1/sub/"));
        assert_eq!(pool.get(p3), Some("/Volumes/ShareExtra/"));

        assert_eq!(pool.find("/Volumes/Share-1/"), Some(p1));
        assert_eq!(pool.find("/Volumes/Share-1/sub/"), Some(p2));
        assert_eq!(pool.find("/Volumes/ShareExtra/"), Some(p3));
        assert_eq!(pool.find("/Volumes/Share/"), None);

        let mut refs = vec![0u32; pool.slot_len()];
        refs[p1 as usize] = 1;
        refs[p2 as usize] = 1;
        refs[p3 as usize] = 1;
        pool.validate_references(&refs).unwrap();
    }

    #[test]
    fn remap_prefix_rejects_duplicate_collision() {
        let mut pool = StringPool::default();
        let p1 = pool.intern("/Volumes/Share/").unwrap();
        let p2 = pool.intern("/Volumes/Share-1/").unwrap();

        let err = pool
            .remap_prefix("/Volumes/Share/", "/Volumes/Share-1/", false)
            .unwrap_err();
        assert!(matches!(err, IndexError::InvalidReference));

        assert_eq!(pool.get(p1), Some("/Volumes/Share/"));
        assert_eq!(pool.get(p2), Some("/Volumes/Share-1/"));
    }
}
