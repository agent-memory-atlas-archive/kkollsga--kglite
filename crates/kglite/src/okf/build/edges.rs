//! The concept-level edges: every link a note wrote, typed through the
//! resolver, emitted together with the structural rows the other builders
//! collected.

use super::resolver::Resolver;
use super::{count_nodes, doc_path, emit_groups, EdgeGroups};
use crate::datatypes::values::{DataFrame, Value};
use crate::graph::mutation::maintain;
use crate::graph::DirGraph;
use crate::okf::model::{BuildOptions, BuildReport, ConceptDoc, DEFAULT_LABEL, SOURCE_LABEL};
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
                let (id, label) = resolver.resolve(link, crate::okf::parent_dir(doc_path(d)));
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

#[cfg(test)]
#[path = "edges_tests.rs"]
mod edges_tests;
