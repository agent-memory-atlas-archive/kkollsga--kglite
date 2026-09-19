//! The nodes a note joins rather than links to: the declared hubs (`Tag`, a
//! vault's `keywords:`) and the `Source` nodes its external links name.

use super::{count_nodes, EdgeGroups};
use crate::datatypes::values::{DataFrame, Value};
use crate::graph::mutation::maintain;
use crate::graph::DirGraph;
use crate::okf::model::{BuildReport, ConceptDoc, Profile, SOURCE_LABEL};
use std::collections::{BTreeMap, BTreeSet};

/// Synthesize `Source` nodes from the concepts' external links. Added before
/// edges so the `CITES` connections find real endpoints instead of vivifying
/// provisional stubs. Hub nodes get the same treatment in [`build_hubs`].
pub(super) fn build_aux_nodes(
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
pub(super) fn build_hubs(
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
            for raw in hub_values(d, key, profile) {
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

/// Every `tag_labels:` rule's nodes and the edges joining them (VAULT.md
/// §5.5).
///
/// Shaped like [`build_hubs`] and deliberately not folded into it: a hub joins
/// the **note**, and these join the derived node the tag was written in, which
/// is a different endpoint and a different row. What they do share — one node
/// per folded id, titled with the commonest spelling — is [`hub_title`].
pub(super) fn build_tag_labels(
    graph: &mut DirGraph,
    docs: &[ConceptDoc],
    profile: &Profile,
    report: &mut BuildReport,
) -> Result<EdgeGroups, String> {
    let mut groups: EdgeGroups = BTreeMap::new();
    for (pattern, spec) in &profile.tag_labels {
        let mut spellings: BTreeMap<String, BTreeMap<&str, usize>> = BTreeMap::new();
        let mut members: Vec<(&ConceptDoc, &crate::okf::tags::TypedTag, String)> = Vec::new();
        for d in docs {
            for tag in d.typed_tags.iter().filter(|t| t.rule == *pattern) {
                let id = tag.name.to_lowercase();
                *spellings
                    .entry(id.clone())
                    .or_default()
                    .entry(tag.name.as_str())
                    .or_default() += 1;
                members.push((d, tag, id));
            }
        }
        if spellings.is_empty() {
            report.warnings.push(format!(
                "`vault.yaml` declares `tag_labels: {pattern}`, but no note wrote a tag \
                 under that prefix"
            ));
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
        for (d, tag, id) in members {
            let (source_id, source_label) = source_of(d, tag);
            groups
                .entry((spec.edge.clone(), source_label, spec.label.clone()))
                .or_default()
                .push((source_id, id, Vec::new()));
        }
    }
    Ok(groups)
}

/// The end of a typed tag's edge: the derived node it was written in, or the
/// note. A suffix naming no derived node cannot happen — the parse produced
/// both — so an unknown one falls back to the note rather than inventing a
/// label no frame carries.
fn source_of(d: &ConceptDoc, tag: &crate::okf::tags::TypedTag) -> (String, String) {
    let node = tag
        .source
        .as_ref()
        .and_then(|suffix| d.derived.nodes.iter().find(|n| n.suffix == *suffix));
    match node {
        Some(node) => (
            format!("{}{}", d.concept_id, node.suffix),
            node.label.clone(),
        ),
        None => (d.concept_id.clone(), d.label.clone()),
    }
}

/// The entries a concept joins a hub by: the string elements of its `key`
/// frontmatter **list**, in order. A scalar is not a list and joins nothing —
/// VAULT.md §7 defines a hub over a list-valued key, and §9 classes a scalar
/// `tags:` as a reserved-key error rather than a one-entry list.
///
/// The `tags` key additionally takes the inline `#tag`s the vault profile
/// found in the body (VAULT.md §5.5): the inline syntax names that hub and no
/// other, and the `tags` *property* still reports only the frontmatter.
fn hub_values<'a>(d: &'a ConceptDoc, key: &str, profile: &Profile) -> Vec<&'a str> {
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
        // A tag a `tag_labels:` rule models is modelled **only** that way
        // (VAULT.md §5.5): it leaves this hub whichever of the two forms
        // wrote it, and the note's `tags` property still reports it.
        if !profile.tag_labels.is_empty() {
            vals.retain(|name| {
                crate::okf::model::tag_label_rule(&profile.tag_labels, name).is_none()
            });
        }
    }
    vals
}

#[cfg(test)]
#[path = "hubs_tests.rs"]
mod hubs_tests;
