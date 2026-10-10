//! Text-index build and drop reach the write-ahead log: a durable session that
//! crashes before its next checkpoint recovers the index it built (or the
//! absence of the one it dropped), for nodes and relationships alike.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use super::{execute_mut, execute_read, ExecuteOptions, Session};
use crate::datatypes::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::text_indexes::edge_text::{build_edge_text_index, drop_edge_text_index};
use crate::graph::text_indexes::{
    build_text_index, drop_text_index, has_text_index, text_index_store,
};
use crate::graph::wal::DurabilityLevel;

fn run(graph: &mut DirGraph, query: &str) -> Result<(), String> {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    execute_mut(graph, query, &opts)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn open(path: &Path) -> Session {
    let p = path.to_string_lossy().into_owned();
    let graph = if path.exists() {
        crate::graph::io::file::load_file(&p).unwrap()
    } else {
        Arc::new(DirGraph::new())
    };
    Session::attach_log(graph, &p, DurabilityLevel::Full).unwrap()
}

fn seed(session: &Session) {
    session
        .write_logged(|g| {
            run(
                g,
                "CREATE (a:Doc {id: 1, body: 'the quick brown fox'}), \
                        (b:Doc {id: 2, body: 'a lazy dog sleeps'}), \
                        (a)-[:CLAIMS {k: 1, text: 'quick fox claim'}]->(b), \
                        (b)-[:CLAIMS {k: 2, text: 'lazy dog claim'}]->(a)",
            )
        })
        .unwrap();
}

fn node_hits(session: &Session) -> Vec<i64> {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let out = execute_read(
        &session.snapshot(),
        "MATCH (n:Doc) WHERE text_bm25(n, 'body', 'fox') > 0 RETURN n.id AS id ORDER BY id",
        &opts,
    )
    .unwrap();
    out.result
        .rows
        .iter()
        .map(|row| match &row[0] {
            Value::Int64(n) => *n,
            other => panic!("id was {other:?}"),
        })
        .collect()
}

fn edge_hits(session: &Session) -> Vec<i64> {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let out = execute_read(
        &session.snapshot(),
        "MATCH ()-[r:CLAIMS]->() WHERE text_bm25(r, 'text', 'fox') > 0 RETURN r.k AS k ORDER BY k",
        &opts,
    )
    .unwrap();
    out.result
        .rows
        .iter()
        .map(|row| match &row[0] {
            Value::Int64(n) => *n,
            other => panic!("k was {other:?}"),
        })
        .collect()
}

#[test]
fn text_indexes_built_after_the_last_checkpoint_survive_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = open(&path);
    seed(&session);
    session
        .write_logged(|g| build_text_index(g, "Doc", "body", Some(7)).map(|_| ()))
        .unwrap();
    session
        .write_logged(|g| build_edge_text_index(g, "CLAIMS", "text", None).map(|_| ()))
        .unwrap();
    assert_eq!(node_hits(&session), vec![1]);
    assert_eq!(edge_hits(&session), vec![1]);
    drop(session); // crash-shaped: no checkpoint

    let recovered = open(&path);
    let snapshot = recovered.snapshot();
    assert!(has_text_index(&snapshot, "Doc", "body"));
    assert_eq!(
        text_index_store(&snapshot, "Doc", "body")
            .unwrap()
            .auto_refresh_limit(),
        7,
        "the declared refresh limit travels with the build"
    );
    assert_eq!(node_hits(&recovered), vec![1]);
    assert_eq!(edge_hits(&recovered), vec![1]);
}

#[test]
fn a_drop_after_a_checkpoint_survives_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = open(&path);
    seed(&session);
    session
        .write_logged(|g| build_text_index(g, "Doc", "body", None).map(|_| ()))
        .unwrap();
    session
        .write_logged(|g| build_edge_text_index(g, "CLAIMS", "text", None).map(|_| ()))
        .unwrap();
    session.save(&path.to_string_lossy(), false).unwrap();
    session
        .write_logged(|g| {
            assert!(drop_text_index(g, "Doc", "body"));
            assert!(drop_edge_text_index(g, "CLAIMS", "text"));
            Ok::<_, String>(())
        })
        .unwrap();
    drop(session);

    let recovered = open(&path);
    assert!(!has_text_index(&recovered.snapshot(), "Doc", "body"));
    assert!(
        !recovered
            .snapshot()
            .edge_text_indexes
            .contains_key(&("CLAIMS".to_string(), "text".to_string())),
        "the dropped relationship index must not come back from the checkpoint"
    );
}

#[test]
fn a_build_then_drop_in_one_log_replays_to_absence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = open(&path);
    seed(&session);
    session
        .write_logged(|g| build_text_index(g, "Doc", "body", None).map(|_| ()))
        .unwrap();
    session
        .write_logged(|g| {
            drop_text_index(g, "Doc", "body");
            Ok::<_, String>(())
        })
        .unwrap();
    drop(session);
    assert!(!has_text_index(&open(&path).snapshot(), "Doc", "body"));
}
