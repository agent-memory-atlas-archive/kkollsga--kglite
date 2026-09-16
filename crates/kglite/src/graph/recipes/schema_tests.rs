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
