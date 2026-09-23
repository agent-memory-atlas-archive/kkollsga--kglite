//! Small invariants shared by the mutable clause pipeline.

use std::sync::{Arc, Mutex};

use crate::graph::languages::cypher::result::{MutationStats, ResultSet};

use super::relationship_identity::StatementRelationshipIdentities;

pub(super) fn check_budget(
    budget: &super::budget::ExecutionBudget,
    stats: &MutationStats,
) -> Result<(), String> {
    let units = stats
        .nodes_created
        .checked_add(stats.relationships_created)
        .and_then(|n| n.checked_add(stats.properties_set))
        .and_then(|n| n.checked_add(stats.nodes_deleted))
        .and_then(|n| n.checked_add(stats.relationships_deleted))
        .and_then(|n| n.checked_add(stats.properties_removed))
        .ok_or_else(|| "Mutation work counter overflow".to_string())?;
    budget.check_work(units, "mutation clauses")
}

pub(super) fn stamp_relationships(
    result: &mut ResultSet,
    identities: &Arc<Mutex<StatementRelationshipIdentities>>,
) {
    let identities = identities.lock().expect("identity lock");
    for row in &mut result.rows {
        for binding in row.edge_bindings.values_mut() {
            if binding.incarnation.is_none() {
                binding.incarnation = Some(identities.capture(binding.edge_index));
            }
        }
    }
}
