//! The graph's stored valid-time default: saved in the file, copied into the
//! default in force on load, and below every runtime setting in precedence.

use std::collections::HashMap;

use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::features::temporal::ValidTimeDefault;
use crate::graph::io::file::{load_kgl_bytes, write_kgl_to};
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};
use std::sync::Arc;

fn run(graph: &mut DirGraph, query: &str) {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params)).unwrap();
}

fn ids(graph: &DirGraph, query: &str) -> Vec<i64> {
    let params = HashMap::new();
    execute_read(graph, query, &ExecuteOptions::eager(&params))
        .unwrap()
        .result
        .rows
        .iter()
        .map(|row| match row[0] {
            Value::Int64(id) => id,
            ref other => panic!("{other:?}"),
        })
        .collect()
}

/// Employees 1 (left in 2010), 2 (employed since 2005), 3 (starts in 2999).
fn staff() -> DirGraph {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (:Employee {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}), \
         (:Employee {id: 2, vf: date('2005-01-01')}), \
         (:Employee {id: 3, vf: date('2999-01-01')})",
    );
    run(
        &mut graph,
        "CALL db.temporal.declare({node: 'Employee', from: 'vf', to: 'vt', \
         convention: 'closed'}) YIELD declared RETURN declared",
    );
    graph
}

const LIST: &str = "MATCH (e:Employee) RETURN e.id AS id ORDER BY id";

fn reload(graph: &DirGraph) -> Arc<DirGraph> {
    let mut bytes = Vec::new();
    write_kgl_to(graph, &mut bytes).unwrap();
    load_kgl_bytes(&bytes).unwrap()
}

#[test]
fn a_stored_default_survives_a_save_and_governs_the_reload() {
    let mut graph = staff();
    graph.set_valid_time_default(ValidTimeDefault::All, true);
    assert_eq!(ids(&graph, LIST), [1, 2, 3]);
    let loaded = reload(&graph);
    assert_eq!(loaded.stored_valid_time_default, ValidTimeDefault::All);
    assert_eq!(loaded.valid_time_default, ValidTimeDefault::All);
    assert_eq!(ids(&loaded, LIST), [1, 2, 3]);
}

#[test]
fn a_session_only_setting_is_not_saved() {
    let mut graph = staff();
    graph.set_valid_time_default(ValidTimeDefault::All, false);
    let loaded = reload(&graph);
    assert_eq!(loaded.stored_valid_time_default, ValidTimeDefault::Today);
    assert_eq!(ids(&loaded, LIST), [2]);
}

/// A graph that never stored a default writes no key for it, so its bytes are
/// the bytes it wrote before the field existed.
#[test]
fn an_unset_default_writes_no_key() {
    let mut bytes = Vec::new();
    write_kgl_to(&staff(), &mut bytes).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("valid_time_default"));
    let mut stored = staff();
    stored.set_valid_time_default(ValidTimeDefault::parse("2008-01-01").unwrap(), true);
    let mut bytes = Vec::new();
    write_kgl_to(&stored, &mut bytes).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("\"valid_time_default\":\"2008-01-01\""));
}

/// Explicit prefix > session setter > server flag (both overwrite the default
/// in force, the later one winning) > stored default > today.
#[test]
fn the_precedence_runs_prefix_then_runtime_then_stored_then_today() {
    let mut graph = staff();
    graph.set_valid_time_default(ValidTimeDefault::All, true);
    let mut loaded = Arc::try_unwrap(reload(&graph)).ok().unwrap();
    assert_eq!(ids(&loaded, LIST), [1, 2, 3], "stored default");

    // A server flag applied after the load.
    loaded.valid_time_default = ValidTimeDefault::parse("2008-01-01").unwrap();
    assert_eq!(ids(&loaded, LIST), [1, 2], "flag over stored");

    // A session setter, later still.
    loaded.set_valid_time_default(ValidTimeDefault::Today, false);
    assert_eq!(ids(&loaded, LIST), [2], "setter over flag");
    assert_eq!(
        loaded.stored_valid_time_default,
        ValidTimeDefault::All,
        "the setter left the stored default alone"
    );

    // An explicit prefix beats all of it.
    assert_eq!(
        ids(&loaded, &format!("FOR VALID_TIME ALL {LIST}")),
        [1, 2, 3]
    );
    assert_eq!(
        ids(
            &loaded,
            &format!("FOR VALID_TIME AS OF date('2008-01-01') {LIST}")
        ),
        [1, 2]
    );
}
