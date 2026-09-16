use std::collections::BTreeSet;

use crate::datatypes::Value as KgliteValue;
use serde_json::{json, Map, Value};

use super::super::ParameterSchema;
use super::*;

fn compile(properties: Value, required: Value) -> ParameterSchema {
    ParameterSchema::compile_root(
        &json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        }),
        &["value".to_string()],
    )
    .unwrap()
}

#[test]
fn nullable_type_arrays_accept_null_and_the_declared_value() {
    let schema = compile(
        json!({"value": {"type": ["string", "null"]}}),
        json!(["value"]),
    );
    schema
        .validate_variables(&Map::from_iter([("value".into(), Value::Null)]))
        .unwrap();
    schema
        .validate_variables(&Map::from_iter([("value".into(), json!("name"))]))
        .unwrap();
    let error = schema
        .validate_variables(&Map::from_iter([("value".into(), json!(3))]))
        .unwrap_err();
    assert_eq!(error.issues[0].kind, VariableIssueKind::WrongType);
}

#[test]
fn nested_arrays_objects_enum_and_bounds_are_validated() {
    let schema = compile(
        json!({
            "value": {
                "type": "array", "minItems": 1, "maxItems": 2,
                "items": {
                    "type": "object",
                    "properties": {
                        "score": {"type": "integer", "minimum": 0, "maximum": 10},
                        "kind": {"type": "string", "enum": ["unit", "integration"]}
                    },
                    "required": ["score", "kind"],
                    "additionalProperties": false
                }
            }
        }),
        json!(["value"]),
    );
    schema
        .validate_variables(&Map::from_iter([(
            "value".into(),
            json!([{"score": 7, "kind": "unit"}]),
        )]))
        .unwrap();

    let error = schema
        .validate_variables(&Map::from_iter([(
            "value".into(),
            json!([{"score": 11, "kind": "other", "extra": true}]),
        )]))
        .unwrap_err();
    let kinds: BTreeSet<_> = error.issues.iter().map(|issue| issue.kind).collect();
    assert!(kinds.contains(&VariableIssueKind::Maximum));
    assert!(kinds.contains(&VariableIssueKind::Enum));
    assert!(kinds.contains(&VariableIssueKind::Unknown));
}

#[test]
fn reports_missing_and_unknown_top_level_variables() {
    let schema = compile(json!({"value": {"type": "string"}}), json!(["value"]));
    let error = schema
        .validate_variables(&Map::from_iter([("other".into(), json!("x"))]))
        .unwrap_err();
    assert_eq!(error.issues.len(), 2);
    assert!(error
        .issues
        .iter()
        .any(|issue| issue.kind == VariableIssueKind::Missing));
    assert!(error
        .issues
        .iter()
        .any(|issue| issue.kind == VariableIssueKind::Unknown));
}

/// A float bound is only compilable on a `number`-typed variable — an
/// `integer` one requires an exact i64 bound — but the value reaching it
/// is still an integer beyond f64's exact range, which is the comparison
/// under test.
#[test]
fn integer_bounds_above_f64_exact_range_are_compared_without_rounding() {
    let schema = compile(
        json!({"value": {"type": "number", "maximum": 9007199254740992.0}}),
        json!(["value"]),
    );
    let error = schema
        .validate_variables(&Map::from_iter([(
            "value".into(),
            json!(9007199254740993_i64),
        )]))
        .unwrap_err();
    assert!(error
        .issues
        .iter()
        .any(|issue| issue.kind == VariableIssueKind::Maximum));
}

#[test]
fn integer_type_accepts_integral_floats_without_changing_conversion_semantics() {
    let schema = compile(json!({"value": {"type": "integer"}}), json!(["value"]));
    let variables = Map::from_iter([("value".into(), json!(1.0))]);
    schema.validate_variables(&variables).unwrap();
    assert!(matches!(
        crate::param::json_value_to_kglite_value(&variables["value"]),
        KgliteValue::Float64(value) if value == 1.0
    ));

    for rejected in [json!(1.5), json!(1e100), json!(9223372036854775808.0)] {
        let error = schema
            .validate_variables(&Map::from_iter([("value".into(), rejected)]))
            .unwrap_err();
        assert!(error
            .issues
            .iter()
            .any(|issue| issue.kind == VariableIssueKind::WrongType));
    }

    for accepted in [
        json!(i64::MIN),
        json!(i64::MAX),
        json!(-9223372036854775808.0),
    ] {
        schema
            .validate_variables(&Map::from_iter([("value".into(), accepted)]))
            .unwrap();
    }
    assert!(!is_integral_i64_float(f64::NAN));
    assert!(!is_integral_i64_float(f64::INFINITY));
}

#[test]
fn enum_numeric_equality_recurses_through_arrays_and_objects() {
    let duplicate = ParameterSchema::compile_root(
        &json!({
            "type": "object",
            "properties": {
                "value": {
                    "type": "object",
                    "enum": [
                        {"nested": [1, {"score": 2.0}]},
                        {"nested": [1.0, {"score": 2}]}
                    ]
                }
            },
            "required": ["value"],
            "additionalProperties": false
        }),
        &["value".to_string()],
    )
    .unwrap_err();
    assert!(format!("{duplicate:#}").contains("duplicate value"));

    let schema = compile(
        json!({
            "value": {
                "type": "object",
                "enum": [{"nested": [1.0, {"score": 2}]}]
            }
        }),
        json!(["value"]),
    );
    schema
        .validate_variables(&Map::from_iter([(
            "value".into(),
            json!({"nested": [1, {"score": 2.0}]}),
        )]))
        .unwrap();
    let error = schema
        .validate_variables(&Map::from_iter([(
            "value".into(),
            json!({"nested": [1, {"score": 3}]}),
        )]))
        .unwrap_err();
    assert!(error
        .issues
        .iter()
        .any(|issue| issue.kind == VariableIssueKind::Enum));
}

#[test]
fn numeric_enum_equality_ignores_equivalent_float_lexemes() {
    let schema_value: Value =
        serde_json::from_str(r#"{"value":{"type":"object","enum":[{"nested":[1.0]}]}}"#).unwrap();
    let schema = compile(schema_value, json!(["value"]));
    let variables: Value = serde_json::from_str(r#"{"value":{"nested":[1e0]}}"#).unwrap();
    schema
        .validate_variables(variables.as_object().unwrap())
        .unwrap();
}

#[test]
fn rejects_integers_outside_i64_at_boot_and_runtime_without_f64_conversion() {
    let overflow = json!(9223372036854775808_u64);
    let schema_error = ParameterSchema::compile_root(
        &json!({
            "type": "object",
            "properties": {"value": {"type": "integer", "enum": [overflow.clone()]}},
            "required": ["value"],
            "additionalProperties": false
        }),
        &["value".to_string()],
    )
    .unwrap_err();
    assert!(format!("{schema_error:#}").contains("signed 64-bit"));

    let schema = compile(json!({"value": {"type": "number"}}), json!(["value"]));
    let runtime_error = schema
        .validate_variables(&Map::from_iter([("value".into(), overflow)]))
        .unwrap_err();
    assert!(runtime_error
        .issues
        .iter()
        .any(|issue| issue.kind == VariableIssueKind::IntegerRange));
}

#[test]
fn rejects_unsupported_keywords_and_keyword_collisions() {
    let unsupported = ParameterSchema::compile_root(
        &json!({
            "type": "object",
            "properties": {"value": {"type": "string", "pattern": "x"}},
            "required": ["value"],
            "additionalProperties": false
        }),
        &["value".to_string()],
    )
    .unwrap_err();
    assert!(format!("{unsupported:#}").contains("unsupported JSON Schema keywords"));

    let duplicate_required = ParameterSchema::compile_root(
        &json!({
            "type": "object",
            "properties": {"value": {"type": "string"}},
            "required": ["value", "value"],
            "additionalProperties": false
        }),
        &["value".to_string()],
    )
    .unwrap_err();
    assert!(format!("{duplicate_required:#}").contains("duplicate property"));
}

#[test]
fn root_is_closed_and_every_parameter_is_required() {
    for raw in [
        json!({"type": "object", "properties": {}, "required": []}),
        json!({"type": "object", "additionalProperties": false}),
        json!({
            "type": ["object", "null"], "properties": {}, "required": [],
            "additionalProperties": false
        }),
        json!({
            "type": "object", "properties": {"value": {"type": "string"}},
            "required": [], "additionalProperties": false
        }),
    ] {
        assert!(ParameterSchema::compile_root(&raw, &[]).is_err());
    }
}
