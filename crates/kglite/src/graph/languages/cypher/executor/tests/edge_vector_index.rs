use super::*;
use crate::graph::edge_embeddings::upsert_edge_embeddings;
use crate::graph::edge_embeddings::vector_index::{
    build_edge_vector_index, EdgeVectorIndexOptions,
};
use petgraph::graph::{EdgeIndex, NodeIndex};

fn indexed_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    for id in 0..3 {
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
    for (target, rank) in [(1, 0), (2, 1)] {
        GraphWrite::add_edge(
            &mut graph.graph,
            NodeIndex::new(0),
            NodeIndex::new(target),
            EdgeData::new(
                "CLAIMS".into(),
                HashMap::from([("rank".into(), Value::Int64(rank))]),
                &mut graph.interner,
            ),
        );
    }
    upsert_edge_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![
            (EdgeIndex::new(0), vec![1.0, 0.0]),
            (EdgeIndex::new(1), vec![0.8, 0.6]),
        ],
        Some("cosine"),
    )
    .unwrap();
    graph
}

fn run(graph: &DirGraph, source: &str) -> CypherResult {
    CypherExecutor::with_params(graph, &HashMap::new(), None)
        .execute(&parser::parse_cypher(source).unwrap())
        .unwrap()
}

#[test]
fn whole_store_query_filters_after_top_k_and_match_remains_exact() {
    let graph = indexed_graph();
    let queried = run(
        &graph,
        "CALL db.edge_embeddings.query({type:'CLAIMS', text_property:'text', \
         vector:[1.0,0.0], top_k:1, exact:true}) YIELD relationship,score \
         WHERE relationship.rank <> 0 RETURN relationship,score",
    );
    assert!(queried.rows.is_empty(), "{queried:?}");

    let matched = run(
        &graph,
        "MATCH ()-[r:CLAIMS]->() WHERE r.rank <> 0 \
         RETURN r.rank,vector_score(r,'text_emb',[1.0,0.0]) AS score \
         ORDER BY score DESC LIMIT 1",
    );
    assert_eq!(matched.rows.len(), 1, "{matched:?}");
    assert_eq!(matched.rows[0][0], Value::Int64(1));
    let Value::Float64(score) = matched.rows[0][1] else {
        panic!("expected numeric score: {matched:?}")
    };
    assert!((score - 0.8).abs() < 1e-6, "{score}");
}

#[test]
fn edge_vector_index_introspection_uses_relationship_entity_kind() {
    let mut graph = indexed_graph();
    build_edge_vector_index(
        &mut graph,
        "CLAIMS",
        "text",
        EdgeVectorIndexOptions::default(),
    )
    .unwrap();

    let rows = run(
        &graph,
        "CALL db.indexes() YIELD name,type,entityType,labelsOrTypes,properties \
         RETURN name,type,entityType,labelsOrTypes,properties",
    )
    .rows;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0][0], Value::String("relationship:CLAIMS.text".into()));
    assert_eq!(rows[0][1], Value::String("VECTOR".into()));
    assert_eq!(rows[0][2], Value::String("RELATIONSHIP".into()));
    assert_eq!(
        rows[0][3],
        Value::List(vec![Value::String("CLAIMS".into())])
    );
    assert_eq!(rows[0][4], Value::List(vec![Value::String("text".into())]));
}

#[test]
fn query_relationship_keeps_statement_identity_for_outer_mutation() {
    let mut graph = indexed_graph();
    let query = parser::parse_cypher(
        "CALL db.edge_embeddings.query({type:'CLAIMS', text_property:'text', \
         vector:[1.0,0.0], top_k:1, exact:true}) YIELD relationship AS r \
         WITH r CALL db.edge_embeddings.remove({type:'CLAIMS', text_property:'text', \
         relationships:[r]}) YIELD removed RETURN removed",
    )
    .unwrap();
    let result = super::super::write::execute_mutable(
        &mut graph,
        &query,
        HashMap::new(),
        crate::graph::algorithms::Interrupt::from_deadline(None),
    )
    .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Int64(1)]]);
    assert_eq!(
        graph
            .edge_embeddings
            .get(&("CLAIMS".into(), "text_emb".into()))
            .unwrap()
            .len(),
        1
    );
}

/// The per-entry map of `db.edge_embeddings.set` is a config map like any
/// other, and an unknown key there is the same silent failure: `vecto:`
/// installed no vector and the call reported `stored`.
#[test]
fn an_unknown_key_in_a_set_entry_is_refused() {
    let mut graph = indexed_graph();
    let before = graph
        .edge_embeddings
        .get(&("CLAIMS".to_string(), "text_emb".to_string()))
        .unwrap()
        .len();
    let query = parser::parse_cypher(
        "MATCH ()-[r:CLAIMS]->() WHERE r.rank = 0 \
         CALL db.edge_embeddings.set({type:'CLAIMS', text_property:'text', \
         entries:[{relationship:r, vecto:[0.0,1.0]}]}) YIELD stored RETURN stored",
    )
    .unwrap();
    let error = super::super::write::execute_mutable(
        &mut graph,
        &query,
        HashMap::new(),
        crate::graph::algorithms::Interrupt::from_deadline(None),
    )
    .expect_err("an unknown entry key must be refused");
    assert!(
        error.contains("unknown parameter 'vecto'") && error.contains("relationship, vector"),
        "{error}"
    );
    assert_eq!(
        graph
            .edge_embeddings
            .get(&("CLAIMS".to_string(), "text_emb".to_string()))
            .unwrap()
            .len(),
        before
    );
}
