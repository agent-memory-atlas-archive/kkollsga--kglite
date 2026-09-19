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
use crate::okf::model::{BuildOptions, HubSpec, TagLabelSpec, TAGGED_CONN_TYPE, TAG_LABEL};
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

// ---------------------------------------------------------------------------
// `tag_labels:` — the tags a vault models as nodes of their own (VAULT.md §5.5)
// ---------------------------------------------------------------------------

/// `intent/*` → `Intent`, the shape `.kglite/vault.yaml` declares.
fn intent_rule() -> (String, TagLabelSpec) {
    (
        "intent/*".to_string(),
        TagLabelSpec {
            prefix: "intent/".to_string(),
            label: "Intent".to_string(),
            edge: "HAS_INTENT".to_string(),
        },
    )
}

/// Every `(conn type, source, target)` of one type, sorted.
fn edges_typed(out: &crate::okf::build::BuildOutput, conn: &str) -> Vec<(String, String)> {
    edges_of(&out.graph)
        .into_iter()
        .filter(|(_, c, _, _)| c == conn)
        .map(|(source, _, target, _)| (source, target))
        .collect()
}

fn note_property(g: &crate::graph::DirGraph, id: &str, key: &str) -> Option<Value> {
    g.graph.node_indices().find_map(|n| {
        let view = g.node_view(n)?;
        (matches!(view.id().as_ref(), Value::String(s) if s == id))
            .then(|| view.get_property_value(key))
            .flatten()
    })
}

/// The whole of decision 4 for a vault with no `structure:`: a matched tag is
/// a node of its own, joined from the note, and it is gone from the `Tag` hub.
#[test]
fn a_matched_tag_becomes_its_own_node_and_leaves_the_tag_hub() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "a.md",
        "Open the dialog. #intent/create-grid, and #seismic for the hub.\n",
    );
    let out = vault_build_with(dir.path(), |p| {
        p.tag_labels.extend([intent_rule()]);
    });
    assert_eq!(
        nodes_with_titles(&out.graph, "Intent"),
        vec![("create-grid".to_string(), "create-grid".to_string())],
        "the id is the tag text after the prefix"
    );
    assert_eq!(
        nodes_with_titles(&out.graph, TAG_LABEL),
        vec![("seismic".to_string(), "seismic".to_string())],
        "a rule takes its tags out of the hub entirely — no `Tag`, no `TAGGED`"
    );
    assert_eq!(
        edges_typed(&out, "HAS_INTENT"),
        vec![("a".to_string(), "create-grid".to_string())],
        "with no `structure:` the edge comes from the note"
    );
    assert_eq!(
        edges_typed(&out, TAGGED_CONN_TYPE),
        vec![("a".to_string(), "seismic".to_string())]
    );
}

/// The two forms §5.5 names feed one hub, so a rule takes a tag out of it
/// whichever way the note wrote it — and the note's own `tags` list still says
/// what the author typed.
#[test]
fn a_matched_frontmatter_tag_leaves_the_hub_and_stays_in_the_tags_property() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "a.md",
        "---\ntags: [Intent/Create-Grid, seismic]\n---\nprose\n",
    );
    let out = vault_build_with(dir.path(), |p| {
        p.tag_labels.extend([intent_rule()]);
    });
    assert_eq!(
        nodes_with_titles(&out.graph, "Intent"),
        vec![("create-grid".to_string(), "Create-Grid".to_string())],
        "the prefix folds case like the hub does; the title keeps the spelling"
    );
    assert_eq!(
        nodes_with_titles(&out.graph, TAG_LABEL),
        vec![("seismic".to_string(), "seismic".to_string())]
    );
    assert_eq!(
        note_property(&out.graph, "a", "tags"),
        Some(Value::List(vec![
            Value::String("Intent/Create-Grid".to_string()),
            Value::String("seismic".to_string()),
        ])),
        "the property reports the frontmatter verbatim; only the modelling changed"
    );
}

/// Two rules can cover one family of tags, and the narrower one is the one the
/// author meant — never whichever the mapping happened to iterate first.
#[test]
fn the_longest_matching_prefix_wins() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a.md", "#intent/grid/create and #intent/open\n");
    let out = vault_build_with(dir.path(), |p| {
        p.tag_labels.extend([
            intent_rule(),
            (
                "intent/grid/*".to_string(),
                TagLabelSpec {
                    prefix: "intent/grid/".to_string(),
                    label: "GridIntent".to_string(),
                    edge: "HAS_GRID_INTENT".to_string(),
                },
            ),
        ]);
    });
    assert_eq!(
        nodes_with_titles(&out.graph, "GridIntent"),
        vec![("create".to_string(), "create".to_string())]
    );
    assert_eq!(
        nodes_with_titles(&out.graph, "Intent"),
        vec![("open".to_string(), "open".to_string())],
        "`intent/*` still takes the tags `intent/grid/*` does not"
    );
}

/// A declared rule no tag matched is the same finding a `structure:` rule that
/// derived nothing is (VAULT.md §9): the declaration is what the vault means
/// to build, and silence would hide a typo in the prefix.
#[test]
fn a_tag_label_rule_no_tag_matched_is_a_warning() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a.md", "#intents/create-grid is a typo\n");
    let out = vault_build_with(dir.path(), |p| {
        p.tag_labels.extend([intent_rule()]);
    });
    assert!(
        out.report
            .warnings
            .iter()
            .any(|w| w.contains("`tag_labels: intent/*`")),
        "{:?}",
        out.report.warnings
    );
    assert_eq!(count_label(&out.graph, "Intent"), 0);
    assert_eq!(
        nodes_with_titles(&out.graph, TAG_LABEL),
        vec![(
            "intents/create-grid".to_string(),
            "intents/create-grid".to_string()
        )],
        "an unmatched tag is exactly the tag it was before"
    );
}

/// A tag that is nothing but the prefix names no node, so it is not a match
/// and keeps its place in the hub.
#[test]
fn a_tag_that_is_only_the_prefix_stays_an_ordinary_tag() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a.md", "#intent/ alone, and #intent/open\n");
    let out = vault_build_with(dir.path(), |p| {
        p.tag_labels.extend([intent_rule()]);
    });
    assert_eq!(
        nodes_with_titles(&out.graph, "Intent"),
        vec![("open".to_string(), "open".to_string())]
    );
    assert_eq!(
        nodes_with_titles(&out.graph, TAG_LABEL),
        vec![("intent/".to_string(), "intent/".to_string())]
    );
}
