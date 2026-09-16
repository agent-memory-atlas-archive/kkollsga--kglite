//! Unit tests for graph-carried recipe records.

use super::super::merge;
use super::*;
use crate::graph::storage::mode::{new_dir_graph_in_mode, StorageMode};

fn graph() -> DirGraph {
    new_dir_graph_in_mode(StorageMode::Memory, None).expect("create graph")
}

fn schema(properties: Json, required: Json) -> Json {
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn no_parameters() -> Json {
    schema(serde_json::json!({}), serde_json::json!([]))
}

fn record(recipe: &str, name: &str) -> RecipeRecord {
    RecipeRecord {
        recipe: recipe.to_string(),
        name: name.to_string(),
        description: format!("what {name} answers"),
        parameters: schema(
            serde_json::json!({"qualified_name": {"type": "string"}}),
            serde_json::json!(["qualified_name"]),
        ),
        cypher: "MATCH (n) WHERE n.name = $qualified_name RETURN n.name".to_string(),
        recipe_description: format!("the {recipe} group"),
    }
}

/// The label is reserved in `schema::SYSTEM_LABELS`; a rename on either side
/// would leave recipe nodes visible in every type enumeration.
#[test]
fn the_recipe_label_is_a_reserved_system_label() {
    assert!(crate::graph::schema::is_system_label(RECIPE_LABEL));
}

// ── Validation ─────────────────────────────────────────────────────────────

#[test]
fn identifiers_must_be_catalogue_tokens() {
    for bad in ["", "bad-name", "two words", "9lives", "a.b"] {
        let mut r = record("code_review", "callers");
        r.recipe = bad.to_string();
        assert!(validate(&r).is_err(), "{bad:?} must not be a recipe id");

        let mut r = record("code_review", "callers");
        r.name = bad.to_string();
        assert!(validate(&r).is_err(), "{bad:?} must not be a query id");
    }
}

#[test]
fn every_text_field_must_be_non_empty() {
    for blank in ["description", "recipe_description", "cypher"] {
        let mut r = record("code_review", "callers");
        match blank {
            "description" => r.description = "  ".to_string(),
            "recipe_description" => r.recipe_description = String::new(),
            _ => r.cypher = " ".to_string(),
        }
        assert!(
            matches!(validate(&r), Err(KgError::InvalidArgument { ref argument, .. }) if argument == blank),
            "{blank} must be refused when empty"
        );
    }
}

/// A record that would be skipped at boot has to be refused where it is
/// written, so every catalogue rule is re-applied here — one mutation each.
#[test]
fn catalogue_rules_apply_to_a_record_exactly_as_they_do_at_boot() {
    let cases: [(&str, Json, &str, &str); 6] = [
        (
            "a mutation",
            no_parameters(),
            "CREATE (:Thing)",
            "read-only",
        ),
        ("EXPLAIN", no_parameters(), "EXPLAIN RETURN 1", "EXPLAIN"),
        ("PROFILE", no_parameters(), "PROFILE RETURN 1", "PROFILE"),
        (
            "the reserved payload cap",
            no_parameters(),
            "RETURN 1 LIMIT 200",
            "reserved for the recipe result payload cap",
        ),
        (
            "a $param absent from the schema",
            no_parameters(),
            "RETURN $missing_one",
            "parameter properties",
        ),
        (
            "an unsupported schema keyword",
            schema(
                serde_json::json!({"qualified_name": {"type": "string", "pattern": "x"}}),
                serde_json::json!(["qualified_name"]),
            ),
            "RETURN $qualified_name",
            "unsupported JSON Schema keywords",
        ),
    ];
    for (label, parameters, cypher, expected) in cases {
        let mut r = record("code_review", "callers");
        r.parameters = parameters;
        r.cypher = cypher.to_string();
        let error = validate(&r).expect_err(label).to_string();
        assert!(error.contains(expected), "{label}: {error}");
    }

    // The same query shape with none of those faults is accepted.
    let mut good = record("code_review", "callers");
    good.parameters = no_parameters();
    good.cypher = "RETURN 1 LIMIT 20".to_string();
    validate(&good).expect("a clean record validates");
}

// ── CRUD ───────────────────────────────────────────────────────────────────

#[test]
fn set_reports_created_then_updated_and_get_reads_every_field_back() {
    let mut g = graph();
    let mut r = record("code_review", "callers");
    assert_eq!(set(&mut g, &r).expect("create"), SetOutcome::Created);
    assert_eq!(get(&g, "code_review", "callers").unwrap(), r);

    r.description = "who calls it".to_string();
    assert_eq!(set(&mut g, &r).expect("update"), SetOutcome::Updated);
    assert_eq!(get(&g, "code_review", "callers").unwrap(), r);
    assert_eq!(list(&g).len(), 1, "an update must not add a second node");
}

#[test]
fn the_key_is_the_pair_not_either_half() {
    let mut g = graph();
    set(&mut g, &record("code_review", "callers")).unwrap();
    set(&mut g, &record("code_review", "tests")).unwrap();
    set(&mut g, &record("ontology", "callers")).unwrap();
    assert_eq!(list(&g).len(), 3);
}

#[test]
fn list_is_sorted_by_recipe_then_name() {
    let mut g = graph();
    for (recipe, name) in [
        ("ontology", "labels"),
        ("code_review", "tests"),
        ("code_review", "callers"),
    ] {
        set(&mut g, &record(recipe, name)).unwrap();
    }
    let keys: Vec<(String, String)> = list(&g).into_iter().map(|r| (r.recipe, r.name)).collect();
    assert_eq!(
        keys,
        [
            ("code_review".to_string(), "callers".to_string()),
            ("code_review".to_string(), "tests".to_string()),
            ("ontology".to_string(), "labels".to_string()),
        ]
    );
}

#[test]
fn unknown_keys_are_not_found_and_delete_reports_nothing_removed() {
    let mut g = graph();
    set(&mut g, &record("code_review", "callers")).unwrap();
    assert!(matches!(
        get(&g, "code_review", "absent"),
        Err(KgError::NodeNotFound { .. })
    ));
    assert!(matches!(
        get(&g, "absent", "callers"),
        Err(KgError::NodeNotFound { .. })
    ));
    assert!(!delete(&mut g, "code_review", "absent").unwrap());
    assert!(delete(&mut g, "code_review", "callers").unwrap());
    assert!(list(&g).is_empty());
}

#[test]
fn a_read_only_graph_refuses_both_writes() {
    let mut g = graph();
    set(&mut g, &record("code_review", "callers")).unwrap();
    g.read_only = true;
    assert!(set(&mut g, &record("code_review", "other")).is_err());
    assert!(delete(&mut g, "code_review", "callers").is_err());
    g.read_only = false;
    assert_eq!(list(&g).len(), 1, "nothing was written while read-only");
}

// ── D14: the parameters representation ─────────────────────────────────────

/// The evidence behind storing `parameters` as a nested map rather than as an
/// encoded string. A schema whose meaning lives in nesting, in mixed enum
/// types, in a non-alphabetical `required` and in the difference between an
/// integer and a float bound has to come back byte-equal from every storage
/// mode and from a `.kgl` file.
#[test]
fn parameters_survive_every_storage_mode_and_a_kgl_round_trip() {
    let rich = schema(
        serde_json::json!({
            "qualified_name": {"type": "string"},
            "filters": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "properties": {
                        "score": {"type": "integer", "minimum": 0, "maximum": 10},
                        "kind": {"type": ["string", "integer", "null"], "enum": ["unit", 3, null]},
                        "weight": {"type": "number", "minimum": -1e300, "maximum": 1.5},
                        "exact": {"type": "number", "minimum": 1, "maximum": 1.0}
                    },
                    "required": ["score", "kind", "weight", "exact"],
                    "additionalProperties": false
                }
            }
        }),
        serde_json::json!(["qualified_name", "filters"]),
    );

    for parameters in [rich, no_parameters()] {
        for mode in [StorageMode::Memory, StorageMode::Mapped, StorageMode::Disk] {
            let dir = tempfile::tempdir().unwrap();
            let mut g =
                new_dir_graph_in_mode(mode, Some(&dir.path().join("store"))).expect("create graph");

            let mut r = record("code_review", "callers");
            r.parameters = parameters.clone();
            r.cypher = if parameters == no_parameters() {
                "RETURN 1".to_string()
            } else {
                "RETURN $qualified_name, $filters".to_string()
            };
            set(&mut g, &r).unwrap_or_else(|error| panic!("{mode:?}: {error}"));

            let path = dir.path().join("recipes.kgl");
            let mut shared = std::sync::Arc::new(g);
            crate::graph::io::file::save_graph(&mut shared, path.to_str().unwrap()).expect("save");
            let reloaded = crate::graph::io::file::load_file(path.to_str().unwrap()).expect("load");

            let stored = get(&reloaded, "code_review", "callers").expect("read back");
            assert_eq!(
                stored.parameters, r.parameters,
                "{mode:?} lost part of the parameter schema"
            );
            assert_eq!(stored, r, "{mode:?} lost part of the record");
        }
    }
}

// ── Catalogue ──────────────────────────────────────────────────────────────

#[test]
fn catalogue_from_graph_skips_an_invalid_record_and_names_it() {
    let mut g = graph();
    set(&mut g, &record("code_review", "callers")).unwrap();

    // Bypass `set`'s validation the way a hand-written CREATE would.
    let mut params: HashMap<String, Value> = HashMap::new();
    params.insert("p".to_string(), no_parameters_value());
    execute_mut(
        &mut g,
        "CREATE (r:KgliteRecipe {recipe: 'code_review', name: 'broken', \
         description: 'd', recipe_description: 'g', cypher: 'CREATE (:Thing)', \
         parameters: $p})",
        &recipe_opts(&params),
    )
    .expect("hand-written node");

    let (catalogue, warnings) = catalogue_from_graph(&g);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].recipe, "code_review");
    assert_eq!(warnings[0].name, "broken");
    assert!(warnings[0].reason.contains("read-only"), "{warnings:?}");
    assert!(warnings[0].to_string().contains("broken"));

    let group = catalogue
        .get("code_review")
        .expect("valid sibling survives");
    assert_eq!(group.queries().len(), 1);
    assert!(group.get("callers").is_some());
}

fn no_parameters_value() -> Value {
    crate::param::json_value_to_kglite_value(&no_parameters())
}

#[test]
fn a_groups_description_is_the_first_records_in_name_order() {
    let mut g = graph();
    let mut second = record("code_review", "zeta");
    second.recipe_description = "written second, sorts last".to_string();
    set(&mut g, &second).unwrap();
    let mut first = record("code_review", "alpha");
    first.recipe_description = "written second, sorts first".to_string();
    set(&mut g, &first).unwrap();

    let (catalogue, warnings) = catalogue_from_graph(&g);
    assert!(warnings.is_empty());
    assert_eq!(
        catalogue.get("code_review").unwrap().description,
        "written second, sorts first"
    );
}

#[test]
fn an_empty_graph_makes_an_empty_catalogue() {
    let (catalogue, warnings) = catalogue_from_graph(&graph());
    assert!(catalogue.is_empty());
    assert_eq!(catalogue.discovery_summary(), None);
    assert!(warnings.is_empty());
}

// ── Merge (D16) ────────────────────────────────────────────────────────────

fn catalogue_of(raw: serde_json::Value) -> RecipeCatalog {
    RecipeCatalog::from_manifest_value(Some(&raw)).expect("valid catalogue")
}

fn query_json(description: &str) -> serde_json::Value {
    serde_json::json!({
        "description": description,
        "parameters": no_parameters(),
        "cypher": "RETURN 1",
    })
}

#[test]
fn a_manifest_wins_per_key_while_graph_only_entries_are_kept() {
    let from_graph = catalogue_of(serde_json::json!({
        "code_review": {
            "description": "graph group text",
            "queries": {
                "callers": query_json("graph callers"),
                "graph_only": query_json("only in the graph"),
            }
        },
        "ontology": {
            "description": "graph-only group",
            "queries": {"labels": query_json("graph labels")}
        }
    }));
    let from_manifest = catalogue_of(serde_json::json!({
        "code_review": {
            "description": "manifest group text",
            "queries": {
                "callers": query_json("manifest callers"),
                "manifest_only": query_json("only in the manifest"),
            }
        },
        "search": {
            "description": "manifest-only group",
            "queries": {"docs": query_json("manifest docs")}
        }
    }));

    let merged = merge(from_graph, from_manifest);
    let review = merged.get("code_review").expect("merged group");
    assert_eq!(review.description, "manifest group text");
    assert_eq!(
        review.get("callers").unwrap().description,
        "manifest callers",
        "the manifest wins per (recipe, name)"
    );
    assert!(review.get("graph_only").is_some(), "graph-only query kept");
    assert!(review.get("manifest_only").is_some());
    assert_eq!(
        merged.get("ontology").unwrap().description,
        "graph-only group"
    );
    assert!(merged.get("search").is_some());
    assert_eq!(merged.summary().query_count, 5);

    let names: Vec<&str> = merged.recipes().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        ["code_review", "ontology", "search"],
        "deterministic"
    );
}

// ── Import / export ────────────────────────────────────────────────────────

fn catalogue_document() -> serde_json::Value {
    serde_json::json!({
        "code_review": {
            "description": "Code review operations.",
            "queries": {
                "direct_callers": {
                    "description": "Functions that call the target directly.",
                    "parameters": schema(
                        serde_json::json!({"qualified_name": {"type": "string"}}),
                        serde_json::json!(["qualified_name"]),
                    ),
                    "cypher": "MATCH (caller:Function)-[:CALLS]->(target:Function) \
                               WHERE target.qualified_name = $qualified_name \
                               RETURN caller.qualified_name ORDER BY caller.qualified_name",
                },
                "counts": {
                    "description": "How many functions there are.",
                    "parameters": no_parameters(),
                    "cypher": "MATCH (f:Function) RETURN count(f) AS total",
                }
            }
        }
    })
}

#[test]
fn a_catalogue_document_imports_and_exports_to_the_same_value() {
    let mut g = graph();
    let written = import_value(&mut g, &catalogue_document()).expect("import");
    assert_eq!(
        written,
        [
            ("code_review".to_string(), "counts".to_string()),
            ("code_review".to_string(), "direct_callers".to_string()),
        ]
    );
    assert_eq!(export_value(&g), catalogue_document());
}

#[test]
fn a_manifest_shaped_document_is_read_from_its_extensions_key() {
    let mut g = graph();
    let manifest = serde_json::json!({
        "name": "local",
        "extensions": {"cypher_recipes": catalogue_document()},
    });
    import_value(&mut g, &manifest).expect("import");
    assert_eq!(export_value(&g), catalogue_document());
}

/// The whole document compiles before anything is written, so a fault in a
/// group that sorts *after* a clean one still leaves the graph untouched — a
/// per-group or per-query import would have written the clean half by then.
#[test]
fn an_invalid_document_writes_nothing() {
    let mut g = graph();
    let mut broken = catalogue_document();
    broken["ontology"] = serde_json::json!({
        "description": "Ontology operations.",
        "queries": {"labels": query_json("labels")},
    });
    broken["ontology"]["queries"]["labels"]["cypher"] = serde_json::json!("CREATE (:Thing)");

    let error = import_value(&mut g, &broken).expect_err("must refuse");
    assert!(error.to_string().contains("read-only"), "{error}");
    assert!(
        error.to_string().contains("labels"),
        "the message must name the offending query: {error}"
    );
    assert!(list(&g).is_empty(), "the graph must be untouched");
}

#[test]
fn import_and_export_round_trip_through_json_files() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("catalogue.json");
    std::fs::write(
        &source,
        serde_json::to_string_pretty(&catalogue_document()).unwrap(),
    )
    .unwrap();

    let mut g = graph();
    import_path(&mut g, &source).expect("import");
    let written = dir.path().join("exported.json");
    export_path(&g, &written).expect("export");

    let mut round_tripped = graph();
    import_path(&mut round_tripped, &written).expect("re-import");
    assert_eq!(export_value(&round_tripped), catalogue_document());
}

/// Core links no general YAML reader — `okf`'s frontmatter helper flattens
/// nested mappings and re-types numbers — so a `.yaml` catalogue is refused
/// here rather than read approximately.
#[test]
fn a_yaml_path_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalogue.yaml");
    std::fs::write(&path, "code_review: {}\n").unwrap();
    let error = import_path(&mut graph(), &path).expect_err("must refuse");
    assert!(error.to_string().contains("JSON only"), "{error}");
}
