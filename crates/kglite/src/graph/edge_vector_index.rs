//! Apply-ready P5 core shape for `graph/edge_vector_index.rs`.
//! Requires the small `EmbeddingStore::{take,restore}_index_state` seam and
//! `MutationOp::SetEdgeVectorIndex` described in storage-design.md.

use petgraph::graph::EdgeIndex;

use super::{edge_store_key, EdgeEmbeddingStore};
use crate::graph::algorithms::hnsw::HnswParams;
use crate::graph::algorithms::vector::{self as vs, DistanceMetric};
use crate::graph::embedding_validation::validate_finite_vector;
use crate::graph::schema::DirGraph;
use crate::graph::storage::GraphRead;

#[derive(Debug, Clone, Default)]
pub(crate) struct EdgeVectorIndexOptions {
    pub(crate) m: Option<usize>,
    pub(crate) ef_construction: Option<usize>,
    pub(crate) ef_search: Option<usize>,
    pub(crate) metric: Option<String>,
    pub(crate) auto_refresh_limit: Option<usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct EdgeVectorIndexReport {
    pub(crate) indexed: usize,
    pub(crate) metric: String,
    pub(crate) m: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct EdgeVectorQueryOptions {
    pub(crate) top_k: usize,
    pub(crate) exact: bool,
    pub(crate) metric: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EdgeVectorQueryHit {
    pub(crate) edge: EdgeIndex,
    pub(crate) score: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EdgeVectorQueryReport {
    pub(crate) hits: Vec<EdgeVectorQueryHit>,
    pub(crate) search_method: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EdgeVectorIndexStatus {
    pub(crate) connection_type: String,
    pub(crate) text_property: String,
    pub(crate) built: bool,
    pub(crate) stale: bool,
    pub(crate) delta: usize,
    pub(crate) unembedded: usize,
}

impl EdgeEmbeddingStore {
    /// The dense store the HNSW index is built over. `.kgl` persistence reads
    /// the index and its freshness state through it, exactly as for a node
    /// store, which is why the relationship section can reuse that payload.
    pub(crate) fn index_store(&self) -> &crate::graph::schema::EmbeddingStore {
        &self.numeric
    }

    /// Mutable twin of [`Self::index_store`], for attaching a persisted index.
    pub(crate) fn index_store_mut(&mut self) -> &mut crate::graph::schema::EmbeddingStore {
        &mut self.numeric
    }
}

pub(crate) fn list_edge_vector_indexes(graph: &DirGraph) -> Vec<EdgeVectorIndexStatus> {
    let guard = graph.graph.begin_query();
    let mut live_by_type = std::collections::HashMap::<&str, usize>::new();
    for edge in graph.graph.edge_indices() {
        if let Some(weight) = graph.graph.edge_weight(edge) {
            *live_by_type
                .entry(weight.connection_type_str(&graph.interner))
                .or_default() += 1;
        }
    }
    let mut statuses = graph
        .edge_embeddings
        .iter()
        .map(
            |((connection_type, store_name), store)| EdgeVectorIndexStatus {
                connection_type: connection_type.clone(),
                text_property: crate::graph::embeddings::text_column_of(store_name)
                    .unwrap_or(store_name)
                    .to_string(),
                built: store.numeric.has_index(),
                stale: store.numeric.index_is_stale(),
                delta: store.numeric.delta_size(),
                unembedded: live_by_type
                    .get(connection_type.as_str())
                    .copied()
                    .unwrap_or_default()
                    .saturating_sub(store.len()),
            },
        )
        .collect::<Vec<_>>();
    drop(guard);
    statuses.sort_by(|left, right| {
        (&left.connection_type, &left.text_property)
            .cmp(&(&right.connection_type, &right.text_property))
    });
    statuses
}

pub(crate) fn build_edge_vector_index(
    graph: &mut DirGraph,
    connection_type: &str,
    text_property: &str,
    options: EdgeVectorIndexOptions,
) -> Result<EdgeVectorIndexReport, String> {
    let key = edge_store_key(connection_type, text_property);
    let store = graph.edge_embeddings.get_mut(&key).ok_or_else(|| {
        format!("No relationship embedding store '{connection_type}.{text_property}' to index")
    })?;
    let metric_name = options
        .metric
        .clone()
        .or_else(|| store.metric().map(str::to_owned))
        .unwrap_or_else(|| "cosine".to_string());
    // Indexability first: a metric HNSW cannot serve at all is a more useful
    // answer than which metric the store prefers.
    let metric = indexable_metric(&metric_name)?;
    // An explicit metric becomes the store's metric so a later metric-less
    // query resolves the one the index was built for. Without this the index
    // answered under one metric while the store still declared another, every
    // default query mismatched it and fell back to the exact scan, and `list`
    // reported the metric nothing used. A metric the store already declares
    // differently is refused rather than overwritten: the vectors were scored
    // under it.
    if let (Some(requested), Some(stored)) = (options.metric.as_deref(), store.metric()) {
        if requested != stored {
            return Err(format!(
                "Relationship embedding store '{connection_type}.{text_property}' declares metric \
                 '{stored}', but this build requested '{requested}'. Build with '{stored}', or \
                 drop the store and set it again with metric '{requested}'."
            ));
        }
    }
    let params = resolve_params(&options)?;
    // `db.edge_embeddings.build_index` runs inside a statement window, so the
    // one field this build writes outside the index state needs an undo story.
    // `EdgeVectorIndexReplaced` restores the index, not the metric, and there
    // is no per-field entry for it — so the rare call that actually moves the
    // metric journals the whole prior store. It is O(store) exactly once per
    // store: afterwards the store declares a metric, and a later build either
    // agrees with it or is refused above.
    let metric_change_prior = (options.metric.is_some()
        && store.metric() != options.metric.as_deref())
    .then(|| store.clone());
    let prior = store.numeric.take_index_state();
    if let Some(limit) = options.auto_refresh_limit {
        store.numeric.set_auto_refresh_limit(limit);
    }
    let indexed = store.len();
    let seed = 0x9E37_79B9_7F4A_7C15 ^ indexed as u64;
    if let Err(error) = store.numeric.build_index(metric, params, seed) {
        store.numeric.restore_index_state(prior);
        return Err(error);
    }
    // After the build, so a failed build leaves the store's metric alone.
    if options.metric.is_some() {
        store.numeric.metric = Some(metric_name.clone());
    }
    let auto_refresh_limit = store.numeric.auto_refresh_limit();
    if let Some(journal) = graph.graph.undo_journal_mut() {
        // Pushed first so the reverse-order undo restores the index state, then
        // the whole prior store — leaving exactly the pre-statement store.
        if let Some(prior_store) = metric_change_prior {
            journal.note_edge_embedding_store_replaced(key.clone(), Some(prior_store));
        }
        journal.note_edge_vector_index_replaced(key.clone(), prior);
    }
    graph.note_declaration(crate::graph::wal::MutationOp::SetEdgeVectorIndex {
        conn_type: connection_type.to_string(),
        text_column: text_property.to_string(),
        metric: Some(metric_name.clone()),
        m: Some(params.m),
        ef_construction: Some(params.ef_construction),
        ef_search: Some(params.ef_search),
        auto_refresh_limit: Some(auto_refresh_limit),
        present: true,
    });
    graph.bump_version();
    Ok(EdgeVectorIndexReport {
        indexed,
        metric: metric_name,
        m: params.m,
    })
}

pub(crate) fn refresh_edge_vector_index(
    graph: &DirGraph,
    connection_type: &str,
    text_property: &str,
) -> Result<usize, String> {
    let store = graph
        .edge_embeddings
        .get(&edge_store_key(connection_type, text_property))
        .ok_or_else(|| {
            format!("No relationship embedding store '{connection_type}.{text_property}'")
        })?;
    if graph.read_only {
        return Ok(0);
    }
    Ok(store.numeric.refresh_index())
}

pub(crate) fn apply_edge_vector_index_declaration(
    graph: &mut DirGraph,
    connection_type: &str,
    text_property: &str,
    options: EdgeVectorIndexOptions,
    present: bool,
) -> Result<(), String> {
    let Some(store) = graph
        .edge_embeddings
        .get_mut(&edge_store_key(connection_type, text_property))
    else {
        return Ok(());
    };
    if !present {
        store.numeric.invalidate_index();
        return Ok(());
    }
    let metric_name = options
        .metric
        .as_deref()
        .or(store.metric())
        .unwrap_or("cosine");
    let metric = indexable_metric(metric_name)?;
    let params = resolve_params(&options)?;
    if let Some(limit) = options.auto_refresh_limit {
        store.numeric.set_auto_refresh_limit(limit);
    }
    let seed = 0x9E37_79B9_7F4A_7C15 ^ store.len() as u64;
    let declared = options.metric.clone();
    store.numeric.build_index(metric, params, seed)?;
    // Replay reproduces the store the build left behind, metric included.
    // Unlike the caller-facing path this reconciles rather than refuses: a log
    // written before the metric was persisted can carry an index metric its
    // store contradicts, and a recovery that refused it would make the graph
    // unopenable.
    if declared.is_some() {
        store.numeric.metric = declared;
    }
    Ok(())
}

pub(crate) fn drop_edge_vector_index(
    graph: &mut DirGraph,
    connection_type: &str,
    text_property: &str,
) -> Result<bool, String> {
    let key = edge_store_key(connection_type, text_property);
    let Some(store) = graph.edge_embeddings.get_mut(&key) else {
        return Ok(false);
    };
    if !store.numeric.has_index() {
        return Ok(false);
    }
    let prior = store.numeric.take_index_state();
    if let Some(journal) = graph.graph.undo_journal_mut() {
        journal.note_edge_vector_index_replaced(key, prior);
    }
    graph.note_declaration(crate::graph::wal::MutationOp::SetEdgeVectorIndex {
        conn_type: connection_type.to_string(),
        text_column: text_property.to_string(),
        metric: None,
        m: None,
        ef_construction: None,
        ef_search: None,
        auto_refresh_limit: None,
        present: false,
    });
    graph.bump_version();
    Ok(true)
}

pub(crate) fn query_edge_embeddings(
    graph: &DirGraph,
    connection_type: &str,
    text_property: &str,
    query: &[f32],
    options: EdgeVectorQueryOptions,
) -> Result<EdgeVectorQueryReport, String> {
    validate_finite_vector(query)
        .map_err(|error| format!("Invalid relationship embedding query: {error}"))?;
    let store = graph
        .edge_embeddings
        .get(&edge_store_key(connection_type, text_property))
        .ok_or_else(|| {
            format!("No relationship embedding store '{connection_type}.{text_property}'")
        })?;
    if query.len() != store.dimension() {
        return Err(format!(
            "Query dimension {} does not match store dimension {}",
            query.len(),
            store.dimension()
        ));
    }
    if options.top_k == 0 || store.is_empty() {
        return Ok(EdgeVectorQueryReport {
            hits: vec![],
            search_method: "exact",
        });
    }
    let metric = resolve_query_metric(store, options.metric.as_deref())?;
    Ok(query_store(
        store,
        query,
        options.top_k,
        options.exact,
        metric,
        graph.read_only,
    ))
}

/// The store half of [`query_edge_embeddings`], for a caller that has already
/// validated the query against the store and resolved the metric — the fused
/// `vector_score(r, …) ORDER BY … LIMIT k` route, whose argument errors must
/// keep the scalar's wording. Same routes, fallbacks and `search_method`.
pub(crate) fn query_store(
    store: &EdgeEmbeddingStore,
    query: &[f32],
    top_k: usize,
    exact: bool,
    metric: DistanceMetric,
    read_only: bool,
) -> EdgeVectorQueryReport {
    if top_k == 0 || store.is_empty() {
        return EdgeVectorQueryReport {
            hits: vec![],
            search_method: "exact",
        };
    }
    if !exact {
        if let Some(hits) = query_index(store, query, top_k, metric, read_only) {
            return EdgeVectorQueryReport {
                hits,
                search_method: "hnsw",
            };
        }
    }
    EdgeVectorQueryReport {
        hits: query_exact(store, query, top_k, metric),
        search_method: "exact",
    }
}

pub(crate) fn resolve_query_metric(
    store: &EdgeEmbeddingStore,
    requested: Option<&str>,
) -> Result<DistanceMetric, String> {
    let name = requested.or(store.metric()).unwrap_or("cosine");
    DistanceMetric::from_name(name).ok_or_else(|| format!("Unknown metric '{name}'"))
}

fn indexable_metric(name: &str) -> Result<DistanceMetric, String> {
    let metric =
        DistanceMetric::from_name(name).ok_or_else(|| format!("Unknown metric '{name}'"))?;
    if crate::graph::algorithms::hnsw::HnswMetric::from_distance(metric).is_none() {
        return Err(format!(
            "the '{name}' metric is not supported by HNSW; use exact relationship search"
        ));
    }
    Ok(metric)
}

fn resolve_params(options: &EdgeVectorIndexOptions) -> Result<HnswParams, String> {
    if options.m.is_some_and(|m| m < 2) {
        return Err("HNSW option 'm' must be at least 2".to_string());
    }
    if options.ef_construction == Some(0) {
        return Err("HNSW option 'ef_construction' must be greater than 0".to_string());
    }
    if options.ef_search == Some(0) {
        return Err("HNSW option 'ef_search' must be greater than 0".to_string());
    }
    let defaults = HnswParams::default();
    Ok(HnswParams {
        m: options.m.unwrap_or(defaults.m),
        ef_construction: options.ef_construction.unwrap_or(defaults.ef_construction),
        ef_search: options.ef_search.unwrap_or(defaults.ef_search),
    })
}

fn query_exact(
    store: &EdgeEmbeddingStore,
    query: &[f32],
    top_k: usize,
    metric: DistanceMetric,
) -> Vec<EdgeVectorQueryHit> {
    let scorer = vs::Scorer::new(metric, query);
    let mut hits = store
        .numeric
        .slot_to_node
        .iter()
        .enumerate()
        .map(|(slot, &edge)| {
            let start = slot * store.numeric.dimension;
            EdgeVectorQueryHit {
                edge: EdgeIndex::new(edge),
                score: scorer.score(
                    query,
                    &store.numeric.data[start..start + store.numeric.dimension],
                    store.numeric.norms[slot],
                ) as f64,
            }
        })
        .collect::<Vec<_>>();
    sort_and_truncate(&mut hits, top_k);
    hits
}

fn query_index(
    store: &EdgeEmbeddingStore,
    query: &[f32],
    top_k: usize,
    metric: DistanceMetric,
    read_only: bool,
) -> Option<Vec<EdgeVectorQueryHit>> {
    let index = store.numeric.index_for_query(read_only)?;
    if crate::graph::algorithms::hnsw::HnswMetric::from_distance(metric) != Some(index.metric()) {
        return None;
    }
    let scorer = vs::Scorer::new(metric, query);
    let query_norm = vs::dot_product(query, query).sqrt();
    let ef = top_k.max(index.params().ef_search);
    let raw = index.search(
        query,
        query_norm,
        top_k,
        Some(ef),
        &store.numeric.data,
        &store.numeric.norms,
    );
    let mut hits = raw
        .into_iter()
        .map(|(slot, _)| {
            let slot = slot as usize;
            let start = slot * store.numeric.dimension;
            EdgeVectorQueryHit {
                edge: EdgeIndex::new(store.numeric.slot_to_node[slot]),
                score: scorer.score(
                    query,
                    &store.numeric.data[start..start + store.numeric.dimension],
                    store.numeric.norms[slot],
                ) as f64,
            }
        })
        .collect::<Vec<_>>();
    sort_and_truncate(&mut hits, top_k);
    Some(hits)
}

/// Best `top_k` first: score descending, then edge index ascending — a total
/// order, so the result is deterministic. Partitions before sorting, so an
/// exact scan over a large store sorts `top_k` hits rather than all of them.
fn sort_and_truncate(hits: &mut Vec<EdgeVectorQueryHit>, top_k: usize) {
    let order = |left: &EdgeVectorQueryHit, right: &EdgeVectorQueryHit| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.edge.index().cmp(&right.edge.index()))
    };
    if top_k < hits.len() {
        hits.select_nth_unstable_by(top_k, order);
        hits.truncate(top_k);
    }
    hits.sort_by(order);
}
