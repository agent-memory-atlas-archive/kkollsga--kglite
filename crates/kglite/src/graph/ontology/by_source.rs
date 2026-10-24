//! Per-source-class property contracts on a relationship.
//!
//! One relationship type can leave several source classes, each carrying its
//! own property names (`MANAGED_BY` from `Site` carries `siteManagedFrom`,
//! from `Contract` `contractValidFrom`). A relationship-level
//! `required_properties` flags every source but one, so the contract is
//! declared per source under the relationship's `by_source` key:
//!
//! ```json
//! "by_source": {"Site": {"required_properties": ["siteManagedFrom"],
//!                        "property_types": {"siteManagedFrom": "date"}}}
//! ```
//!
//! A key matches an edge's source node exactly as an `exempt` entry does: by
//! primary class, by declared ancestor, or by secondary label. An edge whose
//! source matches no key is not judged by `by_source`; the relationship-level
//! contract, when also declared, still binds every edge.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::contracts::{self, ValueContract};
use super::{as_map, parse_property_contract, reject_unknown};
use crate::datatypes::values::Value;

/// The property contract one source class adds to a relationship.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SourceContract {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_properties: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub property_types: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub property_values: BTreeMap<String, ValueContract>,
}

impl SourceContract {
    /// Whether this entry declares anything for `check`.
    pub(crate) fn declares(&self, check: &str) -> bool {
        match check {
            "required_properties" => !self.required_properties.is_empty(),
            "property_values" => !self.property_values.is_empty(),
            _ => !self.property_types.is_empty(),
        }
    }

    /// The properties this entry declares for `check`.
    pub(crate) fn property_names(&self, check: &str) -> Vec<String> {
        match check {
            "required_properties" => {
                super::predicates::unique_required_properties(&self.required_properties)
            }
            "property_values" => self.property_values.keys().cloned().collect(),
            _ => self.property_types.keys().cloned().collect(),
        }
    }
}

const SOURCE_KEYS: &[&str] = &["required_properties", "property_types", "property_values"];

/// `by_source: {SourceClass: {required_properties, property_types,
/// property_values}}`.
pub(super) fn by_source_from_value(
    map: &crate::datatypes::PropMap,
    context: &str,
) -> Result<BTreeMap<String, SourceContract>, String> {
    let Some(value) = map.get("by_source") else {
        return Ok(BTreeMap::new());
    };
    let declared = as_map(value).ok_or_else(|| {
        format!("{context}: 'by_source' must be a {{SourceClass: {{contract}}}} map")
    })?;
    let mut out = BTreeMap::new();
    for (class, entry) in declared {
        let at = format!("{context}: by_source['{class}']");
        if class.is_empty() {
            return Err(format!("{context}: 'by_source' has an empty class name"));
        }
        let Value::Map(inner) = entry else {
            return Err(format!(
                "{at} must be a map of required_properties / property_types / property_values"
            ));
        };
        reject_unknown(inner, SOURCE_KEYS, &at)?;
        let (required_properties, property_types) = parse_property_contract(inner, &at)?;
        let property_values = contracts::property_values_from_value(inner, &at)?;
        let contract = SourceContract {
            required_properties,
            property_types,
            property_values,
        };
        if SOURCE_KEYS.iter().all(|check| !contract.declares(check)) {
            return Err(format!("{at} declares no property rule"));
        }
        out.insert(class.to_string(), contract);
    }
    Ok(out)
}

impl super::RelationshipDecl {
    /// Whether the declaration states any property rule for `check`
    /// (`required_properties`, `property_types` or `property_values`), at
    /// the relationship level or under `by_source`.
    pub(crate) fn declares_property_check(&self, check: &str) -> bool {
        let flat = match check {
            "required_properties" => !self.required_properties.is_empty(),
            "property_values" => !self.property_values.is_empty(),
            _ => !self.property_types.is_empty(),
        };
        flat || self.by_source.values().any(|entry| entry.declares(check))
    }

    /// Every property `check` declares, flat and per source, de-duplicated
    /// and in a stable order (flat names first).
    pub(crate) fn declared_property_names(&self, check: &str) -> Vec<String> {
        let mut names: Vec<String> = match check {
            "required_properties" => {
                super::predicates::unique_required_properties(&self.required_properties)
            }
            "property_values" => self.property_values.keys().cloned().collect(),
            _ => self.property_types.keys().cloned().collect(),
        };
        for entry in self.by_source.values() {
            for name in entry.property_names(check) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }

    /// Whether the relationship-level contract alone binds every edge of
    /// `check` (so no per-source match is needed to judge an edge).
    pub(crate) fn flat_declares(&self, check: &str) -> bool {
        match check {
            "required_properties" => !self.required_properties.is_empty(),
            "property_values" => !self.property_values.is_empty(),
            _ => !self.property_types.is_empty(),
        }
    }

    /// `Class: required_properties [..]; property_types {..}` for the
    /// reader surfaces; `None` when nothing is declared per source.
    pub(crate) fn by_source_summary(&self) -> Option<String> {
        if self.by_source.is_empty() {
            return None;
        }
        Some(
            self.by_source
                .iter()
                .map(|(class, entry)| {
                    let mut parts: Vec<String> = Vec::new();
                    if !entry.required_properties.is_empty() {
                        parts.push(format!(
                            "required_properties [{}]",
                            entry.required_properties.join(", ")
                        ));
                    }
                    if !entry.property_types.is_empty() {
                        let fields: Vec<_> = entry
                            .property_types
                            .iter()
                            .map(|(k, v)| format!("{k}: {v}"))
                            .collect();
                        parts.push(format!("property_types {{{}}}", fields.join(", ")));
                    }
                    if !entry.property_values.is_empty() {
                        let fields: Vec<_> = entry
                            .property_values
                            .iter()
                            .map(|(k, v)| format!("{k}: {}", v.summary()))
                            .collect();
                        parts.push(format!("property_values {{{}}}", fields.join(" | ")));
                    }
                    format!("{class}: {}", parts.join("; "))
                })
                .collect::<Vec<_>>()
                .join(" || "),
        )
    }
}
