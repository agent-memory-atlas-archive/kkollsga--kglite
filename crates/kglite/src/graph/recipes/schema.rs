//! The closed JSON-Schema subset a recipe's `parameters` object may use.
//!
//! Only the keywords `ALLOWED_KEYWORDS` lists compile; anything else is refused
//! rather than ignored, so a schema never advertises a constraint the
//! validator does not actually enforce.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Number, Value};

use super::validation::{
    json_equal, number_cmp, validate_exact_i64_recursive, VariablesValidationError,
};
use super::{invalid, CatalogResult};

const ROOT_KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "description",
];

const ALLOWED_KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "items",
    "enum",
    "minimum",
    "maximum",
    "minItems",
    "maxItems",
    "additionalProperties",
    "description",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ValueType {
    Null,
    Boolean,
    Object,
    Array,
    Number,
    Integer,
    String,
}

impl ValueType {
    fn parse(raw: &str) -> CatalogResult<Self> {
        match raw {
            "null" => Ok(Self::Null),
            "boolean" => Ok(Self::Boolean),
            "object" => Ok(Self::Object),
            "array" => Ok(Self::Array),
            "number" => Ok(Self::Number),
            "integer" => Ok(Self::Integer),
            "string" => Ok(Self::String),
            other => Err(invalid(format!("unsupported JSON Schema type {other:?}"))),
        }
    }

    pub(super) fn display(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Boolean => "boolean",
            Self::Object => "object",
            Self::Array => "array",
            Self::Number => "number",
            Self::Integer => "integer",
            Self::String => "string",
        }
    }
}

/// A compiled schema that keeps its source mapping, so a host can publish the
/// author's exact JSON Schema as the tool's input schema.
#[derive(Debug, Clone)]
pub struct ParameterSchema {
    raw: Map<String, Value>,
    root: SchemaNode,
}

impl ParameterSchema {
    pub fn compile_root(raw: &Value, cypher_parameters: &[String]) -> CatalogResult<Self> {
        let raw_map = raw
            .as_object()
            .ok_or_else(|| invalid("must be a mapping"))?;
        reject_unknown_keys(raw_map, ROOT_KEYWORDS, "parameters")?;
        for required_keyword in ["properties", "required", "additionalProperties"] {
            if !raw_map.contains_key(required_keyword) {
                return Err(invalid(format!("root {required_keyword} is required")));
            }
        }
        let root = SchemaNode::compile(raw, "parameters")?;
        if root.types != BTreeSet::from([ValueType::Object]) {
            return Err(invalid("root type must be exactly \"object\""));
        }
        if root.additional_properties != Some(false) {
            return Err(invalid(
                "root additionalProperties must be explicitly false",
            ));
        }

        let property_names: BTreeSet<_> = root.properties.keys().cloned().collect();
        let referenced: BTreeSet<_> = cypher_parameters.iter().cloned().collect();
        if property_names != referenced {
            let missing: Vec<_> = referenced.difference(&property_names).cloned().collect();
            let unused: Vec<_> = property_names.difference(&referenced).cloned().collect();
            return Err(invalid(format!("parameter properties must exactly match Cypher $parameters; missing={missing:?}, unused={unused:?}")));
        }
        if root.required != property_names {
            let optional: Vec<_> = property_names.difference(&root.required).cloned().collect();
            let unknown: Vec<_> = root.required.difference(&property_names).cloned().collect();
            return Err(invalid(format!("required must list every parameter property exactly; optional={optional:?}, unknown={unknown:?}")));
        }

        Ok(Self {
            raw: raw_map.clone(),
            root,
        })
    }

    pub fn as_json(&self) -> &Map<String, Value> {
        &self.raw
    }

    pub fn validate_variables(
        &self,
        variables: &Map<String, Value>,
    ) -> Result<(), VariablesValidationError> {
        let mut issues = Vec::new();
        validate_exact_i64_recursive(&Value::Object(variables.clone()), "$", &mut issues);
        self.root
            .validate(&Value::Object(variables.clone()), "$", true, &mut issues);
        if issues.is_empty() {
            Ok(())
        } else {
            Err(VariablesValidationError { issues })
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct SchemaNode {
    pub(super) types: BTreeSet<ValueType>,
    pub(super) properties: BTreeMap<String, SchemaNode>,
    pub(super) required: BTreeSet<String>,
    pub(super) items: Option<Box<SchemaNode>>,
    pub(super) enum_values: Option<Vec<Value>>,
    pub(super) minimum: Option<Number>,
    pub(super) maximum: Option<Number>,
    pub(super) min_items: Option<usize>,
    pub(super) max_items: Option<usize>,
    pub(super) additional_properties: Option<bool>,
}

impl SchemaNode {
    fn compile(raw: &Value, path: &str) -> CatalogResult<Self> {
        let map = raw
            .as_object()
            .ok_or_else(|| invalid(format!("{path} must be a schema mapping")))?;
        reject_unknown_keywords(map, path)?;
        if let Some(description) = map.get("description") {
            if !description.is_string() {
                return Err(invalid(format!("{path}.description must be a string")));
            }
        }

        let types = parse_types(
            map.get("type")
                .ok_or_else(|| invalid(format!("{path}.type is required")))?,
            path,
        )?;

        let properties = parse_properties(map.get("properties"), path)?;
        let required = parse_required(map.get("required"), path)?;
        let additional_properties = parse_bool_keyword(map, "additionalProperties", path)?;
        let items = map
            .get("items")
            .map(|value| SchemaNode::compile(value, &format!("{path}.items")).map(Box::new))
            .transpose()?;
        let enum_values = parse_enum(map.get("enum"), path)?;
        let minimum = parse_number_keyword(map, "minimum", path, &types)?;
        let maximum = parse_number_keyword(map, "maximum", path, &types)?;
        let min_items = parse_usize_keyword(map, "minItems", path)?;
        let max_items = parse_usize_keyword(map, "maxItems", path)?;

        validate_keyword_applicability(
            &types,
            &KeywordPresence {
                object: !properties.is_empty()
                    || map.contains_key("properties")
                    || map.contains_key("required")
                    || map.contains_key("additionalProperties"),
                items: items.is_some(),
                numeric_bounds: minimum.is_some() || maximum.is_some(),
                item_bounds: min_items.is_some() || max_items.is_some(),
            },
            path,
        )?;

        let unknown_required: Vec<_> = required
            .difference(&properties.keys().cloned().collect())
            .cloned()
            .collect();
        if !unknown_required.is_empty() {
            return Err(invalid(format!(
                "{path}.required names unknown properties {unknown_required:?}"
            )));
        }
        if let (Some(minimum), Some(maximum)) = (&minimum, &maximum) {
            if number_cmp(minimum, maximum) == Some(Ordering::Greater) {
                return Err(invalid(format!("{path}.minimum must not exceed maximum")));
            }
        }
        if let (Some(min_items), Some(max_items)) = (min_items, max_items) {
            if min_items > max_items {
                return Err(invalid(format!("{path}.minItems must not exceed maxItems")));
            }
        }

        let node = Self {
            types,
            properties,
            required,
            items,
            enum_values,
            minimum,
            maximum,
            min_items,
            max_items,
            additional_properties,
        };
        node.validate_enum_values(path)?;
        Ok(node)
    }

    fn validate_enum_values(&self, path: &str) -> CatalogResult<()> {
        let Some(values) = self.enum_values.as_ref() else {
            return Ok(());
        };
        for (index, value) in values.iter().enumerate() {
            let mut issues = Vec::new();
            validate_exact_i64_recursive(value, &format!("{path}.enum[{index}]"), &mut issues);
            self.validate(value, &format!("{path}.enum[{index}]"), false, &mut issues);
            if let Some(issue) = issues.first() {
                return Err(invalid(issue.message.clone()));
            }
        }
        Ok(())
    }
}

fn reject_unknown_keywords(map: &Map<String, Value>, path: &str) -> CatalogResult<()> {
    reject_unknown_keys(map, ALLOWED_KEYWORDS, path)
}

fn reject_unknown_keys(
    map: &Map<String, Value>,
    allowed: &[&str],
    path: &str,
) -> CatalogResult<()> {
    let allowed: BTreeSet<_> = allowed.iter().copied().collect();
    let unknown: Vec<_> = map
        .keys()
        .filter(|keyword| !allowed.contains(keyword.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(invalid(format!(
            "{path} uses unsupported JSON Schema keywords {unknown:?}"
        )));
    }
    Ok(())
}

fn parse_types(raw: &Value, path: &str) -> CatalogResult<BTreeSet<ValueType>> {
    let names: Vec<&str> = match raw {
        Value::String(name) => vec![name],
        Value::Array(names) if !names.is_empty() => names
            .iter()
            .map(|name| {
                name.as_str()
                    .ok_or_else(|| invalid(format!("{path}.type array must contain strings")))
            })
            .collect::<CatalogResult<_>>()?,
        Value::Array(_) => return Err(invalid(format!("{path}.type array must not be empty"))),
        _ => {
            return Err(invalid(format!(
                "{path}.type must be a string or non-empty string array"
            )))
        }
    };
    let mut types = BTreeSet::new();
    for name in names {
        let parsed =
            ValueType::parse(name).map_err(|error| error.context(format!("{path}.type")))?;
        if !types.insert(parsed) {
            return Err(invalid(format!(
                "{path}.type contains duplicate type {name:?}"
            )));
        }
    }
    Ok(types)
}

fn parse_properties(
    raw: Option<&Value>,
    path: &str,
) -> CatalogResult<BTreeMap<String, SchemaNode>> {
    let Some(raw) = raw else {
        return Ok(BTreeMap::new());
    };
    let map = raw
        .as_object()
        .ok_or_else(|| invalid(format!("{path}.properties must be a mapping")))?;
    map.iter()
        .map(|(name, value)| {
            SchemaNode::compile(value, &format!("{path}.properties.{name}"))
                .map(|schema| (name.clone(), schema))
        })
        .collect()
}

fn parse_required(raw: Option<&Value>, path: &str) -> CatalogResult<BTreeSet<String>> {
    let Some(raw) = raw else {
        return Ok(BTreeSet::new());
    };
    let items = raw
        .as_array()
        .ok_or_else(|| invalid(format!("{path}.required must be an array of strings")))?;
    let mut required = BTreeSet::new();
    for item in items {
        let name = item
            .as_str()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| invalid(format!("{path}.required must contain non-empty strings")))?;
        if !required.insert(name.to_string()) {
            return Err(invalid(format!(
                "{path}.required contains duplicate property {name:?}"
            )));
        }
    }
    Ok(required)
}

fn parse_enum(raw: Option<&Value>, path: &str) -> CatalogResult<Option<Vec<Value>>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let values = raw
        .as_array()
        .filter(|values| !values.is_empty())
        .ok_or_else(|| invalid(format!("{path}.enum must be a non-empty array")))?;
    for (index, value) in values.iter().enumerate() {
        if values[..index].iter().any(|other| json_equal(other, value)) {
            return Err(invalid(format!(
                "{path}.enum contains duplicate value {value}"
            )));
        }
    }
    Ok(Some(values.clone()))
}

fn parse_bool_keyword(
    map: &Map<String, Value>,
    keyword: &str,
    path: &str,
) -> CatalogResult<Option<bool>> {
    map.get(keyword)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| invalid(format!("{path}.{keyword} must be a boolean")))
        })
        .transpose()
}

fn parse_number_keyword(
    map: &Map<String, Value>,
    keyword: &str,
    path: &str,
    types: &BTreeSet<ValueType>,
) -> CatalogResult<Option<Number>> {
    map.get(keyword)
        .map(|value| {
            let number = value
                .as_number()
                .ok_or_else(|| invalid(format!("{path}.{keyword} must be a number")))?;
            validate_numeric_bound(number, &format!("{path}.{keyword}"), types)?;
            Ok(number.clone())
        })
        .transpose()
}

fn parse_usize_keyword(
    map: &Map<String, Value>,
    keyword: &str,
    path: &str,
) -> CatalogResult<Option<usize>> {
    map.get(keyword)
        .map(|value| {
            let number = value.as_u64().ok_or_else(|| {
                invalid(format!("{path}.{keyword} must be a non-negative integer"))
            })?;
            usize::try_from(number).map_err(|_| {
                invalid(format!(
                    "{path}.{keyword} exceeds this platform's size range"
                ))
            })
        })
        .transpose()
}

/// Which keyword groups a compiled schema mapping actually declared.
struct KeywordPresence {
    object: bool,
    items: bool,
    numeric_bounds: bool,
    item_bounds: bool,
}

fn validate_keyword_applicability(
    types: &BTreeSet<ValueType>,
    present: &KeywordPresence,
    path: &str,
) -> CatalogResult<()> {
    if present.object && !types.contains(&ValueType::Object) {
        return Err(invalid(format!(
            "{path} uses object keywords without type object"
        )));
    }
    if present.items && !types.contains(&ValueType::Array) {
        return Err(invalid(format!("{path}.items requires type array")));
    }
    if present.item_bounds && !types.contains(&ValueType::Array) {
        return Err(invalid(format!(
            "{path} uses minItems/maxItems without type array"
        )));
    }
    if present.numeric_bounds
        && !types.contains(&ValueType::Number)
        && !types.contains(&ValueType::Integer)
    {
        return Err(invalid(format!(
            "{path} uses minimum/maximum without type number or integer"
        )));
    }
    Ok(())
}

/// A compiled bound must still be the number the recipe author wrote.
///
/// serde_json folds every integer token outside `[i64::MIN, u64::MAX]` into an
/// `f64` before compilation sees it, so a `type: integer` bound that is not
/// `as_i64()`-exact was either spelled as a float or has already been rounded
/// — and neither can honestly bound a variable whose accepted values are
/// exact signed 64-bit integers.
fn validate_numeric_bound(
    number: &Number,
    path: &str,
    types: &BTreeSet<ValueType>,
) -> CatalogResult<()> {
    if number.as_i64().is_some() {
        return Ok(());
    }
    if types.contains(&ValueType::Integer) && !types.contains(&ValueType::Number) {
        return Err(invalid(format!(
            "{path} must be an exact signed 64-bit integer for type integer"
        )));
    }
    if number.is_f64() {
        return Ok(());
    }
    if number
        .to_string()
        .bytes()
        .any(|byte| matches!(byte, b'.' | b'e' | b'E'))
    {
        return Err(invalid(format!("{path} must be a finite 64-bit float")));
    }
    Err(invalid(format!(
        "{path} integer is outside KGLite's exact signed 64-bit range"
    )))
}

#[cfg(test)]
#[cfg(test)]
#[path = "schema_tests.rs"]
mod numeric_bound_tests;
