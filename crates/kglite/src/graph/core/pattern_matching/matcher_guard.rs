//! The matcher's guard sites for a resolved [`ElementFilter`] (a statement
//! under `FOR VALID_TIME AS OF`). Every node candidate — anchors, pre-bound
//! slots, index and id seeks, the untyped seed the inverted index names — and
//! every fixed hop (the relationship and the node it reaches) is put to the
//! filter; the source end of a hop is a candidate or an earlier hop's target,
//! so both endpoints of every matched relationship are tested.
//!
//! Without a filter each site is one `Option` test. The filtering bodies are
//! `#[cold]` / `#[inline(never)]` so the plain loops keep their register
//! allocation.

use petgraph::graph::{EdgeIndex, NodeIndex};

use super::{NodePattern, PatternExecutor};
use crate::datatypes::values::Value;
use crate::graph::core::graph_filter::ElementFilter;
use crate::graph::schema::{DirGraph, InternedKey};

/// Why a variable-length relationship does not run under a filter yet.
pub(crate) const VAR_LENGTH_NOT_YET: &str =
    "variable-length relationships (and shortestPath) are not available under \
     FOR VALID_TIME AS OF yet; write the hops out, or query without the context";

impl PatternExecutor<'_> {
    /// Every node `pattern` can bind to, before any hop.
    pub(super) fn find_matching_nodes(
        &self,
        pattern: &NodePattern,
    ) -> Result<Vec<NodeIndex>, String> {
        let candidates = self.find_matching_nodes_unguarded(pattern)?;
        Ok(match &self.graph_filter {
            None => candidates,
            Some(filter) => guard_candidates(filter, self.graph, candidates),
        })
    }

    /// Start nodes that came from somewhere other than
    /// [`Self::find_matching_nodes`] (the relationship-type inverted index).
    #[inline]
    pub(super) fn guard_seeds(&self, seeds: Vec<NodeIndex>) -> Vec<NodeIndex> {
        match &self.graph_filter {
            None => seeds,
            Some(filter) => guard_candidates(filter, self.graph, seeds),
        }
    }

    /// The id index's node of `node_type` with id `id` — under a filter, the
    /// visible one (see [`ElementFilter::lookup_id`]).
    #[inline]
    pub(super) fn lookup_node_id(&self, node_type: &str, id: &Value) -> Option<NodeIndex> {
        let hit = self.graph.lookup_by_id_readonly(node_type, id);
        match &self.graph_filter {
            None => hit,
            Some(filter) => filter.lookup_id(self.graph, node_type, id, hit),
        }
    }

    /// Whether the filter hides a fixed hop over `edge` (of type `conn`,
    /// leaving `source`) to `far`.
    #[inline]
    pub(super) fn hop_hidden(
        &self,
        edge: EdgeIndex,
        conn: InternedKey,
        source: NodeIndex,
        far: NodeIndex,
    ) -> bool {
        self.graph_filter
            .as_ref()
            .is_some_and(|filter| hop_rejected(filter, self.graph, edge, conn, source, far))
    }

    /// `Err` when a filter is set: the variable-length expansions do not test
    /// their intermediate nodes yet.
    #[inline]
    pub(super) fn refuse_var_length_under_filter(&self) -> Result<(), String> {
        match self.graph_filter {
            None => Ok(()),
            Some(_) => Err(VAR_LENGTH_NOT_YET.to_string()),
        }
    }
}

#[cold]
#[inline(never)]
fn guard_candidates(
    filter: &ElementFilter,
    graph: &DirGraph,
    mut candidates: Vec<NodeIndex>,
) -> Vec<NodeIndex> {
    candidates.retain(|&idx| filter.admits_node(graph, idx));
    candidates
}

#[cold]
#[inline(never)]
fn hop_rejected(
    filter: &ElementFilter,
    graph: &DirGraph,
    edge: EdgeIndex,
    conn: InternedKey,
    source: NodeIndex,
    far: NodeIndex,
) -> bool {
    !filter.admits_hop(graph, edge, conn, source, far)
}
