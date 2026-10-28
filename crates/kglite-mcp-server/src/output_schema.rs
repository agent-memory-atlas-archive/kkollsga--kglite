//! Tool output schemas declared as object schemas at the root.
//!
//! MCP 2025-06-18 requires `Tool.outputSchema` to carry `"type": "object"` at
//! its root, and strict clients (the Claude desktop app) reject the whole
//! `tools/list` when one tool's schema does not. schemars renders an untagged
//! enum as a bare `anyOf`, so every route declares its schema through
//! [`ObjectOutputSchema::with_object_output_schema`].

use rmcp::model::Tool;
use schemars::JsonSchema;
use serde_json::{Map, Value};
use std::sync::Arc;

/// `format` values defined by JSON Schema 2020-12 (section 7.3). schemars also
/// emits `uint`, `uint8`, `uint32`, `uint64` and `int64` for Rust integers;
/// clients log them as unknown formats and an Ajv strict-mode validator
/// rejects the whole schema.
const STANDARD_FORMATS: &[&str] = &[
    "date-time",
    "date",
    "time",
    "duration",
    "email",
    "idn-email",
    "hostname",
    "idn-hostname",
    "ipv4",
    "ipv6",
    "uri",
    "uri-reference",
    "iri",
    "iri-reference",
    "uuid",
    "uri-template",
    "json-pointer",
    "relative-json-pointer",
    "regex",
];

/// Remove every non-standard `format` from a tool schema, in place.
///
/// The one place KGLite's tool schemas are made portable. The integer types
/// keep `"type": "integer"` and the `minimum: 0` schemars already writes for
/// unsigned ones, so nothing is lost but the label.
pub(crate) fn strip_nonstandard_formats(schema: &mut Map<String, Value>) {
    if schema
        .get("format")
        .and_then(Value::as_str)
        .is_some_and(|format| !STANDARD_FORMATS.contains(&format))
    {
        schema.remove("format");
    }
    for (keyword, value) in schema.iter_mut() {
        // Instance data, not subschemas: a `"format"` key there is a value.
        if !matches!(keyword.as_str(), "default" | "examples" | "const" | "enum") {
            strip_in_value(value);
        }
    }
}

fn strip_in_value(value: &mut Value) {
    match value {
        Value::Object(map) => strip_nonstandard_formats(map),
        Value::Array(items) => items.iter_mut().for_each(strip_in_value),
        _ => {}
    }
}

pub(crate) trait ObjectOutputSchema {
    /// `with_output_schema::<T>()`, then `"type": "object"` at the root when
    /// the generated schema has no root type, and non-standard formats
    /// stripped. Valid for the union shapes used here because every branch is
    /// itself an object.
    fn with_object_output_schema<T: JsonSchema + 'static>(self) -> Self;

    /// `with_input_schema::<T>()` with non-standard formats stripped.
    fn with_portable_input_schema<T: JsonSchema + 'static>(self) -> Self;
}

impl ObjectOutputSchema for Tool {
    fn with_object_output_schema<T: JsonSchema + 'static>(self) -> Self {
        let mut tool = self.with_output_schema::<T>();
        if let Some(schema) = tool.output_schema.as_mut() {
            let schema = Arc::make_mut(schema);
            schema
                .entry("type")
                .or_insert_with(|| serde_json::Value::from("object"));
            strip_nonstandard_formats(schema);
        }
        tool
    }

    fn with_portable_input_schema<T: JsonSchema + 'static>(self) -> Self {
        let mut tool = self.with_input_schema::<T>();
        strip_nonstandard_formats(Arc::make_mut(&mut tool.input_schema));
        tool
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_non_standard_formats_are_removed_and_a_property_named_format_survives() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "format": {"type": "string", "format": "uuid"},
                "n": {"type": "integer", "format": "uint64", "minimum": 0},
                "xs": {"type": "array", "items": {"type": "integer", "format": "int64"}},
                "at": {"anyOf": [{"type": "string", "format": "date-time"}, {"type": "null"}]},
            },
        });
        strip_nonstandard_formats(schema.as_object_mut().expect("object"));
        assert_eq!(schema["properties"]["format"]["format"], "uuid");
        assert!(schema["properties"]["n"].get("format").is_none());
        assert_eq!(schema["properties"]["n"]["minimum"], 0);
        assert!(schema["properties"]["xs"]["items"].get("format").is_none());
        assert_eq!(
            schema["properties"]["at"]["anyOf"][0]["format"],
            "date-time"
        );
    }

    #[test]
    fn instance_data_keeps_a_format_key() {
        let mut schema = json!({
            "type": "object",
            "default": {"format": "csv"},
            "examples": [{"format": "csv"}],
            "properties": {"out": {"const": {"format": "csv"}, "enum": [{"format": "csv"}]}},
        });
        strip_nonstandard_formats(schema.as_object_mut().expect("object"));
        assert_eq!(schema["default"]["format"], "csv");
        assert_eq!(schema["examples"][0]["format"], "csv");
        assert_eq!(schema["properties"]["out"]["const"]["format"], "csv");
        assert_eq!(schema["properties"]["out"]["enum"][0]["format"], "csv");
    }
}
