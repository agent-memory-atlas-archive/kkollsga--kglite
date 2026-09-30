//! Type and property names are data: they never become a path.
//!
//! A type named `../../x` used to choose the directory of its zstd sidecar
//! (`columns/<type>/columns.zst`) and of its column spill files, and a property
//! named the same way chose a spill file, so a crafted name wrote outside the
//! generation and, with one more `../`, outside the graph directory.
use super::DirGraph;
use crate::datatypes::{DataFrame, Value};
use crate::graph::io::columns_meta;
use crate::graph::io::file::load_file;
use crate::graph::mutation::maintain;
use crate::graph::session::execute::{execute_mut, ExecuteOptions};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;

/// Every path under `root`, relative to it.
fn tree(root: &Path) -> BTreeSet<PathBuf> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeSet<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            out.insert(path.strip_prefix(base).unwrap().to_path_buf());
            if path.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(root, root, &mut out);
    out
}

/// The paths a step created that are not under any of `allowed`.
fn strays(before: &BTreeSet<PathBuf>, after: &BTreeSet<PathBuf>, allowed: &[&str]) -> Vec<PathBuf> {
    after
        .difference(before)
        .filter(|path| !allowed.iter().any(|prefix| path.starts_with(prefix)))
        .cloned()
        .collect()
}

fn run(graph: &mut DirGraph, query: &str) {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"));
}

fn add_staff(graph: &mut DirGraph, node_type: &str, extra_property: &str) {
    let rows = (1..=3i64)
        .map(|i| {
            vec![
                Value::Int64(i),
                Value::String(format!("employee-{i}")),
                Value::Int64(i * 100),
            ]
        })
        .collect();
    let frame = DataFrame::from_cypher_rows(
        vec!["id".into(), "name".into(), extra_property.into()],
        rows,
    )
    .unwrap();
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

/// A type holding an int and a string under one property: a `Mixed` column,
/// the one shape a save writes to a zstd sidecar.
fn add_mixed_staff(graph: &mut DirGraph, node_type: &str) {
    add_staff(graph, node_type, "grade");
    run(graph, "MATCH (n {id: 1}) SET n.badge = 7");
    run(graph, "MATCH (n {id: 2}) SET n.badge = 'B-2'");
}

fn current_generation(root: &Path) -> PathBuf {
    let current = std::fs::read_to_string(root.join("CURRENT")).unwrap();
    root.join("generations").join(current.trim())
}

fn badges(graph: &mut DirGraph) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    execute_mut(
        graph,
        "MATCH (n) RETURN n.id AS id, n.badge AS badge ORDER BY id",
        &ExecuteOptions::eager(&params),
    )
    .unwrap()
    .result
    .rows
}

fn expected_badges() -> Vec<Vec<Value>> {
    vec![
        vec![Value::Int64(1), Value::Int64(7)],
        vec![Value::Int64(2), Value::String("B-2".into())],
        vec![Value::Int64(3), Value::Null],
    ]
}

#[test]
fn a_type_name_never_chooses_where_its_sidecar_is_written() {
    let long = "x".repeat(300);
    for (case, hostile) in [
        "../x",
        "../../pwn",
        "../../../../pwn",
        "a/b",
        "..",
        ".",
        "",
        "con",
        "a\\b",
        "Ünïcödé/日本",
        long.as_str(),
        "ABSOLUTE",
    ]
    .iter()
    .enumerate()
    {
        let sandbox = TempDir::new().unwrap();
        let box_dir = sandbox.path().join("box");
        std::fs::create_dir(&box_dir).unwrap();
        // An absolute name pointing inside the sandbox: `join` with it would
        // replace the whole path.
        let name = if *hostile == "ABSOLUTE" {
            sandbox
                .path()
                .join("abs_sink")
                .to_string_lossy()
                .into_owned()
        } else {
            hostile.to_string()
        };
        let root = box_dir.join("g");
        let before = tree(sandbox.path());

        let mut graph = DirGraph::new();
        graph.enable_disk_mode().unwrap();
        add_mixed_staff(&mut graph, &name);
        graph.save_disk(root.to_str().unwrap()).unwrap();
        drop(graph);

        assert_eq!(
            strays(&before, &tree(sandbox.path()), &["box/g"]),
            Vec::<PathBuf>::new(),
            "case {case}: {hostile:?} wrote outside the graph directory"
        );
        let meta = columns_meta::read(
            &columns_meta::locate(&current_generation(&root)).expect("column metadata"),
        )
        .unwrap();
        let sidecar = &meta.sidecars[&name];
        assert!(
            sidecar.starts_with("columns/") && !sidecar.contains(".."),
            "case {case}: {sidecar}"
        );
        assert!(current_generation(&root)
            .join(sidecar)
            .join("columns.zst")
            .is_file());
        let mut reloaded = match Arc::try_unwrap(load_file(root.to_str().unwrap()).unwrap()) {
            Ok(graph) => graph,
            Err(_) => panic!("fresh load unexpectedly shared"),
        };
        assert_eq!(
            badges(&mut reloaded),
            expected_badges(),
            "case {case}: {hostile:?} lost its sidecar values"
        );
    }
}

#[test]
fn a_property_name_never_chooses_where_its_spill_file_is_written() {
    for property in ["../../../pwn", "a/b", "..", "con", "__id__", "x"] {
        let sandbox = TempDir::new().unwrap();
        let spill = sandbox.path().join("box").join("spill");
        std::fs::create_dir_all(&spill).unwrap();
        let before = tree(sandbox.path());

        let mut graph = DirGraph::new();
        graph.memory_limit = Some(0);
        graph.spill_dir = Some(spill);
        add_staff(&mut graph, "../../Employee", property);
        graph.maybe_spill_columns();

        let after = tree(sandbox.path());
        assert_eq!(
            strays(&before, &after, &["box/spill"]),
            Vec::<PathBuf>::new(),
            "property {property:?} wrote outside its spill directory"
        );
        assert!(
            after.iter().any(|p| p.starts_with("box/spill")),
            "property {property:?}: the columns did not spill"
        );
        let params = HashMap::new();
        let total = execute_mut(
            &mut graph,
            &format!("MATCH (n) RETURN sum(n.`{property}`) AS total"),
            &ExecuteOptions::eager(&params),
        )
        .unwrap()
        .result
        .rows;
        assert_eq!(total, vec![vec![Value::Int64(600)]], "{property:?}");
    }
}

/// 0.19.0 named the sidecar directory by the raw type name and recorded nothing
/// in the metadata; such a directory keeps loading by the name it carries.
#[test]
fn a_0_19_0_sidecar_directory_named_by_its_type_is_still_read() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("g");
    let mut graph = DirGraph::new();
    graph.enable_disk_mode().unwrap();
    add_mixed_staff(&mut graph, "Employee");
    graph.save_disk(root.to_str().unwrap()).unwrap();
    drop(graph);

    let generation = current_generation(&root);
    let meta_path = columns_meta::locate(&generation).unwrap();
    let mut meta = columns_meta::read(&meta_path).unwrap();
    let keyed = meta.sidecars.remove("Employee").expect("Employee sidecar");
    std::fs::rename(generation.join(keyed), generation.join("columns/Employee")).unwrap();
    columns_meta::publish_json_synced(meta_path.parent().unwrap(), &meta).unwrap();
    assert!(meta.sidecars.is_empty());

    let mut reloaded = match Arc::try_unwrap(load_file(root.to_str().unwrap()).unwrap()) {
        Ok(graph) => graph,
        Err(_) => panic!("fresh load unexpectedly shared"),
    };
    assert_eq!(badges(&mut reloaded), expected_badges());
}

#[test]
fn a_sidecar_entry_that_leaves_the_columns_directory_is_refused() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("g");
    let mut graph = DirGraph::new();
    graph.enable_disk_mode().unwrap();
    add_mixed_staff(&mut graph, "Employee");
    graph.save_disk(root.to_str().unwrap()).unwrap();
    drop(graph);

    let generation = current_generation(&root);
    let meta_path = columns_meta::locate(&generation).unwrap();
    let original = columns_meta::read(&meta_path).unwrap();
    let outside = dir.path().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    for bad in [
        "../../elsewhere",
        "columns/../../../../elsewhere",
        outside.to_str().unwrap(),
        "type_columns/x",
        "columns",
        "",
    ] {
        let mut meta = columns_meta::read(&meta_path).unwrap();
        meta.sidecars.insert("Employee".into(), bad.into());
        columns_meta::publish_json_synced(meta_path.parent().unwrap(), &meta).unwrap();
        let error = load_file(root.to_str().unwrap())
            .err()
            .unwrap_or_else(|| panic!("{bad:?} must not load"));
        assert!(
            error.to_string().contains("sidecar outside")
                || error.to_string().contains("outside its directory"),
            "{bad:?}: {error}"
        );
    }
    columns_meta::publish_json_synced(meta_path.parent().unwrap(), &original).unwrap();
    assert!(load_file(root.to_str().unwrap()).is_ok());
}
