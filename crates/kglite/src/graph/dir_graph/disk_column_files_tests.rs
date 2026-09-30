//! A disk save writes one immutable column file per node type, reads them back
//! through read-only maps, and moves the live handle onto the published files
//! without ever failing a save that already succeeded.
use super::DirGraph;
use crate::datatypes::{DataFrame, Value};
use crate::graph::io::columns_meta::{self, ColumnsMeta};
use crate::graph::io::file::{load_file, save_graph};
use crate::graph::mutation::maintain;
use crate::graph::session::execute::{execute_mut, ExecuteOptions};
use crate::graph::storage::column_store::TypedColumn;
use crate::graph::storage::disk::type_index::TypeNodesRef;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;

/// Org-chart data: `Employee` carries an integer badge number as its title.
fn add_employees(graph: &mut DirGraph, from: i64, to: i64) {
    let rows = (from..to)
        .map(|i| {
            vec![
                Value::Int64(i),
                Value::Int64(1_000_000 + i),
                Value::Int64(i * 3),
            ]
        })
        .collect();
    let frame =
        DataFrame::from_cypher_rows(vec!["id".into(), "badge".into(), "grade".into()], rows)
            .unwrap();
    maintain::add_nodes(
        graph,
        frame,
        "Employee".into(),
        "id".into(),
        Some("badge".into()),
        None,
    )
    .unwrap();
}

fn add_departments(graph: &mut DirGraph, node_type: &str, count: i64) {
    let rows = (0..count)
        .map(|i| vec![Value::Int64(i), Value::String(format!("{node_type}-{i}"))])
        .collect();
    let frame = DataFrame::from_cypher_rows(vec!["id".into(), "name".into()], rows).unwrap();
    maintain::add_nodes(
        graph,
        frame,
        node_type.into(),
        "id".into(),
        Some("name".into()),
        None,
    )
    .unwrap();
}

fn run(graph: &mut DirGraph, query: &str) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
        .rows
}

fn load_owned(path: &str) -> DirGraph {
    match Arc::try_unwrap(load_file(path).unwrap()) {
        Ok(graph) => graph,
        Err(_) => panic!("fresh load unexpectedly shared"),
    }
}

fn current_generation(root: &str) -> PathBuf {
    let current = std::fs::read_to_string(format!("{root}/CURRENT")).unwrap();
    Path::new(root).join("generations").join(current.trim())
}

fn column_meta(root: &str) -> ColumnsMeta {
    columns_meta::read(&current_generation(root).join("seg_000/columns_meta.json")).unwrap()
}

/// The two-type graph every test starts from, saved at `path`.
fn saved_graph(path: &str) -> DirGraph {
    let mut graph = DirGraph::new();
    graph.enable_disk_mode().unwrap();
    add_employees(&mut graph, 0, 50_000);
    add_departments(&mut graph, "Department", 300);
    graph.save_disk(path).unwrap();
    graph
}

#[test]
fn int_titles_land_typed_in_their_own_file_and_the_live_store_stays_mapped() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    let graph = saved_graph(path);

    let meta = column_meta(path);
    let employee = meta
        .types
        .iter()
        .find(|t| t.type_name == "Employee")
        .unwrap();
    assert_eq!(
        (employee.title_offsets.len, employee.title_data.len),
        (0, 50_000 * 8),
        "an integer title is a bare i64 region with no offsets region"
    );
    assert_ne!(
        meta.files["Employee"], meta.files["Department"],
        "each type has its own file"
    );

    let store = graph.column_store("Employee").unwrap();
    assert!(
        store.pure_mmap_store().is_some(),
        "the save re-points the store at its published file"
    );
    assert_eq!(store.heap_bytes(), 0);
    assert_eq!(store.get_title(7), Some(Value::Int64(1_000_007)));
    drop(graph);

    let reloaded = load_owned(path);
    let store = reloaded.column_store("Employee").unwrap();
    assert_eq!(store.get_title(49_999), Some(Value::Int64(1_049_999)));
    assert_eq!(
        store.get(10, crate::graph::schema::InternedKey::from_str("grade")),
        Some(Value::Int64(30))
    );
}

#[test]
fn a_save_re_emits_a_mapped_store_from_its_mapping_instead_of_flattening_it() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    drop(saved_graph(path));
    let graph = load_owned(path);
    assert!(graph.column_store("Employee").unwrap().has_mmap_base());
    for (name, store) in graph.column_stores_for_save() {
        assert!(
            store.pure_mmap_store().is_some(),
            "{name} was flattened onto the heap for the save instead of re-emitted from its file"
        );
    }
    // One local change makes it a store with an overlay, which does flatten.
    let mut graph = graph;
    run(
        &mut graph,
        "MATCH (e:Employee) WHERE e.id = 3 SET e.grade = 1",
    );
    let stores = graph.column_stores_for_save();
    assert!(stores["Employee"].pure_mmap_store().is_none());
    assert!(stores["Department"].pure_mmap_store().is_some());
}

#[test]
fn a_string_property_index_over_a_mapped_store_indexes_that_property() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    let mut graph = DirGraph::new();
    graph.enable_disk_mode().unwrap();
    let rows = (0..200i64)
        .map(|i| {
            vec![
                Value::Int64(i),
                Value::String(format!("Unit {i}")),
                Value::String(format!("code-{i:03}")),
            ]
        })
        .collect();
    let frame =
        DataFrame::from_cypher_rows(vec!["id".into(), "name".into(), "code".into()], rows).unwrap();
    maintain::add_nodes(
        &mut graph,
        frame,
        "Unit".into(),
        "id".into(),
        Some("name".into()),
        None,
    )
    .unwrap();
    graph.save_disk(path).unwrap();
    assert!(graph.column_store("Unit").unwrap().has_mmap_base());

    let crate::graph::schema::GraphBackend::Disk(disk) = &mut graph.graph else {
        panic!("expected disk backend");
    };
    assert_eq!(disk.build_property_index("Unit", "code").unwrap(), 200);
    let hits = disk.lookup_property_eq("Unit", "code", "code-042").unwrap();
    assert_eq!(hits.len(), 1, "the index holds `code`, not the title");
    assert!(disk
        .lookup_property_eq("Unit", "code", "Unit 42")
        .unwrap()
        .is_empty());
}

#[test]
fn an_append_to_a_mapped_type_is_file_backed_and_survives_the_next_save() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    let mut graph = saved_graph(path);

    add_employees(&mut graph, 50_000, 50_100);
    {
        let store = graph.column_store("Employee").unwrap();
        assert!(!store.has_mmap_base(), "the appended store is owned");
        assert!(
            store
                .columns_ref()
                .all(|c| !matches!(c, TypedColumn::Mixed { .. })),
            "no Mixed column after the append"
        );
        // One-byte null columns sit under the mmap threshold at this size and
        // stay on the heap; the 8-byte data arrays must not.
        assert!(
            store.heap_bytes() <= 50_100 * 4,
            "the appended store is file-backed, not a heap copy: {} heap bytes",
            store.heap_bytes()
        );
    }
    graph.save_disk(path).unwrap();
    drop(graph);

    let reloaded = load_owned(path);
    let store = reloaded.column_store("Employee").unwrap();
    assert!(store.has_mmap_base());
    assert_eq!(store.row_count(), 50_100);
    assert_eq!(store.get_title(50_050), Some(Value::Int64(1_050_050)));
    assert_eq!(store.get_title(3), Some(Value::Int64(1_000_003)));
}

#[test]
fn an_append_that_cannot_spill_returns_the_error_and_keeps_the_store() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    let mut graph = saved_graph(path);
    // A file where the spill directory has to be: `create_dir_all` cannot mint it.
    let spill = graph
        .graph
        .as_disk()
        .and_then(|disk| disk.append_spill_dir());
    assert!(spill.is_none(), "no workspace before the first mutation");
    graph.prepare_mutation().unwrap();
    let spill = graph
        .graph
        .as_disk()
        .and_then(|disk| disk.append_spill_dir())
        .expect("a workspace exists once a mutation has begun");
    std::fs::create_dir_all(spill.parent().unwrap()).unwrap();
    std::fs::write(&spill, b"in the way").unwrap();

    let rows = (50_000..50_010)
        .map(|i| vec![Value::Int64(i), Value::Int64(i), Value::Int64(i)])
        .collect();
    let frame =
        DataFrame::from_cypher_rows(vec!["id".into(), "badge".into(), "grade".into()], rows)
            .unwrap();
    let error = maintain::add_nodes(
        &mut graph,
        frame,
        "Employee".into(),
        "id".into(),
        Some("badge".into()),
        None,
    )
    .expect_err("the spill failure must surface, not be swallowed");
    assert!(error.contains("file-backed columns"), "{error}");

    let store = graph
        .column_store("Employee")
        .expect("the store the failed append took out is back in the graph");
    assert_eq!(store.row_count(), 50_000);
    assert_eq!(store.get_title(7), Some(Value::Int64(1_000_007)));
}

#[test]
fn a_published_column_file_is_never_written_by_a_later_set_and_save() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    let mut graph = saved_graph(path);
    let first = current_generation(path);
    let files_of = |generation: &Path| -> Vec<(PathBuf, Vec<u8>)> {
        let meta = columns_meta::read(&generation.join("seg_000/columns_meta.json")).unwrap();
        meta.files
            .values()
            .map(|relative| {
                let file = generation.join("seg_000").join(relative);
                let bytes = std::fs::read(&file).unwrap();
                (file, bytes)
            })
            .collect()
    };
    let before = files_of(&first);
    assert!(before.len() >= 2 && before.iter().all(|(_, bytes)| !bytes.is_empty()));

    run(
        &mut graph,
        "MATCH (e:Employee) WHERE e.id < 100 SET e.grade = -1",
    );
    graph.save_disk(path).unwrap();
    assert_ne!(
        current_generation(path),
        first,
        "the save published a new generation"
    );
    // A second cycle from the new generation, with another write.
    run(
        &mut graph,
        "MATCH (e:Employee) WHERE e.id < 10 SET e.grade = -2",
    );
    graph.save_disk(path).unwrap();

    let after = files_of(&first);
    assert_eq!(
        before, after,
        "the first generation's files are byte-for-byte unchanged"
    );
    assert_eq!(
        run(
            &mut graph,
            "MATCH (e:Employee) WHERE e.id IN [5, 50, 500] RETURN e.grade AS g ORDER BY e.id"
        ),
        vec![
            vec![Value::Int64(-2)],
            vec![Value::Int64(-1)],
            vec![Value::Int64(1500)]
        ]
    );
}

#[test]
fn a_failed_remap_does_not_fail_a_published_save() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    let mut graph = saved_graph(path);
    run(
        &mut graph,
        "MATCH (e:Employee) WHERE e.id < 100 SET e.grade = -1",
    );
    let before = current_generation(path);

    let result = super::with_failing_stage("remap_column_stores", || graph.save_disk(path));
    assert!(
        result.is_ok(),
        "a post-publish optimisation failed the save: {result:?}"
    );
    assert_ne!(
        current_generation(path),
        before,
        "the generation was published"
    );

    // The live stores are the in-memory ones, and still right.
    let store = graph.column_store("Employee").unwrap();
    assert!(
        store.pure_mmap_store().is_none(),
        "the failed remap left the live store alone"
    );
    assert_eq!(
        run(
            &mut graph,
            "MATCH (e:Employee) WHERE e.id = 5 RETURN e.grade AS g"
        ),
        vec![vec![Value::Int64(-1)]]
    );
    // The next save works and the published data is what was written.
    graph.save_disk(path).unwrap();
    drop(graph);
    let mut reloaded = load_owned(path);
    assert_eq!(
        run(
            &mut reloaded,
            "MATCH (e:Employee) WHERE e.id = 5 RETURN e.grade AS g"
        ),
        vec![vec![Value::Int64(-1)]]
    );
}

#[test]
fn a_save_after_an_append_leaves_no_heap_type_index_and_a_failed_rebase_is_not_an_error() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    let mut graph = saved_graph(path);
    add_employees(&mut graph, 50_000, 51_000);
    assert!(
        matches!(
            graph.type_indices.get("Employee"),
            Some(TypeNodesRef::Overlay(_))
        ),
        "an append grows the heap overlay"
    );

    let result = super::with_failing_stage("rebase_type_indices", || graph.save_disk(path));
    assert!(result.is_ok(), "{result:?}");
    assert!(
        matches!(
            graph.type_indices.get("Employee"),
            Some(TypeNodesRef::Overlay(_))
        ),
        "the failed rebase kept the overlay"
    );

    add_employees(&mut graph, 51_000, 51_010);
    graph.save_disk(path).unwrap();
    match graph.type_indices.get("Employee") {
        Some(TypeNodesRef::Mmap(bytes)) => assert_eq!(bytes.len(), 51_010 * 4),
        Some(_) => panic!("the type index still holds a heap copy after the save"),
        None => panic!("the type index lost Employee"),
    }
    assert_eq!(
        graph.type_indices.get("Department").map(|n| n.len()),
        Some(300)
    );
    assert_eq!(
        run(&mut graph, "MATCH (e:Employee) RETURN count(e) AS c"),
        vec![vec![Value::Int64(51_010)]]
    );
}

#[test]
fn a_hostile_type_name_never_reaches_the_filesystem() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("graph");
    let path = root.to_str().unwrap();
    let mut graph = DirGraph::new();
    graph.enable_disk_mode().unwrap();
    let names = ["../../escape", "a/b", "C:\\evil", "with space", "/abs"];
    for name in names {
        add_departments(&mut graph, name, 3);
    }
    graph.save_disk(path).unwrap();

    let meta = column_meta(path);
    for name in names {
        let file = meta
            .files
            .get(name)
            .unwrap_or_else(|| panic!("{name} has no file"));
        assert!(
            file.starts_with("type_columns/") && !file.contains(".."),
            "{name} -> {file}"
        );
        assert!(current_generation(path)
            .join("seg_000")
            .join(file)
            .is_file());
    }
    assert!(!dir.path().join("escape").exists());
    assert!(!dir.path().join("evil").exists());
    drop(graph);
    let reloaded = load_owned(path);
    for name in names {
        assert_eq!(
            reloaded.column_store(name).map(|s| s.row_count()),
            Some(3),
            "{name}"
        );
    }
}

#[test]
fn a_column_file_shorter_than_its_metadata_is_refused_not_mapped() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    drop(saved_graph(path));
    let file = current_generation(path)
        .join("seg_000")
        .join(&column_meta(path).files["Employee"]);
    let bytes = std::fs::read(&file).unwrap();
    std::fs::write(&file, &bytes[..bytes.len() / 2]).unwrap();
    let error = load_file(path)
        .err()
        .expect("a truncated column file must not load");
    let message = error.to_string();
    assert!(message.contains("needs"), "{message}");
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Every id and title of every type, by type name.
fn snapshot(graph: &DirGraph) -> Vec<(String, Vec<Vec<Option<Value>>>)> {
    let mut out: Vec<_> = graph
        .column_stores_by_name()
        .into_iter()
        .map(|(name, store)| {
            let rows = (0..store.row_count())
                .map(|row| vec![store.get_id(row), store.get_title(row)])
                .collect();
            (name.to_string(), rows)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn fixture_copy(name: &str, tmp: &TempDir) -> String {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/kgl_v6")
        .join(name);
    let root = tmp.path().join("graph");
    copy_tree(&fixture, &root);
    root.to_str().unwrap().to_string()
}

/// A directory 0.19.0 wrote keeps `Unit` and `Tag` in its shared `columns.bin`;
/// the first save gives each its own file. `Pand` still holds a `Mixed` column
/// (timestamps beside a string), so it stays on a sidecar, unchanged.
#[test]
fn a_0_19_0_directory_moves_its_shared_columns_into_per_type_files_on_its_first_save() {
    let tmp = TempDir::new().unwrap();
    let path = fixture_copy("disk", &tmp);
    let mut graph = load_file(&path).unwrap();
    assert!(current_generation(&path)
        .join("seg_000/columns.bin")
        .exists());
    let before = snapshot(&graph);
    save_graph(&mut graph, &path).unwrap();
    drop(graph);

    let meta = column_meta(&path);
    for name in ["Unit", "Tag"] {
        assert!(
            meta.files.contains_key(name),
            "{name} has its own file after the save"
        );
    }
    assert!(
        !meta.files.contains_key("Pand"),
        "Pand holds a Mixed column"
    );
    assert!(!current_generation(&path)
        .join("seg_000/columns.bin")
        .exists());
    let reloaded = load_file(&path).unwrap();
    assert_eq!(snapshot(&reloaded), before, "every id and title survived");
}

/// 0.19.0 sends a type whose title is an integer to a per-type zstd sidecar; the
/// integer title now has a typed column region, so the first save moves the
/// type into its own mmap-served file.
#[test]
fn a_0_19_0_int_title_type_moves_from_its_sidecar_into_a_typed_file() {
    let tmp = TempDir::new().unwrap();
    let path = fixture_copy("disk_int_title", &tmp);
    let mut graph = load_file(&path).unwrap();
    let badge = graph.column_store("Badge").unwrap();
    assert!(
        !badge.has_mmap_base() && badge.title_type_str() == Some("int64"),
        "0.19.0 served Badge's integer title from a heap sidecar store"
    );
    assert!(current_generation(&path)
        .join("columns/Badge/columns.zst")
        .is_file());
    assert!(!current_generation(&path)
        .join("seg_000/columns_meta.json")
        .exists());
    let before = snapshot(&graph);
    assert_eq!(before[0].1[0][1], Some(Value::Int64(7_100_000_000_001)));
    save_graph(&mut graph, &path).unwrap();
    drop(graph);

    assert!(column_meta(&path).files.contains_key("Badge"));
    assert!(
        !current_generation(&path).join("columns").exists(),
        "the type is no longer on a sidecar"
    );
    let reloaded = load_file(&path).unwrap();
    let badge = reloaded.column_store("Badge").unwrap();
    assert!(
        badge.has_mmap_base(),
        "the integer-title type is served from its file"
    );
    assert_eq!(
        snapshot(&reloaded),
        before,
        "every id and title survived the migration"
    );
}
