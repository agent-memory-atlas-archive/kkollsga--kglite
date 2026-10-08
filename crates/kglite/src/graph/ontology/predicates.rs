//! Pure per-entity ontology predicates: each judges ONE node or ONE
//! relationship against the declared store and returns the verdict, with no
//! scan, no executor plumbing and no side effects.
//!
//! Two drivers share them so a defect cannot live in only one: the
//! whole-graph audit (`ontology_audit()`, `node_property_violation()`,
//! `edge_property_violation()`, the blueprint gate) and the write-time
//! gates. The failure mode this prevents is disagreement — an audit that
//! reports a graph clean while the first write is refused, or the reverse.
//!
//! Vocabulary, fixed by the audit and therefore by every caller:
//! - **Labels** are judged on the node's *primary* type only. Secondary
//!   labels, including engine-written materialised (managed) ones, never
//!   enroll a node and are never refused.
//! - **Types** use the permissive
//!   [`value_matches_type`](crate::graph::mutation::validation::value_matches_type)
//!   (`float` admits integers, unknown names pass), not the strict
//!   `CREATE CONSTRAINT … IS ::` set.
//! - **Domain/range** compare the endpoint's primary type; a declaration
//!   naming a class widens to that class plus its declared descendants.
//! - A required property is *absent or null*; a type violation needs a
//!   *present, non-null* value (absence is `required_properties`' concern).

use std::collections::BTreeSet;

use crate::datatypes::values::Value;
use crate::graph::mutation::validation::value_matches_type;
use crate::graph::ontology::{ClassDecl, OntologyStore, RelationshipDecl};
use crate::graph::schema::{DirGraph, InternedKey};
use crate::graph::storage::NodeView;

/// `class_or_type` plus every declared class whose ancestor chain contains
/// it — the accepted endpoint set a supertype declaration widens to.
pub fn accepted_types(store: &OntologyStore, class_or_type: &str) -> Vec<String> {
    let mut out = vec![class_or_type.to_string()];
    for name in store.classes.keys() {
        if store
            .ancestors(name)
            .iter()
            .any(|ancestor| ancestor == class_or_type)
        {
            out.push(name.clone());
        }
    }
    out
}

/// Whether a node of primary type `actual` satisfies a declaration naming
/// `declared` — `declared` itself or any declared descendant of it.
/// Equivalent to `accepted_types(store, declared).contains(actual)` without
/// building the set.
pub fn endpoint_accepted(store: &OntologyStore, declared: &str, actual: &str) -> bool {
    actual == declared
        || store
            .ancestors(actual)
            .iter()
            .any(|ancestor| ancestor == declared)
}

/// The closed-label rule for one node. `true` when the store does not close
/// labels or `primary_type` is a declared class (abstract included — an
/// abstract primary type is refused by its own rule, not this one).
pub fn label_allowed(store: &OntologyStore, primary_type: &str) -> bool {
    !store.closed_labels || store.classes.contains_key(primary_type)
}

/// A repeated declaration still names one property. Applied at read time
/// too, because persisted or directly constructed declarations can repeat.
pub fn unique_required_properties(properties: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    properties
        .iter()
        .filter(|p| seen.insert(*p))
        .cloned()
        .collect()
}

/// The properties a class declares for one of [`NODE_CHECK_NAMES`].
pub fn declared_node_properties(decl: &ClassDecl, check: &str) -> Vec<String> {
    if check == "required_properties" {
        unique_required_properties(&decl.required_properties)
    } else {
        decl.property_types.keys().cloned().collect()
    }
}

/// The subset of `properties` that `view` fails under `check`
/// (`required_properties` or `property_types`) against `decl`. Loader
/// aliases resolve against the node's actual `primary_type`, not the
/// (possibly abstract) class that declares the contract.
pub fn node_property_failures(
    graph: &DirGraph,
    view: &NodeView<'_>,
    primary_type: &str,
    decl: &ClassDecl,
    check: &str,
    properties: &[String],
) -> Vec<String> {
    properties
        .iter()
        .filter(|property| {
            let field = graph.resolve_alias(primary_type, property);
            let value = view.resolved_field(primary_type, field, InternedKey::from_str(field));
            property_fails(decl, check, property, value.as_deref())
        })
        .cloned()
        .collect()
}

/// Whether one property's `value` (`None` = absent) fails `check` against
/// `decl`: a required property is absent or null; a type violation needs a
/// present, non-null value of the wrong type. The leaf both the stored-node
/// predicate and the pre-write row gates evaluate, so they cannot disagree.
pub fn property_fails(
    decl: &ClassDecl,
    check: &str,
    property: &str,
    value: Option<&Value>,
) -> bool {
    let present = value.filter(|v| !matches!(v, Value::Null));
    if check == "required_properties" {
        present.is_none()
    } else {
        present.is_some_and(|v| !value_matches_type(v, &decl.property_types[property]))
    }
}

/// The subset of the declaration's properties one relationship fails under
/// `check` (`required_properties` or `property_types`). `get` reads a
/// property off the relationship.
pub fn edge_property_failures<'v>(
    decl: &RelationshipDecl,
    check: &str,
    get: impl Fn(&str) -> Option<&'v Value>,
) -> Vec<String> {
    if check == "required_properties" {
        unique_required_properties(&decl.required_properties)
            .into_iter()
            .filter(|p| matches!(get(p), None | Some(Value::Null)))
            .collect()
    } else {
        decl.property_types
            .iter()
            .filter(|(p, ty)| {
                get(p).is_some_and(|v| !matches!(v, Value::Null) && !value_matches_type(v, ty))
            })
            .map(|(p, _)| p.clone())
            .collect()
    }
}

/// Whether the declaration's `exempt[check]` classes (and their declared
/// descendants) cover an edge whose source has primary type `source_type`.
/// An exempt violation is counted separately by the audit and excused by a
/// write gate.
pub fn edge_exempt(
    store: &OntologyStore,
    decl: &RelationshipDecl,
    check: &str,
    source_type: &str,
) -> bool {
    decl.exempt_classes(check)
        .iter()
        .any(|class| endpoint_accepted(store, class, source_type))
}

/// `true` when the declaration names a domain and `source_type` is outside it.
pub fn domain_violated(store: &OntologyStore, decl: &RelationshipDecl, source_type: &str) -> bool {
    decl.domain
        .as_deref()
        .is_some_and(|d| !endpoint_accepted(store, d, source_type))
}

/// `true` when the declaration names a range and `target_type` is outside it.
pub fn range_violated(store: &OntologyStore, decl: &RelationshipDecl, target_type: &str) -> bool {
    decl.range
        .as_deref()
        .is_some_and(|r| !endpoint_accepted(store, r, target_type))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::ontology::ontology_from_json;

    fn store() -> OntologyStore {
        ontology_from_json(
            r#"{"closed_labels": true,
                "classes": {"Licensable": {"abstract": true},
                            "Contract": {"is_a": "Licensable"},
                            "Permit": {"is_a": "Licensable"},
                            "Company": {}},
                "relationships": {"MANAGED_BY": {
                    "domain": "Licensable", "range": "Company",
                    "required_properties": ["validFrom", "validFrom"],
                    "property_types": {"rank": "float"},
                    "exempt": {"required_properties": ["Permit"]}}}}"#,
        )
        .unwrap()
    }

    #[test]
    fn accepted_types_widens_to_declared_descendants() {
        let s = store();
        let mut got = accepted_types(&s, "Licensable");
        got.sort();
        assert_eq!(got, ["Contract", "Licensable", "Permit"]);
        assert_eq!(accepted_types(&s, "Company"), ["Company"]);
        assert_eq!(accepted_types(&s, "Unknown"), ["Unknown"]);
    }

    #[test]
    fn endpoint_accepted_agrees_with_accepted_types() {
        let s = store();
        for declared in ["Licensable", "Contract", "Company", "Unknown"] {
            for actual in [
                "Licensable",
                "Contract",
                "Permit",
                "Company",
                "Unknown",
                "Other",
            ] {
                assert_eq!(
                    endpoint_accepted(&s, declared, actual),
                    accepted_types(&s, declared).iter().any(|t| t == actual),
                    "{declared} vs {actual}"
                );
            }
        }
    }

    #[test]
    fn label_allowed_is_primary_label_membership_when_closed() {
        let s = store();
        assert!(label_allowed(&s, "Contract"));
        assert!(label_allowed(&s, "Licensable"));
        assert!(!label_allowed(&s, "Ghost"));
        let open = OntologyStore {
            closed_labels: false,
            ..s
        };
        assert!(label_allowed(&open, "Ghost"));
    }

    #[test]
    fn domain_and_range_use_abstract_widening() {
        let s = store();
        let decl = &s.relationships["MANAGED_BY"];
        assert!(!domain_violated(&s, decl, "Contract"));
        assert!(domain_violated(&s, decl, "Company"));
        assert!(!range_violated(&s, decl, "Company"));
        assert!(range_violated(&s, decl, "Contract"));
        let undeclared = RelationshipDecl::default();
        assert!(!domain_violated(&s, &undeclared, "Anything"));
        assert!(!range_violated(&s, &undeclared, "Anything"));
    }

    #[test]
    fn edge_property_failures_follow_audit_vocabulary() {
        let s = store();
        let decl = &s.relationships["MANAGED_BY"];
        let none = |_: &str| None;
        assert_eq!(
            edge_property_failures(decl, "required_properties", none),
            ["validFrom"]
        );
        let null = Value::Null;
        let nulled = |_: &str| Some(&null);
        assert_eq!(
            edge_property_failures(decl, "required_properties", nulled),
            ["validFrom"]
        );
        // A type violation needs a present non-null value.
        assert!(edge_property_failures(decl, "property_types", none).is_empty());
        assert!(edge_property_failures(decl, "property_types", nulled).is_empty());
        let int = Value::Int64(3);
        assert!(edge_property_failures(decl, "property_types", |_| Some(&int)).is_empty());
        let text = Value::String("x".into());
        assert_eq!(
            edge_property_failures(decl, "property_types", |_| Some(&text)),
            ["rank"]
        );
    }

    #[test]
    fn edge_exempt_covers_listed_class_and_descendants_for_its_check_only() {
        let s = store();
        let decl = &s.relationships["MANAGED_BY"];
        assert!(edge_exempt(&s, decl, "required_properties", "Permit"));
        assert!(!edge_exempt(&s, decl, "required_properties", "Contract"));
        assert!(!edge_exempt(&s, decl, "property_types", "Permit"));
    }
}
