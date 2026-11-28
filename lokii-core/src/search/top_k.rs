use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// A ranked match: `score` is the caller's ranking key (higher is better).
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Candidate<'a, K> {
    pub ordinal: usize,
    pub score: K,
    name: &'a str,
    path: Option<(&'a str, &'a str)>,
}

impl<'a, K> Candidate<'a, K> {
    /// 返回候选条目的文件名，用于与当前堆顶最差项做字典序上界剪枝判断。
    #[inline]
    pub fn name(&self) -> &'a str {
        self.name
    }
}

impl<K: Ord> Ord for Candidate<'_, K> {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .cmp(&self.score)
            .then_with(|| self.name.cmp(other.name))
            .then_with(|| match (self.path, other.path) {
                (Some((left_parent, left_name)), Some((right_parent, right_name))) => left_parent
                    .bytes()
                    .chain(left_name.bytes())
                    .cmp(right_parent.bytes().chain(right_name.bytes())),
                (left, right) => left.cmp(&right),
            })
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
        self.offer_candidate(Candidate {
            ordinal,
            score,
            name,
            path: None,
        });
    }

    pub(crate) fn offer_path(
        &mut self,
        ordinal: usize,
        score: K,
        view: crate::compact_index::EntryView<'a>,
    ) {
        let _ = self.offer_path_tracked(ordinal, score, view);
    }

    pub(crate) fn offer_path_tracked(
        &mut self,
        ordinal: usize,
        score: K,
        view: crate::compact_index::EntryView<'a>,
    ) -> (bool, Option<usize>) {
        let path = view
            .raw_path()
            .map_or((view.parent, view.name), |path| (path, ""));
        self.offer_candidate_tracked(Candidate {
            ordinal,
            score,
            name: view.name,
            path: Some(path),
        })
    }

    fn offer_candidate(&mut self, candidate: Candidate<'a, K>) {
        let _ = self.offer_candidate_tracked(candidate);
    }

    fn offer_candidate_tracked(&mut self, candidate: Candidate<'a, K>) -> (bool, Option<usize>) {
        if self.limit == 0 {
            return (false, None);
        }
        if self.candidates.len() < self.limit {
            self.candidates.push(candidate);
            (true, None)
        } else if let Some(mut worst) = self.candidates.peek_mut() {
            if candidate < *worst {
                let evicted = worst.ordinal;
                *worst = candidate;
                (true, Some(evicted))
            } else {
                (false, None)
            }
        } else {
            (false, None)
        }
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.limit > 0 && self.candidates.len() >= self.limit
    }

    /// 候选堆已满时返回当前堆顶最差元素，供调用方基于分数上界提前剪枝。
    #[inline]
    pub fn worst(&self) -> Option<&Candidate<'a, K>> {
        if self.is_full() {
            self.candidates.peek()
        } else {
            None
        }
    }

    pub fn finish(self) -> Vec<Candidate<'a, K>> {
        self.candidates.into_sorted_vec()
    }
}
