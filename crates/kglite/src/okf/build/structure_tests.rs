//! The derived nodes as the graph holds them (VAULT.md §7.1): ids, inherited
//! properties, `embed_text`, the anchored links that retarget onto them, and
//! the one thing none of them may ever carry — a `file_path`.

use crate::datatypes::values::Value;
use crate::graph::storage::GraphRead;
use crate::graph::DirGraph;
use crate::okf::build::build;
use crate::okf::build::tests_support::{edges_of, nodes_with_titles, vault_build_with, write};
use crate::okf::model::{BuildOptions, BuildReport, Profile};
use crate::okf::structure::profile::{ChunkRule, SectionRule, StructureProfile};
use std::path::Path;
use std::sync::Arc;
use tempfile::tempdir;

fn sections() -> SectionRule {
    SectionRule {
        label: "Section".to_string(),
        edge: "HAS_SECTION".to_string(),
        parent: "PARENT_SECTION".to_string(),
        next: "NEXT_SECTION".to_string(),
    }
}

fn chunks() -> ChunkRule {
    ChunkRule {
        label: "Chunk".to_string(),
        edge: "HAS_CHUNK".to_string(),
        next: "NEXT_CHUNK".to_string(),
        max_words: 650,
        max_chars: 6000,
    }
}

fn with_sections(profile: &mut Profile) {
    profile.structure = Some(StructureProfile {
        sections: Some(sections()),
        ..StructureProfile::default()
    });
}

fn with_sections_and_chunks(profile: &mut Profile) {
    profile.structure = Some(StructureProfile {
        sections: Some(sections()),
        chunks: Some(chunks()),
        ..StructureProfile::default()
    });
}

/// Every property of the node with this id, sorted.
fn props_of(g: &DirGraph, id: &str) -> Vec<(String, Value)> {
    let idx = g
        .graph
        .node_indices()
        .find(|&n| {
            g.node_view(n)
                .and_then(|nd| match nd.id().into_owned() {
                    Value::String(s) => Some(s),
                    _ => None,
                })
                .as_deref()
                == Some(id)
        })
        .unwrap_or_else(|| panic!("no node `{id}`"));
    let view = g.node_view(idx).unwrap();
    let mut props = view.property_pairs_named(&g.interner);
    props.retain(|(_, v)| !matches!(v, Value::Null));
    props.sort_by(|a, b| a.0.cmp(&b.0));
    props
}

fn property(g: &DirGraph, id: &str, name: &str) -> Option<Value> {
    props_of(g, id)
        .into_iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v)
}

#[test]
fn a_derived_node_never_carries_a_file_path() {
    let dir = tempdir().unwrap();
    write(dir.path(), "note.md", "# One\n\ntext\n\n## Two\n\nmore\n");
    let g = vault_build_with(dir.path(), with_sections_and_chunks).graph;
    for id in ["note#One", "note#One#Two", "note#One~chunk1"] {
        let props = props_of(&g, id);
        assert!(
            !props.iter().any(|(k, _)| k == "file_path"),
            "`{id}` is not a file, and the exporter writes every node that has \
             a path (VAULT.md §10.1): {props:?}"
        );
    }
    assert!(
        property(&g, "note", "file_path").is_some(),
        "the note itself still has one"
    );
}

/// The parse holds derived ids as suffixes precisely for this: `resolve_ids`
/// rewrites a colliding note id *after* the body was read, and a derived id
/// minted during the parse would name a note that no longer exists.
#[test]
fn derived_ids_follow_an_id_the_collision_pass_moved() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a/note.md", "# One\n");
    write(dir.path(), "b/note.md", "# Two\n");
    let g = vault_build_with(dir.path(), with_sections).graph;
    let ids: Vec<String> = nodes_with_titles(&g, "Section")
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        ids,
        vec!["a/note#One".to_string(), "b/note#Two".to_string()]
    );
}

#[test]
fn sections_and_chunks_are_labelled_nodes_joined_to_their_note() {
    let dir = tempdir().unwrap();
    write(dir.path(), "note.md", "# One\n\ntext\n\n## Two\n\nmore\n");
    let g = vault_build_with(dir.path(), with_sections_and_chunks).graph;
    assert_eq!(
        nodes_with_titles(&g, "Section"),
        vec![
            ("note#One".to_string(), "One".to_string()),
            ("note#One#Two".to_string(), "Two".to_string()),
        ]
    );
    let edges: Vec<(String, String, String)> = edges_of(&g)
        .into_iter()
        .map(|(s, conn, t, _)| (s, conn, t))
        .collect();
    assert!(edges.contains(&(
        "note".to_string(),
        "HAS_SECTION".to_string(),
        "note#One".to_string()
    )));
    assert!(edges.contains(&(
        "note#One".to_string(),
        "HAS_CHUNK".to_string(),
        "note#One~chunk1".to_string()
    )));
    assert_eq!(
        property(&g, "note#One~chunk1", "section_id"),
        Some(Value::String("note#One".to_string()))
    );
    assert_eq!(
        property(&g, "note#One~chunk1", "note_id"),
        Some(Value::String("note".to_string()))
    );
}

#[test]
fn inherit_copies_the_notes_own_frontmatter() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "note.md",
        "---\ncorpus: rms\n---\n# One\n\ntext\n",
    );
    let g = vault_build_with(dir.path(), |profile| {
        profile.structure = Some(StructureProfile {
            sections: Some(sections()),
            chunks: Some(chunks()),
            inherit: vec!["corpus".to_string(), "absent".to_string()],
            ..StructureProfile::default()
        });
    })
    .graph;
    for id in ["note#One", "note#One~chunk1"] {
        assert_eq!(
            property(&g, id, "corpus"),
            Some(Value::String("rms".to_string()))
        );
        assert_eq!(
            property(&g, id, "absent"),
            None,
            "a key the note does not carry is simply absent"
        );
    }
}

#[test]
fn embed_text_is_materialised_from_the_template() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "note.md",
        "---\ntitle: The Note\n---\n# One\n\n## Two\n\nbody text\n",
    );
    let g = vault_build_with(dir.path(), |profile| {
        profile.structure = Some(StructureProfile {
            sections: Some(sections()),
            chunks: Some(chunks()),
            embed_text: Some("{title} | {heading_path} | {section_title} | {id}\n\n{text}".into()),
            ..StructureProfile::default()
        });
    })
    .graph;
    assert_eq!(
        property(&g, "note#One#Two~chunk1", "embed_text"),
        Some(Value::String(
            "The Note | One > Two | Two | note#One#Two~chunk1\n\nbody text".to_string()
        )),
        "`{{title}}` is the note's, `{{section_title}}` the chunk's own section"
    );
}

#[test]
fn declared_types_reach_a_derived_property() {
    let dir = tempdir().unwrap();
    write(dir.path(), ".kglite/vault.yaml", DECLARED_TYPES_CONFIG);
    write(dir.path(), "note.md", "# One\n\ntext\n");
    let out = build(
        dir.path(),
        &BuildOptions::for_dialect(crate::okf::Dialect::Obsidian),
    )
    .unwrap();
    assert_eq!(
        property(&out.graph, "note#One", "level"),
        Some(Value::String("1".to_string())),
        "`types:` decides what a derived column is, as it does a note's"
    );
    assert!(
        !out.report
            .warnings
            .iter()
            .any(|w| w.contains("no note carries")),
        "a declaration a derived label carries is matched, not reported \
         unmatched: {:?}",
        out.report.warnings
    );
}

const DECLARED_TYPES_CONFIG: &str = "\
kglite_vault: 1
structure:
  sections: {}
types:
  Section: {level: string}
";

#[test]
fn a_rule_that_matched_nothing_is_a_warning() {
    let dir = tempdir().unwrap();
    write(dir.path(), "note.md", "no headings here\n");
    let report = vault_build_with(dir.path(), with_sections_and_chunks).report;
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("structure:") && w.contains("Section")),
        "{:?}",
        report.warnings
    );
    assert!(
        !report.warnings.iter().any(|w| w.contains("`Chunk`")),
        "the note's one paragraph *did* make a chunk: {:?}",
        report.warnings
    );
}

// ── Anchored links retarget (VAULT.md §5.4) ────────────────────────────────

fn linking_vault(link: &str) -> (Arc<DirGraph>, BuildReport) {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "target.md",
        "# Top\n\n## Sub\n\nprose ^anchor-1\n\n# Other\n\n## Sub\n",
    );
    write(dir.path(), "source.md", &format!("Read {link}.\n"));
    let out = vault_build_with(dir.path(), with_sections_and_chunks);
    (out.graph, out.report)
}

/// Every `(target id, anchor property)` `source.md` wrote.
fn source_links(g: &DirGraph) -> Vec<(String, String)> {
    edges_of(g)
        .into_iter()
        .filter(|(s, conn, _, _)| s == "source" && conn == "LINKS_TO")
        .map(|(_, _, t, props)| {
            (
                t,
                props
                    .into_iter()
                    .find(|(k, _)| k == "anchor")
                    .map(|(_, v)| v)
                    .unwrap_or_default(),
            )
        })
        .collect()
}

#[test]
fn a_full_heading_path_retargets_to_its_section() {
    let (g, _) = linking_vault("[[target#Top#Sub]]");
    assert_eq!(
        source_links(&g),
        vec![("target#Top#Sub".to_string(), "Top#Sub".to_string())],
        "the edge moves onto the Section and keeps the fragment as written"
    );
}

/// Obsidian resolves a bare heading to the **first** of that text and has no
/// syntax for a later one (VAULT.md §7.1).
#[test]
fn a_bare_heading_retargets_to_the_first_section_of_that_title() {
    let (g, _) = linking_vault("[[target#Sub]]");
    assert_eq!(
        source_links(&g),
        vec![("target#Top#Sub".to_string(), "Sub".to_string())]
    );
}

#[test]
fn a_heading_link_matches_case_insensitively() {
    let (g, _) = linking_vault("[[target#top#sub]]");
    assert_eq!(
        source_links(&g),
        vec![("target#Top#Sub".to_string(), "top#sub".to_string())]
    );
}

#[test]
fn a_block_id_retargets_to_its_chunk() {
    let (g, _) = linking_vault("[[target#^anchor-1]]");
    assert_eq!(
        source_links(&g),
        vec![("target#^anchor-1".to_string(), "^anchor-1".to_string())]
    );
}

#[test]
fn a_fragment_naming_nothing_stays_on_the_note_and_warns() {
    let (g, report) = linking_vault("[[target#Nowhere]]");
    assert_eq!(
        source_links(&g),
        vec![("target".to_string(), "Nowhere".to_string())]
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("`#Nowhere` names no heading or block id in `target`")),
        "{:?}",
        report.warnings
    );
}

#[test]
fn a_link_without_a_fragment_is_untouched() {
    let (g, report) = linking_vault("[[target]]");
    assert_eq!(
        source_links(&g),
        vec![("target".to_string(), String::new())]
    );
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.contains("names no heading")),
        "{:?}",
        report.warnings
    );
}

/// The whole feature is opt-in: a vault that declares no `structure:` builds
/// the graph it built before, anchor links included.
#[test]
fn without_a_rule_an_anchored_link_stays_on_the_note() {
    let dir = tempdir().unwrap();
    write(dir.path(), "target.md", "# Top\n\n## Sub\n");
    write(dir.path(), "source.md", "Read [[target#Sub]].\n");
    let out = build(
        dir.path(),
        &BuildOptions::for_dialect(crate::okf::Dialect::Obsidian),
    )
    .unwrap();
    assert_eq!(
        source_links(&out.graph),
        vec![("target".to_string(), "Sub".to_string())]
    );
    assert!(out.report.warnings.is_empty(), "{:?}", out.report.warnings);
}

/// The committed structure vault, whose Python counterpart asserts the graph.
/// This half is the report: the two warnings it is built to produce.
#[test]
fn golden_structure_vault_report() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/okf/golden/vault-structure");
    let report = build(
        &root,
        &BuildOptions::for_dialect(crate::okf::Dialect::Obsidian),
    )
    .unwrap()
    .report;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(
        report.warnings,
        vec![
            "structure/duplicate.md: duplicate heading path `Notes#Details`: a link cannot \
             reach the second one, which takes the id `#Notes#Details~2` — give it a \
             `^block-id` (VAULT.md §5.7)"
                .to_string(),
            "structure/links.md: `#Nowhere` names no heading or block id in `chunky`; \
             the link resolved to the note"
                .to_string(),
        ],
    );
}
