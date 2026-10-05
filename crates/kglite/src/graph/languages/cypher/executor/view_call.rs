//! Algorithm procedures under a `FOR VALID_TIME AS OF` context.
//!
//! An algorithm iterates the graph directly, outside the pattern matcher the
//! context guards, so under a context it runs on the valid slice: the
//! elements valid at the statement's instant copied into a graph of their own
//! (`features::temporal::slice`), built once per instant and cached beside
//! the endpoint indexes. A child executor runs the procedure on the slice —
//! its `{node_type, where}` scope, its full-graph guard and its budgets then
//! read the slice, which is the scope intersected with the instant — and
//! each node it yields is mapped back to the base node it was copied from,
//! so the rest of the statement reads the graph's own nodes. The procedures
//! routed here yield no relationship (`every_routed_procedure_is_registered`).
//!
//! Whether the slice fits is known only here: over the slice caps (and, in
//! Disk mode, over the instant-mask cap) the statement is refused naming the
//! cap. When the filter hides nothing at the instant the procedure runs on
//! the graph itself.

use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::graph::features::temporal::instant::slice_for;
use crate::graph::features::temporal::view::ValidSlice;

impl CypherExecutor<'_> {
    /// Run view-routed procedure `proc_name` for one input row: on the valid
    /// slice under a filter, on the graph otherwise.
    pub(super) fn execute_view_routed_call(
        &self,
        proc_name: &str,
        clause: &CallClause,
        mut params: HashMap<String, Value>,
    ) -> Result<Vec<ResultRow>, String> {
        let Some(filter) = self.graph_filter() else {
            return self.validate_and_execute_call_once(proc_name, clause, params);
        };
        // The scope names are checked against the graph: the slice has no
        // relationship-type metadata or schema lock, and a type with no node
        // valid at the instant is empty there, not unknown.
        self.validate_algo_params(proc_name, &mut params)?;
        let slice = self.statement_slice(filter, &clause.procedure_name)?;
        let child = CypherExecutor::with_params(slice.graph(), self.params, self.deadline)
            .with_streaming(self.streaming)
            .with_parallel(self.parallel)
            .with_cancel(self.cancel)
            .with_budget(self.budget.clone())
            .with_statement_instant(self.statement_instant());
        let rows = child.execute_resolved_call_once(proc_name, clause, params);
        self.absorb_warnings(&child);
        let mut rows = rows?;
        for row in &mut rows {
            remap_to_base(row, &slice, proc_name)?;
        }
        Ok(rows)
    }

    /// The statement's valid slice, built (or taken from the graph's cache)
    /// on the first routed call and kept for the rest of the statement.
    fn statement_slice(
        &self,
        filter: &ElementFilter,
        procedure: &str,
    ) -> Result<Arc<ValidSlice>, String> {
        if let Some(slice) = self.view_slice.get() {
            return Ok(Arc::clone(slice));
        }
        let instant = filter
            .instant()
            .ok_or("a valid-time range cannot route a procedure to the valid slice")?;
        let slice = slice_for(self.graph, instant)
            .map_err(|reason| format!("CALL {procedure}() under FOR VALID_TIME AS OF: {reason}"))?;
        Ok(Arc::clone(self.view_slice.get_or_init(|| slice)))
    }
}

/// Point `row`'s node bindings at the base nodes the slice copied.
fn remap_to_base(row: &mut ResultRow, slice: &ValidSlice, proc_name: &str) -> Result<(), String> {
    if !row.edge_bindings.is_empty() || !row.path_bindings.is_empty() {
        return Err(format!(
            "procedure {proc_name} yielded a relationship on the valid slice, which has its \
             own relationship ids"
        ));
    }
    for node in row.node_bindings.values_mut() {
        *node = slice.to_base(*node).ok_or_else(|| {
            format!("procedure {proc_name} yielded a node the valid slice does not hold")
        })?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "view_call_tests.rs"]
mod tests;
