//! A load that owns its edges writes one relationship per row; identical rows
//! are reported by default and collapsed on request, and nothing else about
//! the load changes.

use super::*;

fn graph_with_nodes() -> DirGraph {
    let mut graph = DirGraph::new();
    for (label, ids) in [("Employee", [1, 2]), ("Project", [10, 20])] {
        let rows = ids.iter().map(|id| vec![Value::Int64(*id)]).collect();
        let df = DataFrame::from_cypher_rows(vec!["id".to_string()], rows).unwrap();
        add_nodes(
            &mut graph,
            df,
            label.to_string(),
            "id".to_string(),
            None,
            None,
        )
        .unwrap();
    }
    graph
}

fn load(
    graph: &mut DirGraph,
    rows: &[(i64, i64, &str)],
    policy: IdenticalRows,
) -> ConnectionOperationReport {
    let rows = rows
        .iter()
        .map(|(s, t, role)| {
            vec![
                Value::Int64(*s),
                Value::Int64(*t),
                Value::String((*role).to_string()),
            ]
        })
        .collect();
    let df = DataFrame::from_cypher_rows(
        vec!["src".to_string(), "tgt".to_string(), "role".to_string()],
        rows,
    )
    .unwrap();
    add_connections_with_identical_rows(
        graph,
        df,
        "WORKS_ON".to_string(),
        "Employee".to_string(),
        "src".to_string(),
        "Project".to_string(),
        "tgt".to_string(),
        None,
        None,
        None,
        policy,
    )
    .unwrap()
}

const ROWS: [(i64, i64, &str); 4] = [
    (1, 10, "dev"),
    (1, 10, "dev"),
    (1, 10, "lead"),
    (2, 20, "dev"),
];

#[test]
fn keep_stores_every_row_and_warns_once_with_the_counts() {
    let mut graph = graph_with_nodes();
    let report = load(&mut graph, &ROWS, IdenticalRows::Keep);
    assert_eq!(report.connections_created, 4);
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    let w = &report.warnings[0];
    assert!(w.contains("'WORKS_ON'"), "{w}");
    assert!(w.contains("4 relationships"), "{w}");
    assert!(
        w.contains("3 distinct (source, target, properties) combinations"),
        "{w}"
    );
    assert!(w.contains("up to 2 identical copies"), "{w}");
}

#[test]
fn collapse_keeps_the_first_of_each_identical_group_and_is_silent() {
    let mut graph = graph_with_nodes();
    let report = load(&mut graph, &ROWS, IdenticalRows::Collapse);
    assert_eq!(report.connections_created, 3);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}

#[test]
fn distinct_rows_warn_about_nothing() {
    let mut graph = graph_with_nodes();
    let report = load(&mut graph, &ROWS[2..], IdenticalRows::Keep);
    assert_eq!(report.connections_created, 2);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}

#[test]
fn an_off_tracker_admits_every_row_and_never_warns() {
    let mut tracker = IdenticalRowTracker::off();
    assert!(tracker.admit(1, &[], false));
    assert!(tracker.admit(1, &[], false));
    assert!(tracker.warning("T").is_none());
}

#[test]
fn collapse_is_exact_when_distinct_rows_share_a_hash() {
    use crate::graph::mutation::identical_rows::FORCED_CELLS_HASH;
    FORCED_CELLS_HASH.with(|f| f.set(Some(7)));
    let mut tracker = IdenticalRowTracker::new(IdenticalRows::Collapse);
    let dev = [(2usize, Value::String("dev".into()))];
    let lead = [(2usize, Value::String("lead".into()))];
    assert!(tracker.admit(1, &dev, true));
    assert!(
        tracker.admit(1, &lead, true),
        "a hash collision dropped a distinct row"
    );
    assert!(!tracker.admit(1, &dev, true));
    assert!(!tracker.admit(1, &lead, true));
    FORCED_CELLS_HASH.with(|f| f.set(None));
}
