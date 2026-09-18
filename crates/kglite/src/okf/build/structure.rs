//! The nodes a note's own body derived (VAULT.md §7.1): sections and chunks as
//! rows of their own, and the anchor index a `[[Note#Heading]]` retargets
//! through.
//!
//! The parse produced them as **suffixes** of the note's id (see
//! `okf::structure::derive`); this is where they become ids, because
//! `resolve_ids` has by now settled what each note's id actually is. Nothing
//! here reads the filesystem: a derived node is not a file, and it therefore
//! never carries `file_path` — which is exactly what keeps the exporter from
//! writing one back out as a note (§10.1).

use super::nodes::apply_declared_types;
use super::{count_nodes, EdgeGroups};
use crate::datatypes::values::{DataFrame, Value};
use crate::graph::mutation::maintain;
use crate::graph::DirGraph;
use crate::okf::model::{BuildOptions, BuildReport, ConceptDoc};
use crate::okf::structure::profile::render_embed_text;
use crate::okf::structure::StructureProfile;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The property `embed_text:` materialises (VAULT.md §7.1) — the name a vault
/// then writes in `embed:` and `text_indexes:`.
const EMBED_TEXT_PROPERTY: &str = "embed_text";

/// Where a `#fragment` link lands once a vault derives sections and chunks.
///
/// Built from the same rows the frames were built from, so an anchor can only
/// resolve to a node that exists.
#[derive(Debug, Default)]
pub(super) struct DerivedIndex {
    notes: HashMap<String, NoteAnchors>,
    /// Whether `sections:` is declared at all. Without it a `#fragment` names
    /// nothing derivable and the edge stays on the note in silence, exactly as
    /// it did before this feature.
    sections_declared: bool,
}

#[derive(Debug, Default)]
struct NoteAnchors {
    /// Id suffix (`#A#B`, `#^id`) → the label of the node it names.
    by_suffix: HashMap<String, String>,
    /// Lowercased suffix → the suffix as the note spelled it, for the
    /// case-insensitive rung.
    by_lower_suffix: HashMap<String, String>,
    /// A single heading's text → the **first** section of that title, which is
    /// the one Obsidian jumps to (VAULT.md §5.4, §7.1).
    by_title: HashMap<String, String>,
    by_lower_title: HashMap<String, String>,
}

impl DerivedIndex {
    /// The derived node a link's `anchor` names in `note_id`, as
    /// `(id, label)` — or `None`, which leaves the edge on the note.
    ///
    /// The ladder is Obsidian's: the whole fragment as a heading path, then
    /// the same ignoring case, then a bare heading title matched against the
    /// first section that carries it.
    pub(super) fn retarget(&self, note_id: &str, anchor: &str) -> Option<(String, String)> {
        let anchors = self.notes.get(note_id)?;
        let suffix = format!("#{anchor}");
        let suffix = if anchors.by_suffix.contains_key(&suffix) {
            suffix
        } else {
            anchors
                .by_lower_suffix
                .get(&suffix.to_lowercase())
                .or_else(|| anchors.by_title.get(anchor))
                .or_else(|| anchors.by_lower_title.get(&anchor.to_lowercase()))
                .cloned()?
        };
        let label = anchors.by_suffix.get(&suffix)?;
        Some((format!("{note_id}{suffix}"), label.clone()))
    }

    /// Whether an unresolved fragment is worth a warning: only a vault that
    /// derives sections can know that a heading is absent.
    pub(super) fn sections_declared(&self) -> bool {
        self.sections_declared
    }
}

/// Emit every derived node and collect its edges (VAULT.md §7.1).
pub(super) fn build_structure(
    graph: &mut DirGraph,
    docs: &[ConceptDoc],
    opts: &BuildOptions,
    declared_types: Option<&BTreeMap<String, BTreeMap<String, String>>>,
    unmatched: &mut BTreeSet<(String, String)>,
    report: &mut BuildReport,
) -> Result<(EdgeGroups, DerivedIndex), String> {
    let Some(profile) = &opts.profile.structure else {
        return Ok((EdgeGroups::new(), DerivedIndex::default()));
    };
    let mut index = DerivedIndex {
        sections_declared: profile.sections.is_some(),
        ..DerivedIndex::default()
    };
    let mut rows_by_label: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    let mut groups = EdgeGroups::new();
    for doc in docs {
        if doc.derived.nodes.is_empty() {
            continue;
        }
        let anchors = index.notes.entry(doc.concept_id.clone()).or_default();
        for node in &doc.derived.nodes {
            anchors
                .by_suffix
                .insert(node.suffix.clone(), node.label.clone());
            anchors
                .by_lower_suffix
                .entry(node.suffix.to_lowercase())
                .or_insert_with(|| node.suffix.clone());
            // A section's own title, and only a section's: a chunk's
            // `section_title` is its container's and naming it here would let
            // `[[Note#Heading]]` land on a chunk.
            if let Some(Value::String(title)) = property(node, "title") {
                anchors
                    .by_title
                    .entry(title.clone())
                    .or_insert_with(|| node.suffix.clone());
                anchors
                    .by_lower_title
                    .entry(title.to_lowercase())
                    .or_insert_with(|| node.suffix.clone());
            }
            rows_by_label
                .entry(node.label.clone())
                .or_default()
                .push(row_for(doc, node, profile));
        }
        collect_edges(doc, &mut groups);
    }
    emit_nodes(graph, rows_by_label, declared_types, unmatched, report)?;
    warn_unmatched_rules(profile, report);
    Ok((groups, index))
}

/// One derived node's columns, as `(property, value)` pairs.
type Row = Vec<(String, Value)>;

fn property<'a>(node: &'a crate::okf::structure::DerivedNode, name: &str) -> Option<&'a Value> {
    node.props.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

/// Everything one derived node carries: what it derived, what it inherits from
/// the note, and the `embed_text:` template rendered against both.
fn row_for(
    doc: &ConceptDoc,
    node: &crate::okf::structure::DerivedNode,
    profile: &StructureProfile,
) -> Row {
    let id = format!("{}{}", doc.concept_id, node.suffix);
    let mut row: Row = vec![("concept_id".to_string(), Value::String(id.clone()))];
    row.extend(node.props.iter().cloned());
    row.push(("note_id".to_string(), Value::String(doc.concept_id.clone())));
    if let Some(section) = &node.section {
        row.push((
            "section_id".to_string(),
            Value::String(format!("{}{section}", doc.concept_id)),
        ));
    }
    // `inherit:` copies the note's own frontmatter (VAULT.md §7.1); a key the
    // note does not carry is simply absent here, and the config refused a key
    // a derived node defines itself before the build started.
    for name in &profile.inherit {
        if let Some((_, value)) = doc.props.iter().find(|(k, _)| k == name) {
            row.push((name.clone(), value.clone()));
        }
    }
    if let Some(text) = &node.text {
        if let Some(template) = &profile.embed_text {
            row.push((
                EMBED_TEXT_PROPERTY.to_string(),
                Value::String(render_embed_text(
                    template,
                    &doc.title,
                    node.section_title.as_deref().unwrap_or_default(),
                    &node.heading_path,
                    text,
                    &id,
                )),
            ));
        }
        row.push(("text".to_string(), Value::String(text.clone())));
    }
    row
}

/// The derived edges of one note, with both endpoints resolved to ids and
/// labels. The note itself is an endpoint wherever a rule says "or the note".
fn collect_edges(doc: &ConceptDoc, groups: &mut EdgeGroups) {
    let labels: HashMap<&str, &str> = doc
        .derived
        .nodes
        .iter()
        .map(|n| (n.suffix.as_str(), n.label.as_str()))
        .collect();
    let endpoint = |suffix: &Option<String>| match suffix {
        Some(suffix) => (
            format!("{}{suffix}", doc.concept_id),
            labels
                .get(suffix.as_str())
                .copied()
                .unwrap_or("")
                .to_string(),
        ),
        None => (doc.concept_id.clone(), doc.label.clone()),
    };
    for edge in &doc.derived.edges {
        let (source_id, source_label) = endpoint(&edge.source);
        let (target_id, target_label) = endpoint(&Some(edge.target.clone()));
        groups
            .entry((edge.conn_type.clone(), source_label, target_label))
            .or_default()
            .push((source_id, target_id, Vec::new()));
    }
}

/// One `add_nodes` per derived label. Columns are the union of what that
/// label's rows carry, so a chunk with no `section_id` (the prose above the
/// first heading) gets a Null the mutator drops rather than a column of its
/// own.
fn emit_nodes(
    graph: &mut DirGraph,
    rows_by_label: BTreeMap<String, Vec<Row>>,
    declared_types: Option<&BTreeMap<String, BTreeMap<String, String>>>,
    unmatched: &mut BTreeSet<(String, String)>,
    report: &mut BuildReport,
) -> Result<(), String> {
    for (label, rows) in rows_by_label {
        count_nodes(report, &label, rows.len());
        let mut columns: Vec<String> = rows
            .iter()
            .flat_map(|row| row.iter().map(|(k, _)| k.clone()))
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect();
        // The id column first, so the frame reads like every other one here.
        columns.sort_by_key(|c| (c != "concept_id", c.clone()));
        let declared: BTreeMap<&str, &str> = declared_types
            .and_then(|t| t.get(&label))
            .into_iter()
            .flatten()
            .filter(|(property, _)| property.as_str() != "concept_id")
            .map(|(property, keyword)| (property.as_str(), keyword.as_str()))
            .collect();
        for property in declared.keys() {
            unmatched.remove(&(label.clone(), (*property).to_string()));
        }
        unmatched.remove(&(label.clone(), "concept_id".to_string()));
        let has_title = columns.iter().any(|c| c == "title");
        let mut frame_rows = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut values: Vec<Value> = columns
                .iter()
                .map(|column| {
                    row.iter()
                        .find(|(k, _)| k == column)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(Value::Null)
                })
                .collect();
            if !declared.is_empty() {
                let id = match &values[0] {
                    Value::String(id) => id.clone(),
                    other => crate::datatypes::values::raw_string(other),
                };
                apply_declared_types(&mut values, &columns, &declared, &label, &id, report);
            }
            frame_rows.push(values);
        }
        let df = DataFrame::from_cypher_rows(columns, frame_rows)?;
        maintain::add_nodes(
            graph,
            df,
            label.clone(),
            "concept_id".to_string(),
            has_title.then(|| "title".to_string()),
            Some("update".to_string()),
        )?;
    }
    Ok(())
}

/// A declared rule that derived nothing anywhere in the vault (VAULT.md §9).
/// Worth a warning and not an error: a vault mid-authoring legitimately has no
/// callout yet, and the declaration is still what it means to build.
fn warn_unmatched_rules(profile: &StructureProfile, report: &mut BuildReport) {
    for (declared, label) in [
        (
            profile.sections.is_some(),
            profile.sections.as_ref().map(|r| r.label.as_str()),
        ),
        (
            profile.chunks.is_some(),
            profile.chunks.as_ref().map(|r| r.label.as_str()),
        ),
    ] {
        let Some(label) = label.filter(|_| declared) else {
            continue;
        };
        if !report.nodes_by_label.contains_key(label) {
            report.warnings.push(format!(
                "`vault.yaml` declares a `structure:` rule for `{label}`, but no note's \
                 body produced one"
            ));
        }
    }
}

#[cfg(test)]
#[path = "structure_tests.rs"]
mod structure_tests;
