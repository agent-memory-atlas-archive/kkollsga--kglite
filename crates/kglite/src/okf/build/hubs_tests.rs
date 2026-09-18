//! Hub membership: the id a spelling folds onto, the title the hub takes, and
//! the `Tag` and `Source` nodes every dialect mints.

use crate::datatypes::values::Value;
use crate::graph::schema::InternedKey;
use crate::graph::storage::GraphRead;
use crate::okf::build::build;
use crate::okf::build::tests_support::{
    copy_tree, count_label, edges_of, nodes_with_titles, vault_build, vault_build_with, write,
    EdgeFacts,
};
use crate::okf::model::{BuildOptions, HubSpec, TAGGED_CONN_TYPE, TAG_LABEL};
use std::path::Path;
use tempfile::tempdir;

/// A hub over `keywords:`, the shape `.kglite/vault.yaml` declares.
fn keyword_hub(case_insensitive: bool) -> (String, HubSpec) {
    (
        "keywords".to_string(),
        HubSpec {
            label: "Keyword".to_string(),
            edge: "HAS_KEYWORD".to_string(),
            case_insensitive,
        },
    )
}

fn keyword_vault() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "a.md",
        "---\nkeywords: [Faults, faults, Horizons]\n---\nOne note.",
    );
    write(
        dir.path(),
        "b.md",
        "---\nkeywords: [faults, horizons]\n---\nAnother.",
    );
    dir
}

#[test]
fn a_folding_hub_titles_itself_with_the_commonest_casing() {
    let dir = keyword_vault();
    let out = vault_build_with(dir.path(), |p| {
        p.hubs.extend([keyword_hub(true)]);
    });
    assert_eq!(
        nodes_with_titles(&out.graph, "Keyword"),
        vec![
            // `faults` twice against `Faults` once
            ("faults".to_string(), "faults".to_string()),
            // one each — the tie is settled alphabetically, never by which
            // note was read first
            ("horizons".to_string(), "Horizons".to_string()),
        ]
    );
    let members: Vec<EdgeFacts> = edges_of(&out.graph)
        .into_iter()
        .filter(|(_, c, _, _)| c == "HAS_KEYWORD")
        .collect();
    assert_eq!(
        members,
        vec![
            ("a".into(), "HAS_KEYWORD".into(), "faults".into(), vec![]),
            ("a".into(), "HAS_KEYWORD".into(), "horizons".into(), vec![]),
            ("b".into(), "HAS_KEYWORD".into(), "faults".into(), vec![]),
            ("b".into(), "HAS_KEYWORD".into(), "horizons".into(), vec![]),
        ],
        "`Faults` and `faults` in one note are one membership"
    );
}

#[test]
fn a_case_sensitive_hub_keeps_every_spelling_apart() {
    let dir = keyword_vault();
    let out = vault_build_with(dir.path(), |p| {
        p.hubs.extend([keyword_hub(false)]);
    });
    assert_eq!(
        nodes_with_titles(&out.graph, "Keyword"),
        vec![
            ("Faults".to_string(), "Faults".to_string()),
            ("Horizons".to_string(), "Horizons".to_string()),
            ("faults".to_string(), "faults".to_string()),
            ("horizons".to_string(), "horizons".to_string()),
        ],
        "without folding, the title is the id"
    );
    assert_eq!(out.report.edges_by_type.get("HAS_KEYWORD"), Some(&5));
}

/// A vault folds tag casing because Obsidian does (VAULT.md §5.5): `#Seismic`
/// and `seismic` are one tag, held under the lowercased id and titled with the
/// casing the vault used most often.
#[test]
fn the_tag_hub_folds_casing_in_a_vault() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a.md", "---\ntags: [Seismic, seismic]\n---\nx");
    write(
        dir.path(),
        "b.md",
        "---\ntags: [seismic]\n---\nAnd inline #SEISMIC.",
    );
    let out = vault_build(dir.path());
    assert_eq!(
        nodes_with_titles(&out.graph, TAG_LABEL),
        vec![("seismic".to_string(), "seismic".to_string())],
        "one tag, titled with the casing the vault used most often"
    );
    assert_eq!(
        out.report.edges_by_type.get(TAGGED_CONN_TYPE),
        Some(&2),
        "two spellings in one note, and a third form inline, are one \
         membership each"
    );
}

/// And an `okf`/`loose` bundle does not: tag identity there has always been
/// the string the frontmatter spelled, and folding it would merge two `Tag`
/// nodes in every bundle already built.
#[test]
fn the_tag_hub_keeps_casing_apart_in_a_bundle() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "a.md",
        "---\ntype: Note\ntags: [Seismic, seismic]\n---\nx",
    );
    let out = build(
        dir.path(),
        &BuildOptions::for_dialect(crate::okf::Dialect::Okf),
    )
    .unwrap();
    assert_eq!(
        nodes_with_titles(&out.graph, TAG_LABEL),
        vec![
            ("Seismic".to_string(), "Seismic".to_string()),
            ("seismic".to_string(), "seismic".to_string()),
        ]
    );
    assert_eq!(out.report.edges_by_type.get(TAGGED_CONN_TYPE), Some(&2));
}

/// A vault that wants the two kept apart redeclares the hub, which is the
/// same mechanism any other hub uses — so the fold is a default, not a rule.
#[test]
fn a_vault_can_redeclare_the_tag_hub_case_sensitive() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a.md", "---\ntags: [Seismic, seismic]\n---\nx");
    let out = vault_build_with(dir.path(), |p| {
        p.hubs
            .get_mut("tags")
            .expect("the built-in hub")
            .case_insensitive = false;
    });
    assert_eq!(
        nodes_with_titles(&out.graph, TAG_LABEL),
        vec![
            ("Seismic".to_string(), "Seismic".to_string()),
            ("seismic".to_string(), "seismic".to_string()),
        ]
    );
}

#[test]
fn a_wikilink_valued_hub_key_goes_to_the_typed_edge_rule() {
    let dir = tempdir().unwrap();
    write(dir.path(), "faults.md", "A target.");
    write(
        dir.path(),
        "a.md",
        "---\nkeywords:\n  - \"[[faults]]\"\n---\nx",
    );
    let out = vault_build_with(dir.path(), |p| {
        p.hubs.extend([keyword_hub(true)]);
    });
    assert_eq!(
        count_label(&out.graph, "Keyword"),
        0,
        "the typed-edge rule wins, so the hub gets nothing"
    );
    assert_eq!(out.report.edges_by_type.get("KEYWORDS"), Some(&1));
    let warning = out
        .report
        .warnings
        .iter()
        .find(|w| w.contains("hub key `keywords`"))
        .unwrap_or_else(|| panic!("{:?}", out.report.warnings));
    assert!(warning.contains("a.md"), "{warning}");
}

#[test]
fn vault_inline_tags_join_the_same_hub_without_touching_the_property() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "a.md",
        "---\ntags:\n- alpha\n---\nAlso #beta, and #alpha again.",
    );
    let out = vault_build(dir.path());
    assert_eq!(count_label(&out.graph, TAG_LABEL), 2, "alpha, beta");
    assert_eq!(
        out.report.edges_by_type.get(TAGGED_CONN_TYPE),
        Some(&2),
        "writing `alpha` in both places is one membership"
    );
    let n = out
        .graph
        .graph
        .node_indices()
        .find(|&n| {
            out.graph
                .node_view(n)
                .is_some_and(|nd| nd.node_type_str(&out.graph.interner) == "Note")
        })
        .unwrap();
    assert_eq!(
        GraphRead::get_node_property(&out.graph.graph, n, InternedKey::from_str("tags")),
        Some(Value::List(vec![Value::String("alpha".into())])),
        "the `tags` property still reports only the frontmatter"
    );
}

/// The same hub and heading map the fixture's `.kglite/vault.yaml` declares,
/// set by the **caller** instead — over a copy with the config removed, so
/// the profile route is what produces the result rather than the file.
#[test]
fn golden_vault_bundle_under_a_declared_hub_and_heading_map() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/okf/golden/vault");
    let dir = tempdir().unwrap();
    copy_tree(&fixture, dir.path(), false);
    let root = dir.path().to_path_buf();
    let mut opts = BuildOptions::for_dialect(crate::okf::Dialect::Obsidian);
    opts.profile.hubs.extend([keyword_hub(true)]);
    opts.profile
        .heading_edges
        .insert("Related topics".to_string(), "RELATED_TO".to_string());
    let out = build(&root, &opts).unwrap();

    assert_eq!(
        nodes_with_titles(&out.graph, "Keyword"),
        vec![
            // `faults` in atlas.md and seismic.md against `Faults` once
            ("faults".to_string(), "faults".to_string()),
            // `horizons` once, `Horizons` once — alphabetical
            ("horizons".to_string(), "Horizons".to_string()),
        ]
    );
    assert_eq!(
        out.report.edges_by_type.get("HAS_KEYWORD"),
        Some(&4),
        "two notes × two keywords, with seismic's two casings folded"
    );
    assert_eq!(out.report.edges_by_type.get("RELATED_TO"), Some(&1));
    assert_eq!(
        out.report.edges_by_type.get("RELATED"),
        None,
        "the declared map replaced the ladder's rung"
    );
}

#[test]
fn synthesizes_tag_and_source_nodes() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "a.md",
        "---\ntype: Note\ntags:\n- alpha\n- beta\n---\n# Citations\n[1] [src](https://example.com/x)",
    );
    write(
        dir.path(),
        "b.md",
        "---\ntype: Note\ntags:\n- alpha\n---\nleaf",
    );
    let g = build(dir.path(), &BuildOptions::default()).unwrap().graph;
    assert_eq!(count_label(&g, "Tag"), 2, "alpha, beta");
    assert_eq!(count_label(&g, "Source"), 1, "the cited URL");
    // a→alpha, a→beta, b→alpha (TAGGED) + a→source (CITES) = 4 edges
    assert_eq!(g.graph.edge_count(), 4);
}
