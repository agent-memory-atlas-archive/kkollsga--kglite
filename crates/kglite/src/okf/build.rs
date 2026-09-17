//! Graph builder: turn parsed [`ConceptDoc`]s into a [`DirGraph`].
//!
//! Mirrors the code-graph loader pattern: build columnar [`DataFrame`]s and hand them to
//! the bulk `maintain::add_nodes` / `add_connections` mutators (interning, type
//! schema, id-index, and dedup come for free). Nodes are grouped by label; edges
//! by `(source_label, target_label, conn_type)` so each `add_connections` call
//! has correctly-typed endpoints. Dangling link targets vivify as `_provisional`
//! stub nodes (the mutator's built-in behaviour).
//!
//! Structured frontmatter values (`tags` lists, nested maps surfaced inside
//! lists) are JSON-encoded into String columns for OKF bundles — the same
//! convention code-graph builders use for `parameters`/`fields`. The vault
//! profile turns that off (`Profile::native_collections`) and stores them as
//! `Value::List` / `Value::Map` columns instead.

use crate::datatypes::values::{DataFrame, Value};
use crate::graph::mutation::maintain;
use crate::graph::DirGraph;
use crate::okf::model::{
    BuildOptions, BuildReport, ConceptDoc, FolderNoteDirection, Link, Profile, CONTAINS_CONN_TYPE,
    DEFAULT_LABEL, FOLDER_LABEL, SOURCE_LABEL,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One connection row: the endpoints plus whatever properties the edge itself
/// carries (VAULT.md §5.4 `section`/`anchor`, §6's `alt`/`ordinal`). Structural
/// edges carry none, which keeps their frames two columns wide.
type EdgeRow = (String, String, Vec<(String, Value)>);
/// `(conn_type, source_label, target_label)` → the rows to emit for it. A
/// `BTreeMap` keyed with the connection type first, because [`emit_groups`]
/// needs every group of one type together and in a fixed order — see the
/// initial-load note there.
type EdgeGroups = BTreeMap<(String, String, String), Vec<EdgeRow>>;

/// A finished build: the graph, and what the builder saw producing it.
/// No `Debug` — `DirGraph` has none, and a graph is not a thing to format.
#[derive(Clone)]
pub struct BuildOutput {
    pub graph: Arc<DirGraph>,
    pub report: BuildReport,
}

/// Build a knowledge graph from an OKF bundle directory.
pub fn build(root: &Path, opts: &BuildOptions) -> Result<BuildOutput, String> {
    let walked = super::walk::discover(root, opts)?;
    let (docs, findings) = super::parse_concepts_reported(&walked.concepts, opts);
    let mut report = BuildReport {
        files_scanned: walked.concepts.len(),
        concepts: docs.len(),
        errors: findings.errors,
        warnings: findings.warnings,
        ..BuildReport::default()
    };
    let mut graph = DirGraph::new();
    if docs.is_empty() {
        return Ok(BuildOutput {
            graph: Arc::new(graph),
            report,
        });
    }
    build_nodes(&mut graph, &docs, opts, &mut report)?;
    build_aux_nodes(&mut graph, &docs, &mut report)?;
    // Hub and folder edges are collected rather than emitted, because they
    // meet the link edges in one group map: a note's `parent:` and the folder
    // layout can name the same relationship, and two `emit_groups` calls
    // cannot see each other's rows to fold them into one edge.
    let mut groups = build_hubs(&mut graph, &docs, &opts.profile, &mut report)?;
    merge_groups(
        &mut groups,
        build_folders(&mut graph, &docs, &walked.index_files, opts, &mut report)?,
    );
    build_edges(&mut graph, &docs, opts, groups, &mut report)?;
    Ok(BuildOutput {
        graph: Arc::new(graph),
        report,
    })
}

/// A doc's file path minus `.md` — the directory hierarchy and the path-link
/// namespace both live here. Equal to `concept_id` under the path id scheme,
/// and deliberately *not* under the vault's, where the id is a bare stem: a
/// folder derived from the id would leave every vault note at the root.
fn doc_path(d: &ConceptDoc) -> &str {
    d.file_path.strip_suffix(".md").unwrap_or(&d.file_path)
}

/// Record `count` nodes of `label` in the report.
fn count_nodes(report: &mut BuildReport, label: &str, count: usize) {
    if count > 0 {
        *report.nodes_by_label.entry(label.to_string()).or_default() += count;
    }
}

/// Emit grouped edges: one `add_connections` per `(src_label, tgt_label, conn)`
/// so every call has correctly-typed endpoints.
fn emit_groups(
    graph: &mut DirGraph,
    groups: EdgeGroups,
    report: &mut BuildReport,
) -> Result<(), String> {
    // The initial-load regime belongs to the connection *type*, decided once
    // before the first group of it is emitted. Letting each call re-detect it
    // made the first group of a type keep its parallel edges while every later
    // group folded duplicate endpoint pairs onto one — so two body links that
    // differ only in `section` became two edges or one depending on hash
    // order, and the same vault built two different graphs.
    let fresh: BTreeSet<&str> = groups
        .keys()
        .map(|(conn, _, _)| conn.as_str())
        .filter(|conn| !graph.connection_type_metadata.contains_key(*conn))
        .collect();
    let fresh: BTreeSet<String> = fresh.into_iter().map(str::to_string).collect();
    for ((conn, src_label, tgt_label), edges) in groups {
        // The same relationship can be written twice — a `parent:` naming the
        // folder note the layout already joined this note to (VAULT.md §2.3,
        // §4.3). Identical rows are one edge; rows differing in an edge
        // property are not identical and stay two (§5.4).
        let mut seen: HashSet<EdgeRow> = HashSet::new();
        let edges: Vec<EdgeRow> = edges
            .into_iter()
            .filter(|r| seen.insert(r.clone()))
            .collect();
        *report.edges_by_type.entry(conn.clone()).or_default() += edges.len();
        // One frame per group, so its columns are the union of the property
        // keys any row in it carries; a row missing one gets Null, which
        // `add_connections` drops rather than storing.
        let prop_keys: Vec<String> = edges
            .iter()
            .flat_map(|(_, _, props)| props.iter().map(|(k, _)| k.clone()))
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect();
        let rows: Vec<Vec<Value>> = edges
            .into_iter()
            .map(|(s, t, props)| {
                let mut row = Vec::with_capacity(2 + prop_keys.len());
                row.push(Value::String(s));
                row.push(Value::String(t));
                for key in &prop_keys {
                    row.push(
                        props
                            .iter()
                            .find(|(k, _)| k == key)
                            .map(|(_, v)| v.clone())
                            .unwrap_or(Value::Null),
                    );
                }
                row
            })
            .collect();
        let mut columns = vec!["source_id".to_string(), "target_id".to_string()];
        columns.extend(prop_keys);
        let df = DataFrame::from_cypher_rows(columns, rows)?;
        let initial = maintain::InitialLoad::Preset(fresh.contains(&conn));
        maintain::add_connections_with_initial_load(
            graph,
            df,
            conn,
            src_label,
            "source_id".to_string(),
            tgt_label,
            "target_id".to_string(),
            None,
            None,
            Some("update".to_string()),
            initial,
        )?;
    }
    Ok(())
}

/// Fold one group map into another, concatenating the rows of shared keys.
fn merge_groups(into: &mut EdgeGroups, from: EdgeGroups) {
    for (key, rows) in from {
        into.entry(key).or_default().extend(rows);
    }
}

/// What holds a note or a subfolder: the `Folder` node standing for its
/// directory, or the folder note that replaced it (VAULT.md §2.3).
enum Container<'a> {
    Folder(String),
    Note(&'a ConceptDoc),
}

/// Which directories a folder note replaced. Empty unless
/// [`Profile::folder_notes`] is set, which is what keeps `okf` and `loose`
/// on the pure-`Folder` hierarchy they have always had.
#[derive(Default)]
struct FolderLayout<'a> {
    /// Directory path → the note standing in for its `Folder` node.
    note_of_dir: BTreeMap<String, &'a ConceptDoc>,
    /// A folder note's `doc_path` → the directory it owns. The note is not
    /// held by that directory but by the directory's *parent*: it took the
    /// folder's place, so it hangs where the folder hung.
    dir_of_note: BTreeMap<&'a str, String>,
}

impl<'a> FolderLayout<'a> {
    /// What holds the children of `dir` — `None` at the vault root, which has
    /// neither a `Folder` node nor a folder note.
    fn owner(&self, dir: &str) -> Option<Container<'a>> {
        if dir.is_empty() {
            None
        } else if let Some(note) = self.note_of_dir.get(dir) {
            Some(Container::Note(note))
        } else {
            Some(Container::Folder(dir.to_string()))
        }
    }
}

/// Match each directory with its folder note: `X.md` beside `X/`, or `X/X.md`
/// (VAULT.md §2.3). Declaring both is an error — one directory cannot have two
/// notes standing for it — and `X.md` wins so the build still produces a graph.
fn folder_layout<'a>(
    by_path: &BTreeMap<&'a str, &'a ConceptDoc>,
    dirs: &BTreeSet<String>,
    report: &mut BuildReport,
) -> FolderLayout<'a> {
    let mut layout = FolderLayout::default();
    for dir in dirs {
        let base = dir.rsplit('/').next().unwrap_or(dir);
        let beside = by_path.get(dir.as_str()).copied();
        let inside = by_path.get(format!("{dir}/{base}").as_str()).copied();
        if beside.is_some() && inside.is_some() {
            report.errors.push(format!(
                "folder note declared twice for `{dir}/`: `{dir}.md` and `{dir}/{base}.md`; `{dir}.md` is used"
            ));
        }
        if let Some(note) = beside.or(inside) {
            layout.note_of_dir.insert(dir.clone(), note);
            layout.dir_of_note.insert(doc_path(note), dir.clone());
        }
    }
    layout
}

/// One containment row: the folder-note edge when a note holds a note, and
/// `CONTAINS` whenever a `Folder` node is either endpoint — a folder note
/// standing in for a directory still *contains* the plain subfolders under it
/// (VAULT.md §2.2, §2.3).
fn push_containment(
    groups: &mut EdgeGroups,
    container: &Container<'_>,
    child_label: &str,
    child_id: &str,
    child_is_note: bool,
    profile: &Profile,
) {
    let (conn, src_label, src_id, tgt_label, tgt_id) = match container {
        Container::Note(parent) if child_is_note => {
            let down = profile.folder_note_direction == FolderNoteDirection::ParentToChild;
            let (s, si, t, ti) = if down {
                (
                    parent.label.as_str(),
                    parent.concept_id.as_str(),
                    child_label,
                    child_id,
                )
            } else {
                (
                    child_label,
                    child_id,
                    parent.label.as_str(),
                    parent.concept_id.as_str(),
                )
            };
            (profile.folder_note_edge.as_str(), s, si, t, ti)
        }
        Container::Note(parent) => (
            CONTAINS_CONN_TYPE,
            parent.label.as_str(),
            parent.concept_id.as_str(),
            child_label,
            child_id,
        ),
        Container::Folder(dir) => (
            CONTAINS_CONN_TYPE,
            FOLDER_LABEL,
            dir.as_str(),
            child_label,
            child_id,
        ),
    };
    groups
        .entry((
            conn.to_string(),
            src_label.to_string(),
            tgt_label.to_string(),
        ))
        .or_default()
        .push((src_id.to_string(), tgt_id.to_string(), Vec::new()));
}

/// Materialize the directory hierarchy as `Folder` nodes:
/// `(:Folder)-[:CONTAINS]->(:Concept)` and `(:Folder)-[:CONTAINS]->(:Folder)`.
/// A directory's `index.md` enriches its Folder node's title/description (so the
/// reserved file is recovered as structure rather than discarded). Co-located
/// concepts gain a 2-hop hub, capturing the taxonomic meaning of the layout.
///
/// Under [`Profile::folder_notes`] a directory with a folder note gets no
/// `Folder` node at all: the note takes its place in the hierarchy, and the
/// notes inside it are joined by the profile's folder-note edge instead
/// (VAULT.md §2.3). The containment rows are returned rather than emitted —
/// see the note in [`build`].
fn build_folders<'a>(
    graph: &mut DirGraph,
    docs: &'a [ConceptDoc],
    index_files: &HashMap<String, PathBuf>,
    opts: &BuildOptions,
    report: &mut BuildReport,
) -> Result<EdgeGroups, String> {
    // Every directory holding a concept, plus all ancestor directories.
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for d in docs {
        let mut p = super::parent_dir(doc_path(d)).to_string();
        while !p.is_empty() {
            let parent = super::parent_dir(&p).to_string();
            dirs.insert(p);
            p = parent;
        }
    }
    if dirs.is_empty() {
        return Ok(BTreeMap::new());
    }
    let profile = &opts.profile;
    let by_path: BTreeMap<&'a str, &'a ConceptDoc> =
        docs.iter().map(|d| (doc_path(d), d)).collect();
    let layout = if profile.folder_notes {
        folder_layout(&by_path, &dirs, report)
    } else {
        FolderLayout::default()
    };
    report.folder_notes = layout.note_of_dir.len();
    let folder_dirs: Vec<&str> = dirs
        .iter()
        .map(String::as_str)
        .filter(|d| !layout.note_of_dir.contains_key(*d))
        .collect();
    count_nodes(report, FOLDER_LABEL, folder_dirs.len());

    // Folder nodes (id = dir path; title/description from index.md if present).
    if !folder_dirs.is_empty() {
        let mut rows: Vec<Vec<Value>> = Vec::with_capacity(folder_dirs.len());
        for dir in &folder_dirs {
            let (title, desc) = index_files
                .get(*dir)
                .map(|p| folder_meta(p))
                .unwrap_or((None, None));
            let title = title.unwrap_or_else(|| dir.rsplit('/').next().unwrap_or(dir).to_string());
            rows.push(vec![
                Value::String((*dir).to_string()),
                Value::String(title),
                desc.map(Value::String).unwrap_or(Value::Null),
            ]);
        }
        let df = DataFrame::from_cypher_rows(
            vec![
                "id".to_string(),
                "title".to_string(),
                "description".to_string(),
            ],
            rows,
        )?;
        maintain::add_nodes(
            graph,
            df,
            FOLDER_LABEL.to_string(),
            "id".to_string(),
            Some("title".to_string()),
            Some("update".to_string()),
        )?;
    }

    // Containment: every note and every surviving `Folder` hangs off whatever
    // holds its directory.
    let mut groups: EdgeGroups = BTreeMap::new();
    for d in docs {
        let dir = match layout.dir_of_note.get(doc_path(d)) {
            Some(owned) => super::parent_dir(owned),
            None => super::parent_dir(doc_path(d)),
        };
        if let Some(c) = layout.owner(dir) {
            push_containment(&mut groups, &c, &d.label, &d.concept_id, true, profile);
        }
    }
    for dir in &folder_dirs {
        if let Some(c) = layout.owner(super::parent_dir(dir)) {
            push_containment(&mut groups, &c, FOLDER_LABEL, dir, false, profile);
        }
    }
    Ok(groups)
}

/// Extract a `(title, description)` for a Folder node from its `index.md`:
/// the first heading is the title, the first prose line the description.
fn folder_meta(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (None, None);
    };
    let mut title = None;
    let mut desc = None;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(h) = super::links::heading_text(t) {
            if title.is_none() {
                title = Some(h.to_string());
            }
        } else if desc.is_none() {
            desc = Some(t.to_string());
        }
        if title.is_some() && desc.is_some() {
            break;
        }
    }
    (title.filter(|s| !s.is_empty()), desc)
}

/// Synthesize `Source` nodes from the concepts' external links. Added before
/// edges so the `CITES` connections find real endpoints instead of vivifying
/// provisional stubs. Hub nodes get the same treatment in [`build_hubs`].
fn build_aux_nodes(
    graph: &mut DirGraph,
    docs: &[ConceptDoc],
    report: &mut BuildReport,
) -> Result<(), String> {
    let mut sources: BTreeSet<&str> = BTreeSet::new();
    for d in docs {
        for l in &d.links {
            if l.is_external {
                sources.insert(l.target.as_str());
            }
        }
    }
    count_nodes(report, SOURCE_LABEL, sources.len());
    add_id_nodes(graph, SOURCE_LABEL, &sources)?;
    Ok(())
}

/// Synthesize each declared hub's nodes and return its membership rows
/// (VAULT.md §5.5, §7).
///
/// One hub per [`Profile::hubs`] entry, so the `Tag` hub every dialect has and
/// a vault's `keywords:` are the same code. Nodes are added here — before the
/// edges are emitted — so membership never vivifies a `_provisional` stub.
fn build_hubs(
    graph: &mut DirGraph,
    docs: &[ConceptDoc],
    profile: &Profile,
    report: &mut BuildReport,
) -> Result<EdgeGroups, String> {
    let mut groups: EdgeGroups = BTreeMap::new();
    for (key, spec) in &profile.hubs {
        // id → the original spellings that folded onto it, with their counts.
        let mut spellings: BTreeMap<String, BTreeMap<&str, usize>> = BTreeMap::new();
        let mut members: Vec<(&ConceptDoc, String)> = Vec::new();
        for d in docs {
            for raw in hub_values(d, key) {
                let id = if spec.case_insensitive {
                    raw.to_lowercase()
                } else {
                    raw.to_string()
                };
                *spellings
                    .entry(id.clone())
                    .or_default()
                    .entry(raw)
                    .or_default() += 1;
                // Naming one tag twice, or once in each casing under a folding
                // hub, is one relationship — and one row, folded by the
                // dedupe in `emit_groups` rather than a second one here.
                members.push((d, id));
            }
        }
        if spellings.is_empty() {
            continue;
        }
        count_nodes(report, &spec.label, spellings.len());
        let rows: Vec<Vec<Value>> = spellings
            .iter()
            .map(|(id, counts)| {
                vec![
                    Value::String(id.clone()),
                    Value::String(hub_title(id, counts)),
                ]
            })
            .collect();
        let df = DataFrame::from_cypher_rows(vec!["id".to_string(), "title".to_string()], rows)?;
        maintain::add_nodes(
            graph,
            df,
            spec.label.clone(),
            "id".to_string(),
            Some("title".to_string()),
            Some("update".to_string()),
        )?;
        for (d, id) in members {
            groups
                .entry((spec.edge.clone(), d.label.clone(), spec.label.clone()))
                .or_default()
                .push((d.concept_id.clone(), id, Vec::new()));
        }
    }
    Ok(groups)
}

/// The display title of a hub node: the spelling the vault used most often,
/// alphabetically first among equals so the title never depends on which note
/// happened to be read first. A case-sensitive hub has exactly one spelling
/// per id, which makes this the id itself.
fn hub_title(id: &str, counts: &BTreeMap<&str, usize>) -> String {
    counts
        .iter()
        .min_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)))
        .map(|(spelling, _)| (*spelling).to_string())
        .unwrap_or_else(|| id.to_string())
}

/// Bulk-add bare nodes whose id is their title (Tag names, Source URLs).
fn add_id_nodes(graph: &mut DirGraph, label: &str, ids: &BTreeSet<&str>) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    let rows: Vec<Vec<Value>> = ids
        .iter()
        .map(|s| vec![Value::String((*s).to_string())])
        .collect();
    let df = DataFrame::from_cypher_rows(vec!["id".to_string()], rows)?;
    maintain::add_nodes(
        graph,
        df,
        label.to_string(),
        "id".to_string(),
        None,
        Some("update".to_string()),
    )?;
    Ok(())
}

/// The entries a concept joins a hub by: the string elements of its `key`
/// frontmatter **list**, in order. A scalar is not a list and joins nothing —
/// VAULT.md §7 defines a hub over a list-valued key, and §9 classes a scalar
/// `tags:` as a reserved-key error rather than a one-entry list.
///
/// The `tags` key additionally takes the inline `#tag`s the vault profile
/// found in the body (VAULT.md §5.5): the inline syntax names that hub and no
/// other, and the `tags` *property* still reports only the frontmatter.
fn hub_values<'a>(d: &'a ConceptDoc, key: &str) -> Vec<&'a str> {
    let mut vals: Vec<&str> = d
        .props
        .iter()
        .filter(|(k, _)| k == key)
        .flat_map(|(_, v)| match v {
            Value::List(items) => items
                .iter()
                .filter_map(|x| match x {
                    Value::String(s) => Some(s.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();
    if key == "tags" {
        for t in &d.inline_tags {
            if !vals.contains(&t.as_str()) {
                vals.push(t.as_str());
            }
        }
    }
    vals
}

/// A concept's `aliases:` entries — the names it also answers to in link
/// resolution (VAULT.md §5.2, rung 3). A scalar `aliases: Foo` is read as the
/// one-entry list it means.
fn doc_aliases(d: &ConceptDoc) -> Vec<&str> {
    d.props
        .iter()
        .filter(|(k, _)| k == "aliases")
        .flat_map(|(_, v)| match v {
            Value::List(items) => items
                .iter()
                .filter_map(|x| match x {
                    Value::String(s) => Some(s.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            Value::String(s) => vec![s.as_str()],
            _ => Vec::new(),
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// One `add_nodes` call per label; columns = id/title/file_path (+ body) plus the
/// union of frontmatter keys across that label's concepts (missing → Null).
fn build_nodes(
    graph: &mut DirGraph,
    docs: &[ConceptDoc],
    opts: &BuildOptions,
    report: &mut BuildReport,
) -> Result<(), String> {
    // Sorted, so the graph's node order is the same on every run: a `HashMap`
    // here made node indices — and therefore a saved `.kgl`'s bytes — depend on
    // hash order.
    let mut by_label: BTreeMap<&str, Vec<&ConceptDoc>> = BTreeMap::new();
    for d in docs {
        by_label.entry(d.label.as_str()).or_default().push(d);
    }

    for (label, group) in by_label {
        count_nodes(report, label, group.len());
        let mut keys: BTreeSet<&str> = BTreeSet::new();
        for d in &group {
            for (k, _) in &d.props {
                keys.insert(k.as_str());
            }
        }
        let keys: Vec<&str> = keys.into_iter().collect();

        let mut columns = vec![
            "concept_id".to_string(),
            "title".to_string(),
            "file_path".to_string(),
        ];
        if opts.with_body {
            columns.push("body".to_string());
        }
        columns.extend(keys.iter().map(|k| k.to_string()));

        let mut rows = Vec::with_capacity(group.len());
        for d in &group {
            let mut row = vec![
                Value::String(d.concept_id.clone()),
                Value::String(d.title.clone()),
                Value::String(d.file_path.clone()),
            ];
            if opts.with_body {
                row.push(d.body.clone().map(Value::String).unwrap_or(Value::Null));
            }
            let pm: HashMap<&str, &Value> = d.props.iter().map(|(k, v)| (k.as_str(), v)).collect();
            for k in &keys {
                row.push(
                    pm.get(k)
                        .map(|v| column_value(v, opts.profile.native_collections))
                        .unwrap_or(Value::Null),
                );
            }
            rows.push(row);
        }

        let df = DataFrame::from_cypher_rows(columns, rows)?;
        maintain::add_nodes(
            graph,
            df,
            label.to_string(),
            "concept_id".to_string(),
            Some("title".to_string()),
            Some("update".to_string()),
        )?;
    }
    Ok(())
}

/// Coerce a property value for columnar storage. With `native` set (the vault
/// profile) every value passes through as itself, so a frontmatter sequence
/// reaches the graph as a `Value::List` column. Without it, structured values
/// JSON-encode to a String — the OKF/Loose convention codingest's docs pass
/// shares, kept because those graphs' consumers parse the JSON today.
pub(crate) fn column_value(v: &Value, native: bool) -> Value {
    match v {
        Value::List(_) | Value::Map(_) if !native => Value::String(
            serde_json::to_string(&crate::param::kglite_value_to_json(v)).unwrap_or_default(),
        ),
        other => other.clone(),
    }
}

/// Build the concept-level edges — semantic links, typed via the ladder
/// (internal → concept, external → Source) — and emit them together with the
/// containment and hub rows `groups` arrives carrying, so a relationship two
/// of those sources agree on is one edge.
fn build_edges(
    graph: &mut DirGraph,
    docs: &[ConceptDoc],
    opts: &BuildOptions,
    mut groups: EdgeGroups,
    report: &mut BuildReport,
) -> Result<(), String> {
    let (resolver, alias_warnings) = Resolver::new(docs, &opts.profile);
    report.warnings.extend(alias_warnings);
    // Dangling internal-link targets — concepts referenced but not present.
    let mut dangling: BTreeSet<String> = BTreeSet::new();

    // Semantic links: internal → concept edges (resolved), external → Source.
    for d in docs {
        for link in &d.links {
            let (target_label, target_id) = if link.is_external {
                (SOURCE_LABEL.to_string(), link.target.clone())
            } else {
                let (id, label) = resolver.resolve(link, super::parent_dir(doc_path(d)));
                if !resolver.id_to_label.contains_key(id.as_str()) {
                    dangling.insert(id.clone());
                }
                (label, id)
            };
            // A reversed link (a `parent:` pointing parent → child) is the same
            // edge read from the other end, so only the endpoints swap.
            let (src_label, src_id, tgt_label, tgt_id) = if link.reverse {
                (
                    target_label,
                    target_id,
                    d.label.clone(),
                    d.concept_id.clone(),
                )
            } else {
                (
                    d.label.clone(),
                    d.concept_id.clone(),
                    target_label,
                    target_id,
                )
            };
            groups
                .entry((link.conn_type.clone(), src_label, tgt_label))
                .or_default()
                .push((src_id, tgt_id, link.props.clone()));
        }
    }

    // Pre-create dangling targets as provisional `Concept` nodes carrying
    // `concept_id` — so "references not yet written" are queryable identically to
    // real concepts (`MATCH (n {_provisional:true}) RETURN n.concept_id`) rather
    // than via the mutator's default `id` stub field.
    report.dangling = dangling.len();
    count_nodes(report, DEFAULT_LABEL, dangling.len());
    // A dangling link is legitimate in a real vault — "referenced but not
    // written" is a note to write, not a broken build (VAULT.md §9).
    for id in &dangling {
        report.warnings.push(format!("dangling link: `{id}`"));
    }
    if !dangling.is_empty() {
        let rows: Vec<Vec<Value>> = dangling
            .iter()
            .map(|id| vec![Value::String(id.clone()), Value::Boolean(true)])
            .collect();
        let df = DataFrame::from_cypher_rows(
            vec!["concept_id".to_string(), "_provisional".to_string()],
            rows,
        )?;
        maintain::add_nodes(
            graph,
            df,
            DEFAULT_LABEL.to_string(),
            "concept_id".to_string(),
            None,
            Some("preserve".to_string()),
        )?;
    }

    emit_groups(graph, groups, report)
}

/// Normalize a name/path for forgiving link resolution: lowercase, unify
/// `_`/` `→`-`, collapse repeats, trim. So `Project-0-10` / `project_0_10` /
/// `Project 0 10` all match. Slashes are preserved (paths stay paths).
fn normalize_slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for ch in s.chars() {
        let c = ch.to_ascii_lowercase();
        let c = if c == '_' || c == ' ' { '-' } else { c };
        if c == '-' {
            if prev_dash {
                continue;
            }
            prev_dash = true;
        } else {
            prev_dash = false;
        }
        out.push(c);
    }
    out.trim_matches('-').to_string()
}

/// Forgiving link → concept resolver, one ladder for path links and wikilinks
/// alike (VAULT.md §5.2). Tries, most-specific first: exact id → a
/// vault-relative then note-relative path → exact file stem → an `aliases:`
/// entry → normalized slug (full path or last segment) → normalized title.
/// Unresolved targets keep their raw id and the default label —
/// `add_connections` vivifies them as `_provisional` stubs.
struct Resolver<'a> {
    id_to_label: HashMap<&'a str, &'a str>,
    /// File path minus `.md` → id. Identical to `id_to_label`'s keys under the
    /// path id scheme; under the vault's stem ids it is what keeps a
    /// `[text](sub/note.md)` path link resolving (VAULT.md §5.2).
    path_to_id: HashMap<&'a str, &'a str>,
    stem_to_id: HashMap<&'a str, &'a str>,
    /// Empty unless [`Profile::alias_resolution`] is set.
    alias_to_id: HashMap<&'a str, &'a str>,
    slug_to_id: HashMap<String, &'a str>,
    title_to_id: HashMap<String, &'a str>,
}

impl<'a> Resolver<'a> {
    /// Build the ladder's indexes, and report the alias clashes found on the
    /// way: an alias that names another note's stem, or that two notes both
    /// claim, resolves to exactly one of them, so the vault's author needs to
    /// know (VAULT.md §9, a warning — the graph is still usable).
    fn new(docs: &'a [ConceptDoc], profile: &crate::okf::model::Profile) -> (Self, Vec<String>) {
        let mut id_to_label = HashMap::new();
        let mut path_to_id = HashMap::new();
        let mut stem_to_id = HashMap::new();
        let mut alias_to_id = HashMap::new();
        let mut slug_to_id = HashMap::new();
        let mut title_to_id = HashMap::new();
        let mut warnings = Vec::new();
        for d in docs {
            id_to_label.insert(d.concept_id.as_str(), d.label.as_str());
            let path = doc_path(d);
            path_to_id.entry(path).or_insert(d.concept_id.as_str());
            // The stem the *editor* sees, from the filename — a note carrying
            // its own `id:` is still `[[Stem]]` in the vault.
            let stem = path.rsplit('/').next().unwrap_or(path);
            stem_to_id.entry(stem).or_insert(d.concept_id.as_str());
            slug_to_id
                .entry(normalize_slug(&d.concept_id))
                .or_insert(d.concept_id.as_str());
            slug_to_id
                .entry(normalize_slug(stem))
                .or_insert(d.concept_id.as_str());
            title_to_id
                .entry(normalize_slug(&d.title))
                .or_insert(d.concept_id.as_str());
        }
        if profile.alias_resolution {
            for d in docs {
                for alias in doc_aliases(d) {
                    // The note's own stem is not a clash — an alias repeating
                    // it is redundant, not ambiguous.
                    if let Some(&owner) = stem_to_id.get(alias) {
                        if owner != d.concept_id {
                            warnings.push(format!(
                                "alias `{alias}` on {} is already the file stem of `{owner}`; the stem wins",
                                d.file_path
                            ));
                        }
                        continue;
                    }
                    if let Some(&owner) = alias_to_id.get(alias) {
                        warnings.push(format!(
                            "alias `{alias}` is claimed by both `{owner}` and `{}`; the first wins",
                            d.concept_id
                        ));
                        continue;
                    }
                    alias_to_id.insert(alias, d.concept_id.as_str());
                }
            }
        }
        (
            Self {
                id_to_label,
                path_to_id,
                stem_to_id,
                alias_to_id,
                slug_to_id,
                title_to_id,
            },
            warnings,
        )
    }

    fn label_of(&self, id: &str) -> String {
        self.id_to_label
            .get(id)
            .copied()
            .unwrap_or(DEFAULT_LABEL)
            .to_string()
    }

    /// Resolve one link's target to `(id, label)`. `source_dir` is the linking
    /// note's directory, for the note-relative rung a `/`-bearing target gets
    /// after the vault-relative one (VAULT.md §5.2).
    fn resolve(&self, link: &Link, source_dir: &str) -> (String, String) {
        let t = link.target.as_str();
        if let Some(lbl) = self.id_to_label.get(t) {
            return (t.to_string(), lbl.to_string());
        }
        if let Some(id) = self.path_to_id.get(t) {
            return ((*id).to_string(), self.label_of(id));
        }
        if t.contains('/') && !source_dir.is_empty() {
            if let Some(id) = self.path_to_id.get(format!("{source_dir}/{t}").as_str()) {
                return ((*id).to_string(), self.label_of(id));
            }
        }
        if let Some(id) = self.stem_to_id.get(t) {
            return ((*id).to_string(), self.label_of(id));
        }
        if let Some(id) = self.alias_to_id.get(t) {
            return ((*id).to_string(), self.label_of(id));
        }
        let norm = normalize_slug(t);
        if let Some(id) = self.slug_to_id.get(&norm) {
            return ((*id).to_string(), self.label_of(id));
        }
        if let Some(seg) = t.rsplit('/').next() {
            if let Some(id) = self.slug_to_id.get(&normalize_slug(seg)) {
                return ((*id).to_string(), self.label_of(id));
            }
        }
        if let Some(id) = self.title_to_id.get(&norm) {
            return ((*id).to_string(), self.label_of(id));
        }
        (t.to_string(), DEFAULT_LABEL.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::schema::InternedKey;
    use crate::graph::storage::GraphRead;
    use crate::okf::model::{
        HubSpec, EMBEDS_CONN_TYPE, FOLDER_NOTE_CONN_TYPE, TAGGED_CONN_TYPE, TAG_LABEL,
    };
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::tempdir;

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, content).unwrap();
    }

    fn count_label(g: &DirGraph, label: &str) -> usize {
        g.graph
            .node_indices()
            .filter(|&n| {
                g.node_view(n)
                    .is_some_and(|nd| nd.node_type_str(&g.interner) == label)
            })
            .count()
    }

    fn provisional_count(g: &DirGraph) -> usize {
        let key = InternedKey::from_str("_provisional");
        g.graph
            .node_indices()
            .filter(|&n| {
                matches!(
                    GraphRead::get_node_property(&g.graph, n, key),
                    Some(Value::Boolean(true))
                )
            })
            .count()
    }

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
    fn vault_path_links_resolve_by_path_not_by_last_segment() {
        let dir = tempdir().unwrap();
        // Two notes whose stems slugify alike; only the full path tells them
        // apart, and the forgiving last-segment rung would answer the wrong one.
        write(dir.path(), "Docs/Guide.md", "---\nid: g1\n---\nvendor copy");
        write(dir.path(), "notes/guide.md", "my notes");
        write(dir.path(), "a.md", "See [g](notes/guide.md).");
        let opts = BuildOptions::for_dialect(crate::okf::Dialect::Obsidian);
        let out = build(dir.path(), &opts).unwrap();
        assert_eq!(out.report.dangling, 0, "a path link is not a dangling link");
        assert_eq!(provisional_count(&out.graph), 0);
        let g = &out.graph;
        let targets: Vec<String> = g
            .graph
            .edge_indices()
            .filter(|&e| g.graph[e].connection_type_str(&g.interner) == "LINKS_TO")
            .filter_map(|e| g.graph.edge_endpoints(e))
            .filter_map(|(_, t)| {
                g.node_view(t)
                    .map(|nd| nd.node_type_str(&g.interner).to_string())
            })
            .collect();
        assert_eq!(
            targets,
            vec!["notes".to_string()],
            "the link lands on notes/guide.md (label `notes`), not on Docs/Guide.md"
        );
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
    #[test]
    fn golden_vault_bundle_report() {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/okf/golden/vault");
        let opts = BuildOptions::for_dialect(crate::okf::Dialect::Obsidian);
        let r = build(&root, &opts).unwrap().report;

        assert_eq!(r.files_scanned, 13);
        assert_eq!(r.concepts, 13, "a note needs no frontmatter in a vault");
        assert_eq!(r.dangling, 1, "`[[Missing]]`, named by a `depends_on:`");
        assert_eq!(r.folder_notes, 1, "`projects.md` stands in for `projects/`");
        assert_eq!(
            r.nodes_by_label,
            BTreeMap::from([
                // welcome.md and projects.md, both at the root — a folder note
                // is labelled from where its *folder* sits (VAULT.md §2.3)
                ("Note".to_string(), 2),
                ("Initiative".to_string(), 2), // atlas.md and seismic.md, from `type:`
                ("projects".to_string(), 2),
                ("notes".to_string(), 6),
                ("archive".to_string(), 1),
                // notes, notes/deep, archive — `projects/` has a folder note
                (FOLDER_LABEL.to_string(), 3),
                (TAG_LABEL.to_string(), 3), // seismic, plus two inline `#tag`s
                (DEFAULT_LABEL.to_string(), 1), // the `[[Missing]]` stub
            ])
        );
        assert_eq!(
            r.edges_by_type,
            BTreeMap::from([
                (CONTAINS_CONN_TYPE.to_string(), 8),
                ("LINKS_TO".to_string(), 6),
                ("RELATED".to_string(), 1), // the `## Related topics` heading
                (EMBEDS_CONN_TYPE.to_string(), 1), // `![[old]]`
                // four notes under the `projects` folder note, plus the
                // reserved `parent: "[[atlas]]"`
                (FOLDER_NOTE_CONN_TYPE.to_string(), 5),
                ("DEPENDS_ON".to_string(), 2), // the wikilink-valued key
                (TAGGED_CONN_TYPE.to_string(), 4),
            ])
        );

        assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
        assert!(
            r.errors[0].contains("`alpha`")
                && r.errors[0].contains("notes/alpha.md")
                && r.errors[0].contains("projects/alpha.md"),
            "{}",
            r.errors[0]
        );
        assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
        assert!(
            r.warnings[0].contains("`Roadmap` (projects/Roadmap.md)")
                && r.warnings[0].contains("`roadmap` (notes/roadmap.md)"),
            "{}",
            r.warnings[0]
        );
        assert_eq!(r.warnings[1], "dangling link: `Missing`");
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

    /// One edge as `(source id, conn type, target id, sorted properties)` —
    /// the shape the vault link rules below are stated in.
    type EdgeFacts = (String, String, String, Vec<(String, String)>);

    fn edges_of(g: &DirGraph) -> Vec<EdgeFacts> {
        let name = |n: petgraph::graph::NodeIndex| -> String {
            g.node_view(n)
                .map(|nd| match nd.id().into_owned() {
                    Value::String(s) => s,
                    other => format!("{other:?}"),
                })
                .unwrap_or_default()
        };
        let mut out: Vec<EdgeFacts> = g
            .graph
            .edge_indices()
            .filter_map(|e| {
                let (src, tgt) = g.graph.edge_endpoints(e)?;
                let data = &g.graph[e];
                let mut props: Vec<(String, String)> = data
                    .property_keys(&g.interner)
                    .map(|k| {
                        (
                            k.to_string(),
                            match data.get_property(k) {
                                Some(Value::String(s)) => s.clone(),
                                other => format!("{other:?}"),
                            },
                        )
                    })
                    .collect();
                props.sort();
                Some((
                    name(src),
                    data.connection_type_str(&g.interner).to_string(),
                    name(tgt),
                    props,
                ))
            })
            .collect();
        out.sort();
        out
    }

    fn vault_build(dir: &Path) -> BuildOutput {
        build(
            dir,
            &BuildOptions::for_dialect(crate::okf::Dialect::Obsidian),
        )
        .unwrap()
    }

    /// A vault build whose profile the caller adjusts first — how a
    /// `.kglite/vault.yaml` reaches the builder: as profile overrides.
    fn vault_build_with(dir: &Path, tune: impl FnOnce(&mut Profile)) -> BuildOutput {
        let mut opts = BuildOptions::for_dialect(crate::okf::Dialect::Obsidian);
        tune(&mut opts.profile);
        build(dir, &opts).unwrap()
    }

    /// `(id, title)` of every node carrying `label`, sorted by id.
    fn nodes_with_titles(g: &DirGraph, label: &str) -> Vec<(String, String)> {
        let display = |v: Value| match v {
            Value::String(s) => s,
            other => format!("{other:?}"),
        };
        let mut out: Vec<(String, String)> = g
            .graph
            .node_indices()
            .filter_map(|n| {
                let nd = g.node_view(n)?;
                (nd.node_type_str(&g.interner) == label).then(|| {
                    (
                        display(nd.id().into_owned()),
                        display(nd.title().into_owned()),
                    )
                })
            })
            .collect();
        out.sort();
        out
    }

    /// The label of each note, by id.
    fn labels_by_id(g: &DirGraph) -> BTreeMap<String, String> {
        g.graph
            .node_indices()
            .filter_map(|n| {
                let nd = g.node_view(n)?;
                match nd.id().into_owned() {
                    Value::String(id) => Some((id, nd.node_type_str(&g.interner).to_string())),
                    _ => None,
                }
            })
            .collect()
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

    #[test]
    fn the_tag_hub_is_case_sensitive_in_every_dialect() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.md", "---\ntags: [Seismic, seismic]\n---\nx");
        let out = vault_build(dir.path());
        assert_eq!(
            nodes_with_titles(&out.graph, TAG_LABEL),
            vec![
                ("Seismic".to_string(), "Seismic".to_string()),
                ("seismic".to_string(), "seismic".to_string()),
            ],
            "folding tag identity would silently merge nodes in every bundle \
             already built; a vault that wants it declares the hub again"
        );
        assert_eq!(out.report.edges_by_type.get(TAGGED_CONN_TYPE), Some(&2));
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
    fn heading_edges_beat_the_built_in_ladder_whatever_the_casing() {
        let dir = tempdir().unwrap();
        write(dir.path(), "b.md", "leaf");
        write(
            dir.path(),
            "a.md",
            "## Related Topics\n\n[[b]]\n\n## References\n\n[[b]]",
        );
        let plain = vault_build(dir.path());
        let types: BTreeSet<String> = plain
            .report
            .edges_by_type
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        assert!(
            types.contains("RELATED") && types.contains("REFERENCES"),
            "the built-in ladder types both sections: {types:?}"
        );

        let declared = vault_build_with(dir.path(), |p| {
            // Declared in a different casing than the heading is written in.
            p.heading_edges
                .insert("related topics".to_string(), "RELATED_TO".to_string());
        });
        assert_eq!(
            declared.report.edges_by_type.get("RELATED_TO"),
            Some(&1),
            "the map wins over the ladder"
        );
        assert_eq!(declared.report.edges_by_type.get("RELATED"), None);
        assert_eq!(
            declared.report.edges_by_type.get("REFERENCES"),
            Some(&1),
            "and a heading the map does not name keeps its ladder rung"
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
    fn golden_vault_bundle_under_a_declared_hub_and_heading_map() {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/okf/golden/vault");
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
    fn vault_body_link_edges_carry_section_and_anchor() {
        let dir = tempdir().unwrap();
        write(dir.path(), "b.md", "leaf");
        write(
            dir.path(),
            "a.md",
            "First [[b]] is section-less.\n\n## Deep dive\n\nThen [[b#Goals]].",
        );
        let out = vault_build(dir.path());
        assert_eq!(
            edges_of(&out.graph),
            vec![
                ("a".into(), "LINKS_TO".into(), "b".into(), vec![]),
                (
                    "a".into(),
                    "LINKS_TO".into(),
                    "b".into(),
                    vec![
                        ("anchor".to_string(), "Goals".to_string()),
                        ("section".to_string(), "Deep dive".to_string()),
                    ]
                ),
            ],
            "two links differing only in their section are two edges"
        );
    }

    /// Two links differing only in `section` survive *another* group of the
    /// same connection type being emitted first. Re-detecting the initial-load
    /// regime per call made that first group register `LINKS_TO`, which flipped
    /// every later group into merging and folded these two onto one edge — so
    /// the graph depended on which endpoint labels happened to sort first.
    #[test]
    fn vault_parallel_link_edges_survive_an_earlier_group_of_the_same_type() {
        let dir = tempdir().unwrap();
        write(dir.path(), "b.md", "leaf");
        write(dir.path(), "zzz/a.md", "[[b]]\n\n## Sec\n\n[[b]] again");
        write(dir.path(), "sub/c.md", "see [[b]]");
        let out = vault_build(dir.path());
        assert_eq!(
            out.report.edges_by_type.get("LINKS_TO"),
            Some(&3),
            "the report counts three rows"
        );
        assert_eq!(
            edges_of(&out.graph)
                .iter()
                .filter(|(_, c, _, _)| c == "LINKS_TO")
                .count(),
            3,
            "and the graph holds three edges"
        );
    }

    /// The ladder's first rungs apply to a wikilink too (VAULT.md §5.2): a
    /// declared `id:` is a link target, and a `/`-bearing name is a path —
    /// vault-relative first, then relative to the linking note.
    #[test]
    fn vault_wikilinks_resolve_by_id_and_by_path() {
        let dir = tempdir().unwrap();
        write(dir.path(), "notes/meeting.md", "---\nid: mtg-1\n---\nleaf");
        write(dir.path(), "wing/sub/target.md", "leaf");
        write(dir.path(), "wing/other.md", "leaf");
        write(
            dir.path(),
            "wing/a.md",
            "[[mtg-1]] and [[wing/sub/target]] and [[sub/target]]",
        );
        let out = vault_build(dir.path());
        assert_eq!(out.report.dangling, 0, "no rung fell through to a stub");
        let targets: Vec<String> = edges_of(&out.graph)
            .into_iter()
            .filter(|(_, c, _, _)| c == "LINKS_TO")
            .map(|(_, _, t, _)| t)
            .collect();
        assert_eq!(
            targets,
            vec!["mtg-1".to_string(), "target".to_string()],
            "both spellings reach that note, and land on one edge: they sit in \
             the same section with no anchor, so VAULT.md §5.4 makes them one \
             relationship"
        );
    }

    #[test]
    fn vault_embed_of_a_note_becomes_an_edge() {
        let dir = tempdir().unwrap();
        write(dir.path(), "b.md", "leaf");
        write(dir.path(), "a.md", "![[b]] and ![[diagram.png]]");
        let out = vault_build(dir.path());
        assert_eq!(
            edges_of(&out.graph),
            vec![("a".into(), "EMBEDS".into(), "b".into(), vec![])],
            "an image embed is an attachment, not a link"
        );
        assert_eq!(out.report.dangling, 0, "and it mints no stub");
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

    #[test]
    fn vault_alias_resolves_a_link_and_is_not_a_stub() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "seismic.md",
            "---\naliases:\n- Seismic interpretation\n---\nleaf",
        );
        write(dir.path(), "a.md", "see [[Seismic interpretation]]");
        let out = vault_build(dir.path());
        assert_eq!(out.report.dangling, 0);
        assert_eq!(
            edges_of(&out.graph),
            vec![("a".into(), "LINKS_TO".into(), "seismic".into(), vec![])]
        );
        // A scalar `aliases:` names the one alias it spells.
        write(
            dir.path(),
            "seismic.md",
            "---\naliases: Seismic interpretation\n---\nleaf",
        );
        assert_eq!(vault_build(dir.path()).report.dangling, 0);
    }

    #[test]
    fn vault_alias_collisions_warn() {
        let dir = tempdir().unwrap();
        write(dir.path(), "atlas.md", "leaf");
        write(
            dir.path(),
            "one.md",
            "---\naliases:\n- atlas\n---\nclaims a stem",
        );
        write(dir.path(), "two.md", "---\naliases:\n- shared\n---\nleaf");
        write(dir.path(), "three.md", "---\naliases:\n- shared\n---\nleaf");
        let r = vault_build(dir.path()).report;
        assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
        assert!(
            r.warnings[0].contains("alias `atlas` on one.md") && r.warnings[0].contains("`atlas`"),
            "{}",
            r.warnings[0]
        );
        assert!(
            r.warnings[1].contains("alias `shared`")
                && r.warnings[1].contains("`three`")
                && r.warnings[1].contains("`two`"),
            "{}",
            r.warnings[1]
        );
    }

    #[test]
    fn vault_wikilink_valued_frontmatter_keys_become_typed_edges() {
        let dir = tempdir().unwrap();
        write(dir.path(), "x.md", "leaf");
        write(dir.path(), "y.md", "leaf");
        write(
            dir.path(),
            "a.md",
            "---\nsee_also: \"[[x]]\"\ndepends_on:\n- \"[[x]]\"\n- \"[[y]]\"\nreviewers:\n- \"[[x]]\"\n- ada\n---\nbody",
        );
        let out = vault_build(dir.path());
        assert_eq!(
            edges_of(&out.graph),
            vec![
                ("a".into(), "DEPENDS_ON".into(), "x".into(), vec![]),
                ("a".into(), "DEPENDS_ON".into(), "y".into(), vec![]),
                ("a".into(), "SEE_ALSO".into(), "x".into(), vec![]),
            ]
        );
        let n = out
            .graph
            .graph
            .node_indices()
            .find(|&n| {
                out.graph.node_view(n).map(|nd| nd.id().into_owned())
                    == Some(Value::String("a".into()))
            })
            .unwrap();
        let prop =
            |k: &str| GraphRead::get_node_property(&out.graph.graph, n, InternedKey::from_str(k));
        assert_eq!(prop("see_also"), None, "an edge key is not also a property");
        assert_eq!(prop("depends_on"), None);
        assert_eq!(
            prop("reviewers"),
            Some(Value::List(vec![
                Value::String("[[x]]".into()),
                Value::String("ada".into()),
            ])),
            "a list mixing wikilinks with plain strings stays a property"
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

    #[test]
    fn loose_keeps_every_vault_link_rule_off() {
        let dir = tempdir().unwrap();
        write(dir.path(), "x.md", "---\ntype: Note\n---\nleaf");
        write(
            dir.path(),
            "a.md",
            "---\ntype: Note\ndepends_on: \"[[x]]\"\naliases:\n- Ex\n---\n## Sec\n\n#tag and [[x]] and ![[x]] and [[Ex]]",
        );
        let out = build(
            dir.path(),
            &BuildOptions::for_dialect(crate::okf::Dialect::Loose),
        )
        .unwrap();
        assert_eq!(
            edges_of(&out.graph),
            vec![
                ("a".into(), "LINKS_TO".into(), "Ex".into(), vec![]),
                ("a".into(), "LINKS_TO".into(), "x".into(), vec![]),
            ],
            "two plain wikilinks: no section, no EMBEDS, no frontmatter edge"
        );
        assert_eq!(
            provisional_count(&out.graph),
            1,
            "`[[Ex]]` dangles — the alias rung is a vault rule"
        );
        assert_eq!(count_label(&out.graph, TAG_LABEL), 0);
        assert_eq!(
            GraphRead::get_node_property(
                &out.graph.graph,
                out.graph
                    .graph
                    .node_indices()
                    .find(|&n| out.graph.node_view(n).map(|nd| nd.id().into_owned())
                        == Some(Value::String("a".into())))
                    .unwrap(),
                InternedKey::from_str("depends_on")
            ),
            Some(Value::String("[[x]]".into())),
            "the key stays an ordinary property outside a vault"
        );
    }

    #[test]
    fn empty_bundle_is_empty_graph() {
        let dir = tempdir().unwrap();
        let g = build(dir.path(), &BuildOptions::default()).unwrap().graph;
        assert_eq!(g.graph.node_indices().count(), 0);
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

    #[test]
    fn slug_normalization_unifies_separators() {
        assert_eq!(
            normalize_slug("Project_0-10 Shipped"),
            "project-0-10-shipped"
        );
        assert_eq!(
            normalize_slug("feedback_cypher_first"),
            "feedback-cypher-first"
        );
        assert_eq!(normalize_slug("--A__B--"), "a-b");
    }

    #[test]
    fn resolves_slug_and_title_variants_without_dangling() {
        let dir = tempdir().unwrap();
        // file uses underscores; wikilinks use hyphen-slug and the human title.
        write(
            dir.path(),
            "feedback_cypher_first.md",
            "---\ntype: Note\ntitle: Cypher First\n---\nleaf",
        );
        write(
            dir.path(),
            "a.md",
            "---\ntype: Note\n---\nsee [[feedback-cypher-first]] and [[Cypher First]]",
        );
        // `for_dialect`, not a struct literal: the profile carries `wikilinks`
        // now, and a literal would leave it off and test nothing.
        let opts = BuildOptions::for_dialect(crate::okf::model::Dialect::Loose);
        let g = build(dir.path(), &opts).unwrap().graph;
        // both wikilinks resolve to the one file → 2 nodes, no provisional stub.
        assert_eq!(g.graph.node_indices().count(), 2);
        assert_eq!(provisional_count(&g), 0);
    }

    #[test]
    fn genuinely_missing_target_still_dangles() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "a.md",
            "---\ntype: Note\n---\nsee [[truly-absent]]",
        );
        // `for_dialect`, not a struct literal: the profile carries `wikilinks`
        // now, and a literal would leave it off and test nothing.
        let opts = BuildOptions::for_dialect(crate::okf::model::Dialect::Loose);
        let g = build(dir.path(), &opts).unwrap().graph;
        assert_eq!(provisional_count(&g), 1);
    }
}
