//! The structured value behind an `OntologyViolation` error: which declared
//! rule refused a write (or a declaration), on what, and where.
//!
//! Raised by the write-time gates and by declare-over-data verification; lifted
//! to [`KgError::OntologyViolation`](crate::error::KgError) by `From`. Write
//! sites park it with `DirGraph::record_ontology_violation` exactly as
//! constraint sites park a `ConstraintViolation`.

use crate::graph::constraints::EntityKind;

/// The declared rule that fired. The string forms are the stable wire spelling
/// carried in `KgError::OntologyViolation::rule`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OntologyRule {
    RequiredProperty,
    PropertyType,
    ClosedLabels,
    Domain,
    Range,
    Cardinality,
    RequiredRelationship,
    MinCardinality,
    Inverse,
    Symmetric,
    Transitive,
}

impl OntologyRule {
    pub fn as_str(self) -> &'static str {
        match self {
            OntologyRule::RequiredProperty => "required_property",
            OntologyRule::PropertyType => "property_type",
            OntologyRule::ClosedLabels => "closed_labels",
            OntologyRule::Domain => "domain",
            OntologyRule::Range => "range",
            OntologyRule::Cardinality => "cardinality",
            OntologyRule::RequiredRelationship => "required_relationship",
            OntologyRule::MinCardinality => "min_cardinality",
            OntologyRule::Inverse => "inverse",
            OntologyRule::Symmetric => "symmetric",
            OntologyRule::Transitive => "transitive",
        }
    }
}

/// One refused entity: a write that broke a rule, or one row of a declaration
/// report.
#[derive(Debug, Clone, PartialEq)]
pub struct OntologyViolation {
    pub rule: OntologyRule,
    pub entity: EntityKind,
    /// Node primary label or relationship type the rule was judged on.
    pub entity_type: String,
    /// The offending property, for the property rules.
    pub property: Option<String>,
    /// Human message naming rule, type and property.
    pub message: String,
}

impl OntologyViolation {
    pub fn new(
        rule: OntologyRule,
        entity: EntityKind,
        entity_type: impl Into<String>,
        property: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            entity,
            entity_type: entity_type.into(),
            property,
            message: message.into(),
        }
    }
}

/// One line of a declaration-refusal report: `count` stored entities already
/// break `rule` on `entity_type` (and `property`).
#[derive(Debug, Clone, PartialEq)]
pub struct OntologyReportEntry {
    pub rule: OntologyRule,
    pub entity: EntityKind,
    pub entity_type: String,
    pub property: Option<String>,
    pub count: u64,
}

/// A declaration refused because stored data already violates it. `message`
/// is the full human report; `entries` is the per-rule breakdown.
#[derive(Debug, Clone, PartialEq)]
pub struct OntologyDeclarationRefused {
    pub entries: Vec<OntologyReportEntry>,
    pub message: String,
}

/// Why `DirGraph::define_ontology` did not install a declaration.
#[derive(Debug, Clone, PartialEq)]
pub enum DefineOntologyError {
    /// The declaration itself is unacceptable (structure, abstract class
    /// shadowing a live type, an audit that could not run).
    Invalid(String),
    /// Stored data already breaks an `error`-level rule.
    Refused(OntologyDeclarationRefused),
}

impl std::fmt::Display for DefineOntologyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DefineOntologyError::Invalid(m) => f.write_str(m),
            DefineOntologyError::Refused(r) => f.write_str(&r.message),
        }
    }
}

impl std::error::Error for DefineOntologyError {}

impl From<String> for DefineOntologyError {
    fn from(m: String) -> Self {
        DefineOntologyError::Invalid(m)
    }
}

impl From<DefineOntologyError> for String {
    fn from(e: DefineOntologyError) -> Self {
        e.to_string()
    }
}
