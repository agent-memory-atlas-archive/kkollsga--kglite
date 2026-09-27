//! Retrieval under a `FOR VALID_TIME AS OF` context: vector and BM25 ranking
//! over only the elements the statement's filter admits.
//!
//! One filter serves both routes and every tier — a prefixed statement on the
//! graph, and the same text on a `freeze(valid_at=…)` view (which is that
//! statement). [`CypherExecutor::retrieval_filter`] is the statement's own
//! filter in memory and mapped mode; in Disk mode, where that filter would
//! read the bound properties of every candidate, it is the instant mask
//! (`features::temporal::instant`).
//!
//! * **Vector** (`MATCH (n:T) RETURN … vector_score(n, …) AS s ORDER BY s
//!   DESC LIMIT k`): the admitted nodes of `T` are gathered — bit tests, no
//!   row — and the store's admitted slots are scored exactly, or, above
//!   `vector_mask::MASKED_EXACT_MAX` admitted vectors with a fresh index, the
//!   index's admitted candidates are. Nodes without a vector score NULL and
//!   rank first, as the unfused query ranks them.
//! * **BM25** (`text_bm25()` everywhere it appears): a query prepared under
//!   the filter carries the admitted documents' statistics — `N`, the mean
//!   length, each term's document frequency — so every admitted document
//!   scores what it would in an index of the admitted documents alone. The
//!   fused top-k then walks the postings with one admit test each.
//!
//! A type the filter can hide no node of takes the unfiltered routes.

use std::sync::Arc;

use petgraph::graph::{EdgeIndex, NodeIndex};

use super::retrieval::{FusedTopK, RetrievalPopulation, VectorScoreArgs};
use super::*;
use crate::graph::algorithms::text_index::bm25::PreparedQuery;
use crate::graph::features::temporal::endpoint_index::{self, TextStatsKey};
use crate::graph::features::temporal::instant::instant_filter;
use crate::graph::features::temporal::vector_mask;
use crate::graph::schema::EmbeddingStore;
use crate::graph::text_indexes::TextIndexRead;

impl CypherExecutor<'_> {
    /// The filter retrieval tests candidates against: `None` without a
    /// valid-time context (or when it hides nothing); the statement's filter
    /// in memory and mapped mode; the instant mask on Disk.
    pub(super) fn retrieval_filter(&self) -> Result<Option<Arc<ElementFilter>>, String> {
        let Some(filter) = self.graph_filter() else {
            return Ok(None);
        };
        if !self.graph.graph.is_disk() {
            return Ok(Some(Arc::clone(filter)));
        }
        let instant = filter
            .instant()
            .ok_or("a valid-time range cannot filter retrieval")?;
        let (_, masked) = instant_filter(self.graph, instant)?;
        Ok(masked.map(Arc::new))
    }

    /// [`Self::retrieval_filter`] when it can hide a node of `node_type`.
    pub(super) fn node_retrieval_filter(
        &self,
        node_type: &str,
    ) -> Result<Option<Arc<ElementFilter>>, String> {
        if self
            .graph_filter()
            .is_none_or(|filter| !filter.may_hide_type(self.graph, node_type))
        {
            return Ok(None);
        }
        self.retrieval_filter()
    }

    /// The nodes of `node_type` that `filter` admits, ascending.
    pub(super) fn admitted_nodes(
        &self,
        node_type: &str,
        filter: &ElementFilter,
    ) -> Result<Option<Vec<NodeIndex>>, String> {
        let Some(nodes) = self.graph.type_indices.get(node_type) else {
            return Ok(None);
        };
        self.budget.check_work(nodes.len(), "MATCH")?;
        let mut admitted = Vec::new();
        for (position, node) in nodes.iter().enumerate() {
            if position % INTERRUPT_POLL_INTERVAL == 0 {
                self.check_deadline()?;
            }
            if filter.admits_node(self.graph, node) {
                admitted.push(node);
            }
        }
        Ok(Some(admitted))
    }

    /// The node vector entry under a filter: `None` hands the clause to the
    /// guarded matcher and the per-row operator.
    pub(super) fn try_vector_entry_under_filter(
        &self,
        matched: &MatchClause,
        top: FusedTopK<'_>,
        score_call: &Expression,
    ) -> Result<Option<ResultSet>, String> {
        let Some((variable, node_type)) = self.plain_retrieval_type(matched) else {
            return Ok(None);
        };
        let Some(filter) = self.node_retrieval_filter(node_type)? else {
            return self.try_whole_type_vector_entry(matched, top, score_call);
        };
        let Some(admitted) = self.admitted_nodes(node_type, &filter)? else {
            return Ok(None);
        };
        if admitted.is_empty() {
            return Ok(None);
        }
        let population = RetrievalPopulation::Admitted {
            nodes: &admitted,
            variable,
        };
        let score_expr = self.fold_constants_expr(score_call);
        let seed = population.row(0);
        let Some(args) = self.constant_vector_args(&score_expr, &seed)? else {
            return Ok(None);
        };
        if args.variable != variable {
            return Ok(None);
        }
        let Some(store) = self.graph.embedding_store(node_type, &args.property) else {
            return Ok(None);
        };
        if store.len() == 0 || !slots_ascend_within_type(self.graph, store, node_type) {
            return Ok(None);
        }
        let unembedded: Vec<usize> = (0..admitted.len())
            .filter(|&position| !store.node_to_slot.contains_key(&admitted[position].index()))
            .collect();
        let nulls = if top.non_null_only {
            0
        } else {
            unembedded.len().min(top.limit)
        };
        let admits = |node: usize| filter.admits_node(self.graph, NodeIndex::new(node));
        let (winners, info) =
            self.with_prepared_vector_score(&score_expr, &seed, (store, node_type), |prepared| {
                let embedded = admitted.len() - unembedded.len();
                self.masked_vector_winners(
                    store,
                    prepared,
                    (&args, node_type),
                    (top.limit - nulls, embedded),
                    &admits,
                )
            })?;
        let position = |slot: usize| {
            admitted
                .binary_search(&NodeIndex::new(store.slot_to_node[slot]))
                .expect("an admitted slot's node is an admitted node")
        };
        let scored = unembedded[..nulls]
            .iter()
            .map(|&p| (p, Value::Null))
            .chain(winners.into_iter().map(|(slot, v)| (position(slot), v)))
            .collect::<Vec<_>>();
        let result = self.project_retrieval_winners(
            scored.into_iter(),
            &score_expr,
            &population,
            top.return_clause,
            top.score_item_index,
        )?;
        self.record_retrieval(info);
        Ok(Some(result))
    }

    /// The best `limit` admitted slots as `(slot, score)`, and the route that
    /// ranked them: the index's admitted candidates when `embedded` (the
    /// admitted vectors) reaches the threshold and a fresh index serves the
    /// metric, else an exact pass over the admitted slots.
    fn masked_vector_winners(
        &self,
        store: &EmbeddingStore,
        prepared: &VectorScoreCache,
        (args, node_type): (&VectorScoreArgs, &str),
        (limit, embedded): (usize, usize),
        admits: &dyn Fn(usize) -> bool,
    ) -> Result<(Vec<(usize, Value)>, RetrievalDiagnostics), String> {
        let mut info = RetrievalDiagnostics::exact("exact_mask");
        info.store = Some(format!("{node_type}.{}", args.property));
        if args.options.exact {
            info.requested_policy = "exact".into();
        }
        if limit == 0 {
            return Ok((Vec::new(), info));
        }
        if !args.options.exact && vector_mask::prefers_index(embedded) {
            if let Some(winners) =
                self.indexed_admitted(store, prepared, args, (limit, embedded), admits)
            {
                info.actual_mode = "hnsw_mask".into();
                info.fallback_reason = None;
                return Ok((winners, info));
            }
        }
        let winners = self.exact_vector_winners(store, prepared, limit, Some(admits))?;
        Ok((winners, info))
    }

    /// The index route of [`Self::masked_vector_winners`]; `None` when no
    /// fresh index serves the metric or the index ran short.
    fn indexed_admitted(
        &self,
        store: &EmbeddingStore,
        prepared: &VectorScoreCache,
        args: &VectorScoreArgs,
        (limit, embedded): (usize, usize),
        admits: &dyn Fn(usize) -> bool,
    ) -> Option<Vec<(usize, Value)>> {
        use crate::graph::algorithms::hnsw::HnswMetric;
        use crate::graph::algorithms::vector::DistanceMetric;
        let metric = args
            .options
            .metric
            .or_else(|| DistanceMetric::from_name(store.metric.as_deref().unwrap_or("cosine")));
        let index = store.index_for_query(self.graph.read_only)?;
        if metric.and_then(HnswMetric::from_distance) != Some(index.metric()) {
            return None;
        }
        let slots =
            vector_mask::hnsw_admitted(store, &index, &args.query, limit, embedded, admits)?;
        let mut scored: Vec<(usize, f64)> = slots
            .into_iter()
            .map(|slot| {
                let slot = slot as usize;
                let start = slot * store.dimension;
                let score = prepared.scorer.score(
                    &prepared.query_vec,
                    &store.data[start..start + store.dimension],
                    store.norms[slot],
                );
                (slot, score as f64)
            })
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        scored.truncate(limit);
        Some(
            scored
                .into_iter()
                .map(|(slot, score)| (slot, Value::Float64(score)))
                .collect(),
        )
    }

    /// A `text_bm25` query over `owner.property`, prepared under the
    /// statement's filter when it can hide one of the index's documents —
    /// with the admitted documents' statistics, cached per instant — and
    /// plainly otherwise. The second value is the admitted document count
    /// under a filter.
    pub(super) fn prepare_text_query(
        &self,
        view: &TextIndexRead<'_>,
        text: &str,
        (relationship, owner, property): (bool, &str, &str),
        generation: u64,
    ) -> Result<(PreparedQuery, Option<usize>), String> {
        let Some(filter) = self.text_filter(relationship, owner)? else {
            return Ok((view.prepare_query(text), None));
        };
        let admits = self.text_admits(&filter, relationship);
        let key = TextStatsKey {
            relationship,
            owner_type: owner.to_string(),
            property: property.to_string(),
            generation,
            instant: filter
                .instant()
                .ok_or("a valid-time range cannot filter text_bm25()")?,
        };
        let index = view.index();
        let stats = endpoint_index::text_stats(self.graph, key, || index.masked_stats(&*admits));
        let prepared = index.prepare_query_masked(text, stats, &*admits);
        Ok((prepared, Some(stats.docs)))
    }

    /// The retrieval filter when it can hide one of `owner`'s documents.
    pub(super) fn text_filter(
        &self,
        relationship: bool,
        owner: &str,
    ) -> Result<Option<Arc<ElementFilter>>, String> {
        let hides = self.graph_filter().is_some_and(|filter| {
            if relationship {
                filter.may_hide_relationship_type(self.graph, owner)
            } else {
                filter.may_hide_type(self.graph, owner)
            }
        });
        if hides {
            self.retrieval_filter()
        } else {
            Ok(None)
        }
    }

    /// The admit test on a text index slot: a node, or a relationship with
    /// both its endpoints.
    pub(super) fn text_admits<'f>(
        &'f self,
        filter: &'f ElementFilter,
        relationship: bool,
    ) -> Box<dyn Fn(u32) -> bool + 'f> {
        let graph = self.graph;
        if !relationship {
            return Box::new(move |slot| filter.admits_node(graph, NodeIndex::new(slot as usize)));
        }
        Box::new(move |slot| {
            let edge = EdgeIndex::new(slot as usize);
            match (
                graph.graph.edge_endpoints(edge),
                graph.graph.edge_weight(edge),
            ) {
                (Some((source, target)), Some(weight)) => {
                    filter.admits_relationship(graph, edge, weight.connection_type, source, target)
                }
                _ => false,
            }
        })
    }
}

/// Whether `store`'s slots hold live nodes of `node_type` in ascending node
/// order — the order the guarded scan produces them in, so slot order breaks
/// score ties as the unfused query does.
fn slots_ascend_within_type(graph: &DirGraph, store: &EmbeddingStore, node_type: &str) -> bool {
    let key = InternedKey::from_str(node_type);
    store.slot_to_node.windows(2).all(|pair| pair[0] < pair[1])
        && store
            .slot_to_node
            .iter()
            .all(|&node| graph.graph.node_type_of(NodeIndex::new(node)) == Some(key))
}
