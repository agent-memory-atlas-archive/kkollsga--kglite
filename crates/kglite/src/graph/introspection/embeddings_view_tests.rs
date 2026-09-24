//! `describe()` goldens for relationship embedding stores.

use crate::graph::dir_graph::DirGraph;
use crate::graph::introspection::describe::{compute_description, DescribeRequest};
use crate::graph::introspection::{ConnectionDetail, CypherDetail, DescribeSurface};
use crate::graph::session::execute::{execute_mut, ExecuteOptions};

fn run(graph: &mut DirGraph, query: &str) {
    let params = std::collections::HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("setup query failed: {query}: {e}"));
}

/// Node type and relationship type both named `SUPPORTS`; `node_store` and
/// `edge_store` choose which carries a `body_emb` store (dim 3 on the node
/// side, dim 2 on the relationship side, so the two are distinguishable).
fn graph(node_store: bool, edge_store: bool) -> DirGraph {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (:SUPPORTS {id: 1, title: 'a', body: 'node one'}), \
         (:SUPPORTS {id: 2, title: 'b', body: 'node two'})",
    );
    run(
        &mut graph,
        "MATCH (a:SUPPORTS {id: 1}), (b:SUPPORTS {id: 2}) \
         CREATE (a)-[:SUPPORTS {body: 'edge text'}]->(b), (a)-[:SUPPORTS {body: 'more'}]->(b)",
    );
    if node_store {
        crate::graph::embeddings::set_embeddings(
            &mut graph,
            "SUPPORTS",
            "body",
            None,
            [
                (
                    crate::datatypes::values::Value::Int64(1),
                    vec![1.0, 0.0, 0.0],
                ),
                (
                    crate::datatypes::values::Value::Int64(2),
                    vec![0.0, 1.0, 0.0],
                ),
            ],
        )
        .unwrap();
    }
    if edge_store {
        run(
            &mut graph,
            "MATCH ()-[r:SUPPORTS {body: 'edge text'}]->() \
             CALL db.edge_embeddings.set({type:'SUPPORTS', text_property:'body', \
             entries:[{relationship:r, vector:[0.6, 0.8]}]}) YIELD stored RETURN stored",
        );
    }
    graph
}

fn describe_with(graph: &DirGraph, connections: &ConnectionDetail) -> String {
    let mut request = DescribeRequest::new(DescribeSurface::Python);
    request.connections = connections;
    compute_description(graph, &request).unwrap()
}

fn inventory(graph: &DirGraph) -> String {
    describe_with(graph, &ConnectionDetail::Off)
}

fn line_with<'a>(xml: &'a str, needle: &str) -> &'a str {
    xml.lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line containing {needle:?} in:\n{xml}"))
}

const CONN_LINE: &str = "<conn type=\"SUPPORTS\" count=\"2\" from=\"SUPPORTS\" to=\"SUPPORTS\" \
    properties=\"body:String\" embeddings=\"body(dim=2,count=1)\"/>";

/// The node-only hint, pinned byte-for-byte (`embedding_norm` takes the store
/// name, `'col_emb'` — the raw column spelling was a false claim).
const NODE_SEMANTIC_LINE: &str = "    <semantic hint=\"text_score(n, 'col', 'query'|[0.1,0.2,...], metric) — similarity; a list query is scored as your query vector, a string query is embedded via set_embedder() (metric: 'cosine'|'poincare'|'dot_product'|'euclidean'); embedding_norm(n, 'col_emb') — L2 norm (hierarchy depth in Poincaré space)\"/>";

#[test]
fn the_inventory_map_names_the_relationship_store_on_its_conn_line() {
    let xml = inventory(&graph(true, true));
    assert_eq!(line_with(&xml, "<conn type=\"SUPPORTS\"").trim(), CONN_LINE);
    // The node store keeps its own element and dimension: the same-name pair
    // is never merged.
    assert!(xml.contains("<embeddings text_col=\"body\" dim=\"3\" count=\"2\"/>"));
}

#[test]
fn the_connections_overview_carries_the_same_attribute() {
    let xml = describe_with(&graph(false, true), &ConnectionDetail::Overview);
    assert_eq!(line_with(&xml, "<conn type=\"SUPPORTS\"").trim(), CONN_LINE);
}

#[test]
fn the_connection_detail_view_lists_each_store_as_a_child() {
    let detail = ConnectionDetail::Topics(vec!["SUPPORTS".to_string()]);
    let xml = describe_with(&graph(false, true), &detail);
    assert_eq!(
        line_with(&xml, "<embeddings "),
        "    <embeddings text_col=\"body\" dim=\"2\" count=\"1\"/>"
    );
    let without = describe_with(&graph(true, false), &detail);
    assert!(
        !without.contains("<embeddings "),
        "a node store is not a relationship store:\n{without}"
    );
}

#[test]
fn a_graph_with_only_relationship_stores_gets_the_semantic_hint() {
    let xml = inventory(&graph(false, true));
    let semantic = line_with(&xml, "<semantic ");
    assert!(semantic.contains("vector_score(r, 'col_emb'"), "{semantic}");
    assert!(semantic.contains("db.edge_embeddings.query"), "{semantic}");
    assert!(
        semantic.contains("deleting an embedded relationship or an endpoint drops that index"),
        "the delete contract: {semantic}"
    );
    assert!(
        !semantic.contains("text_score(n,"),
        "no node store, so no node spelling: {semantic}"
    );
}

#[test]
fn a_graph_with_both_entities_names_both_spellings() {
    let xml = inventory(&graph(true, true));
    let semantic = line_with(&xml, "<semantic ");
    assert!(semantic.contains("text_score(n, 'col'"), "{semantic}");
    assert!(semantic.contains("db.edge_embeddings.query"), "{semantic}");
}

#[test]
fn graphs_without_relationship_stores_render_only_the_node_hints() {
    let node_only = inventory(&graph(true, false));
    assert_eq!(line_with(&node_only, "<semantic "), NODE_SEMANTIC_LINE);
    assert!(!node_only.contains("embeddings=\""));

    let neither = inventory(&graph(false, false));
    assert!(!neither.contains("<semantic "));
    assert!(!neither.contains("embeddings=\""));
    let conn = line_with(&neither, "<conn type=\"SUPPORTS\"").trim();
    assert_eq!(
        conn,
        "<conn type=\"SUPPORTS\" count=\"2\" from=\"SUPPORTS\" to=\"SUPPORTS\" properties=\"body:String\"/>"
    );
}

#[test]
fn the_cypher_reference_names_the_relationship_embedding_procedures() {
    let graph = DirGraph::new();
    let mut request = DescribeRequest::new(DescribeSurface::Python);
    request.cypher = &CypherDetail::Overview;
    let overview = compute_description(&graph, &request).unwrap();
    let proc_line = line_with(&overview, "<proc name=\"db.edge_embeddings.*\"");
    for name in [
        "set",
        "embed",
        "list",
        "remove",
        "drop",
        "query",
        "build_index",
        "refresh_index",
        "drop_index",
    ] {
        assert!(
            proc_line.contains(&format!("db.edge_embeddings.{name}(")),
            "{name} missing: {proc_line}"
        );
    }
    assert!(proc_line.contains("text:"), "P5's text option: {proc_line}");
    assert!(
        proc_line.contains("refresh_index refuses when no index is built"),
        "{proc_line}"
    );
    assert!(
        proc_line.contains("DETACH DELETE of either endpoint"),
        "the delete contract: {proc_line}"
    );
    assert!(
        !proc_line.contains("text_bm25"),
        "the lexical lane is not documented before it ships"
    );

    let topics = CypherDetail::Topics(vec!["functions".to_string()]);
    request.cypher = &topics;
    let functions = compute_description(&graph, &request).unwrap();
    let group = line_with(&functions, "<group name=\"relationship_semantic\"");
    assert!(group.contains("vector_score(r, 'col_emb'"), "{group}");
    assert!(group.contains("db.edge_embeddings.query"), "{group}");
}

// ── relationship lexical lane ─────────────────────────────────────────

/// Byte-identical to the lines every node-only graph has always carried.
const NODE_LEXICAL_LINE: &str = "    <lexical hint=\"text_bm25(n, 'prop', 'query text') — BM25 relevance of the node's indexed text; 0.0 = indexed but shares no word with the query, null = no document for that row. Build with build_text_index(node_type, property).\"/>";
const NODE_HYBRID_LINE: &str = "    <hybrid hint=\"score_fuse(text_bm25(n, 'prop', $q), vector_score(n, 'col_emb', $qv)) — one score from both lanes (weights: a trailing list, e.g. [0.7, 0.3]). A lane that cannot see a row scores null and drops out of the average rather than zeroing it; all lanes absent = null. Rank with ORDER BY … DESC LIMIT k.\"/>";

fn with_text_indexes(mut graph: DirGraph, node: bool, edge: bool) -> DirGraph {
    if node {
        crate::graph::text_indexes::build_text_index(&mut graph, "SUPPORTS", "body", None).unwrap();
    }
    if edge {
        run(
            &mut graph,
            "CALL db.edge_text_index.build({type:'SUPPORTS', property:'body'}) \
             YIELD indexed RETURN indexed",
        );
    }
    graph
}

#[test]
fn a_relationship_text_index_alone_gets_the_lexical_hint() {
    let xml = inventory(&with_text_indexes(graph(false, false), false, true));
    let lexical = line_with(&xml, "<lexical ");
    assert!(lexical.contains("text_bm25(r, 'prop'"), "{lexical}");
    assert!(lexical.contains("db.edge_text_index.build"), "{lexical}");
    assert!(!lexical.contains("text_bm25(n,"), "{lexical}");
    assert!(!xml.contains("<hybrid "), "one lane is not hybrid:\n{xml}");
}

#[test]
fn both_relationship_lanes_get_the_hybrid_hint() {
    let xml = inventory(&with_text_indexes(graph(false, true), false, true));
    let hybrid = line_with(&xml, "<hybrid ");
    assert!(hybrid.contains("text_bm25(r, 'prop', $q)"), "{hybrid}");
    assert!(!hybrid.contains("text_bm25(n,"), "{hybrid}");
}

#[test]
fn node_only_retrieval_hints_render_exactly_as_before() {
    let xml = inventory(&with_text_indexes(graph(true, false), true, false));
    assert_eq!(line_with(&xml, "<lexical "), NODE_LEXICAL_LINE);
    assert_eq!(line_with(&xml, "<hybrid "), NODE_HYBRID_LINE);
}

#[test]
fn the_cypher_reference_names_the_relationship_text_index_procedures() {
    let graph = DirGraph::new();
    let mut request = DescribeRequest::new(DescribeSurface::Python);
    request.cypher = &CypherDetail::Overview;
    let overview = compute_description(&graph, &request).unwrap();
    let proc_line = line_with(&overview, "<proc name=\"db.edge_text_index.*\"");
    for name in ["build", "refresh", "drop", "list"] {
        assert!(
            proc_line.contains(&format!("db.edge_text_index.{name}(")),
            "{name} missing: {proc_line}"
        );
    }
    assert!(proc_line.contains("text_bm25(r, 'property'"), "{proc_line}");
    let drop_clause = line_with(&overview, "<clause name=\"DROP INDEX\"");
    assert!(drop_clause.contains("BM25 text index"), "{drop_clause}");

    let topics = CypherDetail::Topics(vec!["functions".to_string()]);
    request.cypher = &topics;
    let functions = compute_description(&graph, &request).unwrap();
    let lexical = line_with(&functions, "<group name=\"lexical\"");
    assert!(lexical.contains("text_bm25(r, 'prop'"), "{lexical}");
    assert!(lexical.contains("db.edge_text_index.build"), "{lexical}");
}
