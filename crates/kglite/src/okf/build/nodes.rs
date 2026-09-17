//! The concept nodes themselves: one columnar frame per label, and the
//! declared types (VAULT.md §7) the columns are built to.

use super::{column_value, count_nodes};
use crate::datatypes::values::{DataFrame, Value};
use crate::graph::mutation::maintain;
use crate::graph::DirGraph;
use crate::okf::model::{BuildOptions, BuildReport, ConceptDoc};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// One `add_nodes` call per label; columns = id/title/file_path (+ body) plus the
/// union of frontmatter keys across that label's concepts (missing → Null).
/// `declared_types` is `.kglite/vault.yaml`'s `types:` (VAULT.md §7), applied
/// **here** rather than to the finished graph: a declared type decides what a
/// column *is*, and `DataFrame::from_cypher_rows` infers that from the values
/// it is handed. Retyping afterwards would be a second, weaker implementation
/// of the same rule, against a store that has already chosen.
pub(super) fn build_nodes(
    graph: &mut DirGraph,
    docs: &[ConceptDoc],
    opts: &BuildOptions,
    declared_types: Option<&BTreeMap<String, BTreeMap<String, String>>>,
    report: &mut BuildReport,
) -> Result<(), String> {
    let mut unmatched: BTreeSet<(&str, &str)> = declared_types
        .into_iter()
        .flatten()
        .flat_map(|(label, props)| props.keys().map(move |p| (label.as_str(), p.as_str())))
        .collect();
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

        let body_column = opts.profile.body_property.as_str();
        let mut columns = vec![
            "concept_id".to_string(),
            "title".to_string(),
            "file_path".to_string(),
        ];
        if opts.with_body {
            columns.push(body_column.to_string());
        }
        columns.extend(keys.iter().map(|k| k.to_string()));

        // The declarations for this label, minus `concept_id`: the id column
        // is the node's identity and the index built on it, and retyping it
        // would silently move every link's target.
        let declared: BTreeMap<&str, &str> = declared_types
            .and_then(|t| t.get(label))
            .into_iter()
            .flatten()
            .filter(|(property, _)| property.as_str() != "concept_id")
            .map(|(property, keyword)| (property.as_str(), keyword.as_str()))
            .collect();
        // The id column is the node's identity and the index built on it;
        // retyping it would silently move every link's target. Reported once,
        // here, rather than also falling out as "no note carries it".
        if unmatched.remove(&(label, "concept_id")) {
            report.warnings.push(format!(
                "`vault.yaml` declares `types.{label}.concept_id`; the id column is not \
                 retyped"
            ));
        }
        for property in declared.keys() {
            if columns.iter().any(|c| c == property) {
                unmatched.remove(&(label, *property));
            }
        }

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
            if !declared.is_empty() {
                apply_declared_types(&mut row, &columns, &declared, label, d, report);
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
    for (label, property) in unmatched {
        report.warnings.push(format!(
            "`vault.yaml` declares `types.{label}.{property}`, but no note carries that \
             label and property"
        ));
    }
    Ok(())
}

/// Coerce one note's row to the label's declared types (VAULT.md §7).
///
/// A value that will not coerce keeps the type it had and is **warned about**,
/// rather than being nulled: the declaration is the author's statement about
/// the vault, and a note that disagrees with it still holds the value a human
/// wrote. Mixed types in one column then settle by inference, which is the
/// same outcome as not having declared anything — visibly so, because the
/// warning names the note.
fn apply_declared_types(
    row: &mut [Value],
    columns: &[String],
    declared: &BTreeMap<&str, &str>,
    label: &str,
    doc: &ConceptDoc,
    report: &mut BuildReport,
) {
    for (index, column) in columns.iter().enumerate() {
        let Some(keyword) = declared.get(column.as_str()) else {
            continue;
        };
        match crate::okf::vault_config::coerce(&row[index], keyword) {
            Some(coerced) => row[index] = coerced,
            None => report.warnings.push(format!(
                "`{}`: {label}.{column} is declared `{keyword}` but holds {} — left as written",
                doc.file_path,
                crate::datatypes::values::raw_string(&row[index])
            )),
        }
    }
}

#[cfg(test)]
#[path = "nodes_tests.rs"]
mod nodes_tests;
