use super::*;
use crate::datatypes::values::{NodeValue, PathValue, RelValue};
use crate::datatypes::PropMap;
use crate::graph::languages::cypher::result::CypherResult;
use std::collections::HashSet;

fn graph_with_one_relationship() -> DirGraph {
    let mut graph = DirGraph::new();
    for id in 1..=2 {
        let node = GraphWrite::add_node(
            &mut graph.graph,
            NodeData::new(
                Value::Int64(id),
                Value::String(format!("n{id}")),
                "N".into(),
                HashMap::new(),
                &mut graph.interner,
            ),
        );
        graph.type_indices.entry_or_default("N".into()).push(node);
    }
    GraphWrite::add_edge(
        &mut graph.graph,
        petgraph::graph::NodeIndex::new(0),
        petgraph::graph::NodeIndex::new(1),
        EdgeData::new(
            "R".into(),
            HashMap::from([("tag".into(), Value::String("old".into()))]),
            &mut graph.interner,
        ),
    );
    graph
}

fn run_mutation(graph: &mut DirGraph, query: &str) -> CypherResult {
    let parsed = parser::parse_cypher(query).unwrap();
    super::super::write::execute_mutable(
        graph,
        &parsed,
        HashMap::new(),
        crate::graph::algorithms::Interrupt::from_deadline(None),
    )
    .unwrap()
}

#[test]
fn stale_binding_cannot_read_reused_slot_or_its_embedding() {
    let mut graph = graph_with_one_relationship();
    let result = run_mutation(
        &mut graph,
        "MATCH (a:N)-[r:R]->(b:N) DELETE r CREATE (a)-[fresh:R {tag:'fresh'}]->(b) \
         WITH r, fresh CALL db.edge_embeddings.set({type:'R', text_property:'text', \
         entries:[{relationship:fresh, vector:[1.0,0.0]}]}) YIELD stored \
         RETURN r.tag AS stale_tag, fresh.tag AS fresh_tag, r AS stale, \
         vector_score(r,'text_emb',[1.0,0.0]) AS stale_score, \
         embedding_norm(r,'text_emb') AS stale_norm, \
         vector_score(fresh,'text_emb',[1.0,0.0]) AS fresh_score",
    );
    assert_eq!(result.rows.len(), 1, "{result:?}");
    assert_eq!(result.rows[0][0], Value::Null);
    assert_eq!(result.rows[0][1], Value::String("fresh".into()));
    let Value::Relationship(stale) = &result.rows[0][2] else {
        panic!("expected stale relationship")
    };
    assert!(stale.properties.is_empty());
    assert_eq!(result.rows[0][3], Value::Null);
    assert_eq!(result.rows[0][4], Value::Null);
    assert_eq!(result.rows[0][5], Value::Float64(1.0));
}

#[test]
fn optimized_vector_score_where_does_not_keep_stale_reused_slot() {
    let mut graph = graph_with_one_relationship();
    let result = run_mutation(
        &mut graph,
        "MATCH (a:N)-[r:R]->(b:N) DELETE r CREATE (a)-[fresh:R]->(b) \
         WITH r, fresh CALL db.edge_embeddings.set({type:'R', text_property:'text', \
         entries:[{relationship:fresh, vector:[1.0,0.0]}]}) YIELD stored \
         WITH r WHERE vector_score(r,'text_emb',[1.0,0.0]) > 0.5 RETURN r",
    );
    assert!(result.rows.is_empty(), "{result:?}");
}

#[test]
fn detach_delete_retires_incident_slot_but_fresh_reuse_is_valid() {
    let mut graph = DirGraph::new();
    let source = GraphWrite::add_node(
        &mut graph.graph,
        NodeData::new(
            Value::Int64(1),
            Value::String("source".into()),
            "N".into(),
            HashMap::new(),
            &mut graph.interner,
        ),
    );
    let target = GraphWrite::add_node(
        &mut graph.graph,
        NodeData::new(
            Value::Int64(2),
            Value::String("target".into()),
            "N".into(),
            HashMap::new(),
            &mut graph.interner,
        ),
    );
    let edge = GraphWrite::add_edge(
        &mut graph.graph,
        source,
        target,
        EdgeData::new("R".into(), HashMap::new(), &mut graph.interner),
    );
    let mut identities =
        super::super::relationship_identity::StatementRelationshipIdentities::new();
    let stale = identities.capture(edge);

    super::super::write::invalidate_deleted_relationships(
        &graph,
        &HashSet::from([source]),
        &HashSet::new(),
        true,
        &mut identities,
    )
    .unwrap();
    crate::graph::mutation::maintain::detach_delete_nodes(&mut graph, &HashSet::from([source]));
    let replacement = GraphWrite::add_node(
        &mut graph.graph,
        NodeData::new(
            Value::Int64(3),
            Value::String("replacement".into()),
            "N".into(),
            HashMap::new(),
            &mut graph.interner,
        ),
    );
    let reused = GraphWrite::add_edge(
        &mut graph.graph,
        replacement,
        target,
        EdgeData::new("R".into(), HashMap::new(), &mut graph.interner),
    );
    assert_eq!(reused, edge, "fixture must exercise physical slot reuse");
    let fresh = identities.capture(reused);
    assert!(!identities.accepts(reused, stale));
    assert!(identities.accepts(reused, fresh));
}

#[test]
fn published_result_recursively_matches_untrusted_relationship_values() {
    let token = super::super::relationship_identity::StatementRelationshipIdentities::new()
        .capture(petgraph::graph::EdgeIndex::new(7));
    let expected = RelValue::new(7, 1, 2, "R".into(), PropMap::default());
    let mut trusted = expected.clone();
    trusted.incarnation = Some(token);
    let nested = Value::List(vec![
        Value::Path(Box::new(PathValue {
            nodes: Vec::<NodeValue>::new(),
            rels: vec![trusted.clone()],
        })),
        Value::Map(PropMap::from_pairs(vec![(
            "relationship".into(),
            Value::Relationship(Box::new(trusted.clone())),
        )])),
    ]);
    let mut result = CypherResult {
        columns: vec!["direct".into(), "nested".into()],
        rows: vec![vec![Value::Relationship(Box::new(trusted)), nested]],
        stats: None,
        profile: None,
        diagnostics: None,
        lazy: None,
    };

    crate::graph::languages::cypher::result::clear_published_relationship_incarnations(&mut result);
    assert_eq!(
        result.rows[0][0],
        Value::Relationship(Box::new(expected.clone()))
    );
    assert_eq!(
        result.rows[0][1],
        Value::List(vec![
            Value::Path(Box::new(PathValue {
                nodes: vec![],
                rels: vec![expected.clone()],
            })),
            Value::Map(PropMap::from_pairs(vec![(
                "relationship".into(),
                Value::Relationship(Box::new(expected)),
            )])),
        ])
    );
}

#[test]
fn lazy_public_materialization_scrubs_nested_relationship_tokens() {
    let graph = DirGraph::new();
    let token = super::super::relationship_identity::StatementRelationshipIdentities::new()
        .capture(petgraph::graph::EdgeIndex::new(4));
    let expected = RelValue::new(4, 1, 2, "R".into(), PropMap::default());
    let mut trusted = expected.clone();
    trusted.incarnation = Some(token);
    let mut pending = ResultRow::new();
    pending.projected.insert(
        "nested".into(),
        Value::List(vec![Value::Path(Box::new(PathValue {
            nodes: vec![],
            rels: vec![trusted],
        }))]),
    );
    let descriptor = crate::graph::languages::cypher::result::LazyResultDescriptor::new(
        vec![pending],
        vec![crate::graph::languages::cypher::ast::ReturnItem {
            expression: Expression::Variable("nested".into()),
            alias: None,
        }],
        &graph,
    );

    let row = crate::graph::languages::cypher::result::materialise_lazy_row(&descriptor, &graph, 0)
        .unwrap();
    assert_eq!(
        row,
        vec![Value::List(vec![Value::Path(Box::new(PathValue {
            nodes: vec![],
            rels: vec![expected],
        }))])]
    );
}
