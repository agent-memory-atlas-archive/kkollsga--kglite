//! A per-type generation that changes whenever the type's member bucket does.
//!
//! A lazily built id index is a pure function of one type's members (ids are
//! immutable), so a graph that holds the same generation for a type as the
//! graph that built its index may reuse that index (see `IdIndexStore`'s shared
//! slot). The bucket is the membership record: every create, delete and
//! rollback of a node edits it through [`TypeIndexStore`]'s mutators, so
//! bumping there needs no cooperation from the write paths.
//!
//! ## Values are unique, not counted per type
//!
//! Every bump takes the next value of one process-wide counter. A per-type
//! `+1` would let two forks that diverge from the same state (a transaction
//! shell and the graph it was cut from) reach the same number with different
//! members, and a reused index would answer for the wrong one. Distinct states
//! never share a value; equal values mean no edit separates them.
//!
//! ## Bounded
//!
//! `touched` holds only types edited since the store was built or last reset,
//! and every fork copies it. Past [`MAX_TOUCHED`] entries the store starts a
//! new epoch instead, which changes every type's generation at once: a
//! one-off rebuild per type, never a stale reuse.

use rustc_hash::FxHashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Distinct types edited before the store resets to a new epoch.
const MAX_TOUCHED: usize = 4096;

fn fresh() -> u64 {
    NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// Generation per type of one `TypeIndexStore`; cloned with it.
#[derive(Debug, Clone)]
pub(crate) struct TypeGenerations {
    /// What every type not in `touched` reports.
    epoch: u64,
    touched: FxHashMap<String, u64>,
}

impl Default for TypeGenerations {
    fn default() -> Self {
        Self {
            epoch: fresh(),
            touched: FxHashMap::default(),
        }
    }
}

impl TypeGenerations {
    pub(crate) fn current(&self, name: &str) -> u64 {
        self.touched.get(name).copied().unwrap_or(self.epoch)
    }

    /// `name`'s bucket is about to change (or may have): give it a value no
    /// other state holds.
    pub(crate) fn touch(&mut self, name: &str) {
        if let Some(generation) = self.touched.get_mut(name) {
            *generation = fresh();
        } else if self.touched.len() >= MAX_TOUCHED {
            self.reset();
        } else {
            self.touched.insert(name.to_string(), fresh());
        }
    }

    /// Every type's bucket may have changed: whole-store replacement.
    pub(crate) fn reset(&mut self) {
        self.epoch = fresh();
        self.touched.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_touch_changes_only_that_type_and_forks_diverge() {
        let mut parent = TypeGenerations::default();
        let (a, b) = (parent.current("A"), parent.current("B"));
        assert_eq!(a, b, "untouched types share the epoch");
        let mut child = parent.clone();
        parent.touch("A");
        child.touch("A");
        assert_ne!(parent.current("A"), a);
        assert_eq!(parent.current("B"), b);
        assert_ne!(
            parent.current("A"),
            child.current("A"),
            "two forks that both edit A must not agree on its generation"
        );
    }

    #[test]
    fn a_reset_and_the_touched_cap_change_every_type() {
        let mut generations = TypeGenerations::default();
        let before = generations.current("Z");
        generations.reset();
        assert_ne!(generations.current("Z"), before);
        let before = generations.current("Z");
        for n in 0..=MAX_TOUCHED {
            generations.touch(&format!("T{n}"));
        }
        assert_ne!(generations.current("Z"), before);
        assert!(generations.touched.len() <= MAX_TOUCHED);
    }
}
