//! `embedding(entity, 'col_emb')` — the stored vector itself, as a list of
//! floats, for a node or a relationship.
//!
//! The read-out twin of `vector_score` / `embedding_norm`: the same first
//! argument (a MATCH binding, or a node / relationship value from `collect`,
//! `UNWIND`, a `CALL { }` column or `db.edge_embeddings.query`), the same
//! store-name second argument, and the same split between "this entity has no
//! vector" (`null`) and "its type has no such store" (an error naming the type
//! and the source property). Its point is composition:
//! `vector_score(r2, 'col_emb', embedding(r1, 'col_emb'))` scores one
//! relationship against another, since `vector_score` takes any list
//! expression as its query.
use super::super::*;
use crate::datatypes::values::Value;
use crate::graph::storage::GraphRead;
use petgraph::graph::NodeIndex;

const USAGE: &str = "embedding() requires 2 arguments: (entity, store), e.g. \
                     embedding(n, 'summary_emb')";
const NOT_AN_ENTITY: &str = "embedding(): first argument must be a node or a relationship";

impl<'a> CypherExecutor<'a> {
    /// Owns `embedding`; `Ok(None)` for every other name.
    pub(super) fn eval_embedding_readout_fn(
        &self,
        name: &str,
        args: &[Expression],
        row: &ResultRow,
    ) -> Result<Option<Value>, String> {
        if name != "embedding" {
            return Ok(None);
        }
        if args.len() != 2 {
            return Err(USAGE.into());
        }
        let store = match self.evaluate_expression(&args[1], row)? {
            Value::String(store) => store,
            _ => return Err("embedding(): second argument must be a store name string".into()),
        };
        let vector = match self.readout_target(&args[0], row)? {
            ReadoutTarget::Node(node_idx) => self.node_vector(node_idx, &store)?,
            ReadoutTarget::Edge(edge) => self.edge_vector(edge, &store)?,
            ReadoutTarget::Nothing => None,
        };
        Ok(Some(vector.map_or(Value::Null, |vector| {
            Value::List(
                vector
                    .iter()
                    .map(|&component| Value::Float64(component as f64))
                    .collect(),
            )
        })))
    }

    /// What the first argument names. A binding wins over evaluation, as in
    /// `embedding_norm`; a value resolves through the same slot checks, and a
    /// value whose slot has since died or been retyped names nothing.
    fn readout_target(&self, arg: &Expression, row: &ResultRow) -> Result<ReadoutTarget, String> {
        if let Expression::Variable(variable) = arg {
            if let Some(edge) = row.edge_bindings.get(variable) {
                return Ok(ReadoutTarget::Edge(*edge));
            }
            if let Some(&node_idx) = row.node_bindings.get(variable) {
                return Ok(ReadoutTarget::Node(node_idx));
            }
            if row.path_bindings.contains_key(variable) {
                return Err(NOT_AN_ENTITY.into());
            }
        }
        match self.evaluate_expression(arg, row)? {
            Value::Relationship(relationship) => Ok(ReadoutTarget::Edge(
                self.projected_relationship_binding(&relationship)?,
            )),
            Value::Node(node) => {
                let node_idx = NodeIndex::new(node.id as usize);
                let current = self
                    .graph
                    .graph
                    .node_view(node_idx)
                    .map(|view| view.node_type_str(&self.graph.interner));
                // `labels[0]` is the primary type; a value of a node deleted
                // earlier in the statement carries none, so it never matches.
                Ok(match (current, node.labels.first()) {
                    (Some(current), Some(label)) if current == label => {
                        ReadoutTarget::Node(node_idx)
                    }
                    _ => ReadoutTarget::Nothing,
                })
            }
            Value::NodeRef(index) => {
                let node_idx = NodeIndex::new(index as usize);
                Ok(if self.graph.graph.node_view(node_idx).is_some() {
                    ReadoutTarget::Node(node_idx)
                } else {
                    ReadoutTarget::Nothing
                })
            }
            Value::Null => Ok(ReadoutTarget::Nothing),
            other => Err(format!("{NOT_AN_ENTITY}, got {}", other.type_name())),
        }
    }

    fn node_vector(&self, node_idx: NodeIndex, store: &str) -> Result<Option<&[f32]>, String> {
        let Some(node_type) = self
            .graph
            .graph
            .node_view(node_idx)
            .map(|view| view.node_type_str(&self.graph.interner))
        else {
            return Ok(None);
        };
        match self.graph.embedding_store(node_type, store) {
            Some(found) => Ok(found.get_embedding(node_idx.index())),
            None => Err(missing_store_error(
                store,
                "node type",
                node_type,
                |suffixed| self.graph.embedding_store(node_type, suffixed).is_some(),
            )),
        }
    }

    fn edge_vector(&self, edge: EdgeBinding, store: &str) -> Result<Option<&[f32]>, String> {
        if !self.relationship_binding_is_current(&edge) {
            return Ok(None);
        }
        let Some(weight) = self.graph.graph.edge_weight(edge.edge_index) else {
            return Ok(None);
        };
        let rel_type = weight.connection_type_str(&self.graph.interner);
        match self
            .graph
            .edge_embeddings
            .get(&(rel_type.to_string(), store.to_string()))
        {
            Some(found) => Ok(found.get(edge.edge_index)),
            None => Err(missing_store_error(
                store,
                "relationship type",
                rel_type,
                |suffixed| {
                    self.graph
                        .edge_embeddings
                        .contains_key(&(rel_type.to_string(), suffixed.to_string()))
                },
            )),
        }
    }
}

enum ReadoutTarget {
    Node(NodeIndex),
    Edge(EdgeBinding),
    Nothing,
}

/// `embedding(): no embedding store 'x_emb' (source property 'x') for node type
/// 'T'`, plus a hint when the caller wrote the source property and the
/// `_emb` store exists.
fn missing_store_error(
    store: &str,
    entity: &str,
    type_name: &str,
    has_store: impl Fn(&str) -> bool,
) -> String {
    let source = crate::graph::embeddings::text_column_of(store).unwrap_or(store);
    let base = format!(
        "embedding(): no embedding store '{store}' (source property '{source}') for {entity} \
         '{type_name}'"
    );
    let suffixed = crate::graph::embeddings::store_name(store);
    if crate::graph::embeddings::text_column_of(store).is_none() && has_store(&suffixed) {
        format!(
            "{base}. Did you mean '{suffixed}'? embedding() takes the embedding store name, as \
             vector_score() does."
        )
    } else {
        base
    }
}
