//! The round-trip contract (VAULT.md §10.9) as a differential corpus.
//!
//! Two properties, over a list of vaults rather than one fixture:
//!
//! - **`export ∘ import ∘ export`** is byte-identical: exporting a graph,
//!   reading the vault back and exporting *that* produces the same tree, file
//!   for file and byte for byte, the manifest included.
//! - **`import ∘ export ∘ import ≡ import`**, apart from §10.9's documented
//!   losses.
//!
//! The losses are taken **once**, on the way out of the author's vault, so the
//! two are asserted together by running three rounds: every vault's second and
//! third export must be byte-identical and their graphs equal — a loss that
//! kept happening would show up there as unbounded drift — and a vault taking
//! no loss that moves bytes must be identical from the *first* export. Then
//! each loss §10.9 names is asserted below as a difference between the first
//! graph and the second that *must* exist. A loss the spec claims and the code
//! does not take is a wrong spec, so the two refinements this phase found
//! ([`a_declared_int_survives_the_round_trip`],
//! [`a_body_stated_edge_keeps_its_properties`]) are asserted as survivals, and
//! §10.9 was rewritten to say so.
//!
//! [`Shape`] is what "graph-equal" means here: the label counter, the
//! edge-type counter, the per-type edge-property counter and every node's
//! property map, rendered to strings so two independently built graphs compare
//! by value.

use super::*;
use crate::okf::model::{BuildOptions, BuildReport, Dialect};
use std::collections::BTreeMap;
use std::sync::Arc;

// ── the harness ────────────────────────────────────────────────────────────

fn golden_vault() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/okf/golden/vault")
}

fn vault_opts() -> BuildOptions {
    BuildOptions::for_dialect(Dialect::Obsidian)
}

fn build_vault(dir: &Path) -> Arc<DirGraph> {
    crate::okf::build(dir, &vault_opts()).unwrap().graph
}

fn write_vault(root: &Path, files: &[(&str, &str)]) {
    for (rel, text) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

/// A vault of `(relative path, contents)` pairs under its own temp directory.
fn vault_of(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_vault(dir.path(), files);
    dir
}

/// Every file's bytes under `dir`, vault-relative — the whole tree as one
/// comparable value, `.kglite/export-manifest.json` included.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    collect_files(dir, dir, &mut out);
    out
}

fn collect_files(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_files(root, &path, out);
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel, std::fs::read(&path).unwrap());
        }
    }
}

/// A graph reduced to what two independent builds of one vault must agree on.
#[derive(Debug, Default, PartialEq, Eq)]
struct Shape {
    labels: BTreeMap<String, usize>,
    edges: BTreeMap<String, usize>,
    /// Edge properties carried, per edge type — §10.9's first loss is measured
    /// here, and so is the prose re-derivation that undoes it.
    edge_props: BTreeMap<String, usize>,
    nodes: BTreeMap<(String, String), BTreeMap<String, String>>,
}

impl Shape {
    fn of(graph: &DirGraph) -> Shape {
        let _arena_guard = graph.graph.begin_query();
        let mut shape = Shape::default();
        for idx in graph.graph.node_indices() {
            let Some(view) = graph.node_view(idx) else {
                continue;
            };
            let label = view.node_type_str(&graph.interner).to_string();
            *shape.labels.entry(label.clone()).or_default() += 1;
            let id = crate::datatypes::values::raw_string(&view.id());
            let props: BTreeMap<String, String> = view
                .property_pairs_named(&graph.interner)
                .into_iter()
                .filter(|(_, v)| !matches!(v, Value::Null))
                .map(|(k, v)| (k, crate::datatypes::values::raw_string(&v)))
                .collect();
            shape.nodes.insert((label, id), props);
        }
        for edge in graph.graph.edge_indices() {
            if let Some(data) = graph.graph.edge_weight(edge) {
                let conn = data.connection_type_str(&graph.interner).to_string();
                *shape.edges.entry(conn.clone()).or_default() += 1;
                *shape.edge_props.entry(conn).or_default() += data.properties.len();
            }
        }
        shape
    }

    fn label(&self, label: &str) -> usize {
        self.labels.get(label).copied().unwrap_or(0)
    }

    fn edge(&self, conn: &str) -> usize {
        self.edges.get(conn).copied().unwrap_or(0)
    }

    /// One node's properties, or `None` when no node carries that identity.
    fn props(&self, label: &str, id: &str) -> Option<&BTreeMap<String, String>> {
        self.nodes.get(&(label.to_string(), id.to_string()))
    }
}

/// Three rounds of `export ∘ import`, from a vault or from a bare graph: the
/// trees each export wrote and the graph each import built.
///
/// The temp directories are kept alive by the value — dropping one would
/// remove the tree its snapshot was taken from.
struct Trip {
    first: Shape,
    second: Shape,
    third: Shape,
    /// The three exported trees, in order.
    exports: Vec<BTreeMap<String, Vec<u8>>>,
    report: ExportReport,
    /// The build report of the re-import — where the `vault.yaml` losses show.
    second_report: BuildReport,
    _dirs: Vec<tempfile::TempDir>,
}

impl Trip {
    /// Run the trip from a source vault, copying its attachments in.
    fn from_vault(source: &Path) -> Trip {
        Trip::run(source, true)
    }

    /// Run it without naming the source root, so no attachment bytes travel.
    fn rootless(source: &Path) -> Trip {
        Trip::run(source, false)
    }

    fn run(source: &Path, with_root: bool) -> Trip {
        let graph = build_vault(source);
        let first = Shape::of(&graph);
        let root = with_root.then(|| source.to_path_buf());
        Trip::from_graph(&graph, root, first)
    }

    fn from_graph(graph: &DirGraph, source_root: Option<PathBuf>, first: Shape) -> Trip {
        let carry_root = source_root.is_some();
        let dirs: Vec<tempfile::TempDir> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
        let mut root = source_root;
        let mut report = None;
        let mut shapes = Vec::new();
        let mut second_report = None;
        let mut exports = Vec::new();
        // Each export reads its attachments from the vault the previous one
        // wrote — the only root the graph it is exporting was ever built from.
        let mut current = Arc::new(graph.clone());
        for (round, dir) in dirs.iter().enumerate() {
            let done = export(
                &current,
                dir.path(),
                &ExportOptions {
                    source_root: root.take(),
                    ..ExportOptions::default()
                },
            )
            .unwrap();
            exports.push(snapshot(dir.path()));
            let built = crate::okf::build(dir.path(), &vault_opts()).unwrap();
            if round == 0 {
                report = Some(done);
                second_report = Some(built.report.clone());
            }
            shapes.push(Shape::of(&built.graph));
            root = carry_root.then(|| dir.path().to_path_buf());
            current = built.graph;
        }
        let mut shapes = shapes.into_iter();
        Trip {
            exports,
            first,
            second: shapes.next().unwrap(),
            third: shapes.next().unwrap(),
            report: report.unwrap(),
            second_report: second_report.unwrap(),
            _dirs: dirs,
        }
    }

    /// The properties every vault in the corpus must satisfy: an exported tree
    /// is a fixed point of `export ∘ import`, and so is the graph it builds.
    ///
    /// The comparison starts at the *second* export, because §10.9's losses are
    /// taken on the way out of the author's vault and one of them — a `types:`
    /// declaration §4.2's inference does not agree with — moves the bytes once.
    /// [`Trip::assert_byte_identical_from_the_first_export`] adds the stronger
    /// claim for a vault that takes no such loss.
    fn assert_stable(&self, name: &str) {
        self.assert_trees_match(name, 1, 2);
        assert_eq!(
            self.second, self.third,
            "{name}: the graph drifted on the second round trip, so the losses are not one-shot"
        );
    }

    /// The whole chain is byte-identical, from the very first export.
    fn assert_byte_identical_from_the_first_export(&self, name: &str) {
        self.assert_trees_match(name, 0, 1);
        self.assert_stable(name);
    }

    fn assert_trees_match(&self, name: &str, a: usize, b: usize) {
        let (left, right) = (&self.exports[a], &self.exports[b]);
        assert_eq!(
            left.keys().collect::<Vec<_>>(),
            right.keys().collect::<Vec<_>>(),
            "{name}: export {a} and export {b} wrote different file lists"
        );
        // Content before the manifest: the manifest is one hash per file, so a
        // single changed note reddens it too and a diff of hashes says nothing
        // about what moved.
        let (manifest, content): (Vec<_>, Vec<_>) = left
            .iter()
            .partition(|(rel, _)| rel.ends_with(MANIFEST_FILE));
        for (rel, bytes) in content.into_iter().chain(manifest) {
            assert_eq!(
                String::from_utf8_lossy(bytes),
                String::from_utf8_lossy(&right[rel]),
                "{name}: {rel} differs between export {a} and export {b}"
            );
        }
    }
}

// ── the corpus ─────────────────────────────────────────────────────────────

/// Nested maps, every class of string the quoting rule has to protect, the
/// date inference §4.2 runs at the top level but not inside a sequence, and a
/// `types:` declaration holding one date-like string as text.
const TYPING: &[(&str, &str)] = &[
    (
        ".kglite/vault.yaml",
        "kglite_vault: 1\ndefault_label: Note\ntypes:\n  Note: {stays_text: string}\n",
    ),
    (
        "notes/typed.md",
        "---\n\
         title: Typed values\n\
         metadata:\n  source: vendor\n  nested:\n    deep: 3\n\
         count: 7\n\
         ratio: 1.5\n\
         whole: 2.0\n\
         flag: true\n\
         released: 2026-01-15\n\
         stamp: 2026-01-15T08:30:00Z\n\
         stays_text: \"2026-02-20\"\n\
         looks_numeric: \"12\"\n\
         looks_hex: \"0x1f\"\n\
         looks_bool: \"yes\"\n\
         looks_null: \"Null\"\n\
         leading_dash: \"- not a sequence\"\n\
         colon_pair: \"key: value\"\n\
         trailing_colon: \"ends:\"\n\
         comment_opener: \"text # not a comment\"\n\
         padded: \"  padded  \"\n\
         empty: \"\"\n\
         brace: \"{not a map}\"\n\
         at_sign: \"@handle\"\n\
         tags: [alpha, 2026-01-15]\n\
         ---\n\
         Prose about typed values.\n",
    ),
];

/// Both folder-note spellings, under a folder that *is* the label so §10.2
/// preserves them; `parent:` extras; and two notes sharing a stem, which forces
/// the folder-qualified `[[Label/Stem]]` spelling on every link to them.
const LAYOUT: &[(&str, &str)] = &[
    (
        "Docs/Guides.md",
        "---\ntitle: Guides\n---\nThe folder note for `Docs/Guides/`, the `X.md` spelling.\n",
    ),
    (
        "Docs/Survey/Survey.md",
        "---\ntitle: Survey\n---\nThe folder note for `Docs/Survey/`, the `X/X.md` spelling.\n",
    ),
    (
        "Docs/Guides/Intro.md",
        "---\nparent: \"[[Survey]]\"\ndepends_on: \"[[Docs/Guides/Faults]]\"\n---\n\
         Cross-listed under Survey as well as its own folder.\n",
    ),
    (
        "Docs/Guides/Faults.md",
        "Its stem collides with `Docs/Survey/Faults.md`, so both fall back to their paths.\n",
    ),
    (
        "Docs/Survey/Faults.md",
        "The other Faults. A link to either has to name the folder.\n",
    ),
];

/// Attachments: an image reached note-relative, one reached on the bare
/// filename rung, a non-image attachment, and one that is simply not there.
const MEDIA: &[(&str, &str)] = &[
    (
        "Media/figures.md",
        "---\ntitle: Figures\n---\n\
         ## Plates\n\n\
         ![Fault map](../img/faults.png) is note-relative, ![[diagram.png]] resolves\n\
         on the bare filename, and ![[handbook.pdf]] is not an image at all.\n\n\
         ## Gaps\n\n\
         ![missing](../img/absent.png) resolves to nothing.\n",
    ),
    ("img/faults.png", "stand-in bytes\n"),
    ("img/diagram.png", "other stand-in bytes\n"),
    ("img/handbook.pdf", "not a pdf either\n"),
];

// ── the two properties, over the whole corpus ──────────────────────────────

#[test]
fn every_corpus_vault_reaches_a_fixed_point() {
    // `typing` is the one that declares a `types:` entry §4.2 re-infers
    // differently, so only it takes a loss between the first export and the
    // second; every other vault is byte-identical from the first.
    Trip::from_vault(&golden_vault()).assert_byte_identical_from_the_first_export("golden");
    for (name, files) in [("layout", LAYOUT), ("media", MEDIA)] {
        let dir = vault_of(files);
        Trip::from_vault(dir.path()).assert_byte_identical_from_the_first_export(name);
    }
    let dir = vault_of(TYPING);
    Trip::from_vault(dir.path()).assert_stable("typing");
    // Without a source root the attachments do not travel, which is a
    // different second vault — and it has to be a fixed point too.
    let dir = vault_of(MEDIA);
    Trip::rootless(dir.path())
        .assert_byte_identical_from_the_first_export("media (no source root)");
}

#[test]
fn a_graph_that_was_never_a_vault_reaches_a_fixed_point() {
    let graph = cypher_graph();
    Trip::from_graph(&graph, None, Shape::of(&graph))
        .assert_byte_identical_from_the_first_export("cypher");
}

/// A graph built by `CREATE`, carrying no `file_path` anywhere: every node in
/// it is a note (§10.1), and nothing about it came from a vault — so nothing
/// the export drops can be re-derived from prose it never wrote.
fn cypher_graph() -> DirGraph {
    let mut graph = crate::graph::storage::mode::new_dir_graph_in_mode(
        crate::graph::storage::mode::StorageMode::Memory,
        None,
    )
    .unwrap();
    let params = std::collections::HashMap::new();
    crate::graph::session::execute::execute_mut(
        &mut graph,
        "CREATE (a:Paper {concept_id: 'alpha', title: 'Alpha', body: 'The first paper.', \
         year: 2026, score: 1.5, open: true}), \
         (b:Paper {concept_id: 'beta', title: 'Beta', body: 'The second paper.', year: 2025}), \
         (a)-[:CITES {section: 'Background', anchor: 'intro'}]->(b)",
        &crate::graph::session::execute::ExecuteOptions::eager(&params),
    )
    .unwrap();
    graph
}

// ── §10.9, loss by loss ────────────────────────────────────────────────────

/// Loss 1. Edge properties are not written, and the report says how many went.
/// Where no body states the edge — a graph that was never a vault — they are
/// simply gone.
#[test]
fn edge_properties_are_dropped_where_no_prose_restates_them() {
    let graph = cypher_graph();
    let trip = Trip::from_graph(&graph, None, Shape::of(&graph));
    assert_eq!(
        trip.first.edge_props.get("CITES"),
        Some(&2),
        "the created edge carries `section` and `anchor`"
    );
    assert_eq!(
        trip.report.edge_properties_dropped, 2,
        "and the export counts what it dropped"
    );
    assert_eq!(
        trip.second.edge("CITES"),
        1,
        "the edge itself survives as a frontmatter wikilink"
    );
    assert_eq!(
        trip.second.edge_props.get("CITES"),
        Some(&0),
        "but with nothing on it"
    );
}

/// Loss 1, refined — the claim §10.9 makes flatly is narrower than it reads,
/// so the spec says so. An edge the *body* states keeps its properties, because
/// the body travels verbatim and the next import re-derives them from the same
/// prose with the same scanner.
#[test]
fn a_body_stated_edge_keeps_its_properties() {
    let dir = vault_of(&[
        ("Note/a.md", "## Deep dive\n\n[[b#anchor]]\n"),
        ("Note/b.md", "prose\n"),
    ]);
    let trip = Trip::from_vault(dir.path());
    assert_eq!(trip.report.edge_properties_dropped, 2);
    assert_eq!(
        trip.first.edge_props.get("LINKS_TO"),
        Some(&2),
        "`section` and `anchor`"
    );
    assert_eq!(
        trip.second.edge_props.get("LINKS_TO"),
        Some(&2),
        "the prose said it, so the re-import says it again"
    );
    assert_eq!(trip.second.edge("LINKS_TO"), 1, "and does not double it");
}

/// Loss 2. Attachment bytes travel only when the caller names the root they
/// were read from; without it the references are counted unresolvable and come
/// back as `missing: true` stubs.
#[test]
fn attachment_bytes_travel_only_with_a_source_root() {
    let dir = vault_of(MEDIA);
    let carried = Trip::from_vault(dir.path());
    assert_eq!(
        (
            carried.report.attachments_copied,
            carried.report.attachments_unresolved
        ),
        (3, 0)
    );
    assert_eq!(carried.first.label("Image"), 3, "two real, one missing");
    assert_eq!(carried.second.label("Image"), 3);
    assert_eq!(carried.second_report.missing_attachments, 1);

    let rootless = Trip::rootless(dir.path());
    assert_eq!(
        (
            rootless.report.attachments_copied,
            rootless.report.attachments_unresolved
        ),
        (0, 3)
    );
    assert_eq!(
        rootless.second_report.missing_attachments, 4,
        "every reference now names a file the exported vault does not hold"
    );
    assert_eq!(
        rootless
            .second
            .props(ATTACHMENT_LABEL, "handbook.pdf")
            .and_then(|p| p.get("missing"))
            .map(String::as_str),
        Some("true"),
        "the pdf did not travel, so nothing about it is known but its absence —          §6.6 still labels the stub from the extension"
    );
    assert_eq!(
        rootless
            .second
            .props(ATTACHMENT_LABEL, "handbook.pdf")
            .unwrap()
            .get("size_bytes"),
        None,
        "and `stat` said nothing, because there was nothing to stat"
    );
}

/// Loss 3. Synthesized nodes are not files: hubs, folders, tags, attachments
/// and stubs are whatever the *new* layout regenerates, not what the old one
/// held. The golden vault's `Keyword` hub needs `.kglite/vault.yaml`, so it
/// does not come back at all; its `Folder` nodes follow the new one-folder-per
/// -label layout; its `Tag`, `Image` and stub nodes are re-made from prose.
#[test]
fn synthesized_nodes_are_regenerated_not_carried() {
    let trip = Trip::from_vault(&golden_vault());
    assert!(trip.first.label("Keyword") > 0);
    assert_eq!(
        trip.second.label("Keyword"),
        0,
        "a hub needs its declaration"
    );
    assert_eq!(trip.second.edge("HAS_KEYWORD"), 0);
    assert_ne!(
        trip.first.label(FOLDER_LABEL),
        trip.second.label(FOLDER_LABEL),
        "the folders are the export's layout, not the author's"
    );
    for regenerated in [TAG_LABEL, IMAGE_LABEL, "Concept"] {
        assert_eq!(
            trip.first.label(regenerated),
            trip.second.label(regenerated),
            "{regenerated} is re-made from the prose, which travelled verbatim"
        );
    }
}

/// Loss 4. `.kglite/vault.yaml` is not written, so everything it declared is
/// re-derived from the defaults: hubs (above), index and text-index
/// declarations, the `embed:` targets, and `heading_edges` retyping — which
/// shows up as the built-in ladder's own type appearing *beside* the declared
/// one, because the exporter keeps the declared edge and the prose still says
/// what the ladder reads.
#[test]
fn the_vault_yaml_declarations_are_not_carried() {
    let first_report = crate::okf::build(&golden_vault(), &vault_opts())
        .unwrap()
        .report;
    assert!(first_report.indexes_declared > 0);
    assert!(first_report.text_indexes_built > 0);
    assert!(!first_report.embed_targets.is_empty());

    let trip = Trip::from_vault(&golden_vault());
    assert_eq!(trip.second_report.indexes_declared, 0);
    assert_eq!(trip.second_report.text_indexes_built, 0);
    assert!(trip.second_report.embed_targets.is_empty());

    assert_eq!(trip.first.edge("RELATED"), 0, "the declaration retyped it");
    assert!(trip.first.edge("RELATED_TO") > 0);
    assert_eq!(
        trip.second.edge("RELATED_TO"),
        trip.first.edge("RELATED_TO"),
        "the typed edge itself is written as a frontmatter key and survives"
    );
    assert!(
        trip.second.edge("RELATED") > 0,
        "but the same prose now also reads as the built-in ladder's RELATED"
    );
}

/// Loss 5. A top-level string that looks like a date comes back a date. Only
/// `types:` stops §4.2's inference, quoting does not, and `types:` lives in the
/// file the export does not write.
#[test]
fn a_declared_string_that_looks_like_a_date_comes_back_a_date() {
    let dir = vault_of(TYPING);
    let trip = Trip::from_vault(dir.path());
    assert_eq!(
        trip.first
            .props("Note", "typed")
            .and_then(|p| p.get("stays_text"))
            .map(String::as_str),
        Some("2026-02-20"),
        "the declaration kept it text"
    );
    let second = trip
        .second
        .props("Note", "typed")
        .expect("the note came back");
    assert_eq!(
        second.get("stays_text").map(String::as_str),
        Some("2026-02-20")
    );
    // The rendered text is the same; the *type* is not, which is what the next
    // `WHERE n.stays_text < date('…')` sees. `DateTime` is the engine's name
    // for a date-without-time; `Timestamp` is the one with a clock on it.
    assert_eq!(
        property_type(dir.path(), "stays_text"),
        "String",
        "before the round trip the declaration held"
    );
    assert_eq!(
        property_type(trip.exported_dir(), "stays_text"),
        "DateTime",
        "after it — §4.2 infers, and nothing is left to say otherwise"
    );
}

/// Loss 5, refined — the same sentence in §10.9 used to reach every declared
/// type. It does not: the writer emits an `int` as an int and §4.2 reads it
/// back as one, so a declaration that merely *agreed* with what the emitted
/// spelling infers survives without the file that made it.
#[test]
fn a_declared_int_survives_the_round_trip() {
    let dir = vault_of(&[
        (
            ".kglite/vault.yaml",
            "kglite_vault: 1\ndefault_label: Note\ntypes:\n  Note: {toc_depth: int}\n",
        ),
        ("notes/atlas.md", "---\ntoc_depth: \"2\"\n---\nprose\n"),
    ]);
    assert_eq!(property_type(dir.path(), "toc_depth"), "Int64");
    let trip = Trip::from_vault(dir.path());
    assert_eq!(
        property_type(trip.exported_dir(), "toc_depth"),
        "Int64",
        "the export wrote an int, and inference agrees with the declaration"
    );
    assert_eq!(
        trip.second
            .props("Note", "atlas")
            .and_then(|p| p.get("toc_depth"))
            .map(String::as_str),
        Some("2")
    );
}

/// Loss 6. A re-filed note carries its body verbatim, so a note-relative
/// reference in that body resolves from where the note *now* is.
#[test]
fn a_re_filed_notes_relative_reference_moves_with_it() {
    let dir = vault_of(&[
        (
            "deep/nested/note.md",
            "---\ntype: Media\ntitle: Nested\n---\n![chart](../../img/chart.png)\n",
        ),
        ("img/chart.png", "stand-in bytes\n"),
    ]);
    let build = crate::okf::build(dir.path(), &vault_opts()).unwrap();
    assert!(build.report.errors.is_empty(), "{:?}", build.report.errors);
    let trip = Trip::from_vault(dir.path());
    assert!(
        trip.first.props(IMAGE_LABEL, "img/chart.png").is_some(),
        "it resolved from `deep/nested/`"
    );
    // `type: Media` and a top folder of `deep` do not match, so the note is
    // re-filed to `Media/Nested.md` — one level up, where `../../` climbs out of
    // the vault entirely.
    assert!(trip.exports[0].contains_key("Media/Nested.md"));
    assert!(
        trip.second_report
            .errors
            .iter()
            .any(|e| e.contains("escapes the vault root")),
        "the same two dots now name a place outside the vault: {:?}",
        trip.second_report.errors
    );
    // §6.2's third rung — a unique filename anywhere in the vault — still finds
    // the copied file, so the picture is not lost; the *reference* is wrong, and
    // a vault holding two `chart.png`s would resolve to neither.
    assert!(trip.second.props(IMAGE_LABEL, "img/chart.png").is_some());
}

/// The declared type of one property, read out of a freshly built vault — the
/// thing a rendered value cannot tell you (`"2026-02-20"` and a `Date` print
/// the same).
fn property_type(vault: &Path, property: &str) -> String {
    let graph = build_vault(vault);
    let _arena_guard = graph.graph.begin_query();
    for idx in graph.graph.node_indices() {
        let Some(view) = graph.node_view(idx) else {
            continue;
        };
        if let Some(value) = view.get_property_value(property) {
            return value.type_name().to_string();
        }
    }
    panic!("no node carries `{property}` in {}", vault.display());
}

impl Trip {
    /// The directory the first export wrote into — kept alive by `_dirs`.
    fn exported_dir(&self) -> &Path {
        self._dirs[0].path()
    }
}
