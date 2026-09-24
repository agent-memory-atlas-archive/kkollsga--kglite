//! `MATCH p = shortestPath(...)` executor.
//!
//! Its own module rather than part of `executor/mod.rs`, which is held to a
//! 1300-line shim cap by `test_mod_rs_purity`: this is a distinct execution
//! shape (BFS between two anchor points rather than the usual pattern-walk).

use super::*;
use crate::graph::core::pattern_matching::{NodePattern, PathHop, PatternExecutor};
use crate::graph::schema::{DirGraph, InternedKey};
use crate::graph::storage::GraphRead;
use petgraph::graph::NodeIndex;

/// Expand one shortest node sequence into exact relationship sequences.
/// Parallel edges create distinct paths; repeated edges are rejected.
fn exact_shortest_hops(
    graph: &DirGraph,
    nodes: &[NodeIndex],
    edge_direction: EdgeDirection,
    connection_types: Option<&[String]>,
    max_paths: usize,
) -> Vec<Vec<PathHop>> {
    let allowed: Option<Vec<InternedKey>> =
        connection_types.map(|types| types.iter().map(|t| InternedKey::from_str(t)).collect());
    let single_type = allowed.as_ref().and_then(|types| {
        if types.len() == 1 {
            Some(types[0])
        } else {
            None
        }
    });
    let mut paths: Vec<Vec<PathHop>> = vec![Vec::with_capacity(nodes.len().saturating_sub(1))];

    for pair in nodes.windows(2) {
        let from = pair[0];
        let to = pair[1];
        let directions: &[petgraph::Direction] = match edge_direction {
            EdgeDirection::Outgoing => &[petgraph::Direction::Outgoing],
            EdgeDirection::Incoming => &[petgraph::Direction::Incoming],
            EdgeDirection::Both => &[petgraph::Direction::Outgoing, petgraph::Direction::Incoming],
        };
        let mut candidates = Vec::new();
        for &direction in directions {
            for edge in graph
                .graph
                .edges_directed_filtered(from, direction, single_type)
            {
                let peer = match direction {
                    petgraph::Direction::Outgoing => edge.target(),
                    petgraph::Direction::Incoming => edge.source(),
                };
                if peer != to
                    || allowed
                        .as_ref()
                        .is_some_and(|types| !types.contains(&edge.connection_type()))
                    || candidates.iter().any(|hop: &PathHop| hop.edge == edge.id())
                {
                    continue;
                }
                candidates.push(PathHop {
                    node: to,
                    edge: edge.id(),
                    connection_type: edge.connection_type(),
                });
            }
        }

        let mut expanded = Vec::new();
        for path in &paths {
            for &hop in &candidates {
                if path.iter().any(|used| used.edge == hop.edge) {
                    continue;
                }
                let mut next = path.clone();
                next.push(hop);
                expanded.push(next);
                if expanded.len() >= max_paths {
                    break;
                }
            }
            if expanded.len() >= max_paths {
                break;
            }
        }
        paths = expanded;
        if paths.is_empty() {
            break;
        }
    }

    paths
}

/// One endpoint pair a `shortestPath` clause searches, with the input row it
/// came from (`None` only when the clause opens the query). A bound endpoint
/// is only ever paired with the node it is bound to, so writing both
/// variables into the output row never re-binds one.
struct EndpointPair<'r> {
    source: NodeIndex,
    target: NodeIndex,
    prior_row: Option<&'r ResultRow>,
}

/// The two endpoint node patterns of a `shortestPath` pattern.
fn shortest_path_endpoints(pattern: &Pattern) -> Result<(&NodePattern, &NodePattern), String> {
    let elements = &pattern.elements;
    if elements.len() < 3 {
        return Err("shortestPath requires a pattern like (a)-[:REL*..N]->(b)".to_string());
    }
    let PatternElement::Node(source) = &elements[0] else {
        return Err("shortestPath pattern must start with a node".to_string());
    };
    let Some(PatternElement::Node(target)) = elements.last() else {
        return Err("shortestPath pattern must end with a node".to_string());
    };
    Ok((source, target))
}

/// How one endpoint variable stands on one input row.
enum EndpointAnchor {
    /// An earlier clause bound it — as a pattern binding or as a node value.
    Bound(NodeIndex),
    /// Bound to NULL (an unmatched OPTIONAL MATCH) or to a value that is not a
    /// node: no node can match it, so the row yields nothing.
    NoMatch,
    /// Not bound before this clause: resolved from the pattern.
    Free,
}

impl<'a> CypherExecutor<'a> {
    /// Where `variable` stands on `row`. A stale node value is an error, never
    /// a reason to widen the endpoint to a scan.
    fn shortest_path_anchor(
        &self,
        row: &ResultRow,
        variable: Option<&str>,
    ) -> Result<EndpointAnchor, String> {
        let Some(var) = variable else {
            return Ok(EndpointAnchor::Free);
        };
        if let Some(&idx) = row.node_bindings.get(var) {
            return Ok(EndpointAnchor::Bound(idx));
        }
        if let Some(idx) = super::projected_targets::projected_node_target(self.graph, row, var)? {
            return Ok(EndpointAnchor::Bound(idx));
        }
        if row.projected.contains_key(var)
            || row.edge_bindings.contains_key(var)
            || row.path_bindings.contains_key(var)
        {
            return Ok(EndpointAnchor::NoMatch);
        }
        Ok(EndpointAnchor::Free)
    }

    /// The `(source, target)` work-list one `shortestPath` clause searches,
    /// built per input row.
    ///
    /// Each endpoint anchors independently: a variable an earlier clause bound
    /// (a pattern binding, or a node value from `WITH` / `UNWIND` /
    /// `startNode(r)` / a parameter) contributes exactly that node — checked
    /// against the endpoint's labels and properties — and only a free endpoint
    /// is resolved from its pattern. Resolving a bound endpoint from its bare
    /// pattern instead returns every node in the graph and turns one search
    /// into an all-pairs cross product that also re-binds the variable.
    fn shortest_path_endpoint_pairs<'r>(
        &self,
        pattern: &Pattern,
        existing: &'r ResultSet,
    ) -> Result<Vec<EndpointPair<'r>>, String> {
        let row_dependent = Self::pattern_has_vars(pattern);
        let seed_row = ResultRow::new();
        let rows: Vec<Option<&'r ResultRow>> = if existing.rows.is_empty() {
            // Only the opening clause sees no rows: the pipeline stops a later
            // MATCH at an empty input.
            vec![None]
        } else {
            existing.rows.iter().map(Some).collect()
        };
        // Free endpoints whose pattern reads nothing from the row resolve once.
        let mut free_cache: [Option<Vec<NodeIndex>>; 2] = [None, None];
        let mut out: Vec<EndpointPair<'r>> = Vec::new();

        for prior_row in rows {
            let row = prior_row.unwrap_or(&seed_row);
            let resolved;
            let pattern = if row_dependent {
                resolved = self.resolve_pattern_vars(pattern, row)?;
                &resolved
            } else {
                pattern
            };
            let (source_pattern, target_pattern) = shortest_path_endpoints(pattern)?;
            let src_var = source_pattern.variable.as_deref();
            let tgt_var = target_pattern.variable.as_deref();
            let same_var = src_var.is_some() && src_var == tgt_var;

            let src_anchor = self.shortest_path_anchor(row, src_var)?;
            let tgt_anchor = self.shortest_path_anchor(row, tgt_var)?;
            if matches!(src_anchor, EndpointAnchor::NoMatch)
                || matches!(tgt_anchor, EndpointAnchor::NoMatch)
            {
                continue;
            }
            let mut pre_bindings: Bindings<NodeIndex> = Bindings::new();
            for (var, anchor) in [(src_var, &src_anchor), (tgt_var, &tgt_anchor)] {
                if let (Some(v), EndpointAnchor::Bound(idx)) = (var, anchor) {
                    pre_bindings.insert(v.to_string(), *idx);
                }
            }

            let executor = PatternExecutor::with_bindings_and_params(
                self.graph,
                None,
                &pre_bindings,
                self.params,
            )
            .set_deadline(self.deadline)
            .set_cancel(self.cancel)
            .set_parallel(self.parallel);
            let mut candidates = |slot: usize,
                                  node: &NodePattern,
                                  anchor: &EndpointAnchor|
             -> Result<Vec<NodeIndex>, String> {
                // A bound endpoint goes through the matcher too: its
                // pre-binding short-circuits the scan to a label and property
                // check of that one node.
                if row_dependent || !matches!(anchor, EndpointAnchor::Free) {
                    return executor.find_matching_nodes_pub(node);
                }
                if let Some(cached) = &free_cache[slot] {
                    return Ok(cached.clone());
                }
                let found = executor.find_matching_nodes_pub(node)?;
                free_cache[slot] = Some(found.clone());
                Ok(found)
            };
            let source_nodes = candidates(0, source_pattern, &src_anchor)?;
            if source_nodes.is_empty() {
                continue;
            }
            let target_nodes = candidates(1, target_pattern, &tgt_anchor)?;

            // `shortestPath((a)-[*]-(a))`: one variable is one node, so the
            // pairs are the nodes both endpoint patterns accept.
            if same_var {
                let accepted: std::collections::HashSet<NodeIndex> =
                    target_nodes.into_iter().collect();
                for &node in source_nodes.iter().filter(|n| accepted.contains(n)) {
                    self.budget.check_rows(out.len() + 1, "shortestPath")?;
                    out.push(EndpointPair {
                        source: node,
                        target: node,
                        prior_row,
                    });
                }
                continue;
            }

            // Each endpoint set is node-bounded, but their product is not: an
            // unanchored `shortestPath((a:X)-[*]-(b:Y))` over two 10k-node
            // labels asks for 100M pairs, and the pair set IS the materialized
            // row set, so the ordinary row check governs it.
            let total = source_nodes
                .len()
                .checked_mul(target_nodes.len())
                .and_then(|row_pairs| out.len().checked_add(row_pairs))
                .ok_or_else(|| {
                    "Query row count overflow while executing shortestPath".to_string()
                })?;
            self.budget.check_rows(total, "shortestPath")?;
            out.reserve(total - out.len());
            for &s in &source_nodes {
                for &t in &target_nodes {
                    out.push(EndpointPair {
                        source: s,
                        target: t,
                        prior_row,
                    });
                }
            }
        }
        Ok(out)
    }

    /// One output row for a found path: the input row it extends (so a
    /// downstream RETURN still sees what earlier clauses exposed), both
    /// endpoint bindings, and the path.
    fn shortest_path_row(
        &self,
        pair: &EndpointPair<'_>,
        endpoint_vars: (Option<&str>, Option<&str>),
        path_variable: &str,
        hops: usize,
        path: Vec<PathHop>,
    ) -> ResultRow {
        let mut row = match pair.prior_row {
            Some(pr) => pr.clone(),
            None => ResultRow::new(),
        };
        if let Some(var) = endpoint_vars.0 {
            row.node_bindings.insert(var.to_string(), pair.source);
        }
        if let Some(var) = endpoint_vars.1 {
            row.node_bindings.insert(var.to_string(), pair.target);
        }
        row.path_bindings.insert(
            path_variable.to_string(),
            PathBinding {
                hop_incarnations: self.capture_path_incarnations(&path),
                source: pair.source,
                hops,
                path,
            },
        );
        row
    }

    pub(super) fn execute_shortest_path_match(
        &self,
        clause: &MatchClause,
        path_assignment: &PathAssignment,
        existing: ResultSet,
        inline_where: Option<&Predicate>,
    ) -> Result<ResultSet, String> {
        let pattern = clause
            .patterns
            .get(path_assignment.pattern_index)
            .ok_or("Invalid pattern index for shortestPath")?;

        let (source_pattern, target_pattern) = shortest_path_endpoints(pattern)?;
        let endpoint_vars = (
            source_pattern.variable.as_deref(),
            target_pattern.variable.as_deref(),
        );
        let elements = &pattern.elements;

        // Direction, relationship types and hop bounds of the one relationship
        // (the parser refuses any other shape, and a minimum above 1).
        // `min_hops == 0` is what makes a path from a node to itself an answer
        // rather than a skip. The maximum binds only when it was written: a
        // bare `*` / `*1..` is unbounded here, not the var-length MATCH cap —
        // the BFS visits each node at most once, and the deadline, the cancel
        // flag and the pair-row budget still apply. A relationship without
        // `*` is one hop.
        let edge = elements.iter().find_map(|elem| match elem {
            PatternElement::Edge(ep) => Some(ep),
            PatternElement::Node(_) => None,
        });
        let edge_direction = edge.map_or(EdgeDirection::Both, |ep| ep.direction);
        let connection_types_vec = edge.and_then(|ep| {
            ep.connection_types
                .clone()
                .or_else(|| ep.connection_type.clone().map(|name| vec![name]))
        });
        let includes_zero_length = edge.and_then(|ep| ep.var_length).map(|(min, _)| min) == Some(0);
        let max_hops: Option<usize> = edge.and_then(|ep| match ep.var_length {
            None => Some(1),
            Some((_, max)) if ep.var_length_max_written => Some(max),
            Some(_) => None,
        });

        let connection_types: Option<&[String]> = connection_types_vec.as_deref();

        let pairs = self.shortest_path_endpoint_pairs(pattern, &existing)?;

        let mut all_rows = Vec::new();

        for pair in &pairs {
            let (source_idx, target_idx) = (pair.source, pair.target);
            {
                if source_idx == target_idx {
                    // A `*0..` segment includes the zero-length path: both
                    // endpoints are the same node and the path holds no
                    // relationship, so the segment's type and direction say
                    // nothing about it. Every other bound has no trail from a
                    // node back to itself that the BFS below could shorten.
                    if includes_zero_length {
                        all_rows.push(self.shortest_path_row(
                            pair,
                            endpoint_vars,
                            &path_assignment.variable,
                            0,
                            Vec::new(),
                        ));
                    }
                    continue;
                }

                // Dispatch based on edge direction + the all-shortest flag.
                // `shortestPath` yields ≤1 path; `allShortestPaths` yields
                // every minimal path (one output row each), capped to bound
                // pathological fan-out.
                use crate::graph::algorithms::graph_algorithms as ga;
                const MAX_ALL_SHORTEST: usize = 256;
                let path_results: Vec<ga::PathResult> = if path_assignment.all_shortest {
                    match edge_direction {
                        EdgeDirection::Both => ga::all_shortest_paths(
                            self.graph,
                            source_idx,
                            target_idx,
                            connection_types,
                            self.interrupt(),
                            MAX_ALL_SHORTEST,
                        ),
                        EdgeDirection::Outgoing => ga::all_shortest_paths_directed(
                            self.graph,
                            source_idx,
                            target_idx,
                            connection_types,
                            self.interrupt(),
                            MAX_ALL_SHORTEST,
                        ),
                        EdgeDirection::Incoming => ga::all_shortest_paths_directed(
                            self.graph,
                            target_idx,
                            source_idx,
                            connection_types,
                            self.interrupt(),
                            MAX_ALL_SHORTEST,
                        )
                        .into_iter()
                        .map(|mut pr| {
                            pr.path.reverse();
                            pr
                        })
                        .collect(),
                    }
                } else {
                    // Direction comes from the dispatch below, not from here.
                    let path_opts = ga::PathOptions {
                        connection_types,
                        ..ga::PathOptions::default().with_interrupt(self.interrupt())
                    };
                    let single = match edge_direction {
                        EdgeDirection::Both => {
                            ga::shortest_path(self.graph, source_idx, target_idx, &path_opts)
                        }
                        EdgeDirection::Outgoing => ga::shortest_path_directed(
                            self.graph, source_idx, target_idx, &path_opts,
                        ),
                        EdgeDirection::Incoming => ga::shortest_path_directed(
                            self.graph, target_idx, source_idx, &path_opts,
                        )
                        .map(|mut pr| {
                            pr.path.reverse();
                            pr
                        }),
                    };
                    single.into_iter().collect()
                };

                let mut exact_paths = Vec::new();
                let mut seen_node_paths = std::collections::HashSet::new();
                for path_result in path_results {
                    // The graph algorithm is node-oriented and may surface the
                    // same node sequence once per parallel edge. Expand that
                    // sequence exactly once into relationship combinations.
                    if max_hops.is_some_and(|max| path_result.cost > max)
                        || !seen_node_paths.insert(path_result.path.clone())
                    {
                        continue;
                    }
                    let remaining = if path_assignment.all_shortest {
                        MAX_ALL_SHORTEST.saturating_sub(exact_paths.len())
                    } else {
                        1
                    };
                    for hops in exact_shortest_hops(
                        self.graph,
                        &path_result.path,
                        edge_direction,
                        connection_types,
                        remaining,
                    ) {
                        exact_paths.push((path_result.cost, hops));
                    }
                    if exact_paths.len() >= MAX_ALL_SHORTEST {
                        break;
                    }
                }

                for (path_cost, path_nodes) in exact_paths {
                    all_rows.push(self.shortest_path_row(
                        pair,
                        endpoint_vars,
                        &path_assignment.variable,
                        path_cost,
                        path_nodes,
                    ));
                }
            }
        }

        // The pipeline folds a WHERE that directly follows the opening MATCH
        // into this call and then skips it as a clause.
        if let Some(predicate) = inline_where {
            let mut kept = Vec::with_capacity(all_rows.len());
            for row in all_rows {
                if self.evaluate_predicate(predicate, &row)? {
                    kept.push(row);
                }
            }
            all_rows = kept;
        }

        Ok(ResultSet {
            rows: all_rows,
            columns: existing.columns,
            lazy_return_items: None,
        })
    }
}
