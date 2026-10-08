//! Declaration-time verification: judge the data already stored against an
//! ontology that has just been installed, at each rule's declared severity.
//!
//! This reuses the audit (`audit_counts_by_property`, which runs the same
//! per-entity predicates the write gates use) and the closed-label
//! predicate, then filters by severity — there is no third implementation of
//! any rule. Only the rules a write gate enforces can refuse a declaration
//! (required/typed properties, closed labels, domain, range); the remaining
//! audit-only checks (cardinality, inverse, …) are reported as warnings.

use std::collections::BTreeMap;

use super::predicates::label_allowed;
use super::violation::{
    DefineOntologyError, OntologyDeclarationRefused, OntologyReportEntry, OntologyRule,
};
use super::Enforcement;
use crate::graph::constraints::EntityKind;
use crate::graph::languages::cypher::executor::ontology_procedures::audit_counts_by_property;
use crate::graph::schema::DirGraph;

fn write_rule(check: &str) -> Option<OntologyRule> {
    match check {
        "required_properties" => Some(OntologyRule::RequiredProperty),
        "property_types" => Some(OntologyRule::PropertyType),
        "domain" => Some(OntologyRule::Domain),
        "range" => Some(OntologyRule::Range),
        _ => None,
    }
}

/// Whether any rule of `graph.ontology` carries a non-advisory severity —
/// the gate that keeps advisory declarations free of any scan.
fn any_enforced(graph: &DirGraph) -> bool {
    let store = &graph.ontology;
    let on = |e: Enforcement| e != Enforcement::Advisory;
    (store.closed_labels && on(store.enforcement))
        || store
            .classes
            .values()
            .any(|c| on(c.enforcement) || c.enforcement_overrides.values().any(|e| on(*e)))
        || store
            .relationships
            .values()
            .any(|r| on(r.enforcement) || r.enforcement_overrides.values().any(|e| on(*e)))
}

/// Verify stored data against the installed `graph.ontology`. `Ok` carries
/// the `warn`-level (and audit-only) findings as report lines; `Err` is a
/// refusal naming every `error`-level rule the data breaks.
pub(crate) fn verify_declaration(graph: &DirGraph) -> Result<Vec<String>, DefineOntologyError> {
    if !any_enforced(graph) {
        return Ok(Vec::new());
    }
    let mut refused: Vec<OntologyReportEntry> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    if !graph.ontology.is_empty() {
        for line in audit_counts_by_property(graph)? {
            if line.rule == "closed_labels" || (line.violations == 0 && line.exempted == 0) {
                continue;
            }
            let Some((entity_type, check)) = line.rule.rsplit_once('.') else {
                continue;
            };
            let tail = if line.exempted > 0 {
                format!(" (+{} exempted)", line.exempted)
            } else {
                String::new()
            };
            let summary = format!(
                "{} {}{}: {}/{} ({:.1}%) violations{tail}",
                line.entity_kind,
                line.rule,
                line.property
                    .as_deref()
                    .map(|p| format!(" [{p}]"))
                    .unwrap_or_default(),
                line.violations,
                line.total,
                line.pct
            );
            if line.severity == Enforcement::Advisory {
                continue;
            }
            if line.violations == 0 {
                // Everything flagged was exempt: still surfaced, so an
                // exemption cannot silently absorb a whole rule.
                warnings.push(format!("ontology: {summary}"));
                continue;
            }
            match (line.severity, write_rule(check)) {
                (Enforcement::Error, Some(rule)) => {
                    refused.push(OntologyReportEntry {
                        rule,
                        entity: if line.entity_kind == "edge" {
                            EntityKind::Relationship
                        } else {
                            EntityKind::Node
                        },
                        entity_type: entity_type.to_string(),
                        property: line.property.clone(),
                        count: line.violations as u64,
                    });
                    lines.push(summary);
                }
                _ => warnings.push(format!("ontology: {summary}")),
            }
        }
    }

    let store = &graph.ontology;
    if store.closed_labels && store.enforcement != Enforcement::Advisory {
        let mut offending = BTreeMap::<String, u64>::new();
        for (primary_type, nodes) in graph.type_indices.iter() {
            if !nodes.is_empty() && !label_allowed(store, primary_type) {
                offending.insert(primary_type.to_string(), nodes.len() as u64);
            }
        }
        for (entity_type, count) in offending {
            let summary = format!("node closed_labels on '{entity_type}': {count} node(s)");
            if store.enforcement == Enforcement::Error {
                refused.push(OntologyReportEntry {
                    rule: OntologyRule::ClosedLabels,
                    entity: EntityKind::Node,
                    entity_type,
                    property: None,
                    count,
                });
                lines.push(summary);
            } else {
                warnings.push(format!("ontology: {summary}"));
            }
        }
    }

    if refused.is_empty() {
        return Ok(warnings);
    }
    refused.sort_by(|a, b| {
        (a.entity as u8, &a.entity_type, &a.property).cmp(&(
            b.entity as u8,
            &b.entity_type,
            &b.property,
        ))
    });
    Err(DefineOntologyError::Refused(OntologyDeclarationRefused {
        message: format!(
            "ontology declaration refused — stored data already violates {} error-level \
             rule(s):\n  {}\nFix the data, or declare the rule at 'warn' or 'advisory'; \
             nothing was changed.",
            lines.len(),
            lines.join("\n  ")
        ),
        entries: refused,
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::graph::ontology::ontology_from_json;
    use crate::graph::ontology::violation::{DefineOntologyError, OntologyRule};
    use crate::graph::schema::DirGraph;
    use crate::graph::session::execute::{execute_mut, ExecuteOptions};

    fn run(graph: &mut DirGraph, query: &str) {
        let params = HashMap::new();
        let opts = ExecuteOptions::eager(&params);
        execute_mut(graph, query, &opts)
            .unwrap_or_else(|e| panic!("setup query failed: {query}: {e}"));
    }

    /// Three `Doc`s (one with an `owner`), two `Person`s and one `AUTHORED`
    /// edge (Doc -> Person) carrying no `since`.
    fn data() -> DirGraph {
        let mut g = DirGraph::new();
        run(
            &mut g,
            "CREATE (:Doc {id: 1, title: 'a', owner: 'x'}), (:Doc {id: 2, title: 'b'}), \
             (:Doc {id: 3, title: 'c'}), (:Person {id: 10, title: 'p'}), (:Person {id: 11, title: 'q'})",
        );
        run(
            &mut g,
            "MATCH (d:Doc {id: 1}), (p:Person {id: 10}) CREATE (d)-[:AUTHORED]->(p)",
        );
        g
    }

    fn store(json: &str) -> crate::graph::ontology::OntologyStore {
        ontology_from_json(json).unwrap()
    }

    fn refused(
        r: Result<Vec<String>, DefineOntologyError>,
    ) -> crate::graph::ontology::violation::OntologyDeclarationRefused {
        match r {
            Err(DefineOntologyError::Refused(r)) => r,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn error_level_violations_refuse_with_a_report_and_keep_the_previous_store() {
        let mut g = data();
        let before = store(r#"{"classes": {"Doc": {}, "Person": {}}}"#);
        g.define_ontology(before.clone()).unwrap();
        let r = refused(g.define_ontology(store(
            r#"{"classes": {"Doc": {"required_properties": ["owner"],
                                    "property_types": {"title": "integer"}},
                            "Person": {}},
                "enforcement": "error"}"#,
        )));
        let got: Vec<_> = r
            .entries
            .iter()
            .map(|e| {
                (
                    e.rule,
                    e.entity_type.as_str(),
                    e.property.as_deref(),
                    e.count,
                )
            })
            .collect();
        assert!(
            got.contains(&(OntologyRule::RequiredProperty, "Doc", Some("owner"), 2)),
            "{got:?}"
        );
        assert!(
            got.contains(&(OntologyRule::PropertyType, "Doc", Some("title"), 3)),
            "{got:?}"
        );
        assert!(r.message.contains("refused"), "{}", r.message);
        assert_eq!(*g.ontology, before, "a refusal must change nothing");
    }

    #[test]
    fn warn_installs_and_reports() {
        let mut g = data();
        let s = store(
            r#"{"classes": {"Doc": {"required_properties": ["owner"]}, "Person": {}},
                "enforcement": "warn"}"#,
        );
        let warnings = g.define_ontology(s.clone()).unwrap();
        assert_eq!(*g.ontology, s);
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("Doc.required_properties") && w.contains("2/3")),
            "{warnings:?}"
        );
    }

    #[test]
    fn advisory_installs_silently() {
        let mut g = data();
        let warnings = g
            .define_ontology(store(
                r#"{"classes": {"Doc": {"required_properties": ["owner"]}, "Person": {}}}"#,
            ))
            .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn per_rule_severity_mixes_error_and_warn() {
        let mut g = data();
        // The error-level rule passes (every Doc has a title); the violated
        // one is only warn-level, so the declaration installs.
        let warnings = g
            .define_ontology(store(
                r#"{"classes": {"Doc": {"required_properties": ["title", "owner"],
                                        "enforcement": {"required_properties": "warn"}},
                                "Person": {}}}"#,
            ))
            .unwrap();
        assert!(warnings
            .iter()
            .any(|w| w.contains("Doc.required_properties")));
    }

    #[test]
    fn exempt_excuses_a_refusal() {
        let mut g = data();
        let rels = |exempt: &str| {
            format!(
                r#"{{"classes": {{"Doc": {{}}, "Person": {{}}}},
                    "relationships": {{"AUTHORED": {{"required_properties": ["since"],
                                                    "enforcement": "error"{exempt}}}}}}}"#
            )
        };
        let r = refused(g.define_ontology(store(&rels(""))));
        assert_eq!(r.entries[0].rule, OntologyRule::RequiredProperty);
        assert_eq!(r.entries[0].entity_type, "AUTHORED");
        assert_eq!(r.entries[0].count, 1);
        let warnings = g
            .define_ontology(store(&rels(
                r#", "exempt": {"required_properties": ["Doc"]}"#,
            )))
            .unwrap();
        assert!(
            warnings.iter().any(|w| w.contains("exempted")),
            "{warnings:?}"
        );
    }

    #[test]
    fn domain_range_and_closed_labels_refuse() {
        let mut g = data();
        let r = refused(g.define_ontology(store(
            r#"{"classes": {"Doc": {}, "Person": {}},
                "relationships": {"AUTHORED": {"domain": "Person", "range": "Doc"}},
                "enforcement": "error"}"#,
        )));
        let rules: Vec<_> = r.entries.iter().map(|e| e.rule).collect();
        assert!(rules.contains(&OntologyRule::Domain), "{rules:?}");
        assert!(rules.contains(&OntologyRule::Range), "{rules:?}");

        let r = refused(g.define_ontology(store(
            r#"{"classes": {"Doc": {}}, "closed_labels": true, "enforcement": "error"}"#,
        )));
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].rule, OntologyRule::ClosedLabels);
        assert_eq!(
            (r.entries[0].entity_type.as_str(), r.entries[0].count),
            ("Person", 2)
        );
        assert!(g.ontology.is_empty());
    }

    #[test]
    fn wal_replay_does_not_reverify() {
        use crate::graph::mutation::wal_replay::apply_frames;
        use crate::graph::wal::{MutationOp, WalFrame};

        let mut g = data();
        // A declaration that live verification would refuse.
        let violating = store(
            r#"{"classes": {"Doc": {"required_properties": ["owner"]}, "Person": {}},
                "enforcement": "error"}"#,
        );
        assert!(g.define_ontology(violating.clone()).is_err());
        let frames = vec![WalFrame {
            lsn: 1,
            ops: vec![MutationOp::SetOntology {
                document: serde_json::to_string(&violating).unwrap(),
            }],
        }];
        apply_frames(&mut g, &frames, 0).unwrap();
        assert_eq!(*g.ontology, violating);
    }
}
