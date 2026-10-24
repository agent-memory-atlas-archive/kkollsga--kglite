//! Declared value contracts and pair uniqueness: the two ontology rule
//! shapes that read more than a property's presence or type.
//!
//! - [`ValueContract`]: what one property's *values* may be (`enum`,
//!   `pattern` / `not_pattern`, `min` / `max`). Declared per property under
//!   `property_values` on a class or a relationship.
//! - [`DistinctDecl`]: a relationship type holds at most one edge per
//!   `(source, target[, listed property values])` group.
//!
//! Both are spec only: nothing here is stored per value or per edge.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use regex::Regex;
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{as_map, reject_unknown};
use crate::datatypes::values::Value;

/// A relationship type's pair-uniqueness contract. `properties` empty means
/// "no two edges between the same source and target"; otherwise the group key
/// also carries the listed property values.
///
/// Serialised as the document form `true` or `["prop", ...]`, so a persisted
/// or `ontology()`-read declaration re-parses unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DistinctDecl {
    pub properties: Vec<String>,
}

impl Serialize for DistinctDecl {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.properties.is_empty() {
            serializer.serialize_bool(true)
        } else {
            self.properties.serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for DistinctDecl {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DistinctVisitor;
        impl<'de> Visitor<'de> for DistinctVisitor {
            type Value = DistinctDecl;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("`true` or a list of property names")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<DistinctDecl, E> {
                if v {
                    Ok(DistinctDecl::default())
                } else {
                    Err(E::custom("`distinct: false` is not stored; omit the key"))
                }
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<DistinctDecl, A::Error> {
                let mut properties = Vec::new();
                while let Some(p) = seq.next_element::<String>()? {
                    properties.push(p);
                }
                Ok(DistinctDecl { properties })
            }
        }
        deserializer.deserialize_any(DistinctVisitor)
    }
}

impl DistinctDecl {
    /// Reader-surface rendering: `true` or `[side, kind]`.
    pub(crate) fn summary(&self) -> String {
        if self.properties.is_empty() {
            "true".to_string()
        } else {
            format!("[{}]", self.properties.join(", "))
        }
    }
}

/// One accepted value of an `enum`.
#[derive(Debug, Clone, PartialEq)]
enum Allowed {
    Str(String),
    Num(f64),
    Bool(bool),
}

/// The contract's executable form: regexes compiled once, numbers widened.
#[derive(Debug)]
struct Parts {
    allowed: Vec<Allowed>,
    pattern: Option<Regex>,
    not_pattern: Option<Regex>,
    min: Option<f64>,
    max: Option<f64>,
}

/// Lazily-built [`Parts`], shared by every clone of the contract so a spec
/// that is cloned into per-type rule tables still compiles its regexes once.
/// Never part of equality or serialisation.
#[derive(Clone, Default)]
struct Compiled(Arc<OnceLock<Parts>>);

impl std::fmt::Debug for Compiled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Compiled")
    }
}

impl PartialEq for Compiled {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Compiled {}

/// What the values of one property may be. A property absent from a node or
/// edge, or holding null, is never judged here (`required_properties` owns
/// presence).
///
/// - `enum`: the value must equal one entry. Judged on every value type;
///   integers and floats compare numerically.
/// - `pattern` / `not_pattern`: a regex the value must match / must not
///   match, unanchored (use `^…$` to anchor). Judged on string values only;
///   other types are `property_types`' concern.
/// - `min` / `max`: inclusive numeric bounds. Judged on integer and float
///   values only.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ValueContract {
    #[serde(rename = "enum", default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<serde_json::Number>,
    #[serde(skip)]
    compiled: Compiled,
}

fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Int64(n) => Some(*n as f64),
        Value::Float64(n) => Some(*n),
        Value::UniqueId(n) => Some(f64::from(*n)),
        _ => None,
    }
}

impl Allowed {
    fn from_json(value: &serde_json::Value) -> Option<Self> {
        match value {
            serde_json::Value::String(s) => Some(Allowed::Str(s.clone())),
            serde_json::Value::Bool(b) => Some(Allowed::Bool(*b)),
            serde_json::Value::Number(n) => n.as_f64().map(Allowed::Num),
            _ => None,
        }
    }

    fn matches(&self, value: &Value) -> bool {
        match (self, value) {
            (Allowed::Str(a), Value::String(b)) => a == b,
            (Allowed::Bool(a), Value::Boolean(b)) => a == b,
            (Allowed::Num(a), v) => numeric(v).is_some_and(|b| *a == b),
            _ => false,
        }
    }
}

impl ValueContract {
    fn parts(&self) -> &Parts {
        self.compiled.0.get_or_init(|| Parts {
            allowed: self.allowed.iter().filter_map(Allowed::from_json).collect(),
            // A pattern that fails to compile was refused at declaration;
            // only a hand-edited persisted store reaches `None` here.
            pattern: self.pattern.as_deref().and_then(|p| Regex::new(p).ok()),
            not_pattern: self.not_pattern.as_deref().and_then(|p| Regex::new(p).ok()),
            min: self.min.as_ref().and_then(serde_json::Number::as_f64),
            max: self.max.as_ref().and_then(serde_json::Number::as_f64),
        })
    }

    /// Whether `value` (present, non-null) breaks the contract.
    pub fn violates(&self, value: &Value) -> bool {
        if matches!(value, Value::Null) {
            return false;
        }
        let parts = self.parts();
        if !parts.allowed.is_empty() && !parts.allowed.iter().any(|a| a.matches(value)) {
            return true;
        }
        if let Value::String(text) = value {
            if parts.pattern.as_ref().is_some_and(|re| !re.is_match(text)) {
                return true;
            }
            if parts
                .not_pattern
                .as_ref()
                .is_some_and(|re| re.is_match(text))
            {
                return true;
            }
        }
        if let Some(n) = numeric(value) {
            if parts.min.is_some_and(|min| n < min) || parts.max.is_some_and(|max| n > max) {
                return true;
            }
        }
        false
    }

    /// The contract as `enum [a, b]; pattern /…/; min 1` — the reader-surface
    /// rendering and the tail of a write-time violation message.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.allowed.is_empty() {
            let items: Vec<String> = self
                .allowed
                .iter()
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect();
            parts.push(format!("enum [{}]", items.join(", ")));
        }
        if let Some(p) = &self.pattern {
            parts.push(format!("pattern /{p}/"));
        }
        if let Some(p) = &self.not_pattern {
            parts.push(format!("not_pattern /{p}/"));
        }
        if let Some(n) = &self.min {
            parts.push(format!("min {n}"));
        }
        if let Some(n) = &self.max {
            parts.push(format!("max {n}"));
        }
        parts.join("; ")
    }

    fn validate(&self, context: &str, property: &str) -> Result<(), String> {
        let at = format!("{context}: property_values['{property}']");
        if self.allowed.is_empty()
            && self.pattern.is_none()
            && self.not_pattern.is_none()
            && self.min.is_none()
            && self.max.is_none()
        {
            return Err(format!(
                "{at} declares no constraint — give it at least one of enum, pattern, \
                 not_pattern, min, max"
            ));
        }
        for (key, source) in [
            ("pattern", &self.pattern),
            ("not_pattern", &self.not_pattern),
        ] {
            if let Some(source) = source {
                Regex::new(source)
                    .map_err(|e| format!("{at}: '{key}' is not a valid regex: {e}"))?;
            }
        }
        if let (Some(min), Some(max)) = (
            self.min.as_ref().and_then(serde_json::Number::as_f64),
            self.max.as_ref().and_then(serde_json::Number::as_f64),
        ) {
            if min > max {
                return Err(format!("{at}: min {min} is above max {max}"));
            }
        }
        Ok(())
    }
}

const CONTRACT_KEYS: &[&str] = &["enum", "pattern", "not_pattern", "min", "max"];

fn number_from_value(value: &Value, at: &str, key: &str) -> Result<serde_json::Number, String> {
    match value {
        Value::Int64(n) => Ok((*n).into()),
        Value::Float64(n) => serde_json::Number::from_f64(*n)
            .ok_or_else(|| format!("{at}: '{key}' must be a finite number")),
        _ => Err(format!("{at}: '{key}' must be a number")),
    }
}

fn contract_from_value(
    context: &str,
    property: &str,
    value: &Value,
) -> Result<ValueContract, String> {
    let at = format!("{context}: property_values['{property}']");
    let map = as_map(value)
        .ok_or_else(|| format!("{at} must be a map of enum / pattern / not_pattern / min / max"))?;
    reject_unknown(map, CONTRACT_KEYS, &at)?;
    let mut contract = ValueContract::default();
    if let Some(list) = map.get("enum") {
        let Value::List(items) = list else {
            return Err(format!("{at}: 'enum' must be a list"));
        };
        if items.is_empty() {
            return Err(format!("{at}: 'enum' must list at least one value"));
        }
        for item in items {
            contract.allowed.push(match item {
                Value::String(s) => serde_json::Value::String(s.clone()),
                Value::Boolean(b) => serde_json::Value::Bool(*b),
                Value::Int64(_) | Value::Float64(_) => {
                    serde_json::Value::Number(number_from_value(item, &at, "enum")?)
                }
                _ => {
                    return Err(format!(
                        "{at}: 'enum' entries must be strings, numbers or booleans"
                    ))
                }
            });
        }
    }
    for (key, slot) in [
        ("pattern", &mut contract.pattern),
        ("not_pattern", &mut contract.not_pattern),
    ] {
        match map.get(key) {
            None => {}
            Some(Value::String(s)) => *slot = Some(s.clone()),
            Some(_) => return Err(format!("{at}: '{key}' must be a regex string")),
        }
    }
    for (key, slot) in [("min", &mut contract.min), ("max", &mut contract.max)] {
        if let Some(v) = map.get(key) {
            *slot = Some(number_from_value(v, &at, key)?);
        }
    }
    contract.validate(context, property)?;
    Ok(contract)
}

/// `property_values: {property: {enum|pattern|not_pattern|min|max}}`.
pub(super) fn property_values_from_value(
    map: &crate::datatypes::PropMap,
    context: &str,
) -> Result<BTreeMap<String, ValueContract>, String> {
    let Some(value) = map.get("property_values") else {
        return Ok(BTreeMap::new());
    };
    let declared = as_map(value).ok_or_else(|| {
        format!("{context}: 'property_values' must be a {{property: contract}} map")
    })?;
    declared
        .iter()
        .map(|(property, contract)| {
            contract_from_value(context, property, contract).map(|c| (property.to_string(), c))
        })
        .collect()
}

/// `distinct: true | [property, ...]`; `false` and an absent key declare
/// nothing.
pub(super) fn distinct_from_value(
    name: &str,
    value: Option<&Value>,
) -> Result<Option<DistinctDecl>, String> {
    let context = format!("relationship '{name}': 'distinct'");
    match value {
        None | Some(Value::Boolean(false)) => Ok(None),
        Some(Value::Boolean(true)) => Ok(Some(DistinctDecl::default())),
        Some(Value::List(items)) => {
            if items.is_empty() {
                return Err(format!(
                    "{context} lists no properties — use `true` for endpoint-pair uniqueness"
                ));
            }
            let mut properties: Vec<String> = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(s) if !s.is_empty() => {
                        if properties.contains(s) {
                            return Err(format!("{context} names '{s}' twice"));
                        }
                        properties.push(s.clone());
                    }
                    _ => return Err(format!("{context} entries must be non-empty strings")),
                }
            }
            Ok(Some(DistinctDecl { properties }))
        }
        Some(_) => Err(format!(
            "{context} must be `true` or a list of property names"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(json: &str) -> ValueContract {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn enum_matches_by_value_and_numeric_equality() {
        let c = contract(r#"{"enum": ["a", 2, true]}"#);
        assert!(!c.violates(&Value::String("a".into())));
        assert!(!c.violates(&Value::Int64(2)));
        assert!(!c.violates(&Value::Float64(2.0)));
        assert!(!c.violates(&Value::Boolean(true)));
        assert!(c.violates(&Value::String("b".into())));
        assert!(c.violates(&Value::Int64(3)));
        assert!(c.violates(&Value::Boolean(false)));
    }

    #[test]
    fn pattern_and_bounds_ignore_values_of_other_types_and_null() {
        let c = contract(r#"{"pattern": "^a", "not_pattern": "z$", "min": 1, "max": 5}"#);
        assert!(!c.violates(&Value::Null));
        assert!(!c.violates(&Value::String("ab".into())));
        assert!(c.violates(&Value::String("ba".into())));
        assert!(c.violates(&Value::String("az".into())));
        // Numbers are not regex-judged; strings are not range-judged.
        assert!(!c.violates(&Value::Int64(3)));
        assert!(c.violates(&Value::Int64(0)));
        assert!(c.violates(&Value::Float64(5.5)));
        assert!(!c.violates(&Value::Int64(5)));
    }

    #[test]
    fn distinct_round_trips_through_serde() {
        let pair: DistinctDecl = serde_json::from_str("true").unwrap();
        assert!(pair.properties.is_empty());
        assert_eq!(serde_json::to_string(&pair).unwrap(), "true");
        let keyed: DistinctDecl = serde_json::from_str(r#"["side","kind"]"#).unwrap();
        assert_eq!(serde_json::to_string(&keyed).unwrap(), r#"["side","kind"]"#);
        assert!(serde_json::from_str::<DistinctDecl>("false").is_err());
    }
}
