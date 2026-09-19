use super::*;
use serde_json::json;

fn query(parameters: Value, cypher: &str) -> Value {
    json!({
        "description": "A stored read operation.",
        "parameters": parameters,
        "cypher": cypher,
    })
}

fn catalog(query_value: Value) -> Value {
    json!({
        "code_review": {
            "description": "Code review operations.",
            "queries": {"direct_callers": query_value}
        }
    })
}

/// The plain single-query catalogue, with no `tool:` anywhere.
fn catalog_fixture() -> Value {
    catalog(query(
        string_parameter(),
        "MATCH (n) WHERE n.name = $qualified_name RETURN n.name AS name",
    ))
}

fn string_parameter() -> Value {
    json!({
        "type": "object",
        "properties": {"qualified_name": {"type": "string"}},
        "required": ["qualified_name"],
        "additionalProperties": false
    })
}

#[test]
fn absent_and_empty_catalogs_are_disabled() {
    let absent = RecipeCatalog::from_manifest_value(None).unwrap();
    assert!(absent.is_empty());
    assert_eq!(absent.discovery_summary(), None);

    let empty = RecipeCatalog::from_manifest_value(Some(&json!({}))).unwrap();
    assert!(empty.is_empty());
    assert_eq!(empty.discovery_summary(), None);
}

#[test]
fn catalog_is_immutable_and_summarized_deterministically() {
    let raw = catalog(query(
        string_parameter(),
        "MATCH (n:Function) WHERE n.qualified_name = $qualified_name RETURN n.name",
    ));
    let parsed = RecipeCatalog::from_manifest_value(Some(&raw)).unwrap();
    assert_eq!(
        parsed.discovery_summary(),
        Some(CatalogSummary {
            recipe_count: 1,
            query_count: 1
        })
    );
    let recipe = parsed.get("code_review").unwrap();
    assert_eq!(recipe.name, "code_review");
    assert_eq!(recipe.queries().len(), 1);
    assert_eq!(recipe.get("direct_callers").unwrap().name, "direct_callers");
    assert_eq!(parsed.recipes().len(), 1);
}

#[test]
fn rejects_invalid_identifiers_empty_fields_and_unknown_config() {
    let invalid_identifier = json!({
        "bad-name": {
            "description": "Recipe.",
            "queries": {"q": query(json!({
                "type": "object", "properties": {}, "required": [],
                "additionalProperties": false
            }), "RETURN 1")}
        }
    });
    assert!(
        RecipeCatalog::from_manifest_value(Some(&invalid_identifier))
            .unwrap_err()
            .to_string()
            .contains("identifier")
    );

    let empty = json!({"r": {"description": " ", "queries": {}}});
    let error = RecipeCatalog::from_manifest_value(Some(&empty)).unwrap_err();
    assert!(format!("{error:#}").contains("description"));

    let unknown = json!({"r": {"description": "R", "queries": {}, "workflow": []}});
    let error = RecipeCatalog::from_manifest_value(Some(&unknown)).unwrap_err();
    assert!(format!("{error:#}").contains("unsupported recipe keys"));
}

#[test]
fn requires_exact_parameter_property_and_required_sets() {
    let missing_property = catalog(query(
        json!({
            "type": "object", "properties": {}, "required": [],
            "additionalProperties": false
        }),
        "RETURN $qualified_name",
    ));
    let error = RecipeCatalog::from_manifest_value(Some(&missing_property)).unwrap_err();
    assert!(format!("{error:#}").contains("parameter properties"));

    let optional_property = catalog(query(
        json!({
            "type": "object",
            "properties": {"qualified_name": {"type": "string"}},
            "required": [],
            "additionalProperties": false
        }),
        "RETURN $qualified_name",
    ));
    let error = RecipeCatalog::from_manifest_value(Some(&optional_property)).unwrap_err();
    assert!(format!("{error:#}").contains("required must list every"));
}

#[test]
fn tokenizer_ignores_parameter_lookalikes_in_strings_and_comments() {
    let raw = catalog(query(
        string_parameter(),
        "// $comment\nRETURN '$literal' AS text, $qualified_name AS name",
    ));
    RecipeCatalog::from_manifest_value(Some(&raw)).unwrap();
}

#[test]
fn rejects_mutations_and_banned_read_modes() {
    let no_parameters = json!({
        "type": "object", "properties": {}, "required": [],
        "additionalProperties": false
    });
    let cases = [
        ("CREATE (:Thing)", "read-only"),
        ("EXPLAIN RETURN 1", "EXPLAIN"),
        ("PROFILE RETURN 1", "PROFILE"),
        ("RETURN 1 FORMAT CSV", "FORMAT CSV"),
        ("LOAD CSV FROM 'rows.csv' AS row RETURN row", "LOAD CSV"),
    ];
    for (cypher, expected) in cases {
        let raw = catalog(query(no_parameters.clone(), cypher));
        let error = RecipeCatalog::from_manifest_value(Some(&raw)).unwrap_err();
        let chain = format!("{error:#}");
        assert!(chain.contains(expected), "{cypher:?}: {chain}");
    }
}

#[test]
fn third_party_queries_without_order_by_remain_valid() {
    let raw = catalog(query(
        string_parameter(),
        "MATCH (n) WHERE n.name = $qualified_name RETURN n.name",
    ));
    RecipeCatalog::from_manifest_value(Some(&raw)).unwrap();
}

#[test]
fn payload_cap_limit_is_rejected_but_other_semantic_limits_are_valid() {
    let no_parameters = json!({
        "type": "object", "properties": {}, "required": [],
        "additionalProperties": false
    });
    let cap = catalog(query(no_parameters.clone(), "RETURN 1 LIMIT 200"));
    let error = RecipeCatalog::from_manifest_value(Some(&cap)).unwrap_err();
    assert!(format!("{error:#}").contains("reserved for the recipe result payload cap"));

    let semantic = catalog(query(no_parameters.clone(), "RETURN 1 LIMIT 20"));
    RecipeCatalog::from_manifest_value(Some(&semantic)).unwrap();

    // The rule is equality with the cap, not a ceiling: a query asking for
    // more than the cap is accepted and the server reports the overflow,
    // which is the half VAULT.md §8 now spells out because the P16 probe
    // read the refusal of 200 as "no LIMIT may exceed 200" and rewrote its
    // queries.
    let above = catalog(query(no_parameters, "RETURN 1 LIMIT 201"));
    RecipeCatalog::from_manifest_value(Some(&above)).unwrap();
}

/// `tool:` is the seventh catalogue key: optional, and the name the MCP
/// server registers the query under.
#[test]
fn a_query_may_declare_the_tool_name_it_is_served_under() {
    let mut raw = query(
        string_parameter(),
        "MATCH (n) WHERE n.name = $qualified_name RETURN n.name AS name",
    );
    raw["tool"] = json!("find_callers");
    let catalog = RecipeCatalog::from_manifest_value(Some(&catalog(raw))).unwrap();
    let query = catalog
        .get("code_review")
        .and_then(|recipe| recipe.get("direct_callers"))
        .unwrap();
    assert_eq!(query.tool.as_deref(), Some("find_callers"));

    let plain = RecipeCatalog::from_manifest_value(Some(&catalog_fixture())).unwrap();
    assert_eq!(
        plain
            .get("code_review")
            .and_then(|recipe| recipe.get("direct_callers"))
            .unwrap()
            .tool,
        None
    );
}

/// Two queries under one name would leave the router serving whichever
/// registered last, silently. The catalogue refuses to build instead, naming
/// both claimants.
#[test]
fn two_queries_cannot_claim_one_tool_name() {
    let cypher = "MATCH (n) WHERE n.name = $qualified_name RETURN n.name AS name";
    let mut first = query(string_parameter(), cypher);
    first["tool"] = json!("find_callers");
    let mut second = query(string_parameter(), cypher);
    second["tool"] = json!("find_callers");
    let error = RecipeCatalog::from_manifest_value(Some(&json!({
        "code_review": {
            "description": "Code review operations.",
            "queries": {"direct_callers": first, "indirect_callers": second}
        }
    })))
    .expect_err("one tool name, two queries");
    let message = error.to_string();
    assert!(message.contains("find_callers"), "{message}");
    assert!(
        message.contains("code_review.direct_callers")
            && message.contains("code_review.indirect_callers"),
        "both claimants must be named: {message}"
    );
}

#[test]
fn a_catalogue_tool_name_is_held_to_the_mcp_token_rule() {
    let cypher = "MATCH (n) WHERE n.name = $qualified_name RETURN n.name AS name";
    for bad in json!(["", "two words", "a.b", "9lives"])
        .as_array()
        .unwrap()
    {
        let mut raw = query(string_parameter(), cypher);
        raw["tool"] = bad.clone();
        assert!(
            RecipeCatalog::from_manifest_value(Some(&catalog(raw))).is_err(),
            "{bad} must not be a tool name"
        );
    }
}
