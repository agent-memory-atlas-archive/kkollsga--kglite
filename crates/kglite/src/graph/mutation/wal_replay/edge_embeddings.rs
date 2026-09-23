use std::collections::{BTreeMap, BTreeSet};

use crate::datatypes::Value;
use crate::graph::algorithms::vector::DistanceMetric;
use crate::graph::embedding_validation::validate_finite_vector;
use crate::graph::wal::EdgeGroupEmbeddingPatchWal;
use crate::graph::wal::{EdgeEmbeddingStoreState, EdgeGroupStoreWalState, MutationOp};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LogicalGroupKey {
    pub conn_type: String,
    pub src_type: String,
    pub src_id: Value,
    pub tgt_type: String,
    pub tgt_id: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct LogicalStoreKey {
    pub conn_type: String,
    pub text_column: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct LogicalStoreMetadata {
    pub dimension: usize,
    pub metric: Option<String>,
    pub model_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LogicalGroupState {
    pub properties: Vec<Vec<(String, Value)>>,
    pub stores: BTreeMap<String, Vec<Option<crate::graph::wal::EdgeVectorWalState>>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct LogicalEdgeEmbeddingState {
    pub stores: BTreeMap<LogicalStoreKey, LogicalStoreMetadata>,
    pub groups: BTreeMap<LogicalGroupKey, LogicalGroupState>,
}

/// Topology and embedding events retain their frame and operation order. The
/// ordinary replay plan may independently fold topology to its final state.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum OrderedEdgeEmbeddingEvent {
    ReplaceTopology {
        key: LogicalGroupKey,
        edges: Vec<Vec<(String, Value)>>,
    },
    SetStore {
        key: LogicalStoreKey,
        state: EdgeEmbeddingStoreState,
    },
    ReplaceEmbeddings {
        key: LogicalGroupKey,
        member_count: usize,
        stores: Vec<EdgeGroupStoreWalState>,
    },
    PatchEmbeddings {
        key: LogicalGroupKey,
        patch: EdgeGroupEmbeddingPatchWal,
    },
}

pub(super) fn event_from_op(op: &MutationOp) -> Option<OrderedEdgeEmbeddingEvent> {
    match op {
        MutationOp::ReplaceEdgeGroup {
            conn_type,
            src_type,
            src_id,
            tgt_type,
            tgt_id,
            edges,
        } => Some(OrderedEdgeEmbeddingEvent::ReplaceTopology {
            key: LogicalGroupKey {
                conn_type: conn_type.clone(),
                src_type: src_type.clone(),
                src_id: src_id.clone(),
                tgt_type: tgt_type.clone(),
                tgt_id: tgt_id.clone(),
            },
            edges: edges.clone(),
        }),
        MutationOp::SetEdgeEmbeddingStore {
            conn_type,
            text_column,
            state,
        } => Some(OrderedEdgeEmbeddingEvent::SetStore {
            key: LogicalStoreKey {
                conn_type: conn_type.clone(),
                text_column: text_column.clone(),
            },
            state: state.clone(),
        }),
        MutationOp::ReplaceEdgeGroupEmbeddings {
            conn_type,
            src_type,
            src_id,
            tgt_type,
            tgt_id,
            member_count,
            stores,
        } => Some(OrderedEdgeEmbeddingEvent::ReplaceEmbeddings {
            key: LogicalGroupKey {
                conn_type: conn_type.clone(),
                src_type: src_type.clone(),
                src_id: src_id.clone(),
                tgt_type: tgt_type.clone(),
                tgt_id: tgt_id.clone(),
            },
            member_count: *member_count,
            stores: stores.clone(),
        }),
        MutationOp::PatchEdgeGroupEmbeddings {
            conn_type,
            src_type,
            src_id,
            tgt_type,
            tgt_id,
            patch,
        } => Some(OrderedEdgeEmbeddingEvent::PatchEmbeddings {
            key: LogicalGroupKey {
                conn_type: conn_type.clone(),
                src_type: src_type.clone(),
                src_id: src_id.clone(),
                tgt_type: tgt_type.clone(),
                tgt_id: tgt_id.clone(),
            },
            patch: patch.clone(),
        }),
        _ => None,
    }
}

impl LogicalEdgeEmbeddingState {
    // Retained as the full-state correctness oracle for delta comparison tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn interpret(
        mut self,
        events: impl IntoIterator<Item = OrderedEdgeEmbeddingEvent>,
    ) -> Result<Self, String> {
        for event in events {
            self.apply_event(event)?;
        }
        self.validate_complete()?;
        Ok(self)
    }

    /// Intermediate state can be incomplete because declarations, topology,
    /// and group matrices are separate ordered WAL operations.
    pub(super) fn apply_event(&mut self, event: OrderedEdgeEmbeddingEvent) -> Result<(), String> {
        match event {
            OrderedEdgeEmbeddingEvent::ReplaceTopology { key, edges } => {
                let prior = self.groups.remove(&key);
                let stores = prior.map_or_else(BTreeMap::new, |group| group.stores);
                self.groups.insert(
                    key,
                    LogicalGroupState {
                        properties: edges,
                        stores,
                    },
                );
            }
            OrderedEdgeEmbeddingEvent::SetStore { key, state } => match state {
                EdgeEmbeddingStoreState::Absent => {
                    validate_store_key(&key)?;
                    self.stores.remove(&key);
                    for (group_key, group) in &mut self.groups {
                        if group_key.conn_type == key.conn_type {
                            group.stores.remove(&key.text_column);
                        }
                    }
                }
                EdgeEmbeddingStoreState::Present {
                    dimension,
                    metric,
                    model_id,
                } => {
                    validate_store_key(&key)?;
                    if dimension == 0 {
                        return Err(format!(
                            "relationship embedding store '{}.{}' has zero dimension",
                            key.conn_type, key.text_column
                        ));
                    }
                    if let Some(name) = metric.as_deref() {
                        if DistanceMetric::from_name(name).is_none() {
                            return Err(format!(
                                "relationship embedding store '{}.{}' has unknown metric '{name}'",
                                key.conn_type, key.text_column
                            ));
                        }
                    }
                    self.stores.insert(
                        key,
                        LogicalStoreMetadata {
                            dimension,
                            metric,
                            model_id,
                        },
                    );
                }
            },
            OrderedEdgeEmbeddingEvent::ReplaceEmbeddings {
                key,
                member_count,
                stores,
            } => {
                let group = self.groups.get_mut(&key).ok_or_else(|| {
                    format!("embedding state precedes topology for {}", key.conn_type)
                })?;
                if group.properties.len() != member_count {
                    return Err(format!(
                        "relationship group '{}' has {member_count} embedding members but {} topology members",
                        key.conn_type,
                        group.properties.len()
                    ));
                }
                let mut seen = BTreeSet::new();
                let mut replacement = BTreeMap::new();
                for store in stores {
                    if !seen.insert(store.text_column.clone()) {
                        return Err(format!(
                            "relationship group '{}' repeats store '{}'",
                            key.conn_type, store.text_column
                        ));
                    }
                    if store.members.len() != member_count {
                        return Err(format!(
                            "relationship group '{}.{}' has {} cells, expected {member_count}",
                            key.conn_type,
                            store.text_column,
                            store.members.len()
                        ));
                    }
                    replacement.insert(store.text_column, store.members);
                }
                group.stores = replacement;
            }
            OrderedEdgeEmbeddingEvent::PatchEmbeddings { .. } => {
                return Err("relationship embedding patch requires ordered delta replay".into());
            }
        }
        Ok(())
    }

    pub(super) fn validate_complete(&self) -> Result<(), String> {
        for (group_key, group) in &self.groups {
            let expected: BTreeSet<_> = self
                .stores
                .keys()
                .filter(|key| key.conn_type == group_key.conn_type)
                .map(|key| key.text_column.as_str())
                .collect();
            let actual: BTreeSet<_> = group.stores.keys().map(String::as_str).collect();
            if actual != expected {
                return Err(format!(
                    "relationship group '{}' does not contain complete store state",
                    group_key.conn_type
                ));
            }
            for (text_column, members) in &group.stores {
                let store_key = LogicalStoreKey {
                    conn_type: group_key.conn_type.clone(),
                    text_column: text_column.clone(),
                };
                let metadata = &self.stores[&store_key];
                if members.len() != group.properties.len() {
                    return Err("embedding/topology member cardinality differs".into());
                }
                for state in members.iter().flatten() {
                    if state.vector.len() != metadata.dimension {
                        return Err(format!(
                            "relationship embedding '{}.{}' has dimension {}, expected {}",
                            store_key.conn_type,
                            store_key.text_column,
                            state.vector.len(),
                            metadata.dimension
                        ));
                    }
                    validate_finite_vector(&state.vector).map_err(|error| error.to_string())?;
                }
            }
        }
        Ok(())
    }
}

fn validate_store_key(key: &LogicalStoreKey) -> Result<(), String> {
    if key.conn_type.is_empty() {
        return Err("relationship embedding store has an empty relationship type".into());
    }
    if key.text_column.is_empty() {
        return Err(format!(
            "relationship embedding store '{}' has an empty source text column",
            key.conn_type
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::wal::EdgeVectorWalState;

    fn group() -> LogicalGroupKey {
        LogicalGroupKey {
            conn_type: "ASSERTS".into(),
            src_type: "Doc".into(),
            src_id: Value::Int64(1),
            tgt_type: "Doc".into(),
            tgt_id: Value::Int64(2),
        }
    }

    fn store() -> LogicalStoreKey {
        LogicalStoreKey {
            conn_type: "ASSERTS".into(),
            text_column: "description".into(),
        }
    }

    fn props(value: i64) -> Vec<Vec<(String, Value)>> {
        vec![vec![("revision".into(), Value::Int64(value))]]
    }

    fn vector(x: f32, hash: Option<u64>) -> EdgeGroupStoreWalState {
        EdgeGroupStoreWalState {
            text_column: "description".into(),
            members: vec![Some(EdgeVectorWalState {
                vector: vec![x, 1.0 - x],
                text_hash: hash,
            })],
        }
    }

    #[test]
    fn ordered_full_state_preserves_property_and_provenance_updates() {
        let result = LogicalEdgeEmbeddingState::default()
            .interpret([
                OrderedEdgeEmbeddingEvent::SetStore {
                    key: store(),
                    state: EdgeEmbeddingStoreState::Present {
                        dimension: 2,
                        metric: Some("cosine".into()),
                        model_id: Some("model-a".into()),
                    },
                },
                OrderedEdgeEmbeddingEvent::ReplaceTopology {
                    key: group(),
                    edges: props(1),
                },
                OrderedEdgeEmbeddingEvent::ReplaceEmbeddings {
                    key: group(),
                    member_count: 1,
                    stores: vec![vector(0.25, Some(7))],
                },
                OrderedEdgeEmbeddingEvent::ReplaceTopology {
                    key: group(),
                    edges: props(2),
                },
                OrderedEdgeEmbeddingEvent::SetStore {
                    key: store(),
                    state: EdgeEmbeddingStoreState::Present {
                        dimension: 2,
                        metric: Some("cosine".into()),
                        model_id: None,
                    },
                },
                OrderedEdgeEmbeddingEvent::ReplaceEmbeddings {
                    key: group(),
                    member_count: 1,
                    stores: vec![vector(0.75, None)],
                },
            ])
            .unwrap();
        assert_eq!(result.groups[&group()].properties, props(2));
        assert_eq!(result.stores[&store()].model_id, None);
        assert_eq!(
            result.groups[&group()].stores["description"][0]
                .as_ref()
                .unwrap()
                .text_hash,
            None
        );
    }

    #[test]
    fn full_record_rejects_omission_bad_width_and_nonfinite_values() {
        let base = LogicalEdgeEmbeddingState {
            stores: BTreeMap::from([(
                store(),
                LogicalStoreMetadata {
                    dimension: 2,
                    metric: None,
                    model_id: None,
                },
            )]),
            groups: BTreeMap::new(),
        };
        for (stores, message) in [
            (vec![], "complete store state"),
            (
                vec![EdgeGroupStoreWalState {
                    text_column: "description".into(),
                    members: vec![Some(EdgeVectorWalState {
                        vector: vec![1.0],
                        text_hash: None,
                    })],
                }],
                "dimension 1, expected 2",
            ),
            (
                vec![EdgeGroupStoreWalState {
                    text_column: "description".into(),
                    members: vec![Some(EdgeVectorWalState {
                        vector: vec![f32::NAN, 0.0],
                        text_hash: None,
                    })],
                }],
                "must be finite",
            ),
        ] {
            let error = base
                .clone()
                .interpret([
                    OrderedEdgeEmbeddingEvent::ReplaceTopology {
                        key: group(),
                        edges: props(1),
                    },
                    OrderedEdgeEmbeddingEvent::ReplaceEmbeddings {
                        key: group(),
                        member_count: 1,
                        stores,
                    },
                ])
                .unwrap_err();
            assert!(error.contains(message), "unexpected error: {error}");
        }
    }

    #[test]
    fn absent_and_present_empty_are_distinct() {
        let present = LogicalEdgeEmbeddingState::default()
            .interpret([OrderedEdgeEmbeddingEvent::SetStore {
                key: store(),
                state: EdgeEmbeddingStoreState::Present {
                    dimension: 2,
                    metric: None,
                    model_id: None,
                },
            }])
            .unwrap();
        assert!(present.stores.contains_key(&store()));
        let absent = present
            .interpret([OrderedEdgeEmbeddingEvent::SetStore {
                key: store(),
                state: EdgeEmbeddingStoreState::Absent,
            }])
            .unwrap();
        assert!(!absent.stores.contains_key(&store()));

        let invalid_metric = LogicalEdgeEmbeddingState::default()
            .interpret([OrderedEdgeEmbeddingEvent::SetStore {
                key: store(),
                state: EdgeEmbeddingStoreState::Present {
                    dimension: 2,
                    metric: Some("angular".into()),
                    model_id: None,
                },
            }])
            .unwrap_err();
        assert!(invalid_metric.contains("unknown metric 'angular'"));

        for invalid in [
            LogicalStoreKey {
                conn_type: String::new(),
                text_column: "description".into(),
            },
            LogicalStoreKey {
                conn_type: "ASSERTS".into(),
                text_column: String::new(),
            },
        ] {
            let error = LogicalEdgeEmbeddingState::default()
                .interpret([OrderedEdgeEmbeddingEvent::SetStore {
                    key: invalid,
                    state: EdgeEmbeddingStoreState::Present {
                        dimension: 2,
                        metric: None,
                        model_id: None,
                    },
                }])
                .unwrap_err();
            assert!(error.contains("empty"));
        }
    }

    #[test]
    fn mutation_mapping_keeps_topology_and_embedding_operations_orderable() {
        let topology = MutationOp::ReplaceEdgeGroup {
            conn_type: "ASSERTS".into(),
            src_type: "Doc".into(),
            src_id: Value::Int64(1),
            tgt_type: "Doc".into(),
            tgt_id: Value::Int64(2),
            edges: props(3),
        };
        let embedding = MutationOp::ReplaceEdgeGroupEmbeddings {
            conn_type: "ASSERTS".into(),
            src_type: "Doc".into(),
            src_id: Value::Int64(1),
            tgt_type: "Doc".into(),
            tgt_id: Value::Int64(2),
            member_count: 1,
            stores: vec![vector(0.5, None)],
        };
        assert!(matches!(
            event_from_op(&topology),
            Some(OrderedEdgeEmbeddingEvent::ReplaceTopology { .. })
        ));
        assert!(matches!(
            event_from_op(&embedding),
            Some(OrderedEdgeEmbeddingEvent::ReplaceEmbeddings { .. })
        ));
    }
}
