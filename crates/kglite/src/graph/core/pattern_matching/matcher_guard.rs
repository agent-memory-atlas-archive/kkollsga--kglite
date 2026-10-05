//! The matcher's guard sites for a resolved [`ElementFilter`] (a statement
//! under `FOR VALID_TIME AS OF`). Every node candidate — anchors, pre-bound
//! slots, index and id seeks, the untyped seed the inverted index names — and
//! every fixed hop (the relationship and the node it reaches) is put to the
//! filter; the source end of a hop is a candidate or an earlier hop's target,
//! so both endpoints of every matched relationship are tested. Variable-length
//! segments test each relationship and node they cross in
//! `matcher_var_length_guarded.rs`.
//!
//! Without a filter each site is one `Option` test. The filtering bodies are
//! `#[cold]` / `#[inline(never)]` so the plain loops keep their register
//! allocation.

use petgraph::graph::{EdgeIndex, NodeIndex};

use super::{NodePattern, PatternExecutor};
use crate::datatypes::values::Value;
use crate::graph::core::graph_filter::ElementFilter;
use crate::graph::schema::{DirGraph, InternedKey};

impl PatternExecutor<'_> {
    /// Every node `pattern` can bind to, before any hop.
    pub(super) fn find_matching_nodes(
        &self,
        pattern: &NodePattern,
    ) -> Result<Vec<NodeIndex>, String> {
        self.find_matching_nodes_up_to(pattern, None)
    }

    /// [`Self::find_matching_nodes`] for a caller that keeps at most `cap`
    /// of them and learns only whether more exist: the guard stops at the
    /// first `cap + 1` admitted nodes, which is the prefix the caller would
    /// have kept, and a pattern the filter cannot hide a node of is not put to
    /// it at all.
    pub(super) fn find_matching_nodes_up_to(
        &self,
        pattern: &NodePattern,
        cap: Option<usize>,
    ) -> Result<Vec<NodeIndex>, String> {
        let hides = self
            .graph_filter
            .as_ref()
            .filter(|filter| pattern_may_hide(filter, self.graph, pattern));
        // A hiding filter drops candidates after the scan, so the scan itself
        // cannot stop at the cap.
        let candidates =
            self.find_matching_nodes_unguarded(pattern, cap.filter(|_| hides.is_none()))?;
        Ok(match hides {
            Some(filter) => guard_candidates(filter, self.graph, candidates, cap),
            None => candidates,
        })
    }

    /// Start nodes that came from somewhere other than
    /// [`Self::find_matching_nodes`] (the relationship-type inverted index).
    #[inline]
    pub(super) fn guard_seeds(&self, seeds: Vec<NodeIndex>, cap: Option<usize>) -> Vec<NodeIndex> {
        match &self.graph_filter {
            None => seeds,
            Some(filter) => guard_candidates(filter, self.graph, seeds, cap),
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
}

/// Whether the filter can hide a node `pattern` binds: it names no label, or
/// a label the filter may hide nodes of.
#[cold]
#[inline(never)]
fn pattern_may_hide(filter: &ElementFilter, graph: &DirGraph, pattern: &NodePattern) -> bool {
    let labels = pattern
        .label_alternatives()
        .iter()
        .chain(&pattern.extra_labels)
        .map(String::as_str);
    filter.may_hide_labels(graph, labels)
}

/// The candidates the filter admits, in order; with a `cap`, only the first
/// `cap + 1` of them.
#[cold]
#[inline(never)]
fn guard_candidates(
    filter: &ElementFilter,
    graph: &DirGraph,
    mut candidates: Vec<NodeIndex>,
    cap: Option<usize>,
) -> Vec<NodeIndex> {
    let Some(keep) = cap.map(|cap| cap.saturating_add(1)) else {
        candidates.retain(|&idx| filter.admits_node(graph, idx));
        return candidates;
    };
    let mut admitted = Vec::with_capacity(keep.min(candidates.len()));
    for idx in candidates {
        if filter.admits_node(graph, idx) {
            admitted.push(idx);
            if admitted.len() >= keep {
                break;
            }
        }
    }
    admitted
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
