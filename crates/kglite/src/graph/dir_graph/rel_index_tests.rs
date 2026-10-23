//! Relationship property indexes: DDL, listing, every edge write path,
//! rollback, persistence, and the edge-anchored match start.
//!
//! The structural oracle [`assert_consistent`] rebuilds each index from the live
//! edges and compares it with the maintained one, so a write path that forgets
//! to maintain the index fails here without a query having to notice the
//! missing row. [`the_anchor_is_what_answers_the_query`] is the non-vacuity
//! check: it empties the index behind the graph's back and demands that the
//! indexed query then finds nothing, which is only true if the index is what
//! seeds the match.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use petgraph::graph::EdgeIndex;

use crate::datatypes::Value;
use crate::graph::dir_graph::rel_index::{edge_slot, slot_edge};
use crate::graph::dir_graph::DirGraph;
use crate::graph::io::file::{load_file_with, prepare_save, write_kgl, LoadOptions};
use crate::graph::schema::InternedKey;
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};
use crate::graph::storage::GraphRead;

fn try_run(graph: &mut DirGraph, query: &str) -> Result<(), String> {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    execute_mut(graph, query, &opts)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn run(graph: &mut DirGraph, query: &str) {
    try_run(graph, query).unwrap_or_else(|e| panic!("{query}: {e}"));
}

fn rows(graph: &DirGraph, query: &str) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    execute_read(graph, query, &opts)
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
        .rows
}

fn count(graph: &DirGraph, query: &str) -> i64 {
    match rows(graph, query)[0][0] {
        Value::Int64(n) => n,
        ref other => panic!("not a count: {other:?}"),
    }
}

fn plan(graph: &DirGraph, query: &str) -> String {
    rows(graph, &format!("EXPLAIN {query}"))
        .iter()
        .map(|row| format!("{:?}", row[1]))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Every maintained index equals the index rebuilt from the live edges.
pub(crate) fn assert_consistent(graph: &DirGraph) {
    assert!(graph.has_rel_property_indices(), "no relationship index");
    for ((rel_type, property), index) in &graph.rel.indices {
        let type_key = InternedKey::from_str(rel_type);
        let property_key = InternedKey::from_str(property);
        let mut expected: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
        for edge in graph.graph.edge_references() {
            if edge.connection_type() != type_key {
                continue;
            }
            if let Some(value) = graph.graph.get_edge_property(edge.id(), property_key) {
                expected
                    .entry(format!("{value:?}"))
                    .or_default()
                    .insert(edge.id().index());
            }
        }
        let mut actual: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
        for (value, members) in index.iter() {
            let slots: BTreeSet<usize> = members.iter().map(|m| m.index()).collect();
            assert_eq!(
                slots.len(),
                members.len(),
                "{rel_type}.{property} holds an edge twice under {value:?}"
            );
            assert!(
                !slots.is_empty(),
                "{rel_type}.{property} keeps an empty bucket"
            );
            actual.insert(format!("{value:?}"), slots);
        }
        assert_eq!(
            actual, expected,
            "{rel_type}.{property} drifted from the edges"
        );
    }
}

fn indexed_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (:P {id: 1}), (:P {id: 2}), (:P {id: 3}), (:M {id: 10}), (:M {id: 11})",
    );
    run(
        &mut graph,
        "MATCH (p:P {id: 1}), (m:M {id: 10}) CREATE (p)-[:T {k: 'a', n: 1}]->(m)",
    );
    run(
        &mut graph,
        "MATCH (p:P {id: 2}), (m:M {id: 10}) CREATE (p)-[:T {k: 'b', n: 2}]->(m)",
    );
    run(
        &mut graph,
        "MATCH (p:P {id: 3}), (m:M {id: 11}) CREATE (p)-[:T {k: 'c'}]->(m), (p)-[:OTHER {k: 'a'}]->(m)",
    );
    run(&mut graph, "CREATE INDEX FOR ()-[r:T]-() ON (r.k)");
    graph
}

// ── DDL and listing ──

#[test]
fn create_index_builds_lists_and_drops() {
    let mut graph = indexed_graph();
    assert_consistent(&graph);
    let listed = rows(
        &graph,
        "SHOW INDEXES YIELD name, entityType, labelsOrTypes, properties WHERE entityType = 'RELATIONSHIP' \
         RETURN name, labelsOrTypes, properties",
    );
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0][0], Value::String("relationship:T.k".to_string()));

    let duplicate = try_run(&mut graph, "CREATE INDEX FOR ()-[r:T]-() ON (r.k)").unwrap_err();
    assert!(duplicate.contains("already exists"), "{duplicate}");
    run(
        &mut graph,
        "CREATE INDEX IF NOT EXISTS FOR ()-[r:T]-() ON (r.k)",
    );
    run(&mut graph, "CREATE RANGE INDEX FOR ()-[r:T]-() ON (r.n)");
    assert_eq!(graph.rel.indices.len(), 2);

    run(&mut graph, "DROP INDEX FOR ()-[r:T]-() ON (r.n)");
    run(&mut graph, "DROP INDEX `relationship:T.k`");
    assert!(!graph.has_rel_property_indices());
    let missing = try_run(&mut graph, "DROP INDEX FOR ()-[r:T]-() ON (r.k)").unwrap_err();
    assert!(missing.contains("no index"), "{missing}");
}

#[test]
fn a_composite_relationship_index_is_refused() {
    let mut graph = indexed_graph();
    let error = try_run(&mut graph, "CREATE INDEX FOR ()-[r:T]-() ON (r.k, r.n)").unwrap_err();
    assert!(error.contains("exactly one property"), "{error}");
}

// ── The anchor ──

#[test]
fn explain_names_the_seek_only_when_an_index_covers_the_pattern() {
    let graph = indexed_graph();
    let indexed = plan(&graph, "MATCH ()-[r:T {k: 'a'}]->() RETURN r");
    assert!(indexed.contains("RelIndexSeek :T(k)"), "{indexed}");
    let unindexed_property = plan(&graph, "MATCH ()-[r:T {n: 1}]->() RETURN r");
    assert!(
        !unindexed_property.contains("RelIndexSeek"),
        "{unindexed_property}"
    );
    let other_type = plan(&graph, "MATCH ()-[r:OTHER {k: 'a'}]->() RETURN r");
    assert!(!other_type.contains("RelIndexSeek"), "{other_type}");
    let where_form = plan(&graph, "MATCH ()-[r:T]->() WHERE r.k = 'a' RETURN r");
    assert!(where_form.contains("RelIndexSeek :T(k)"), "{where_form}");
    assert!(
        where_form.contains("anchor_rel_property_index"),
        "{where_form}"
    );
}

#[test]
fn the_anchor_is_what_answers_the_query() {
    let mut graph = indexed_graph();
    let query = "MATCH (p)-[r:T {k: 'a'}]->(m) RETURN count(r)";
    assert_eq!(count(&graph, query), 1);
    // Empty the index behind the graph's back. If the match still found the
    // edge it would be scanning, and the anchor would be decoration.
    for index in graph.rel.indices.values_mut() {
        index.clear();
    }
    assert_eq!(count(&graph, query), 0, "the query did not read the index");
    // The unanchored forms are unaffected.
    assert_eq!(
        count(&graph, "MATCH ()-[r:T {n: 1}]->() RETURN count(r)"),
        1
    );
}

#[test]
fn indexed_and_unindexed_answers_agree_for_every_probe_shape() {
    let mut graph = indexed_graph();
    run(
        &mut graph,
        "MATCH (p:P {id: 1}), (m:M {id: 11}) CREATE (p)-[:T {k: 1}]->(m), (p)-[:T {k: 1.0}]->(m), (p)-[:T {k: '1'}]->(m)",
    );
    let probes = [
        "MATCH ()-[r:T {k: 'a'}]->() RETURN count(r)",
        "MATCH ()-[r:T {k: 'zzz'}]->() RETURN count(r)",
        "MATCH ()-[r:T]->() WHERE r.k = 'b' RETURN count(r)",
        "MATCH ()-[r:T]->() WHERE r.k IN ['a', 'b', 'nope'] RETURN count(r)",
        "MATCH ()-[r:T {k: 1}]->() RETURN count(r)",
        "MATCH ()-[r:T {k: 1.0}]->() RETURN count(r)",
        "MATCH (p:P)<-[r:T {k: 'a'}]-(m) RETURN count(r)",
        "MATCH (p:P)-[r:T {k: 'a'}]->(m:M {id: 10}) RETURN count(r)",
        "MATCH (p:P)-[r:T {k: 'a'}]->(m:M {id: 11}) RETURN count(r)",
        "MATCH (a)-[r:T {k: 'a'}]-(b) RETURN count(r)",
        "MATCH ()-[r:T {k: 'a'}]->() RETURN r.n AS n",
    ];
    let before: Vec<_> = probes.iter().map(|q| rows(&graph, q)).collect();
    run(&mut graph, "DROP INDEX FOR ()-[r:T]-() ON (r.k)");
    let scanned: Vec<_> = probes.iter().map(|q| rows(&graph, q)).collect();
    assert_eq!(before, scanned);
}

#[test]
fn a_correlated_where_seeds_from_the_index() {
    let mut graph = indexed_graph();
    let query = "MATCH (r:P)-[x:T]->(:M) WITH x CALL (x) { MATCH ()-[c:T]->() WHERE c.k = x.k \
                 WITH c LIMIT 2 RETURN count(*) AS n } RETURN x.k AS k, n ORDER BY k";
    let indexed = rows(&graph, query);
    assert_eq!(indexed.len(), 3);
    assert!(
        indexed.iter().all(|row| row[1] == Value::Int64(1)),
        "{indexed:?}"
    );
    run(&mut graph, "DROP INDEX FOR ()-[r:T]-() ON (r.k)");
    assert_eq!(rows(&graph, query), indexed);
}

// ── Maintenance on every write path ──

#[test]
fn cypher_writes_keep_the_index_exact() {
    let mut graph = indexed_graph();
    // CREATE
    run(
        &mut graph,
        "MATCH (p:P {id: 1}), (m:M {id: 11}) CREATE (p)-[:T {k: 'a'}]->(m)",
    );
    assert_consistent(&graph);
    assert_eq!(
        count(&graph, "MATCH ()-[r:T {k: 'a'}]->() RETURN count(r)"),
        2
    );
    // MERGE creates, then hits
    run(
        &mut graph,
        "MATCH (p:P {id: 2}), (m:M {id: 11}) MERGE (p)-[r:T {k: 'merged'}]->(m)",
    );
    run(
        &mut graph,
        "MATCH (p:P {id: 2}), (m:M {id: 11}) MERGE (p)-[r:T {k: 'merged'}]->(m)",
    );
    assert_eq!(
        count(&graph, "MATCH ()-[r:T {k: 'merged'}]->() RETURN count(r)"),
        1
    );
    assert_consistent(&graph);
    // SET, SET +=, SET = , REMOVE
    run(&mut graph, "MATCH ()-[r:T {k: 'b'}]->() SET r.k = 'b2'");
    assert_consistent(&graph);
    assert_eq!(
        count(&graph, "MATCH ()-[r:T {k: 'b'}]->() RETURN count(r)"),
        0
    );
    assert_eq!(
        count(&graph, "MATCH ()-[r:T {k: 'b2'}]->() RETURN count(r)"),
        1
    );
    run(
        &mut graph,
        "MATCH ()-[r:T {k: 'b2'}]->() SET r += {k: 'b3', z: 1}",
    );
    assert_consistent(&graph);
    run(&mut graph, "MATCH ()-[r:T {k: 'b3'}]->() SET r = {k: 'b4'}");
    assert_consistent(&graph);
    assert_eq!(
        count(&graph, "MATCH ()-[r:T {k: 'b4'}]->() RETURN count(r)"),
        1
    );
    run(&mut graph, "MATCH ()-[r:T {k: 'b4'}]->() REMOVE r.k");
    assert_consistent(&graph);
    assert_eq!(
        count(&graph, "MATCH ()-[r:T {k: 'b4'}]->() RETURN count(r)"),
        0
    );
    run(
        &mut graph,
        "MATCH ()-[r:T]->() WHERE r.k IS NULL SET r.k = 'back'",
    );
    assert_consistent(&graph);
    // A null SET leaves the property absent, and the index without the edge.
    run(&mut graph, "MATCH ()-[r:T {k: 'back'}]->() SET r.k = null");
    assert_consistent(&graph);
    // DELETE r
    run(&mut graph, "MATCH ()-[r:T {k: 'c'}]->() DELETE r");
    assert_consistent(&graph);
    // DETACH DELETE a node takes its edges out of the index
    run(&mut graph, "MATCH (p:P {id: 1}) DETACH DELETE p");
    assert_consistent(&graph);
    // Both 'a' edges left from p1.
    assert_eq!(
        count(&graph, "MATCH ()-[r:T {k: 'a'}]->() RETURN count(r)"),
        0
    );
    // A freed edge slot is reused by a new edge with another value.
    run(
        &mut graph,
        "MATCH (p:P {id: 3}), (m:M {id: 10}) CREATE (p)-[:T {k: 'reused'}]->(m)",
    );
    assert_consistent(&graph);
}

#[test]
fn a_failed_statement_restores_the_index() {
    let mut graph = indexed_graph();
    let snapshot = |graph: &DirGraph| -> BTreeMap<String, Vec<usize>> {
        graph
            .rel
            .indices
            .get(&("T".to_string(), "k".to_string()))
            .unwrap()
            .iter()
            .map(|(v, m)| (format!("{v:?}"), m.iter().map(|n| n.index()).collect()))
            .collect()
    };
    let before = snapshot(&graph);
    // A create, a move, a delete, then a failure.
    assert!(try_run(
        &mut graph,
        "MATCH (p:P {id: 3}), (m:M {id: 10}) CREATE (p)-[:T {k: 'new'}]->(m) \
         WITH 1 AS x MATCH ()-[r:T {k: 'a'}]->() SET r.k = 'moved' \
         WITH 1 AS y MATCH ()-[q:T {k: 'b'}]->() DELETE q \
         WITH 1 AS z UNWIND [1, 0] AS d RETURN 1 / d"
    )
    .is_err());
    assert_eq!(snapshot(&graph), before);
    assert_consistent(&graph);
    assert_eq!(
        count(&graph, "MATCH ()-[r:T {k: 'a'}]->() RETURN count(r)"),
        1
    );
}

#[test]
fn vacuum_renumbers_and_the_index_follows() {
    let mut graph = indexed_graph();
    run(&mut graph, "MATCH ()-[r:T {k: 'a'}]->() DELETE r");
    run(&mut graph, "MATCH (p:P {id: 1}) DETACH DELETE p");
    graph.vacuum();
    assert_consistent(&graph);
    assert_eq!(
        count(&graph, "MATCH ()-[r:T {k: 'b'}]->() RETURN count(r)"),
        1
    );
}

#[test]
fn the_bulk_edge_path_files_creates_updates_and_replacements() {
    use crate::graph::mutation::edge_specs::{add_edges_from_specs, EdgeSpec};
    let mut graph = indexed_graph();
    let spec = |source: i64, target: i64, k: &str| EdgeSpec {
        source_type: "P".to_string(),
        source_id: Value::Int64(source),
        target_type: "M".to_string(),
        target_id: Value::Int64(target),
        edge_type: "T".to_string(),
        properties: HashMap::from([("k".to_string(), Value::String(k.to_string()))]),
    };
    // One new edge, and one that merges into the (p1, m10) edge and moves it.
    add_edges_from_specs(
        &mut graph,
        vec![spec(2, 11, "bulk"), spec(1, 10, "a-updated")],
    )
    .unwrap();
    assert_consistent(&graph);
    assert_eq!(
        count(&graph, "MATCH ()-[r:T {k: 'a'}]->() RETURN count(r)"),
        0
    );
    assert_eq!(
        count(
            &graph,
            "MATCH ()-[r:T {k: 'a-updated'}]->() RETURN count(r)"
        ),
        1
    );
    assert_eq!(
        count(&graph, "MATCH ()-[r:T {k: 'bulk'}]->() RETURN count(r)"),
        1
    );
}

// ── Persistence ──

fn saved_and_loaded(graph: DirGraph, deferred: bool) -> Arc<DirGraph> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rel.kgl");
    let path = path.to_str().unwrap();
    let mut arc = Arc::new(graph);
    prepare_save(&mut arc);
    write_kgl(&arc, path).unwrap();
    let loaded =
        load_file_with(path, &LoadOptions::new().with_defer_index_rebuild(deferred)).unwrap();
    drop(dir);
    loaded
}

#[test]
fn a_save_carries_the_declaration_and_a_load_rebuilds_the_index() {
    let loaded = saved_and_loaded(indexed_graph(), false);
    assert_eq!(loaded.rel.index_keys, [("T".to_string(), "k".to_string())]);
    assert_consistent(&loaded);
    assert_eq!(
        count(&loaded, "MATCH ()-[r:T {k: 'a'}]->() RETURN count(r)"),
        1
    );
}

#[test]
fn a_deferred_load_lists_the_index_and_the_first_write_builds_it() {
    let loaded = saved_and_loaded(indexed_graph(), true);
    let listed = rows(
        &loaded,
        "SHOW INDEXES YIELD name, state WHERE name = 'relationship:T.k' RETURN state",
    );
    assert_eq!(listed, vec![vec![Value::String("DEFERRED".to_string())]]);
    // Unbuilt, a probe scans and still answers.
    assert_eq!(
        count(&loaded, "MATCH ()-[r:T {k: 'a'}]->() RETURN count(r)"),
        1
    );
    let mut graph = Arc::try_unwrap(loaded).ok().expect("sole owner");
    run(
        &mut graph,
        "MATCH (p:P {id: 3}), (m:M {id: 10}) CREATE (p)-[:T {k: 'a'}]->(m)",
    );
    assert_consistent(&graph);
}

#[test]
fn a_graph_without_a_relationship_index_writes_the_same_metadata_as_before() {
    let mut graph = DirGraph::new();
    run(&mut graph, "CREATE (:P {id: 1})-[:T {k: 1}]->(:M {id: 2})");
    graph.populate_index_keys();
    assert!(graph.rel.index_keys.is_empty());
    assert!(!graph.has_rel_property_indices());
}

// ── Slots ──

#[test]
fn slot_conversion_is_an_identity() {
    for n in [0usize, 1, 7, 4096] {
        assert_eq!(slot_edge(edge_slot(EdgeIndex::new(n))).index(), n);
    }
}
