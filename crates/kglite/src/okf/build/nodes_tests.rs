//! What a frontmatter value becomes in its column: JSON under the OKF
//! convention, native lists and maps and temporals under the vault profile.

use crate::datatypes::values::Value;
use crate::graph::schema::InternedKey;
use crate::graph::storage::GraphRead;
use crate::okf::build::build;
use crate::okf::build::tests_support::write;
use crate::okf::model::BuildOptions;
use tempfile::tempdir;

#[test]
fn tags_list_becomes_json_string() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "x.md",
        "---\ntype: Note\ntags:\n- alpha\n- beta\n---\nbody",
    );
    let g = build(dir.path(), &BuildOptions::default()).unwrap().graph;
    let n = g.graph.node_indices().next().unwrap();
    let key = InternedKey::from_str("tags");
    let v = GraphRead::get_node_property(&g.graph, n, key);
    assert_eq!(v, Some(Value::String("[\"alpha\",\"beta\"]".to_string())));
}

#[test]
fn vault_keeps_lists_and_maps_native() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "x.md",
        "---\ntags:\n- alpha\n- beta\nrelease:\n- name: v1\n  ok: true\n---\nbody",
    );
    let opts = BuildOptions::for_dialect(crate::okf::Dialect::Obsidian);
    let g = build(dir.path(), &opts).unwrap().graph;
    let n = g.graph.node_indices().next().unwrap();
    assert_eq!(
        GraphRead::get_node_property(&g.graph, n, InternedKey::from_str("tags")),
        Some(Value::List(vec![
            Value::String("alpha".into()),
            Value::String("beta".into()),
        ])),
        "a vault sequence reaches the graph as a list column"
    );
    assert!(
        matches!(
            GraphRead::get_node_property(&g.graph, n, InternedKey::from_str("release")),
            Some(Value::List(items)) if matches!(items.as_slice(), [Value::Map(_)])
        ),
        "a sequence of mappings stays a list of maps"
    );
}

#[test]
fn vault_dates_reach_the_graph_as_temporal_columns() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "x.md",
        "---\nupdated: 2026-01-15\nreviewed: '2026-01-15T09:30:00Z'\n---\nbody",
    );
    let opts = BuildOptions::for_dialect(crate::okf::Dialect::Obsidian);
    let g = build(dir.path(), &opts).unwrap().graph;
    let n = g.graph.node_indices().next().unwrap();
    assert_eq!(
        GraphRead::get_node_property(&g.graph, n, InternedKey::from_str("updated")),
        Some(Value::DateTime(
            chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap()
        ))
    );
    assert_eq!(
        GraphRead::get_node_property(&g.graph, n, InternedKey::from_str("reviewed")),
        Some(Value::Timestamp(
            chrono::NaiveDate::from_ymd_opt(2026, 1, 15)
                .unwrap()
                .and_hms_opt(9, 30, 0)
                .unwrap()
        ))
    );
}
