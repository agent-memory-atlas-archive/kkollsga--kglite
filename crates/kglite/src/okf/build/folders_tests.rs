//! The hierarchy: `Folder` nodes and their `CONTAINS` edges, the folder notes
//! that stand in for a directory, and the `parent:` key that names one.

use crate::datatypes::values::Value;
use crate::graph::schema::InternedKey;
use crate::graph::storage::GraphRead;
use crate::okf::build::build;
use crate::okf::build::tests_support::{
    count_label, edges_of, labels_by_id, nodes_with_titles, vault_build, vault_build_with, write,
};
use crate::okf::model::{
    BuildOptions, FolderNoteDirection, CONTAINS_CONN_TYPE, FOLDER_LABEL, FOLDER_NOTE_CONN_TYPE,
};
use tempfile::tempdir;

#[test]
fn folder_nodes_and_contains_edges() {
    let dir = tempdir().unwrap();
    write(dir.path(), "tables/orders.md", "---\ntype: Table\n---\nx");
    write(
        dir.path(),
        "tables/customers.md",
        "---\ntype: Table\n---\ny",
    );
    write(
        dir.path(),
        "tables/index.md",
        "# All Tables\nStructured data tables.",
    );
    let g = build(dir.path(), &BuildOptions::default()).unwrap().graph;
    // 2 concepts + 1 Folder("tables")
    assert_eq!(count_label(&g, "Folder"), 1);
    assert_eq!(g.graph.node_indices().count(), 3);
    // Folder(tables) CONTAINS both concepts = 2 edges (no links in bodies)
    assert_eq!(g.graph.edge_count(), 2);
    // index.md enriches the folder title (stored as the node title field).
    let folder_title = g
        .graph
        .node_indices()
        .find(|&n| {
            g.node_view(n)
                .is_some_and(|nd| nd.node_type_str(&g.interner) == "Folder")
        })
        .and_then(|n| g.node_view(n).map(|nd| nd.title().into_owned()));
    assert_eq!(folder_title, Some(Value::String("All Tables".to_string())));
}

#[test]
fn folder_title_ignores_a_leading_tag_line() {
    let dir = tempdir().unwrap();
    write(dir.path(), "tables/orders.md", "---\ntype: Table\n---\nx");
    write(dir.path(), "tables/index.md", "#data\n# All Tables\nProse.");
    let g = build(dir.path(), &BuildOptions::default()).unwrap().graph;
    let folder_title = g
        .graph
        .node_indices()
        .find(|&n| {
            g.node_view(n)
                .is_some_and(|nd| nd.node_type_str(&g.interner) == "Folder")
        })
        .and_then(|n| g.node_view(n).map(|nd| nd.title().into_owned()));
    assert_eq!(folder_title, Some(Value::String("All Tables".to_string())));
}

#[test]
fn nested_folders_chain_contains() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a/b/c.md", "---\ntype: Note\n---\ndeep");
    let g = build(dir.path(), &BuildOptions::default()).unwrap().graph;
    // folders a, a/b ; concept a/b/c
    assert_eq!(count_label(&g, "Folder"), 2);
    // CONTAINS: a→a/b, a/b→a/b/c = 2
    assert_eq!(g.graph.edge_count(), 2);
}

#[test]
fn vault_folders_come_from_the_file_path_not_the_stem_id() {
    let dir = tempdir().unwrap();
    write(dir.path(), "Geology/deep/faults.md", "prose");
    write(dir.path(), "root.md", "prose");
    let opts = BuildOptions::for_dialect(crate::okf::Dialect::Obsidian);
    let out = build(dir.path(), &opts).unwrap();
    assert_eq!(
        out.report.nodes_by_label.get(FOLDER_LABEL),
        Some(&2),
        "Geology and Geology/deep — a stem id would have flattened both away"
    );
    let contains: Vec<(String, String)> = out
        .report
        .edges_by_type
        .iter()
        .map(|(k, v)| (k.clone(), v.to_string()))
        .collect();
    assert_eq!(contains, vec![(CONTAINS_CONN_TYPE.to_string(), "2".into())]);
    assert_eq!(count_label(&out.graph, "Geology"), 1);
    assert_eq!(count_label(&out.graph, "Note"), 1, "the root note");
}

#[test]
fn vault_reads_index_and_log_as_ordinary_notes() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "notes/index.md",
        "---\ntitle: Index\n---\nA note.",
    );
    write(
        dir.path(),
        "notes/log.md",
        "---\ntitle: Log\n---\nA journal.",
    );
    write(dir.path(), "notes/a.md", "---\ntype: Note\n---\nA note.");
    let vault = vault_build(dir.path());
    assert_eq!(vault.report.files_scanned, 3);
    assert_eq!(
        labels_by_id(&vault.graph)
            .into_keys()
            .collect::<Vec<String>>(),
        vec![
            "a".to_string(),
            "index".to_string(),
            "log".to_string(),
            "notes".to_string()
        ],
        "both reserved names are notes of their own (VAULT.md §2.4)"
    );

    // …and the okf dialect still diverts one and drops the other.
    let okf = build(
        dir.path(),
        &BuildOptions::for_dialect(crate::okf::Dialect::Okf),
    )
    .unwrap();
    assert_eq!(okf.report.files_scanned, 1, "index.md and log.md reserved");
    assert_eq!(
        nodes_with_titles(&okf.graph, FOLDER_LABEL),
        vec![("notes".to_string(), "notes".to_string())],
        "index.md became this folder's metadata"
    );
}

#[test]
fn a_folder_note_inside_its_folder_replaces_it() {
    let dir = tempdir().unwrap();
    write(dir.path(), "projects/projects.md", "The folder note.");
    write(dir.path(), "projects/alpha.md", "A project.");
    let out = vault_build(dir.path());
    assert_eq!(out.report.folder_notes, 1);
    assert_eq!(
        count_label(&out.graph, FOLDER_LABEL),
        0,
        "the note took the Folder node's place"
    );
    assert_eq!(
        edges_of(&out.graph),
        vec![(
            "alpha".into(),
            FOLDER_NOTE_CONN_TYPE.into(),
            "projects".into(),
            vec![]
        )]
    );

    // The other spelling is the same folder note, so it must label the
    // same way: from where the *folder* sits, not from inside it.
    let beside = tempdir().unwrap();
    write(beside.path(), "projects.md", "The folder note.");
    write(beside.path(), "projects/alpha.md", "A project.");
    let other = vault_build(beside.path());
    assert_eq!(labels_by_id(&out.graph), labels_by_id(&other.graph));
    assert_eq!(
        labels_by_id(&out.graph).get("projects").map(String::as_str),
        Some("Note"),
        "a root folder's note is labelled from the root, not from its own folder"
    );
}

#[test]
fn a_plain_subfolder_under_a_folder_note_keeps_contains() {
    let dir = tempdir().unwrap();
    write(dir.path(), "projects.md", "The folder note.");
    write(dir.path(), "projects/alpha.md", "A project.");
    write(dir.path(), "projects/sub/deep.md", "Deeper.");
    let out = vault_build(dir.path());
    assert_eq!(
        nodes_with_titles(&out.graph, FOLDER_LABEL),
        vec![("projects/sub".to_string(), "sub".to_string())],
        "`sub/` has no folder note of its own, so it keeps its Folder node"
    );
    assert_eq!(
        edges_of(&out.graph),
        vec![
            // the folder-note edge joins notes …
            (
                "alpha".into(),
                FOLDER_NOTE_CONN_TYPE.into(),
                "projects".into(),
                vec![]
            ),
            // … and the note still *contains* the plain subfolder, which
            // in turn contains its own notes (VAULT.md §2.2, §2.3)
            (
                "projects".into(),
                CONTAINS_CONN_TYPE.into(),
                "projects/sub".into(),
                vec![]
            ),
            (
                "projects/sub".into(),
                CONTAINS_CONN_TYPE.into(),
                "deep".into(),
                vec![]
            ),
        ]
    );
}

#[test]
fn declaring_a_folder_note_twice_is_an_error() {
    let dir = tempdir().unwrap();
    write(dir.path(), "projects.md", "Beside the folder.");
    write(dir.path(), "projects/projects.md", "And inside it.");
    write(dir.path(), "projects/alpha.md", "A project.");
    let out = vault_build(dir.path());
    let clash = out
        .report
        .errors
        .iter()
        .find(|e| e.contains("folder note declared twice"))
        .unwrap_or_else(|| panic!("{:?}", out.report.errors));
    assert!(
        clash.contains("`projects.md`") && clash.contains("`projects/projects.md`"),
        "{clash}"
    );
    // One of them has to win, and the build still produces a hierarchy.
    assert_eq!(out.report.folder_notes, 1);
}

#[test]
fn the_folder_note_edge_can_point_down_instead() {
    let dir = tempdir().unwrap();
    write(dir.path(), "projects.md", "The folder note.");
    write(dir.path(), "projects/alpha.md", "A project.");
    let down = vault_build_with(dir.path(), |p| {
        p.folder_note_direction = FolderNoteDirection::ParentToChild;
    });
    assert_eq!(
        edges_of(&down.graph),
        vec![(
            "projects".into(),
            FOLDER_NOTE_CONN_TYPE.into(),
            "alpha".into(),
            vec![]
        )],
        "the same edge type, read from the parent's end"
    );
}

#[test]
fn a_parent_key_repeating_the_layout_is_one_edge() {
    let dir = tempdir().unwrap();
    write(dir.path(), "projects.md", "The folder note.");
    write(
        dir.path(),
        "projects/alpha.md",
        "---\nparent: \"[[projects]]\"\n---\nAlso says so itself.",
    );
    let out = vault_build(dir.path());
    assert_eq!(
        edges_of(&out.graph),
        vec![(
            "alpha".into(),
            FOLDER_NOTE_CONN_TYPE.into(),
            "projects".into(),
            vec![]
        )],
        "the layout and the `parent:` key name one relationship"
    );
    assert_eq!(
        out.report.edges_by_type.get(FOLDER_NOTE_CONN_TYPE),
        Some(&1),
        "and the report counts what the graph holds"
    );
}

#[test]
fn vault_parent_key_emits_the_folder_note_edge_in_its_direction() {
    let dir = tempdir().unwrap();
    write(dir.path(), "atlas.md", "leaf");
    write(dir.path(), "a.md", "---\nparent: \"[[atlas]]\"\n---\nbody");
    let out = vault_build(dir.path());
    assert_eq!(
        edges_of(&out.graph),
        vec![("a".into(), "CHILD_OF".into(), "atlas".into(), vec![])],
        "by default the child points at the parent"
    );
    assert_eq!(
        GraphRead::get_node_property(
            &out.graph.graph,
            out.graph
                .graph
                .node_indices()
                .find(|&n| out.graph.node_view(n).map(|nd| nd.id().into_owned())
                    == Some(Value::String("a".into())))
                .unwrap(),
            InternedKey::from_str("parent")
        ),
        None,
        "`parent:` is reserved, never a property"
    );

    let mut opts = BuildOptions::for_dialect(crate::okf::Dialect::Obsidian);
    opts.profile.folder_note_edge = "CONTAINS_NOTE".to_string();
    opts.profile.folder_note_direction = crate::okf::FolderNoteDirection::ParentToChild;
    let flipped = build(dir.path(), &opts).unwrap();
    assert_eq!(
        edges_of(&flipped.graph),
        vec![("atlas".into(), "CONTAINS_NOTE".into(), "a".into(), vec![])],
        "the profile names both the type and the direction"
    );
}
