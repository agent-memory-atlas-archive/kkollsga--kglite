//! Algorithm procedures under a valid-time context run on the valid slice and
//! yield the graph's own nodes.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::graph::features::temporal::declarations::{declare, TemporalTarget};
use crate::graph::features::temporal::endpoint_index;
use crate::graph::features::temporal::eval::IntervalConvention;
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};

fn run(graph: &mut DirGraph, query: &str) {
    let params: HashMap<String, Value> = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"));
}

type Row = HashMap<String, Value>;

fn rows(graph: &DirGraph, query: &str) -> Result<Vec<Row>, String> {
    let params: HashMap<String, Value> = HashMap::new();
    let result = execute_read(graph, query, &ExecuteOptions::eager(&params))
        .map_err(|e| e.to_string())?
        .result;
    Ok(result
        .rows
        .into_iter()
        .map(|row| result.columns.iter().cloned().zip(row).collect())
        .collect())
}

fn int(row: &Row, column: &str) -> i64 {
    match row.get(column) {
        Some(Value::Int64(v)) => *v,
        other => panic!("{column}: {other:?}"),
    }
}

/// Two chains of wells: 1 → 2 → 3 valid throughout, and 4 → 5 where well 5
/// has not started before 2010. Well 5 is created first, so the slice's node
/// indexes differ from the graph's and an unmapped row would name the wrong
/// well. Relationship 2 → 3 is declared and ended in
/// 2005, so as of 2006 the chain splits.
fn chains() -> DirGraph {
    let mut g = DirGraph::new();
    run(
        &mut g,
        "CREATE (w5:Well {id: 5, vf: date('2010-01-01')}),
                (w1:Well {id: 1, vf: date('2000-01-01'), vt: date('2040-01-01')}),
                (w2:Well {id: 2, vf: date('2000-01-01')}),
                (w3:Well {id: 3, vf: date('2000-01-01')}),
                (w4:Well {id: 4, vf: date('2000-01-01')}),
                (w1)-[:NEXT {f: date('2000-01-01')}]->(w2),
                (w2)-[:NEXT {f: date('2000-01-01'), t: date('2005-01-01')}]->(w3),
                (w4)-[:NEXT {f: date('2000-01-01')}]->(w5)",
    );
    declare(
        &mut g,
        &TemporalTarget::Node("Well".into()),
        "vf",
        "vt",
        IntervalConvention::Closed,
    )
    .unwrap();
    let rel = TemporalTarget::Relationship {
        rel_type: "NEXT".into(),
        source_type: None,
    };
    declare(&mut g, &rel, "f", "t", IntervalConvention::HalfOpen).unwrap();
    g
}

const AS_OF: &str = "FOR VALID_TIME AS OF date('2006-06-01') ";

/// Component membership as sets of user ids.
fn components(graph: &DirGraph, prefix: &str) -> BTreeSet<BTreeSet<i64>> {
    let query = format!(
        "{prefix}CALL connected_components() YIELD node, component \
         RETURN node.id AS id, component AS c"
    );
    let mut groups: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
    for row in rows(graph, &query).unwrap() {
        groups
            .entry(int(&row, "c"))
            .or_default()
            .insert(int(&row, "id"));
    }
    groups.into_values().collect()
}

#[test]
fn a_routed_algorithm_sees_only_the_valid_elements_and_yields_base_nodes() {
    let g = chains();
    let expected: BTreeSet<BTreeSet<i64>> = [
        BTreeSet::from([1, 2]),
        BTreeSet::from([3]),
        BTreeSet::from([4]),
    ]
    .into();
    assert_eq!(components(&g, AS_OF), expected);
    // Unprefixed, every element counts.
    let whole: BTreeSet<BTreeSet<i64>> = [BTreeSet::from([1, 2, 3]), BTreeSet::from([4, 5])].into();
    assert_eq!(components(&g, ""), whole);

    // The yielded node is the graph's own: its element id and a hop from it
    // read the base graph.
    let query = format!(
        "{AS_OF}CALL pagerank() YIELD node, score \
         MATCH (w:Well) WHERE elementId(w) = elementId(node) \
         RETURN node.id AS id, w.id AS same"
    );
    let found = rows(&g, &query).unwrap();
    assert_eq!(found.len(), 4);
    for row in &found {
        assert_eq!(int(row, "id"), int(row, "same"));
    }
}

#[test]
fn a_statement_builds_its_slice_once_and_a_write_rebuilds_it() {
    let mut g = chains();
    let query =
        format!("{AS_OF}UNWIND range(1, 3) AS i CALL k_core() YIELD node RETURN count(*) AS n");
    let found = rows(&g, &query).unwrap();
    assert_eq!(
        int(&found[0], "n"),
        12,
        "four visible nodes, three rows each"
    );
    assert_eq!(endpoint_index::cached_slice_count(&g), 1);
    rows(
        &g,
        &format!("{AS_OF}CALL degree() YIELD node RETURN count(*) AS n"),
    )
    .unwrap();
    assert_eq!(
        endpoint_index::cached_slice_count(&g),
        1,
        "one slice per instant"
    );

    run(
        &mut g,
        "MATCH (w:Well {id: 3}) SET w.vt = date('2003-01-01')",
    );
    assert_eq!(
        endpoint_index::cached_slice_count(&g),
        0,
        "a write drops it"
    );
    let after = components(&g, AS_OF);
    assert!(after.iter().all(|c| !c.contains(&3)), "{after:?}");
}

#[test]
fn a_procedure_outside_the_registry_is_still_refused() {
    let g = chains();
    let err = rows(
        &g,
        &format!("{AS_OF}CALL orphan_node() YIELD node RETURN node"),
    )
    .unwrap_err();
    assert!(err.contains("procedure orphan_node"), "{err}");
    // In 2002 only well 5 (not started) is hidden.
    let early = "FOR VALID_TIME AS OF date('2002-06-01') \
                 CALL connected_components() YIELD node RETURN count(*) AS n";
    assert_eq!(int(&rows(&g, early).unwrap()[0], "n"), 4);
}
