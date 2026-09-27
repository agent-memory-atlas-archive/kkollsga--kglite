//! Id seeks under a valid-time filter when version nodes share an id: the
//! same answer whatever numeric kind each version stores its id as, in every
//! storage mode, and a seek that tests only the nodes carrying the id.

use std::collections::HashMap;

use super::*;
use crate::graph::core::graph_filter::seek_probe;
use crate::graph::features::temporal::endpoint_index::set_byte_cap;
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};
use crate::graph::storage::mode::{new_dir_graph_in_mode, StorageMode};

fn run(graph: &mut DirGraph, query: &str, params: &HashMap<String, Value>) {
    execute_mut(graph, query, &ExecuteOptions::eager(params))
        .unwrap_or_else(|e| panic!("{query}: {e}"));
}

fn names(graph: &DirGraph, query: &str) -> Vec<Value> {
    let params = HashMap::new();
    execute_read(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
        .rows
        .into_iter()
        .map(|mut row| row.remove(0))
        .collect()
}

fn text(s: &str) -> Value {
    Value::String(s.into())
}

/// A graph in `mode` (Disk under `dir`).
fn empty(mode: StorageMode, dir: &tempfile::TempDir) -> DirGraph {
    let path = matches!(mode, StorageMode::Disk).then(|| dir.path());
    new_dir_graph_in_mode(mode, path).expect("graph")
}

/// `M` versions of id 1, `old` (2000–2009) stored with id `old_id` and `new`
/// (2010–) with `new_id`, the older one created first; an `A` node whose id
/// is the loader's `UniqueId(1)`, for a seek by a `UniqueId` value.
fn versions(mode: StorageMode, dir: &tempfile::TempDir, old_id: Value, new_id: Value) -> DirGraph {
    let mut graph = empty(mode, dir);
    let create = |graph: &mut DirGraph, query: &str, id: Value| {
        run(graph, query, &HashMap::from([("id".to_string(), id)]));
    };
    create(&mut graph, "CREATE (:A {id: $id})", Value::UniqueId(1));
    create(
        &mut graph,
        "CREATE (:M {id: $id, name: 'old', vf: date('2000-01-01'), vt: date('2009-12-31')})",
        old_id.clone(),
    );
    create(
        &mut graph,
        "CREATE (:M {id: $id, name: 'new', vf: date('2010-01-01')})",
        new_id.clone(),
    );
    run(
        &mut graph,
        "CALL db.temporal.declare({node: 'M', from: 'vf', to: 'vt', convention: 'closed'}) \
         YIELD declared RETURN declared",
        &HashMap::new(),
    );
    let order = graph.type_indices.get("M").unwrap().to_vec();
    let stored: Vec<Value> = order
        .iter()
        .map(|&idx| graph.graph.get_node_id(idx).unwrap())
        .collect();
    assert_eq!(
        stored,
        [old_id, new_id],
        "{mode:?}: the kinds are stored as given"
    );
    let a = graph.type_indices.get("A").unwrap().to_vec()[0];
    assert_eq!(graph.graph.get_node_id(a), Some(Value::UniqueId(1)));
    graph
}

const SEEKS: [&str; 3] = [
    "MATCH (m:M {id: 1}) RETURN m.name",
    "MATCH (m:M {id: 1.0}) RETURN m.name",
    "MATCH (a:A {id: 1}) MATCH (m:M {id: a.id}) RETURN m.name",
];

#[test]
fn a_seek_finds_the_valid_version_whatever_numeric_kind_each_stores() {
    let kinds = [
        (Value::UniqueId(1), Value::Int64(1)),
        (Value::Int64(1), Value::UniqueId(1)),
        (Value::Float64(1.0), Value::UniqueId(1)),
    ];
    for mode in [StorageMode::Memory, StorageMode::Mapped, StorageMode::Disk] {
        for (old_id, new_id) in kinds.clone() {
            let dir = tempfile::tempdir().unwrap();
            let fresh = versions(mode, &dir, old_id.clone(), new_id.clone());
            for capped in [false, true] {
                // A clone starts with a cold cache; capped, it builds no map
                // and the type walk keeps the same answer.
                let graph = fresh.clone();
                if capped {
                    set_byte_cap(&graph, 1);
                }
                for seek in SEEKS {
                    for (date, name) in [("2005-01-01", "old"), ("2020-01-01", "new")] {
                        let query = format!("FOR VALID_TIME AS OF date('{date}') {seek}");
                        assert_eq!(
                            names(&graph, &query),
                            [text(name)],
                            "{mode:?} {old_id:?}/{new_id:?} capped={capped}: {query}"
                        );
                    }
                }
            }
        }
    }
}

/// Two versions valid at once, one per numeric kind: the seek returns the
/// last in the type's node order whichever kind the query spells the id in
/// (the index alone answers each kind with its own node).
#[test]
fn among_valid_versions_of_mixed_kinds_the_last_in_node_order_wins() {
    for mode in [StorageMode::Memory, StorageMode::Mapped, StorageMode::Disk] {
        for (first, last) in [
            (Value::UniqueId(1), Value::Int64(1)),
            (Value::Int64(1), Value::UniqueId(1)),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut graph = empty(mode, &dir);
            for (id, name) in [(first.clone(), "first"), (last.clone(), "last")] {
                run(
                    &mut graph,
                    &format!(
                        "CREATE (:M {{id: $id, name: '{name}', \
                         vf: date('2000-01-01'), vt: date('2099-12-31')}})"
                    ),
                    &HashMap::from([("id".to_string(), id)]),
                );
            }
            run(
                &mut graph,
                "CALL db.temporal.declare({node: 'M', from: 'vf', to: 'vt', \
                 convention: 'closed'}) YIELD declared RETURN declared",
                &HashMap::new(),
            );
            for seek in &SEEKS[..2] {
                let query = format!("FOR VALID_TIME AS OF date('2020-01-01') {seek}");
                assert_eq!(
                    names(&graph, &query),
                    [text("last")],
                    "{mode:?} {first:?}/{last:?}: {query}"
                );
            }
        }
    }
}

/// A seek at an instant where the index's node is not valid consults the
/// duplicate-id map and admit-tests only the id's versions — never a walk
/// over the type — however many other nodes the type holds. Over the byte
/// cap it walks from the type's end (the counter can see a walk).
#[test]
fn a_seek_admit_tests_only_the_ids_versions() {
    let mut graph = DirGraph::new();
    let params = HashMap::new();
    run(
        &mut graph,
        "CREATE (:M {id: 7, name: 'v1', vf: date('2000-01-01'), vt: date('2004-12-31')}), \
         (:M {id: 7, name: 'v2', vf: date('2005-01-01'), vt: date('2009-12-31')}), \
         (:M {id: 7, name: 'v3', vf: date('2010-01-01')})",
        &params,
    );
    run(
        &mut graph,
        "UNWIND range(100, 1099) AS i \
         CREATE (:M {id: i, vf: date('2000-01-01'), vt: date('2099-12-31')})",
        &params,
    );
    run(
        &mut graph,
        "CALL db.temporal.declare({node: 'M', from: 'vf', to: 'vt', convention: 'closed'}) \
         YIELD declared RETURN declared",
        &params,
    );
    let seek = "FOR VALID_TIME AS OF date('2002-01-01') MATCH (m:M {id: 7}) RETURN m.name";
    assert_eq!(names(&graph, seek), [text("v1")]);
    seek_probe::take();
    assert_eq!(names(&graph, seek), [text("v1")]);
    let (admitted, walked) = seek_probe::take();
    assert_eq!(walked, 0, "the seek walked the type");
    assert!(admitted <= 3, "{admitted} admit tests for three versions");
    assert!(admitted >= 2, "the index's node, then the older versions");

    let capped = graph.clone();
    set_byte_cap(&capped, 1);
    seek_probe::take();
    assert_eq!(names(&capped, seek), [text("v1")]);
    let (_, walked) = seek_probe::take();
    assert!(
        walked > 1000,
        "over the cap the seek walks the type: {walked}"
    );
}

/// The map's heap is its array's capacity, and a build refuses before the
/// old and the grown buffer together would pass the budget.
#[test]
fn a_build_never_holds_more_than_its_budget() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "UNWIND range(1, 40) AS i CREATE (:M {id: i % 20})",
        &HashMap::new(),
    );
    let full = DuplicateIds::build(&graph, "M", usize::MAX).unwrap();
    assert_eq!(full.entries.len(), 40, "every node's id repeats");
    assert_eq!(full.bytes(), full.entries.capacity() * ENTRY_BYTES);
    for budget in 0..(4 * 64 * ENTRY_BYTES) {
        match DuplicateIds::build(&graph, "M", budget) {
            Some(map) => {
                assert!(map.bytes() <= budget, "{budget}");
                assert_eq!(map.entries.len(), 40);
            }
            // 16 → 32 → 64 slots: the last growth holds 32 + 64 at once.
            None => assert!(budget < (32 + 64) * ENTRY_BYTES, "{budget}"),
        }
    }
    assert!(DuplicateIds::build(&graph, "M", (32 + 64) * ENTRY_BYTES).is_some());
}
