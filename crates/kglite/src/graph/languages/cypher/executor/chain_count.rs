//! `Clause::FusedChainPathCount`: the number of paths of a linear chain,
//! counted by a forward degree-product DP instead of enumerating them.
//!
//! `count[v]` after hop `i` is the number of partial paths that end at `v`;
//! one hop moves each frontier node's count along its admitted relationships
//! to the peer's entry. The frontier is sparse (a map keyed by node), so an
//! anchored chain costs O(the part of the graph it reaches), never a type
//! scan. Every test the matcher applies to a hop is applied here per
//! relationship and per peer: connection type, the relationship's inline
//! filters and property map, the peer's labels and property map, and — under
//! a valid-time filter — the relationship and both endpoints.
//!
//! The matcher also refuses a path that uses one relationship for two hops.
//! That cannot happen when the hop types are pairwise disjoint, so the count
//! needs no correction there. For a two-hop chain with overlapping types the
//! planner sets `overlapping_types`, and the paths the DP over-counts are
//! exactly those whose two hops traverse the same relationship: each is fixed
//! by its first relationship, so they are counted by one pass over the first
//! hop and subtracted ([`CypherExecutor::same_relationship_paths`]).
//!
//! `Clause::FusedChainDistinctCount` counts the distinct nodes or relationships
//! one chain variable takes over those paths. A forward sweep gives `F_i`, the
//! nodes at position `i` reached from the start nodes; a backward sweep keeps
//! `B_i`, the members of `F_i` with an admitted relationship into `B_{i+1}`
//! (`B_k = F_k`). `B_j` is exactly the node at position `j` of some complete
//! path, so a node target counts `|B_j|`. A relationship at hop `i` lies on a
//! complete path iff it starts in `F_{i-1}` and ends in `B_i`; an undirected
//! hop can meet one relationship from both ends, so its ids are deduplicated.
//! Disjoint hop types make the matcher's relationship-uniqueness rule moot, as
//! above.

use petgraph::graph::NodeIndex;
use petgraph::Direction;
use rustc_hash::{FxHashMap, FxHashSet};

use super::*;
use crate::graph::core::graph_filter::ElementFilter;
use crate::graph::core::iterators::GraphEdgeRef;
use crate::graph::core::pattern_matching::pattern::ConnTypeFilter;
use crate::graph::core::pattern_matching::{EdgePattern, NodePattern, Pattern, PatternExecutor};

/// One hop of the chain: the relationship pattern, the node it reaches, and
/// the directions to sweep from the node it leaves.
struct ChainHop<'p> {
    edge: &'p EdgePattern,
    peer: &'p NodePattern,
    dirs: &'static [Direction],
}

fn chain_hops(pattern: &Pattern) -> Option<(&NodePattern, Vec<ChainHop<'_>>)> {
    let PatternElement::Node(start) = pattern.elements.first()? else {
        return None;
    };
    let hops = pattern.elements[1..]
        .chunks(2)
        .map(|pair| match pair {
            [PatternElement::Edge(edge), PatternElement::Node(peer)] => Some(ChainHop {
                edge,
                peer,
                dirs: match edge.direction {
                    EdgeDirection::Outgoing => &[Direction::Outgoing],
                    EdgeDirection::Incoming => &[Direction::Incoming],
                    EdgeDirection::Both => &[Direction::Outgoing, Direction::Incoming],
                },
            }),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some((start, hops))
}

/// One hop's per-relationship and per-peer tests, in the matcher's order.
struct HopTests<'a> {
    exec: &'a CypherExecutor<'a>,
    pe: &'a PatternExecutor<'a>,
    filter: Option<&'a ElementFilter>,
    hop: &'a ChainHop<'a>,
    conn_filter: ConnTypeFilter,
    peer_labels: crate::graph::dir_graph::LabelCheck<'a>,
    /// Peers the hop's node tests refused, so a refused peer reached over many
    /// relationships is tested once.
    refused: std::cell::RefCell<FxHashSet<NodeIndex>>,
}

impl<'a> HopTests<'a> {
    fn new(
        exec: &'a CypherExecutor<'a>,
        pe: &'a PatternExecutor<'a>,
        filter: Option<&'a ElementFilter>,
        hop: &'a ChainHop<'a>,
    ) -> Self {
        Self {
            exec,
            pe,
            filter,
            hop,
            conn_filter: hop.edge.conn_filter(),
            peer_labels: exec.pattern_label_check(hop.peer),
            refused: Default::default(),
        }
    }

    /// The node `edge_ref` reaches from `from` along `dir`, when the hop
    /// admits that relationship (its type, the filter, its inline filters) —
    /// the peer's own labels and properties are [`Self::peer_accepted`].
    fn admit_relationship(
        &self,
        from: NodeIndex,
        dir: Direction,
        edge_ref: &GraphEdgeRef<'_>,
    ) -> Option<NodeIndex> {
        let conn = edge_ref.connection_type();
        if !self.conn_filter.accepts(conn) {
            return None;
        }
        let peer = if dir == Direction::Outgoing {
            edge_ref.target()
        } else {
            edge_ref.source()
        };
        // An undirected hop reaches a self-loop from its Outgoing sweep; the
        // Incoming sweep would count it twice.
        if self.hop.edge.direction == EdgeDirection::Both
            && dir == Direction::Incoming
            && peer == from
        {
            return None;
        }
        if self.filter.is_some_and(|f| {
            !f.admits_hop(
                self.exec.graph,
                edge_ref.id(),
                conn,
                edge_ref.source(),
                peer,
            )
        }) || !self
            .pe
            .edge_passes_inline_filters(self.hop.edge, edge_ref, conn, dir)
        {
            return None;
        }
        Some(peer)
    }

    /// Whether `peer` satisfies the hop's node pattern (labels, properties).
    fn peer_accepted(&self, peer: NodeIndex) -> bool {
        let pattern = self.hop.peer;
        if pattern.label_alternatives().is_empty()
            && pattern.extra_labels.is_empty()
            && pattern.properties.is_none()
        {
            return true;
        }
        if self.refused.borrow().contains(&peer) {
            return false;
        }
        let accepted = self.peer_labels.matches(self.exec.graph, peer)
            && pattern
                .properties
                .as_ref()
                .is_none_or(|props| self.pe.node_matches_properties_pub(peer, props));
        if !accepted {
            self.refused.borrow_mut().insert(peer);
        }
        accepted
    }

    /// [`Self::admit_relationship`] and [`Self::peer_accepted`] together.
    fn admit(
        &self,
        from: NodeIndex,
        dir: Direction,
        edge_ref: &GraphEdgeRef<'_>,
    ) -> Option<NodeIndex> {
        self.admit_relationship(from, dir, edge_ref)
            .filter(|&peer| self.peer_accepted(peer))
    }

    fn edges(
        &self,
        from: NodeIndex,
        dir: Direction,
    ) -> impl Iterator<Item = GraphEdgeRef<'a>> + 'a {
        self.exec
            .graph
            .graph
            .edges_directed_filtered(from, dir, self.conn_filter.hint())
    }
}

impl CypherExecutor<'_> {
    pub(super) fn execute_fused_chain_path_count(
        &self,
        clause: &Clause,
    ) -> Result<ResultSet, String> {
        let Clause::FusedChainPathCount {
            pattern,
            overlapping_types,
            alias,
        } = clause
        else {
            return Err("internal: not a FusedChainPathCount clause".into());
        };
        let Some((start, hops)) = chain_hops(pattern) else {
            return Err("internal: FusedChainPathCount holds a pattern that is not a chain".into());
        };
        let pe = self.pattern_executor(None, None);
        let filter = self.graph_filter().map(|f| f.as_ref());
        let seeds = pe.find_matching_nodes_pub(start)?;
        let tests: Vec<HopTests<'_>> = hops
            .iter()
            .map(|hop| HopTests::new(self, &pe, filter, hop))
            .collect();
        let mut visited: usize = 0;
        let mut frontier: FxHashMap<NodeIndex, i128> = seeds.iter().map(|&n| (n, 1)).collect();
        for hop in &tests {
            if frontier.is_empty() {
                break;
            }
            frontier = self.advance_chain_frontier(hop, &frontier, &mut visited)?;
            self.budget.check_work(visited, "fused chain path count")?;
        }
        let mut total: i128 = 0;
        for &n in frontier.values() {
            total = total.checked_add(n).ok_or(RANGE_ERROR)?;
        }
        if *overlapping_types {
            let [first, second] = tests.as_slice() else {
                return Err("internal: overlapping hop types need exactly two hops".into());
            };
            total -= self.same_relationship_paths(first, second, &seeds, &mut visited)?;
        }
        let count = i64::try_from(total).map_err(|_| RANGE_ERROR)?;
        Ok(single_count_result(alias, count))
    }

    /// Move every frontier node's path count across `hop`'s admitted
    /// relationships to the peers they reach.
    fn advance_chain_frontier(
        &self,
        hop: &HopTests<'_>,
        frontier: &FxHashMap<NodeIndex, i128>,
        visited: &mut usize,
    ) -> Result<FxHashMap<NodeIndex, i128>, String> {
        let mut next: FxHashMap<NodeIndex, i128> =
            FxHashMap::with_capacity_and_hasher(frontier.len(), Default::default());
        for (&from, &paths) in frontier {
            for &dir in hop.hop.dirs {
                for edge_ref in hop.edges(from, dir) {
                    self.check_interrupt_periodic(*visited)?;
                    *visited += 1;
                    let Some(peer) = hop.admit_relationship(from, dir, &edge_ref) else {
                        continue;
                    };
                    // A peer already in `next` has passed the node tests.
                    if let Some(slot) = next.get_mut(&peer) {
                        *slot = slot.checked_add(paths).ok_or(RANGE_ERROR)?;
                    } else if hop.peer_accepted(peer) {
                        next.insert(peer, paths);
                    }
                }
            }
        }
        Ok(next)
    }

    /// The two-hop paths whose hops traverse one relationship, which the
    /// matcher's trail rule drops. Such a path is `(u, e, n1, e, n2)`: `e`
    /// leaves a seed `u` along the first hop, is incident at `n1` along the
    /// second, and both hops admit it with its peers. Neither hop is
    /// undirected (the planner mints the correction for directed hops only).
    fn same_relationship_paths(
        &self,
        first: &HopTests<'_>,
        second: &HopTests<'_>,
        seeds: &[NodeIndex],
        visited: &mut usize,
    ) -> Result<i128, String> {
        let (dir1, dir2) = (first.hop.dirs[0], second.hop.dirs[0]);
        let mut shared: i128 = 0;
        for &seed in seeds {
            for edge_ref in first.edges(seed, dir1) {
                self.check_interrupt_periodic(*visited)?;
                *visited += 1;
                let Some(middle) = first.admit(seed, dir1, &edge_ref) else {
                    continue;
                };
                let incident = if dir2 == Direction::Outgoing {
                    edge_ref.source()
                } else {
                    edge_ref.target()
                };
                if incident == middle && second.admit(middle, dir2, &edge_ref).is_some() {
                    shared += 1;
                }
            }
        }
        Ok(shared)
    }
}

impl CypherExecutor<'_> {
    pub(super) fn execute_chain_distinct(&self, clause: &Clause) -> Result<ResultSet, String> {
        let Clause::FusedChainDistinctCount {
            pattern,
            target,
            alias,
        } = clause
        else {
            return Err("internal: not a FusedChainDistinctCount clause".into());
        };
        let Some((start, hops)) = chain_hops(pattern) else {
            return Err(
                "internal: FusedChainDistinctCount holds a pattern that is not a chain".into(),
            );
        };
        if *target >= pattern.elements.len() {
            return Err("internal: FusedChainDistinctCount target is outside the pattern".into());
        }
        let pe = self.pattern_executor(None, None);
        let filter = self.graph_filter().map(|f| f.as_ref());
        let seeds = pe.find_matching_nodes_pub(start)?;
        let tests: Vec<HopTests<'_>> = hops
            .iter()
            .map(|hop| HopTests::new(self, &pe, filter, hop))
            .collect();
        let mut visited: usize = 0;
        let mut reach: Vec<FxHashSet<NodeIndex>> = vec![seeds.into_iter().collect()];
        for hop in &tests {
            let next = self.advance_chain_reach(hop, &reach[reach.len() - 1], &mut visited)?;
            self.budget
                .check_work(visited, "fused chain distinct count")?;
            reach.push(next);
        }
        // The position the answer is read at: the target node, or the node a
        // targeted relationship reaches.
        let lowest = target.div_ceil(2);
        let mut back = std::mem::take(&mut reach[tests.len()]);
        for i in (lowest..tests.len()).rev() {
            back = self.retain_reaching(&tests[i], &reach[i], &back, &mut visited)?;
            self.budget
                .check_work(visited, "fused chain distinct count")?;
        }
        let count = if target.is_multiple_of(2) {
            back.len()
        } else {
            let hop = &tests[lowest - 1];
            self.count_reaching_relationships(hop, &reach[lowest - 1], &back, &mut visited)?
        };
        let count = i64::try_from(count).map_err(|_| RANGE_ERROR)?;
        Ok(single_count_result(alias, count))
    }

    /// The peers `hop` admits from any node of `frontier`.
    fn advance_chain_reach(
        &self,
        hop: &HopTests<'_>,
        frontier: &FxHashSet<NodeIndex>,
        visited: &mut usize,
    ) -> Result<FxHashSet<NodeIndex>, String> {
        let mut next: FxHashSet<NodeIndex> = FxHashSet::default();
        for &from in frontier {
            for &dir in hop.hop.dirs {
                for edge_ref in hop.edges(from, dir) {
                    self.check_interrupt_periodic(*visited)?;
                    *visited += 1;
                    let Some(peer) = hop.admit_relationship(from, dir, &edge_ref) else {
                        continue;
                    };
                    if !next.contains(&peer) && hop.peer_accepted(peer) {
                        next.insert(peer);
                    }
                }
            }
        }
        Ok(next)
    }

    /// The members of `from` that `hop` admits a relationship from into
    /// `into`. `into` holds only nodes that already passed the hop's peer
    /// tests.
    fn retain_reaching(
        &self,
        hop: &HopTests<'_>,
        from: &FxHashSet<NodeIndex>,
        into: &FxHashSet<NodeIndex>,
        visited: &mut usize,
    ) -> Result<FxHashSet<NodeIndex>, String> {
        let mut kept: FxHashSet<NodeIndex> = FxHashSet::default();
        'nodes: for &node in from {
            for &dir in hop.hop.dirs {
                for edge_ref in hop.edges(node, dir) {
                    self.check_interrupt_periodic(*visited)?;
                    *visited += 1;
                    let reaches = hop
                        .admit_relationship(node, dir, &edge_ref)
                        .is_some_and(|peer| into.contains(&peer));
                    if reaches {
                        kept.insert(node);
                        continue 'nodes;
                    }
                }
            }
        }
        Ok(kept)
    }

    /// The distinct relationships `hop` admits from a node of `from` to a
    /// node of `into`.
    fn count_reaching_relationships(
        &self,
        hop: &HopTests<'_>,
        from: &FxHashSet<NodeIndex>,
        into: &FxHashSet<NodeIndex>,
        visited: &mut usize,
    ) -> Result<usize, String> {
        // A directed hop meets each relationship from one end only; an
        // undirected one can meet it from both.
        let undirected = hop.hop.edge.direction == EdgeDirection::Both;
        let mut ids: FxHashSet<petgraph::graph::EdgeIndex> = FxHashSet::default();
        let mut directed: usize = 0;
        for &node in from {
            for &dir in hop.hop.dirs {
                for edge_ref in hop.edges(node, dir) {
                    self.check_interrupt_periodic(*visited)?;
                    *visited += 1;
                    let reaches = hop
                        .admit_relationship(node, dir, &edge_ref)
                        .is_some_and(|peer| into.contains(&peer));
                    if !reaches {
                        continue;
                    }
                    if undirected {
                        ids.insert(edge_ref.id());
                    } else {
                        directed += 1;
                    }
                }
            }
        }
        Ok(if undirected { ids.len() } else { directed })
    }
}

const RANGE_ERROR: &str = "path count exceeds Cypher integer range";
