//! What a disk save does with the cross-type `title`/`nid` bundles and with
//! `id_indices.bin` / `type_indices.bin`.
//!
//! The global bundles are rebuilt by an O(nodes) scan, so a save that carries a
//! bundle which still covers the graph saves that scan; a save that carries one
//! the graph has moved under publishes a bundle the next process believes (its
//! freshness restarts at "covers everything"), and every title lookup after the
//! reload answers from it. The cases here therefore drive each way a graph can
//! move and read the answer back through a *reloaded* graph, where a stale
//! bundle cannot be told from a current one except by its content.
use super::disk_link_tests::saved_graph;
use super::disk_test_support::{current_generation, load_owned, run};
use super::DirGraph;
use crate::datatypes::{DataFrame, Value};
use crate::graph::mutation::maintain;
use crate::graph::storage::disk::graph_property_index::take_global_builds;

fn title_hits(graph: &DirGraph, title: &str) -> Option<usize> {
    graph
        .graph
        .as_disk()
        .expect("disk backend")
        .lookup_global_eq("title", title)
        .map(|hits| hits.len())
}

fn add_employees(graph: &mut DirGraph, rows: &[(i64, &str)], conflict: Option<&str>) {
    let frame = DataFrame::from_cypher_rows(
        vec!["id".into(), "name".into(), "grade".into()],
        rows.iter()
            .map(|(id, name)| {
                vec![
                    Value::Int64(*id),
                    Value::String((*name).to_string()),
                    Value::Int64(1),
                ]
            })
            .collect(),
    )
    .unwrap();
    maintain::add_nodes(
        graph,
        frame,
        "Employee".into(),
        "id".into(),
        Some("name".into()),
        conflict.map(str::to_string),
    )
    .unwrap();
}

/// Link Employee 3 to Department 2, stating the employee's title on the way.
fn connect_with_source_title(graph: &mut DirGraph) {
    let frame = DataFrame::from_cypher_rows(
        vec!["emp".into(), "dept".into(), "emp_name".into()],
        vec![vec![
            Value::Int64(3),
            Value::Int64(2),
            Value::String("Via-Edge".into()),
        ]],
    )
    .unwrap();
    maintain::add_connections(
        graph,
        frame,
        "WORKS_IN".into(),
        "Employee".into(),
        "emp".into(),
        "Department".into(),
        "dept".into(),
        Some("emp_name".into()),
        None,
        None,
    )
    .unwrap();
}

/// A statement that writes a title and is then rejected, so it rolls back.
fn rolled_back_title_set(graph: &mut DirGraph) {
    let params = std::collections::HashMap::new();
    let outcome = crate::graph::session::execute::execute_mut(
        graph,
        "MATCH (e:Employee) WHERE e.id = 3 SET e.title = 'RolledBack', \
         e.bad = duration({months: 2147483648})",
        &crate::graph::session::execute::ExecuteOptions::eager(&params),
    );
    assert!(outcome.is_err(), "the statement must fail to roll back");
}

/// One way a graph moves under its title bundle, and what a title lookup must
/// say afterwards: `(title, rows carrying it)`.
struct Change {
    name: &'static str,
    apply: fn(&mut DirGraph),
    expect: &'static [(&'static str, usize)],
}

const CHANGES: &[Change] = &[
    Change {
        name: "a SET of the title",
        apply: |g| {
            run(
                g,
                "MATCH (e:Employee) WHERE e.id = 3 SET e.title = 'Renamed'",
            );
        },
        expect: &[("Renamed", 1), ("Employee-3", 0), ("Employee-4", 1)],
    },
    Change {
        name: "a SET of the title through its alias",
        apply: |g| {
            run(
                g,
                "MATCH (e:Employee) WHERE e.id = 3 SET e.name = 'Aliased'",
            );
        },
        expect: &[("Aliased", 1), ("Employee-3", 0)],
    },
    Change {
        name: "a node appended through the loader",
        apply: |g| add_employees(g, &[(20_000, "Appended-1")], None),
        expect: &[("Appended-1", 1), ("Employee-3", 1)],
    },
    Change {
        name: "a node created by a statement",
        apply: |g| {
            run(g, "CREATE (:Employee {id: 90000, name: 'Created-1'})");
        },
        expect: &[("Created-1", 1)],
    },
    Change {
        name: "a node merged by a statement",
        apply: |g| {
            run(g, "MERGE (:Employee {id: 90001, name: 'Merged-1'})");
        },
        expect: &[("Merged-1", 1)],
    },
    Change {
        name: "a delete",
        apply: |g| {
            run(g, "MATCH (e:Employee) WHERE e.id = 3 DETACH DELETE e");
        },
        expect: &[("Employee-3", 0), ("Employee-4", 1)],
    },
    Change {
        name: "a delete followed by a create into the freed slot",
        apply: |g| {
            run(g, "MATCH (e:Employee) WHERE e.id = 3 DETACH DELETE e");
            run(g, "CREATE (:Employee {id: 91000, name: 'Reborn'})");
        },
        expect: &[("Employee-3", 0), ("Reborn", 1)],
    },
    Change {
        name: "a loader update of an existing node's title",
        apply: |g| add_employees(g, &[(3, "Updated-3")], Some("update")),
        expect: &[("Updated-3", 1), ("Employee-3", 0)],
    },
    Change {
        name: "a title stated by a relationship load",
        apply: connect_with_source_title,
        expect: &[("Via-Edge", 1), ("Employee-3", 0)],
    },
    Change {
        name: "a SET of the title on a sibling type",
        apply: |g| {
            run(
                g,
                "MATCH (d:Department) WHERE d.id = 2 SET d.title = 'Renamed Dept'",
            );
        },
        expect: &[("Renamed Dept", 1), ("Department-2", 0)],
    },
    Change {
        name: "a rolled-back title SET followed by a committed one",
        apply: |g| {
            rolled_back_title_set(g);
            run(g, "MATCH (e:Employee) WHERE e.id = 4 SET e.title = 'After'");
        },
        expect: &[
            ("After", 1),
            ("RolledBack", 0),
            ("Employee-3", 1),
            ("Employee-4", 0),
        ],
    },
];

/// Apply `change`, save, reload, and check what the reloaded graph answers.
/// `reopen_first` makes the change land on a graph loaded from the published
/// generation (the state `open` reaches) rather than on the handle that built it.
fn check_change(change: &Change, reopen_first: bool) {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap().to_string();
    let mut graph = saved_graph(&path);
    if reopen_first {
        drop(graph);
        graph = load_owned(&path);
    }
    take_global_builds();
    (change.apply)(&mut graph);
    graph.save_disk(&path).unwrap();
    let builds = take_global_builds();
    assert!(
        builds.iter().any(|property| property == "title"),
        "{} (reopened: {reopen_first}): the title bundle was carried over a change, builds = {builds:?}",
        change.name
    );
    drop(graph);
    let reloaded = load_owned(&path);
    for (title, expected) in change.expect {
        assert_eq!(
            title_hits(&reloaded, title),
            Some(*expected),
            "{} (reopened: {reopen_first}): lookup of {title:?} on the reloaded graph",
            change.name
        );
    }
}

#[test]
fn every_kind_of_change_rebuilds_the_title_bundle_on_the_handle_that_made_it() {
    for change in CHANGES {
        check_change(change, false);
    }
}

#[test]
fn every_kind_of_change_rebuilds_the_title_bundle_on_a_reopened_graph() {
    for change in CHANGES {
        check_change(change, true);
    }
}

#[test]
fn a_save_that_changed_nothing_builds_no_global_bundle() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap().to_string();
    let mut graph = saved_graph(&path);
    assert_eq!(
        take_global_builds(),
        vec!["title".to_string(), "nid".to_string()],
        "a first save has nothing to carry"
    );

    graph.save_disk(&path).unwrap();
    assert_eq!(
        take_global_builds(),
        Vec::<String>::new(),
        "a second save on the same handle rescans every node for nothing"
    );
    assert_eq!(title_hits(&graph, "Employee-5"), Some(1));
    drop(graph);

    let mut reloaded = load_owned(&path);
    reloaded.save_disk(&path).unwrap();
    assert_eq!(
        take_global_builds(),
        Vec::<String>::new(),
        "a reopened graph saved unchanged rescans every node for nothing"
    );
    assert_eq!(title_hits(&reloaded, "Employee-5"), Some(1));
    drop(reloaded);

    let after = load_owned(&path);
    assert_eq!(
        title_hits(&after, "Employee-5"),
        Some(1),
        "the carried bundle still answers after a reload"
    );
    assert_eq!(title_hits(&after, "Office-39"), Some(1));
}

#[test]
fn a_bundle_the_generation_does_not_hold_is_built() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap().to_string();
    drop(saved_graph(&path));
    let mut removed = 0;
    let mut pending = vec![current_generation(&path)];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else if entry
                .file_name()
                .to_string_lossy()
                .starts_with("global_index_")
            {
                std::fs::remove_file(entry.path()).unwrap();
                removed += 1;
            }
        }
    }
    assert!(removed > 0, "the fixture must have written global bundles");

    let mut graph = load_owned(&path);
    take_global_builds();
    graph.save_disk(&path).unwrap();
    let builds = take_global_builds();
    assert!(
        builds.iter().any(|property| property == "title"),
        "a save with no bundle to carry must build one, builds = {builds:?}"
    );
    drop(graph);
    assert_eq!(title_hits(&load_owned(&path), "Employee-5"), Some(1));
}

/// A type whose title field is `label`, beside an ordinary `name` and `code`.
fn add_items(graph: &mut DirGraph) {
    let frame = DataFrame::from_cypher_rows(
        vec!["id".into(), "label".into(), "name".into(), "code".into()],
        (0..50)
            .map(|i| {
                vec![
                    Value::Int64(i),
                    Value::String(format!("Item-{i}")),
                    Value::String(format!("name-{i}")),
                    Value::Int64(i),
                ]
            })
            .collect(),
    )
    .unwrap();
    maintain::add_nodes(
        graph,
        frame,
        "Item".into(),
        "id".into(),
        Some("label".into()),
        None,
    )
    .unwrap();
}

/// Rows whose title is `value`, counted by a scan: the function around the
/// property keeps the predicate off every index.
fn scanned_title_count(graph: &mut DirGraph, value: &str) -> i64 {
    match run(
        graph,
        &format!("MATCH (n) WHERE toString(n.title) = '{value}' RETURN count(n) AS c"),
    )[0][0]
    {
        Value::Int64(count) => count,
        ref other => panic!("count was {other:?}"),
    }
}

/// A write that no global bundle reads is carried over; one that any of them
/// reads is not. The bundle is held to what a scan answers, so the test does
/// not decide which spelling is the title: it asks the graph.
#[test]
fn a_save_carries_a_global_bundle_exactly_when_no_write_could_have_changed_it() {
    // (statement, the global bundles that must have been rebuilt)
    let writes: &[(&str, &[&str])] = &[
        ("MATCH (e:Employee {id: 3}) SET e.grade = 99", &[]),
        (
            "MATCH (e:Employee {id: 3}) SET e.grade = 99, e.rank = 'x'",
            &[],
        ),
        ("MATCH (i:Item {id: 4}) SET i.code = 99", &[]),
        (
            "MATCH (i:Item {id: 4}) SET i.label = 'Relabelled'",
            &["nid", "title"],
        ),
        (
            "MATCH (i:Item {id: 4}) SET i.name = 'Relabelled'",
            &["nid", "title"],
        ),
        (
            "MATCH (i:Item {id: 4}) SET i.title = 'Relabelled'",
            &["nid", "title"],
        ),
        (
            "MATCH (e:Employee {id: 3}) SET e.name = 'Relabelled'",
            &["nid", "title"],
        ),
        (
            "MATCH (e:Employee {id: 3}) SET e.nid = 'Relabelled'",
            &["nid"],
        ),
    ];
    for reopen_first in [false, true] {
        for (statement, rebuilt) in writes {
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().to_str().unwrap().to_string();
            let mut graph = saved_graph(&path);
            add_items(&mut graph);
            graph.save_disk(&path).unwrap();
            if reopen_first {
                drop(graph);
                graph = load_owned(&path);
            }
            take_global_builds();
            run(&mut graph, statement);
            graph.save_disk(&path).unwrap();
            let mut builds = take_global_builds();
            builds.sort();
            assert_eq!(
                builds,
                rebuilt.to_vec(),
                "{statement} (reopened: {reopen_first}): the global bundles rebuilt"
            );
            drop(graph);
            let mut reloaded = load_owned(&path);
            for title in [
                "Relabelled",
                "Employee-3",
                "Employee-4",
                "Item-4",
                "Item-5",
                "name-4",
            ] {
                assert_eq!(
                    title_hits(&reloaded, title),
                    Some(scanned_title_count(&mut reloaded, title) as usize),
                    "{statement} (reopened: {reopen_first}): the bundle and a scan disagree on {title:?}"
                );
            }
        }
    }
}

// Only the unix-gated hard-link tests call this.
#[cfg(unix)]
/// `(id_indices.bin, type_indices.bin)` of the generation `path` is on.
fn index_files(path: &str) -> [std::path::PathBuf; 2] {
    let generation = current_generation(path);
    [
        generation.join("id_indices.bin"),
        generation.join("type_indices.bin"),
    ]
}

#[cfg(unix)]
fn inodes(files: &[std::path::PathBuf; 2]) -> [u64; 2] {
    use super::disk_link_tests::inode;
    [inode(&files[0]), inode(&files[1])]
}

/// Apply `change` to a saved graph, save, and report whether each index file of
/// the new generation is the previous generation's file: `[ids, types]`.
#[cfg(unix)]
fn linked_after(change: impl FnOnce(&mut DirGraph)) -> ([bool; 2], String, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap().to_string();
    let mut graph = saved_graph(&path);
    drop(graph);
    // A reopened graph is what serves its indexes from the files.
    graph = load_owned(&path);
    let before = inodes(&index_files(&path));
    change(&mut graph);
    graph.save_disk(&path).unwrap();
    drop(graph);
    let after = inodes(&index_files(&path));
    ([before[0] == after[0], before[1] == after[1]], path, dir)
}

#[cfg(unix)]
#[test]
fn a_save_with_no_change_links_both_index_files() {
    let (linked, path, _dir) = linked_after(|_| {});
    assert_eq!(linked, [true, true], "id_indices.bin, type_indices.bin");
    let mut reloaded = load_owned(&path);
    assert_eq!(
        run(
            &mut reloaded,
            "MATCH (e:Employee {id: 777}) RETURN e.grade AS g"
        ),
        vec![vec![Value::Int64(777 * 3)]]
    );
    assert_eq!(
        run(&mut reloaded, "MATCH (e:Employee) RETURN count(e) AS c"),
        vec![vec![Value::Int64(20_000)]]
    );
}

#[cfg(unix)]
#[test]
fn a_set_of_a_property_no_index_holds_links_both_index_files() {
    let (linked, _path, _dir) = linked_after(|graph| {
        run(graph, "MATCH (e:Employee {id: 3}) SET e.grade = 1");
    });
    assert_eq!(linked, [true, true], "id_indices.bin, type_indices.bin");
}

/// Each change an index can carry, and the answers a reload must give.
#[cfg(unix)]
#[test]
fn every_change_to_what_an_index_holds_rewrites_it() {
    type Check = fn(&mut DirGraph);
    // `[ids, types]`: which file the new generation still shares with the old.
    let cases: &[(&str, [bool; 2], Check, Check)] = &[
        (
            "an appended row",
            [false, false],
            |g| add_employees(g, &[(20_000, "Appended")], None),
            |g| {
                assert_eq!(
                    run(g, "MATCH (e:Employee {id: 20000}) RETURN e.name AS n"),
                    vec![vec![Value::String("Appended".into())]]
                );
                assert_eq!(
                    run(g, "MATCH (e:Employee) RETURN count(e) AS c"),
                    vec![vec![Value::Int64(20_001)]]
                );
            },
        ),
        (
            "a created node",
            [false, false],
            |g| {
                run(g, "CREATE (:Employee {id: 90000, name: 'Created'})");
            },
            |g| {
                assert_eq!(
                    run(g, "MATCH (e:Employee {id: 90000}) RETURN e.name AS n"),
                    vec![vec![Value::String("Created".into())]]
                );
            },
        ),
        (
            "a created node of a new type",
            [false, false],
            |g| {
                run(g, "CREATE (:Gadget {id: 5, name: 'Gizmo'})");
            },
            |g| {
                assert_eq!(
                    run(g, "MATCH (x:Gadget {id: 5}) RETURN x.name AS n"),
                    vec![vec![Value::String("Gizmo".into())]]
                );
            },
        ),
        (
            "a delete",
            [false, false],
            |g| {
                run(g, "MATCH (e:Employee {id: 3}) DETACH DELETE e");
            },
            |g| {
                assert_eq!(
                    run(g, "MATCH (e:Employee {id: 3}) RETURN e.name AS n"),
                    Vec::<Vec<Value>>::new()
                );
                assert_eq!(
                    run(g, "MATCH (e:Employee) RETURN count(e) AS c"),
                    vec![vec![Value::Int64(19_999)]]
                );
            },
        ),
        (
            "a delete followed by a create into the freed slot",
            [false, false],
            |g| {
                run(g, "MATCH (e:Employee {id: 3}) DETACH DELETE e");
                run(g, "CREATE (:Employee {id: 91000, name: 'Reborn'})");
            },
            |g| {
                assert_eq!(
                    run(g, "MATCH (e:Employee {id: 91000}) RETURN e.name AS n"),
                    vec![vec![Value::String("Reborn".into())]]
                );
                assert_eq!(
                    run(g, "MATCH (e:Employee {id: 3}) RETURN e.name AS n"),
                    Vec::<Vec<Value>>::new()
                );
            },
        ),
    ];
    for (name, expect, change, check) in cases {
        let (linked, path, _dir) = linked_after(*change);
        assert_eq!(
            linked, *expect,
            "{name}: which index files were linked unchanged (id, type)"
        );
        let mut reloaded = load_owned(&path);
        check(&mut reloaded);
    }
}

/// A file in the previous layout is what the next save rewrites, so a link
/// must not carry it forward.
#[cfg(unix)]
#[test]
fn an_id_index_file_in_an_older_layout_is_rewritten_not_linked() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap().to_string();
    drop(saved_graph(&path));
    let [ids, _] = index_files(&path);
    let mut bytes = std::fs::read(&ids).unwrap();
    assert_eq!(&bytes[8..12], &3u32.to_le_bytes(), "the current layout");
    bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
    std::fs::write(&ids, &bytes).unwrap();

    let mut graph = load_owned(&path);
    let before = inodes(&index_files(&path));
    graph.save_disk(&path).unwrap();
    drop(graph);

    let after = index_files(&path);
    assert_ne!(inodes(&after)[0], before[0], "the old layout was linked on");
    assert_eq!(
        &std::fs::read(&after[0]).unwrap()[8..12],
        &3u32.to_le_bytes(),
        "the next save writes the current layout"
    );
    assert_eq!(inodes(&after)[1], before[1], "the type index was unchanged");
}

/// Where hard links are refused the files are copied: same bytes, own inode.
#[cfg(unix)]
#[test]
fn a_refused_link_copies_the_index_files() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap().to_string();
    drop(saved_graph(&path));
    let mut graph = load_owned(&path);
    let before = index_files(&path);
    let bytes: Vec<Vec<u8>> = before.iter().map(|f| std::fs::read(f).unwrap()).collect();
    let before_inodes = inodes(&before);

    crate::graph::io::column_link::with_linking_refused(|| graph.save_disk(&path).unwrap());
    drop(graph);

    let after = index_files(&path);
    assert_ne!(inodes(&after), before_inodes);
    for (file, original) in after.iter().zip(&bytes) {
        assert_eq!(&std::fs::read(file).unwrap(), original);
    }
    let mut reloaded = load_owned(&path);
    assert_eq!(
        run(
            &mut reloaded,
            "MATCH (e:Employee {id: 5}) RETURN e.grade AS g"
        ),
        vec![vec![Value::Int64(15)]]
    );
}
