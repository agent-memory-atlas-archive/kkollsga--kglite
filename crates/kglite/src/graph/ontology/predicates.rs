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
use crate::graph::schema::{soft_alias_fallback, DirGraph, InternedKey, SoftAliasFallback};
use crate::graph::storage::NodeView;
use petgraph::graph::NodeIndex;

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
    match check {
        "required_properties" => unique_required_properties(&decl.required_properties),
        "property_values" => decl.property_values.keys().cloned().collect(),
        _ => decl.property_types.keys().cloned().collect(),
    }
}

/// Whether `title` is the value the engine mints for a node whose creator
/// supplied none: `<Label>_<id>`, or the id itself on a type with no declared
/// title field (what `add_nodes` without a title column and an untitled
/// `CREATE` on such a type both write). A stored node carries no marker
/// beyond the value, so this is the only evidence the audit has; a title a
/// caller deliberately supplies that is exactly `<Label>_<id>` (or, on an
/// undeclared type, equal to its id) is indistinguishable from a minted one.
pub fn title_is_synthesised(
    graph: &DirGraph,
    primary_type: &str,
    id: &Value,
    title: &Value,
) -> bool {
    if let Value::String(t) = title {
        let minted = match id {
            Value::UniqueId(n) => format!("{primary_type}_{n}"),
            Value::Int64(n) => format!("{primary_type}_{n}"),
            other => format!(
                "{primary_type}_{}",
                crate::datatypes::values::raw_string(other)
            ),
        };
        if *t == minted {
            return true;
        }
    }
    !graph.title_field_aliases.contains_key(primary_type)
        && crate::graph::core::filtering::values_equal(id, title)
}

/// The value ontology rules judge for `property` on a stored node: the same
/// resolution a filter applies, except that a title the engine minted never
/// counts as a supplied value. `name` falls back to the title only for reads;
/// a required `name` is satisfied by a stored `name` property or a supplied
/// title under the type's declared title field, never by a default.
pub fn stored_property_value(
    graph: &DirGraph,
    view: &NodeView<'_>,
    primary_type: &str,
    property: &str,
) -> Option<Value> {
    let field = graph.resolve_alias(primary_type, property);
    match field {
        "id" => Some(view.id().into_owned()),
        "title" => {
            let (id, title) = (view.id(), view.title());
            (!title_is_synthesised(graph, primary_type, &id, &title)).then(|| title.into_owned())
        }
        _ => view
            .get(InternedKey::from_str(field))
            .map(|v| v.into_owned())
            .or_else(|| match soft_alias_fallback(field) {
                Some(SoftAliasFallback::TypeString) => {
                    Some(Value::String(primary_type.to_string()))
                }
                _ => None,
            }),
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
            let value = stored_property_value(graph, view, primary_type, property);
            property_fails(decl, check, property, value.as_ref())
        })
        .cloned()
        .collect()
}

/// Whether one property's `value` (`None` = absent) fails `check` against
/// `decl`: a required property is absent or null; a type or value violation
/// needs a present, non-null value of the wrong type or outside its contract.
/// The leaf both the stored-node
/// predicate and the pre-write row gates evaluate, so they cannot disagree.
pub fn property_fails(
    decl: &ClassDecl,
    check: &str,
    property: &str,
    value: Option<&Value>,
) -> bool {
    let present = value.filter(|v| !matches!(v, Value::Null));
    match check {
        "required_properties" => present.is_none(),
        "property_values" => present.is_some_and(|v| decl.property_values[property].violates(v)),
        _ => present.is_some_and(|v| !value_matches_type(v, &decl.property_types[property])),
    }
}

/// One declared property an edge fails, with the requirement it broke:
/// the type name for `property_types`, the contract summary for
/// `property_values`, empty for `required_properties`.
pub struct PropertyFailure {
    pub property: String,
    pub requirement: String,
}

/// The verdict on one relationship under one property check.
pub struct EdgePropertyVerdict {
    /// Some declared rule of the check binds this edge: the relationship-level
    /// contract (every edge) or a `by_source` entry its source matches. The
    /// audit's denominator counts judged edges only.
    pub judged: bool,
    pub failures: Vec<PropertyFailure>,
}

/// Whether a `by_source` or `exempt` key covers a source node of primary type
/// `source_type`: the key names that type or a declared ancestor of it, or the
/// node carries it as a secondary label (`has_secondary` answers that, and is
/// `|_| false` for a node that does not exist yet).
pub fn source_key_matches(
    store: &OntologyStore,
    key: &str,
    source_type: &str,
    has_secondary: impl Fn(&str) -> bool,
) -> bool {
    endpoint_accepted(store, key, source_type) || has_secondary(key)
}

/// Judge one relationship under `check` (`required_properties`,
/// `property_types` or `property_values`): the relationship-level contract
/// binds every edge, each `by_source` entry whose key matches the edge's
/// source binds it too. A property failing under several layers is reported
/// once, under the first. `get` reads a property off the relationship.
pub fn edge_property_verdict<'v>(
    store: &OntologyStore,
    decl: &RelationshipDecl,
    check: &str,
    source_type: &str,
    has_secondary: impl Fn(&str) -> bool,
    get: impl Fn(&str) -> Option<&'v Value>,
) -> EdgePropertyVerdict {
    let mut verdict = EdgePropertyVerdict {
        judged: decl.flat_declares(check),
        failures: Vec::new(),
    };
    judge_layer(
        check,
        &decl.required_properties,
        &decl.property_types,
        &decl.property_values,
        &get,
        &mut verdict.failures,
    );
    for (key, entry) in &decl.by_source {
        if entry.declares(check) && source_key_matches(store, key, source_type, &has_secondary) {
            verdict.judged = true;
            judge_layer(
                check,
                &entry.required_properties,
                &entry.property_types,
                &entry.property_values,
                &get,
                &mut verdict.failures,
            );
        }
    }
    verdict
}

fn judge_layer<'v>(
    check: &str,
    required: &[String],
    types: &std::collections::BTreeMap<String, String>,
    values: &std::collections::BTreeMap<String, crate::graph::ontology::ValueContract>,
    get: &impl Fn(&str) -> Option<&'v Value>,
    out: &mut Vec<PropertyFailure>,
) {
    let mut push = |property: &str, requirement: String| {
        if !out.iter().any(|f| f.property == property) {
            out.push(PropertyFailure {
                property: property.to_string(),
                requirement,
            });
        }
    };
    match check {
        "required_properties" => {
            for p in unique_required_properties(required) {
                if matches!(get(&p), None | Some(Value::Null)) {
                    push(&p, String::new());
                }
            }
        }
        "property_values" => {
            for (p, contract) in values {
                if get(p).is_some_and(|v| contract.violates(v)) {
                    push(p, contract.summary());
                }
            }
        }
        _ => {
            for (p, ty) in types {
                if get(p).is_some_and(|v| !matches!(v, Value::Null) && !value_matches_type(v, ty)) {
                    push(p, ty.clone());
                }
            }
        }
    }
}

/// Whether `idx` carries `label` as a secondary label (primary types do not
/// count). O(log bucket) and free on a graph with no secondary labels.
pub fn has_secondary_label(graph: &DirGraph, idx: NodeIndex, label: &str) -> bool {
    graph.has_secondary_labels
        && graph
            .secondary_label_index
            .get(&InternedKey::from_str(label))
            .is_some_and(|bucket| bucket.binary_search(&idx).is_ok())
}

/// Whether the declaration's `exempt[check]` entries cover a source node of
/// primary type `source_type`. An entry covers the node when it names that
/// type or a declared ancestor of it (the `domain` widening), or when the
/// node carries it as a secondary label (`has_secondary` answers that, and is
/// `|_| false` for a node that does not exist yet). An exempt violation is
/// counted separately by the audit and excused by a write gate.
pub fn source_exempt(
    store: &OntologyStore,
    decl: &RelationshipDecl,
    check: &str,
    source_type: &str,
    has_secondary: impl Fn(&str) -> bool,
) -> bool {
    decl.exempt_classes(check)
        .iter()
        .any(|entry| source_key_matches(store, entry, source_type, &has_secondary))
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

    fn failed(
        s: &OntologyStore,
        check: &str,
        source: &str,
        get: impl Fn(&str) -> Option<&'static Value>,
    ) -> Vec<String> {
        let decl = &s.relationships["MANAGED_BY"];
        edge_property_verdict(s, decl, check, source, |_| false, get)
            .failures
            .into_iter()
            .map(|f| f.property)
            .collect()
    }

    #[test]
    fn edge_property_failures_follow_audit_vocabulary() {
        let s = store();
        static NULL: Value = Value::Null;
        static INT: Value = Value::Int64(3);
        let text: &'static Value = Box::leak(Box::new(Value::String("x".into())));
        assert_eq!(
            failed(&s, "required_properties", "Contract", |_| None),
            ["validFrom"]
        );
        assert_eq!(
            failed(&s, "required_properties", "Contract", |_| Some(&NULL)),
            ["validFrom"]
        );
        // A type violation needs a present non-null value.
        assert!(failed(&s, "property_types", "Contract", |_| None).is_empty());
        assert!(failed(&s, "property_types", "Contract", |_| Some(&NULL)).is_empty());
        assert!(failed(&s, "property_types", "Contract", |_| Some(&INT)).is_empty());
        assert_eq!(
            failed(&s, "property_types", "Contract", |_| Some(text)),
            ["rank"]
        );
    }

    #[test]
    fn by_source_binds_only_matching_sources_and_reports_the_requirement() {
        let s = ontology_from_json(
            r#"{"classes": {"Licensable": {"abstract": true},
                            "Site": {"is_a": "Licensable"},
                            "Contract": {"is_a": "Licensable"},
                            "Other": {}},
                "relationships": {"MANAGED_BY": {
                    "by_source": {
                        "Site": {"required_properties": ["siteFrom"],
                                  "property_types": {"siteFrom": "date"}},
                        "Historical": {"required_properties": ["histFrom"]}}}}}"#,
        )
        .unwrap();
        let decl = &s.relationships["MANAGED_BY"];
        let none = |_: &str| None;
        let run = |check: &str, source: &str, secondary: bool| {
            edge_property_verdict(
                &s,
                decl,
                check,
                source,
                |l| secondary && l == "Historical",
                none,
            )
        };
        let field = run("required_properties", "Site", false);
        assert!(field.judged);
        assert_eq!(field.failures[0].property, "siteFrom");
        // A source no key matches is not judged at all.
        let other = run("required_properties", "Contract", false);
        assert!(!other.judged && other.failures.is_empty());
        // A secondary-label key judges a node of any class.
        let hist = run("required_properties", "Other", true);
        assert!(hist.judged);
        assert_eq!(hist.failures[0].property, "histFrom");
        // A check no matching entry declares is not judged for that source.
        assert!(!run("property_values", "Site", false).judged);
        let typed = edge_property_verdict(
            &s,
            decl,
            "property_types",
            "Site",
            |_| false,
            |_| Some(Box::leak(Box::new(Value::Int64(1))) as &Value),
        );
        assert_eq!(typed.failures[0].requirement, "date");
    }

    #[test]
    fn edge_exempt_covers_listed_class_and_descendants_for_its_check_only() {
        let s = store();
        let decl = &s.relationships["MANAGED_BY"];
        let none = |_: &str| false;
        assert!(source_exempt(
            &s,
            decl,
            "required_properties",
            "Permit",
            none
        ));
        assert!(!source_exempt(
            &s,
            decl,
            "required_properties",
            "Contract",
            none
        ));
        assert!(!source_exempt(&s, decl, "property_types", "Permit", none));
        // A secondary label named by the entry covers the node whatever its class.
        assert!(source_exempt(
            &s,
            decl,
            "required_properties",
            "Contract",
            |l| l == "Permit"
        ));
    }
}
