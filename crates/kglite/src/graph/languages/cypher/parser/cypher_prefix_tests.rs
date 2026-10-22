//! The `CYPHER [<version>] [<option>=<value> ...]` statement prefix.

use super::super::parse_cypher;
use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::session::{execute_read, ExecuteOptions};
use std::collections::HashMap;

fn parse_err(query: &str) -> String {
    match parse_cypher(query) {
        Ok(q) => panic!("{query} parsed: {:?}", q.clauses),
        Err(e) => e.to_string(),
    }
}

#[test]
fn versions_5_and_25_prefix_any_statement_kind() {
    let body = [
        "MATCH (n) RETURN n",
        "RETURN 1",
        "CREATE (n:T {id: 1})",
        "MATCH (n) SET n.x = 1",
        "CALL db.labels()",
        "CREATE INDEX FOR (n:T) ON (n.p)",
        "SHOW INDEXES",
        "UNWIND [1] AS x RETURN x UNION RETURN 2",
    ];
    for version in ["5", "25"] {
        for statement in body {
            let prefixed = format!("CYPHER {version} {statement}");
            let plain = parse_cypher(statement).unwrap();
            let with_prefix = parse_cypher(&prefixed).unwrap_or_else(|e| panic!("{prefixed}: {e}"));
            assert_eq!(
                format!("{:?}", plain.clauses),
                format!("{:?}", with_prefix.clauses),
                "{prefixed}"
            );
        }
    }
    parse_cypher("cypher 5 RETURN 1").unwrap();
    parse_cypher("CYPHER RETURN 1").unwrap();
}

#[test]
fn composes_with_explain_and_profile_in_either_order() {
    for (word, explain, profile) in [("EXPLAIN", true, false), ("PROFILE", false, true)] {
        for text in [
            format!("CYPHER 5 {word} MATCH (n) RETURN n"),
            format!("{word} CYPHER 5 MATCH (n) RETURN n"),
            format!("CYPHER 25 {word} MATCH (n) RETURN n"),
        ] {
            let query = parse_cypher(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!((query.explain, query.profile), (explain, profile), "{text}");
        }
    }
    parse_cypher("CYPHER 5 FOR VALID_TIME AS OF date('2020-01-01') RETURN 1").unwrap();
}

#[test]
fn known_options_are_accepted_and_unknown_ones_rejected() {
    parse_cypher("CYPHER runtime=slotted RETURN 1").unwrap();
    parse_cypher("CYPHER 5 planner=cost runtime=interpreted RETURN 1").unwrap();
    parse_cypher("CYPHER 25 expressionEngine='interpreted' debug=toString debug=ast RETURN 1")
        .unwrap();
    parse_cypher("CYPHER replan=skip updateStrategy=eager RETURN 1").unwrap();

    let err = parse_err("CYPHER 5 turbo=on RETURN 1");
    assert!(err.contains("Unknown CYPHER option 'turbo'"), "{err}");
    assert!(err.contains("runtime"), "lists the known options: {err}");
    let err = parse_err("CYPHER runtime=");
    assert!(
        err.contains("Expected a value for CYPHER option 'runtime'"),
        "{err}"
    );
}

#[test]
fn unknown_versions_and_doubled_prefixes_are_rejected() {
    for text in [
        "CYPHER 99 RETURN 1",
        "CYPHER 4 RETURN 1",
        "CYPHER 0 RETURN 1",
    ] {
        let err = parse_err(text);
        assert!(err.contains("Unsupported Cypher version"), "{text}: {err}");
        assert!(err.contains("5 and 25"), "{text}: {err}");
    }
    let err = parse_err("CYPHER 2026.06 RETURN 1");
    assert!(err.contains("Unsupported Cypher version"), "{err}");
    let err = parse_err("CYPHER 5 CYPHER 25 RETURN 1");
    assert!(err.contains("one CYPHER prefix"), "{err}");
}

/// GraphBridge's statements parse past the prefix; the procedures themselves
/// are answered (or refused) by the executor, not the parser.
#[test]
fn the_graphbridge_prefixed_statements_parse() {
    for text in [
        "CYPHER 25 CALL db.index.fulltext.queryNodes($index,$query,{limit:$limit}) YIELD node AS entity,score RETURN entity,score ORDER BY score DESC LIMIT $limit",
        "CYPHER 25 MATCH (entity)\nSEARCH entity IN (VECTOR INDEX `i` FOR $query LIMIT $limit) SCORE AS score\nRETURN entity, score ORDER BY score DESC LIMIT $limit",
        "CYPHER 5 MATCH (start:`L` {id: 1})-[r*1..3]->(stop) RETURN stop",
        "CYPHER 25 SHOW CURRENT GRAPH TYPE",
    ] {
        // Statement forms the engine does not implement yet fail on the
        // statement itself, never on the prefix.
        if let Err(e) = parse_cypher(text) {
            let e = e.to_string();
            assert!(
                !e.contains("Unsupported Cypher version")
                    && !e.contains("CYPHER option")
                    && !e.contains("one CYPHER prefix"),
                "{text}: {e}"
            );
        }
    }
    parse_cypher("CYPHER 5 MATCH (start:`L` {id: 1})-[r*1..3]->(stop) RETURN stop").unwrap();
}

#[test]
fn a_prefixed_statement_runs_and_explains() {
    let g = DirGraph::new();
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let rows = execute_read(&g, "CYPHER 25 RETURN 1 AS one", &opts)
        .unwrap()
        .result
        .rows;
    assert_eq!(rows, vec![vec![Value::Int64(1)]]);
    let explained = execute_read(&g, "CYPHER 5 EXPLAIN MATCH (n) RETURN n", &opts).unwrap();
    assert!(explained.explain && !explained.result.rows.is_empty());
    let Err(err) = execute_read(&g, "CYPHER 7 RETURN 1", &opts) else {
        panic!("CYPHER 7 must be rejected");
    };
    assert!(
        err.to_string().contains("Unsupported Cypher version"),
        "{err}"
    );
}
