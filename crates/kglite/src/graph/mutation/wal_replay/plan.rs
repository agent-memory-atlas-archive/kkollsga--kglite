use std::collections::{HashMap, HashSet};

use crate::datatypes::Value;
use crate::graph::wal::{MutationOp, WalFrame};

use super::declarations::Declarations;

pub(super) type NodeKey = (String, Value);
pub(super) type EdgeKey = (String, String, Value, String, Value);
pub(super) type Properties = Vec<(String, Value)>;

/// `key` with its id in the one spelling the id index treats every numeric
/// spelling as: `UniqueId(5)`, `Int64(5)` and `Float64(5.0)` are one node. A
/// UniqueId column widened to Int64 mid-log logs the same node under both, so
/// any slot or identity map keyed by the raw `Value` splits it in two.
pub(super) fn node_slot(node_type: &str, id: &Value) -> NodeKey {
    (
        node_type.to_string(),
        crate::graph::schema::canonical_id(id).into_owned(),
    )
}

fn edge_slot(key: &EdgeKey) -> EdgeKey {
    let canonical = |id| crate::graph::schema::canonical_id(id).into_owned();
    (
        key.0.clone(),
        key.1.clone(),
        canonical(&key.2),
        key.3.clone(),
        canonical(&key.4),
    )
}

#[derive(Default)]
pub(super) struct NodeState {
    pub row: Option<(Value, Properties)>,
    pub removed: bool,
    pub reset: bool,
    pub generation: u64,
    pub labels: Option<Vec<String>>,
}

pub(super) struct EdgeState {
    pub properties: Option<Properties>,
    pub reset: bool,
    pub group: Option<Vec<Properties>>,
    source_generation: u64,
    target_generation: u64,
}

#[derive(Default)]
pub(super) struct ReplayPlan {
    /// Keyed by the latest spelling logged for the node; the slot maps are
    /// keyed by [`node_slot`] / `edge_slot`.
    pub nodes: Vec<(NodeKey, NodeState)>,
    node_slots: HashMap<NodeKey, usize>,
    pub edges: Vec<(EdgeKey, EdgeState)>,
    edge_slots: HashMap<EdgeKey, usize>,
    /// Declarations. Deliberately *not* folded into `nodes`: a declaration
    /// has no id, so an identity-keyed slot would either drop it or coalesce
    /// it into some node's state, and the op would be logged but never
    /// applied — indistinguishable from not logging it at all.
    pub declarations: Declarations,
    pub edge_embedding_events: Vec<super::edge_embeddings::OrderedEdgeEmbeddingEvent>,
    pub max_lsn: u64,
}

impl ReplayPlan {
    pub fn fold(frames: &[WalFrame], after: u64) -> Self {
        let mut plan = Self {
            max_lsn: after,
            ..Self::default()
        };
        for frame in frames.iter().filter(|frame| frame.lsn > after) {
            plan.max_lsn = plan.max_lsn.max(frame.lsn);
            for op in &frame.ops {
                if let Some(event) = super::edge_embeddings::event_from_op(op) {
                    plan.edge_embedding_events.push(event);
                }
                plan.fold_op(op);
            }
        }
        plan
    }

    fn node_mut(&mut self, key: NodeKey) -> &mut NodeState {
        let slot_key = node_slot(&key.0, &key.1);
        match self.node_slots.get(&slot_key) {
            Some(&slot) => {
                // A created node is installed under the spelling it last had.
                self.nodes[slot].0 = key;
                &mut self.nodes[slot].1
            }
            None => {
                let slot = self.nodes.len();
                self.node_slots.insert(slot_key, slot);
                self.nodes.push((key, NodeState::default()));
                &mut self.nodes[slot].1
            }
        }
    }

    fn fold_op(&mut self, op: &MutationOp) {
        // Declarations key on what they declare, not on an identity, so they
        // fold in their own structure — see `declarations`.
        if self.declarations.fold(op) {
            return;
        }
        match op {
            MutationOp::ReplaceNodeState {
                node_type,
                id,
                title,
                properties,
                labels,
                reset,
            } => {
                let node = self.node_mut((node_type.clone(), id.clone()));
                if *reset {
                    node.reset = true;
                    node.generation += 1;
                }
                node.row = Some((title.clone(), properties.clone()));
                node.removed = false;
                node.labels = Some(labels.clone());
            }
            MutationOp::ReplaceEdgeGroup {
                conn_type,
                src_type,
                src_id,
                tgt_type,
                tgt_id,
                edges,
            } => {
                let key = (
                    conn_type.clone(),
                    src_type.clone(),
                    src_id.clone(),
                    tgt_type.clone(),
                    tgt_id.clone(),
                );
                self.fold_edge(key.clone(), None);
                let slot = self.edge_slots[&edge_slot(&key)];
                self.edges[slot].1.group = Some(edges.clone());
            }
            MutationOp::UpsertNode {
                node_type,
                id,
                title,
                properties,
            } => {
                let node = self.node_mut((node_type.clone(), id.clone()));
                node.row = Some((title.clone(), properties.clone()));
                node.removed = false;
            }
            MutationOp::RemoveNode { node_type, id } => {
                // The payload half of the barrier. A slot is handed to the
                // next node created, so a timeseries or vector still pending
                // for this identity would land on whatever recreates it.
                self.declarations.forget_node(node_type, id);
                let node = self.node_mut((node_type.clone(), id.clone()));
                node.row = None;
                node.removed = true;
                node.reset = true;
                node.generation += 1;
                node.labels = None;
            }
            MutationOp::SetNodeLabels {
                node_type,
                id,
                labels,
            } => {
                self.node_mut((node_type.clone(), id.clone())).labels = Some(labels.clone());
            }
            MutationOp::UpsertEdge {
                conn_type,
                src_type,
                src_id,
                tgt_type,
                tgt_id,
                properties,
            } => {
                self.fold_edge(
                    (
                        conn_type.clone(),
                        src_type.clone(),
                        src_id.clone(),
                        tgt_type.clone(),
                        tgt_id.clone(),
                    ),
                    Some(properties.clone()),
                );
            }
            MutationOp::RemoveEdge {
                conn_type,
                src_type,
                src_id,
                tgt_type,
                tgt_id,
            } => {
                self.fold_edge(
                    (
                        conn_type.clone(),
                        src_type.clone(),
                        src_id.clone(),
                        tgt_type.clone(),
                        tgt_id.clone(),
                    ),
                    None,
                );
            }
            // Declarations are handled above, before this match runs.
            MutationOp::SetTypeFieldAliases { .. }
            | MutationOp::SetTypeParent { .. }
            | MutationOp::SetOntology { .. }
            | MutationOp::SetSchemaVersion { .. }
            | MutationOp::SetSpatialConfig { .. }
            | MutationOp::SetPropertyIndex { .. }
            | MutationOp::SetConstraint { .. }
            | MutationOp::SetNodeTimeseries { .. }
            | MutationOp::SetTimeseriesConfig { .. }
            | MutationOp::SetEmbeddings { .. }
            | MutationOp::SetVectorIndex { .. }
            | MutationOp::SetEdgeVectorIndex { .. }
            | MutationOp::SetTemporalDeclaration { .. }
            | MutationOp::SetEdgeEmbeddingStore { .. }
            | MutationOp::ReplaceEdgeGroupEmbeddings { .. }
            | MutationOp::PatchEdgeGroupEmbeddings { .. } => {}
        }
    }

    fn fold_edge(&mut self, key: EdgeKey, properties: Option<Properties>) {
        let slot_key = edge_slot(&key);
        let prior_reset = self
            .edge_slots
            .get(&slot_key)
            .is_some_and(|&slot| self.edges[slot].1.reset);
        let state = EdgeState {
            reset: properties.is_none() || prior_reset,
            group: None,
            properties,
            source_generation: self.generation(&(key.1.clone(), key.2.clone())),
            target_generation: self.generation(&(key.3.clone(), key.4.clone())),
        };
        if let Some(&slot) = self.edge_slots.get(&slot_key) {
            self.edges[slot] = (key, state);
        } else {
            self.edge_slots.insert(slot_key, self.edges.len());
            self.edges.push((key, state));
        }
    }

    fn generation(&self, key: &NodeKey) -> u64 {
        self.node_slots
            .get(&node_slot(&key.0, &key.1))
            .map_or(0, |&slot| self.nodes[slot].1.generation)
    }

    pub fn node_removed(&self, key: &NodeKey) -> bool {
        self.node_slots
            .get(&node_slot(&key.0, &key.1))
            .is_some_and(|&slot| self.nodes[slot].1.removed)
    }

    pub fn edge_survives(&self, key: &EdgeKey, state: &EdgeState) -> bool {
        let source = (key.1.clone(), key.2.clone());
        let target = (key.3.clone(), key.4.clone());
        !self.node_removed(&source)
            && !self.node_removed(&target)
            && self.generation(&source) == state.source_generation
            && self.generation(&target) == state.target_generation
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
            && self.edges.is_empty()
            && self.declarations.is_empty()
            && self.edge_embedding_events.is_empty()
    }

    pub fn node_types(&self) -> HashSet<String> {
        self.nodes
            .iter()
            .map(|(key, _)| key.0.clone())
            .chain(
                self.edges
                    .iter()
                    .flat_map(|(key, _)| [key.1.clone(), key.3.clone()]),
            )
            .collect()
    }

    pub fn edge_types(&self) -> HashSet<String> {
        self.edges.iter().map(|(key, _)| key.0.clone()).collect()
    }

    /// Relationship types named by this plan's embedding events. Bounds the
    /// base capture in `capture_edge_embedding_state`, which has to record base
    /// groups for a type whose first store this log creates and which therefore
    /// has no store to find it by.
    pub fn embedding_conn_types(&self) -> std::collections::BTreeSet<String> {
        use super::edge_embeddings::OrderedEdgeEmbeddingEvent as Event;
        self.edge_embedding_events
            .iter()
            .map(|event| match event {
                Event::ReplaceTopology { key, .. }
                | Event::ReplaceEmbeddings { key, .. }
                | Event::PatchEmbeddings { key, .. } => key.conn_type.clone(),
                Event::SetStore { key, .. } => key.conn_type.clone(),
            })
            .collect()
    }
}
