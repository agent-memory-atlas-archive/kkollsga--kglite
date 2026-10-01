//! A disk directory 0.19.0 wrote holds its timestamp properties in `Mixed`
//! columns; the first save of it stores every column whose values are all
//! exact timestamps in the typed form, and leaves a column that mixes kinds
//! alone.
use super::DirGraph;
use crate::datatypes::Value;
use crate::graph::io::file::{load_file, save_graph};
use crate::graph::schema::InternedKey;
use std::path::Path;
use std::sync::Arc;

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

/// `Employment`'s column kind per property, as the store reports it.
fn employment_kinds(graph: &DirGraph, properties: &[&str]) -> Vec<(String, Option<&'static str>)> {
    let store = graph
        .column_stores_by_name()
        .into_iter()
        .find(|(name, _)| *name == "Employment")
        .map(|(_, store)| Arc::clone(store))
        .expect("the fixture has an Employment type");
    properties
        .iter()
        .map(|name| {
            let kind = store
                .slot(InternedKey::from_str(name))
                .and_then(|slot| store.column_type_str(slot as usize));
            (name.to_string(), kind)
        })
        .collect()
}

fn employment_values(graph: &DirGraph, property: &str) -> Vec<Option<Value>> {
    let store = graph
        .column_stores_by_name()
        .into_iter()
        .find(|(name, _)| *name == "Employment")
        .map(|(_, store)| Arc::clone(store))
        .unwrap();
    let key = InternedKey::from_str(property);
    (0..store.row_count())
        .map(|row| store.get(row, key))
        .collect()
}

#[test]
fn a_0_19_0_disk_directory_retypes_its_timestamp_columns_on_the_first_save() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/kgl_v6/disk");
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("graph");
    copy_tree(&fixture, &dir);
    let path = dir.to_str().unwrap();
    let properties = ["vf", "vt", "seen", "rec", "status"];

    let mut graph = load_file(path).unwrap();
    let before: Vec<Option<Value>> = employment_values(&graph, "seen");
    assert!(
        before
            .iter()
            .any(|v| matches!(v, Some(Value::Timestamp(_)))),
        "the fixture's `seen` column holds timestamps"
    );
    let kinds = employment_kinds(&graph, &properties);
    for (name, kind) in &kinds[..4] {
        assert_eq!(*kind, Some("mixed"), "{name} is Mixed as 0.19.0 wrote it");
    }
    let seen_before: Vec<_> = properties
        .iter()
        .map(|p| employment_values(&graph, p))
        .collect();

    save_graph(&mut graph, path).unwrap();
    drop(graph);
    let graph = load_file(path).unwrap();

    let kinds = employment_kinds(&graph, &properties);
    let kind_of = |name: &str| kinds.iter().find(|(n, _)| n == name).unwrap().1;
    for name in ["vf", "vt", "seen"] {
        assert_eq!(kind_of(name), Some("timestamp"), "{name} is re-typed");
    }
    assert_eq!(
        kind_of("rec"),
        Some("mixed"),
        "a column holding several kinds is never typed"
    );
    let seen_after: Vec<_> = properties
        .iter()
        .map(|p| employment_values(&graph, p))
        .collect();
    assert_eq!(seen_after, seen_before, "no value changed in the re-typing");
}
