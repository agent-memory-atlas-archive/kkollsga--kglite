//! A saved disk directory must be one this build can read back.
//!
//! Disk snapshots address types by `InternedKey` hash in `id_indices.bin` and
//! `type_indices.bin`, and the loader resolves every one of those hashes
//! through the `interner.bin.zst` sidecar written beside them. Any type name
//! that reaches an index without reaching the interner therefore produces a
//! directory that `save()` writes happily and `load()` rejects with
//! "invalid id_indices.bin: directory contains an unresolved type key".
//!
//! Two shapes used to do exactly that, both of them ordinary usage:
//!
//! * a node type declared with **zero data rows** — a real data slice
//!   legitimately leaves declared types empty — because `add_nodes` built an
//!   (empty) id index for the type while only node *creation* interned its
//!   name; and
//! * a **label a read-only query merely mentioned** (`MATCH (n:Ghost {id: 1})`),
//!   because the read path caches a build-on-miss index under that name.
//!
//! The tests below pin the round trip for both, with the in-memory and
//! one-row variants as controls.

use crate::datatypes::{DataFrame, Value};
use crate::graph::dir_graph::DirGraph;
use crate::graph::io::file::{load_file, save_graph};
use crate::graph::mutation::maintain::add_nodes;
use crate::graph::session::{execute_read, ExecuteOptions};
use std::sync::Arc;

/// `id,name` rows, matching the two-column CSV shape a blueprint declares.
fn rows(count: usize) -> DataFrame {
    let rows: Vec<Vec<Value>> = (0..count)
        .map(|i| {
            vec![
                Value::Int64(i as i64 + 1),
                Value::String(format!("row-{i}")),
            ]
        })
        .collect();
    DataFrame::from_cypher_rows(vec!["id".to_string(), "name".to_string()], rows).unwrap()
}

fn declare(graph: &mut DirGraph, node_type: &str, row_count: usize) {
    add_nodes(
        graph,
        rows(row_count),
        node_type.to_string(),
        "id".to_string(),
        Some("name".to_string()),
        None,
    )
    .unwrap_or_else(|e| panic!("add_nodes({node_type}, {row_count} rows) failed: {e}"));
}

fn count_of_type(graph: &DirGraph, node_type: &str) -> i64 {
    let params = std::collections::HashMap::new();
    let opts = ExecuteOptions::new(&params);
    let query = format!("MATCH (n:{node_type}) RETURN count(n) AS c");
    let outcome = execute_read(graph, &query, &opts).expect("count query");
    match outcome.result.rows.first().and_then(|row| row.first()) {
        Some(Value::Int64(n)) => *n,
        other => panic!("unexpected count value: {other:?}"),
    }
}

/// Save `graph` as a disk directory and read it back.
fn disk_round_trip(mut graph: DirGraph, dir: &std::path::Path) -> Arc<DirGraph> {
    graph.enable_disk_mode().expect("enable disk mode");
    let mut handle = Arc::new(graph);
    save_graph(&mut handle, dir.to_str().unwrap()).expect("save disk graph");
    load_file(dir.to_str().unwrap()).expect("a saved disk directory must load")
}

/// The reported bug: one populated type, one declared type with no rows.
#[test]
fn declared_type_with_zero_rows_survives_a_disk_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let mut graph = DirGraph::new();
    declare(&mut graph, "Full", 2);
    declare(&mut graph, "Empty", 0);

    let loaded = disk_round_trip(graph, &tmp.path().join("graph"));

    assert!(
        loaded.get_node_type_metadata("Empty").is_some(),
        "the declared-but-unpopulated type must survive the round trip"
    );
    assert_eq!(count_of_type(&loaded, "Full"), 2);
    assert_eq!(count_of_type(&loaded, "Empty"), 0);
}

/// Consumers declare tens of types and populate a handful; every unpopulated
/// one used to add another unresolvable key to the same directory.
#[test]
fn many_declared_types_with_zero_rows_survive_a_disk_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let mut graph = DirGraph::new();
    declare(&mut graph, "Full", 3);
    for name in ["Filing", "Insider", "Holding", "Transaction", "Rating"] {
        declare(&mut graph, name, 0);
    }

    let loaded = disk_round_trip(graph, &tmp.path().join("graph"));

    assert_eq!(count_of_type(&loaded, "Full"), 3);
    for name in ["Filing", "Insider", "Holding", "Transaction", "Rating"] {
        assert!(
            loaded.get_node_type_metadata(name).is_some(),
            "declared type '{name}' must survive the round trip"
        );
        assert_eq!(count_of_type(&loaded, name), 0);
    }
}

/// Control: the same shape in memory always worked, and must keep working.
#[test]
fn declared_type_with_zero_rows_survives_a_kgl_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("graph.kgl");
    let mut graph = DirGraph::new();
    declare(&mut graph, "Full", 2);
    declare(&mut graph, "Empty", 0);

    let mut handle = Arc::new(graph);
    save_graph(&mut handle, path.to_str().unwrap()).expect("save .kgl");
    let loaded = load_file(path.to_str().unwrap()).expect("load .kgl");

    assert!(loaded.get_node_type_metadata("Empty").is_some());
    assert_eq!(count_of_type(&loaded, "Full"), 2);
    assert_eq!(count_of_type(&loaded, "Empty"), 0);
}

/// Control: one row is enough to intern the type name, so this shape was
/// never broken — it guards against a fix that only moved the boundary.
#[test]
fn single_row_type_survives_a_disk_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let mut graph = DirGraph::new();
    declare(&mut graph, "Full", 2);
    declare(&mut graph, "Sparse", 1);

    let loaded = disk_round_trip(graph, &tmp.path().join("graph"));

    assert_eq!(count_of_type(&loaded, "Full"), 2);
    assert_eq!(count_of_type(&loaded, "Sparse"), 1);
}

/// A read-only query naming a type the graph does not have caches an empty id
/// index under that name (`IdIndexStore::lookup_or_build`). Saving afterwards
/// must not carry that name into the directory — it has no interned identity,
/// so the snapshot could not resolve it back.
#[test]
fn a_label_only_mentioned_by_a_query_does_not_poison_the_next_save() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("graph");
    let mut graph = DirGraph::new();
    declare(&mut graph, "Full", 2);
    graph.enable_disk_mode().expect("enable disk mode");

    let params = std::collections::HashMap::new();
    let opts = ExecuteOptions::new(&params);
    execute_read(&graph, "MATCH (n:Ghost {id: 1}) RETURN n", &opts).expect("ghost query");
    assert!(
        graph.id_indices.contains_key("Ghost"),
        "precondition: the read path caches an index for the mentioned label"
    );

    let mut handle = Arc::new(graph);
    save_graph(&mut handle, dir.to_str().unwrap()).expect("save disk graph");
    let loaded = load_file(dir.to_str().unwrap()).expect("a saved disk directory must load");
    assert_eq!(count_of_type(&loaded, "Full"), 2);
}

/// `(name)` of the `M` node each id seek returns: a parameter (`Int64`) and a
/// bound `UniqueId` from another node.
fn m_seeks(graph: &DirGraph) -> [Vec<Value>; 2] {
    let params = std::collections::HashMap::from([("p".to_string(), Value::Int64(1))]);
    let opts = ExecuteOptions::new(&params);
    [
        "MATCH (n:M {id: $p}) RETURN n.name",
        "MATCH (a:A {id: 1}) MATCH (n:M {id: a.id}) RETURN n.name",
    ]
    .map(|query| {
        execute_read(graph, query, &opts)
            .unwrap_or_else(|e| panic!("{query}: {e}"))
            .result
            .rows
            .into_iter()
            .map(|mut row| row.remove(0))
            .collect()
    })
}

/// Through 0.18.1 a type holding one id in two numeric kinds — an `add_nodes`
/// `UniqueId(1)` beside a Cypher-created `Int64(1)` — was persisted with one
/// index entry per spelling, so `{id: $p}` and `{id: a.id}` answered with
/// different nodes. Such a directory is healed on load (the node last in the
/// type's order answers every spelling, as a fresh index does), and the next
/// save no longer writes the two-spelling entry back.
#[test]
fn an_id_index_saved_with_two_spellings_of_one_id_is_healed_on_load() {
    use crate::graph::schema::TypeIdIndex;
    use crate::graph::session::execute_mut;
    use crate::graph::storage::disk::generation::resolve_snapshot;
    use crate::graph::storage::disk::id_index::{write_id_indices_bin, IdIndexBase, IdIndexStore};
    use crate::graph::storage::disk::type_index::TypeIndexStore;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("graph");
    let mut graph = DirGraph::new();
    for (query, id) in [
        ("CREATE (:A {id: $id})", Value::UniqueId(1)),
        ("CREATE (:M {id: $id, name: 'first'})", Value::UniqueId(1)),
        ("CREATE (:M {id: $id, name: 'last'})", Value::Int64(1)),
    ] {
        let params = std::collections::HashMap::from([("id".to_string(), id)]);
        execute_mut(&mut graph, query, &ExecuteOptions::eager(&params)).unwrap();
    }
    let loaded = disk_round_trip(graph, &dir);
    let members = loaded.type_indices.get("M").unwrap().to_vec();
    let interner = loaded.interner.clone();
    drop(loaded);

    // The 0.18.1 shape, written past the writer's repeat check.
    let snapshot = resolve_snapshot(&dir).unwrap().snapshot_dir;
    let mut old = IdIndexStore::default();
    old.replace_with(std::collections::HashMap::from([(
        "M".to_string(),
        TypeIdIndex::General(rustc_hash::FxHashMap::from_iter([
            (Value::UniqueId(1), members[0]),
            (Value::Int64(1), members[1]),
        ])),
    )]));
    write_id_indices_bin(&snapshot, &old, &TypeIndexStore::default(), &interner).unwrap();

    let last = vec![Value::String("last".into())];
    let healed = load_file(dir.to_str().unwrap()).unwrap();
    assert_eq!(m_seeks(&healed), [last.clone(), last.clone()]);

    let mut handle = healed;
    save_graph(&mut handle, dir.to_str().unwrap()).unwrap();
    drop(handle);
    let snapshot = resolve_snapshot(&dir).unwrap().snapshot_dir;
    let base = IdIndexBase::load_from(&snapshot, &interner)
        .unwrap()
        .unwrap();
    // Its ids repeat, so the save leaves the index to the reload's rebuild
    // from the type's node order rather than persisting a spelling per node.
    assert!(
        !base.contains("M"),
        "the two-spelling index was written back"
    );
    let reloaded = load_file(dir.to_str().unwrap()).unwrap();
    assert_eq!(m_seeks(&reloaded), [last.clone(), last]);
}
