//! The fluent filter answers through the core `ElementFilter`: the same
//! lookup order, label rule and hop rule a `FOR VALID_TIME AS OF` statement
//! uses, and nothing built on a graph with no declaration.

use std::collections::HashMap;

use chrono::NaiveDate;
use petgraph::graph::NodeIndex;

use super::FluentFilter;
use crate::datatypes::Value;
use crate::graph::core::traversal::make_traversal;
use crate::graph::dir_graph::DirGraph;
use crate::graph::schema::{CurrentSelection, InternedKey};
use crate::graph::session::execute::{execute_mut, ExecuteOptions};
use crate::graph::storage::GraphRead;
use crate::graph::TemporalContext;

fn graph(queries: &[&str]) -> DirGraph {
    let mut graph = DirGraph::new();
    let params: HashMap<String, Value> = HashMap::new();
    for query in queries {
        execute_mut(&mut graph, query, &ExecuteOptions::eager(&params))
            .unwrap_or_else(|e| panic!("{query}: {e}"));
    }
    graph
}

fn day(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
}

fn node(graph: &DirGraph, title: &str) -> NodeIndex {
    graph
        .graph
        .node_indices()
        .find(|&idx| {
            graph
                .graph
                .node_view(idx)
                .is_some_and(|n| *n.title() == Value::String(title.into()))
        })
        .unwrap_or_else(|| panic!("no node {title}"))
}

/// A relationship takes its source's keyed declaration, and the unkeyed one
/// only when its source has none — whatever order they were declared in.
#[test]
fn a_relationship_takes_its_sources_keyed_declaration_before_the_fallback() {
    let g = graph(&[
        "CREATE (f:Project {title: 'F'}), (l:Contract {title: 'L'}), (c:Company {title: 'C'}), \
         (f)-[:HAS {f: '2000-01-01', t: '2009-12-31'}]->(c), \
         (l)-[:HAS {f: '2000-01-01', t: '2009-12-31'}]->(c)",
        "CALL db.temporal.declare({relationship: 'HAS', from: 'f', to: 't', convention: 'closed'})",
        "CALL db.temporal.declare({relationship: 'HAS', source_type: 'Project', from: 'f', to: 't', \
         convention: 'half_open'})",
    ]);
    let filter = FluentFilter::for_traverse(
        &g,
        &TemporalContext::All,
        Some(day("2009-12-31")),
        None,
        None,
        "HAS",
        None,
    )
    .unwrap();
    let conn = InternedKey::from_str("HAS");
    let company = node(&g, "C");
    let hop_from = |source: &str| {
        let source = node(&g, source);
        let edge = g
            .graph
            .edges_directed(source, petgraph::Direction::Outgoing)
            .next()
            .unwrap()
            .id();
        filter.admits_hop(&g, edge, conn, source, company).unwrap()
    };
    // Project's keyed declaration is half-open: its to day is not valid.
    assert!(!hop_from("F"));
    // Contract has no keyed one, so the closed fallback keeps the to day.
    assert!(hop_from("L"));
    filter.finish().unwrap();
}

#[test]
fn a_graph_with_no_declaration_builds_nothing() {
    let g = graph(&["CREATE (:A {title: 'a', vf: '2000-01-01'})-[:R]->(:B {title: 'b'})"]);
    let today = TemporalContext::Today;
    assert!(FluentFilter::for_select(&g, &today, "A", None)
        .unwrap()
        .is_empty());
    assert!(FluentFilter::for_walk(&g, &today, None, "expand()")
        .unwrap()
        .is_empty());
    let traverse = FluentFilter::for_traverse(&g, &today, None, None, None, "R", None).unwrap();
    assert!(traverse.is_empty());
}

/// A traversal keeps a target only when the target is valid too: the hop
/// rule `FOR VALID_TIME AS OF` applies to every pattern.
#[test]
fn a_traversal_drops_a_target_that_is_not_valid() {
    let g = graph(&[
        "CREATE (p:P {title: 'p'}), (a:A {title: 'a', vf: '2000-01-01', vt: '2005-01-01'}), \
         (p)-[:IN {vf: '2000-01-01', vt: '2030-01-01'}]->(a)",
        "CALL db.temporal.declare({node: 'A', from: 'vf', to: 'vt', convention: 'half_open'})",
        "CALL db.temporal.declare({relationship: 'IN', from: 'vf', to: 'vt', convention: 'half_open'})",
    ]);
    let reached = |context: TemporalContext, temporal: Option<bool>| {
        let mut selection = CurrentSelection::new();
        selection.add_level();
        crate::graph::core::filtering::select_nodes(
            &g,
            &mut selection,
            "P",
            false,
            None,
            None,
            &FluentFilter::default(),
        )
        .unwrap();
        let filter =
            FluentFilter::for_traverse(&g, &context, None, None, temporal, "IN", None).unwrap();
        make_traversal(
            &g,
            &mut selection,
            "IN".to_string(),
            None,
            Some("outgoing".to_string()),
            None,
            None,
            None,
            None,
            None,
            Some(&filter),
            None,
        )
        .unwrap();
        selection.current_node_count()
    };
    assert_eq!(reached(TemporalContext::At(day("2010-01-01")), None), 0);
    assert_eq!(reached(TemporalContext::At(day("2003-01-01")), None), 1);
    assert_eq!(
        reached(TemporalContext::At(day("2010-01-01")), Some(false)),
        1
    );
    assert_eq!(reached(TemporalContext::All, None), 1);
}

/// The unreadable-bound error names the fluent step, not the Cypher context.
#[test]
fn an_unreadable_bound_is_reported_under_the_steps_name() {
    let g = crate::graph::features::temporal::unchecked(|| {
        graph(&[
            "CREATE (:S {title: 's', vf: '2000-01-01', vt: '2005-01-01'})",
            "CALL db.temporal.declare({node: 'S', from: 'vf', to: 'vt', convention: 'closed'})",
            "MATCH (s:S) SET s.vt = 20210101",
        ])
    });
    let mut selection = CurrentSelection::new();
    selection.add_level();
    let filter =
        FluentFilter::for_select(&g, &TemporalContext::At(day("2003-01-01")), "S", None).unwrap();
    let err = crate::graph::core::filtering::select_nodes(
        &g,
        &mut selection,
        "S",
        false,
        None,
        None,
        &filter,
    )
    .unwrap_err();
    assert!(err.starts_with("select(): node '"), "{err}");
    assert!(err.contains("property 'vt'"), "{err}");
}
