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
use super::disk_link_tests::{current_generation, load_owned, run, saved_graph};
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
