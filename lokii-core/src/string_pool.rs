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
    pub lo: u64,
    pub hi: u64,
}

impl NameSignature {
    #[inline]
    pub fn from_name(name: &str, folded_non_ascii: Option<&str>) -> Self {
        let target = folded_non_ascii.unwrap_or(name);
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
                let u = ch as u32;
                if (0x4E00..=0x9FFF).contains(&u) || (0x3400..=0x4DBF).contains(&u) {
                    lo |= 1u64 << 52;
                } else if (0x3040..=0x30FF).contains(&u) {
                    lo |= 1u64 << 53;
                } else if (0xAC00..=0xD7AF).contains(&u) {
                    lo |= 1u64 << 54;
                }
            }
        }

        Self { lo, hi }
    }

    /// 根据已经转小写的查询子串片段生成必需签名位（超集过滤掩码）。
    ///
    /// 任何包含 `needle_lower` 的文件名，其 `NameSignature` 必然包含返回掩码中的所有 1 位。
    #[inline]
    pub fn from_substring_query(needle_lower: &str) -> Self {
        let bytes = needle_lower.as_bytes();
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
            for ch in needle_lower.chars() {
                let u = ch as u32;
                if (0x4E00..=0x9FFF).contains(&u) || (0x3400..=0x4DBF).contains(&u) {
                    lo |= 1u64 << 52;
                } else if (0x3040..=0x30FF).contains(&u) {
                    lo |= 1u64 << 53;
                } else if (0xAC00..=0xD7AF).contains(&u) {
                    lo |= 1u64 << 54;
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
        (self.lo & required.lo) == required.lo && (self.hi & required.hi) == required.hi
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

struct PoolEntry {
    text: Box<str>,
    references: u32,
    next: u32,
}

#[derive(Default)]
pub(crate) struct StringPool {
    slots: Vec<Option<PoolEntry>>,
    lookup: std::collections::HashMap<u64, u32>,
    free: Vec<u32>,
    live: usize,
}

impl StringPool {
    pub(crate) fn from_cache(
        strings: Vec<String>,
        references: Vec<u32>,
    ) -> Result<Self, IndexError> {
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
            let hash = pool.hash(&text);
            let next = pool.lookup.get(&hash).copied().unwrap_or(u32::MAX);
            let mut current = next;
            while current != u32::MAX {
                let slot = pool.slots[current as usize]
                    .as_ref()
                    .ok_or(IndexError::InvalidReference)?;
                if slot.text.as_ref() == text {
                    return Err(IndexError::InvalidReference);
                }
                current = slot.next;
            }
            pool.slots.push(Some(PoolEntry {
                text: text.into_boxed_str(),
                references,
                next,
            }));
            pool.lookup.insert(hash, id);
            pool.live += 1;
        }
        Ok(pool)
    }

    fn hash(&self, text: &str) -> u64 {
        use std::hash::BuildHasher;
        self.lookup.hasher().hash_one(text)
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

    pub(crate) fn cache_strings(&self) -> (Vec<String>, Vec<u32>) {
        let mut strings = Vec::with_capacity(self.live);
        let mut mapping = vec![u32::MAX; self.slots.len()];
        for (ordinal, slot) in self.slots.iter().enumerate() {
            if let Some(slot) = slot {
                mapping[ordinal] = u32::try_from(strings.len()).expect("pool ID fits u32");
                strings.push(slot.text.to_string());
            }
        }
        (strings, mapping)
    }
}

/// 零分配大小写无关比较与子串查找辅助函数。
#[inline]
pub(crate) fn ascii_contains_ignore_case(haystack: &[u8], needle_lower: &[u8]) -> bool {
    let n_len = needle_lower.len();
    if n_len == 0 {
        return true;
    }
    if haystack.len() < n_len {
        return false;
    }
    let first = needle_lower[0];
    if n_len == 1 {
        return haystack.iter().any(|&h| h.to_ascii_lowercase() == first);
    }
    let last = needle_lower[n_len - 1];
    let limit = haystack.len() - n_len;
    for i in 0..=limit {
        if haystack[i].to_ascii_lowercase() == first
            && haystack[i + n_len - 1].to_ascii_lowercase() == last
            && haystack[i + 1..i + n_len - 1]
                .iter()
                .zip(&needle_lower[1..n_len - 1])
                .all(|(&h, &n)| h.to_ascii_lowercase() == n)
        {
            return true;
        }
    }
    false
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
}
