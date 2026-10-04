//! `ExportManifest`: build, JSON identity, and re-declaration.

use std::collections::HashMap;

use super::*;
use crate::datatypes::Value;
use crate::graph::features::temporal::{list, IntervalConvention};
use crate::graph::session::execute::{execute_mut, ExecuteOptions};

fn run(graph: &mut DirGraph, query: &str) {
    let params: HashMap<String, Value> = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"));
}

/// Org-chart graph: a node-declared type with two versions of one id, a
/// secondary label, every property kind, and a relationship from two source
/// types with one source-keyed declaration.
fn org_graph() -> DirGraph {
    let mut g = DirGraph::new();
    for q in [
        "CREATE (:Person:Employee {id: 1, title: 'Ada', hired: '2000-01-01', left: '2009-12-31', \
         score: 1.5, active: true, at: datetime('2020-01-02T03:04:05'), tags: ['a', 'b'], \
         meta: {k: 1}, span: duration({days: 3}), home: point({latitude: 1.0, longitude: 2.0})})",
        "CREATE (:Person:Employee {id: 1, title: 'Ada', hired: '2010-01-01', left: null})",
        "CREATE (:Plant {id: 'p1', title: 'North'})",
        "MATCH (a:Person), (p:Plant) WHERE a.hired = '2000-01-01' CREATE (a)-[:WORKS_AT {since: '2001-01-01', until: '2002-01-01', note: 'x'}]->(p)",
        "MATCH (a:Plant), (p:Person) WHERE p.hired = '2000-01-01' CREATE (a)-[:WORKS_AT {from: '2001-01-01', to: '2002-01-01'}]->(p)",
    ] {
        run(&mut g, q);
    }
    let nodes = [
        ("Person", "hired", "left", IntervalConvention::Closed),
        ("Employee", "hired", "left", IntervalConvention::HalfOpen),
    ];
    for (label, from, to, convention) in nodes {
        declare_loaded(
            &mut g,
            &TemporalTarget::Node(label.into()),
            from,
            to,
            convention,
            &[],
        )
        .unwrap_or_else(|e| panic!("{label}: {e}"));
    }
    for (source, from, to, convention) in [
        ("Person", "since", "until", IntervalConvention::Closed),
        ("Plant", "from", "to", IntervalConvention::HalfOpen),
    ] {
        declare_loaded(
            &mut g,
            &TemporalTarget::Relationship {
                rel_type: "WORKS_AT".into(),
                source_type: Some(source.into()),
            },
            from,
            to,
            convention,
            &[],
        )
        .unwrap_or_else(|e| panic!("{source}: {e}"));
    }
    g
}

#[test]
fn manifest_describes_types_labels_columns_and_declarations() {
    let g = org_graph();
    let parents = HashMap::from([("Plant".to_string(), "Person".to_string())]);
    let m = ExportManifest::build(&g, None, &parents).unwrap();
    assert_eq!(m.format, "kglite-export/1");
    let person = &m.node_types["Person"];
    assert_eq!(person.count, 2);
    assert_eq!(person.labels, vec!["Employee".to_string()]);
    assert_eq!(person.id_kind, ColumnKind::Int64);
    assert_eq!(person.title_kind, ColumnKind::String);
    assert_eq!(person.properties["hired"], ColumnKind::String);
    assert_eq!(person.properties["score"], ColumnKind::Float64);
    assert_eq!(person.properties["active"], ColumnKind::Boolean);
    assert_eq!(person.properties["at"], ColumnKind::Timestamp);
    assert_eq!(person.properties["tags"], ColumnKind::List);
    assert_eq!(person.properties["meta"], ColumnKind::Map);
    assert_eq!(person.properties["span"], ColumnKind::Duration);
    assert_eq!(m.node_types["Plant"].parent.as_deref(), Some("Person"));
    assert_eq!(m.node_types["Plant"].id_kind, ColumnKind::String);
    let works = &m.relationship_types["WORKS_AT"];
    assert_eq!(works.count, 2);
    assert_eq!(works.sources["Person"].targets, vec!["Plant".to_string()]);
    assert_eq!(works.sources["Plant"].targets, vec!["Person".to_string()]);
    assert_eq!(m.temporal.len(), 4);
}

#[test]
fn json_round_trip_is_identity_and_stable() {
    let g = org_graph();
    let m = ExportManifest::build(&g, None, &HashMap::new()).unwrap();
    let text = m.to_json();
    let back = ExportManifest::from_json(&text).unwrap();
    assert_eq!(m, back);
    assert_eq!(text, back.to_json());
    assert!(
        ExportManifest::from_json(&text.replace("kglite-export/1", "kglite-export/9")).is_err()
    );
}

#[test]
fn declarations_round_trip_for_both_conventions_and_source_keys() {
    let source = org_graph();
    let m = ExportManifest::build(&source, None, &HashMap::new()).unwrap();
    // Same rows, no declarations: rebuild the rows then apply the manifest.
    let mut target = org_graph();
    for info in list(&target) {
        crate::graph::features::temporal::undeclare(&mut target, &info.target);
    }
    assert!(list(&target).is_empty());
    let warnings = m.apply_declarations(&mut target).unwrap();
    assert!(
        warnings.iter().all(|w| !w.message.contains("refused")),
        "{warnings:?}"
    );
    let key = |g: &DirGraph| {
        list(g)
            .into_iter()
            .map(|i| {
                format!(
                    "{:?}|{}|{}|{:?}",
                    i.target, i.config.valid_from, i.config.valid_to, i.config.convention
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(key(&source), key(&target));
    // Applying twice is a no-op.
    m.apply_declarations(&mut target).unwrap();
    assert_eq!(key(&source), key(&target));
}

#[test]
fn mixed_kinds_are_flagged() {
    let mut g = DirGraph::new();
    run(&mut g, "CREATE (:T {id: 1, v: 1}), (:T {id: 2, v: 'x'})");
    let m = ExportManifest::build(&g, None, &HashMap::new()).unwrap();
    assert_eq!(m.node_types["T"].properties["v"], ColumnKind::Mixed);
}
