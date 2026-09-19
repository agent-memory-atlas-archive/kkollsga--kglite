//! The concept-level edges: every link a note wrote, typed through the
//! resolver, emitted together with the structural rows the other builders
//! collected.

use super::resolver::Resolver;
use super::structure::DerivedIndex;
use super::{count_nodes, doc_path, emit_groups, EdgeGroups};
use crate::datatypes::values::{DataFrame, Value};
use crate::graph::mutation::maintain;
use crate::graph::DirGraph;
use crate::okf::model::{BuildOptions, BuildReport, ConceptDoc, Link, DEFAULT_LABEL, SOURCE_LABEL};
use std::collections::BTreeSet;

/// Build the concept-level edges — semantic links, typed via the ladder
/// (internal → concept, external → Source) — and emit them together with the
/// containment and hub rows `groups` arrives carrying, so a relationship two
/// of those sources agree on is one edge.
pub(super) fn build_edges(
    graph: &mut DirGraph,
    docs: &[ConceptDoc],
    opts: &BuildOptions,
    mut groups: EdgeGroups,
    derived: &DerivedIndex,
    report: &mut BuildReport,
) -> Result<(), String> {
    let (resolver, alias_warnings) = Resolver::new(docs, &opts.profile);
    report.warnings.extend(alias_warnings);
    // Dangling internal-link targets — concepts referenced but not present.
    let mut dangling: BTreeSet<String> = BTreeSet::new();

    // Semantic links: internal → concept edges (resolved), external → Source.
    for d in docs {
        for (source, link) in note_links(d).chain(derived_links(d)) {
            let (target_label, target_id) = if link.is_external {
                (SOURCE_LABEL.to_string(), link.target.clone())
            } else {
                let (id, label) = resolver.resolve(link, crate::okf::parent_dir(doc_path(d)));
                if !resolver.id_to_label.contains_key(id.as_str()) {
                    dangling.insert(id.clone());
                    (label, id)
                } else {
                    retarget(link, id, label, derived, d, report)
                }
            };
            // A reversed link (a `parent:` pointing parent → child) is the same
            // edge read from the other end, so only the endpoints swap.
            let (source_id, source_label) = source;
            let (src_label, src_id, tgt_label, tgt_id) = if link.reverse {
                (target_label, target_id, source_label, source_id)
            } else {
                (source_label, source_id, target_label, target_id)
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

    emit_groups(graph, groups, &opts.profile.edge_defaults, report)
}

/// One link's source endpoint as `(id, label)`.
type Source = (String, String);

/// Every link the note itself wrote — prose, frontmatter, edge-table rows.
fn note_links(d: &ConceptDoc) -> impl Iterator<Item = (Source, &Link)> {
    d.links
        .iter()
        .map(|link| ((d.concept_id.clone(), d.label.clone()), link))
}

/// The links a directive wrote **from a derived node** (VAULT.md §5.8). The
/// target travels the same ladder a note's own link travels — stub and all —
/// and only the tail differs: the section the directive sat under.
fn derived_links(d: &ConceptDoc) -> impl Iterator<Item = (Source, &Link)> {
    d.derived
        .links_from
        .iter()
        .filter_map(move |(suffix, link)| {
            let label = d
                .derived
                .nodes
                .iter()
                .find(|node| node.suffix == *suffix)?
                .label
                .clone();
            Some(((format!("{}{suffix}", d.concept_id), label), link))
        })
}

/// The `anchor` a body link carries (VAULT.md §5.4), or `None` for a link
/// without a fragment.
fn anchor_of(link: &Link) -> Option<&str> {
    link.props
        .iter()
        .find(|(k, _)| k == "anchor")
        .and_then(|(_, v)| match v {
            Value::String(anchor) if !anchor.is_empty() => Some(anchor.as_str()),
            _ => None,
        })
}

/// Move a fragment link onto the derived node its fragment names (VAULT.md
/// §5.4): `[[Note#Heading]]` reaches the Section, `[[Note#^id]]` the chunk, and
/// a fragment naming neither leaves the edge on the note and is a warning.
///
/// The note the link resolved to never changes — only which node of that note
/// the edge ends on — and the `anchor` property is kept either way, so the
/// fragment as written survives the move.
fn retarget(
    link: &Link,
    id: String,
    label: String,
    derived: &DerivedIndex,
    d: &ConceptDoc,
    report: &mut BuildReport,
) -> (String, String) {
    let Some(anchor) = anchor_of(link) else {
        return (label, id);
    };
    if let Some((derived_id, derived_label)) = derived.retarget(&id, anchor) {
        return (derived_label, derived_id);
    }
    if derived.sections_declared() {
        report.warnings.push(format!(
            "{}: `#{anchor}` names no heading or block id in `{id}`; the link resolved \
             to the note",
            d.file_path
        ));
    }
    (label, id)
}

#[cfg(test)]
#[path = "edges_tests.rs"]
mod edges_tests;
