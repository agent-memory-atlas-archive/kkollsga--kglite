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

/// Obsidian's own `cssclasses:` names a stylesheet, not a fact about the
/// note, and VAULT.md §4.1 reserves it: no property, and — its value being a
/// list of strings — no typed edge either when a vault writes one that looks
/// like a wikilink.
#[test]
fn a_vault_ignores_the_cssclasses_key() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "x.md",
        "---\ncssclasses:\n- wide-table\ncssclasses.theme: dark\nkeep: yes\n---\nbody",
    );
    write(dir.path(), "y.md", "---\ncssclasses: \"[[x]]\"\n---\nbody");
    let opts = BuildOptions::for_dialect(crate::okf::Dialect::Obsidian);
    let out = build(dir.path(), &opts).unwrap();
    let g = &out.graph;
    let x = g
        .graph
        .node_indices()
        .find(|&n| {
            matches!(g.node_view(n).map(|nd| nd.id().into_owned()),
                Some(Value::String(id)) if id == "x")
        })
        .unwrap();
    assert_eq!(
        GraphRead::get_node_property(&g.graph, x, InternedKey::from_str("cssclasses")),
        None
    );
    assert_eq!(
        GraphRead::get_node_property(&g.graph, x, InternedKey::from_str("cssclasses.theme")),
        None,
        "the dotted keys a nested spelling flattens to go with it"
    );
    assert_eq!(
        GraphRead::get_node_property(&g.graph, x, InternedKey::from_str("keep")),
        Some(Value::String("yes".into())),
        "only the ignored key leaves"
    );
    assert_eq!(
        out.report.edges_by_type.get("CSSCLASSES"),
        None,
        "a wikilink-shaped value never reaches the typed-edge rule"
    );
}

/// The key is reserved in a **vault**, not in a bundle: `okf`/`loose` have no
/// Obsidian to render for, and a producer writing `cssclasses:` there means
/// whatever it means.
#[test]
fn an_okf_bundle_stores_cssclasses_like_any_other_key() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "x.md",
        "---\ntype: Note\ncssclasses: wide-table\n---\nbody",
    );
    let g = build(dir.path(), &BuildOptions::default()).unwrap().graph;
    let n = g.graph.node_indices().next().unwrap();
    assert_eq!(
        GraphRead::get_node_property(&g.graph, n, InternedKey::from_str("cssclasses")),
        Some(Value::String("wide-table".into()))
    );
}
