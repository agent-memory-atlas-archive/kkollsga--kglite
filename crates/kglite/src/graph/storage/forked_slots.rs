//! The node slots an overlay allocated where its base holds no live node.
//!
//! Appends past the base's bound are one contiguous ascending run, and a
//! creation-heavy statement makes millions of them, so the run is a bare
//! `Range` (one increment per node). A node removed out of the run's middle
//! leaves a hole, recorded in a paged set; the run keeps growing past it. A
//! slot the base had vacated and is reused goes in a second paged set. Both
//! sets share their pages between clones, so forking the overlay costs the same
//! however many slots it holds. [`ExtraSlots::iter`] merges the run and the
//! second set in ascending order, which is the order `node_indices` promises.

use std::ops::Range;

use crate::graph::storage::shared_tree::PagedSet;

#[derive(Clone, Default)]
pub(crate) struct ExtraSlots {
    run: Range<u32>,
    /// Slots inside `run` that were removed. The ends of the run are never holes.
    holes: PagedSet,
    /// Slots outside `run`.
    rest: PagedSet,
}

impl ExtraSlots {
    pub(crate) fn len(&self) -> usize {
        self.run.len() - self.holes.len() + self.rest.len()
    }

    /// Add a slot the set does not hold. Extends the run when it is adjacent.
    pub(crate) fn insert(&mut self, slot: u32) {
        if self.run.contains(&slot) {
            self.holes.remove(slot);
        } else if self.run.is_empty() {
            self.run = slot..slot + 1;
        } else if slot == self.run.end {
            self.run.end += 1;
        } else if slot + 1 == self.run.start {
            self.run.start -= 1;
        } else {
            self.rest.insert(slot);
        }
    }

    /// Remove a slot, if held. Removing an end of the run shrinks it (and
    /// drops the holes it exposes); a removal from the middle records a hole.
    pub(crate) fn remove(&mut self, slot: u32) {
        if !self.run.contains(&slot) {
            self.rest.remove(slot);
            return;
        }
        if self.holes.contains(slot) {
            return;
        }
        if slot + 1 == self.run.end {
            self.run.end -= 1;
        } else if slot == self.run.start {
            self.run.start += 1;
        } else {
            self.holes.insert(slot);
            return;
        }
        while self.run.end > self.run.start && self.holes.remove(self.run.end - 1) {
            self.run.end -= 1;
        }
        while self.run.end > self.run.start && self.holes.remove(self.run.start) {
            self.run.start += 1;
        }
        if self.run.is_empty() {
            self.run = 0..0;
        }
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, slot: u32) -> bool {
        (self.run.contains(&slot) && !self.holes.contains(slot)) || self.rest.contains(slot)
    }

    pub(crate) fn last(&self) -> Option<u32> {
        let run = (!self.run.is_empty()).then(|| self.run.end - 1);
        run.into_iter().chain(self.rest.max()).max()
    }

    /// `Some(run)` when the set is exactly one contiguous run (or empty).
    pub(crate) fn as_single_run(&self) -> Option<Range<u32>> {
        (self.rest.is_empty() && self.holes.is_empty()).then(|| self.run.clone())
    }

    /// Slots held outside the run and holes inside it: what a clone shares
    /// as pages rather than copies. Zero for an append-only overlay.
    #[cfg(test)]
    pub(crate) fn scattered(&self) -> usize {
        self.rest.len() + self.holes.len()
    }

    pub(crate) fn iter(&self) -> ExtraIter<'_> {
        let mut rest = self.rest.iter_sorted();
        ExtraIter {
            run: self.run.clone(),
            holes: &self.holes,
            next_rest: rest.next(),
            rest: Box::new(rest),
            left: self.len(),
        }
    }
}

/// Ascending merge of the run (minus its holes) and the set; they never share a slot.
pub struct ExtraIter<'a> {
    run: Range<u32>,
    holes: &'a PagedSet,
    rest: Box<dyn Iterator<Item = u32> + 'a>,
    next_rest: Option<u32>,
    left: usize,
}

impl ExtraIter<'_> {
    fn next_run(&mut self) -> Option<u32> {
        let holes = self.holes;
        self.run
            .by_ref()
            .find(|slot| holes.is_empty() || !holes.contains(*slot))
    }
}

impl Iterator for ExtraIter<'_> {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<u32> {
        let item = match self.next_rest {
            Some(rest) if self.run.start >= self.run.end || rest < self.run.start => {
                self.next_rest = self.rest.next();
                Some(rest)
            }
            _ => self.next_run().or_else(|| {
                let rest = self.next_rest.take();
                if rest.is_some() {
                    self.next_rest = self.rest.next();
                }
                rest
            }),
        };
        if item.is_some() {
            self.left -= 1;
        }
        item
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.left, Some(self.left))
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

    /// A removal from the middle of the run, then more appends, must not turn
    /// the appends into scattered slots: a clone of those costs their count.
    #[test]
    fn appends_after_a_middle_removal_stay_in_the_run() {
        let mut set = ExtraSlots::default();
        for slot in 1000..1100 {
            set.insert(slot);
        }
        set.remove(1050);
        for slot in 1100..51_100 {
            set.insert(slot);
        }
        assert_eq!(set.len(), 50_099);
        assert_eq!(set.scattered(), 1, "one hole, no scattered appends");
        let all: Vec<u32> = set.iter().collect();
        assert_eq!(all.len(), 50_099);
        assert!(all.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(!all.contains(&1050));
        set.insert(1050);
        assert_eq!(set.as_single_run(), Some(1000..51_100));
    }
}
