//! The compatibility identity's components rows and the GraphBridge search
//! statements it unlocks, through the backend's Bolt entry point.

use std::collections::HashMap;

use boltr::server::{BoltBackend, SessionHandle};
use boltr::types::{BoltDict, BoltValue};
use kglite::api::session::CsvImportPolicy;
use kglite::api::storage::{new_dir_graph_in_mode, StorageMode};

use super::*;

/// components_rows follows the identity, and under compatibility carries
/// exactly the rows GraphBridge's preflight parses: one 2026.x kernel and
/// one Cypher row advertising "25".
#[test]
fn components_rows_follow_identity() {
    let rows = ServerIdentity::Kglite.components_rows("9.9.9");
    assert_eq!(
        rows,
        vec![("kglite-bolt-server".to_string(), vec!["9.9.9".to_string()])]
    );
    let rows = ServerIdentity::Neo4jCompatible.components_rows("9.9.9");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "Neo4j Kernel");
    assert_eq!(rows[0].1, vec![NEO4J_COMPAT_VERSION.to_string()]);
    assert!(NEO4J_COMPAT_VERSION.starts_with("2026."));
    assert_eq!(rows[1].0, "Cypher");
    assert!(rows[1].1.contains(&"25".to_string()));
}

/// GraphBridge's search preflight and full-text statement, verbatim, against
/// the compatibility identity: the components rows satisfy its parser (one
/// 2026.x kernel, one Cypher row advertising "25") and the statement ranks.
#[tokio::test]
async fn compat_identity_answers_the_search_preflight_and_a_fulltext_call() {
    let graph = new_dir_graph_in_mode(StorageMode::Memory, None).expect("create memory graph");
    let backend = KgliteBackend::new(
        kglite::api::session::Session::new(graph),
        std::env::temp_dir().join("search-identity-unused.kgl"),
        false,
        "127.0.0.1:0".into(),
        CsvImportPolicy::Denied,
        ServerIdentity::Neo4jCompatible,
        None,
    );
    let session = SessionHandle("compat-search".into());
    for query in [
        "CREATE (:Doc {id: 1, text: 'quick brown fox'}), (:Doc {id: 2, text: 'slow green turtle'}), \
         (:Doc {id: 3, text: 'quick quick hare'})",
        "CALL db.node_text_index.build({type: 'Doc', text_column: 'text'})",
    ] {
        backend
            .execute(&session, query, &HashMap::new(), &BoltDict::new(), None)
            .await
            .unwrap_or_else(|e| panic!("{query}: {e:?}"));
    }
    let limit = HashMap::from([("limit".to_string(), BoltValue::Integer(5))]);
    let components = backend
        .execute(
            &session,
            "CALL dbms.components() YIELD name, versions, edition RETURN name, versions, edition LIMIT 5",
            &HashMap::new(),
            &BoltDict::new(),
            None,
        )
        .await
        .expect("components");
    let rows: Vec<(String, Vec<String>)> = components
        .records
        .iter()
        .map(|r| match (&r.values[0], &r.values[1]) {
            (BoltValue::String(n), BoltValue::List(v)) => (
                n.clone(),
                v.iter()
                    .map(|x| match x {
                        BoltValue::String(s) => s.clone(),
                        other => panic!("version not a string: {other:?}"),
                    })
                    .collect(),
            ),
            other => panic!("unexpected components row: {other:?}"),
        })
        .collect();
    let kernel: Vec<_> = rows.iter().filter(|(n, _)| n == "Neo4j Kernel").collect();
    let cypher: Vec<_> = rows.iter().filter(|(n, _)| n == "Cypher").collect();
    assert_eq!(kernel.len(), 1, "{rows:?}");
    assert_eq!(kernel[0].1.len(), 1, "{rows:?}");
    assert!(kernel[0].1[0].starts_with("2026."), "{rows:?}");
    assert_eq!(cypher.len(), 1, "{rows:?}");
    assert!(cypher[0].1.contains(&"25".to_string()), "{rows:?}");

    let hits = backend
        .execute(
            &session,
            "CYPHER 25 CALL db.index.fulltext.queryNodes($index,$query,{limit:$limit}) \
             YIELD node AS entity,score RETURN entity.id AS id, score ORDER BY score DESC LIMIT $limit",
            &HashMap::from([
                ("index".to_string(), BoltValue::String("Doc.text".into())),
                ("query".to_string(), BoltValue::String("quick".into())),
                ("limit".to_string(), limit["limit"].clone()),
            ]),
            &BoltDict::new(),
            None,
        )
        .await
        .expect("the verbatim search statement runs");
    let ids: Vec<&BoltValue> = hits.records.iter().map(|r| &r.values[0]).collect();
    assert_eq!(ids, [&BoltValue::Integer(3), &BoltValue::Integer(1)]);
}
