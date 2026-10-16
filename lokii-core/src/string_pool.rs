use std::collections::{hash_map::Entry, HashMap};
use std::hash::BuildHasher;

use crate::compact_index::IndexError;

struct PoolEntry {
    text: Box<str>,
    references: usize,
    next: u32,
}

#[derive(Default)]
pub(crate) struct StringPool {
    slots: Vec<Option<PoolEntry>>,
    lookup: HashMap<u64, u32>,
    free: Vec<u32>,
    live: usize,
}

impl StringPool {
    pub(crate) fn from_cache(
        strings: Vec<String>,
        references: Vec<usize>,
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

    pub(crate) fn get(&self, id: u32) -> Option<&str> {
        self.slots
            .get(id as usize)?
            .as_ref()
            .map(|slot| slot.text.as_ref())
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

    pub(crate) fn validate_references(&self, references: &[usize]) -> Result<(), IndexError> {
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

    pub(crate) fn reference_counts(&self) -> Vec<usize> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hinted_intern_preserves_references_and_rejects_overflow() {
        let mut pool = StringPool::default();
        let mut hint = None;
        let first = pool.intern_with_hint("rs", &mut hint).unwrap();
        assert_eq!(pool.intern_with_hint("rs", &mut hint).unwrap(), first);
        pool.release(first);
        assert_eq!(pool.get(first), Some("rs"));
        pool.slots[first as usize].as_mut().unwrap().references = usize::MAX;
        assert_eq!(
            pool.intern_with_hint("rs", &mut hint),
            Err(IndexError::ReferenceOverflow)
        );
        assert_eq!(
            pool.slots[first as usize].as_ref().unwrap().references,
            usize::MAX
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
        pool.slots[id as usize].as_mut().unwrap().references = usize::MAX;
        assert!(matches!(
            pool.intern("rs"),
            Err(IndexError::ReferenceOverflow)
        ));
        assert_eq!(
            pool.slots[id as usize].as_ref().unwrap().references,
            usize::MAX
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
