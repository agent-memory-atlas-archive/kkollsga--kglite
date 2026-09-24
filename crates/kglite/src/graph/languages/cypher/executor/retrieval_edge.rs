//! The relationship arm of `FusedVectorScoreTopK`:
//! `MATCH (a)-[r:T]->(b) RETURN … vector_score(r, 'p_emb', $q) AS s ORDER BY s
//! DESC LIMIT k`.
//!
//! Two routes, mirroring the node arm in `retrieval.rs`:
//!
//! - **Entry** — a plain single-type relationship scan (no WHERE, no property
//!   maps, no second pattern) whose population provably *is* the relationship
//!   store: every relationship of the type is embedded and satisfies the
//!   pattern's endpoint labels. Served by `edge_vector_index::query_store` (HNSW
//!   when an index is online and the metric matches, exact scan otherwise —
//!   the same routes `db.edge_embeddings.query` takes), with RETURN projected
//!   and endpoints bound for the k winners only. The pattern is never
//!   materialised, which is the whole saving.
//! - **Rows** — anything else reaches the clause as materialised rows. With an
//!   online index the rows' relationships are searched through HNSW with a 4×
//!   over-fetch and filtered to the rows (the node arm's post-filter), falling
//!   back to the exact generic top-k when the filter underfills; without one
//!   the generic top-k scores the rows exactly.
//!
//! **Several types.** A type alternation `[r:A|B]` or an untyped `[r]` puts
//! several relationship types in play (the untyped pattern: every type the
//! graph holds). Both routes then query each type's store and merge, but only
//! when every type in play carries the store; otherwise they step aside and
//! the scalar raises its missing-store error, as it does for a node label
//! without a store. Each store scores under its own metric, exactly as the
//! scalar does row by row, so the merge ranks what the unfused query ranks.
//!
//! **Tie order.** The unfused query ranks equal scores in the order the
//! pattern matcher produced the rows, which the entry never sees. So the entry
//! declines whenever the k+1 best scores are not strictly decreasing, and the
//! generic route answers instead: the fused answer is then identical to the
//! unfused one, not merely equivalent. The rows route breaks ties by row
//! position, exactly as the node arm does.
use super::retrieval::{HnswOutcome, VectorScoreArgs};
use super::*;
use crate::graph::algorithms::vector::DistanceMetric;
use crate::graph::core::pattern_matching::NodePattern;
use crate::graph::edge_embeddings::vector_index::{
    query_store, resolve_query_metric, EdgeVectorQueryHit,
};
use crate::graph::edge_embeddings::EdgeEmbeddingStore;
use petgraph::graph::{EdgeIndex, NodeIndex};
use rustc_hash::FxHashMap;

/// A plain single-hop relationship scan the entry can serve from the stores.
struct PlainEdgeScan<'q> {
    variable: &'q str,
    /// The written types, deduplicated; `None` for an untyped pattern.
    rel_types: Option<Vec<&'q str>>,
    left: &'q NodePattern,
    right: &'q NodePattern,
    outgoing: bool,
}

/// `(a)-[r:T]->(b)`, `(a)-[r:A|B]->(b)` or `(a)-[r]->(b)`, either direction,
/// with nothing that filters: no WHERE, no property maps, no multi-label or
/// parameterised labels, no paths, hints or anchors, distinct variables.
/// Undirected patterns match each relationship twice and are not a
/// store-shaped population.
fn plain_edge_scan(matched: &MatchClause) -> Option<PlainEdgeScan<'_>> {
    let [pattern] = matched.patterns.as_slice() else {
        return None;
    };
    let [PatternElement::Node(left), PatternElement::Edge(edge), PatternElement::Node(right)] =
        pattern.elements.as_slice()
    else {
        return None;
    };
    if matched.where_clause.is_some()
        || !matched.path_assignments.is_empty()
        || !matched.node_anchors.is_empty()
        || matched.limit_hint.is_some()
        || matched.distinct_node_hint.is_some()
    {
        return None;
    }
    let variable = edge.variable.as_deref()?;
    let rel_types = match (&edge.connection_types, &edge.connection_type) {
        (Some(types), _) => {
            let mut types: Vec<&str> = types.iter().map(String::as_str).collect();
            types.sort_unstable();
            types.dedup();
            Some(types)
        }
        (None, Some(single)) => Some(vec![single.as_str()]),
        (None, None) => None,
    };
    if edge.properties.is_some()
        || edge.var_length.is_some()
        || edge.edge_filter.is_some()
        || !edge.type_params.is_empty()
        || edge.direction == EdgeDirection::Both
    {
        return None;
    }
    let plain_node = |node: &NodePattern| {
        node.properties.is_none() && !node.multi_label_constrained() && node.label_params.is_empty()
    };
    if !plain_node(left) || !plain_node(right) {
        return None;
    }
    let names = [left.variable.as_deref(), right.variable.as_deref()];
    if names.contains(&Some(variable)) || (names[0].is_some() && names[0] == names[1]) {
        return None;
    }
    Some(PlainEdgeScan {
        variable,
        rel_types,
        left,
        right,
        outgoing: edge.direction == EdgeDirection::Outgoing,
    })
}

/// One store's winners, tagged with the store's route.
struct StoreHits {
    hits: Vec<EdgeVectorQueryHit>,
    search_method: &'static str,
}

/// The diagnostics name of the stores a merged route read, in type order.
fn store_label(rel_types: &[&str], property: &str) -> String {
    rel_types
        .iter()
        .map(|rel_type| format!("relationship:{rel_type}.{property}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Whether the k+1 best hits leave the first k ranked by score alone: no two
/// equal scores among them, none tied with the first excluded hit, no NaN.
fn ranked_by_score_alone(hits: &[EdgeVectorQueryHit], limit: usize) -> bool {
    let considered = &hits[..hits.len().min(limit + 1)];
    considered.iter().all(|hit| !hit.score.is_nan())
        && considered
            .windows(2)
            .all(|pair| pair[0].score > pair[1].score)
}

impl<'a> CypherExecutor<'a> {
    /// The entry route. `Ok(None)` hands the clause to the established path
    /// (materialise the MATCH, then the rows route) — for any shape, store or
    /// argument this route does not own, including every argument error, so
    /// errors keep the scalar's wording.
    pub(super) fn try_edge_vector_retrieval_entry(
        &self,
        matched: &MatchClause,
        return_clause: &ReturnClause,
        score_item_index: usize,
        score_call: &Expression,
        limit: usize,
    ) -> Result<Option<ResultSet>, String> {
        let Some(scan) = plain_edge_scan(matched) else {
            return Ok(None);
        };
        let score_expr = self.fold_constants_expr(score_call);
        let Some(args) = self.constant_vector_args(&score_expr, &ResultRow::new())? else {
            return Ok(None);
        };
        if args.variable != scan.variable {
            return Ok(None);
        }
        let counts = self.graph.get_edge_type_counts();
        let rel_types: Vec<&str> = match &scan.rel_types {
            Some(types) => types.clone(),
            None => {
                let mut present: Vec<&str> = counts
                    .iter()
                    .filter(|(_, &count)| count > 0)
                    .map(|(rel_type, _)| rel_type.as_str())
                    .collect();
                present.sort_unstable();
                present
            }
        };
        if rel_types.is_empty() {
            return Ok(None);
        }
        let mut stores = Vec::with_capacity(rel_types.len());
        for &rel_type in &rel_types {
            let Some(store) = self
                .graph
                .edge_embeddings
                .get(&(rel_type.to_string(), args.property.clone()))
            else {
                return Ok(None);
            };
            let Some(metric) = self.edge_query_metric(store, &args) else {
                return Ok(None);
            };
            let numeric = store.index_store();
            // A pending refresh belongs to the established route, as on the node arm.
            if !args.options.exact && numeric.has_index() && numeric.index_is_stale() {
                return Ok(None);
            }
            if counts.get(rel_type).copied().unwrap_or(0) != store.len()
                || !self.edge_scan_covers_store(&scan, rel_type, store)?
            {
                return Ok(None);
            }
            stores.push((store, metric));
        }
        let population: usize = stores.iter().map(|(store, _)| store.len()).sum();
        self.budget.check_work(population, "MATCH")?;
        self.check_deadline()?;
        let per_store = self.query_stores(
            &stores,
            &args.query,
            limit.saturating_add(1),
            args.options.exact,
        );
        let search_method = per_store[0].search_method;
        let mut merged: Vec<EdgeVectorQueryHit> =
            per_store.into_iter().flat_map(|store| store.hits).collect();
        merged.sort_by(|left, right| right.score.total_cmp(&left.score));
        merged.truncate(limit.saturating_add(1));
        if !ranked_by_score_alone(&merged, limit) {
            return Ok(None);
        }
        let rows = merged
            .iter()
            .take(limit)
            .map(|hit| self.edge_scan_row(&scan, hit.edge))
            .collect::<Option<Vec<_>>>();
        let Some(rows) = rows else {
            return Ok(None);
        };
        let scores: Vec<(usize, Value)> = merged
            .iter()
            .take(rows.len())
            .enumerate()
            .map(|(position, hit)| (position, Value::Float64(hit.score)))
            .collect();
        let population = ResultSet {
            rows,
            columns: Vec::new(),
            lazy_return_items: None,
        };
        let result = self.project_retrieval_winners(
            scores.into_iter(),
            &score_expr,
            &super::retrieval::RetrievalPopulation::Rows(&population),
            return_clause,
            score_item_index,
        )?;
        let has_index = stores
            .iter()
            .all(|(store, _)| store.index_store().has_index());
        self.record_retrieval(edge_diagnostics(
            &args,
            has_index,
            search_method,
            store_label(&rel_types, &args.property),
        ));
        Ok(Some(result))
    }

    /// Each store's best `top_k`, all on one route: HNSW only when every store
    /// answers through its index, otherwise every store by exact scan, so the
    /// merge never mixes approximate and exact candidates.
    fn query_stores(
        &self,
        stores: &[(&EdgeEmbeddingStore, DistanceMetric)],
        query: &[f32],
        top_k: usize,
        exact: bool,
    ) -> Vec<StoreHits> {
        let run = |exact: bool| -> Vec<StoreHits> {
            stores
                .iter()
                .map(|&(store, metric)| {
                    let report =
                        query_store(store, query, top_k, exact, metric, self.graph.read_only);
                    StoreHits {
                        hits: report.hits,
                        search_method: report.search_method,
                    }
                })
                .collect()
        };
        let first = run(exact);
        if first
            .iter()
            .all(|store| store.search_method == first[0].search_method)
        {
            first
        } else {
            run(true)
        }
    }

    /// The metric the scalar would score with, or `None` when the query or
    /// store cannot be scored (the established route raises the error).
    fn edge_query_metric(
        &self,
        store: &EdgeEmbeddingStore,
        args: &VectorScoreArgs,
    ) -> Option<DistanceMetric> {
        if crate::graph::embedding_validation::validate_finite_vector(&args.query).is_err()
            || args.query.len() != store.dimension()
        {
            return None;
        }
        match args.options.metric {
            Some(metric) => Some(metric),
            None => resolve_query_metric(store, None).ok(),
        }
    }

    /// Whether every relationship in `rel_type`'s store satisfies the scan's
    /// endpoint labels (the caller has checked that the store holds a vector
    /// for every relationship of the type — one without would score NULL and
    /// rank first). Labels are proved from the
    /// connection metadata when it names exactly the pattern's label, else by
    /// checking each stored relationship's endpoint.
    fn edge_scan_covers_store(
        &self,
        scan: &PlainEdgeScan<'_>,
        rel_type: &str,
        store: &EdgeEmbeddingStore,
    ) -> Result<bool, String> {
        let (source_pattern, target_pattern) = if scan.outgoing {
            (scan.left, scan.right)
        } else {
            (scan.right, scan.left)
        };
        let metadata = self.graph.connection_type_metadata.get(rel_type);
        let proven = |pattern: &NodePattern, sides: Option<&std::collections::HashSet<String>>| {
            pattern.node_type.as_ref().is_none_or(|label| {
                sides.is_some_and(|types| types.len() == 1 && types.contains(label))
            })
        };
        if proven(source_pattern, metadata.map(|info| &info.source_types))
            && proven(target_pattern, metadata.map(|info| &info.target_types))
        {
            return Ok(true);
        }
        for (position, &edge) in store.index_store().slot_to_node.iter().enumerate() {
            if position % INTERRUPT_POLL_INTERVAL == 0 {
                self.check_deadline()?;
            }
            let Some((source, target)) = self.graph.graph.edge_endpoints(EdgeIndex::new(edge))
            else {
                return Ok(false);
            };
            if !self.node_has_primary(source, source_pattern)
                || !self.node_has_primary(target, target_pattern)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn node_has_primary(&self, node: NodeIndex, pattern: &NodePattern) -> bool {
        let Some(label) = &pattern.node_type else {
            return true;
        };
        self.graph
            .graph
            .node_view(node)
            .is_some_and(|view| view.node_type_str(&self.graph.interner) == label)
    }

    /// The row the matcher would have produced for one winning relationship.
    fn edge_scan_row(&self, scan: &PlainEdgeScan<'_>, edge: EdgeIndex) -> Option<ResultRow> {
        let (source, target) = self.graph.graph.edge_endpoints(edge)?;
        let (left, right) = if scan.outgoing {
            (source, target)
        } else {
            (target, source)
        };
        let mut row = ResultRow::new();
        row.edge_bindings.insert(
            scan.variable.to_string(),
            EdgeBinding {
                incarnation: self.relationship_incarnation(edge),
                source: left,
                target: right,
                edge_index: edge,
            },
        );
        for (pattern, node) in [(scan.left, left), (scan.right, right)] {
            if let Some(variable) = &pattern.variable {
                row.node_bindings.insert(variable.clone(), node);
            }
        }
        Some(row)
    }

    /// The rows route's index half. `Ok(None)` when the score call does not
    /// read a relationship binding (the node arm owns it); otherwise the HNSW
    /// result, or the exact fallback's diagnostics for the caller to record
    /// before running the generic top-k.
    pub(super) fn try_edge_rows_fused_top_k(
        &self,
        score_expr: &Expression,
        descending: bool,
        limit: usize,
        rows: &ResultSet,
        return_clause: &ReturnClause,
        score_item_index: usize,
    ) -> Result<Option<HnswOutcome>, String> {
        let Expression::FunctionCall {
            args: call_args, ..
        } = score_expr
        else {
            return Ok(None);
        };
        let Some(Expression::Variable(variable)) = call_args.first() else {
            return Ok(None);
        };
        let first_row = &rows.rows[0];
        if !first_row.edge_bindings.contains_key(variable) {
            return Ok(None);
        }
        let mut info = RetrievalDiagnostics::exact("unsupported_shape");
        if (3..=5).contains(&call_args.len()) {
            info.requested_policy = self.requested_retrieval_policy(call_args)?;
        }
        if !descending || limit == 0 {
            return Ok(Some(HnswOutcome::Exact(info.fallback("unsupported_shape"))));
        }
        let Some(args) = self.constant_vector_args(score_expr, first_row)? else {
            return Ok(Some(HnswOutcome::Exact(
                info.fallback("row_dependent_selectors"),
            )));
        };
        info.requested_policy = if args.options.exact { "exact" } else { "auto" }.into();
        if args.options.exact {
            return Ok(Some(HnswOutcome::Exact(info.fallback("forced_exact"))));
        }
        let Some(coverage) = self.edge_row_coverage(variable, &args.property, rows) else {
            return Ok(Some(HnswOutcome::Exact(info.fallback("row_coverage"))));
        };
        let EdgeRowCoverage {
            edge_to_row,
            stores,
        } = coverage;
        let rel_types: Vec<&str> = stores.iter().map(|store| store.rel_type.as_str()).collect();
        info.store = Some(store_label(&rel_types, &args.property));
        let mut metrics = Vec::with_capacity(stores.len());
        for covered in &stores {
            let numeric = covered.store.index_store();
            if numeric.index_for_query(self.graph.read_only).is_none() {
                if numeric.has_index() && numeric.index_is_stale() {
                    self.warn(format!(
                        "relationship vector index '{}.{}' is behind its store by {} vectors, \
                         over its auto_refresh_limit of {} — this query was served by exact \
                         scan. Refresh with CALL db.edge_embeddings.refresh_index.",
                        covered.rel_type,
                        args.property,
                        numeric.delta_size(),
                        numeric.auto_refresh_limit(),
                    ));
                }
                let reason = if numeric.has_index() {
                    "stale_index"
                } else {
                    "no_index"
                };
                return Ok(Some(HnswOutcome::Exact(info.fallback(reason))));
            }
            let Some(metric) = self.edge_query_metric(covered.store, &args) else {
                return Ok(Some(HnswOutcome::Exact(info.fallback("unsupported_shape"))));
            };
            metrics.push(metric);
        }
        let mut scored: Vec<(usize, f64)> = Vec::new();
        for (covered, metric) in stores.iter().zip(metrics) {
            let store_len = covered.store.len();
            let k_fetch = limit.saturating_mul(4).max(limit).min(store_len);
            let report = query_store(
                covered.store,
                &args.query,
                k_fetch,
                false,
                metric,
                self.graph.read_only,
            );
            if report.search_method != "hnsw" {
                return Ok(Some(HnswOutcome::Exact(info.fallback("metric_mismatch"))));
            }
            let before = scored.len();
            scored.extend(report.hits.iter().filter_map(|hit| {
                edge_to_row
                    .get(&hit.edge.index())
                    .map(|&row| (row, hit.score))
            }));
            // A store the rows cover only in part must still yield `limit`
            // candidates, or one of its rows may rank above what the merge saw.
            let whole_store = covered.rows == store_len;
            if !whole_store && scored.len() - before < limit {
                return Ok(Some(HnswOutcome::Exact(
                    info.fallback("filtered_underfill"),
                )));
            }
        }
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(limit);
        let result = self.project_retrieval_winners(
            scored
                .into_iter()
                .map(|(position, score)| (position, Value::Float64(score))),
            score_expr,
            &super::retrieval::RetrievalPopulation::Rows(rows),
            return_clause,
            score_item_index,
        )?;
        info.actual_mode = "hnsw".into();
        info.fallback_reason = None;
        Ok(Some(HnswOutcome::Indexed(result, info)))
    }

    /// Relationship-to-row lookup plus the stores in play, valid only when
    /// every row binds a current relationship whose type carries the
    /// `property` store and holds a vector in it, and no relationship repeats —
    /// the HNSW route cannot rank a NULL score or a duplicate row. A row whose
    /// type has no store also answers `None`: the exact route then raises the
    /// scalar's missing-store error.
    fn edge_row_coverage<'s>(
        &'s self,
        variable: &str,
        property: &str,
        rows: &ResultSet,
    ) -> Option<EdgeRowCoverage<'s>> {
        let mut stores: Vec<CoveredStore<'s>> = Vec::new();
        let mut store_of_type: FxHashMap<InternedKey, usize> = FxHashMap::default();
        let mut edge_to_row =
            FxHashMap::with_capacity_and_hasher(rows.rows.len(), Default::default());
        for (position, row) in rows.rows.iter().enumerate() {
            let binding = row.edge_bindings.get(variable)?;
            if !self.relationship_binding_is_current(binding) {
                return None;
            }
            let weight = self.graph.graph.edge_weight(binding.edge_index)?;
            let at = match store_of_type.get(&weight.connection_type) {
                Some(&at) => at,
                None => {
                    let rel_type = weight.connection_type_str(&self.graph.interner).to_string();
                    let store = self
                        .graph
                        .edge_embeddings
                        .get(&(rel_type.clone(), property.to_string()))?;
                    stores.push(CoveredStore {
                        rel_type,
                        store,
                        rows: 0,
                    });
                    store_of_type.insert(weight.connection_type, stores.len() - 1);
                    stores.len() - 1
                }
            };
            let covered = &mut stores[at];
            if !covered
                .store
                .index_store()
                .node_to_slot
                .contains_key(&binding.edge_index.index())
                || edge_to_row
                    .insert(binding.edge_index.index(), position)
                    .is_some()
            {
                return None;
            }
            covered.rows += 1;
        }
        stores.sort_by(|left, right| left.rel_type.cmp(&right.rel_type));
        Some(EdgeRowCoverage {
            edge_to_row,
            stores,
        })
    }
}

/// One store the rows route reads, and how many rows bind into it.
struct CoveredStore<'s> {
    rel_type: String,
    store: &'s EdgeEmbeddingStore,
    rows: usize,
}

struct EdgeRowCoverage<'s> {
    edge_to_row: FxHashMap<usize, usize>,
    stores: Vec<CoveredStore<'s>>,
}

/// The entry's retrieval evidence, in the node arm's vocabulary.
fn edge_diagnostics(
    args: &VectorScoreArgs,
    has_index: bool,
    search_method: &str,
    store: String,
) -> RetrievalDiagnostics {
    let store = Some(store);
    if search_method == "hnsw" {
        let mut info = RetrievalDiagnostics::exact("unsupported_shape");
        info.actual_mode = "hnsw".into();
        info.fallback_reason = None;
        info.store = store;
        return info;
    }
    let mut info = RetrievalDiagnostics::exact(if args.options.exact {
        "forced_exact"
    } else if has_index {
        "metric_mismatch"
    } else {
        "no_index"
    });
    if args.options.exact {
        info.requested_policy = "exact".into();
    } else {
        info.store = store;
    }
    info
}
