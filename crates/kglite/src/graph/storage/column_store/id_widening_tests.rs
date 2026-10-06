//! A `UniqueId` column meeting an `Int64` widens to `Int64` instead of
//! demoting to `Mixed` (`TypedColumn::widened_for`), and an `Int64` or
//! `Float64` column takes a `UniqueId` as the same number. `add_nodes` stores
//! ids that fit a `u32` as `UniqueId`; a Cypher `CREATE` writes an `Int64` id,
//! so the first `CREATE` on a bulk-loaded type used to leave its id column a
//! 32 B/row heap `Mixed` column that cannot spill.

use super::tail::HEAP_TAIL_MIN_ROWS;
use super::{column_clones, reset_column_clones, ColumnStore};
use crate::datatypes::values::{ColumnData, ColumnType, DataFrame};
use crate::datatypes::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::schema::{InternedKey, StringInterner, TypeSchema};
use crate::graph::session::{execute_mut, execute_read, CommitOutcome, ExecuteOptions, Session};
use std::collections::HashMap;
use std::sync::Arc;

fn empty_store() -> ColumnStore {
    ColumnStore::new(
        Arc::new(TypeSchema::new()),
        &HashMap::new(),
        &StringInterner::new(),
    )
}

#[test]
fn a_uniqueid_id_column_widens_to_int64_instead_of_demoting_to_mixed() {
    let mut store = empty_store();
    for i in 0..3u32 {
        store.push_id(&Value::UniqueId(i));
        store.push_row(&[]);
    }
    store.push_id(&Value::Null);
    store.push_row(&[]);
    assert_eq!(store.id_type_str(), Some("uniqueid"));

    reset_column_clones();
    store.push_id(&Value::Int64(1 << 40));
    store.push_row(&[]);
    assert_eq!(store.id_type_str(), Some("int64"));
    assert_eq!(column_clones(), 1, "the widening copies the column once");

    // A later bulk id keeps the column typed.
    store.push_id(&Value::UniqueId(9));
    store.push_row(&[]);
    assert_eq!(store.id_type_str(), Some("int64"));

    let ids: Vec<_> = (0..6).map(|row| store.get_id(row)).collect();
    assert_eq!(
        ids,
        vec![
            Some(Value::Int64(0)),
            Some(Value::Int64(1)),
            Some(Value::Int64(2)),
            None,
            Some(Value::Int64(1 << 40)),
            Some(Value::Int64(9)),
        ]
    );
}

/// The same widening on a property column, on push and on set, and on a
/// title column; a `Float64` column takes a `UniqueId` as its exact float;
/// anything a typed shape cannot hold still lands in `Mixed`.
#[test]
fn uniqueid_property_and_title_columns_widen_to_int64() {
    let key = InternedKey::from_str("ref");
    let mut store = empty_store();
    store.push_title(&Value::UniqueId(4));
    store.push_row(&[(key, Value::UniqueId(4))]);
    assert_eq!(store.title_type_str(), Some("int64"));
    let slot = store.schema.slot(key).unwrap() as usize;
    assert_eq!(store.column_type_str(slot), Some("uniqueid"));

    store.push_title(&Value::Int64(-5));
    store.push_row(&[(key, Value::Int64(-5))]);
    assert_eq!(store.column_type_str(slot), Some("int64"));
    assert_eq!(store.title_type_str(), Some("int64"));
    assert_eq!(store.get(0, key), Some(Value::Int64(4)));
    assert_eq!(store.get(1, key), Some(Value::Int64(-5)));

    let mut set_store = empty_store();
    set_store.push_row(&[(key, Value::UniqueId(1))]);
    set_store.push_row(&[(key, Value::UniqueId(2))]);
    assert!(set_store.set(1, key, &Value::Int64(-2), None));
    let slot = set_store.schema.slot(key).unwrap() as usize;
    assert_eq!(set_store.column_type_str(slot), Some("int64"));
    assert_eq!(set_store.get(0, key), Some(Value::Int64(1)));
    assert_eq!(set_store.get(1, key), Some(Value::Int64(-2)));
    assert!(set_store.set(0, key, &Value::UniqueId(3), None));
    assert_eq!(set_store.column_type_str(slot), Some("int64"));
    assert_eq!(set_store.get(0, key), Some(Value::Int64(3)));

    assert!(set_store.set(0, key, &Value::String("x".into()), None));
    assert_eq!(set_store.column_type_str(slot), Some("mixed"));
    assert_eq!(set_store.get(1, key), Some(Value::Int64(-2)));

    let mut float_store = empty_store();
    float_store.push_row(&[(key, Value::Float64(1.5))]);
    float_store.push_row(&[(key, Value::UniqueId(7))]);
    let slot = float_store.schema.slot(key).unwrap() as usize;
    assert_eq!(float_store.column_type_str(slot), Some("float64"));
    assert_eq!(float_store.get(1, key), Some(Value::Float64(7.0)));
    assert!(float_store.set(0, key, &Value::UniqueId(u32::MAX), None));
    assert_eq!(
        float_store.get(0, key),
        Some(Value::Float64(f64::from(u32::MAX)))
    );
    assert_eq!(float_store.column_type_str(slot), Some("float64"));
}

/// A title `set` a `Str` title column cannot hold still lands in `Mixed`,
/// keeping the value as given.
#[test]
fn a_set_title_no_typed_shape_holds_demotes_to_mixed() {
    let mut store = empty_store();
    store.push_id(&Value::UniqueId(1));
    store.push_title(&Value::String("a".into()));
    store.push_row(&[]);
    assert!(store.set_title(0, &Value::UniqueId(3)));
    assert_eq!(store.title_type_str(), Some("mixed"));
    assert_eq!(store.get_title(0), Some(Value::UniqueId(3)));
}

fn bulk_loaded(rows: u32) -> DirGraph {
    let mut graph = DirGraph::new();
    let mut df = DataFrame::new(Vec::new());
    df.add_column(
        "uid".into(),
        ColumnType::UniqueId,
        ColumnData::UniqueId((0..rows).map(Some).collect()),
    )
    .unwrap();
    crate::graph::mutation::maintain::add_nodes(
        &mut graph,
        df,
        "Item".into(),
        "uid".into(),
        None,
        None,
    )
    .unwrap();
    graph
}

fn id_kind(graph: &DirGraph) -> Option<&'static str> {
    graph.column_store("Item").unwrap().id_type_str()
}

fn count(graph: &DirGraph, query: &str) -> Value {
    let params = HashMap::new();
    execute_read(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
        .rows[0][0]
        .clone()
}

/// Every id, old and new, is still found by its id after the column widened.
fn assert_ids_found(graph: &DirGraph, ids: &[i64]) {
    for id in ids {
        assert_eq!(
            count(
                graph,
                &format!("MATCH (n:Item {{uid: {id}}}) RETURN count(n)")
            ),
            Value::Int64(1),
            "id {id}"
        );
    }
}

/// End to end: an `add_nodes` type with compact ids stays typed through a
/// Cypher `CREATE` in a transaction — below and above the heap-tail floor, so
/// both the shared-column copy and the tail fold are crossed — and through a
/// save and load.
#[test]
fn a_create_in_a_transaction_on_a_bulk_loaded_type_keeps_its_id_column_typed() {
    for rows in [50, HEAP_TAIL_MIN_ROWS + 10] {
        let graph = bulk_loaded(rows);
        assert_eq!(id_kind(&graph), Some("uniqueid"));
        let session = Session::new(graph);
        let created = i64::from(rows) + 1_000;
        let mut tx = session.begin();
        let params = HashMap::new();
        execute_mut(
            tx.working_mut().unwrap(),
            &format!("CREATE (:Item {{uid: {created}}})"),
            &ExecuteOptions::eager(&params),
        )
        .unwrap();
        assert!(matches!(
            session.commit(tx, true),
            CommitOutcome::Committed { .. }
        ));
        let published = session.snapshot();
        let ids = [0, 3, i64::from(rows) - 1, created];
        assert_eq!(id_kind(&published), Some("int64"), "{rows} rows");
        assert_ids_found(&published, &ids);

        let mut bytes = Vec::new();
        crate::graph::io::file::write_kgl_to(&published, &mut bytes).unwrap();
        let loaded = crate::graph::io::file::load_kgl_bytes(&bytes).unwrap();
        assert_eq!(id_kind(&loaded), Some("int64"), "{rows} rows, loaded");
        assert_ids_found(&loaded, &ids);
    }
}

/// The reverse order: a type a `CREATE` started holds `Int64` ids, and a bulk
/// load of compact ids into it pushes `UniqueId` values the column now takes.
#[test]
fn a_bulk_load_into_a_created_type_keeps_its_int64_id_column() {
    let mut graph = DirGraph::new();
    let params = HashMap::new();
    execute_mut(
        &mut graph,
        "CREATE (:Item {id: 100000})",
        &ExecuteOptions::eager(&params),
    )
    .unwrap();
    assert_eq!(id_kind(&graph), Some("int64"));
    let mut df = DataFrame::new(Vec::new());
    df.add_column(
        "id".into(),
        ColumnType::UniqueId,
        ColumnData::UniqueId((0..20).map(Some).collect()),
    )
    .unwrap();
    crate::graph::mutation::maintain::add_nodes(
        &mut graph,
        df,
        "Item".into(),
        "id".into(),
        None,
        None,
    )
    .unwrap();
    assert_eq!(id_kind(&graph), Some("int64"));
    for id in [0, 19, 100000] {
        assert_eq!(
            count(
                &graph,
                &format!("MATCH (n:Item {{id: {id}}}) RETURN count(n)")
            ),
            Value::Int64(1),
            "id {id}"
        );
    }
}

/// Recovery reproduces the live state: a checkpoint of a bulk-loaded type
/// with compact ids and a compact-id property, then a `CREATE` writing an
/// `Int64` into both, replayed from the WAL after a crash, leaves the same
/// column types and values the live graph held.
#[test]
fn wal_replay_widens_the_id_column_as_the_live_graph_did() {
    use crate::graph::io::file::{load_file, save_graph};
    use crate::graph::wal::DurabilityLevel;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.kgl").to_string_lossy().into_owned();
    {
        let mut graph = DirGraph::new();
        let mut df = DataFrame::new(Vec::new());
        for column in ["uid", "ref"] {
            df.add_column(
                column.into(),
                ColumnType::UniqueId,
                ColumnData::UniqueId((0..50).map(Some).collect()),
            )
            .unwrap();
        }
        crate::graph::mutation::maintain::add_nodes(
            &mut graph,
            df,
            "Item".into(),
            "uid".into(),
            None,
            None,
        )
        .unwrap();
        save_graph(&mut Arc::new(graph), &path).unwrap();
    }
    let open = || Session::open_durable(load_file(&path).unwrap(), &path, DurabilityLevel::Full);
    let shape = |graph: &DirGraph| {
        let store = graph.column_store("Item").unwrap();
        let slot = store.schema.slot(InternedKey::from_str("ref")).unwrap() as usize;
        let refs = count(
            graph,
            "MATCH (n:Item) WHERE n.uid IN [3, 7000] RETURN collect(n.ref)",
        );
        (store.id_type_str(), store.column_type_str(slot), refs)
    };
    let live = {
        let session = open().unwrap();
        assert_eq!(shape(&session.snapshot()).0, Some("uniqueid"));
        let mut tx = session.begin();
        let params = HashMap::new();
        execute_mut(
            tx.working_mut().unwrap(),
            "CREATE (:Item {uid: 7000, ref: -5})",
            &ExecuteOptions::eager(&params),
        )
        .unwrap();
        assert!(matches!(
            session.commit(tx, true),
            CommitOutcome::Committed { .. }
        ));
        shape(&session.snapshot())
    };
    assert_eq!(
        live,
        (
            Some("int64"),
            Some("int64"),
            Value::List(vec![Value::Int64(3), Value::Int64(-5)])
        )
    );
    let recovered = open().unwrap();
    assert_eq!(shape(&recovered.snapshot()), live);
}
