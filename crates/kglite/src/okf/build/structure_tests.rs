//! The derived nodes as the graph holds them (VAULT.md §7.1): ids, inherited
//! properties, `embed_text`, the anchored links that retarget onto them, and
//! the one thing none of them may ever carry — a `file_path`.

use crate::datatypes::values::Value;
use crate::graph::storage::GraphRead;
use crate::graph::DirGraph;
use crate::okf::build::tests_support::{edges_of, nodes_with_titles, vault_build_with, write};
use crate::okf::build::{build, BuildOutput};
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

// ---------------------------------------------------------------------------
// `edge_defaults:` (VAULT.md §7.2)
// ---------------------------------------------------------------------------

/// A vault with one edge of several types: the folder layout (`CONTAINS`), a
/// body link (`LINKS_TO`), a procedure (`HAS_PROCEDURE`/`HAS_STEP`/
/// `NEXT_STEP`) and two chunks (`HAS_CHUNK`/`NEXT_CHUNK`).
fn edge_type_vault(dir: &Path) {
    write(
        dir,
        "guide/steps.md",
        "# Steps\n\n1. Open it.\n2. Close it.\n\nSee [[guide/other]].\n",
    );
    write(dir, "guide/other.md", "# Other\n\nText.\n");
}

fn full_structure(profile: &mut Profile) {
    profile.structure = Some(StructureProfile {
        sections: Some(sections()),
        chunks: Some(ChunkRule {
            max_chars: 40,
            ..chunks()
        }),
        ordered_lists: Some(
            crate::okf::structure::profile::parse(
                &crate::okf::frontmatter::parse_yaml("ordered_lists:\n").unwrap(),
            )
            .unwrap()
            .ordered_lists
            .unwrap(),
        ),
        ..StructureProfile::default()
    });
}

fn derivation_of(g: &DirGraph) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = edges_of(g)
        .into_iter()
        .map(|(_, conn, _, props)| {
            (
                conn,
                props
                    .iter()
                    .find(|(k, _)| k == "derivation")
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default(),
            )
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The `derivation` table `RMS_HelpDesk/build_rms_graph.py:186-192` writes onto
/// every edge it adds, verbatim, plus its `.get(kind, …)` fallback. The
/// acceptance test for `edge_defaults:` is that declaring this reproduces that
/// column on every edge of the types a vault build produces.
const RMS_DERIVATION: &[(&str, &str)] = &[
    ("HAS_TOPIC", "curated_navigation_crosswalk"),
    ("SUPPORTS_ACTION", "normalized_source_verb"),
    ("HAS_GUI_ROUTE", "candidate_topic_action_grouping"),
    ("HAS_API_ROUTE", "candidate_topic_action_grouping"),
    ("HAS_CANDIDATE", "candidate_topic_action_grouping"),
    ("ABOUT_TOPIC", "candidate_topic_action_grouping"),
    ("HAS_ACTION", "candidate_topic_action_grouping"),
    ("CONTAINS", "directory_index_hierarchy"),
    ("MEMBER_OF", "qualified_symbol_name"),
    ("NEXT_STEP", "source_order"),
    ("NEXT_CHUNK", "source_order"),
    ("HAS_UI_ROUTE", "source_access_instruction"),
    ("ANCHORED_IN", "source_access_instruction"),
];
const RMS_FALLBACK: &str = "source_structure";

fn rms_derivation(conn: &str) -> &'static str {
    RMS_DERIVATION
        .iter()
        .find(|(kind, _)| *kind == conn)
        .map(|(_, value)| *value)
        .unwrap_or(RMS_FALLBACK)
}

#[test]
fn the_rms_derivation_table_declared_as_edge_defaults_reproduces_itself() {
    let dir = tempdir().unwrap();
    edge_type_vault(dir.path());
    // What the producer wrote per edge: one lookup in its own table, exactly
    // as its `dict.get(kind, 'source_structure')` does.
    let present: Vec<String> = derivation_of(&vault_build_with(dir.path(), full_structure).graph)
        .into_iter()
        .map(|(conn, _)| conn)
        .collect();
    assert!(
        present.len() >= 6,
        "the fixture exercises several types: {present:?}"
    );
    let out = vault_build_with(dir.path(), |profile| {
        full_structure(profile);
        for conn in &present {
            profile.edge_defaults.insert(
                conn.clone(),
                vec![(
                    "derivation".to_string(),
                    Value::String(rms_derivation(conn).to_string()),
                )],
            );
        }
    });
    assert!(out.report.warnings.is_empty(), "{:?}", out.report.warnings);
    for (source, conn, target, props) in edges_of(&out.graph) {
        let got = props
            .iter()
            .find(|(k, _)| k == "derivation")
            .map(|(_, v)| v.as_str());
        assert_eq!(
            got,
            Some(rms_derivation(&conn)),
            "{source} -[{conn}]-> {target}"
        );
    }
}

#[test]
fn a_default_never_overwrites_a_property_the_edge_already_carries() {
    let dir = tempdir().unwrap();
    edge_type_vault(dir.path());
    let out = vault_build_with(dir.path(), |profile| {
        profile.edge_defaults.insert(
            "LINKS_TO".to_string(),
            vec![
                ("section".to_string(), Value::String("declared".to_string())),
                ("derivation".to_string(), Value::String("prose".to_string())),
            ],
        );
    });
    let link: Vec<(String, String)> = edges_of(&out.graph)
        .into_iter()
        .filter(|(_, conn, _, _)| conn == "LINKS_TO")
        .flat_map(|(_, _, _, props)| props)
        .collect();
    assert!(
        link.contains(&("section".to_string(), "Steps".to_string())),
        "the edge's own value is kept: {link:?}"
    );
    assert!(link.contains(&("derivation".to_string(), "prose".to_string())));
    assert_eq!(
        out.report.warnings,
        vec![
            "`edge_defaults.LINKS_TO.section` names a property a `LINKS_TO` edge \
             already carries; the edge's own value is kept"
                .to_string()
        ],
    );
}

#[test]
fn a_default_for_a_type_the_vault_has_no_edge_of_is_a_warning() {
    let dir = tempdir().unwrap();
    edge_type_vault(dir.path());
    let out = vault_build_with(dir.path(), |profile| {
        profile.edge_defaults.insert(
            "NEXT_STEP".to_string(),
            vec![("derivation".to_string(), Value::String("x".to_string()))],
        );
    });
    assert_eq!(
        out.report.warnings,
        vec![
            "`edge_defaults:` declares `NEXT_STEP`, but the vault has no edge of that type"
                .to_string()
        ],
        "nothing declares `ordered_lists:` here, so the type is never emitted"
    );
}

#[test]
fn a_construct_rule_that_matched_nothing_anywhere_is_a_warning() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "plain.md",
        "# A\n\nProse with no construct in it.\n",
    );
    let out = vault_build_with(dir.path(), |profile| {
        full_structure(profile);
        profile.structure.as_mut().unwrap().callouts = Some(
            crate::okf::structure::profile::parse(
                // Not the spec's default `Note`: the obsidian dialect already
                // labels an untyped note that, and the rule would then read as
                // matched by the notes themselves.
                &crate::okf::frontmatter::parse_yaml("callouts: {label: Admonition}").unwrap(),
            )
            .unwrap()
            .callouts
            .unwrap(),
        );
    });
    assert_eq!(
        out.report.warnings,
        vec![
            "`vault.yaml` declares a `structure:` rule for `Admonition`, but no note's body \
             produced one"
                .to_string(),
            "`vault.yaml` declares a `structure:` rule for `ProcedureStep`, but no note's body \
             produced one"
                .to_string(),
        ],
    );
}

#[test]
fn a_procedure_never_claims_the_heading_a_link_names() {
    // A `Procedure` carries its section's title as its own, so with no
    // `sections:` declared there is nothing a `#Steps` fragment may land on —
    // and the by-title rung of the anchor ladder, which is a section's alone,
    // must not hand it the procedure that sits under that heading.
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "steps.md",
        "# Steps\n\n1. Open it.\n2. Close it.\n",
    );
    write(dir.path(), "links.md", "See [[steps#Steps]].\n");
    let link_targets = |out: BuildOutput| -> Vec<String> {
        edges_of(&out.graph)
            .into_iter()
            .filter(|(_, conn, _, _)| conn == "LINKS_TO")
            .map(|(_, _, target, _)| target)
            .collect()
    };
    let without_sections = vault_build_with(dir.path(), |profile| {
        full_structure(profile);
        profile.structure.as_mut().unwrap().sections = None;
    });
    assert_eq!(
        link_targets(without_sections),
        vec!["steps".to_string()],
        "no `sections:`, so the fragment names nothing derivable and the edge stays on the note"
    );
    assert_eq!(
        link_targets(vault_build_with(dir.path(), full_structure)),
        vec!["steps#Steps".to_string()],
        "with `sections:` it reaches the section, never the procedure under it"
    );
}

/// The second golden vault's whole census, so a rule that quietly stops
/// deriving is a failure here and not only in the Python suite.
#[test]
fn golden_structure_vault_census() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/okf/golden/vault-structure");
    let report = build(
        &root,
        &BuildOptions::for_dialect(crate::okf::Dialect::Obsidian),
    )
    .unwrap()
    .report;
    let nodes: Vec<(&str, usize)> = report
        .nodes_by_label
        .iter()
        .map(|(label, n)| (label.as_str(), *n))
        .collect();
    assert_eq!(
        nodes,
        vec![
            ("Article", 5),
            ("Chunk", 13),
            ("Example", 3),
            ("Folder", 1),
            ("Note", 3),
            ("Procedure", 1),
            ("ProcedureStep", 4),
            ("Section", 11),
        ]
    );
    let edges: Vec<(&str, usize)> = report
        .edges_by_type
        .iter()
        .map(|(conn, n)| (conn.as_str(), *n))
        .collect();
    assert_eq!(
        edges,
        vec![
            ("CONTAINS", 4),
            ("HAS_CHUNK", 13),
            ("HAS_EXAMPLE", 3),
            ("HAS_NOTE", 3),
            ("HAS_PROCEDURE", 1),
            ("HAS_SECTION", 11),
            ("HAS_STEP", 4),
            ("LINKS_TO", 3),
            ("NEXT_CHUNK", 4),
            ("NEXT_SECTION", 3),
            ("NEXT_STEP", 2),
            ("PARENT_SECTION", 6),
        ]
    );
}
