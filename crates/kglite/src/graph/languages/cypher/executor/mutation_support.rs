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

/// Give every relationship a clause just bound its statement identity token.
///
/// Runs at clause end, so "the token now" is still the token as of the bind —
/// which is what makes it safe: a later clause that retires the slot leaves
/// the stamped token stale, and the stale value is refused instead of silently
/// following the slot to its replacement.
///
/// Path hops are stamped on the same rule as edge bindings, and for the same
/// reason: a `CREATE`/`MERGE` produces bindings with no token, and a path
/// synthesised from them would otherwise carry an untracked hop that every
/// downstream token check must refuse.
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
        for path in row.path_bindings.values_mut() {
            let tokens = path
                .hop_incarnations
                .get_or_insert_with(|| vec![None; path.path.len()]);
            tokens.resize(path.path.len(), None);
            for (token, hop) in tokens.iter_mut().zip(path.path.iter()) {
                if token.is_none() {
                    *token = Some(identities.capture(hop.edge));
                }
            }
        }
    }
}
