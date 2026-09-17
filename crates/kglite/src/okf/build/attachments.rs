//! Attachments: the §6.2 resolution ladder, the `Image` / `Attachment` nodes
//! it reaches, and the stubs for what it does not (VAULT.md §6).

use super::{count_nodes, doc_path, EdgeGroups};
use crate::datatypes::values::{DataFrame, Value};
use crate::graph::mutation::maintain;
use crate::graph::DirGraph;
use crate::okf::links::normalize_path_parts;
use crate::okf::model::{extension_of, label_for_mime, mime_for_extension};
use crate::okf::model::{
    BuildReport, ConceptDoc, Profile, HAS_ATTACHMENT_CONN_TYPE, HAS_IMAGE_CONN_TYPE, IMAGE_LABEL,
};
use crate::okf::walk::DiscoveredAttachment;
use std::collections::{BTreeMap, HashMap};

/// One attachment reference after the §6.2 ladder ran.
enum Resolution<'a> {
    /// The file, and its vault-relative path (the node id).
    Found(&'a DiscoveredAttachment),
    /// The bare filename named more than one file, so it names none.
    Ambiguous(Vec<&'a str>),
    /// Nothing in the vault matches.
    Missing,
}

/// The vault's non-`.md` files, indexed for the §6.2 ladder.
struct AttachmentIndex<'a> {
    by_path: HashMap<&'a str, &'a DiscoveredAttachment>,
    /// Filename → every file with that name. The third rung resolves only
    /// when there is exactly one; a `Vec` is what lets the ambiguity be
    /// *reported* instead of silently picking a winner.
    by_name: HashMap<&'a str, Vec<&'a DiscoveredAttachment>>,
}

impl<'a> AttachmentIndex<'a> {
    fn new(files: &'a [DiscoveredAttachment]) -> Self {
        let mut by_path = HashMap::with_capacity(files.len());
        let mut by_name: HashMap<&str, Vec<&DiscoveredAttachment>> = HashMap::new();
        for f in files {
            by_path.insert(f.rel_path.as_str(), f);
            let name = f.rel_path.rsplit('/').next().unwrap_or(&f.rel_path);
            by_name.entry(name).or_default().push(f);
        }
        Self { by_path, by_name }
    }

    /// Walk the ladder: note-relative → vault-root-relative → a unique
    /// filename anywhere in the vault (VAULT.md §6.2).
    ///
    /// A leading `/` means vault-root, matching how a path *link* has always
    /// read one — so the note-relative rung is skipped for it rather than
    /// resolving `/img/x.png` against the note's own directory.
    fn resolve(&self, target: &str, source_dir: &str) -> Resolution<'a> {
        let rooted = target.starts_with('/');
        let bare = target.trim_start_matches('/');
        if !rooted && !source_dir.is_empty() {
            let joined = normalize_path_parts(source_dir.split('/').chain(bare.split('/')));
            if let Some(f) = self.by_path.get(joined.as_str()) {
                return Resolution::Found(f);
            }
        }
        let from_root = normalize_path_parts(bare.split('/'));
        if let Some(f) = self.by_path.get(from_root.as_str()) {
            return Resolution::Found(f);
        }
        let name = from_root.rsplit('/').next().unwrap_or(&from_root);
        match self.by_name.get(name).map(Vec::as_slice) {
            Some([only]) => Resolution::Found(only),
            Some(many) if many.len() > 1 => {
                Resolution::Ambiguous(many.iter().map(|f| f.rel_path.as_str()).collect())
            }
            _ => Resolution::Missing,
        }
    }
}

/// What a resolved attachment node needs, accumulated across every note that
/// references it.
#[derive(Default)]
struct AttachmentNode<'a> {
    label: &'static str,
    mime: &'static str,
    size: u64,
    mtime: Option<i64>,
    /// `Image.text`: distinct alt texts and using-note titles in first-use
    /// order (VAULT.md §6.3), so a caption stays text-searchable — an edge
    /// property is not.
    text: Vec<&'a str>,
}

/// Resolve every `![…]` reference, synthesize the `Image` / `Attachment` nodes
/// it reaches, and return the `HAS_IMAGE` / `HAS_ATTACHMENT` rows (VAULT.md §6).
///
/// Nodes are added here — before [`super::edges::build_edges`] emits — so a reference never
/// vivifies an untyped stub. Nothing reads a file: `size_bytes` and `mtime`
/// come from the walk's `stat`, which is the whole of §6.5.
pub(super) fn build_attachments<'a>(
    graph: &mut DirGraph,
    docs: &'a [ConceptDoc],
    files: &'a [DiscoveredAttachment],
    profile: &Profile,
    report: &mut BuildReport,
) -> Result<EdgeGroups, String> {
    let mut groups: EdgeGroups = BTreeMap::new();
    if !profile.attachments {
        return Ok(groups);
    }
    let index = AttachmentIndex::new(files);
    let mut nodes: BTreeMap<String, AttachmentNode<'a>> = BTreeMap::new();
    // id → (label, whether the filename was ambiguous). Sorted, so the stub
    // frames and their warnings come out in the same order on every run.
    let mut missing: BTreeMap<String, (&'static str, bool)> = BTreeMap::new();
    let mut warnings: BTreeMap<String, String> = BTreeMap::new();

    for d in docs {
        let source_dir = crate::okf::parent_dir(doc_path(d));
        // `ordinal` counts the edges this note emits of each kind, so it is
        // assigned after the dedupe below rather than while scanning: a
        // reference folded into an earlier edge must not consume a number, or
        // the ordinals of one note would have holes in them.
        let mut seen: Vec<(String, Option<&str>, Option<&str>)> = Vec::new();
        for r in &d.attachments {
            let (id, label) = match index.resolve(&r.target, source_dir) {
                Resolution::Found(f) => {
                    let mime = mime_for_extension(&extension_of(&f.rel_path));
                    let entry = nodes.entry(f.rel_path.clone()).or_default();
                    entry.label = label_for_mime(mime);
                    entry.mime = mime;
                    entry.size = f.size;
                    entry.mtime = f.mtime;
                    if entry.label == IMAGE_LABEL {
                        for t in r.alt.as_deref().into_iter().chain([d.title.as_str()]) {
                            if !t.is_empty() && !entry.text.contains(&t) {
                                entry.text.push(t);
                            }
                        }
                    }
                    (f.rel_path.clone(), entry.label)
                }
                other => {
                    // An unresolved reference keeps the name the note wrote,
                    // normalised, so the stub is the thing to go and create.
                    let id = normalize_path_parts(r.target.trim_start_matches('/').split('/'));
                    let label = label_for_mime(mime_for_extension(&extension_of(&id)));
                    missing
                        .entry(id.clone())
                        .or_insert((label, matches!(other, Resolution::Ambiguous(_))));
                    warnings.entry(id.clone()).or_insert(match other {
                        Resolution::Ambiguous(cands) => format!(
                            "missing attachment `{id}`: the filename matches {} — qualify it",
                            cands
                                .iter()
                                .map(|c| format!("`{c}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        _ => format!("missing attachment: `{id}`"),
                    });
                    (id, label)
                }
            };
            // VAULT.md §6.4: two references to one file from one note are one
            // edge unless they differ in `section` or `alt` — the §5.4 rule,
            // applied to attachments.
            let key = (id.clone(), r.section.as_deref(), r.alt.as_deref());
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let conn = if label == IMAGE_LABEL {
                HAS_IMAGE_CONN_TYPE
            } else {
                HAS_ATTACHMENT_CONN_TYPE
            };
            let ordinal = groups
                .get(&(conn.to_string(), d.label.clone(), label.to_string()))
                .map(|rows| rows.iter().filter(|(s, _, _)| s == &d.concept_id).count())
                .unwrap_or(0);
            let mut props = vec![("ordinal".to_string(), Value::Int64(ordinal as i64))];
            if let Some(alt) = &r.alt {
                props.push(("alt".to_string(), Value::String(alt.clone())));
            }
            if let Some(section) = &r.section {
                props.push(("section".to_string(), Value::String(section.clone())));
            }
            groups
                .entry((conn.to_string(), d.label.clone(), label.to_string()))
                .or_default()
                .push((d.concept_id.clone(), id, props));
        }
    }

    add_attachment_nodes(graph, &nodes, report)?;
    report.missing_attachments = missing.len();
    report.ambiguous_attachments = missing.values().filter(|(_, amb)| *amb).count();
    add_missing_attachment_nodes(graph, &missing, report)?;
    report.warnings.extend(warnings.into_values());
    Ok(groups)
}

/// One `add_nodes` per label for the files that resolved. The id column is
/// `path`, which is also §6.3's `path` property — one column, so the id and
/// the property cannot drift apart.
fn add_attachment_nodes(
    graph: &mut DirGraph,
    nodes: &BTreeMap<String, AttachmentNode<'_>>,
    report: &mut BuildReport,
) -> Result<(), String> {
    let mut by_label: BTreeMap<&str, Vec<(&String, &AttachmentNode)>> = BTreeMap::new();
    for (path, n) in nodes {
        by_label.entry(n.label).or_default().push((path, n));
    }
    for (label, group) in by_label {
        count_nodes(report, label, group.len());
        let mut columns = vec![
            "path".to_string(),
            "title".to_string(),
            "mime".to_string(),
            "size_bytes".to_string(),
            "mtime".to_string(),
        ];
        if label == IMAGE_LABEL {
            columns.push("text".to_string());
        }
        let rows: Vec<Vec<Value>> = group
            .iter()
            .map(|(path, n)| {
                let name = path.rsplit('/').next().unwrap_or(path);
                let mut row = vec![
                    Value::String((*path).clone()),
                    Value::String(name.to_string()),
                    Value::String(n.mime.to_string()),
                    Value::Int64(n.size as i64),
                    n.mtime
                        .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
                        .map(|dt| Value::Timestamp(dt.naive_utc()))
                        .unwrap_or(Value::Null),
                ];
                if label == IMAGE_LABEL {
                    row.push(Value::String(n.text.join("\n")));
                }
                row
            })
            .collect();
        let df = DataFrame::from_cypher_rows(columns, rows)?;
        maintain::add_nodes(
            graph,
            df,
            label.to_string(),
            "path".to_string(),
            Some("title".to_string()),
            Some("update".to_string()),
        )?;
    }
    Ok(())
}

/// The stubs for references that resolved to nothing (VAULT.md §6.6). They
/// carry the `_provisional` marker every stub carries — which is what keeps the
/// exporter from writing them back out as files (§10.1) — plus `missing: true`,
/// so "the file is gone" is distinguishable from "the note is unwritten".
fn add_missing_attachment_nodes(
    graph: &mut DirGraph,
    missing: &BTreeMap<String, (&'static str, bool)>,
    report: &mut BuildReport,
) -> Result<(), String> {
    let mut by_label: BTreeMap<&str, Vec<&String>> = BTreeMap::new();
    for (id, (label, _)) in missing {
        by_label.entry(label).or_default().push(id);
    }
    for (label, ids) in by_label {
        count_nodes(report, label, ids.len());
        let rows: Vec<Vec<Value>> = ids
            .iter()
            .map(|id| {
                let name = id.rsplit('/').next().unwrap_or(id);
                vec![
                    Value::String((*id).clone()),
                    Value::String(name.to_string()),
                    Value::Boolean(true),
                    Value::Boolean(true),
                ]
            })
            .collect();
        let df = DataFrame::from_cypher_rows(
            vec![
                "path".to_string(),
                "title".to_string(),
                "_provisional".to_string(),
                "missing".to_string(),
            ],
            rows,
        )?;
        maintain::add_nodes(
            graph,
            df,
            label.to_string(),
            "path".to_string(),
            Some("title".to_string()),
            Some("preserve".to_string()),
        )?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "attachments_tests.rs"]
mod attachments_tests;
