use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// A ranked match: `score` is the caller's ranking key (higher is better).
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Candidate<'a, K> {
    pub ordinal: usize,
    pub score: K,
    name: &'a str,
}

impl<K: Ord> Ord for Candidate<'_, K> {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .cmp(&self.score)
            .then_with(|| self.name.cmp(other.name))
            .then_with(|| self.ordinal.cmp(&other.ordinal))
    }
}

impl<K: Ord> PartialOrd for Candidate<'_, K> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub struct TopK<'a, K> {
    limit: usize,
    candidates: BinaryHeap<Candidate<'a, K>>,
}

impl<'a, K: Ord> TopK<'a, K> {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            candidates: BinaryHeap::new(),
        }
    }

    pub fn offer(&mut self, ordinal: usize, score: K, name: &'a str) {
        if self.limit == 0 {
            return;
        }
        let candidate = Candidate {
            ordinal,
            score,
            name,
        };
        if self.candidates.len() < self.limit {
            self.candidates.push(candidate);
        } else if self
            .candidates
            .peek()
            .is_some_and(|worst| candidate < *worst)
        {
            *self.candidates.peek_mut().unwrap() = candidate;
        }
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.limit > 0 && self.candidates.len() >= self.limit
    }

    pub fn finish(self) -> Vec<Candidate<'a, K>> {
        self.candidates.into_sorted_vec()
    }
}
