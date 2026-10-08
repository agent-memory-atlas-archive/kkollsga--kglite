//! Class property findings shared by audit, drill-down and blueprint gating.

use std::collections::{BTreeMap, HashMap};

use petgraph::graph::NodeIndex;

use super::ontology_procedures::{yield_alias, AuditBreakdown, AuditLine};
use crate::datatypes::values::Value;
use crate::graph::languages::cypher::ast::YieldItem;
use crate::graph::languages::cypher::result::ResultRow;
use crate::graph::ontology::predicates::{
    accepted_types, declared_node_properties as declared_properties, label_allowed,
    node_property_failures,
};
use crate::graph::ontology::{ClassDecl, NODE_CHECK_NAMES};
use crate::graph::schema::DirGraph;
use crate::graph::storage::GraphRead;

struct Finding {
    node: NodeIndex,
    primary_type: String,
    properties: Vec<String>,
}

/// Primary membership plus declared descendants; arbitrary secondary labels
/// do not enroll a node. Both consumers use these exact per-node findings.
fn findings(
    graph: &DirGraph,
    class: &str,
    decl: &ClassDecl,
    check: &str,
    properties: &[String],
) -> (usize, Vec<Finding>) {
    let mut total = 0;
    let mut out = Vec::new();
    for primary_type in accepted_types(&graph.ontology, class) {
        let Some(nodes) = graph.type_indices.get(&primary_type) else {
            continue;
        };
        for node in nodes.iter() {
            let Some(view) = graph.graph.node_view(node) else {
                continue;
            };
            total += 1;
            let failed =
                node_property_failures(graph, &view, &primary_type, decl, check, properties);
            if !failed.is_empty() {
                out.push(Finding {
                    node,
                    primary_type: primary_type.clone(),
                    properties: failed,
                });
            }
        }
    }
    (total, out)
}

pub(super) fn audit_lines(graph: &DirGraph, breakdown: AuditBreakdown) -> Vec<AuditLine> {
    let mut out = Vec::new();
    for (class, decl) in &graph.ontology.classes {
        for &check in NODE_CHECK_NAMES {
            let properties = declared_properties(decl, check);
            if properties.is_empty() {
                continue;
            }
            let (total, findings) = findings(graph, class, decl, check, &properties);
            let line = |domain_class, property, violations| AuditLine {
                entity_kind: "node",
                rule: format!("{class}.{check}"),
                domain_class,
                property,
                severity: decl.enforcement_for(check),
                violations,
                exempted: 0,
                total,
                pct: if total == 0 {
                    0.0
                } else {
                    ((violations as f64 / total as f64 * 100.0) * 10.0).round() / 10.0
                },
            };
            match breakdown {
                AuditBreakdown::None => out.push(line(None, None, findings.len())),
                AuditBreakdown::DomainClass => {
                    let mut counts = BTreeMap::<String, usize>::new();
                    for finding in &findings {
                        *counts.entry(finding.primary_type.clone()).or_default() += 1;
                    }
                    if counts.is_empty() {
                        out.push(line(None, None, 0));
                    } else {
                        out.extend(
                            counts
                                .into_iter()
                                .map(|(name, n)| line(Some(name), None, n)),
                        );
                    }
                }
                AuditBreakdown::Property => {
                    let mut counts: BTreeMap<_, usize> =
                        properties.into_iter().map(|p| (p, 0)).collect();
                    for finding in &findings {
                        for property in &finding.properties {
                            *counts
                                .get_mut(property)
                                .expect("a finding names a declared property") += 1;
                        }
                    }
                    out.extend(
                        counts
                            .into_iter()
                            .map(|(name, n)| line(None, Some(name), n)),
                    );
                }
            }
        }
    }
    if graph.ontology.closed_labels {
        out.extend(label_lines(graph, breakdown));
    }
    out
}

/// The allowed-labels rule as scorecard lines: `total` is every node, a
/// violation is a node whose primary type [`label_allowed`] refuses.
/// Judged per primary type (the predicate's input), so it is O(types).
/// `DomainClass` fans out over the offending types; `Property` has nothing
/// to fan over and keeps the aggregate line.
fn label_lines(graph: &DirGraph, breakdown: AuditBreakdown) -> Vec<AuditLine> {
    let store = &graph.ontology;
    let mut total = 0usize;
    let mut offending = BTreeMap::<String, usize>::new();
    for (primary_type, nodes) in graph.type_indices.iter() {
        let count = nodes.len();
        total += count;
        if count > 0 && !label_allowed(store, primary_type) {
            offending.insert(primary_type.to_string(), count);
        }
    }
    let violations: usize = offending.values().sum();
    let line = |domain_class, violations| AuditLine {
        entity_kind: "node",
        rule: "closed_labels".to_string(),
        domain_class,
        property: None,
        severity: store.enforcement,
        violations,
        exempted: 0,
        total,
        pct: if total == 0 {
            0.0
        } else {
            ((violations as f64 / total as f64 * 100.0) * 10.0).round() / 10.0
        },
    };
    if breakdown == AuditBreakdown::DomainClass && !offending.is_empty() {
        return offending
            .into_iter()
            .map(|(name, n)| line(Some(name), n))
            .collect();
    }
    vec![line(None, violations)]
}

pub(super) fn execute_node_property_violation(
    graph: &DirGraph,
    params: &HashMap<String, Value>,
    yield_items: &[YieldItem],
) -> Result<Vec<ResultRow>, String> {
    if !params.is_empty() {
        return Err(
            "CALL node_property_violation takes no parameters — it reads class declarations."
                .to_string(),
        );
    }
    if graph.ontology.is_empty() {
        return Err(
            "node_property_violation: no ontology declared — define one with define_ontology()"
                .to_string(),
        );
    }
    let mut out = Vec::new();
    for (class, decl) in &graph.ontology.classes {
        for &check in NODE_CHECK_NAMES {
            let properties = declared_properties(decl, check);
            if properties.is_empty() {
                continue;
            }
            for finding in findings(graph, class, decl, check, &properties).1 {
                let mut row = ResultRow::new();
                if let Some(alias) = yield_alias(yield_items, "node") {
                    row.node_bindings.insert(alias, finding.node);
                }
                let cells = [
                    ("class", Value::String(class.clone())),
                    ("check", Value::String(check.to_string())),
                    ("property", Value::String(finding.properties[0].clone())),
                    (
                        "properties",
                        Value::List(finding.properties.into_iter().map(Value::String).collect()),
                    ),
                ];
                for (name, value) in cells {
                    if let Some(alias) = yield_alias(yield_items, name) {
                        row.projected.insert(alias, value);
                    }
                }
                out.push(row);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod node_ontology_tests {
    use super::*;
    use crate::datatypes::DataFrame;
    use crate::graph::ontology::ontology_from_json;

    fn add(g: &mut DirGraph, node_type: &str, n: i64) {
        let rows = (1..=n)
            .map(|i| vec![Value::Int64(i), Value::String(format!("t{i}"))])
            .collect();
        let df =
            DataFrame::from_cypher_rows(vec!["id".to_string(), "title".to_string()], rows).unwrap();
        crate::graph::mutation::maintain::add_nodes(
            g,
            df,
            node_type.to_string(),
            "id".to_string(),
            Some("title".to_string()),
            None,
        )
        .unwrap();
    }

    fn graph_with(ontology: &str) -> DirGraph {
        let mut g = DirGraph::new();
        add(&mut g, "Doc", 3);
        add(&mut g, "Stray", 2);
        // Audit tests need data that already breaks an `error` rule, which a
        // verified declaration would refuse.
        g.define_ontology_unverified(ontology_from_json(ontology).unwrap())
            .unwrap();
        g
    }

    fn label_line(g: &DirGraph, breakdown: AuditBreakdown) -> Vec<AuditLine> {
        audit_lines(g, breakdown)
            .into_iter()
            .filter(|l| l.rule == "closed_labels")
            .collect()
    }

    #[test]
    fn closed_labels_counts_undeclared_primary_types() {
        let g = graph_with(
            r#"{"classes": {"Doc": {}}, "closed_labels": true, "enforcement": "error"}"#,
        );
        let lines = label_line(&g, AuditBreakdown::None);
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!((line.violations, line.total), (2, 5));
        assert_eq!(line.entity_kind, "node");
        assert_eq!(line.severity, crate::graph::ontology::Enforcement::Error);
        assert_eq!(line.pct, 40.0);

        let by_class = label_line(&g, AuditBreakdown::DomainClass);
        assert_eq!(by_class.len(), 1);
        assert_eq!(by_class[0].domain_class.as_deref(), Some("Stray"));
        assert_eq!(by_class[0].violations, 2);
    }

    #[test]
    fn open_labels_add_no_audit_line() {
        let g = graph_with(r#"{"classes": {"Doc": {}}}"#);
        assert!(label_line(&g, AuditBreakdown::None).is_empty());
    }

    #[test]
    fn declaring_every_live_type_clears_the_label_rule() {
        let g = graph_with(r#"{"classes": {"Doc": {}, "Stray": {}}, "closed_labels": true}"#);
        let lines = label_line(&g, AuditBreakdown::DomainClass);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            (lines[0].violations, lines[0].domain_class.clone()),
            (0, None)
        );
    }

    #[test]
    fn node_property_failures_judge_one_node() {
        let g = graph_with(
            r#"{"classes": {"Doc": {"required_properties": ["owner"],
                                    "property_types": {"title": "integer"}}}}"#,
        );
        let decl = &g.ontology.classes["Doc"];
        let idx = g.type_indices.get("Doc").unwrap().iter().next().unwrap();
        let view = g.graph.node_view(idx).unwrap();
        let required = node_property_failures(
            &g,
            &view,
            "Doc",
            decl,
            "required_properties",
            &declared_properties(decl, "required_properties"),
        );
        assert_eq!(required, vec!["owner".to_string()]);
        let typed = node_property_failures(
            &g,
            &view,
            "Doc",
            decl,
            "property_types",
            &declared_properties(decl, "property_types"),
        );
        assert_eq!(typed, vec!["title".to_string()]);
    }
}
