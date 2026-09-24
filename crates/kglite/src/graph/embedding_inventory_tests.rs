//! Inventory over node and relationship stores, including a node type and a
//! relationship type that share a name.

use super::*;
use crate::graph::edge_embeddings::upsert_edge_embeddings;
use crate::graph::embeddings::{list_embeddings, set_embeddings};
use crate::graph::schema::{EdgeData, NodeData};
use crate::graph::storage::GraphWrite;
use std::collections::HashMap;

/// Node type `SUPPORTS` (two nodes, `body` store of dimension 3) and
/// relationship type `SUPPORTS` (one edge with `body` and `note`, `body` store
/// of dimension 2, dot product), plus an unembedded `LINKS` edge.
fn same_name_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    let add_node = |graph: &mut DirGraph, id: i64, node_type: &str, body: Option<&str>| {
        let properties = body
            .map(|b| HashMap::from([("body".to_string(), Value::String(b.into()))]))
            .unwrap_or_default();
        GraphWrite::add_node(
            &mut graph.graph,
            NodeData::new(
                Value::Int64(id),
                Value::String(format!("n{id}")),
                node_type.into(),
                properties,
                &mut graph.interner,
            ),
        )
    };
    let a = add_node(&mut graph, 1, "SUPPORTS", Some("node text one"));
    let b = add_node(&mut graph, 2, "SUPPORTS", Some("node text two"));
    let c = add_node(&mut graph, 3, "Doc", None);
    graph.rebuild_type_indices_and_schemas();
    for conn in ["SUPPORTS", "LINKS"] {
        graph.register_connection_type(conn.into());
    }
    let edge_props = HashMap::from([
        ("body".to_string(), Value::String("edge text".into())),
        ("note".to_string(), Value::String("aside".into())),
    ]);
    let supports = GraphWrite::add_edge(
        &mut graph.graph,
        a,
        b,
        EdgeData::new("SUPPORTS".into(), edge_props, &mut graph.interner),
    );
    GraphWrite::add_edge(
        &mut graph.graph,
        b,
        c,
        EdgeData::new(
            "LINKS".into(),
            HashMap::from([("caption".to_string(), Value::String("a caption".into()))]),
            &mut graph.interner,
        ),
    );
    set_embeddings(
        &mut graph,
        "SUPPORTS",
        "body",
        None,
        [
            (Value::Int64(1), vec![1.0, 0.0, 0.0]),
            (Value::Int64(2), vec![0.0, 1.0, 0.0]),
        ],
    )
    .unwrap();
    upsert_edge_embeddings(
        &mut graph,
        "SUPPORTS",
        "body",
        vec![(supports, vec![0.6, 0.8])],
        Some("dot_product"),
    )
    .unwrap();
    graph
}

#[test]
fn relationship_listing_is_separate_from_the_node_listing() {
    let graph = same_name_graph();
    let nodes = list_embeddings(&graph);
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].dimension, 3);

    let edges = list_relationship_embeddings(&graph);
    assert_eq!(
        edges,
        vec![RelationshipEmbeddingStoreInfo {
            relationship_type: "SUPPORTS".into(),
            text_column: "body".into(),
            store_name: "body_emb".into(),
            dimension: 2,
            count: 1,
            metric: "dot_product".into(),
        }]
    );
}

#[test]
fn info_resolves_the_requested_entity_on_a_same_name_pair() {
    let graph = same_name_graph();
    let node = embedding_info(&graph, EmbeddingEntity::Node, "SUPPORTS", "body").unwrap();
    assert_eq!(
        (node.entity, node.dimension, node.count),
        (EmbeddingEntity::Node, 3, 2)
    );
    assert_eq!(node.metric, "cosine");

    let edge = embedding_info(&graph, EmbeddingEntity::Relationship, "SUPPORTS", "body").unwrap();
    assert_eq!(
        (edge.entity, edge.dimension, edge.count, edge.hashed),
        (EmbeddingEntity::Relationship, 2, 1, 0)
    );
    assert_eq!(
        (edge.metric.as_str(), edge.model.as_deref()),
        ("dot_product", None)
    );

    assert!(embedding_info(&graph, EmbeddingEntity::Relationship, "SUPPORTS", "note").is_none());
    assert!(embedding_info(&graph, EmbeddingEntity::Relationship, "LINKS", "caption").is_none());
}

#[test]
fn entity_parse_accepts_only_the_two_spellings() {
    assert_eq!(EmbeddingEntity::parse("node"), Ok(EmbeddingEntity::Node));
    assert_eq!(
        EmbeddingEntity::parse("relationship"),
        Ok(EmbeddingEntity::Relationship)
    );
    assert!(EmbeddingEntity::parse("edge")
        .unwrap_err()
        .contains("entity"));
}

fn summary(rows: &[EmbeddingDiagnostic]) -> Vec<(EmbeddingEntity, &str, &str, &str, usize, usize)> {
    rows.iter()
        .map(|row| {
            (
                row.entity,
                row.type_name.as_str(),
                row.text_column.as_str(),
                row.status.as_str(),
                row.with_property,
                row.embedded,
            )
        })
        .collect()
}

#[test]
fn diagnostics_scan_every_node_and_relationship_type_by_default() {
    use EmbeddingEntity::{Node, Relationship};
    let graph = same_name_graph();
    let rows = embedding_diagnostics(&graph, None, None).unwrap();
    assert_eq!(
        summary(&rows),
        vec![
            (Node, "SUPPORTS", "body", "embedded", 2, 2),
            (Relationship, "LINKS", "caption", "embeddable", 1, 0),
            (Relationship, "SUPPORTS", "body", "embedded", 1, 1),
            (Relationship, "SUPPORTS", "note", "embeddable", 1, 0),
        ],
        "LINKS carries no store, but its string property is a candidate like a node column"
    );
    let body = &rows[2];
    assert_eq!(body.embedding_key, "body_emb");
    assert_eq!(
        (body.dimension, body.metric.as_deref()),
        (Some(2), Some("dot_product"))
    );
    assert_eq!(body.length_stats.max_length, "edge text".len());
    assert_eq!(body.length_stats.distinct_ratio, 1.0);
}

#[test]
fn diagnostics_scopes_select_entities_and_reject_unknown_types() {
    use EmbeddingEntity::{Node, Relationship};
    let graph = same_name_graph();
    assert_eq!(
        summary(&embedding_diagnostics(&graph, None, Some("LINKS")).unwrap()),
        vec![(Relationship, "LINKS", "caption", "embeddable", 1, 0)]
    );
    assert_eq!(
        summary(&embedding_diagnostics(&graph, Some("SUPPORTS"), None).unwrap()),
        vec![(Node, "SUPPORTS", "body", "embedded", 2, 2)]
    );
    assert!(embedding_diagnostics(&graph, Some("NOPE"), None)
        .unwrap_err()
        .contains("Node type 'NOPE'"));
    assert!(embedding_diagnostics(&graph, None, Some("NOPE"))
        .unwrap_err()
        .contains("Relationship type 'NOPE'"));
}

#[test]
fn a_relationship_store_whose_property_is_gone_is_an_orphan() {
    let mut graph = same_name_graph();
    for edge in graph.graph.edge_indices().collect::<Vec<_>>() {
        if let Some(data) = graph.graph.edge_weight_mut(edge) {
            data.properties
                .retain(|(key, _)| *key != crate::graph::schema::InternedKey::from_str("body"));
        }
    }
    let rows = embedding_diagnostics(&graph, None, None).unwrap();
    let body = rows
        .iter()
        .find(|row| row.entity == EmbeddingEntity::Relationship && row.text_column == "body")
        .unwrap();
    assert_eq!(body.status, EmbeddingCoverage::StoreOrphan);
    assert_eq!((body.with_property, body.embedded), (0, 1));
}
