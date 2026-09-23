use super::*;
use crate::graph::edge_embeddings::upsert_edge_embeddings;
use petgraph::graph::EdgeIndex;

fn graph() -> DirGraph {
    let mut graph = DirGraph::new();
    for id in 1..=3 {
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
    let edge = |graph: &mut DirGraph, source, target, kind: &str| {
        GraphWrite::add_edge(
            &mut graph.graph,
            source,
            target,
            EdgeData::new(kind.into(), HashMap::new(), &mut graph.interner),
        )
    };
    let n0 = petgraph::graph::NodeIndex::new(0);
    let n1 = petgraph::graph::NodeIndex::new(1);
    let n2 = petgraph::graph::NodeIndex::new(2);
    edge(&mut graph, n0, n1, "ASSERTS");
    edge(&mut graph, n0, n1, "ASSERTS");
    edge(&mut graph, n1, n2, "ASSERTS");
    edge(&mut graph, n0, n2, "MENTIONS");
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "text",
        vec![
            (EdgeIndex::new(0), vec![1.0, 0.0]),
            (EdgeIndex::new(1), vec![1.0, 0.0]),
        ],
        Some("cosine"),
    )
    .unwrap();
    upsert_edge_embeddings(
        &mut graph,
        "MENTIONS",
        "text",
        vec![(EdgeIndex::new(3), vec![1.0, 0.0, 0.0])],
        Some("dot_product"),
    )
    .unwrap();
    graph
}

fn run(graph: &DirGraph, query: &str) -> Result<CypherResult, String> {
    let params = HashMap::new();
    CypherExecutor::with_params(graph, &params, None).execute(&parser::parse_cypher(query).unwrap())
}

#[test]
fn exact_relationship_scoring_preserves_endpoint_filter_parallel_rows_and_ties() {
    let graph = graph();
    let result = run(
        &graph,
        "MATCH (a:N)-[r:ASSERTS]->(b:N) WHERE a.id = 1 AND b.id = 2 \
         RETURN id(r) AS edge, vector_score(r,'text_emb',[1.0,0.0]) AS score \
         ORDER BY score DESC, edge ASC LIMIT 10",
    )
    .unwrap();
    assert_eq!(
        result.rows,
        vec![
            vec![Value::Int64(0), Value::Float64(1.0)],
            vec![Value::Int64(1), Value::Float64(1.0)],
        ]
    );
}

#[test]
fn relationship_type_selects_its_own_dimension_metric_and_missing_vectors_are_null() {
    let graph = graph();
    let rows = run(
        &graph,
        "MATCH ()-[r]->() RETURN type(r) AS kind, id(r) AS edge, \
         CASE WHEN type(r)='ASSERTS' THEN vector_score(r,'text_emb',[1.0,0.0]) \
         ELSE vector_score(r,'text_emb',[2.0,0.0,0.0]) END AS score \
         ORDER BY edge",
    )
    .unwrap()
    .rows;
    assert_eq!(rows[0][2], Value::Float64(1.0));
    assert_eq!(rows[1][2], Value::Float64(1.0));
    assert_eq!(rows[2][2], Value::Null);
    assert_eq!(rows[3][2], Value::Float64(2.0));

    let error = run(
        &graph,
        "MATCH ()-[r:MENTIONS]->() RETURN vector_score(r,'text_emb',[1.0,0.0])",
    )
    .unwrap_err();
    assert!(
        error.contains("dimension 2") && error.contains("dimension 3"),
        "{error}"
    );
}

#[test]
fn relationship_scoring_rejects_maps_and_paths_as_scalar_entities() {
    let graph = graph();
    for query in [
        "RETURN vector_score({id:0},'text_emb',[1.0,0.0])",
        "MATCH p=()-[:ASSERTS]->() RETURN vector_score(p,'text_emb',[1.0,0.0])",
    ] {
        let error = run(&graph, query).unwrap_err();
        assert!(error.contains("node or relationship variable"), "{error}");
    }
}

#[test]
fn collected_and_unwound_relationships_score_like_direct_bindings() {
    let graph = graph();
    let result = run(
        &graph,
        "MATCH ()-[r:ASSERTS]->() WITH collect(r) AS relationships \
         UNWIND relationships AS rel \
         RETURN id(rel) AS edge, vector_score(rel,'text_emb',[1.0,0.0]) AS score, \
         embedding_norm(rel,'text_emb') AS norm ORDER BY edge",
    )
    .unwrap();
    assert_eq!(
        result.rows[0],
        vec![Value::Int64(0), Value::Float64(1.0), Value::Float64(1.0)]
    );
    assert_eq!(
        result.rows[1],
        vec![Value::Int64(1), Value::Float64(1.0), Value::Float64(1.0)]
    );
    assert_eq!(
        result.rows[2],
        vec![Value::Int64(2), Value::Null, Value::Null]
    );
}
