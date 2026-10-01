//! Appending to a type a reopened disk graph serves from its column file costs
//! the new rows: the base is neither copied nor written, the type's index bucket
//! and id index stay on the file, and a failed statement leaves the type as a
//! pure file-backed store again.
use super::DirGraph;
use crate::datatypes::{DataFrame, Value};
use crate::graph::io::file::load_file;
use crate::graph::mutation::maintain;
use crate::graph::session::execute::{execute_mut, ExecuteOptions};
use crate::graph::storage::column_store::{column_clones, flattens, reset_column_clones};
use crate::graph::storage::disk::id_index::full_maps_built;
use crate::graph::storage::disk::type_index::{buckets_materialized, TypeNodesRef};
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

const STAFF: i64 = 30_000;

fn run(graph: &mut DirGraph, query: &str) -> Result<Vec<Vec<Value>>, String> {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .map(|out| out.result.rows)
        .map_err(|e| e.to_string())
}

fn add_staff(graph: &mut DirGraph, from: i64, to: i64) {
    let rows = (from..to)
        .map(|i| {
            vec![
                Value::Int64(3_000_000_000_000 + i),
                Value::String(format!("Employee {i}")),
                Value::Int64(i % 9),
            ]
        })
        .collect();
    let frame = DataFrame::from_cypher_rows(vec!["id".into(), "name".into(), "level".into()], rows)
        .unwrap();
    maintain::add_nodes(
        graph,
        frame,
        "Staff".into(),
        "id".into(),
        Some("name".into()),
        None,
    )
    .unwrap();
}

fn reopened(dir: &TempDir) -> DirGraph {
    let path = dir.path().join("staff");
    let path = path.to_str().unwrap();
    let mut graph = DirGraph::new();
    graph.enable_disk_mode().unwrap();
    add_staff(&mut graph, 0, STAFF);
    graph.save_disk(path).unwrap();
    drop(graph);
    match Arc::try_unwrap(load_file(path).unwrap()) {
        Ok(graph) => graph,
        Err(_) => panic!("fresh load unexpectedly shared"),
    }
}

fn count(graph: &mut DirGraph) -> i64 {
    match run(graph, "MATCH (s:Staff) RETURN count(s) AS c").unwrap()[0][0] {
        Value::Int64(c) => c,
        ref other => panic!("count: {other:?}"),
    }
}

#[test]
fn a_bulk_append_to_a_reopened_type_copies_nothing_the_type_holds() {
    let dir = TempDir::new().unwrap();
    let mut graph = reopened(&dir);
    assert!(graph.column_store("Staff").unwrap().has_mmap_base());
    let (maps, buckets) = (full_maps_built(), buckets_materialized());
    reset_column_clones();

    add_staff(&mut graph, STAFF, STAFF + 1_000);

    assert_eq!(column_clones(), 0, "an existing column was deep-copied");
    assert_eq!(full_maps_built(), maps, "the id index was copied to a map");
    assert_eq!(
        buckets_materialized(),
        buckets,
        "the type's mapped bucket was copied onto the heap"
    );
    let store = graph.column_store("Staff").unwrap();
    assert!(store.has_mmap_base());
    assert_eq!(store.tail_rows(), 1_000);
    assert!(
        store.heap_bytes() < 100_000,
        "the heap holds the appended rows only: {} bytes",
        store.heap_bytes()
    );
    assert!(
        matches!(
            graph.type_indices.get("Staff"),
            Some(TypeNodesRef::MmapLayered { .. })
        ),
        "the bucket is the mapped payload plus the appended members"
    );
    assert_eq!(count(&mut graph), STAFF + 1_000);
}

#[test]
fn a_cypher_create_on_a_reopened_type_copies_nothing_the_type_holds() {
    let dir = TempDir::new().unwrap();
    let mut graph = reopened(&dir);
    let (maps, buckets) = (full_maps_built(), buckets_materialized());
    reset_column_clones();

    run(
        &mut graph,
        "UNWIND range(1, 500) AS i CREATE (:Staff {id: 4000000000000 + i, name: 'New ' + toString(i), level: i % 9})",
    )
    .unwrap();

    assert_eq!(column_clones(), 0);
    assert_eq!(full_maps_built(), maps);
    assert_eq!(buckets_materialized(), buckets);
    assert_eq!(graph.column_store("Staff").unwrap().tail_rows(), 500);
    assert_eq!(
        run(
            &mut graph,
            "MATCH (s:Staff {id: 4000000000123}) RETURN s.name AS n, s.level AS l"
        )
        .unwrap(),
        vec![vec![Value::String("New 123".into()), Value::Int64(123 % 9)]]
    );
    assert_eq!(
        run(
            &mut graph,
            "MATCH (s:Staff {id: 3000000000077}) RETURN s.name AS n"
        )
        .unwrap(),
        vec![vec![Value::String("Employee 77".into())]]
    );
    assert_eq!(count(&mut graph), STAFF + 500);
}

#[test]
fn a_failed_create_on_a_reopened_type_leaves_a_pure_file_backed_store() {
    let dir = TempDir::new().unwrap();
    let mut graph = reopened(&dir);
    let error = run(
        &mut graph,
        "UNWIND range(1, 300) AS i CREATE (:Staff {id: 4000000000000 + i, name: 'x', \
         level: CASE WHEN i = 250 THEN duration({months: 2147483648}) ELSE i END})",
    );
    assert!(error.is_err(), "the statement must fail after writing rows");
    let store = graph.column_store("Staff").unwrap();
    assert_eq!(store.row_count(), STAFF as u32);
    assert!(
        store.pure_mmap_store().is_some(),
        "the rolled-back append left no tail behind"
    );
    assert!(matches!(
        graph.type_indices.get("Staff"),
        Some(TypeNodesRef::Mmap(_))
    ));
    assert_eq!(count(&mut graph), STAFF);

    // The rows the failed statement vacated are the ones the next append takes.
    run(
        &mut graph,
        "CREATE (:Staff {id: 4000000000001, name: 'again', level: 1})",
    )
    .unwrap();
    let store = graph.column_store("Staff").unwrap();
    assert_eq!(store.row_count(), STAFF as u32 + 1);
    assert_eq!(count(&mut graph), STAFF + 1);
}

#[test]
fn an_append_survives_two_save_and_reopen_cycles_without_leaving_the_file() {
    let dir = TempDir::new().unwrap();
    let mut graph = reopened(&dir);
    let path = dir.path().join("staff");
    let path = path.to_str().unwrap();
    let mut expected = STAFF;
    for cycle in 0..2 {
        let from = STAFF + cycle * 700;
        add_staff(&mut graph, from, from + 700);
        expected += 700;
        run(
            &mut graph,
            &format!(
                "MATCH (s:Staff {{id: {}}}) SET s.level = 99",
                3_000_000_000_000 + from + 5
            ),
        )
        .unwrap();
        let flattened = flattens();
        graph.save_disk(path).unwrap();
        assert_eq!(
            flattens(),
            flattened,
            "the save wrote base and tail regions; it did not flatten the type (cycle {cycle})"
        );
        let store = graph.column_store("Staff").unwrap();
        assert!(
            store.pure_mmap_store().is_some(),
            "the save re-points the store at the file it published (cycle {cycle})"
        );
        drop(graph);
        graph = match Arc::try_unwrap(load_file(path).unwrap()) {
            Ok(graph) => graph,
            Err(_) => panic!("fresh load unexpectedly shared"),
        };
        assert_eq!(count(&mut graph), expected);
        assert_eq!(
            run(
                &mut graph,
                &format!(
                    "MATCH (s:Staff {{id: {}}}) RETURN s.level AS l, s.name AS n",
                    3_000_000_000_000 + from + 5
                )
            )
            .unwrap(),
            vec![vec![
                Value::Int64(99),
                Value::String(format!("Employee {}", from + 5))
            ]]
        );
    }
}

/// Register-shaped versions (integer id and title, two timestamps one of which
/// is often null, a category, an integer): the columns a real register appends
/// to. The save must still write regions, including when an appended batch
/// carries no value for a column at all.
#[test]
fn a_register_shaped_append_saves_without_flattening_the_type() {
    use chrono::NaiveDate;
    let stamp = |i: i64| {
        Value::Timestamp(
            NaiveDate::from_ymd_opt(2001, 1, 1)
                .unwrap()
                .and_hms_micro_opt(1, 2, 3, (i % 1000) as u32)
                .unwrap()
                + chrono::Duration::days(i % 4000),
        )
    };
    let versions = |from: i64, to: i64, closed: bool| {
        let rows = (from..to)
            .map(|i| {
                vec![
                    Value::Int64(3_200_000_000_000 + i),
                    Value::Int64(3_100_000_000_000 + i / 3),
                    stamp(i),
                    if closed && i % 3 == 0 {
                        stamp(i + 5000)
                    } else {
                        Value::Null
                    },
                    Value::String(["active", "terminated"][(i % 2) as usize].into()),
                    Value::Int64(1900 + i % 100),
                ]
            })
            .collect();
        DataFrame::from_cypher_rows(
            vec![
                "id".into(),
                "ident".into(),
                "valid_from".into(),
                "valid_to".into(),
                "status".into(),
                "hire_year".into(),
            ],
            rows,
        )
        .unwrap()
    };
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("register");
    let path = path.to_str().unwrap();
    let mut graph = DirGraph::new();
    graph.enable_disk_mode().unwrap();
    maintain::add_nodes(
        &mut graph,
        versions(0, 20_000, true),
        "Employment".into(),
        "id".into(),
        Some("ident".into()),
        None,
    )
    .unwrap();
    graph.save_disk(path).unwrap();
    drop(graph);
    let mut graph = match Arc::try_unwrap(load_file(path).unwrap()) {
        Ok(graph) => graph,
        Err(_) => panic!("fresh load unexpectedly shared"),
    };
    // One batch with closed versions, one with every `valid_to` null.
    for (from, closed) in [(20_000, true), (20_500, false)] {
        maintain::add_nodes(
            &mut graph,
            versions(from, from + 500, closed),
            "Employment".into(),
            "id".into(),
            Some("ident".into()),
            None,
        )
        .unwrap();
    }
    assert_eq!(graph.column_store("Employment").unwrap().tail_rows(), 1_000);
    let flattened = flattens();
    graph.save_disk(path).unwrap();
    assert_eq!(
        flattens(),
        flattened,
        "a register-shaped type was flattened onto the heap by the save"
    );
    drop(graph);
    let mut graph = match Arc::try_unwrap(load_file(path).unwrap()) {
        Ok(graph) => graph,
        Err(_) => panic!("fresh load unexpectedly shared"),
    };
    let rows = run(
        &mut graph,
        "MATCH (p:Employment) WHERE p.valid_to IS NULL RETURN count(p) AS c",
    )
    .unwrap();
    let open = (0..21_000i64)
        .filter(|i| !(*i < 20_500 && i % 3 == 0))
        .count() as i64;
    assert_eq!(rows, vec![vec![Value::Int64(open)]]);
}

/// Closing a register's rows is a `SET` of a timestamp column over a mapped
/// type, and a save after it must not bring the whole type onto the heap: the
/// untouched columns are written from their mapping, the touched ones with the
/// `SET` cells laid over them.
#[test]
fn a_set_on_a_register_shaped_type_saves_without_flattening_it() {
    use chrono::NaiveDate;
    let stamp = |i: i64| {
        Value::Timestamp(
            NaiveDate::from_ymd_opt(2001, 1, 1)
                .unwrap()
                .and_hms_micro_opt(1, 2, 3, (i % 1000) as u32)
                .unwrap()
                + chrono::Duration::days(i % 4000),
        )
    };
    let rows = (0..20_000i64)
        .map(|i| {
            vec![
                Value::Int64(3_200_000_000_000 + i),
                Value::Int64(3_100_000_000_000 + i / 3),
                stamp(i),
                if i % 3 == 0 {
                    stamp(i + 5000)
                } else {
                    Value::Null
                },
                Value::String(["active", "terminated"][(i % 2) as usize].into()),
                Value::Int64(1900 + i % 100),
            ]
        })
        .collect();
    let frame = DataFrame::from_cypher_rows(
        vec![
            "id".into(),
            "ident".into(),
            "valid_from".into(),
            "valid_to".into(),
            "status".into(),
            "hire_year".into(),
        ],
        rows,
    )
    .unwrap();
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("register");
    let path = path.to_str().unwrap();
    let mut graph = DirGraph::new();
    graph.enable_disk_mode().unwrap();
    maintain::add_nodes(
        &mut graph,
        frame,
        "Employment".into(),
        "id".into(),
        Some("ident".into()),
        None,
    )
    .unwrap();
    graph.save_disk(path).unwrap();
    drop(graph);
    let mut graph = match Arc::try_unwrap(load_file(path).unwrap()) {
        Ok(graph) => graph,
        Err(_) => panic!("fresh load unexpectedly shared"),
    };

    // Close the open versions built in 1951, and mark those built in 1950 as
    // something else: a timestamp column and a string column, both overlaid.
    run(
        &mut graph,
        "MATCH (p:Employment) WHERE p.hire_year = 1951 AND p.valid_to IS NULL \
         SET p.valid_to = datetime('2031-01-01T00:00:00.000001')",
    )
    .unwrap();
    run(
        &mut graph,
        "MATCH (p:Employment) WHERE p.hire_year = 1950 SET p.status = 'onboarding_approved'",
    )
    .unwrap();
    assert_eq!(
        graph.column_store("Employment").unwrap().tail_rows(),
        0,
        "no row was appended: this is the overlay alone"
    );

    let flattened = flattens();
    graph.save_disk(path).unwrap();
    assert_eq!(
        flattens(),
        flattened,
        "a SET-changed register-shaped type was flattened onto the heap by the save"
    );
    drop(graph);
    let mut graph = match Arc::try_unwrap(load_file(path).unwrap()) {
        Ok(graph) => graph,
        Err(_) => panic!("fresh load unexpectedly shared"),
    };
    let open = (0..20_000i64)
        .filter(|i| i % 3 != 0 && i % 100 != 51)
        .count() as i64;
    assert_eq!(
        run(
            &mut graph,
            "MATCH (p:Employment) WHERE p.valid_to IS NULL RETURN count(p) AS c"
        )
        .unwrap(),
        vec![vec![Value::Int64(open)]]
    );
    let renamed = (0..20_000i64).filter(|i| i % 100 == 50).count() as i64;
    assert_eq!(
        run(
            &mut graph,
            "MATCH (p:Employment) WHERE p.status = 'onboarding_approved' RETURN count(p) AS c"
        )
        .unwrap(),
        vec![vec![Value::Int64(renamed)]]
    );
    // Cells the statements did not touch are as they were.
    assert_eq!(
        run(
            &mut graph,
            "MATCH (p:Employment) WHERE p.hire_year = 1952 AND p.status = 'active' RETURN count(p) AS c"
        )
        .unwrap(),
        vec![vec![Value::Int64(
            (0..20_000i64)
                .filter(|i| i % 100 == 52 && i % 2 == 0)
                .count() as i64
        )]]
    );
}
