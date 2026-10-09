//! The node slots an overlay allocated where its base holds no live node.
//!
//! Appends past the base's bound are one contiguous ascending run, and a
//! creation-heavy statement makes millions of them, so the run is a bare
//! `Range` (one increment per node). Anything else — a slot the base had
//! vacated, a hole left by removing a node out of the run's middle — goes in a
//! `BTreeSet`. [`ExtraSlots::iter`] merges the two in ascending order, which is
//! the order `node_indices` promises.

use std::collections::btree_set;
use std::ops::Range;

#[derive(Clone, Default)]
pub(crate) struct ExtraSlots {
    run: Range<u32>,
    rest: btree_set::BTreeSet<u32>,
}

impl ExtraSlots {
    pub(crate) fn len(&self) -> usize {
        self.run.len() + self.rest.len()
    }

    /// Add a slot the set does not hold. Extends the run when it is adjacent.
    pub(crate) fn insert(&mut self, slot: u32) {
        if self.run.is_empty() {
            self.run = slot..slot + 1;
        } else if slot == self.run.end {
            self.run.end += 1;
        } else if slot + 1 == self.run.start {
            self.run.start -= 1;
        } else {
            self.rest.insert(slot);
        }
    }

    /// Remove a slot, if held. Newest-first removal (rollback) shrinks the run;
    /// a removal from its middle moves the upper part into the set.
    pub(crate) fn remove(&mut self, slot: u32) {
        if self.run.contains(&slot) {
            if slot + 1 == self.run.end {
                self.run.end -= 1;
            } else if slot == self.run.start {
                self.run.start += 1;
            } else {
                self.rest.extend(slot + 1..self.run.end);
                self.run.end = slot;
            }
        } else {
            self.rest.remove(&slot);
        }
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, slot: u32) -> bool {
        self.run.contains(&slot) || self.rest.contains(&slot)
    }

    pub(crate) fn last(&self) -> Option<u32> {
        let run = (!self.run.is_empty()).then(|| self.run.end - 1);
        run.into_iter().chain(self.rest.last().copied()).max()
    }

    /// `Some(run)` when the set is exactly one contiguous run (or empty).
    pub(crate) fn as_single_run(&self) -> Option<Range<u32>> {
        self.rest.is_empty().then(|| self.run.clone())
    }

    pub(crate) fn iter(&self) -> ExtraIter<'_> {
        let mut rest = self.rest.iter();
        ExtraIter {
            run: self.run.clone(),
            next_rest: rest.next().copied(),
            rest,
        }
    }
}

/// Ascending merge of the run and the set; they never share a slot.
pub struct ExtraIter<'a> {
    run: Range<u32>,
    rest: btree_set::Iter<'a, u32>,
    next_rest: Option<u32>,
}

impl Iterator for ExtraIter<'_> {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<u32> {
        match (self.run.start < self.run.end, self.next_rest) {
            (true, Some(rest)) if rest < self.run.start => {
                self.next_rest = self.rest.next().copied();
                Some(rest)
            }
            (true, _) => self.run.next(),
            (false, Some(rest)) => {
                self.next_rest = self.rest.next().copied();
                Some(rest)
            }
            (false, None) => None,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.run.len() + self.rest.len() + usize::from(self.next_rest.is_some());
        (left, Some(left))
    }
}

impl ExactSizeIterator for ExtraIter<'_> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The run and the set always read as the sorted union of what went in,
    /// whatever order the slots arrive and leave in.
    #[test]
    fn it_reads_as_a_sorted_set_under_random_inserts_and_removals() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |n: u32| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as u32
        };
        for _ in 0..300 {
            let mut set = ExtraSlots::default();
            let mut model = std::collections::BTreeSet::new();
            for _ in 0..80 {
                let slot = next(24);
                if model.contains(&slot) {
                    set.remove(slot);
                    model.remove(&slot);
                } else {
                    set.insert(slot);
                    model.insert(slot);
                }
                assert_eq!(
                    set.iter().collect::<Vec<_>>(),
                    model.iter().copied().collect::<Vec<_>>()
                );
                assert_eq!(set.len(), model.len());
                assert_eq!(set.last(), model.last().copied());
                assert!((0..24).all(|s| set.contains(s) == model.contains(&s)));
            }
        }
    }
}
