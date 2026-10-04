//! The per-group counter of the fused grouped counts, served from a cached
//! [`PeerHist`] when one applies.
//!
//! `MATCH (a:A)-[:T]->(b:B) RETURN b.p, count(a)` and its `WITH` and
//! `OPTIONAL MATCH` forms count, once per group node, the relationships of
//! one type to peers of one label. On the in-memory store each count walks
//! every relationship incident to the node; one pass over the relationships
//! of the type counts them for every node at once. [`GroupCounter`] is one
//! operator's counter: it walks exactly as before until the walks have cost
//! about what that pass costs, then builds the histogram and reads it.
//!
//! The histogram serves only a count it answers exactly: a non-distinct
//! count, one relationship type, no relationship or peer property filter, a
//! label constraint the peer check can evaluate, on an in-memory store, with
//! no filter or one whose masks alone decide admission. Anything else keeps
//! the walk. The two stores that index adjacency by type keep it too.

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use petgraph::graph::NodeIndex;
use petgraph::Direction;

use super::match_clause::{bound_hop, simple_node_edge_node};
use super::*;
use crate::graph::core::pattern_matching::pattern::ConnTypeFilter;
use crate::graph::core::pattern_matching::{EdgePattern, NodePattern};
use crate::graph::features::temporal::endpoint_index::unfiltered_peer_hists;
use crate::graph::features::temporal::peer_hist::{
    build_threshold_ns, HistDir, HistKey, HistSlot, PeerHist, HIST_BYTE_CAP,
};

/// What a counter resolved once, from its first count.
struct Route {
    slot: Arc<HistSlot>,
    conn: InternedKey,
    dir: HistDir,
    /// The peer pattern the histogram was keyed on, by address: a row that
    /// binds the other end of the hop counts a different peer and walks.
    other: usize,
    threshold_ns: u64,
}

/// One fused operator's counter for a hop pattern, shared by the rows (and
/// threads) the operator counts on.
#[derive(Default)]
pub(in crate::graph::languages::cypher::executor) struct GroupCounter {
    route: OnceLock<Option<Route>>,
    hist: OnceLock<Arc<PeerHist>>,
}

fn hist_dir(dirs: &[Direction]) -> Option<HistDir> {
    match dirs {
        [Direction::Outgoing] => Some(HistDir::Out),
        [Direction::Incoming] => Some(HistDir::In),
        [Direction::Outgoing, Direction::Incoming] => Some(HistDir::Both),
        _ => None,
    }
}

impl GroupCounter {
    /// The non-distinct count of `pattern` from the node `bindings` binds:
    /// what `try_count_simple_pattern` answers, read from the histogram once
    /// that exists.
    pub(in crate::graph::languages::cypher::executor) fn count(
        &self,
        exec: &CypherExecutor<'_>,
        pattern: &Pattern,
        bindings: &Bindings<NodeIndex>,
    ) -> Result<Option<i64>, String> {
        let Some((node_a, edge, node_b)) = simple_node_edge_node(pattern) else {
            return exec.try_count_simple_pattern(pattern, bindings);
        };
        let Some(hop) = bound_hop(node_a, edge, node_b, |v| bindings.get(v).copied()) else {
            return exec.try_count_simple_pattern(pattern, bindings);
        };
        let route = self
            .route
            .get_or_init(|| exec.peer_hist_route(edge, hop.other, hist_dir(hop.traverse_dirs)));
        let Some(route) = route else {
            return exec.try_count_simple_pattern(pattern, bindings);
        };
        if route.other != std::ptr::from_ref(hop.other) as usize
            || hist_dir(hop.traverse_dirs) != Some(route.dir)
        {
            return exec.try_count_simple_pattern(pattern, bindings);
        }
        let hist = match self.hist.get() {
            Some(hist) => Some(hist),
            None => route
                .slot
                .built()
                .map(|hist| self.hist.get_or_init(|| Arc::clone(hist))),
        };
        let hist = match hist {
            Some(hist) => hist,
            None if route.slot.due(route.threshold_ns) => {
                let built = route
                    .slot
                    .build_once(|| exec.build_peer_hist(route, hop.other))?;
                self.hist.get_or_init(|| built)
            }
            None => {
                let started = Instant::now();
                let counted = exec.try_count_simple_pattern(pattern, bindings);
                route
                    .slot
                    .charge(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
                return counted;
            }
        };
        peer_hist_probe::served();
        Ok(Some(hist.get(hop.bound_idx)))
    }
}

impl CypherExecutor<'_> {
    /// The histogram a count over this hop may be served from, or `None`
    /// when the count must walk (see the module docs for what qualifies).
    fn peer_hist_route(
        &self,
        edge: &EdgePattern,
        other: &NodePattern,
        dir: Option<HistDir>,
    ) -> Option<Route> {
        let graph = self.graph;
        if !graph.graph.is_memory()
            || edge.edge_filter.is_some()
            || other.properties.is_some()
            || (graph.graph.node_bound() * size_of::<u32>()) > HIST_BYTE_CAP
        {
            return None;
        }
        let ConnTypeFilter::One(conn) = edge.conn_filter() else {
            return None;
        };
        let dir = dir?;
        let key = HistKey::new(conn, dir, other.label_alternatives(), &other.extra_labels);
        let slot = match self.graph_filter() {
            None => unfiltered_peer_hists(graph).slot(&key),
            Some(filter) => filter.decisive_masks()?.peer_hists.slot(&key),
        };
        Some(Route {
            slot,
            conn,
            dir,
            other: std::ptr::from_ref(other) as usize,
            threshold_ns: build_threshold_ns(graph.graph.edge_count()),
        })
    }

    /// One pass over the relationships of the route's type: each admitted
    /// relationship adds one to the grouped node whose peer the label check
    /// accepts. An undirected hop counts a self-loop once, as the walk does.
    fn build_peer_hist(&self, route: &Route, other: &NodePattern) -> Result<PeerHist, String> {
        let graph = self.graph;
        self.budget
            .check_work(graph.graph.edge_count(), "grouped relationship count")?;
        peer_hist_probe::built();
        let label = self.pattern_label_check(other);
        let filter = self.graph_filter();
        let mut counts = vec![0u32; graph.graph.node_bound()];
        for (i, edge) in graph.graph.edge_references().enumerate() {
            self.check_interrupt_periodic(i)?;
            if edge.connection_type() != route.conn {
                continue;
            }
            let (source, target) = (edge.source(), edge.target());
            if filter.is_some_and(|f| {
                !f.admits_relationship(graph, edge.id(), route.conn, source, target)
            }) {
                continue;
            }
            if matches!(route.dir, HistDir::Out | HistDir::Both) && label.matches(graph, target) {
                counts[source.index()] += 1;
            }
            let reverse = match route.dir {
                HistDir::In => true,
                HistDir::Both => source != target,
                HistDir::Out => false,
            };
            if reverse && label.matches(graph, source) {
                counts[target.index()] += 1;
            }
        }
        Ok(PeerHist::new(counts))
    }
}

/// Histograms built and counts served from one, per thread in tests, so a
/// test can prove which route a count took. A no-op outside tests.
pub(crate) mod peer_hist_probe {
    #[cfg(test)]
    thread_local! {
        static BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        static SERVED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    #[inline]
    pub(super) fn built() {
        #[cfg(test)]
        BUILT.with(|c| c.set(c.get() + 1));
    }

    #[inline]
    pub(super) fn served() {
        #[cfg(test)]
        SERVED.with(|c| c.set(c.get() + 1));
    }

    /// `(builds, served counts)` since the last call.
    #[cfg(test)]
    pub(crate) fn take() -> (usize, usize) {
        (BUILT.with(|c| c.replace(0)), SERVED.with(|c| c.replace(0)))
    }
}
