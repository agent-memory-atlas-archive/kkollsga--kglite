//! Catalogue tests that need the manifest loader.
//!
//! The catalogue model itself lives in `kglite::api::recipes` and is tested
//! there. These two cases are about the YAML that reaches it: duplicate-key
//! rejection belongs to the manifest loader, and the canonical example is a
//! `.yaml` file this crate ships as its own documented contract.

use std::io::Write;

use serde_json::Value;

use super::RecipeCatalog;

#[test]
fn manifest_loader_rejects_duplicate_yaml_recipe_and_query_keys() {
    let cases = [
        r#"
extensions:
  cypher_recipes:
    review: {description: First, queries: {}}
    review: {description: Second, queries: {}}
"#,
        r#"
extensions:
  cypher_recipes:
    review:
      description: Review.
      queries:
        lookup: {description: First, parameters: {}, cypher: RETURN 1}
        lookup: {description: Second, parameters: {}, cypher: RETURN 2}
"#,
    ];
    for yaml in cases {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();
        let error = mcp_methods::server::load_manifest(file.path()).unwrap_err();
        assert!(error.to_string().contains("duplicate entry"), "{error}");
    }
}

#[test]
fn canonical_code_review_example_is_a_valid_exact_catalog() {
    let manifest_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/local_code_review_mcp.yaml");
    let manifest = mcp_methods::server::load_manifest(&manifest_path)
        .unwrap_or_else(|error| panic!("{}: {error}", manifest_path.display()));
    let catalog = RecipeCatalog::from_manifest_value(manifest.extensions.get("cypher_recipes"))
        .expect("canonical recipe catalog must pass the production parser");
    let recipe = catalog.get("code_review").expect("code_review recipe");
    let names = recipe
        .queries()
        .map(|query| query.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["affected_tests", "direct_callers", "resolve_function"]
    );

    for query in recipe.queries() {
        assert!(query.cypher.contains("ORDER BY"), "{}", query.name);
        assert!(!query.cypher.contains("LIMIT 200"), "{}", query.name);
        assert_eq!(
            query.parameters.as_json().get("additionalProperties"),
            Some(&Value::Bool(false)),
            "{}",
            query.name
        );
        assert_eq!(
            query.parameters.as_json().get("required"),
            Some(&serde_json::json!(["qualified_name"])),
            "{}",
            query.name
        );
    }
    assert!(recipe
        .get("direct_callers")
        .unwrap()
        .cypher
        .contains("(caller:Function)-[:CALLS]->(target:Function)"));
    assert!(recipe
        .get("affected_tests")
        .unwrap()
        .cypher
        .contains("(test:Function)-[:CALLS*1..5]->(target:Function)"));
}
