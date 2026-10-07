//! The valid slice: its node and relationship rule, the index map back to
//! the base, user ids, its caps, and agreement between the mask route and the
//! evaluator route.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use petgraph::graph::NodeIndex;

use super::*;
use crate::datatypes::values::Value;
use crate::graph::core::graph_filter::{GraphFilter, ValidTimeSelector};
use crate::graph::features::temporal::declarations::{declare, TemporalTarget};
use crate::graph::features::temporal::endpoint_index;
use crate::graph::features::temporal::eval::IntervalConvention;
use crate::graph::languages::cypher::valid_time::declared_template;
use crate::graph::session::execute::{execute_mut, ExecuteOptions};

fn run(graph: &mut DirGraph, query: &str) {
    let params: HashMap<String, Value> = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"));
}

/// Wells (declared, closed) on fields (undeclared); `IN` relationships
/// declared half-open. At 2005-06-01: well 1 valid, well 2 not yet, well 3
/// valid; the relationship from well 1 has ended, the one from well 3 runs.
/// Well 4 carries the declared secondary label `Pad`, whose own interval has
/// not started, so it is hidden although its `Well` interval holds.
fn fixture() -> DirGraph {
    let mut g = DirGraph::new();
    run(
        &mut g,
        "CREATE (w1:Well {id: 1, vf: date('2000-01-01'), vt: date('2009-12-31')}),
                (w2:Well {id: 2, vf: date('2008-01-01')}),
                (w3:Well {id: 3, vf: date('2001-01-01')}),
                (w4:Well {id: 4, vf: date('2001-01-01'), pf: date('2010-01-01'), pt: date('2040-01-01')}),
                (f:Project {id: 10}),
                (w1)-[:IN {eid: 1, f: date('2000-01-01'), t: date('2004-01-01')}]->(f),
                (w2)-[:IN {eid: 2, f: date('2000-01-01')}]->(f),
                (w3)-[:IN {eid: 3, f: date('2000-01-01')}]->(f),
                (w4)-[:IN {eid: 4, f: date('2000-01-01')}]->(f),
                (w1)-[:NEAR {eid: 5}]->(w3)",
    );
    run(&mut g, "MATCH (w:Well {id: 4}) SET w:Pad");
    let closed = IntervalConvention::Closed;
    declare(
        &mut g,
        &TemporalTarget::Node("Well".into()),
        "vf",
        "vt",
        closed,
    )
    .unwrap();
    declare(
        &mut g,
        &TemporalTarget::Node("Pad".into()),
        "pf",
        "pt",
        closed,
    )
    .unwrap();
    let in_rel = TemporalTarget::Relationship {
        rel_type: "IN".into(),
        source_type: None,
    };
    declare(&mut g, &in_rel, "f", "t", IntervalConvention::HalfOpen).unwrap();
    g
}

fn instant(text: &str) -> Instant {
    Instant::Date(chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap())
}

fn filter_at(g: &DirGraph, t: &str) -> Option<ElementFilter> {
    let filter = GraphFilter {
        template: Arc::new(declared_template(g).unwrap()),
        selector: ValidTimeSelector::AsOf(instant(t)),
    };
    let resolved = filter.resolve(g);
    ElementFilter::new(&filter, resolved)
}

fn unlimited() -> SliceCaps {
    SliceCaps {
        bytes: usize::MAX,
        disk_elements: None,
    }
}

fn node_ids(g: &DirGraph) -> BTreeSet<i64> {
    g.graph
        .node_indices()
        .filter_map(|idx| match g.graph.get_node_id(idx) {
            Some(Value::Int64(id)) => Some(id),
            _ => None,
        })
        .collect()
}

fn edge_ids(g: &DirGraph) -> BTreeSet<i64> {
    let key = InternedKey::from_str("eid");
    g.graph
        .edge_indices()
        .filter_map(|e| match g.graph.get_edge_property(e, key) {
            Some(Value::Int64(id)) => Some(id),
            _ => None,
        })
        .collect()
}

fn slice(g: &DirGraph, t: &str) -> ValidSlice {
    slice_at(g, filter_at(g, t).as_ref(), unlimited()).unwrap()
}

#[test]
fn nodes_pass_every_declared_label_and_relationships_need_both_endpoints() {
    let g = fixture();
    let s = slice(&g, "2005-06-01");
    // Well 2 has not started; well 4's Pad interval has not started.
    assert_eq!(node_ids(s.graph()), BTreeSet::from([1, 3, 10]));
    // eid 1 ended in 2004; eid 2 is valid but reaches the hidden well 2;
    // eid 4 leaves the hidden well 4; NEAR is undeclared and joins 1 → 3.
    assert_eq!(edge_ids(s.graph()), BTreeSet::from([3, 5]));
    assert!(
        super::super::declared(s.graph()).is_empty(),
        "declarations are not copied"
    );
    let later = slice(&g, "2011-06-01");
    // Well 1 closed at the end of 2009; well 4's Pad interval has opened.
    assert_eq!(node_ids(later.graph()), BTreeSet::from([2, 3, 4, 10]));
    assert_eq!(edge_ids(later.graph()), BTreeSet::from([2, 3, 4]));
}

#[test]
fn to_base_maps_every_slice_node_back_and_keeps_user_ids() {
    let g = fixture();
    let s = slice(&g, "2005-06-01");
    let mut last = None;
    for idx in s.graph().graph.node_indices() {
        let base = s.to_base(idx).unwrap();
        assert!(last < Some(base), "to_base ascends");
        last = Some(base);
        assert_eq!(s.graph().graph.get_node_id(idx), g.graph.get_node_id(base));
        assert_eq!(
            s.graph().graph.node_type_of(idx),
            g.graph.node_type_of(base)
        );
        assert_eq!(s.from_base(base), Some(idx));
    }
    assert_eq!(s.to_base(NodeIndex::new(99)), None);
    let hidden = g
        .graph
        .node_indices()
        .find(|&idx| g.graph.get_node_id(idx) == Some(Value::Int64(2)))
        .unwrap();
    assert_eq!(s.from_base(hidden), None);
    // Relationship endpoints map to the same user ids on both sides.
    for e in s.graph().graph.edge_indices() {
        let (a, b) = s.graph().graph.edge_endpoints(e).unwrap();
        let (base_a, base_b) = (s.to_base(a).unwrap(), s.to_base(b).unwrap());
        assert!(g.graph.find_edge(base_a, base_b).is_some());
    }
    // The secondary label travels with the node.
    let later = slice(&g, "2011-06-01");
    assert!(later.graph().has_secondary_labels);
}

#[test]
fn the_evaluator_route_slices_as_the_mask_route_does() {
    let g = fixture();
    // The same graph with a one-byte endpoint-index cap: no target indexed,
    // every admit test evaluates the bound properties.
    let guarded = fixture();
    endpoint_index::set_byte_cap(&guarded, 1);
    for t in [
        "1999-06-01",
        "2003-06-01",
        "2005-06-01",
        "2009-12-31",
        "2011-06-01",
    ] {
        let masked = slice(&g, t);
        let evaluated = slice(&guarded, t);
        assert_eq!(node_ids(masked.graph()), node_ids(evaluated.graph()), "{t}");
        assert_eq!(edge_ids(masked.graph()), edge_ids(evaluated.graph()), "{t}");
    }
}

#[test]
fn an_unreadable_bound_is_an_error_naming_the_element() {
    let mut g = fixture();
    crate::graph::features::temporal::unchecked(|| {
        run(&mut g, "MATCH (w:Well {id: 3}) SET w.vt = 42")
    });
    let err = slice_at(&g, filter_at(&g, "2005-06-01").as_ref(), unlimited()).unwrap_err();
    assert!(err.contains("node '3'"), "{err}");
}

#[test]
fn a_slice_over_the_byte_cap_is_refused() {
    let g = fixture();
    let whole = slice(&g, "2005-06-01");
    let caps = SliceCaps {
        bytes: whole.bytes() - 1,
        disk_elements: None,
    };
    let err = slice_at(&g, filter_at(&g, "2005-06-01").as_ref(), caps).unwrap_err();
    assert!(err.contains("slice cap"), "{err}");
    assert!(err.contains(SLICE_BYTE_CAP_ENV), "{err}");
}

#[test]
fn the_element_cap_refuses_as_the_walk_passes_it() {
    let g = fixture();
    // Three nodes and two relationships are admitted at 2005-06-01.
    let caps = |n| SliceCaps {
        bytes: usize::MAX,
        disk_elements: Some(n),
    };
    let filter = filter_at(&g, "2005-06-01");
    assert!(slice_at(&g, filter.as_ref(), caps(5)).is_ok());
    let err = slice_at(&g, filter.as_ref(), caps(4)).unwrap_err();
    assert!(err.contains("more than 4"), "{err}");
    assert!(err.contains(DISK_SLICE_CAP_ENV), "{err}");
}

#[test]
fn no_filter_keeps_every_element() {
    let g = fixture();
    let s = slice_at(&g, None, unlimited()).unwrap();
    assert_eq!(node_ids(s.graph()), BTreeSet::from([1, 2, 3, 4, 10]));
    assert_eq!(edge_ids(s.graph()), BTreeSet::from([1, 2, 3, 4, 5]));
}

#[test]
fn the_slice_cache_evicts_oldest_first_under_its_cap() {
    let g = fixture();
    let key = |t: &str| SliceKey {
        segments: Vec::new(),
        instant: Some(instant(t)),
    };
    let a = Arc::new(slice(&g, "2005-06-01"));
    let b = Arc::new(slice(&g, "2011-06-01"));
    let cap = a.bytes().max(b.bytes()) + 1;
    endpoint_index::store_slice(&g, key("2005-06-01"), &a, cap);
    assert!(endpoint_index::cached_slice(&g, &key("2005-06-01")).is_some());
    endpoint_index::store_slice(&g, key("2011-06-01"), &b, cap);
    assert_eq!(endpoint_index::cached_slice_count(&g), 1);
    assert!(endpoint_index::cached_slice(&g, &key("2005-06-01")).is_none());
    let hit = endpoint_index::cached_slice(&g, &key("2011-06-01")).unwrap();
    assert!(Arc::ptr_eq(&hit, &b));
    // One that alone passes the cap is not kept.
    endpoint_index::store_slice(&g, key("2003-06-01"), &a, a.bytes() - 1);
    assert!(endpoint_index::cached_slice(&g, &key("2003-06-01")).is_none());
}
