//! A disk save links the column file of a type nothing touched from the
//! generation it replaces, and rewrites the file of every type anything touched.
//!
//! The decision is the whole risk: linking a file whose type changed would
//! publish a generation that silently lacks the change, and rewriting an
//! untouched one only costs time. So each kind of change a type can carry is
//! driven here against an untouched sibling, and the changed type's file must
//! differ from the previous generation's while the sibling's is the same file.
use super::disk_test_support::{column_meta, current_generation, load_owned, run};
use super::DirGraph;
use crate::datatypes::{DataFrame, Value};
use crate::graph::io::column_link;
use crate::graph::mutation::maintain;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Org-chart data: `Employee` is the type every change is applied to; the
/// others are untouched siblings.
fn add_type(graph: &mut DirGraph, node_type: &str, count: i64) {
    let rows = (0..count)
        .map(|i| {
            vec![
                Value::Int64(i),
                Value::String(format!("{node_type}-{i}")),
                Value::Int64(i * 3),
            ]
        })
        .collect();
    let frame = DataFrame::from_cypher_rows(vec!["id".into(), "name".into(), "grade".into()], rows)
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

/// Type name -> that type's column file in `generation`.
pub(super) fn type_files(generation: &Path) -> BTreeMap<String, PathBuf> {
    column_meta(generation)
        .files
        .into_iter()
        .map(|(name, relative)| (name, generation.join("seg_000").join(relative)))
        .collect()
}

#[cfg(unix)]
pub(super) fn inode(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).unwrap().ino()
}

#[cfg(unix)]
pub(super) fn link_count(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).unwrap().nlink()
}

/// Every file under `generation`, by relative path, with its bytes.
pub(super) fn snapshot(generation: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![generation.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry.path());
            } else {
                let relative = entry.path().strip_prefix(generation).unwrap().to_owned();
                out.insert(
                    relative.to_string_lossy().into_owned(),
                    std::fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    out
}

const SIBLINGS: [&str; 2] = ["Department", "Office"];

/// Three types saved as generation 1 at `path`; the live handle maps them.
pub(super) fn saved_graph(path: &str) -> DirGraph {
    let mut graph = DirGraph::new();
    graph.enable_disk_mode().unwrap();
    add_type(&mut graph, "Employee", 20_000);
    add_type(&mut graph, "Department", 300);
    add_type(&mut graph, "Office", 40);
    graph.save_disk(path).unwrap();
    graph
}

/// Apply `change` to `Employee`, save, and report the files of generation 1
/// and of the new one.
fn save_after(
    change: impl FnOnce(&mut DirGraph),
) -> (TempDir, String, DirGraph, BTreeMap<String, PathBuf>) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap().to_string();
    let mut graph = saved_graph(&path);
    let first = type_files(&current_generation(&path));
    change(&mut graph);
    graph.save_disk(&path).unwrap();
    (dir, path, graph, first)
}

#[cfg(unix)]
fn assert_siblings_linked_and_employee_rewritten(
    first: &BTreeMap<String, PathBuf>,
    path: &str,
    what: &str,
) {
    let second = type_files(&current_generation(path));
    for sibling in SIBLINGS {
        assert_eq!(
            inode(&second[sibling]),
            inode(&first[sibling]),
            "{what}: the untouched {sibling} was rewritten instead of linked"
        );
        assert!(link_count(&second[sibling]) >= 2, "{what}: {sibling}");
    }
    assert_ne!(
        inode(&second["Employee"]),
        inode(&first["Employee"]),
        "{what}: the changed Employee file is the previous generation's"
    );
}

#[cfg(unix)]
#[test]
fn a_save_with_no_change_links_every_type() {
    let (_dir, path, _graph, first) = save_after(|_| {});
    let second = type_files(&current_generation(&path));
    for (name, file) in &first {
        assert_eq!(inode(&second[name]), inode(file), "{name} was rewritten");
        assert!(link_count(file) >= 2, "{name}");
    }
}

#[cfg(unix)]
#[test]
fn a_set_on_a_base_row_rewrites_only_that_types_file() {
    let (_dir, path, mut graph, first) = save_after(|graph| {
        run(graph, "MATCH (e:Employee) WHERE e.id = 3 SET e.grade = 1");
    });
    assert_siblings_linked_and_employee_rewritten(&first, &path, "SET of an existing column");
    assert_eq!(
        run(
            &mut graph,
            "MATCH (e:Employee) WHERE e.id = 3 RETURN e.grade AS g"
        ),
        vec![vec![Value::Int64(1)]]
    );
    drop(graph);
    let mut reloaded = load_owned(&path);
    assert_eq!(
        run(
            &mut reloaded,
            "MATCH (e:Employee) WHERE e.id IN [3, 4] RETURN e.grade AS g ORDER BY e.id"
        ),
        vec![vec![Value::Int64(1)], vec![Value::Int64(12)]]
    );
}

#[cfg(unix)]
#[test]
fn a_set_that_creates_a_column_rewrites_only_that_types_file() {
    let (_dir, path, graph, first) = save_after(|graph| {
        run(
            graph,
            "MATCH (e:Employee) WHERE e.id = 3 SET e.nickname = 'Ace'",
        );
    });
    assert_siblings_linked_and_employee_rewritten(&first, &path, "SET creating a column");
    drop(graph);
    let mut reloaded = load_owned(&path);
    assert_eq!(
        run(
            &mut reloaded,
            "MATCH (e:Employee) WHERE e.id = 3 RETURN e.nickname AS n"
        ),
        vec![vec![Value::String("Ace".into())]]
    );
}

#[cfg(unix)]
#[test]
fn a_set_of_the_title_rewrites_only_that_types_file() {
    let (_dir, path, graph, first) = save_after(|graph| {
        run(
            graph,
            "MATCH (e:Employee) WHERE e.id = 3 SET e.title = 'Renamed'",
        );
    });
    assert_siblings_linked_and_employee_rewritten(&first, &path, "SET of the title");
    drop(graph);
    let mut reloaded = load_owned(&path);
    assert_eq!(
        run(
            &mut reloaded,
            "MATCH (e:Employee) WHERE e.id = 3 RETURN e.title AS t"
        ),
        vec![vec![Value::String("Renamed".into())]]
    );
}

#[cfg(unix)]
#[test]
fn an_append_rewrites_only_that_types_file() {
    let (_dir, path, graph, first) = save_after(|graph| {
        let rows = (20_000..20_010i64)
            .map(|i| {
                vec![
                    Value::Int64(i),
                    Value::String(format!("Employee-{i}")),
                    Value::Int64(i * 3),
                ]
            })
            .collect();
        let frame =
            DataFrame::from_cypher_rows(vec!["id".into(), "name".into(), "grade".into()], rows)
                .unwrap();
        maintain::add_nodes(
            graph,
            frame,
            "Employee".into(),
            "id".into(),
            Some("name".into()),
            None,
        )
        .unwrap();
    });
    assert_siblings_linked_and_employee_rewritten(&first, &path, "an appended tail");
    drop(graph);
    let reloaded = load_owned(&path);
    assert_eq!(
        reloaded.column_store("Employee").unwrap().row_count(),
        20_010
    );
}

#[cfg(unix)]
#[test]
fn a_delete_rewrites_only_that_types_file() {
    let (_dir, path, graph, first) = save_after(|graph| {
        run(graph, "MATCH (e:Employee) WHERE e.id = 3 DETACH DELETE e");
    });
    assert_siblings_linked_and_employee_rewritten(&first, &path, "a tombstone");
    drop(graph);
    let mut reloaded = load_owned(&path);
    assert_eq!(
        run(&mut reloaded, "MATCH (e:Employee) RETURN count(e) AS c"),
        vec![vec![Value::Int64(19_999)]]
    );
}

#[cfg(unix)]
#[test]
fn a_set_that_changes_a_columns_kind_leaves_no_stale_file_for_that_type() {
    let (_dir, path, graph, first) = save_after(|graph| {
        run(
            graph,
            "MATCH (e:Employee) WHERE e.id = 3 SET e.grade = 'twelve'",
        );
    });
    // The widened column cannot live in a column file, so the type may have no
    // file at all; what it must not have is the previous generation's.
    let second = type_files(&current_generation(&path));
    if let Some(file) = second.get("Employee") {
        assert_ne!(inode(file), inode(&first["Employee"]));
    }
    for sibling in SIBLINGS {
        assert_eq!(inode(&second[sibling]), inode(&first[sibling]), "{sibling}");
    }
    drop(graph);
    let mut reloaded = load_owned(&path);
    assert_eq!(
        run(
            &mut reloaded,
            "MATCH (e:Employee) WHERE e.id IN [3, 4] RETURN e.grade AS g ORDER BY e.id"
        ),
        vec![vec![Value::String("twelve".into())], vec![Value::Int64(12)]]
    );
}

#[cfg(unix)]
#[test]
fn a_rolled_back_set_is_never_linked_over_the_stale_overlay() {
    // Whatever a rollback leaves in the store, the next generation must hold
    // the data the graph has now: the first value, never the rolled-back one.
    let (_dir, path, mut graph, first) = save_after(|graph| {
        run(graph, "MATCH (e:Employee) WHERE e.id = 3 SET e.grade = 500");
        run(graph, "MATCH (e:Employee) WHERE e.id = 3 SET e.grade = 9");
    });
    let second = type_files(&current_generation(&path));
    assert_ne!(inode(&second["Employee"]), inode(&first["Employee"]));
    assert_eq!(
        run(
            &mut graph,
            "MATCH (e:Employee) WHERE e.id = 3 RETURN e.grade AS g"
        ),
        vec![vec![Value::Int64(9)]]
    );
}

#[cfg(unix)]
#[test]
fn a_type_mapped_from_another_generation_is_not_carried_from_this_one() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    let mut first_handle = saved_graph(path);
    let generation_one = type_files(&current_generation(path));
    // A second handle maps generation 1, then the first publishes generation 2.
    let mut stale = load_owned(path);
    run(
        &mut first_handle,
        "MATCH (e:Employee) WHERE e.id = 3 SET e.grade = 1",
    );
    first_handle.save_disk(path).unwrap();
    drop(first_handle);
    let generation_two = type_files(&current_generation(path));
    assert_ne!(
        inode(&generation_two["Employee"]),
        inode(&generation_one["Employee"])
    );

    stale.save_disk(path).unwrap();
    let generation_three = type_files(&current_generation(path));
    for name in ["Employee", "Department", "Office"] {
        assert_ne!(
            inode(&generation_three[name]),
            inode(&generation_two[name]),
            "{name} maps generation 1's file and so cannot stand in for generation 2's"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_differently_spelled_path_to_the_same_directory_still_links() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    drop(saved_graph(path));
    let spelled = format!(
        "{path}/../{}",
        dir.path().file_name().unwrap().to_str().unwrap()
    );
    let mut graph = load_owned(&spelled);
    let first = type_files(&current_generation(path));
    graph.save_disk(path).unwrap();
    let second = type_files(&current_generation(path));
    for (name, file) in &first {
        assert_eq!(
            inode(&second[name]),
            inode(file),
            "{name}: the mapping's path and the previous generation's name one file"
        );
    }
}

#[test]
fn a_refused_hard_link_falls_back_to_a_copy_of_the_same_bytes() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    let mut graph = saved_graph(path);
    let first = type_files(&current_generation(path));
    column_link::with_linking_refused(|| graph.save_disk(path).unwrap());
    let second = type_files(&current_generation(path));
    for (name, file) in &first {
        assert_eq!(
            std::fs::read(&second[name]).unwrap(),
            std::fs::read(file).unwrap(),
            "{name}: the copy differs from the file it stands in for"
        );
        #[cfg(unix)]
        assert_ne!(inode(&second[name]), inode(file), "{name} was linked");
    }
    drop(graph);
    let mut reloaded = load_owned(path);
    assert_eq!(
        run(
            &mut reloaded,
            "MATCH (e:Employee) WHERE e.id = 7 RETURN e.grade AS g"
        ),
        vec![vec![Value::Int64(21)]]
    );
}

/// Generation 1's files are byte-identical after later generations replaced
/// some and linked the rest.
#[test]
fn a_previous_generation_is_byte_identical_after_the_next_one_links_and_rewrites() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    let mut graph = saved_graph(path);
    let generation_one = current_generation(path);
    let before = snapshot(&generation_one);
    assert!(before.len() > 10, "the snapshot covers a whole generation");

    // Generation 2 rewrites Employee and links the rest from generation 1.
    run(
        &mut graph,
        "MATCH (e:Employee) WHERE e.id < 50 SET e.grade = -1",
    );
    graph.save_disk(path).unwrap();
    assert_eq!(
        snapshot(&generation_one),
        before,
        "generation 2 altered a file of generation 1"
    );

    // Generation 3 rewrites Department and links Employee (the file generation 2
    // wrote) and Office. Generation 1 is gone by now; generation 2 is the
    // previous one, and it too must be as it was.
    let generation_two = current_generation(path);
    let second = snapshot(&generation_two);
    run(
        &mut graph,
        "MATCH (d:Department) WHERE d.id < 5 SET d.grade = -2",
    );
    graph.save_disk(path).unwrap();
    assert_eq!(
        snapshot(&generation_two),
        second,
        "generation 3 altered a file of generation 2"
    );
}

/// A save that fails after it has linked files, at any point of the publish,
/// leaves the previous generation selected and byte-for-byte as it was, and the
/// next save from the same handle publishes what the failed one would have.
#[test]
fn a_publish_that_fails_after_linking_leaves_the_previous_generation_selected_and_untouched() {
    use crate::graph::storage::disk::generation::with_publish_failpoint;
    for stage in [
        "before_generation_rename",
        "after_generation_rename",
        "before_current_replace",
    ] {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_str().unwrap();
        let mut graph = saved_graph(path);
        let previous = current_generation(path);
        let before = snapshot(&previous);
        run(
            &mut graph,
            "MATCH (e:Employee) WHERE e.id = 3 SET e.grade = -7",
        );

        let failed = with_publish_failpoint(stage, || graph.save_disk(path));
        assert!(
            failed.is_err(),
            "{stage}: the injected failure did not surface"
        );
        assert_eq!(current_generation(path), previous, "{stage}: CURRENT moved");
        assert_eq!(
            snapshot(&previous),
            before,
            "{stage}: the failed save altered the previous generation"
        );
        let mut view = load_owned(path);
        assert_eq!(
            run(
                &mut view,
                "MATCH (e:Employee) WHERE e.id = 3 RETURN e.grade AS g"
            ),
            vec![vec![Value::Int64(9)]],
            "{stage}: the previous generation no longer reads as it did"
        );
        drop(view);

        graph.save_disk(path).unwrap();
        assert_ne!(current_generation(path), previous, "{stage}");
        drop(graph);
        let mut reopened = load_owned(path);
        assert_eq!(
            run(
                &mut reopened,
                "MATCH (e:Employee) WHERE e.id = 3 RETURN e.grade AS g"
            ),
            vec![vec![Value::Int64(-7)]],
            "{stage}: the retry did not publish the change"
        );
        assert_eq!(
            run(&mut reopened, "MATCH (o:Office) RETURN count(o) AS c"),
            vec![vec![Value::Int64(40)]],
            "{stage}: a linked type lost rows"
        );
        // No stage directory survives the retry.
        let stale: Vec<_> = std::fs::read_dir(Path::new(path).join("generations"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".stage-"))
            .collect();
        assert!(stale.is_empty(), "{stage}: {stale:?}");
    }
}
