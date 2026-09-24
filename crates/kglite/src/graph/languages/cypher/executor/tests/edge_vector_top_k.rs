//! `vector_score(r, …) ORDER BY … DESC LIMIT k` over relationships: the fused
//! relationship route must return exactly what the unfused pipeline returns —
//! rows, order, endpoint projection — on every shape, with and without an
//! index, and including ties, NULL scores and filtered populations.
use super::*;
use crate::graph::languages::cypher::result::{CypherResult, RetrievalDiagnostics};
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};
use std::collections::HashSet;

const PASS: &str = "fuse_vector_score_order_limit";

fn run(graph: &mut DirGraph, query: &str) {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"));
}

fn read(graph: &DirGraph, query: &str, disabled: bool) -> CypherResult {
    let params = HashMap::new();
    let passes: HashSet<String> = HashSet::from([PASS.to_string()]);
    let mut opts = ExecuteOptions::eager(&params);
    if disabled {
        opts.disabled_passes = Some(&passes);
    }
    execute_read(graph, query, &opts)
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
}

/// A hub with six `C` relationships to six docs (inserted in an order unlike
/// the vector order), plus one `C` between two docs and one `T` relationship.
/// k=1 and k=4 carry the *same* vector, a tie.
fn corpus(embed_all: bool) -> DirGraph {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (h:Hub {id: 0}), (a:Doc {id: 1}), (b:Doc {id: 2}), (c:Doc {id: 3}), \
         (d:Doc {id: 4}), (e:Doc {id: 5}), (f:Doc {id: 6}), \
         (h)-[:C {k: 3}]->(c), (h)-[:C {k: 1}]->(a), (h)-[:C {k: 5}]->(e), \
         (h)-[:C {k: 2}]->(b), (h)-[:C {k: 6}]->(f), (h)-[:C {k: 4}]->(d), \
         (a)-[:C {k: 7}]->(b), (a)-[:T {k: 8}]->(c)",
    );
    let vectors = [
        (1, "[1.0, 0.2]"),
        (2, "[0.1, 1.0]"),
        (3, "[0.9, 0.5]"),
        (4, "[1.0, 0.2]"),
        (5, "[-1.0, 0.1]"),
        (6, "[0.6, 0.6]"),
        (7, "[0.3, 0.9]"),
    ];
    for (k, vector) in vectors {
        if !embed_all && k == 6 {
            continue;
        }
        run(
            &mut graph,
            &format!(
                "MATCH ()-[r:C {{k: {k}}}]->() CALL db.edge_embeddings.set({{type:'C', \
                 text_property:'text', entries:[{{relationship:r, vector:{vector}}}]}}) \
                 YIELD stored RETURN stored"
            ),
        );
    }
    graph
}

fn assert_same(graph: &DirGraph, query: &str) -> CypherResult {
    let fused = read(graph, query, false);
    let unfused = read(graph, query, true);
    assert_eq!(fused.columns, unfused.columns, "{query}");
    assert_eq!(fused.rows, unfused.rows, "{query}");
    fused
}

fn retrieval(result: &CypherResult) -> Vec<RetrievalDiagnostics> {
    result
        .diagnostics
        .as_ref()
        .map(|d| d.retrieval.clone())
        .unwrap_or_default()
}

const SCAN: &str = "MATCH (h:Hub)-[r:C]->(d:Doc) \
                    RETURN h.id AS h, d.id AS d, r.k AS k, vector_score(r, 'text_emb', [1.0, 0.0]) AS s \
                    ORDER BY s DESC LIMIT ";

#[test]
fn plain_scan_is_served_from_the_store_and_equals_the_unfused_answer() {
    // Hub-only pattern: seven C relationships exist, one is doc-to-doc, so
    // the labelled pattern is not the store — the entry must decline.
    let graph = corpus(true);
    assert_same(&graph, &format!("{SCAN}2"));
    // The unlabelled whole-type pattern *is* the store: served by the entry.
    let query = "MATCH ()-[r:C]->() RETURN r.k AS k, vector_score(r, 'text_emb', [0.0, 1.0]) AS s \
                 ORDER BY s DESC LIMIT 3";
    let fused = assert_same(&graph, query);
    assert_eq!(
        fused
            .rows
            .iter()
            .map(|row| row[0].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int64(2), Value::Int64(7), Value::Int64(6)]
    );
    let records = retrieval(&fused);
    assert!(
        records.iter().any(
            |record| record.store.as_deref() == Some("relationship:C.text_emb")
                && record.actual_mode == "exact"
                && record.fallback_reason.as_deref() == Some("no_index")
        ),
        "{records:?}"
    );
}

#[test]
fn endpoints_resolve_for_the_winners() {
    let graph = corpus(true);
    let fused = assert_same(
        &graph,
        "MATCH (a)-[r:C]->(b) RETURN a.id AS a, b.id AS b, r.k AS k, \
         vector_score(r, 'text_emb', [0.0, 1.0]) AS s ORDER BY s DESC LIMIT 3",
    );
    assert_eq!(
        fused.rows[0][..3],
        [Value::Int64(0), Value::Int64(2), Value::Int64(2)]
    );
    assert_eq!(
        fused.rows[1][..3],
        [Value::Int64(1), Value::Int64(2), Value::Int64(7)]
    );
    assert_same(
        &graph,
        "MATCH (b)<-[r:C]-(a) RETURN a.id AS a, b.id AS b, \
         vector_score(r, 'text_emb', [0.0, 1.0]) AS s ORDER BY s DESC LIMIT 3",
    );
}

#[test]
fn ties_nulls_where_and_asc_keep_the_unfused_answer() {
    let graph = corpus(true);
    // k=1 and k=4 tie for first: the entry declines, order is the matcher's.
    assert_same(
        &graph,
        "MATCH ()-[r:C]->() RETURN r.k AS k, vector_score(r, 'text_emb', [1.0, 0.0]) AS s \
         ORDER BY s DESC LIMIT 1",
    );
    assert_same(
        &graph,
        "MATCH ()-[r:C]->() RETURN r.k AS k, vector_score(r, 'text_emb', [1.0, 0.0]) AS s \
         ORDER BY s DESC LIMIT 5",
    );
    assert_same(
        &graph,
        "MATCH (a)-[r:C]->(b) WHERE b.id > 2 RETURN r.k AS k, \
         vector_score(r, 'text_emb', [0.0, 1.0]) AS s ORDER BY s DESC LIMIT 2",
    );
    assert_same(
        &graph,
        "MATCH ()-[r:C]->() RETURN r.k AS k, vector_score(r, 'text_emb', [0.0, 1.0]) AS s \
         ORDER BY s ASC LIMIT 2",
    );
    // One relationship of the type is unembedded: it scores NULL and ranks
    // first under DESC, which the store alone cannot know.
    let sparse = corpus(false);
    let fused = assert_same(
        &sparse,
        "MATCH ()-[r:C]->() RETURN r.k AS k, vector_score(r, 'text_emb', [0.0, 1.0]) AS s \
         ORDER BY s DESC LIMIT 2",
    );
    assert_eq!(fused.rows[0], vec![Value::Int64(6), Value::Null]);
}

#[test]
fn an_indexed_store_is_served_through_hnsw() {
    let mut graph = corpus(true);
    run(
        &mut graph,
        "CALL db.edge_embeddings.build_index({type:'C', text_property:'text'}) YIELD indexed RETURN indexed",
    );
    let fused = assert_same(
        &graph,
        "MATCH ()-[r:C]->() RETURN r.k AS k, vector_score(r, 'text_emb', [0.0, 1.0]) AS s \
         ORDER BY s DESC LIMIT 3",
    );
    assert!(
        retrieval(&fused)
            .iter()
            .any(|record| record.actual_mode == "hnsw"
                && record.store.as_deref() == Some("relationship:C.text_emb")),
        "{:?}",
        retrieval(&fused)
    );
    // The rows route (a WHERE) with an index: HNSW over-fetch, filtered.
    let filtered = assert_same(
        &graph,
        "MATCH (a)-[r:C]->(b) WHERE a.id = 0 RETURN r.k AS k, \
         vector_score(r, 'text_emb', [0.0, 1.0]) AS s ORDER BY s DESC LIMIT 2",
    );
    assert!(
        retrieval(&filtered)
            .iter()
            .any(|record| record.store.as_deref() == Some("relationship:C.text_emb")),
        "{:?}",
        retrieval(&filtered)
    );
    // Forced exact bypasses the index.
    let forced = assert_same(
        &graph,
        "MATCH ()-[r:C]->() RETURN r.k AS k, \
         vector_score(r, 'text_emb', [0.0, 1.0], {exact: true}) AS s ORDER BY s DESC LIMIT 3",
    );
    assert!(retrieval(&forced)
        .iter()
        .any(|record| record.fallback_reason.as_deref() == Some("forced_exact")));
}

#[test]
fn argument_errors_keep_the_scalar_message() {
    let graph = corpus(true);
    let params = HashMap::new();
    let Err(error) = execute_read(
        &graph,
        "MATCH ()-[r:C]->() RETURN vector_score(r, 'text_emb', [0.0, 1.0, 0.0]) AS s \
         ORDER BY s DESC LIMIT 2",
        &ExecuteOptions::eager(&params),
    ) else {
        panic!("a wrong-dimension query vector must fail");
    };
    let error = error.to_string();
    assert!(
        error.contains("vector_score(): query vector dimension"),
        "{error}"
    );
}

/// Which shapes the store entry itself serves — the equality tests above hold
/// whichever route answers, so the route is pinned here directly.
#[test]
fn the_store_entry_serves_exactly_the_store_shaped_scans() {
    let graph = corpus(true);
    let sparse = corpus(false);
    let entry = |graph: &DirGraph, source: &str| {
        let params = HashMap::new();
        let mut query = parser::parse_cypher(source).unwrap();
        crate::graph::languages::cypher::planner::optimize(&mut query, graph, &params);
        CypherExecutor::with_params(graph, &params, None)
            .try_retrieval_entry(&query.clauses)
            .unwrap()
            .map(|result| result.rows.len())
    };
    let q = |pattern: &str, vector: &str, limit: usize| {
        format!(
            "MATCH {pattern} RETURN r.k AS k, vector_score(r, 'text_emb', {vector}) AS s \
             ORDER BY s DESC LIMIT {limit}"
        )
    };
    assert_eq!(entry(&graph, &q("()-[r:C]->()", "[0.0, 1.0]", 3)), Some(3));
    assert_eq!(
        entry(&graph, &q("(a)<-[r:C]-(b)", "[0.0, 1.0]", 3)),
        Some(3)
    );
    // Tie at the boundary, an unembedded relationship, a label that excludes
    // one relationship of the type, a WHERE: all left to the pipeline.
    assert_eq!(entry(&graph, &q("()-[r:C]->()", "[1.0, 0.0]", 1)), None);
    assert_eq!(entry(&sparse, &q("()-[r:C]->()", "[0.0, 1.0]", 3)), None);
    assert_eq!(entry(&graph, &q("(:Hub)-[r:C]->()", "[0.0, 1.0]", 3)), None);
    assert_eq!(
        entry(
            &graph,
            "MATCH (a)-[r:C]->() WHERE a.id = 0 RETURN vector_score(r, 'text_emb', [0.0, 1.0]) AS s \
             ORDER BY s DESC LIMIT 3"
        ),
        None
    );
}
