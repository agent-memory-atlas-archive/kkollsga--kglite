//! The build as a whole: what a bundle becomes, and what the report says it
//! became.

use crate::graph::storage::GraphRead;
use crate::okf::build::build;
use crate::okf::build::tests_support::{
    copy_tree, labels_by_id, provisional_count, vault_build_with, write,
};
use crate::okf::model::{
    BuildOptions, ATTACHMENT_LABEL, CONTAINS_CONN_TYPE, DEFAULT_LABEL, EMBEDS_CONN_TYPE,
    FOLDER_LABEL, FOLDER_NOTE_CONN_TYPE, HAS_ATTACHMENT_CONN_TYPE, HAS_IMAGE_CONN_TYPE,
    IMAGE_LABEL, TAGGED_CONN_TYPE, TAG_LABEL,
};
use std::collections::BTreeMap;
use std::path::Path;
use tempfile::tempdir;

#[test]
fn builds_nodes_edges_and_dangling_stub() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "a.md",
        "---\ntype: Note\n---\nSee [b](b.md) and [gone](missing.md).",
    );
    write(dir.path(), "b.md", "---\ntype: Note\n---\nleaf");

    let g = build(dir.path(), &BuildOptions::default()).unwrap().graph;
    // a, b, + vivified `missing` stub = 3 nodes.
    assert_eq!(g.graph.node_indices().count(), 3);
    // a→b and a→missing = 2 edges.
    assert_eq!(g.graph.edge_count(), 2);
    assert_eq!(provisional_count(&g), 1, "missing.md is a provisional stub");
}

#[test]
fn vault_report_carries_the_collision_findings() {
    let dir = tempdir().unwrap();
    write(dir.path(), "projects/alpha.md", "prose");
    write(dir.path(), "archive/alpha.md", "prose");
    write(dir.path(), "notes/Roadmap.md", "prose");
    write(dir.path(), "plans/roadmap.md", "prose");
    let opts = BuildOptions::for_dialect(crate::okf::Dialect::Obsidian);
    let r = build(dir.path(), &opts).unwrap().report;
    assert_eq!(r.errors.len(), 1, "the stem collision: {:?}", r.errors);
    assert_eq!(r.warnings.len(), 1, "the case clash: {:?}", r.warnings);
}

/// The committed vault bundle, whose Python counterpart
/// (`tests/test_okf.py::TestVaultGoldenBundle`) asserts the graph it makes.
/// This one asserts the half Python cannot reach yet: the build report.
///
/// The bundle carries a `.kglite/vault.yaml`, so every number here is the
/// *declared* vault's, not the bare dialect's:
/// `golden_vault_bundle_without_its_config` holds the other half.
#[test]
fn golden_vault_bundle_report() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/okf/golden/vault");
    let opts = BuildOptions::for_dialect(crate::okf::Dialect::Obsidian);
    let r = build(&root, &opts).unwrap().report;

    assert_eq!(r.files_scanned, 13, "`.kglite/` is never walked");
    assert_eq!(r.concepts, 13, "a note needs no frontmatter in a vault");
    assert_eq!(r.dangling, 1, "`[[Missing]]`, named by a `depends_on:`");
    assert_eq!(r.folder_notes, 1, "`projects.md` stands in for `projects/`");
    assert_eq!(
        r.nodes_by_label,
        BTreeMap::from([
            // `default_label: Article` sits ahead of the folder rung, so
            // every note without a `type:` is one
            ("Article".to_string(), 11),
            ("Initiative".to_string(), 2), // atlas.md and seismic.md, from `type:`
            // notes, notes/deep, archive — `projects/` has a folder note
            (FOLDER_LABEL.to_string(), 3),
            (TAG_LABEL.to_string(), 3), // seismic, plus two inline `#tag`s
            // `faults` and `horizons`, folded from five spellings by the
            // declared `case_insensitive` hub
            ("Keyword".to_string(), 2),
            (DEFAULT_LABEL.to_string(), 1), // the `[[Missing]]` stub
            // img/diagram.png and img/faults.png (VAULT.md §6)
            (IMAGE_LABEL.to_string(), 2),
            // img/handbook.pdf, plus the absent img/appendix.pdf
            (ATTACHMENT_LABEL.to_string(), 2),
        ])
    );
    assert_eq!(
        r.edges_by_type,
        BTreeMap::from([
            (CONTAINS_CONN_TYPE.to_string(), 8),
            ("LINKS_TO".to_string(), 6),
            // the `## Related topics` heading, retyped by `heading_edges`
            ("RELATED_TO".to_string(), 1),
            (EMBEDS_CONN_TYPE.to_string(), 1), // `![[old]]`
            // four notes under the `projects` folder note, plus the
            // reserved `parent: "[[atlas]]"`
            (FOLDER_NOTE_CONN_TYPE.to_string(), 5),
            ("DEPENDS_ON".to_string(), 2), // the wikilink-valued key
            (TAGGED_CONN_TYPE.to_string(), 4),
            // two notes × two folded keywords
            ("HAS_KEYWORD".to_string(), 4),
            // links.md reaches both images; seismic.md re-reaches faults.png
            (HAS_IMAGE_CONN_TYPE.to_string(), 3),
            // index.md → handbook.pdf, links.md → the absent appendix
            (HAS_ATTACHMENT_CONN_TYPE.to_string(), 2),
        ])
    );
    assert_eq!(r.missing_attachments, 1, "`img/appendix.pdf`");
    assert_eq!(r.ambiguous_attachments, 0);

    // What `.kglite/` declared and carried (VAULT.md §7, §8).
    assert_eq!(
        r.indexes_declared, 2,
        "concept_id, and toc_depth as a range"
    );
    assert_eq!(r.text_indexes_built, 1, "Initiative.body");
    assert_eq!(
        r.embed_targets,
        vec![("Article".to_string(), "body".to_string())]
    );
    assert_eq!(r.skills_imported, 1);
    assert_eq!(r.recipes_imported, 1);

    assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
    assert!(
        r.errors[0].contains("`alpha`")
            && r.errors[0].contains("notes/alpha.md")
            && r.errors[0].contains("projects/alpha.md"),
        "{}",
        r.errors[0]
    );
    assert_eq!(r.warnings.len(), 3, "{:?}", r.warnings);
    assert!(
        r.warnings[0].contains("`Roadmap` (projects/Roadmap.md)")
            && r.warnings[0].contains("`roadmap` (notes/roadmap.md)"),
        "{}",
        r.warnings[0]
    );
    // Attachments are resolved before the link edges, so their warnings
    // land between the id findings and the dangling links.
    assert_eq!(r.warnings[1], "missing attachment: `img/appendix.pdf`");
    assert_eq!(r.warnings[2], "dangling link: `Missing`");
}

/// The same bundle with its `.kglite/vault.yaml` moved out of reach — the
/// control for the test above. Every difference between the two is the
/// config doing something, which is what makes each declaration in the
/// fixture non-vacuous.
#[test]
fn golden_vault_bundle_without_its_config() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/okf/golden/vault");
    let dir = tempdir().unwrap();
    copy_tree(&root, dir.path(), false);
    let opts = BuildOptions::for_dialect(crate::okf::Dialect::Obsidian);
    let r = build(dir.path(), &opts).unwrap().report;

    assert_eq!(
        r.nodes_by_label,
        BTreeMap::from([
            ("Note".to_string(), 2),
            ("Initiative".to_string(), 2),
            ("projects".to_string(), 2),
            ("notes".to_string(), 6),
            ("archive".to_string(), 1),
            (FOLDER_LABEL.to_string(), 3),
            (TAG_LABEL.to_string(), 3),
            (DEFAULT_LABEL.to_string(), 1),
            (IMAGE_LABEL.to_string(), 2),
            (ATTACHMENT_LABEL.to_string(), 2),
        ]),
        "no `default_label`, no `keywords` hub"
    );
    assert_eq!(
        r.edges_by_type.get("RELATED"),
        Some(&1),
        "the built-in ladder types the `## Related topics` links"
    );
    assert_eq!(r.edges_by_type.get("HAS_KEYWORD"), None);
    assert_eq!(r.indexes_declared, 0);
    assert_eq!(r.text_indexes_built, 0);
    assert!(r.embed_targets.is_empty());
    assert_eq!(r.skills_imported, 0, "no `.kglite/skills/` was copied");
    assert_eq!(r.recipes_imported, 0);
}

#[test]
fn report_counts_every_node_and_edge_the_build_made() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "a.md",
        "---\ntype: Note\ntags:\n- alpha\n---\nSee [gone](missing.md).\n# Citations\n[src](https://example.com)",
    );
    write(dir.path(), "sub/b.md", "---\ntype: Note\n---\nleaf");
    write(dir.path(), "plain.md", "no frontmatter");

    let out = build(dir.path(), &BuildOptions::default()).unwrap();
    let r = &out.report;
    assert_eq!(r.files_scanned, 3);
    assert_eq!(r.concepts, 2, "plain.md has no frontmatter");
    assert_eq!(r.dangling, 1, "missing.md");
    assert_eq!(
        r.nodes_by_label,
        BTreeMap::from([
            ("Note".to_string(), 2),
            ("Tag".to_string(), 1),
            ("Source".to_string(), 1),
            ("Folder".to_string(), 1),
            (DEFAULT_LABEL.to_string(), 1),
        ])
    );
    assert_eq!(
        r.edges_by_type,
        BTreeMap::from([
            ("LINKS_TO".to_string(), 1),
            ("CITES".to_string(), 1),
            ("TAGGED".to_string(), 1),
            ("CONTAINS".to_string(), 1),
        ])
    );
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(
        r.warnings,
        vec!["dangling link: `missing`".to_string()],
        "the stub is reported as the warning VAULT.md §9 classifies it as"
    );
    // The report must describe the graph that was actually built.
    assert_eq!(
        out.graph.graph.node_indices().count(),
        r.nodes_by_label.values().sum::<usize>()
    );
    assert_eq!(
        out.graph.graph.edge_count(),
        r.edges_by_type.values().sum::<usize>()
    );
}

#[test]
fn the_profile_prunes_directories_too() {
    let dir = tempdir().unwrap();
    write(dir.path(), "keep/a.md", "kept");
    write(dir.path(), "drafts/b.md", "pruned");
    let out = vault_build_with(dir.path(), |p| {
        p.skip_dirs.push("drafts".to_string());
    });
    assert_eq!(out.report.files_scanned, 1);
    assert_eq!(
        labels_by_id(&out.graph).keys().cloned().collect::<Vec<_>>(),
        vec!["a".to_string(), "keep".to_string()],
        "the note and its Folder, and nothing from `drafts/`"
    );
}

#[test]
fn empty_bundle_is_empty_graph() {
    let dir = tempdir().unwrap();
    let g = build(dir.path(), &BuildOptions::default()).unwrap().graph;
    assert_eq!(g.graph.node_indices().count(), 0);
}
