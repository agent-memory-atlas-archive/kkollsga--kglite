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
}

impl OntologyRule {
    pub fn as_str(self) -> &'static str {
        match self {
            OntologyRule::RequiredProperty => "required_property",
            OntologyRule::PropertyType => "property_type",
            OntologyRule::ClosedLabels => "closed_labels",
            OntologyRule::Domain => "domain",
            OntologyRule::Range => "range",
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
