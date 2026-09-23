use crate::datatypes::values::RelationshipIncarnation;
use petgraph::graph::EdgeIndex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_STATEMENT_NONCE: AtomicU64 = AtomicU64::new(1);

/// Sparse relationship incarnation state owned by one Cypher statement.
/// Untouched slots are generation zero and consume no map entry.
pub(super) struct StatementRelationshipIdentities {
    nonce: u64,
    generations: HashMap<EdgeIndex, u32>,
}

impl StatementRelationshipIdentities {
    pub(super) fn new() -> Self {
        let nonce = NEXT_STATEMENT_NONCE.fetch_add(1, Ordering::Relaxed);
        Self {
            nonce,
            generations: HashMap::new(),
        }
    }

    pub(super) fn capture(&self, edge: EdgeIndex) -> RelationshipIncarnation {
        RelationshipIncarnation::new(
            self.nonce,
            self.generations.get(&edge).copied().unwrap_or(0),
        )
    }

    /// Retire every binding captured before this deletion. Call immediately
    /// before physical removal, so a CREATE that reuses the slot captures the
    /// incremented generation and remains valid.
    pub(super) fn invalidate(&mut self, edge: EdgeIndex) -> Result<(), String> {
        let generation = self.generations.entry(edge).or_default();
        *generation = generation
            .checked_add(1)
            .ok_or_else(|| format!("relationship slot {} incarnation overflow", edge.index()))?;
        Ok(())
    }

    pub(super) fn accepts(&self, edge: EdgeIndex, token: RelationshipIncarnation) -> bool {
        token == self.capture(edge)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_capture_is_rejected_but_fresh_reuse_is_valid() {
        let edge = EdgeIndex::new(4);
        let mut ids = StatementRelationshipIdentities::new();
        let stale = ids.capture(edge);
        ids.invalidate(edge).unwrap();
        let fresh = ids.capture(edge);
        assert!(!ids.accepts(edge, stale));
        assert!(ids.accepts(edge, fresh));
        assert_ne!(stale, fresh);
    }

    #[test]
    fn statements_never_accept_each_others_tokens() {
        let edge = EdgeIndex::new(0);
        let first = StatementRelationshipIdentities::new();
        let second = StatementRelationshipIdentities::new();
        assert!(!second.accepts(edge, first.capture(edge)));
    }
}
