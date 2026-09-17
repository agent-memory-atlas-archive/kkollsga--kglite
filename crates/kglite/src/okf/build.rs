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
//! lists) are JSON-encoded into String columns — the same convention code-graph builders
//! uses for `parameters`/`fields` (the columnar `DataFrame` has no list/map
//! column type).

use crate::datatypes::values::{DataFrame, Value};
use crate::graph::mutation::maintain;
use crate::graph::DirGraph;
use crate::okf::model::{
    BuildOptions, BuildReport, ConceptDoc, Link, CONTAINS_CONN_TYPE, DEFAULT_LABEL, FOLDER_LABEL,
    SOURCE_LABEL, TAGGED_CONN_TYPE, TAG_LABEL,
};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// `(source_label, target_label, conn_type)` → `[(source_id, target_id)]`.
type EdgeGroups = HashMap<(String, String, String), Vec<(String, String)>>;

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
    let docs = super::parse_concepts(&walked.concepts, opts);
    let mut report = BuildReport {
        files_scanned: walked.concepts.len(),
        concepts: docs.len(),
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
    build_folders(&mut graph, &docs, &walked.index_files, &mut report)?;
    build_edges(&mut graph, &docs, &mut report)?;
    Ok(BuildOutput {
        graph: Arc::new(graph),
        report,
    })
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
    for ((src_label, tgt_label, conn), pairs) in groups {
        *report.edges_by_type.entry(conn.clone()).or_default() += pairs.len();
        let rows: Vec<Vec<Value>> = pairs
            .into_iter()
            .map(|(s, t)| vec![Value::String(s), Value::String(t)])
            .collect();
        let df = DataFrame::from_cypher_rows(
            vec!["source_id".to_string(), "target_id".to_string()],
            rows,
        )?;
        maintain::add_connections(
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
        )?;
    }
    Ok(())
}

/// Materialize the directory hierarchy as `Folder` nodes:
/// `(:Folder)-[:CONTAINS]->(:Concept)` and `(:Folder)-[:CONTAINS]->(:Folder)`.
/// A directory's `index.md` enriches its Folder node's title/description (so the
/// reserved file is recovered as structure rather than discarded). Co-located
/// concepts gain a 2-hop hub, capturing the taxonomic meaning of the layout.
fn build_folders(
    graph: &mut DirGraph,
    docs: &[ConceptDoc],
    index_files: &HashMap<String, PathBuf>,
    report: &mut BuildReport,
) -> Result<(), String> {
    // Every directory holding a concept, plus all ancestor directories.
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for d in docs {
        let mut p = super::parent_dir(&d.concept_id).to_string();
        while !p.is_empty() {
            let parent = super::parent_dir(&p).to_string();
            dirs.insert(p);
            p = parent;
        }
    }
    if dirs.is_empty() {
        return Ok(());
    }
    count_nodes(report, FOLDER_LABEL, dirs.len());

    // Folder nodes (id = dir path; title/description from index.md if present).
    let mut rows: Vec<Vec<Value>> = Vec::with_capacity(dirs.len());
    for dir in &dirs {
        let (title, desc) = index_files
            .get(dir)
            .map(|p| folder_meta(p))
            .unwrap_or((None, None));
        let title = title.unwrap_or_else(|| dir.rsplit('/').next().unwrap_or(dir).to_string());
        rows.push(vec![
            Value::String(dir.clone()),
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

    // CONTAINS edges: folder → immediate child concepts and subfolders.
    let mut groups: EdgeGroups = HashMap::new();
    for d in docs {
        let dir = super::parent_dir(&d.concept_id);
        if !dir.is_empty() {
            groups
                .entry((
                    FOLDER_LABEL.to_string(),
                    d.label.clone(),
                    CONTAINS_CONN_TYPE.to_string(),
                ))
                .or_default()
                .push((dir.to_string(), d.concept_id.clone()));
        }
    }
    for dir in &dirs {
        let parent = super::parent_dir(dir);
        if !parent.is_empty() {
            groups
                .entry((
                    FOLDER_LABEL.to_string(),
                    FOLDER_LABEL.to_string(),
                    CONTAINS_CONN_TYPE.to_string(),
                ))
                .or_default()
                .push((parent.to_string(), dir.clone()));
        }
    }
    emit_groups(graph, groups, report)
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

/// Synthesize `Tag` and `Source` nodes from the concepts' tags and external
/// links. Added before edges so the `TAGGED` / `CITES` connections find real
/// endpoints instead of vivifying provisional stubs.
fn build_aux_nodes(
    graph: &mut DirGraph,
    docs: &[ConceptDoc],
    report: &mut BuildReport,
) -> Result<(), String> {
    let mut tags: BTreeSet<&str> = BTreeSet::new();
    let mut sources: BTreeSet<&str> = BTreeSet::new();
    for d in docs {
        for t in doc_tags(d) {
            tags.insert(t);
        }
        for l in &d.links {
            if l.is_external {
                sources.insert(l.target.as_str());
            }
        }
    }
    count_nodes(report, TAG_LABEL, tags.len());
    count_nodes(report, SOURCE_LABEL, sources.len());
    add_id_nodes(graph, TAG_LABEL, &tags)?;
    add_id_nodes(graph, SOURCE_LABEL, &sources)?;
    Ok(())
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

/// The string items of a concept's `tags` frontmatter list (empty if none).
fn doc_tags(d: &ConceptDoc) -> Vec<&str> {
    d.props
        .iter()
        .filter(|(k, _)| k == "tags")
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
    let mut by_label: HashMap<&str, Vec<&ConceptDoc>> = HashMap::new();
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
                row.push(pm.get(k).map(|v| column_value(v)).unwrap_or(Value::Null));
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

/// Coerce a property value for columnar storage: structured values (lists, maps)
/// JSON-encode to a String; scalars pass through unchanged. `pub(crate)` so
/// codingest's docs pass reuses the same frontmatter→column convention.
pub(crate) fn column_value(v: &Value) -> Value {
    match v {
        Value::List(_) | Value::Map(_) => Value::String(
            serde_json::to_string(&crate::param::kglite_value_to_json(v)).unwrap_or_default(),
        ),
        other => other.clone(),
    }
}

/// Build the concept-level edges: semantic links (typed via the ladder; internal
/// → concept, external → Source) and tag membership. Directory `CONTAINS` edges
/// are built in [`build_folders`].
fn build_edges(
    graph: &mut DirGraph,
    docs: &[ConceptDoc],
    report: &mut BuildReport,
) -> Result<(), String> {
    let resolver = Resolver::new(docs);
    let mut groups: EdgeGroups = HashMap::new();
    // Dangling internal-link targets — concepts referenced but not present.
    let mut dangling: BTreeSet<String> = BTreeSet::new();

    // Semantic links: internal → concept edges (resolved), external → Source.
    for d in docs {
        for link in &d.links {
            let (target_label, target_id) = if link.is_external {
                (SOURCE_LABEL.to_string(), link.target.clone())
            } else {
                let (id, label) = resolver.resolve(link);
                if !resolver.id_to_label.contains_key(id.as_str()) {
                    dangling.insert(id.clone());
                }
                (label, id)
            };
            groups
                .entry((d.label.clone(), target_label, link.conn_type.clone()))
                .or_default()
                .push((d.concept_id.clone(), target_id));
        }
    }

    // Pre-create dangling targets as provisional `Concept` nodes carrying
    // `concept_id` — so "references not yet written" are queryable identically to
    // real concepts (`MATCH (n {_provisional:true}) RETURN n.concept_id`) rather
    // than via the mutator's default `id` stub field.
    report.dangling = dangling.len();
    count_nodes(report, DEFAULT_LABEL, dangling.len());
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

    // Tag membership: concept → Tag.
    for d in docs {
        for tag in doc_tags(d) {
            groups
                .entry((
                    d.label.clone(),
                    TAG_LABEL.to_string(),
                    TAGGED_CONN_TYPE.to_string(),
                ))
                .or_default()
                .push((d.concept_id.clone(), tag.to_string()));
        }
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

/// Forgiving link/wikilink → concept resolver. Tries, most-specific first:
/// exact concept-id (path links) → exact file stem → normalized slug (full path
/// or last segment) → normalized title. Unresolved targets keep their raw id and
/// the default label — `add_connections` vivifies them as `_provisional` stubs.
struct Resolver<'a> {
    id_to_label: HashMap<&'a str, &'a str>,
    stem_to_id: HashMap<&'a str, &'a str>,
    slug_to_id: HashMap<String, &'a str>,
    title_to_id: HashMap<String, &'a str>,
}

impl<'a> Resolver<'a> {
    fn new(docs: &'a [ConceptDoc]) -> Self {
        let mut id_to_label = HashMap::new();
        let mut stem_to_id = HashMap::new();
        let mut slug_to_id = HashMap::new();
        let mut title_to_id = HashMap::new();
        for d in docs {
            id_to_label.insert(d.concept_id.as_str(), d.label.as_str());
            let stem = d.concept_id.rsplit('/').next().unwrap_or(&d.concept_id);
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
        Self {
            id_to_label,
            stem_to_id,
            slug_to_id,
            title_to_id,
        }
    }

    fn label_of(&self, id: &str) -> String {
        self.id_to_label
            .get(id)
            .copied()
            .unwrap_or(DEFAULT_LABEL)
            .to_string()
    }

    fn resolve(&self, link: &Link) -> (String, String) {
        let t = link.target.as_str();
        if !link.is_wikilink {
            if let Some(lbl) = self.id_to_label.get(t) {
                return (t.to_string(), lbl.to_string());
            }
        }
        if let Some(id) = self.stem_to_id.get(t) {
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
        assert!(r.errors.is_empty() && r.warnings.is_empty());
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
        let opts = BuildOptions {
            dialect: crate::okf::model::Dialect::Loose,
            ..BuildOptions::default()
        };
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
        let opts = BuildOptions {
            dialect: crate::okf::model::Dialect::Loose,
            ..BuildOptions::default()
        };
        let g = build(dir.path(), &opts).unwrap().graph;
        assert_eq!(provisional_count(&g), 1);
    }
}
