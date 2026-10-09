//! The declared semantic layer: an `is_a` forest over type names plus
//! relationship semantics, persisted in `.kgl` metadata.
//!
//! **Annotations, not axioms.** The store never invents facts and never
//! changes what a query matches: it feeds `describe()`, provides defaults
//! for the rule-procedure validators, and (via the blueprint gate and the
//! node write gate) turns declarations into a data-quality contract. In spirit this is
//! SKOS, not OWL — no entailment, no open-world semantics.
//!
//! Deliberately independent of `DirGraph::parent_types`: that map is a
//! *presentation* hierarchy (describe() tiering, `graph_scale`), this one is
//! *semantic* ("kind of"). `ProposalEstimate → Proposal` is ownership and
//! belongs there; `Contract is_a Licensable` belongs here. Neither is ever
//! derived from the other.
//!
//! Like `schema_from_value`, [`ontology_from_value`] is the one external
//! dialect chokepoint: Python dicts and C-ABI JSON both become a [`Value`]
//! before parsing, so every binding shares one grammar and one set of
//! messages. Canonical documents are JSON; YAML stays a Python-side
//! convenience.
//!
//! Grammar (all keys optional unless said otherwise):
//!
//! ```json
//! {
//!   "version": 1,
//!   "classes": {
//!     "Licensable": {"abstract": true, "description": "..."},
//!     "Contract":    {"is_a": "Licensable", "by": "kind", "description": "..."}
//!   },
//!   "relationships": {
//!     "MANAGED_BY": {
//!       "domain": "Licensable", "range": "Company",
//!       "required_properties": ["validFrom"],
//!       "property_types": {"validFrom": "date"},
//!       "inverse_name": "OPERATOR_OF",
//!       "cardinality": {"min": 0, "max": 1},
//!       "required": true, "transitive": false, "symmetric": false,
//!       "ancestry": false,
//!       "enforcement": "warn",
//!       "exempt": {"required_properties": ["RegisterContract"]},
//!       "description": "Operatorship over time"
//!     }
//!   }
//! }
//! ```
//!
//! Semantics the fields promise (and no more):
//! - `is_a` is a forest — one parent, no cycles, parent must be declared.
//! - `cardinality` / `required` describe **outgoing** edges of the domain
//!   type (the validators they feed — `cardinality_violation`,
//!   `missing_required_edge` — count outgoing only).
//! - `symmetric` lowers to `inverse_violation(rel, rel)`, which reports each
//!   asymmetric pair once per direction encountered.
//! - `transitive` and `ancestry` are the two readings of "this edge is a
//!   hierarchy", and they are mutually exclusive. `transitive` promises a
//!   **stored** closure and lowers to `transitivity_violation`, which flags
//!   every `a→b→c` without a stored `a→c`. `ancestry` promises nothing to
//!   audit: it records that the chain is meaningful and is walked with
//!   `*1..`, which is what a parent-pointer taxonomy (`STRAT_PARENT`,
//!   `wdt:P279`) actually is — declaring *that* `transitive` would report
//!   100% violations.
//! - `enforcement` at `warn` or `error` is enforced at write time for the
//!   node rules (class `required_properties` / `property_types` and
//!   `closed_labels`): a violating Cypher statement or bulk load is refused
//!   (`error`) or reported (`warn`), judged on the node's primary label
//!   alone. Relationship rules are data for the blueprint gate and
//!   `ontology_audit()`, not engine write guarantees.
//! - `exempt` names, per check, source classes whose violations are counted
//!   *separately* (`ontology_audit`'s `exempted`) instead of against
//!   severity, so one legitimately-nonconforming source type cannot pin a
//!   whole rule at `advisory`. Accepted for [`EXEMPTABLE_CHECKS`] only.
//! - `by` names a discriminator property and is documentation only.
//! - `closed_labels: true` adds the allowed-labels rule: a node's primary
//!   label must be a declared class (needs at least one class).
//! - A top-level `enforcement` severity string governs that label rule and
//!   is the default for any class/relationship that states no `enforcement`
//!   of its own; an explicit per-declaration value (including `advisory`)
//!   always wins. Resolution happens at parse time, so a persisted
//!   declaration carries its effective severity.

pub(crate) mod cardinality_gate;
pub(crate) mod declare_check;
pub(crate) mod edge_gate;
pub(crate) mod node_gate;
#[cfg(test)]
mod ontology_cardinality_tests;
#[cfg(test)]
mod ontology_gate_tests;
#[cfg(test)]
mod ontology_procedure_tests;
#[cfg(test)]
mod ontology_rel_gate_tests;
pub mod predicates;
pub mod violation;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::datatypes::values::Value;

/// Schema-vocabulary scale boundary, enforced rather than documented: the
/// layer is for tens-to-hundreds of classes. Thousand-class taxonomies
/// (Wikidata P279) are *data* and belong in edges.
pub const MAX_ONTOLOGY_CLASSES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OntologyStore {
    #[serde(default = "default_version")]
    pub version: u32,
    /// Allowed-labels rule: when set, a node's **primary** label must be a
    /// declared class. Secondary labels (including engine-written
    /// materialised ones) are never judged. Absent in files written before
    /// the key existed, which read as `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closed_labels: bool,
    /// Store-level severity: governs the label rule, and is the severity
    /// every class/relationship declaration inherits at parse time when it
    /// states none of its own.
    #[serde(default, skip_serializing_if = "Enforcement::is_default")]
    pub enforcement: Enforcement,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub classes: BTreeMap<String, ClassDecl>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub relationships: BTreeMap<String, RelationshipDecl>,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ClassDecl {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_a: Option<String>,
    #[serde(
        rename = "abstract",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub is_abstract: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_properties: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub property_types: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Enforcement::is_default")]
    pub enforcement: Enforcement,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub enforcement_overrides: BTreeMap<String, Enforcement>,
    /// Documentation-only discriminator property name (unenforced).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RelationshipDecl {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_properties: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub property_types: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inverse_name: Option<String>,
    /// Opt-in: audit `inverse_name` as a physical-pairing contract. Without
    /// it the name is a reading-direction alias only (describe()/agent
    /// metadata) and enrolls no check.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inverse_enforced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cardinality: Option<CardinalityDecl>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    /// Opt-in **stored-closure** audit: `transitivity_violation` flags every
    /// `a→b→c` with no stored `a→c` edge. Correct only for graphs that
    /// materialize the closure; mutually exclusive with [`Self::ancestry`],
    /// which is what a parent-pointer taxonomy wants.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub transitive: bool,
    /// Annotation that ancestry along this relationship is meaningful and is
    /// walked with `*1..` — the parent-pointer taxonomy shape, whose closure
    /// is deliberately not stored. Reader-facing only (`describe()`, agent
    /// topics): it enrolls **no** check, which is the point — the stored-closure
    /// audit that [`Self::transitive`] enrolls would flag every edge of such
    /// a taxonomy. Declaring both is refused by [`OntologyStore::validate`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ancestry: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub symmetric: bool,
    #[serde(default, skip_serializing_if = "Enforcement::is_default")]
    pub enforcement: Enforcement,
    /// Per-check severities (keys from [`CHECK_NAMES`]); unlisted checks
    /// fall back to `enforcement`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub enforcement_overrides: BTreeMap<String, Enforcement>,
    /// Per-check source-class exemptions (keys from [`EXEMPTABLE_CHECKS`]).
    /// A violation whose edge source is one of the listed classes — or a
    /// declared descendant of one — is reported as `exempted` rather than
    /// counted against the check's severity.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub exempt: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn enforcement_summary(base: Enforcement, overrides: &BTreeMap<String, Enforcement>) -> String {
    let base = base.as_str().to_string();
    if overrides.is_empty() {
        return base;
    }
    let overrides: Vec<String> = overrides
        .iter()
        .map(|(check, severity)| format!("{check}={}", severity.as_str()))
        .collect();
    format!("{base}; {}", overrides.join(", "))
}

impl ClassDecl {
    pub(crate) fn enforcement_for(&self, check: &str) -> Enforcement {
        self.enforcement_overrides
            .get(check)
            .copied()
            .unwrap_or(self.enforcement)
    }

    pub(crate) fn enforcement_summary(&self) -> String {
        enforcement_summary(self.enforcement, &self.enforcement_overrides)
    }
}

impl RelationshipDecl {
    /// The severity governing one check of this declaration.
    pub fn enforcement_for(&self, check: &str) -> Enforcement {
        self.enforcement_overrides
            .get(check)
            .copied()
            .unwrap_or(self.enforcement)
    }

    /// The base severity, plus any per-check overrides as `check=severity`.
    /// Both reader surfaces (`SHOW ONTOLOGY`, `describe()`) render this, so a
    /// declaration whose overrides raise a check above its base severity can
    /// never read as the bare base in one of them.
    pub(crate) fn enforcement_summary(&self) -> String {
        enforcement_summary(self.enforcement, &self.enforcement_overrides)
    }

    /// Source classes exempted from `check`; empty when none are.
    pub fn exempt_classes(&self, check: &str) -> &[String] {
        self.exempt.get(check).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Declared exemptions as `check: [Class, …]`, `None` when there are
    /// none — the reader-surface companion to [`Self::enforcement_summary`],
    /// rendered by both `SHOW ONTOLOGY` and `describe()`.
    pub(crate) fn exempt_summary(&self) -> Option<String> {
        if self.exempt.is_empty() {
            return None;
        }
        Some(
            self.exempt
                .iter()
                .map(|(check, classes)| format!("{check}: [{}]", classes.join(", ")))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Enforcement {
    #[default]
    Advisory,
    Warn,
    Error,
}

impl Enforcement {
    fn is_default(&self) -> bool {
        *self == Enforcement::Advisory
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Enforcement::Advisory => "advisory",
            Enforcement::Warn => "warn",
            Enforcement::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CardinalityDecl {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u64>,
}

/// State of one materialized (managed) label — see
/// `dir_graph/ontology_apply.rs` for the invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ManagedLabelState {
    /// The engine is the bucket's only writer; it holds exactly the closure.
    Closed,
    /// Something outside the closure touched the bucket (manual SET, adopt,
    /// extend union). Correct, but closure-reliant optimizations stay off.
    Open,
}

impl ManagedLabelState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ManagedLabelState::Closed => "closed",
            ManagedLabelState::Open => "open",
        }
    }
}

/// `skip_serializing_if` helper for the `Arc<OntologyStore>` field on
/// `DirGraph` (serde hands the function `&Arc<_>`, which method syntax
/// won't coerce in the derive).
pub fn arc_store_is_empty(store: &std::sync::Arc<OntologyStore>) -> bool {
    store.is_empty()
}

impl OntologyStore {
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty() && self.relationships.is_empty()
    }

    /// Store-level settings as `closed_labels=true; enforcement=error`, or
    /// `None` when both are at their defaults — the reader-surface summary
    /// (`SHOW ONTOLOGY`, `describe()`).
    pub(crate) fn store_summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.closed_labels {
            parts.push("closed_labels=true".to_string());
        }
        if self.enforcement != Enforcement::Advisory {
            parts.push(format!("enforcement={}", self.enforcement.as_str()));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }

    /// Ancestor chain of `class`, nearest first. Empty for roots and for
    /// names the store does not declare. Bounded by the forest invariant
    /// (`validate` rejects cycles), with a defensive cap for stores built
    /// without it.
    pub fn ancestors(&self, class: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut current = class;
        while let Some(parent) = self.classes.get(current).and_then(|c| c.is_a.as_deref()) {
            if out.len() > self.classes.len() {
                break;
            }
            out.push(parent.to_string());
            current = parent;
        }
        out
    }

    /// Structural validation: the forest invariant, dangling `is_a`
    /// targets, and the class cap. Graph-aware checks (abstract vs live
    /// primary types) live in `DirGraph::define_ontology`, which has the
    /// graph.
    pub fn validate(&self) -> Result<(), String> {
        if self.closed_labels && self.classes.is_empty() {
            return Err(
                "ontology 'closed_labels' needs at least one declared class: with none, \
                 every node label would be refused"
                    .to_string(),
            );
        }
        if self.classes.len() > MAX_ONTOLOGY_CLASSES {
            return Err(format!(
                "ontology declares {} classes; the layer is for schema-level vocabularies \
                 (max {MAX_ONTOLOGY_CLASSES}). Large taxonomies are data — model them as edges \
                 walked with `*1..` paths, declaring that relationship `ancestry: true`. Do not \
                 reach for `transitive: true` there: it audits a *stored* closure, so a \
                 parent-pointer taxonomy reports 100% violations.",
                self.classes.len()
            ));
        }
        for (name, decl) in &self.classes {
            if let Some(parent) = &decl.is_a {
                if !self.classes.contains_key(parent) {
                    return Err(format!(
                        "class '{name}': is_a target '{parent}' is not a declared class"
                    ));
                }
                if parent == name {
                    return Err(format!("class '{name}': is_a itself"));
                }
            }
        }
        // Cycle check: walk each chain; the forest has ≤ classes.len() edges,
        // so a longer walk is a cycle.
        for name in self.classes.keys() {
            let mut steps = 0usize;
            let mut current = name.as_str();
            while let Some(parent) = self.classes.get(current).and_then(|c| c.is_a.as_deref()) {
                steps += 1;
                if steps > self.classes.len() {
                    return Err(format!("class '{name}': is_a chain contains a cycle"));
                }
                current = parent;
            }
        }
        for (name, decl) in &self.relationships {
            for endpoint in [&decl.domain, &decl.range].into_iter().flatten() {
                if endpoint.is_empty() {
                    return Err(format!("relationship '{name}': empty endpoint name"));
                }
            }
            if decl.transitive && decl.ancestry {
                return Err(format!(
                    "relationship '{name}': 'transitive' and 'ancestry' are mutually exclusive. \
                     'transitive' audits a stored closure — transitivity_violation flags every \
                     a→b→c with no stored a→c edge. 'ancestry' annotates a parent-pointer \
                     taxonomy walked with `*1..` and enrolls no check. Declare one."
                ));
            }
            for (check, classes) in &decl.exempt {
                for class in classes {
                    if !self.classes.contains_key(class) {
                        return Err(format!(
                            "relationship '{name}': exempt['{check}'] names '{class}', which is \
                             not a declared class. An exemption widens over the declared is_a \
                             forest, so a name outside it would silently exempt nothing — \
                             declare the class (abstract or concrete) first."
                        ));
                    }
                }
            }
            if let Some(card) = &decl.cardinality {
                if let (Some(min), Some(max)) = (card.min, card.max) {
                    if min > max {
                        return Err(format!(
                            "relationship '{name}': cardinality min {min} > max {max}"
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Parse the external dialect out of the shared [`Value`] data model — the
/// single chokepoint every binding routes through (see module doc). Unknown
/// keys are refused with a did-you-mean, the `define_schema` posture: a
/// declaration key the parser cannot place is never a harmless extra.
pub fn ontology_from_value(doc: &Value) -> Result<OntologyStore, String> {
    let map = as_map(doc).ok_or("ontology document must be a map")?;
    reject_unknown(
        map,
        &[
            "version",
            "classes",
            "relationships",
            "closed_labels",
            "enforcement",
        ],
        "ontology",
    )?;

    let mut store = OntologyStore {
        version: 1,
        ..Default::default()
    };
    if let Some(v) = map.get("version") {
        store.version = match v {
            Value::Int64(n) if *n >= 1 => *n as u32,
            _ => return Err("ontology 'version' must be a positive integer".to_string()),
        };
    }
    store.closed_labels = opt_bool(map, "closed_labels", "ontology")?;
    store.enforcement = match map.get("enforcement") {
        None => Enforcement::Advisory,
        Some(Value::String(s)) => severity_from_str(s, "ontology")?,
        Some(_) => return Err("ontology: 'enforcement' must be a severity string".to_string()),
    };
    let default = store.enforcement;
    if let Some(classes) = map.get("classes") {
        let classes = as_map(classes).ok_or("ontology 'classes' must be a map")?;
        for (name, decl) in classes {
            store
                .classes
                .insert(name.to_string(), class_from_value(name, decl, default)?);
        }
    }
    if let Some(rels) = map.get("relationships") {
        let rels = as_map(rels).ok_or("ontology 'relationships' must be a map")?;
        for (name, decl) in rels {
            store.relationships.insert(
                name.to_string(),
                relationship_from_value(name, decl, default)?,
            );
        }
    }
    store.validate()?;
    Ok(store)
}

/// [`ontology_from_value`] over a JSON document — the C ABI / file entry
/// point, routed through the same JSON→[`Value`] conversion every other
/// JSON-carrying surface uses.
pub fn ontology_from_json(json: &str) -> Result<OntologyStore, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("ontology JSON parse: {e}"))?;
    let value = crate::param::json_value_to_kglite_value(&parsed);
    ontology_from_value(&value)
}

/// The declaration as the JSON document [`ontology_from_json`] accepts —
/// the serde form, plus an explicit `enforcement` on each advisory class and
/// relationship when the store default is not advisory. The serde form drops
/// an `advisory` severity, which the parser would re-inherit from that
/// default; stating it keeps `ontology_from_json(ontology_to_json(s)) == s`
/// without bloating documents that need no help.
pub fn ontology_to_json(store: &OntologyStore) -> Result<serde_json::Value, String> {
    let mut doc =
        serde_json::to_value(store).map_err(|e| format!("ontology serialization: {e}"))?;
    if store.enforcement == Enforcement::Advisory {
        return Ok(doc);
    }
    for (section, severities) in [
        (
            "classes",
            store
                .classes
                .iter()
                .map(|(k, d)| (k, d.enforcement))
                .collect::<Vec<_>>(),
        ),
        (
            "relationships",
            store
                .relationships
                .iter()
                .map(|(k, d)| (k, d.enforcement))
                .collect(),
        ),
    ] {
        for (name, severity) in severities {
            if let Some(decl) = doc
                .get_mut(section)
                .and_then(|m| m.get_mut(name))
                .and_then(|d| d.as_object_mut())
            {
                decl.insert(
                    "enforcement".to_string(),
                    serde_json::Value::String(severity.as_str().to_string()),
                );
            }
        }
    }
    Ok(doc)
}

/// The closed accept-list for `property_types` values — every spelling
/// [`crate::graph::mutation::validation::value_matches_type`] resolves.
const PROPERTY_TYPE_NAMES: &[&str] = &[
    "string",
    "str",
    "integer",
    "int",
    "i64",
    "int64",
    "float",
    "double",
    "f64",
    "number",
    "float64",
    "boolean",
    "bool",
    "date",
    "datetime",
    "timestamp",
    "uniqueid",
    "point",
    "list",
    "array",
    "any",
];

/// Every declaration-driven check name (`DeclaredCheck::name` values) —
/// the accepted key set for the map form of `enforcement`.
pub const CHECK_NAMES: &[&str] = &[
    "domain",
    "range",
    "required",
    "required_properties",
    "property_types",
    "cardinality",
    "inverse",
    "symmetric",
    "transitive",
];

/// The checks a source-class exemption is well-defined for. Both flag one
/// edge, and the edge's source node carries exactly one domain-side class.
/// Every other check either **is** a domain-side class test (`domain`,
/// `range`, and the node-scoped `required` / `cardinality` — exempting a
/// class there disables the check for it outright) or flags a tuple of two
/// or three nodes with no single domain-side class (`inverse`, `symmetric`,
/// `transitive`).
pub const EXEMPTABLE_CHECKS: &[&str] = &["required_properties", "property_types"];

pub(crate) const NODE_CHECK_NAMES: &[&str] = &["required_properties", "property_types"];
const CLASS_KEYS: &[&str] = &[
    "is_a",
    "abstract",
    "description",
    "by",
    "required_properties",
    "property_types",
    "enforcement",
    "enforcement_overrides",
];
const REL_KEYS: &[&str] = &[
    "domain",
    "range",
    "required_properties",
    "property_types",
    "inverse_name",
    "inverse_enforced",
    "cardinality",
    "required",
    "transitive",
    "ancestry",
    "symmetric",
    "enforcement",
    "exempt",
    "description",
    "enforcement_overrides",
];

fn severity_from_str(s: &str, context: &str) -> Result<Enforcement, String> {
    match s {
        "advisory" => Ok(Enforcement::Advisory),
        "warn" => Ok(Enforcement::Warn),
        "error" => Ok(Enforcement::Error),
        other => Err(format!(
            "{context}: enforcement '{other}' is not one of 'advisory', 'warn', 'error'"
        )),
    }
}

fn parse_enforcement(
    map: &crate::datatypes::PropMap,
    context: &str,
    checks: &[&str],
    default: Enforcement,
) -> Result<(Enforcement, BTreeMap<String, Enforcement>), String> {
    let severity = |s: &str| severity_from_str(s, context);
    let (base, mut overrides) = parse_enforcement_key(map, context, checks, default)?;
    // `enforcement_overrides` is the canonical spelling `ontology()` emits next
    // to a string `enforcement` base; the map form of `enforcement` is the
    // authoring shorthand. Both land in the same per-check table.
    if let Some(extra) = map.get("enforcement_overrides") {
        let per_check = as_map(extra).ok_or_else(|| {
            format!("{context}: 'enforcement_overrides' must be a {{check: severity}} map")
        })?;
        for (check, sv) in per_check {
            if !checks.contains(&check) {
                return Err(format!(
                    "{context}: enforcement_overrides key '{check}' \
                     is not a check — use one of {checks:?}"
                ));
            }
            let Value::String(sv) = sv else {
                return Err(format!(
                    "{context}: enforcement_overrides['{check}'] must be a severity string"
                ));
            };
            overrides.insert(check.to_string(), severity(sv)?);
        }
    }
    Ok((base, overrides))
}

fn parse_enforcement_key(
    map: &crate::datatypes::PropMap,
    context: &str,
    checks: &[&str],
    default: Enforcement,
) -> Result<(Enforcement, BTreeMap<String, Enforcement>), String> {
    let severity = |s: &str| severity_from_str(s, context);
    let parsed = match map.get("enforcement") {
        None => (default, BTreeMap::new()),
        Some(Value::String(s)) => (severity(s)?, BTreeMap::new()),
        // Map form: per-check severities; unlisted checks keep the
        // store-level default base (advisory when the store sets none).
        Some(other) => match as_map(other) {
            Some(per_check) => {
                let mut overrides = BTreeMap::new();
                for (check, sv) in per_check {
                    if !checks.contains(&check) {
                        return Err(format!(
                            "{context}: enforcement key '{check}' \
                             is not a check — use one of {checks:?}"
                        ));
                    }
                    let Value::String(sv) = sv else {
                        return Err(format!(
                            "{context}: enforcement['{check}'] \
                             must be a severity string"
                        ));
                    };
                    overrides.insert(check.to_string(), severity(sv)?);
                }
                (default, overrides)
            }
            None => {
                return Err(format!(
                    "{context}: 'enforcement' must be a severity \
                     string or a {{check: severity}} map"
                ))
            }
        },
    };
    Ok(parsed)
}

fn parse_property_contract(
    map: &crate::datatypes::PropMap,
    context: &str,
) -> Result<(Vec<String>, BTreeMap<String, String>), String> {
    let required_properties = match map.get("required_properties") {
        None => Vec::new(),
        Some(Value::List(items)) => items
            .iter()
            .map(|v| match v {
                Value::String(s) => Ok(s.clone()),
                _ => Err(format!(
                    "{context}: 'required_properties' entries must be strings"
                )),
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(format!("{context}: 'required_properties' must be a list")),
    };
    let property_types = match map.get("property_types") {
        None => BTreeMap::new(),
        Some(v) => {
            let types =
                as_map(v).ok_or_else(|| format!("{context}: 'property_types' must be a map"))?;
            let mut out = BTreeMap::new();
            for (k, tv) in types {
                match tv {
                    Value::String(s) => {
                        // value_matches_type is permissive on unknown names,
                        // so a typo here would otherwise never fail anything.
                        if !PROPERTY_TYPE_NAMES.contains(&s.to_lowercase().as_str()) {
                            return Err(format!(
                                "{context}: 'property_types' entry \
                                 '{k}: {s}' names an unknown type — use one of \
                                 string, integer, float, boolean, date, \
                                 datetime, timestamp, point, list (array), any"
                            ));
                        }
                        out.insert(k.to_string(), s.clone());
                    }
                    _ => {
                        return Err(format!(
                            "{context}: 'property_types' values must be strings"
                        ))
                    }
                }
            }
            out
        }
    };
    Ok((required_properties, property_types))
}

fn class_from_value(name: &str, value: &Value, default: Enforcement) -> Result<ClassDecl, String> {
    let map = as_map(value).ok_or_else(|| format!("class '{name}' must be a map"))?;
    let context = format!("class '{name}'");
    reject_unknown(map, CLASS_KEYS, &context)?;
    let (required_properties, property_types) = parse_property_contract(map, &context)?;
    let (enforcement, enforcement_overrides) =
        parse_enforcement(map, &context, NODE_CHECK_NAMES, default)?;
    Ok(ClassDecl {
        required_properties,
        property_types,
        enforcement,
        enforcement_overrides,
        is_a: opt_string(map, "is_a", name)?,
        is_abstract: opt_bool(map, "abstract", name)?,
        description: opt_string(map, "description", name)?,
        by: opt_string(map, "by", name)?,
    })
}

fn relationship_from_value(
    name: &str,
    value: &Value,
    default: Enforcement,
) -> Result<RelationshipDecl, String> {
    let map = as_map(value).ok_or_else(|| format!("relationship '{name}' must be a map"))?;
    reject_unknown(map, REL_KEYS, &format!("relationship '{name}'"))?;
    let context = format!("relationship '{name}'");
    let (enforcement, enforcement_overrides) =
        parse_enforcement(map, &context, CHECK_NAMES, default)?;
    let cardinality = match map.get("cardinality") {
        None => None,
        Some(v) => {
            let card = as_map(v)
                .ok_or_else(|| format!("relationship '{name}': 'cardinality' must be a map"))?;
            reject_unknown(
                card,
                &["min", "max"],
                &format!("relationship '{name}' cardinality"),
            )?;
            Some(CardinalityDecl {
                min: opt_u64(card, "min", name)?,
                max: opt_u64(card, "max", name)?,
            })
        }
    };
    let (required_properties, property_types) = parse_property_contract(map, &context)?;
    let inverse_name = opt_string(map, "inverse_name", name)?;
    let inverse_enforced = opt_bool(map, "inverse_enforced", name)?;
    if inverse_enforced && inverse_name.is_none() {
        return Err(format!(
            "relationship '{name}': 'inverse_enforced' requires 'inverse_name'"
        ));
    }
    let exempt = exempt_from_value(name, map.get("exempt"))?;
    Ok(RelationshipDecl {
        domain: opt_string(map, "domain", name)?,
        range: opt_string(map, "range", name)?,
        required_properties,
        property_types,
        inverse_name,
        inverse_enforced,
        cardinality,
        required: opt_bool(map, "required", name)?,
        transitive: opt_bool(map, "transitive", name)?,
        ancestry: opt_bool(map, "ancestry", name)?,
        symmetric: opt_bool(map, "symmetric", name)?,
        enforcement,
        enforcement_overrides,
        exempt,
        description: opt_string(map, "description", name)?,
    })
}

/// `exempt: {check: [class, …]}`. The flat `exempt: [class, …]` form is
/// refused rather than spread across every check: an exemption that does
/// not name its check would silently apply where a source class is not what
/// the check tests, quietly weakening rules the declarer never considered.
fn exempt_from_value(
    name: &str,
    value: Option<&Value>,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    if matches!(value, Value::List(_)) {
        return Err(format!(
            "relationship '{name}': 'exempt' must be a {{check: [class, ...]}} map, not a list \
             — name the check each exemption applies to, e.g. \
             exempt: {{required_properties: ['RegisterContract']}}"
        ));
    }
    let per_check = as_map(value).ok_or_else(|| {
        format!("relationship '{name}': 'exempt' must be a {{check: [class, ...]}} map")
    })?;
    let mut out = BTreeMap::new();
    for (check, classes) in per_check {
        if !EXEMPTABLE_CHECKS.contains(&check) {
            return Err(exempt_check_refusal(name, check));
        }
        let Value::List(items) = classes else {
            return Err(format!(
                "relationship '{name}': exempt['{check}'] must be a list of class names"
            ));
        };
        let mut names = Vec::with_capacity(items.len());
        for item in items {
            match item {
                Value::String(s) if !s.is_empty() => names.push(s.clone()),
                Value::String(_) => {
                    return Err(format!(
                        "relationship '{name}': exempt['{check}'] has an empty class name"
                    ))
                }
                _ => {
                    return Err(format!(
                        "relationship '{name}': exempt['{check}'] entries must be strings"
                    ))
                }
            }
        }
        out.insert(check.to_string(), names);
    }
    Ok(out)
}

/// Why one check refuses exemption — the reason, not just the accept-list,
/// because "use required_properties instead" is not an answer to someone
/// who wants `domain` exempted.
fn exempt_check_refusal(name: &str, check: &str) -> String {
    let why = match check {
        "domain" | "range" | "required" | "cardinality" => {
            "that check already tests the domain-side class, so exempting a class would \
             disable it for that class outright"
        }
        "inverse" | "symmetric" | "transitive" => {
            "it flags a tuple of nodes rather than one edge, so there is no single \
             domain-side class to exempt"
        }
        _ => {
            let suggestion =
                crate::graph::mutation::validation::did_you_mean(check, EXEMPTABLE_CHECKS);
            return format!(
                "relationship '{name}': 'exempt' key '{check}' is not a check name.{suggestion} \
                 Exemption is accepted for {EXEMPTABLE_CHECKS:?} only."
            );
        }
    };
    format!(
        "relationship '{name}': 'exempt' does not accept check '{check}' — {why}. Exemption is \
         accepted for {EXEMPTABLE_CHECKS:?} only, where the exempted class is the edge's source \
         type."
    )
}

fn as_map(value: &Value) -> Option<&crate::datatypes::PropMap> {
    match value {
        Value::Map(map) => Some(map),
        _ => None,
    }
}

fn opt_string(
    map: &crate::datatypes::PropMap,
    key: &str,
    ctx: &str,
) -> Result<Option<String>, String> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("'{ctx}': '{key}' must be a string")),
    }
}

fn opt_bool(map: &crate::datatypes::PropMap, key: &str, ctx: &str) -> Result<bool, String> {
    match map.get(key) {
        None => Ok(false),
        Some(Value::Boolean(b)) => Ok(*b),
        Some(_) => Err(format!("'{ctx}': '{key}' must be a boolean")),
    }
}

fn opt_u64(map: &crate::datatypes::PropMap, key: &str, ctx: &str) -> Result<Option<u64>, String> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::Int64(n)) if *n >= 0 => Ok(Some(*n as u64)),
        Some(_) => Err(format!(
            "relationship '{ctx}': cardinality '{key}' must be a non-negative integer"
        )),
    }
}

fn reject_unknown(
    map: &crate::datatypes::PropMap,
    accepted: &[&str],
    ctx: &str,
) -> Result<(), String> {
    for (key, _) in map {
        if accepted.contains(&key) {
            continue;
        }
        let suggestion = crate::graph::mutation::validation::did_you_mean(key, accepted);
        if !suggestion.is_empty() {
            return Err(format!("{ctx}: unknown key '{key}'.{suggestion}"));
        }
        let list = accepted
            .iter()
            .map(|k| format!("'{k}'"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "{ctx}: unknown key '{key}'. Accepted keys: {list}."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<OntologyStore, String> {
        ontology_from_json(json)
    }

    #[test]
    fn round_trips_the_full_grammar() {
        let store = parse(
            r#"{"version": 1,
                "classes": {
                  "Licensable": {"abstract": true, "description": "d"},
                  "Contract": {"is_a": "Licensable", "by": "kind"}
                },
                "relationships": {
                  "MANAGED_BY": {
                    "domain": "Licensable", "range": "Company",
                    "required_properties": ["validFrom"],
                    "property_types": {"validFrom": "date"},
                    "inverse_name": "OPERATOR_OF",
                    "cardinality": {"min": 0, "max": 1},
                    "required": true, "transitive": false, "symmetric": false,
                    "ancestry": true,
                    "enforcement": "warn",
                    "exempt": {"required_properties": ["Contract"]},
                    "description": "op"
                  }
                }}"#,
        )
        .unwrap();
        assert!(store.classes["Licensable"].is_abstract);
        assert_eq!(
            store.classes["Contract"].is_a.as_deref(),
            Some("Licensable")
        );
        assert_eq!(store.ancestors("Contract"), vec!["Licensable"]);
        let rel = &store.relationships["MANAGED_BY"];
        assert_eq!(rel.enforcement, Enforcement::Warn);
        assert!(rel.ancestry && !rel.transitive);
        assert_eq!(rel.cardinality.unwrap().max, Some(1));
        assert_eq!(rel.exempt_classes("required_properties"), ["Contract"]);
        assert!(rel.exempt_classes("property_types").is_empty());
        assert_eq!(
            rel.exempt_summary().as_deref(),
            Some("required_properties: [Contract]")
        );
        // Serde round-trip (the FileMetadata path).
        let json = serde_json::to_string(&store).unwrap();
        let back: OntologyStore = serde_json::from_str(&json).unwrap();
        assert_eq!(back, store);
    }

    #[test]
    fn closed_labels_and_store_enforcement_parse_and_inherit() {
        let store = parse(
            r#"{"closed_labels": true, "enforcement": "error",
                "classes": {"A": {}, "B": {"enforcement": "advisory"},
                            "C": {"enforcement": {"required_properties": "warn"}}},
                "relationships": {"R": {"domain": "A"}}}"#,
        )
        .unwrap();
        assert!(store.closed_labels);
        assert_eq!(store.enforcement, Enforcement::Error);
        assert_eq!(store.classes["A"].enforcement, Enforcement::Error);
        // An explicit value, even `advisory`, wins over the store default.
        assert_eq!(store.classes["B"].enforcement, Enforcement::Advisory);
        // Map form: unlisted checks fall to the store default.
        assert_eq!(
            store.classes["C"].enforcement_for("property_types"),
            Enforcement::Error
        );
        assert_eq!(
            store.classes["C"].enforcement_for("required_properties"),
            Enforcement::Warn
        );
        assert_eq!(store.relationships["R"].enforcement, Enforcement::Error);
        assert_eq!(
            store.store_summary().as_deref(),
            Some("closed_labels=true; enforcement=error")
        );
    }

    #[test]
    fn closed_labels_validation_and_type_errors() {
        let err = parse(r#"{"closed_labels": true}"#).unwrap_err();
        assert!(err.contains("at least one declared class"), "{err}");
        let err = parse(r#"{"classes": {"A": {}}, "closed_labels": "yes"}"#).unwrap_err();
        assert!(
            err.contains("closed_labels") && err.contains("boolean"),
            "{err}"
        );
        let err = parse(r#"{"classes": {"A": {}}, "enforcement": "fatal"}"#).unwrap_err();
        assert!(err.contains("fatal"), "{err}");
        let err = parse(r#"{"classes": {"A": {}}, "enforcement": {"x": "warn"}}"#).unwrap_err();
        assert!(err.contains("severity string"), "{err}");
    }

    #[test]
    fn store_keys_persist_and_old_documents_read_as_defaults() {
        let store =
            parse(r#"{"classes": {"A": {}}, "closed_labels": true, "enforcement": "warn"}"#)
                .unwrap();
        let json = serde_json::to_string(&store).unwrap();
        assert_eq!(serde_json::from_str::<OntologyStore>(&json).unwrap(), store);
        // A document written before the keys existed: absent = old behaviour.
        let old: OntologyStore =
            serde_json::from_str(r#"{"version":1,"classes":{"A":{}}}"#).unwrap();
        assert!(!old.closed_labels);
        assert_eq!(old.enforcement, Enforcement::Advisory);
        // Defaults are not written, so ontologies using neither key serialize
        // byte-identically to before.
        let plain = parse(r#"{"classes": {"A": {}}}"#).unwrap();
        let text = serde_json::to_string(&plain).unwrap();
        assert!(!text.contains("closed_labels") && !text.contains("\"enforcement\":\"advisory"));
        assert!(store.store_summary().is_some() && plain.store_summary().is_none());
    }

    #[test]
    fn rejects_unknown_keys_with_suggestion() {
        let err = parse(r#"{"clases": {}}"#).unwrap_err();
        assert!(err.contains("classes"), "{err}");
        let err = parse(r#"{"classes": {"A": {"isa": "B"}}}"#).unwrap_err();
        assert!(err.contains("is_a"), "{err}");
        let err = parse(r#"{"relationships": {"R": {"enforcement": "fatal"}}}"#).unwrap_err();
        assert!(err.contains("advisory"), "{err}");
    }

    #[test]
    fn rejects_forest_violations() {
        let err = parse(r#"{"classes": {"A": {"is_a": "Missing"}}}"#).unwrap_err();
        assert!(err.contains("not a declared class"), "{err}");
        let err = parse(r#"{"classes": {"A": {"is_a": "B"}, "B": {"is_a": "A"}}}"#).unwrap_err();
        assert!(err.contains("cycle"), "{err}");
        let err = parse(r#"{"classes": {"A": {"is_a": "A"}}}"#).unwrap_err();
        assert!(err.contains("itself"), "{err}");
    }

    #[test]
    fn enforces_the_class_cap() {
        let classes: Vec<String> = (0..=MAX_ONTOLOGY_CLASSES)
            .map(|i| format!("\"C{i}\": {{}}"))
            .collect();
        let doc = format!("{{\"classes\": {{{}}}}}", classes.join(","));
        let err = parse(&doc).unwrap_err();
        assert!(err.contains("schema-level vocabularies"), "{err}");
        // The refusal used to recommend `transitive:` for exactly the shape
        // that check reports as 100% violating; it must steer to `ancestry:`
        // and say why `transitive` is not the answer.
        assert!(err.contains("`ancestry: true`"), "{err}");
        assert!(err.contains("stored"), "{err}");
    }

    #[test]
    fn transitive_and_ancestry_are_mutually_exclusive() {
        let err = parse(r#"{"relationships": {"P279": {"transitive": true, "ancestry": true}}}"#)
            .unwrap_err();
        assert!(err.contains("mutually exclusive"), "{err}");
        assert!(err.contains("stored closure"), "{err}");
        assert!(err.contains("*1.."), "{err}");
        // Either alone is accepted.
        for doc in [
            r#"{"relationships": {"P279": {"transitive": true}}}"#,
            r#"{"relationships": {"P279": {"ancestry": true}}}"#,
        ] {
            parse(doc).unwrap();
        }
    }

    #[test]
    fn ancestry_is_not_a_check_name() {
        // `ancestry` enrolls no check, so it is refused everywhere a check
        // name is expected — enforcement overrides and exempt alike.
        let err = parse(r#"{"relationships": {"R": {"enforcement": {"ancestry": "error"}}}}"#)
            .unwrap_err();
        assert!(err.contains("is not a check"), "{err}");
        let err = parse(
            r#"{"classes": {"A": {}},
                "relationships": {"R": {"exempt": {"ancestry": ["A"]}}}}"#,
        )
        .unwrap_err();
        assert!(err.contains("not a check name"), "{err}");
        assert!(!CHECK_NAMES.contains(&"ancestry"));
        assert!(!EXEMPTABLE_CHECKS.contains(&"ancestry"));
    }

    #[test]
    fn cardinality_min_over_max_rejected() {
        let err = parse(r#"{"relationships": {"R": {"cardinality": {"min": 2, "max": 1}}}}"#)
            .unwrap_err();
        assert!(err.contains("min 2 > max 1"), "{err}");
    }

    #[test]
    fn exempt_refuses_the_flat_form_and_unexemptable_checks() {
        let flat = parse(
            r#"{"classes": {"A": {}},
                "relationships": {"R": {"exempt": ["A"]}}}"#,
        )
        .unwrap_err();
        assert!(flat.contains("{check: [class, ...]}"), "{flat}");
        for (check, marker) in [
            ("domain", "already tests the domain-side class"),
            ("cardinality", "already tests the domain-side class"),
            ("transitive", "no single domain-side class"),
        ] {
            let err = parse(&format!(
                r#"{{"classes": {{"A": {{}}}},
                     "relationships": {{"R": {{"exempt": {{"{check}": ["A"]}}}}}}}}"#
            ))
            .unwrap_err();
            assert!(err.contains(marker), "{check}: {err}");
        }
        let typo = parse(
            r#"{"classes": {"A": {}},
                "relationships": {"R": {"exempt": {"required_propertys": ["A"]}}}}"#,
        )
        .unwrap_err();
        assert!(
            typo.contains("Did you mean 'required_properties'?"),
            "{typo}"
        );
    }

    #[test]
    fn exempt_class_must_be_declared() {
        let err = parse(
            r#"{"classes": {"A": {}},
                "relationships": {"R": {"exempt": {"property_types": ["Ghost"]}}}}"#,
        )
        .unwrap_err();
        assert!(err.contains("not a declared class"), "{err}");
    }

    #[test]
    fn empty_store_serializes_to_nothing_extra() {
        let store = OntologyStore::default();
        assert!(store.is_empty());
        assert_eq!(serde_json::to_string(&store).unwrap(), r#"{"version":0}"#);
    }

    /// The `ontology()` document must be accepted back by the declaring
    /// grammar, per-check overrides and a non-advisory store default
    /// included.
    #[test]
    fn rich_ontology_round_trips_through_its_own_document() {
        let rich = parse(
            r#"{"classes": {"Thing": {"abstract": true, "description": "root"},
                            "Doc": {"is_a": "Thing", "by": "kind",
                                    "required_properties": ["owner"],
                                    "property_types": {"title": "string"},
                                    "enforcement": {"required_properties": "error",
                                                    "property_types": "advisory"}},
                            "Person": {"enforcement": "advisory"}},
                "relationships": {"AUTHORED": {"domain": "Person", "range": "Doc",
                                               "inverse_name": "AUTHORED_BY",
                                               "inverse_enforced": true,
                                               "cardinality": {"min": 0, "max": 5},
                                               "required_properties": ["since"],
                                               "property_types": {"since": "integer"},
                                               "enforcement": {"domain": "error", "range": "advisory"},
                                               "exempt": {"required_properties": ["Person"]},
                                               "description": "d"}},
                "closed_labels": true,
                "enforcement": "warn"}"#,
        )
        .unwrap();
        assert!(
            !store_overrides_empty(&rich),
            "fixture must carry overrides"
        );
        let doc = ontology_to_json(&rich).unwrap();
        let back = ontology_from_json(&doc.to_string()).unwrap();
        assert_eq!(back, rich);
    }

    fn store_overrides_empty(s: &OntologyStore) -> bool {
        s.classes
            .values()
            .all(|c| c.enforcement_overrides.is_empty())
            && s.relationships
                .values()
                .all(|r| r.enforcement_overrides.is_empty())
    }
}
