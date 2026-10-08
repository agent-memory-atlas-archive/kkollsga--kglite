//! The node half of the write-time ontology gate: which rules bind a node,
//! and the verdict on one node's stored state or on a row about to be stored.
//!
//! Three drivers share it — the Cypher statement-end judge
//! ([`judge_stored_nodes`]), the bulk-loader frame gates
//! (`mutation::ontology_frame_gate`) and nothing else — and every verdict is
//! computed by the same [`predicates`](super::predicates), so the gates and
//! `ontology_audit()` cannot disagree.
//!
//! Vocabulary (decisions D3/D4): a node is judged on its **primary** label
//! only. It answers to the class named by that label plus every declared
//! ancestor, each at that class's own severity; the allowed-labels rule
//! answers to the store-level severity.

use std::collections::{BTreeMap, HashMap};

use petgraph::graph::NodeIndex;

use super::predicates::{
    declared_node_properties, label_allowed, property_fails, stored_property_value,
};
use super::violation::{OntologyRule, OntologyViolation};
use super::{ClassDecl, Enforcement, OntologyStore, NODE_CHECK_NAMES};
use crate::datatypes::values::Value;
use crate::graph::constraints::EntityKind;
use crate::graph::schema::{DirGraph, InternedKey, PROVISIONAL_KEY};
use crate::graph::storage::{GraphRead, GraphWrite};

fn enforced(severity: Enforcement) -> bool {
    severity != Enforcement::Advisory
}

/// Whether any node rule of `store` is declared at `warn` or `error` — the
/// value of the cached `DirGraph::ontology_node_gate`. A class that declares
/// no properties for a check enrols no rule for it, whatever its severity.
pub(crate) fn node_gate_enabled(store: &OntologyStore) -> bool {
    (store.closed_labels && enforced(store.enforcement))
        || store.classes.values().any(|decl| {
            NODE_CHECK_NAMES.iter().any(|&check| {
                enforced(decl.enforcement_for(check))
                    && !declared_node_properties(decl, check).is_empty()
            })
        })
}

/// One class-level property rule that binds a node type.
struct PropertyRule {
    class: String,
    decl: ClassDecl,
    check: &'static str,
    severity: Enforcement,
    properties: Vec<String>,
}

/// Every enforced node rule that binds nodes of one primary type. Built once
/// per type per statement/frame, so the per-node cost is the predicate alone.
pub(crate) struct TypeRules {
    label: Option<Enforcement>,
    rules: Vec<PropertyRule>,
}

impl TypeRules {
    pub(crate) fn build(store: &OntologyStore, primary_type: &str) -> Self {
        let label = (!label_allowed(store, primary_type) && enforced(store.enforcement))
            .then_some(store.enforcement);
        let mut classes = store.ancestors(primary_type);
        if store.classes.contains_key(primary_type) {
            classes.insert(0, primary_type.to_string());
        }
        let mut rules = Vec::new();
        for class in classes {
            let Some(decl) = store.classes.get(&class) else {
                continue;
            };
            for &check in NODE_CHECK_NAMES {
                let severity = decl.enforcement_for(check);
                let properties = declared_node_properties(decl, check);
                if enforced(severity) && !properties.is_empty() {
                    rules.push(PropertyRule {
                        class: class.clone(),
                        decl: decl.clone(),
                        check,
                        severity,
                        properties,
                    });
                }
            }
        }
        Self { label, rules }
    }

    /// No enforced rule binds this type: its nodes need no per-node work.
    pub(crate) fn is_empty(&self) -> bool {
        self.label.is_none() && self.rules.is_empty()
    }

    /// Whether any property rule binds this type (the label rule is
    /// type-level and judged by [`Self::judge_label`]).
    pub(crate) fn has_property_rules(&self) -> bool {
        !self.rules.is_empty()
    }

    /// The allowed-labels rule, which depends on the type alone.
    pub(crate) fn judge_label(&self, primary_type: &str, tally: &mut Tally) {
        if let Some(severity) = self.label {
            tally.flag(
                severity,
                OntologyViolation::new(
                    OntologyRule::ClosedLabels,
                    EntityKind::Node,
                    primary_type,
                    None,
                    format!(
                        "ontology violation (closed_labels): node type '{primary_type}' is not \
                         a declared ontology class, and the ontology closes labels \
                         (enforcement: {})",
                        severity.as_str()
                    ),
                ),
            );
        }
    }

    /// The property rules against values `read` supplies (`None` = absent).
    /// Stops at the first `error`-level failure.
    pub(crate) fn judge_values(
        &self,
        primary_type: &str,
        read: impl Fn(&str) -> Option<Value>,
        tally: &mut Tally,
    ) {
        for rule in &self.rules {
            for property in &rule.properties {
                let value = read(property);
                if property_fails(&rule.decl, rule.check, property, value.as_ref()) {
                    tally.flag(rule.severity, rule.violation(primary_type, property));
                    if tally.is_refused() {
                        return;
                    }
                }
            }
        }
    }
}

impl PropertyRule {
    fn violation(&self, primary_type: &str, property: &str) -> OntologyViolation {
        let (rule, what) = if self.check == "required_properties" {
            (
                OntologyRule::RequiredProperty,
                format!("requires property '{property}'"),
            )
        } else {
            (
                OntologyRule::PropertyType,
                format!(
                    "property '{property}' must be of type '{}'",
                    self.decl.property_types[property]
                ),
            )
        };
        OntologyViolation::new(
            rule,
            EntityKind::Node,
            primary_type,
            Some(property.to_string()),
            format!(
                "ontology violation ({}): node type '{primary_type}' {what} (declared on class \
                 '{}', enforcement: {})",
                rule.as_str(),
                self.class,
                self.severity.as_str()
            ),
        )
    }
}

/// What a judging pass found: the first `error`-level violation (which
/// refuses the write) and a count of `warn`-level ones per rule and target.
#[derive(Default)]
pub(crate) struct Tally {
    refusal: Option<OntologyViolation>,
    warned: BTreeMap<(&'static str, String, Option<String>), usize>,
}

impl Tally {
    pub(crate) fn flag(&mut self, severity: Enforcement, violation: OntologyViolation) {
        match severity {
            Enforcement::Error => {
                self.refusal.get_or_insert(violation);
            }
            Enforcement::Warn => {
                *self
                    .warned
                    .entry((
                        violation.rule.as_str(),
                        violation.entity_type,
                        violation.property,
                    ))
                    .or_default() += 1;
            }
            Enforcement::Advisory => {}
        }
    }

    pub(crate) fn is_refused(&self) -> bool {
        self.refusal.is_some()
    }

    pub(crate) fn take_refusal(&mut self) -> Option<OntologyViolation> {
        self.refusal.take()
    }

    /// One line per warned rule and target, with its count of entities.
    pub(crate) fn warnings(&self) -> Vec<String> {
        self.warned
            .iter()
            .map(|((rule, entity_type, property), count)| {
                let target = match property {
                    Some(p) => format!("'{entity_type}' property '{p}'"),
                    None => format!("'{entity_type}'"),
                };
                format!(
                    "ontology warning ({rule}): {count} {} on {target} break(s) the \
                     declaration (enforcement: warn)",
                    if *count == 1 { "node" } else { "nodes" }
                )
            })
            .collect()
    }
}

impl DirGraph {
    /// Record the tally's refusal on the typed side channel and hand back the
    /// message for `Err(..)`; `Ok` carries the `warn`-level lines.
    pub(crate) fn settle_ontology_tally(
        &mut self,
        mut tally: Tally,
    ) -> Result<Vec<String>, String> {
        match tally.take_refusal() {
            Some(violation) => Err(self.record_ontology_violation(violation)),
            None => Ok(tally.warnings()),
        }
    }
}

/// Whether a stored node carries the auto-vivification marker. Such a stub is
/// deferred exactly as NOT NULL defers it: it holds only its id until the real
/// row arrives, and that promoting write is judged in full.
fn is_provisional_stub(view: &crate::graph::storage::NodeView<'_>) -> bool {
    matches!(
        view.get(InternedKey::from_str(PROVISIONAL_KEY)).as_deref(),
        Some(Value::Boolean(true))
    )
}

impl DirGraph {
    /// Note a node a write just changed, for the statement-end judge. One
    /// branch on the cached gate when no node rule is enforced.
    #[inline]
    pub(crate) fn note_ontology_touch(&mut self, idx: NodeIndex) {
        if self.ontology_node_gate {
            self.ontology_touched.push(idx);
        }
    }

    /// Judge, and drain, the nodes touched since the last call: their
    /// *stored* state against the enforced node rules, so a later write in the
    /// same statement can fix an earlier one. Findings accumulate in `tally`;
    /// `Err` is the first `error`-level violation, parked on the typed side
    /// channel. A node deleted since it was touched is skipped.
    pub(crate) fn judge_touched_nodes(&mut self, tally: &mut Tally) -> Result<(), String> {
        if self.ontology_touched.is_empty() {
            return Ok(());
        }
        let mut touched = std::mem::take(&mut self.ontology_touched);
        // Disk stages writes in a mut-cache that reads do not see; the judge
        // reads stored state.
        GraphWrite::flush_pending_writes(&mut self.graph);
        touched.sort_unstable();
        touched.dedup();
        {
            let graph: &DirGraph = self;
            let _arena_guard = graph.graph.begin_query();
            let mut by_type: HashMap<InternedKey, Option<TypeRules>> = HashMap::new();
            for idx in touched {
                let Some(view) = graph.graph.node_view(idx) else {
                    continue;
                };
                let type_key = view.node_type();
                let primary_type = graph.interner.resolve(type_key);
                let rules = by_type.entry(type_key).or_insert_with(|| {
                    let rules = TypeRules::build(&graph.ontology, primary_type);
                    (!rules.is_empty()).then_some(rules)
                });
                let Some(rules) = rules else {
                    continue;
                };
                rules.judge_label(primary_type, tally);
                if rules.has_property_rules() && !is_provisional_stub(&view) {
                    rules.judge_values(
                        primary_type,
                        |property| stored_property_value(graph, &view, primary_type, property),
                        tally,
                    );
                }
                if tally.is_refused() {
                    break;
                }
            }
        }
        match tally.take_refusal() {
            Some(violation) => Err(self.record_ontology_violation(violation)),
            None => Ok(()),
        }
    }
}
