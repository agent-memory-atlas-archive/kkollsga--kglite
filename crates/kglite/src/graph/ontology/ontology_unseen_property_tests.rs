//! Declaring a property name that no stored row carries returns a warning
//! (a typo or a rename looks exactly like this) and never refuses.

use super::ontology_gate_tests::run;
use crate::graph::dir_graph::DirGraph;
use crate::graph::ontology::ontology_from_json;

fn seeded() -> DirGraph {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (a:Law {id: 1, title: 'A', name: 'x', kind: 'k'}), (b:Actor {id: 10}), \
         (a)-[:HAS {side: 'a', w: 1}]->(b)",
    )
    .unwrap();
    graph
}

fn warnings(graph: &mut DirGraph, json: &str) -> Vec<String> {
    graph
        .define_ontology(ontology_from_json(json).unwrap())
        .expect("a warning, never a refusal")
}

fn unseen(warnings: &[String]) -> Vec<&String> {
    warnings
        .iter()
        .filter(|w| w.contains("no stored"))
        .collect()
}

#[test]
fn a_relationship_property_no_edge_carries_is_named() {
    let mut graph = seeded();
    let all = warnings(
        &mut graph,
        r#"{"classes": {"Law": {}, "Actor": {}},
            "relationships": {"HAS": {"distinct": ["sidee"],
                "required_properties": ["reqd"],
                "property_types": {"typed": "integer"},
                "property_values": {"valued": {"min": 1}}}}}"#,
    );
    let found = unseen(&all);
    assert_eq!(found.len(), 4, "{all:?}");
    for name in ["sidee", "reqd", "typed", "valued"] {
        assert!(found.iter().any(|w| w.contains(name)), "{name}: {all:?}");
    }
}

#[test]
fn carried_properties_and_empty_types_warn_nothing() {
    let mut graph = seeded();
    let all = warnings(
        &mut graph,
        r#"{"classes": {"Law": {}, "Actor": {}},
            "relationships": {
              "HAS": {"distinct": ["side"], "required_properties": ["w"]},
              "LATER": {"distinct": ["anything"], "required_properties": ["x"]}}}"#,
    );
    assert!(unseen(&all).is_empty(), "{all:?}");
}

#[test]
fn a_class_property_no_node_carries_is_named_but_id_title_are_not() {
    let mut graph = seeded();
    let all = warnings(
        &mut graph,
        r#"{"classes": {"Law": {"required_properties": ["id", "title", "kind", "knd"],
                                "property_types": {"name": "string", "nme": "string"},
                                "property_values": {"kindd": {"enum": ["k"]}}},
                        "Actor": {}}}"#,
    );
    let found = unseen(&all);
    assert_eq!(found.len(), 3, "{all:?}");
    for name in ["knd", "nme", "kindd"] {
        assert!(found.iter().any(|w| w.contains(name)), "{name}: {all:?}");
    }
}
