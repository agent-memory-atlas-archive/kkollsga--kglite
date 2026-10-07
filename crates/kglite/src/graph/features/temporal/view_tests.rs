//! The valid-time view: creation refusals, the prefix it writes, the masks
//! it pins, and its cached slice.

use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::graph::core::graph_filter::GuardTemplate;
use crate::graph::features::temporal::declarations::{declare, TemporalTarget};
use crate::graph::features::temporal::eval::IntervalConvention;
use crate::graph::languages::cypher::result::CypherResult;
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};
use crate::graph::storage::GraphRead;

fn run(graph: &mut DirGraph, query: &str) {
    let params: HashMap<String, Value> = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"));
}

fn read(graph: &DirGraph, query: &str) -> CypherResult {
    let params: HashMap<String, Value> = HashMap::new();
    execute_read(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
}

fn wells() -> Arc<DirGraph> {
    let mut g = DirGraph::new();
    run(
        &mut g,
        "UNWIND range(0, 11) AS i CREATE (w:Well {id: i, \
         vf: date({year: 2000 + i, month: 1, day: 1}), \
         vt: date({year: 2000 + i, month: 12, day: 31})})-[:IN {f: date('2004-01-01'), t: null}]->(:Project {id: 100 + i})",
    );
    run(
        &mut g,
        "MATCH ()-[r:IN]->() WHERE r.f IS NOT NULL SET r.t = date('2040-01-01')",
    );
    let closed = IntervalConvention::Closed;
    declare(
        &mut g,
        &TemporalTarget::Node("Well".into()),
        "vf",
        "vt",
        closed,
    )
    .unwrap();
    let rel = TemporalTarget::Relationship {
        rel_type: "IN".into(),
        source_type: None,
    };
    declare(&mut g, &rel, "f", "t", closed).unwrap();
    Arc::new(g)
}

fn date(text: &str) -> Value {
    Value::DateTime(chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap())
}

fn ids(result: &CypherResult) -> Vec<Value> {
    let mut ids: Vec<Value> = result.rows.iter().map(|row| row[0].clone()).collect();
    ids.sort_by_key(|v| format!("{v:?}"));
    ids
}

#[test]
fn a_view_refuses_a_bad_instant_and_a_graph_without_declarations() {
    for bad in [Value::String("soon".into()), Value::Int64(3)] {
        let err = view_at(wells(), &bad).unwrap_err().to_string();
        assert!(err.contains("valid_at"), "{err}");
    }
    let err = view_at(Arc::new(DirGraph::new()), &date("2005-06-01"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("needs a validity declaration"), "{err}");
}

#[test]
fn a_view_refuses_an_ambiguous_relationship_declaration() {
    let mut g = DirGraph::new();
    run(
        &mut g,
        "CREATE (:A {id: 1})-[:R {a: date('2000-01-01'), b: date('2001-01-01'), c: null, d: null}]->(:B {id: 2})",
    );
    run(
        &mut g,
        "MATCH ()-[r:R]->() SET r.c = date('2000-01-01'), r.d = date('2002-01-01')",
    );
    let rel = TemporalTarget::Relationship {
        rel_type: "R".into(),
        source_type: None,
    };
    let closed = IntervalConvention::Closed;
    declare(&mut g, &rel, "a", "b", closed).unwrap();
    // A second unkeyed declaration, as a legacy route could leave one.
    let second = crate::graph::schema::TemporalConfig {
        valid_from: "c".to_string(),
        valid_to: "d".to_string(),
        convention: closed,
        source_type: None,
        empty_when: None,
    };
    g.temporal.insert(&rel, second, None);
    assert!(g.temporal.is_ambiguous("R"));
    let err = view_at(Arc::new(g), &date("2000-06-01"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("'R'"), "{err}");
}

#[test]
fn the_view_prefixes_queries_and_refuses_a_second_context() {
    let view = view_at(wells(), &date("2005-06-01")).unwrap();
    assert_eq!(view.as_of(), "date('2005-06-01')");
    let text = view.cypher_text("MATCH (w:Well) RETURN w.id").unwrap();
    assert_eq!(
        text,
        "FOR VALID_TIME AS OF date('2005-06-01') MATCH (w:Well) RETURN w.id"
    );
    assert_eq!(ids(&read(view.base(), &text)), vec![Value::Int64(5)]);
    let err = view
        .cypher_text("FOR VALID_TIME AS OF date('2001-01-01') RETURN 1")
        .unwrap_err();
    assert_eq!(
        err,
        PrependError::ViewAlreadyAsOf {
            literal: "date('2005-06-01')".into()
        }
    );
    assert!(err.to_string().contains("already as of date('2005-06-01')"));
    let explain = view
        .cypher_text("EXPLAIN MATCH (w:Well) RETURN w.id")
        .unwrap();
    assert!(!read(view.base(), &explain).rows.is_empty());
}

#[test]
fn a_view_pins_the_masks_of_every_declared_target() {
    let base = wells();
    let view = view_at(Arc::clone(&base), &date("2005-06-01")).unwrap();
    let pinned = Arc::clone(view.pinned().expect("an indexed view pins its masks"));
    // Queries at nine other years overflow the mask LRU.
    for year in (2000..2012).filter(|&y| y != 2005).take(9) {
        let q = format!(
            "FOR VALID_TIME AS OF date('{year}-06-01') MATCH (w:Well)-[:IN]->(f) RETURN count(*)"
        );
        read(&base, &q);
    }
    // A query on the view that reaches only wells is served the pin.
    let filter = GraphFilter {
        template: Arc::new(GuardTemplate {
            nodes: declared_template(&base).unwrap().nodes,
            edges: Vec::new(),
        }),
        selector: ValidTimeSelector::AsOf(view.instant),
    };
    let resolved = filter.resolve(&base);
    assert!(Arc::ptr_eq(resolved.masks.as_ref().unwrap(), &pinned));
    assert_eq!(
        ids(&read(
            &base,
            &view.cypher_text("MATCH (w:Well) RETURN w.id").unwrap()
        )),
        vec![Value::Int64(5)]
    );
}

#[test]
fn the_view_slice_is_cached_per_segment_and_maps_back_to_the_base() {
    let base = wells();
    let view = view_at(Arc::clone(&base), &date("2005-06-01")).unwrap();
    let slice = view.slice().unwrap();
    // One well and every field; the one IN relationship between them.
    assert_eq!(slice.graph().graph.node_count(), 13);
    assert_eq!(slice.graph().graph.edge_count(), 1);
    let again = view.slice().unwrap();
    assert!(Arc::ptr_eq(&slice, &again));
    // Another view in the same segment shares the slice.
    let same_segment = view_at(Arc::clone(&base), &date("2005-11-30")).unwrap();
    assert!(Arc::ptr_eq(&slice, &same_segment.slice().unwrap()));
    for idx in slice.graph().graph.node_indices() {
        let back = slice.to_base(idx).unwrap();
        assert_eq!(
            slice.graph().graph.get_node_id(idx),
            base.graph.get_node_id(back)
        );
    }
    // The slice answers the unguarded query as the view answers the prefixed one.
    let plain = "MATCH (w:Well)-[:IN]->(f) RETURN f.id";
    assert_eq!(
        ids(&read(slice.graph(), plain)),
        ids(&read(&base, &view.cypher_text(plain).unwrap()))
    );
}
