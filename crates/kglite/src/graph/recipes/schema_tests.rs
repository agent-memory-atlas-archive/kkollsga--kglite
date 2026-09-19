use super::*;

fn schema_with_bounds(bounds: &str) -> Value {
    serde_json::from_str(&format!(
        r#"{{
            "type":"object",
            "properties":{{"value":{{"type":"number",{bounds}}}}},
            "required":["value"],
            "additionalProperties":false
        }}"#
    ))
    .unwrap()
}

#[test]
fn typed_unsigned_bound_above_i64_is_rejected() {
    let mut schema = schema_with_bounds(r#""minimum":0"#);
    schema["properties"]["value"]["maximum"] = Value::Number(Number::from(u64::MAX));
    let error = ParameterSchema::compile_root(&schema, &["value".to_string()])
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("parameters.properties.value.maximum"),
        "{error}"
    );
    assert!(error.contains("signed 64-bit range"), "{error}");
}

/// A recipe bound spelled beyond `i64` is folded to an `f64` by
/// serde_json before compilation sees it, so an integer variable would
/// silently be bounded by a different number than the author wrote.
#[test]
fn integer_typed_bounds_outside_exact_i64_are_rejected() {
    for bound in [
        r#""minimum":-9223372036854775809"#,
        r#""maximum":18446744073709551616"#,
        r#""minimum":-18446744073709551616"#,
    ] {
        let raw: Value = serde_json::from_str(&format!(
            r#"{{
                "type":"object",
                "properties":{{"value":{{"type":"integer",{bound}}}}},
                "required":["value"],
                "additionalProperties":false
            }}"#
        ))
        .unwrap();
        let error = ParameterSchema::compile_root(&raw, &["value".to_string()])
            .unwrap_err()
            .to_string();
        assert!(error.contains("parameters.properties.value"), "{error}");
        assert!(error.contains("exact signed 64-bit integer"), "{error}");
    }
}

#[test]
fn ordinary_json_parser_refuses_nonfinite_bound_before_schema_compile() {
    let source = r#"{
        "type":"object",
        "properties":{"value":{"type":"number","minimum":1e400}},
        "required":["value"],
        "additionalProperties":false
    }"#;
    assert!(serde_json::from_str::<Value>(source).is_err());
}

#[test]
fn numeric_bounds_accept_signed_limits_and_finite_explicit_floats() {
    for bounds in [
        r#""minimum":-9223372036854775808,"maximum":9223372036854775807"#,
        r#""minimum":-1e300,"maximum":1e300"#,
    ] {
        ParameterSchema::compile_root(&schema_with_bounds(bounds), &["value".to_string()]).unwrap();
    }
}

fn schema_with_limit(limit: &str, required: &str) -> Value {
    serde_json::from_str(&format!(
        r#"{{
            "type":"object",
            "properties":{{"query":{{"type":"string"}},"limit":{limit}}},
            "required":{required},
            "additionalProperties":false
        }}"#
    ))
    .unwrap()
}

fn compile_with_limit(limit: &str, required: &str) -> CatalogResult<ParameterSchema> {
    ParameterSchema::compile_root(
        &schema_with_limit(limit, required),
        &["limit".to_string(), "query".to_string()],
    )
}

#[test]
fn a_property_default_is_accepted_and_excuses_it_from_required() {
    let schema = compile_with_limit(r#"{"type":"integer","default":5}"#, r#"["query"]"#)
        .expect("a defaulted property may be omitted from required");
    assert_eq!(
        schema.as_json()["properties"]["limit"]["default"],
        Value::from(5),
        "the author's schema is published unchanged"
    );
}

#[test]
fn a_defaulted_property_listed_as_required_is_refused() {
    let error = compile_with_limit(r#"{"type":"integer","default":5}"#, r#"["limit","query"]"#)
        .unwrap_err()
        .to_string();
    assert!(error.contains("required must list every"), "{error}");
    assert!(error.contains("limit"), "{error}");
}

#[test]
fn a_property_without_a_default_must_still_be_required() {
    let error = compile_with_limit(r#"{"type":"integer"}"#, r#"["query"]"#)
        .unwrap_err()
        .to_string();
    assert!(error.contains("required must list every"), "{error}");
    assert!(error.contains("limit"), "{error}");
}

#[test]
fn a_default_that_fails_its_own_property_is_refused_at_compile() {
    for (limit, expected) in [
        (r#"{"type":"integer","default":"five"}"#, "must have type"),
        (
            r#"{"type":"string","enum":["a","b"],"default":"c"}"#,
            "not one of the allowed enum values",
        ),
        (
            r#"{"type":"integer","minimum":1,"maximum":10,"default":0}"#,
            "below minimum",
        ),
        (
            r#"{"type":"integer","minimum":1,"maximum":10,"default":11}"#,
            "exceeds maximum",
        ),
    ] {
        let error = compile_with_limit(limit, r#"["query"]"#)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("parameters.properties.limit.default"),
            "{error}"
        );
        assert!(error.contains(expected), "{error}");
    }
}

/// Defaults bind absent *Cypher parameters*, which are the root's own
/// properties. A `default` deeper in the schema would be published to clients
/// and never applied, so it is refused rather than advertised.
#[test]
fn a_default_below_a_top_level_property_is_refused() {
    let raw: Value = serde_json::from_str(
        r#"{
            "type":"object",
            "properties":{"filter":{
                "type":"object",
                "properties":{"mode":{"type":"string","default":"any"}},
                "required":[],
                "additionalProperties":false
            }},
            "required":["filter"],
            "additionalProperties":false
        }"#,
    )
    .unwrap();
    let error = ParameterSchema::compile_root(&raw, &["filter".to_string()])
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("parameters.properties.filter.properties.mode.default"),
        "{error}"
    );
    assert!(error.contains("top-level"), "{error}");
}

#[test]
fn apply_defaults_fills_only_absent_keys() {
    let schema = compile_with_limit(r#"{"type":["integer","null"],"default":5}"#, r#"["query"]"#)
        .expect("valid schema");

    let mut omitted = serde_json::json!({"query": "wells"})
        .as_object()
        .unwrap()
        .clone();
    schema.apply_defaults(&mut omitted);
    assert_eq!(omitted["limit"], Value::from(5));
    schema.validate_variables(&omitted).expect("defaults bind");

    let mut explicit = serde_json::json!({"query": "wells", "limit": 2})
        .as_object()
        .unwrap()
        .clone();
    schema.apply_defaults(&mut explicit);
    assert_eq!(explicit["limit"], Value::from(2), "an explicit value wins");

    let mut null = serde_json::json!({"query": "wells", "limit": null})
        .as_object()
        .unwrap()
        .clone();
    schema.apply_defaults(&mut null);
    assert_eq!(null["limit"], Value::Null, "an explicit null stays null");
}
