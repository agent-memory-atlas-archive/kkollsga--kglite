//! Compile-ready P4 module draft.
//!
//! Production assumptions are limited to the storage-owned
//! `GeneratedEdgeEmbeddingWrite` and `install_generated_edge_embeddings` seam
//! declared in `interface-handoff.md`.

use std::collections::HashSet;

use petgraph::graph::EdgeIndex;

use crate::graph::algorithms::vector::DistanceMetric;
use crate::graph::algorithms::Interrupt;
use crate::graph::edge_embeddings::{
    edge_store_key, install_generated_edge_embeddings, GeneratedEdgeEmbeddingWrite,
};
use crate::graph::embedder::Embedder;
use crate::graph::embeddings::EmbedMode;
use crate::graph::schema::{DirGraph, EmbeddingStore};
use crate::graph::storage::GraphRead;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectedEdgeText {
    pub edge: EdgeIndex,
    /// `None` covers missing, non-string, and empty-string source values.
    pub text: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct EdgeGenerationRequest {
    pub connection_type: String,
    pub text_property: String,
    pub selected: Vec<SelectedEdgeText>,
    pub mode: EmbedMode,
    pub batch_size: usize,
    pub metric: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EdgeGenerationReport {
    pub embedded: usize,
    pub skipped: usize,
    pub skipped_existing: usize,
    pub reembedded_changed: usize,
    pub removed_missing: usize,
    pub stored: usize,
    pub dimension: usize,
    pub model_id: Option<String>,
}

pub(crate) struct EmbeddingExecutionService<'a> {
    pub model: &'a dyn Embedder,
    pub interrupt: Interrupt,
}

#[derive(Debug)]
struct Pending {
    edge: EdgeIndex,
    text: String,
    text_hash: u64,
}

#[derive(Default)]
struct SelectionPlan {
    pending: Vec<Pending>,
    remove_selected: Vec<EdgeIndex>,
    affected: Vec<EdgeIndex>,
    skipped: usize,
    skipped_existing: usize,
    reembedded_changed: usize,
    removed_missing: usize,
}

impl EmbeddingExecutionService<'_> {
    fn interrupted(&self) -> Result<(), String> {
        if self.interrupt.deadline_expired() {
            Err("relationship embedding generation exceeded its deadline".into())
        } else if self.interrupt.is_cancelled() {
            Err("relationship embedding generation was cancelled".into())
        } else {
            Ok(())
        }
    }

    /// Loads at most once and unloads on every exit after a successful load.
    fn generate(
        &self,
        pending: &[Pending],
        batch_size: usize,
    ) -> Result<Vec<(EdgeIndex, Vec<f32>, u64)>, String> {
        if pending.is_empty() {
            return Ok(Vec::new());
        }
        self.interrupted()?;
        self.model.load()?;
        let result = (|| {
            self.interrupted()?;
            let dimension = self.model.dimension();
            if dimension == 0 {
                return Err("embedder declared dimension 0".into());
            }
            let mut generated = Vec::with_capacity(pending.len());
            for batch in pending.chunks(batch_size) {
                self.interrupted()?;
                let texts: Vec<_> = batch.iter().map(|item| item.text.clone()).collect();
                let vectors = self.model.embed(&texts)?;
                self.interrupted()?;
                if vectors.len() != batch.len() {
                    return Err(format!(
                        "embedder returned {} vectors for {} relationship texts",
                        vectors.len(),
                        batch.len()
                    ));
                }
                for (item, vector) in batch.iter().zip(vectors) {
                    if vector.len() != dimension {
                        return Err(format!(
                            "embedder returned a {}-d vector for relationship slot {}, expected {dimension}",
                            vector.len(),
                            item.edge.index()
                        ));
                    }
                    if vector.iter().any(|value| !value.is_finite()) {
                        return Err(format!(
                            "embedder returned a non-finite vector for relationship slot {}",
                            item.edge.index()
                        ));
                    }
                    generated.push((item.edge, vector, item.text_hash));
                }
            }
            Ok(generated)
        })();
        self.model.unload();
        result
    }
}

pub(crate) fn embed_selected_relationships(
    graph: &mut DirGraph,
    request: EdgeGenerationRequest,
    service: Option<&EmbeddingExecutionService<'_>>,
) -> Result<EdgeGenerationReport, String> {
    validate_request_syntax(&request)?;
    let key = edge_store_key(&request.connection_type, &request.text_property);
    let existing = graph.edge_embeddings.get(&key);
    if request.selected.is_empty() {
        return Ok(EdgeGenerationReport {
            embedded: 0,
            skipped: 0,
            skipped_existing: 0,
            reembedded_changed: 0,
            removed_missing: 0,
            stored: existing.map_or(0, |store| store.len()),
            dimension: existing.map_or(0, |store| store.dimension()),
            model_id: existing
                .and_then(|store| store.model_id())
                .map(str::to_string),
        });
    }
    let service = service.ok_or_else(|| {
        "relationship embedding generation requires a registered embedder".to_string()
    })?;
    validate_selection(graph, &request.connection_type, &request.selected)?;

    let requested_model = service.model.model_id();
    if request.mode != EmbedMode::All {
        if let Some(prior_model) = existing.and_then(|store| store.model_id()) {
            if requested_model.as_deref() != Some(prior_model) {
                return Err(format!(
                    "the existing relationship embedding store was generated by model '{prior_model}', but the current model is {}; use mode='all' to rebuild the selected slice",
                    requested_model
                        .as_deref()
                        .map(|model| format!("'{model}'"))
                        .unwrap_or_else(|| "unknown".into())
                ));
            }
        }
    }

    let selected_slots: HashSet<_> = request
        .selected
        .iter()
        .map(|selected| selected.edge.index())
        .collect();
    let unselected_vectors_remain = existing.is_some_and(|store| {
        store
            .edges()
            .any(|edge| !selected_slots.contains(&edge.index()))
    });
    let dimension = service.model.dimension();
    if dimension == 0 {
        return Err("embedder declared dimension 0".into());
    }
    if let Some(store) = existing.filter(|store| store.dimension() != dimension) {
        if request.mode != EmbedMode::All || unselected_vectors_remain {
            return Err(format!(
                "the model produces {dimension}-d vectors but the existing relationship store is {}-d and retained vectors would remain",
                store.dimension()
            ));
        }
    }

    let mut plan = plan_selection(existing, &request.selected, request.mode);
    let generated = service.generate(&plan.pending, request.batch_size)?;
    let final_model_id = final_model_id(
        existing.and_then(|store| store.model_id()),
        requested_model.as_deref(),
        request.mode,
        unselected_vectors_remain,
    );
    let affected = std::mem::take(&mut plan.affected);
    let write = GeneratedEdgeEmbeddingWrite {
        dimension,
        metric: request.metric.clone(),
        final_model_id: final_model_id.clone(),
        generated,
        remove_selected: std::mem::take(&mut plan.remove_selected),
        affected,
    };
    let storage = install_generated_edge_embeddings(
        graph,
        &request.connection_type,
        &request.text_property,
        write,
    )?;
    Ok(EdgeGenerationReport {
        embedded: plan.pending.len(),
        skipped: plan.skipped,
        skipped_existing: plan.skipped_existing,
        reembedded_changed: plan.reembedded_changed,
        removed_missing: plan.removed_missing,
        stored: storage.stored,
        dimension: storage.dimension,
        model_id: final_model_id,
    })
}

fn validate_request_syntax(request: &EdgeGenerationRequest) -> Result<(), String> {
    if request.connection_type.is_empty() {
        return Err("relationship type must not be empty".into());
    }
    if request.text_property.is_empty() {
        return Err("text property must not be empty".into());
    }
    if request.batch_size == 0 {
        return Err("batch_size must be greater than zero".into());
    }
    if let Some(metric) = request.metric.as_deref() {
        if DistanceMetric::from_name(metric).is_none() {
            return Err(format!(
                "unknown distance metric '{metric}'; use cosine, dot_product, euclidean, or poincare"
            ));
        }
    }
    Ok(())
}

fn validate_selection(
    graph: &DirGraph,
    connection_type: &str,
    selected: &[SelectedEdgeText],
) -> Result<(), String> {
    let guard = graph.graph.begin_query();
    let mut seen = HashSet::with_capacity(selected.len());
    for item in selected {
        if !seen.insert(item.edge.index()) {
            return Err(format!(
                "relationship slot {} appears more than once in the selection",
                item.edge.index()
            ));
        }
        let relationship = graph
            .graph
            .edge_weight(item.edge)
            .ok_or_else(|| format!("relationship slot {} is not live", item.edge.index()))?;
        let actual = relationship.connection_type_str(&graph.interner);
        if actual != connection_type {
            return Err(format!(
                "relationship slot {} has type '{actual}', not '{connection_type}'",
                item.edge.index()
            ));
        }
    }
    drop(guard);
    Ok(())
}

fn plan_selection(
    existing: Option<&crate::graph::edge_embeddings::EdgeEmbeddingStore>,
    selected: &[SelectedEdgeText],
    mode: EmbedMode,
) -> SelectionPlan {
    let mut plan = SelectionPlan::default();
    for item in selected {
        let vector_exists = existing.is_some_and(|store| store.get(item.edge).is_some());
        let Some(text) = item.text.as_ref().filter(|text| !text.is_empty()) else {
            plan.skipped += 1;
            if mode == EmbedMode::All && vector_exists {
                plan.remove_selected.push(item.edge);
                plan.affected.push(item.edge);
                plan.removed_missing += 1;
            }
            continue;
        };
        let text_hash = EmbeddingStore::text_hash(text);
        let take = match mode {
            EmbedMode::All => true,
            EmbedMode::Missing => !vector_exists,
            EmbedMode::Changed => {
                !vector_exists
                    || existing.is_none_or(|store| store.text_hash(item.edge) != Some(text_hash))
            }
        };
        if take {
            if mode == EmbedMode::Changed && vector_exists {
                plan.reembedded_changed += 1;
            }
            plan.pending.push(Pending {
                edge: item.edge,
                text: text.clone(),
                text_hash,
            });
            plan.affected.push(item.edge);
        } else {
            plan.skipped_existing += 1;
        }
    }
    plan
}

fn final_model_id(
    prior: Option<&str>,
    requested: Option<&str>,
    mode: EmbedMode,
    unselected_vectors_remain: bool,
) -> Option<String> {
    if prior.is_some() && prior == requested {
        return requested.map(str::to_string);
    }
    if mode == EmbedMode::All && !unselected_vectors_remain {
        return requested.map(str::to_string);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datatypes::Value;
    use crate::graph::edge_embeddings::{
        install_generated_edge_embeddings, GeneratedEdgeEmbeddingWrite,
    };
    use crate::graph::schema::{EdgeData, NodeData};
    use crate::graph::storage::GraphWrite;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    enum Reply {
        Echo,
        WrongCount,
        WrongWidth,
        NonFinite,
        Error,
    }

    struct FakeEmbedder {
        dimension: usize,
        model_id: Option<String>,
        reply: Reply,
        loads: AtomicUsize,
        embeds: AtomicUsize,
        unloads: AtomicUsize,
    }

    impl FakeEmbedder {
        fn new(dimension: usize, reply: Reply) -> Self {
            Self {
                dimension,
                model_id: None,
                reply,
                loads: AtomicUsize::new(0),
                embeds: AtomicUsize::new(0),
                unloads: AtomicUsize::new(0),
            }
        }

        fn named(dimension: usize, model_id: &str, reply: Reply) -> Self {
            Self {
                model_id: Some(model_id.into()),
                ..Self::new(dimension, reply)
            }
        }
    }

    impl Embedder for FakeEmbedder {
        fn dimension(&self) -> usize {
            self.dimension
        }

        fn model_id(&self) -> Option<String> {
            self.model_id.clone()
        }

        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            self.embeds.fetch_add(1, Ordering::Relaxed);
            match self.reply {
                Reply::Echo => Ok(texts.iter().map(|_| vec![0.5; self.dimension]).collect()),
                Reply::WrongCount => Ok(Vec::new()),
                Reply::WrongWidth => Ok(texts.iter().map(|_| vec![0.5]).collect()),
                Reply::NonFinite => Ok(texts
                    .iter()
                    .map(|_| vec![f32::NAN; self.dimension])
                    .collect()),
                Reply::Error => Err("callback failed".into()),
            }
        }

        fn load(&self) -> Result<(), String> {
            self.loads.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn unload(&self) {
            self.unloads.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn pending(count: usize) -> Vec<Pending> {
        (0..count)
            .map(|slot| Pending {
                edge: EdgeIndex::new(slot),
                text: format!("text-{slot}"),
                text_hash: slot as u64,
            })
            .collect()
    }

    fn graph_with_parallel_edges() -> (DirGraph, EdgeIndex, EdgeIndex) {
        let mut graph = DirGraph::new();
        let source = GraphWrite::add_node(
            &mut graph.graph,
            NodeData::new(
                Value::Int64(1),
                Value::String("source".into()),
                "Doc".into(),
                HashMap::new(),
                &mut graph.interner,
            ),
        );
        let target = GraphWrite::add_node(
            &mut graph.graph,
            NodeData::new(
                Value::Int64(2),
                Value::String("target".into()),
                "Doc".into(),
                HashMap::new(),
                &mut graph.interner,
            ),
        );
        let first = GraphWrite::add_edge(
            &mut graph.graph,
            source,
            target,
            EdgeData::new("ASSERTS".into(), HashMap::new(), &mut graph.interner),
        );
        let second = GraphWrite::add_edge(
            &mut graph.graph,
            source,
            target,
            EdgeData::new("ASSERTS".into(), HashMap::new(), &mut graph.interner),
        );
        (graph, first, second)
    }

    fn seed(
        graph: &mut DirGraph,
        dimension: usize,
        model_id: Option<&str>,
        generated: Vec<(EdgeIndex, Vec<f32>, u64)>,
    ) {
        let affected = generated.iter().map(|(edge, _, _)| *edge).collect();
        install_generated_edge_embeddings(
            graph,
            "ASSERTS",
            "description",
            GeneratedEdgeEmbeddingWrite {
                dimension,
                metric: Some("cosine".into()),
                final_model_id: model_id.map(str::to_string),
                generated,
                remove_selected: Vec::new(),
                affected,
            },
        )
        .unwrap();
    }

    fn request(selected: Vec<SelectedEdgeText>, mode: EmbedMode) -> EdgeGenerationRequest {
        EdgeGenerationRequest {
            connection_type: "ASSERTS".into(),
            text_property: "description".into(),
            selected,
            mode,
            batch_size: 2,
            metric: Some("cosine".into()),
        }
    }

    #[test]
    fn generation_batches_and_unloads_once_after_success() {
        let model = FakeEmbedder::new(2, Reply::Echo);
        let service = EmbeddingExecutionService {
            model: &model,
            interrupt: Interrupt::default(),
        };
        let generated = service.generate(&pending(3), 2).unwrap();
        assert_eq!(generated.len(), 3);
        assert_eq!(model.loads.load(Ordering::Relaxed), 1);
        assert_eq!(model.embeds.load(Ordering::Relaxed), 2);
        assert_eq!(model.unloads.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn every_post_load_failure_unloads_without_returning_partial_vectors() {
        for reply in [
            Reply::WrongCount,
            Reply::WrongWidth,
            Reply::NonFinite,
            Reply::Error,
        ] {
            let model = FakeEmbedder::new(2, reply);
            let service = EmbeddingExecutionService {
                model: &model,
                interrupt: Interrupt::default(),
            };
            assert!(service.generate(&pending(2), 2).is_err());
            assert_eq!(model.loads.load(Ordering::Relaxed), 1);
            assert_eq!(model.unloads.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn cancellation_before_load_performs_no_model_lifecycle_call() {
        static CANCELLED: AtomicBool = AtomicBool::new(true);
        let model = FakeEmbedder::new(2, Reply::Echo);
        let service = EmbeddingExecutionService {
            model: &model,
            interrupt: Interrupt {
                deadline: None,
                cancel: Some(&CANCELLED),
            },
        };
        assert!(service.generate(&pending(1), 1).is_err());
        assert_eq!(model.loads.load(Ordering::Relaxed), 0);
        assert_eq!(model.embeds.load(Ordering::Relaxed), 0);
        assert_eq!(model.unloads.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn empty_pending_does_not_load_even_when_already_cancelled() {
        static CANCELLED: AtomicBool = AtomicBool::new(true);
        let model = FakeEmbedder::new(2, Reply::Echo);
        let service = EmbeddingExecutionService {
            model: &model,
            interrupt: Interrupt {
                deadline: None,
                cancel: Some(&CANCELLED),
            },
        };
        assert!(service.generate(&[], 1).unwrap().is_empty());
        assert_eq!(model.loads.load(Ordering::Relaxed), 0);
        assert_eq!(model.unloads.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn provenance_requires_matching_prior_or_full_all_coverage() {
        assert_eq!(
            final_model_id(Some("A"), Some("B"), EmbedMode::All, true),
            None
        );
        assert_eq!(
            final_model_id(Some("B"), Some("B"), EmbedMode::All, true),
            Some("B".into())
        );
        assert_eq!(
            final_model_id(None, Some("B"), EmbedMode::All, false),
            Some("B".into())
        );
        assert_eq!(
            final_model_id(None, Some("B"), EmbedMode::Changed, false),
            None
        );
    }

    #[test]
    fn empty_selection_is_a_noop_without_an_embedder() {
        let (mut graph, _, _) = graph_with_parallel_edges();
        let before = graph.version();
        let report =
            embed_selected_relationships(&mut graph, request(Vec::new(), EmbedMode::All), None)
                .unwrap();
        assert_eq!(report.stored, 0);
        assert_eq!(report.dimension, 0);
        assert_eq!(report.model_id, None);
        assert_eq!(graph.version(), before);
        assert!(graph.edge_embeddings.is_empty());
    }

    #[test]
    fn empty_selection_still_validates_syntactic_options() {
        let (mut graph, _, _) = graph_with_parallel_edges();
        let mut invalid = request(Vec::new(), EmbedMode::All);
        invalid.metric = Some("not-a-metric".into());
        assert!(embed_selected_relationships(&mut graph, invalid, None)
            .unwrap_err()
            .contains("unknown distance metric"));
        assert!(graph.edge_embeddings.is_empty());
    }

    #[test]
    fn nonempty_selection_requires_an_embedder_before_mutation() {
        let (mut graph, first, _) = graph_with_parallel_edges();
        let before = graph.version();
        let error = embed_selected_relationships(
            &mut graph,
            request(
                vec![SelectedEdgeText {
                    edge: first,
                    text: Some("claim".into()),
                }],
                EmbedMode::All,
            ),
            None,
        )
        .unwrap_err();
        assert!(error.contains("registered embedder"));
        assert_eq!(graph.version(), before);
        assert!(graph.edge_embeddings.is_empty());
    }

    #[test]
    fn partial_all_model_swap_preserves_unselected_and_clears_provenance() {
        let (mut graph, first, second) = graph_with_parallel_edges();
        seed(
            &mut graph,
            2,
            Some("A"),
            vec![(first, vec![1.0, 0.0], 11), (second, vec![0.0, 1.0], 22)],
        );
        let model = FakeEmbedder::named(2, "B", Reply::Echo);
        let service = EmbeddingExecutionService {
            model: &model,
            interrupt: Interrupt::default(),
        };
        let report = embed_selected_relationships(
            &mut graph,
            request(
                vec![SelectedEdgeText {
                    edge: first,
                    text: Some("changed".into()),
                }],
                EmbedMode::All,
            ),
            Some(&service),
        )
        .unwrap();

        let store = &graph.edge_embeddings[&edge_store_key("ASSERTS", "description")];
        assert_eq!(store.get(first), Some(&[0.5, 0.5][..]));
        assert_eq!(store.get(second), Some(&[0.0, 1.0][..]));
        assert_eq!(store.text_hash(second), Some(22));
        assert_eq!(store.model_id(), None);
        assert_eq!(report.model_id, None);
    }

    #[test]
    fn incremental_known_model_mismatch_rejects_before_load() {
        let (mut graph, first, _) = graph_with_parallel_edges();
        seed(&mut graph, 2, Some("A"), vec![(first, vec![1.0, 0.0], 11)]);
        let before = graph.version();
        let model = FakeEmbedder::named(2, "B", Reply::Echo);
        let service = EmbeddingExecutionService {
            model: &model,
            interrupt: Interrupt::default(),
        };
        assert!(embed_selected_relationships(
            &mut graph,
            request(
                vec![SelectedEdgeText {
                    edge: first,
                    text: Some("changed".into()),
                }],
                EmbedMode::Changed,
            ),
            Some(&service),
        )
        .unwrap_err()
        .contains("model 'A'"));
        assert_eq!(model.loads.load(Ordering::Relaxed), 0);
        assert_eq!(graph.version(), before);
    }

    #[test]
    fn incremental_unknown_store_stays_unknown_after_refresh() {
        let (mut graph, first, _) = graph_with_parallel_edges();
        seed(&mut graph, 2, None, vec![(first, vec![1.0, 0.0], 11)]);
        let model = FakeEmbedder::named(2, "B", Reply::Echo);
        let service = EmbeddingExecutionService {
            model: &model,
            interrupt: Interrupt::default(),
        };
        let report = embed_selected_relationships(
            &mut graph,
            request(
                vec![SelectedEdgeText {
                    edge: first,
                    text: Some("changed".into()),
                }],
                EmbedMode::Changed,
            ),
            Some(&service),
        )
        .unwrap();
        let store = &graph.edge_embeddings[&edge_store_key("ASSERTS", "description")];
        assert_eq!(store.get(first), Some(&[0.5, 0.5][..]));
        assert_eq!(store.model_id(), None);
        assert_eq!(report.model_id, None);
    }

    #[test]
    fn dimension_change_needs_only_prior_vector_coverage() {
        let (mut graph, first, second) = graph_with_parallel_edges();
        seed(&mut graph, 2, Some("A"), vec![(first, vec![1.0, 0.0], 11)]);
        let model = FakeEmbedder::named(3, "B", Reply::Echo);
        let service = EmbeddingExecutionService {
            model: &model,
            interrupt: Interrupt::default(),
        };
        let report = embed_selected_relationships(
            &mut graph,
            request(
                vec![SelectedEdgeText {
                    edge: first,
                    text: Some("changed".into()),
                }],
                EmbedMode::All,
            ),
            Some(&service),
        )
        .unwrap();

        let store = &graph.edge_embeddings[&edge_store_key("ASSERTS", "description")];
        assert_eq!(store.dimension(), 3);
        assert_eq!(store.get(first), Some(&[0.5, 0.5, 0.5][..]));
        assert_eq!(store.get(second), None);
        assert_eq!(store.model_id(), Some("B"));
        assert_eq!(report.model_id.as_deref(), Some("B"));
    }

    #[test]
    fn dimension_change_with_unselected_vector_rejects_before_load() {
        let (mut graph, first, second) = graph_with_parallel_edges();
        seed(
            &mut graph,
            2,
            Some("A"),
            vec![(first, vec![1.0, 0.0], 11), (second, vec![0.0, 1.0], 22)],
        );
        let before = graph.version();
        let model = FakeEmbedder::named(3, "B", Reply::Echo);
        let service = EmbeddingExecutionService {
            model: &model,
            interrupt: Interrupt::default(),
        };
        assert!(embed_selected_relationships(
            &mut graph,
            request(
                vec![SelectedEdgeText {
                    edge: first,
                    text: Some("changed".into()),
                }],
                EmbedMode::All,
            ),
            Some(&service),
        )
        .unwrap_err()
        .contains("retained vectors"));
        assert_eq!(model.loads.load(Ordering::Relaxed), 0);
        assert_eq!(graph.version(), before);
    }

    #[test]
    fn missing_source_is_retained_incrementally_and_removed_by_all() {
        let (mut graph, first, _) = graph_with_parallel_edges();
        seed(&mut graph, 2, Some("A"), vec![(first, vec![1.0, 0.0], 11)]);
        let model = FakeEmbedder::named(2, "A", Reply::Echo);
        let service = EmbeddingExecutionService {
            model: &model,
            interrupt: Interrupt::default(),
        };
        let selected = vec![SelectedEdgeText {
            edge: first,
            text: None,
        }];
        let incremental = embed_selected_relationships(
            &mut graph,
            request(selected.clone(), EmbedMode::Changed),
            Some(&service),
        )
        .unwrap();
        assert_eq!(incremental.skipped, 1);
        assert_eq!(
            graph.edge_embeddings[&edge_store_key("ASSERTS", "description")].get(first),
            Some(&[1.0, 0.0][..])
        );

        let rebuilt = embed_selected_relationships(
            &mut graph,
            request(selected, EmbedMode::All),
            Some(&service),
        )
        .unwrap();
        assert_eq!(rebuilt.removed_missing, 1);
        assert_eq!(rebuilt.stored, 0);
        assert_eq!(
            graph.edge_embeddings[&edge_store_key("ASSERTS", "description")].get(first),
            None
        );
    }

    #[test]
    fn callback_failure_preserves_store_and_version() {
        let (mut graph, first, _) = graph_with_parallel_edges();
        seed(&mut graph, 2, Some("A"), vec![(first, vec![1.0, 0.0], 11)]);
        let before = graph.version();
        let model = FakeEmbedder::named(2, "A", Reply::Error);
        let service = EmbeddingExecutionService {
            model: &model,
            interrupt: Interrupt::default(),
        };
        assert!(embed_selected_relationships(
            &mut graph,
            request(
                vec![SelectedEdgeText {
                    edge: first,
                    text: Some("changed".into()),
                }],
                EmbedMode::All,
            ),
            Some(&service),
        )
        .is_err());
        let store = &graph.edge_embeddings[&edge_store_key("ASSERTS", "description")];
        assert_eq!(store.get(first), Some(&[1.0, 0.0][..]));
        assert_eq!(store.text_hash(first), Some(11));
        assert_eq!(store.model_id(), Some("A"));
        assert_eq!(graph.version(), before);
        assert_eq!(model.unloads.load(Ordering::Relaxed), 1);
    }
}
