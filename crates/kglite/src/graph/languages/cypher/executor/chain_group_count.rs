//! The grouped forms of the chain counts:
//! `MATCH <chain> RETURN <key over chain node g>, count(DISTINCT x)`
//! (`Clause::FusedChainDistinctCount`) and `... count(*)`
//! (`Clause::FusedChainGroupedPathCount`, at the end of this file).
//!
//! The chain's forward and backward sweeps (see `chain_count.rs`) leave `V_i`,
//! the nodes at position `i` that lie on some complete path. A complete path
//! through a node of `V_g` continues to `x` through nodes of `V` only, so the
//! distinct `x` of the group node `n` are the targets reachable from `n` along
//! the hops between `g` and `x`. They are built one hop at a time from `x`
//! back to `g`: each node's target set is the union of its admitted peers'
//! sets, sorted and deduplicated, so a peer reached from many nodes is
//! expanded once. Group nodes then merge by key *value* (several nodes can
//! share a title) and each key counts the union of its nodes' sets.
//!
//! The walk runs from `g` towards `x`; when `x` lies before `g` the pattern
//! is reversed first (a relationship's identity does not depend on the end it
//! is met from), so the same sweeps serve both orders.

use petgraph::graph::NodeIndex;
use rustc_hash::{FxHashMap, FxHashSet};

use super::chain_count::{chain_hops, ChainHop, HopTests, RANGE_ERROR};
use super::*;
use crate::graph::core::pattern_matching::{EdgeDirection, Pattern};

/// What a target set holds at the level adjacent to the target: the peer
/// nodes (node target), the relationship ids (relationship target), or the
/// union of the next level's sets.
enum Pull<'m> {
    Peers,
    Edges,
    Inherit(&'m FxHashMap<NodeIndex, Vec<u32>>),
}

/// `pattern` read from its other end.
fn reversed(pattern: &Pattern) -> Pattern {
    let mut elements = pattern.elements.clone();
    elements.reverse();
    for element in &mut elements {
        if let PatternElement::Edge(edge) = element {
            edge.direction = match edge.direction {
                EdgeDirection::Outgoing => EdgeDirection::Incoming,
                EdgeDirection::Incoming => EdgeDirection::Outgoing,
                EdgeDirection::Both => EdgeDirection::Both,
            };
        }
    }
    Pattern { elements }
}

/// The variable a grouping key reads.
fn key_variable(key: &Expression) -> Option<&str> {
    match key {
        Expression::Variable(var) => Some(var),
        Expression::PropertyAccess { variable, .. } => Some(variable),
        _ => None,
    }
}

impl CypherExecutor<'_> {
    pub(super) fn execute_grouped_chain_distinct(
        &self,
        pattern: &Pattern,
        target: usize,
        group: &ChainGroupKey,
        alias: &str,
    ) -> Result<ResultSet, String> {
        let last = pattern.elements.len() - 1;
        if target > last || group.position > last || !group.position.is_multiple_of(2) {
            return Err("internal: grouped chain count positions are outside the pattern".into());
        }
        let Some(var) = key_variable(&group.key) else {
            return Err("internal: grouped chain count key reads no variable".into());
        };
        let flipped = target < group.position;
        let oriented = if flipped {
            reversed(pattern)
        } else {
            pattern.clone()
        };
        let (gpos, tpos) = if flipped {
            (last - group.position, last - target)
        } else {
            (group.position, target)
        };
        let Some((start, hops)) = chain_hops(&oriented) else {
            return Err("internal: grouped chain count holds a pattern that is not a chain".into());
        };
        let pe = self.pattern_executor(None, None);
        let filter = self.graph_filter().map(|f| f.as_ref());
        let seeds = pe.find_matching_nodes_pub(start)?;
        let tests: Vec<HopTests<'_>> = hops
            .iter()
            .map(|hop: &ChainHop<'_>| HopTests::new(self, &pe, filter, hop))
            .collect();
        let mut visited: usize = 0;
        let gi = gpos / 2;
        let valid = self.valid_chain_nodes(&tests, seeds, gi, &mut visited)?;
        let sets = self.target_sets(&tests, &valid, gi, tpos, &mut visited)?;
        self.group_target_counts(&sets, group, var, alias)
    }

    /// `V_i` for `i` from the group node's position to the chain's end, as a
    /// vector indexed by `i - gi`.
    fn valid_chain_nodes(
        &self,
        tests: &[HopTests<'_>],
        seeds: Vec<NodeIndex>,
        gi: usize,
        visited: &mut usize,
    ) -> Result<Vec<FxHashSet<NodeIndex>>, String> {
        const WHAT: &str = "fused grouped chain distinct count";
        let mut reach: Vec<FxHashSet<NodeIndex>> = vec![seeds.into_iter().collect()];
        for hop in tests {
            let next = self.advance_chain_reach(hop, &reach[reach.len() - 1], visited)?;
            self.budget.check_work(*visited, WHAT)?;
            reach.push(next);
        }
        let mut valid = vec![std::mem::take(&mut reach[tests.len()])];
        for i in (gi..tests.len()).rev() {
            let kept = self.retain_reaching(&tests[i], &reach[i], &valid[0], visited)?;
            self.budget.check_work(*visited, WHAT)?;
            valid.insert(0, kept);
        }
        Ok(valid)
    }

    /// For each valid node at the group position, the sorted distinct ids of
    /// the targets reachable from it. `tpos` is the target's position in the
    /// oriented pattern, at or after the group node.
    fn target_sets(
        &self,
        tests: &[HopTests<'_>],
        valid: &[FxHashSet<NodeIndex>],
        gi: usize,
        tpos: usize,
        visited: &mut usize,
    ) -> Result<FxHashMap<NodeIndex, Vec<u32>>, String> {
        let level = |node_index: usize| &valid[node_index - gi];
        // The deepest node level whose sets are built from edges; every level
        // before it inherits.
        let (mut sets, mut at) = if tpos.is_multiple_of(2) {
            let node_level = tpos / 2;
            if node_level == gi {
                let own = level(gi)
                    .iter()
                    .map(|&n| (n, vec![n.index() as u32]))
                    .collect();
                return Ok(own);
            }
            let hop = node_level - 1;
            let sets = self.pull_level(
                &tests[hop],
                level(hop),
                level(node_level),
                Pull::Peers,
                visited,
            )?;
            (sets, hop)
        } else {
            let hop = (tpos - 1) / 2;
            let sets = self.pull_level(
                &tests[hop],
                level(hop),
                level(hop + 1),
                Pull::Edges,
                visited,
            )?;
            (sets, hop)
        };
        while at > gi {
            at -= 1;
            sets = self.pull_level(
                &tests[at],
                level(at),
                level(at + 1),
                Pull::Inherit(&sets),
                visited,
            )?;
        }
        Ok(sets)
    }

    /// One level's target sets: for each node of `from`, what `pull` reads
    /// off the hop's admitted relationships into `into`.
    fn pull_level(
        &self,
        hop: &HopTests<'_>,
        from: &FxHashSet<NodeIndex>,
        into: &FxHashSet<NodeIndex>,
        pull: Pull<'_>,
        visited: &mut usize,
    ) -> Result<FxHashMap<NodeIndex, Vec<u32>>, String> {
        let mut out: FxHashMap<NodeIndex, Vec<u32>> =
            FxHashMap::with_capacity_and_hasher(from.len(), Default::default());
        let mut ids: Vec<u32> = Vec::new();
        for &node in from {
            ids.clear();
            for &dir in hop.hop.dirs {
                for edge_ref in hop.edges(node, dir) {
                    self.check_interrupt_periodic(*visited)?;
                    *visited += 1;
                    let Some(peer) = hop.admit_relationship(node, dir, &edge_ref) else {
                        continue;
                    };
                    if into.contains(&peer) {
                        ids.push(match pull {
                            Pull::Edges => edge_ref.id().index() as u32,
                            _ => peer.index() as u32,
                        });
                    }
                }
            }
            ids.sort_unstable();
            ids.dedup();
            let targets = if let Pull::Inherit(next) = pull {
                let mut union: Vec<u32> = Vec::new();
                for &peer in &ids {
                    if let Some(peer_targets) = next.get(&NodeIndex::new(peer as usize)) {
                        union.extend_from_slice(peer_targets);
                    }
                }
                *visited += union.len();
                union.sort_unstable();
                union.dedup();
                union
            } else {
                ids.clone()
            };
            if !targets.is_empty() {
                out.insert(node, targets);
            }
        }
        self.budget
            .check_work(*visited, "fused grouped chain distinct count")?;
        Ok(out)
    }

    /// One row per distinct key value: the size of the union of the target
    /// sets of the group nodes that share it.
    fn group_target_counts(
        &self,
        sets: &FxHashMap<NodeIndex, Vec<u32>>,
        group: &ChainGroupKey,
        var: &str,
        alias: &str,
    ) -> Result<ResultSet, String> {
        let mut nodes: Vec<NodeIndex> = sets.keys().copied().collect();
        nodes.sort_unstable();
        let mut slots: FxHashMap<Value, usize> = FxHashMap::default();
        let mut groups: Vec<(Value, Vec<NodeIndex>)> = Vec::new();
        let mut row = ResultRow::new();
        for (scanned, node) in nodes.into_iter().enumerate() {
            if scanned.is_multiple_of(2048) {
                self.check_deadline()?;
            }
            row.node_bindings.insert(var.to_string(), node);
            let key = self.evaluate_expression(&group.key, &row)?;
            match slots.get(&key) {
                Some(&slot) => groups[slot].1.push(node),
                None => {
                    slots.insert(key.clone(), groups.len());
                    groups.push((key, vec![node]));
                }
            }
        }
        let columns = if group.key_first {
            vec![group.key_alias.clone(), alias.to_string()]
        } else {
            vec![alias.to_string(), group.key_alias.clone()]
        };
        let mut rows = Vec::with_capacity(groups.len());
        for (key, members) in groups {
            let count = match members.as_slice() {
                [only] => sets[only].len(),
                _ => {
                    let mut union: Vec<u32> = members
                        .iter()
                        .flat_map(|member| sets[member].iter().copied())
                        .collect();
                    union.sort_unstable();
                    union.dedup();
                    union.len()
                }
            };
            let count = i64::try_from(count).map_err(|_| "count exceeds Cypher integer range")?;
            let mut projected = Bindings::with_capacity(2);
            projected.insert(group.key_alias.clone(), key);
            projected.insert(alias.to_string(), Value::Int64(count));
            let mut row = ResultRow::from_projected(projected);
            // An ORDER BY on the unaliased key (`c.kind`) reads it off the
            // node; every member of the group yields the same value.
            row.node_bindings.insert(var.to_string(), members[0]);
            rows.push(row);
        }
        Ok(ResultSet {
            rows,
            columns,
            lazy_return_items: None,
        })
    }
}

impl CypherExecutor<'_> {
    /// The paths of `RETURN <key over g>, count(*)` per key value. The paths
    /// through a node `n` at `g` are the partial paths that reach it from the
    /// start times those that reach it from the far end (hop types are
    /// disjoint, so every combination is a path the matcher keeps); the
    /// second factor is the same degree-product DP run over the reversed
    /// pattern. Nodes outside either frontier lie on no path and emit no row.
    pub(super) fn execute_chain_grouped_paths(&self, clause: &Clause) -> Result<ResultSet, String> {
        let Clause::FusedChainGroupedPathCount {
            pattern,
            alias,
            group,
        } = clause
        else {
            return Err("internal: not a FusedChainGroupedPathCount clause".into());
        };
        let last = pattern.elements.len() - 1;
        let Some(var) = key_variable(&group.key) else {
            return Err("internal: grouped chain count key reads no variable".into());
        };
        if group.position > last || !group.position.is_multiple_of(2) {
            return Err("internal: grouped chain count key is outside the pattern".into());
        }
        let gi = group.position / 2;
        let hops = last / 2;
        let before = self.chain_prefix_counts(pattern, gi)?;
        let after = self.chain_prefix_counts(&reversed(pattern), hops - gi)?;
        let mut nodes: Vec<NodeIndex> = before
            .keys()
            .filter(|node| after.contains_key(node))
            .copied()
            .collect();
        nodes.sort_unstable();
        let mut slots: FxHashMap<Value, usize> = FxHashMap::default();
        let mut groups: Vec<(Value, i128, NodeIndex)> = Vec::new();
        let mut row = ResultRow::new();
        for (scanned, node) in nodes.into_iter().enumerate() {
            if scanned.is_multiple_of(2048) {
                self.check_deadline()?;
            }
            let paths = before[&node].checked_mul(after[&node]).ok_or(RANGE_ERROR)?;
            row.node_bindings.insert(var.to_string(), node);
            let key = self.evaluate_expression(&group.key, &row)?;
            if let Some(&slot) = slots.get(&key) {
                groups[slot].1 = groups[slot].1.checked_add(paths).ok_or(RANGE_ERROR)?;
            } else {
                slots.insert(key.clone(), groups.len());
                groups.push((key, paths, node));
            }
        }
        let columns = if group.key_first {
            vec![group.key_alias.clone(), alias.clone()]
        } else {
            vec![alias.clone(), group.key_alias.clone()]
        };
        let mut rows = Vec::with_capacity(groups.len());
        for (key, paths, node) in groups {
            let count = i64::try_from(paths).map_err(|_| RANGE_ERROR)?;
            let mut projected = Bindings::with_capacity(2);
            projected.insert(group.key_alias.clone(), key);
            projected.insert(alias.clone(), Value::Int64(count));
            let mut row = ResultRow::from_projected(projected);
            row.node_bindings.insert(var.to_string(), node);
            rows.push(row);
        }
        Ok(ResultSet {
            rows,
            columns,
            lazy_return_items: None,
        })
    }

    /// The number of partial paths of `pattern` that end at each node
    /// reached after its first `hops` relationships.
    fn chain_prefix_counts(
        &self,
        pattern: &Pattern,
        hops: usize,
    ) -> Result<FxHashMap<NodeIndex, i128>, String> {
        let Some((start, chain)) = chain_hops(pattern) else {
            return Err("internal: grouped chain count holds a pattern that is not a chain".into());
        };
        let pe = self.pattern_executor(None, None);
        let filter = self.graph_filter().map(|f| f.as_ref());
        let seeds = pe.find_matching_nodes_pub(start)?;
        let tests: Vec<HopTests<'_>> = chain
            .iter()
            .take(hops)
            .map(|hop: &ChainHop<'_>| HopTests::new(self, &pe, filter, hop))
            .collect();
        let mut visited: usize = 0;
        let mut frontier: FxHashMap<NodeIndex, i128> = seeds.iter().map(|&n| (n, 1)).collect();
        for hop in &tests {
            if frontier.is_empty() {
                break;
            }
            frontier = self.advance_chain_frontier(hop, &frontier, &mut visited)?;
            self.budget
                .check_work(visited, "fused grouped chain path count")?;
        }
        Ok(frontier)
    }
}
