//! Tests for the two recipe layers under the manifest's — the graph's own
//! records and the embedding binary's catalogue — and their merge.

use std::path::Path;
use std::sync::Arc;

use kglite::api::recipes::RecipeRecord;
use kglite::api::storage::StorageMode;
use mcp_methods::server::{McpServer, ServerOptions};
use serde_json::json;

use super::*;
use crate::recipe_queries::{
    register_recipe_query_routes, run_recipe_query, wire::RunRecipeQueryArgs, RecipeRouteOptions,
    LIST_RECIPE_QUERIES_TOOL, RUN_RECIPE_QUERY_TOOL,
};

const NO_PARAMETERS: &str =
    r#"{"type":"object","properties":{},"required":[],"additionalProperties":false}"#;

fn record(recipe: &str, name: &str, cypher: &str) -> RecipeRecord {
    RecipeRecord {
        recipe: recipe.to_string(),
        name: name.to_string(),
        description: format!("Query {name}."),
        parameters: serde_json::from_str(NO_PARAMETERS).expect("the empty closed schema"),
        cypher: cypher.to_string(),
        recipe_description: format!("Group {recipe}."),
        tool: None,
    }
}

/// A live graph carrying the given records. `set` refuses an invalid one, so a
/// record that must reach the boot reader unvalidated is written by `CREATE` —
/// which is exactly how such a node gets into a real graph.
fn state_with(dir: &Path, records: &[RecipeRecord], raw_creates: &[&str]) -> GraphState {
    let state = GraphState::new(None);
    state
        .create_in_mode(&dir.join("recipes.kgl"), StorageMode::Memory)
        .expect("activate graph");
    state
        .with_active_mut(|active| {
            let graph = kglite::api::make_dir_graph_mut(active.kg.dir_mut());
            for record in records {
                kglite::api::recipes::set(graph, record).unwrap_or_else(|error| {
                    panic!("write {}/{}: {error}", record.recipe, record.name)
                });
            }
            for statement in raw_creates {
                let params = std::collections::HashMap::new();
                let options = kglite::api::session::ExecuteOptions::eager(&params);
                kglite::api::session::execute_mut(graph, statement, &options)
                    .unwrap_or_else(|error| panic!("{statement}: {error}"));
            }
        })
        .expect("active graph");
    state
}

fn graph_mode(dir: &Path) -> Mode {
    Mode::Graph {
        path: dir.join("recipes.kgl"),
    }
}

/// The two-layer merge these tests were written against, with the producer
/// layer empty. The assertion is the point: an empty producer layer must
/// contribute nothing, so every graph-vs-manifest expectation below still
/// measures what it always did.
fn merge_graph_recipes(
    mode: &Mode,
    state: &GraphState,
    manifest: RecipeCatalog,
) -> (RecipeCatalog, GraphRecipeStats) {
    let (catalogue, producer, graph) =
        merge_recipe_layers(mode, state, RecipeCatalog::default(), manifest);
    assert_eq!(
        producer,
        ProducerRecipeStats::default(),
        "an absent producer catalogue must contribute nothing"
    );
    (catalogue, graph)
}

fn manifest_catalogue(raw: serde_json::Value) -> RecipeCatalog {
    RecipeCatalog::from_manifest_value(Some(&raw)).expect("a valid manifest catalogue")
}

fn query_cypher<'a>(catalogue: &'a RecipeCatalog, recipe: &str, name: &str) -> &'a str {
    &catalogue
        .get(recipe)
        .unwrap_or_else(|| panic!("recipe {recipe}"))
        .get(name)
        .unwrap_or_else(|| panic!("query {recipe}/{name}"))
        .cypher
}

/// A `.kgl` that carries recipes is served even when the manifest declares
/// none — that is the whole point of the layer, and the routes, the skill and
/// the overview hint all key off the merged catalogue being non-empty.
#[test]
fn a_graph_only_catalogue_registers_the_routes_and_runs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = state_with(
        temp.path(),
        &[record("wells", "count", "RETURN 7 AS answer")],
        &[],
    );

    let (catalogue, stats) =
        merge_graph_recipes(&graph_mode(temp.path()), &state, RecipeCatalog::default());
    assert_eq!(stats.served, 1);
    assert_eq!(stats.overridden, 0);
    assert!(stats.skipped.is_empty(), "{:?}", stats.skipped);
    let summary = catalogue
        .discovery_summary()
        .expect("a non-empty catalogue");
    assert_eq!((summary.recipe_count, summary.query_count), (1, 1));

    let catalogue = Arc::new(catalogue);
    let mut server = McpServer::new(ServerOptions::default());
    let registered = register_recipe_query_routes(
        &mut server,
        state.clone(),
        catalogue.clone(),
        &RecipeRouteOptions::default(),
    )
    .expect("routes");
    assert_eq!(registered, 2);
    let names: Vec<String> = server
        .tool_router_mut()
        .list_all()
        .iter()
        .map(|tool| tool.name.to_string())
        .collect();
    assert!(
        names.iter().any(|name| name == RUN_RECIPE_QUERY_TOOL),
        "{names:?}"
    );
    assert!(
        names.iter().any(|name| name == LIST_RECIPE_QUERIES_TOOL),
        "{names:?}"
    );

    let result = run_recipe_query(
        &state,
        &catalogue,
        RunRecipeQueryArgs {
            recipe: "wells".into(),
            query: "count".into(),
            variables: serde_json::Map::new(),
            include_cypher: false,
        },
    )
    .into_call_tool_result();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let structured = result.structured_content.expect("structured content");
    assert_eq!(structured["result"]["rows"], json!([[7]]));
}

/// The manifest is the surface an operator can edit, so it replaces a
/// same-keyed graph query — and only that one. A sibling the graph alone
/// carries has to survive, or overriding one query would silently delete the
/// rest of its group.
#[test]
fn the_manifest_wins_per_key_while_a_graph_only_query_survives() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = state_with(
        temp.path(),
        &[
            record("wells", "count", "RETURN 'graph' AS source"),
            record("wells", "depth", "RETURN 'graph-only' AS source"),
        ],
        &[],
    );

    let manifest = manifest_catalogue(json!({
        "wells": {
            "description": "Manifest group text.",
            "queries": {
                "count": {
                    "description": "Manifest count.",
                    "parameters": serde_json::from_str::<serde_json::Value>(NO_PARAMETERS).unwrap(),
                    "cypher": "RETURN 'manifest' AS source"
                }
            }
        },
        "areas": {
            "description": "Manifest-only group.",
            "queries": {
                "list": {
                    "description": "Manifest list.",
                    "parameters": serde_json::from_str::<serde_json::Value>(NO_PARAMETERS).unwrap(),
                    "cypher": "RETURN 'manifest' AS source"
                }
            }
        }
    }));

    let (catalogue, stats) = merge_graph_recipes(&graph_mode(temp.path()), &state, manifest);
    assert_eq!(
        stats.served, 1,
        "only `depth` is served as the graph wrote it"
    );
    assert_eq!(stats.overridden, 1);

    assert_eq!(
        query_cypher(&catalogue, "wells", "count"),
        "RETURN 'manifest' AS source"
    );
    assert_eq!(
        query_cypher(&catalogue, "wells", "depth"),
        "RETURN 'graph-only' AS source"
    );
    assert_eq!(
        query_cypher(&catalogue, "areas", "list"),
        "RETURN 'manifest' AS source"
    );

    // The group description follows the same rule as the queries.
    assert_eq!(
        catalogue.get("wells").expect("group").description,
        "Manifest group text."
    );

    let summary = catalogue.discovery_summary().expect("non-empty");
    assert_eq!((summary.recipe_count, summary.query_count), (2, 3));
}

/// A group the manifest does not mention keeps the description the graph gave
/// it — otherwise a manifest that touches one group would blank the others.
#[test]
fn a_graph_only_group_keeps_its_own_description() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = state_with(
        temp.path(),
        &[record("wells", "count", "RETURN 1 AS n")],
        &[],
    );
    let manifest = manifest_catalogue(json!({
        "areas": {
            "description": "Manifest-only group.",
            "queries": {
                "list": {
                    "description": "Manifest list.",
                    "parameters": serde_json::from_str::<serde_json::Value>(NO_PARAMETERS).unwrap(),
                    "cypher": "RETURN 1 AS n"
                }
            }
        }
    }));

    let (catalogue, _) = merge_graph_recipes(&graph_mode(temp.path()), &state, manifest);
    assert_eq!(
        catalogue.get("wells").expect("group").description,
        "Group wells."
    );
}

/// A hand-written `CREATE` bypasses every rule `set` enforces, so the boot
/// reader is the only gate such a node meets. Taking the deployment down for
/// one of them would be a worse trade than serving the rest — the manifest's
/// fail-boot rule exists because an operator is looking at the file, and
/// nobody is looking at the graph.
#[test]
fn an_invalid_record_is_skipped_while_its_sibling_serves() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = state_with(
        temp.path(),
        &[record("wells", "good", "RETURN 1 AS n")],
        &["CREATE (:KgliteRecipe {recipe: 'wells', name: 'broken', \
           description: 'Broken.', parameters: {type: 'object', properties: {}, \
           required: [], additionalProperties: false}, \
           cypher: 'CREATE (:Well {id: 1})', recipe_description: 'Group wells.'})"],
    );

    let (catalogue, stats) =
        merge_graph_recipes(&graph_mode(temp.path()), &state, RecipeCatalog::default());
    assert_eq!(stats.served, 1);
    assert_eq!(stats.skipped.len(), 1, "{:?}", stats.skipped);
    assert!(
        stats.skipped[0].starts_with("wells/broken: "),
        "{:?}",
        stats.skipped
    );
    assert!(
        stats.skipped[0].contains("read-only"),
        "{:?}",
        stats.skipped
    );

    let group = catalogue.get("wells").expect("the group still exists");
    assert!(group.get("good").is_some());
    assert!(group.get("broken").is_none());
}

/// The workspace, source-root and bare modes have no graph when the catalogue
/// is built, and the catalogue is immutable afterwards. Reading nothing there
/// is the honest answer rather than a layer that works in a third of the
/// deployments.
#[test]
fn only_graph_watch_and_vault_modes_contribute_a_layer() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = state_with(
        temp.path(),
        &[record("wells", "count", "RETURN 1 AS n")],
        &[],
    );

    for mode in [
        graph_mode(temp.path()),
        Mode::Watch {
            dir: temp.path().to_path_buf(),
        },
        // `.kglite/recipes/*.md` reaches the graph the same way the vault's
        // skills do, and loses the same way if this arm is missed.
        Mode::Vault {
            dir: temp.path().to_path_buf(),
        },
    ] {
        let (catalogue, stats) = merge_graph_recipes(&mode, &state, RecipeCatalog::default());
        assert_eq!(stats.served, 1, "{mode:?}");
        assert!(!catalogue.is_empty(), "{mode:?}");
    }

    for mode in [
        Mode::Bare,
        Mode::SourceRoot {
            dir: temp.path().to_path_buf(),
        },
        Mode::LocalWorkspace {
            root: temp.path().to_path_buf(),
            watch: false,
        },
        Mode::Workspace {
            dir: temp.path().to_path_buf(),
        },
    ] {
        let (catalogue, stats) = merge_graph_recipes(&mode, &state, RecipeCatalog::default());
        assert!(catalogue.is_empty(), "{mode:?}");
        assert_eq!(stats, GraphRecipeStats::default(), "{mode:?}");
    }
}

#[test]
fn the_boot_summary_names_the_graph_layer_only_when_there_is_one() {
    assert_eq!(GraphRecipeStats::default().summary(), None);

    let plain = GraphRecipeStats {
        served: 3,
        ..GraphRecipeStats::default()
    };
    assert_eq!(plain.summary().as_deref(), Some("graph recipes: 3 served"));

    let full = GraphRecipeStats {
        served: 2,
        overridden: 1,
        skipped: vec!["wells/broken: cypher must be read-only".to_string()],
    };
    let summary = full.summary().expect("summary");
    assert!(summary.starts_with("graph recipes: 2 served"), "{summary}");
    assert!(
        summary.contains("1 overridden by the manifest"),
        "{summary}"
    );
    assert!(
        summary.contains("1 skipped: wells/broken: cypher must be read-only"),
        "{summary}"
    );
}

// ── The producer layer (`ServerExtensions::with_recipes`) ──────────────────

/// A catalogue the embedding binary shipped, in the shape
/// `extensions.cypher_recipes` takes — the exact route
/// `ServerExtensions::with_recipes` documents.
fn producer_catalogue(raw: serde_json::Value) -> RecipeCatalog {
    RecipeCatalog::from_manifest_value(Some(&raw)).expect("a valid producer catalogue")
}

fn one_query(recipe: &str, name: &str, cypher: &str, group_description: &str) -> serde_json::Value {
    json!({
        recipe: {
            "description": group_description,
            "queries": {
                name: {
                    "description": format!("Query {name}."),
                    "parameters": serde_json::from_str::<serde_json::Value>(NO_PARAMETERS).unwrap(),
                    "cypher": cypher
                }
            }
        }
    })
}

/// The deployment the builder exists for: a workspace-mode producer, no
/// manifest and no graph at boot. The graph layer is empty in these modes by
/// design, so the producer's is the only catalogue there is — and it has to be
/// enough to register the routes.
#[test]
fn a_producer_only_catalogue_registers_the_routes_in_the_workspace_modes() {
    let temp = tempfile::tempdir().expect("tempdir");
    // A graph exists in the slot but the mode keeps it out of the catalogue —
    // the same state a workspace server reaches after its first activation.
    let state = state_with(
        temp.path(),
        &[record("wells", "count", "RETURN 'graph' AS source")],
        &[],
    );

    for mode in [
        Mode::LocalWorkspace {
            root: temp.path().to_path_buf(),
            watch: false,
        },
        Mode::Workspace {
            dir: temp.path().to_path_buf(),
        },
    ] {
        let (catalogue, producer, graph) = merge_recipe_layers(
            &mode,
            &state,
            producer_catalogue(one_query(
                "review",
                "hotspots",
                "RETURN 'producer' AS source",
                "Reviewing this builder's graphs.",
            )),
            RecipeCatalog::default(),
        );
        assert_eq!(producer.served, 1, "{mode:?}");
        assert_eq!(producer.overridden, 0, "{mode:?}");
        assert_eq!(graph, GraphRecipeStats::default(), "{mode:?}");
        let summary = catalogue
            .discovery_summary()
            .expect("a producer catalogue is a catalogue");
        assert_eq!(
            (summary.recipe_count, summary.query_count),
            (1, 1),
            "{mode:?}"
        );

        let catalogue = Arc::new(catalogue);
        let mut server = McpServer::new(ServerOptions::default());
        let registered = register_recipe_query_routes(
            &mut server,
            state.clone(),
            catalogue.clone(),
            &RecipeRouteOptions::default(),
        )
        .expect("routes");
        assert_eq!(registered, 2, "{mode:?}");

        // And it runs against whatever graph the server activated later —
        // nothing about the catalogue is tied to the graph it was built beside.
        let result = run_recipe_query(
            &state,
            &catalogue,
            RunRecipeQueryArgs {
                recipe: "review".into(),
                query: "hotspots".into(),
                variables: serde_json::Map::new(),
                include_cypher: false,
            },
        )
        .into_call_tool_result();
        assert_ne!(result.is_error, Some(true), "{mode:?}: {result:?}");
        let structured = result.structured_content.expect("structured content");
        assert_eq!(
            structured["result"]["rows"],
            json!([["producer"]]),
            "{mode:?}"
        );
    }
}

/// **Precedence pin (D6).** `producer < graph < manifest`, per `(recipe, name)`
/// *and* per group description, asserted on one key at a time so each step is
/// an observable difference between two stored statements. Everything only one
/// layer carries survives: overriding a query must never delete its siblings.
#[test]
fn the_three_catalogue_layers_compose_producer_under_graph_under_manifest() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = state_with(
        temp.path(),
        &[
            record("wells", "count", "RETURN 'graph' AS source"),
            record("wells", "depth", "RETURN 'graph-only' AS source"),
        ],
        &[],
    );
    let mut producer = producer_catalogue(one_query(
        "wells",
        "count",
        "RETURN 'producer' AS source",
        "The producer's own wells group.",
    ));
    producer = recipes::merge(
        producer,
        producer_catalogue(one_query(
            "wells",
            "spud",
            "RETURN 'producer-only' AS source",
            "The producer's own wells group.",
        )),
    );
    let manifest = manifest_catalogue(one_query(
        "wells",
        "depth",
        "RETURN 'manifest' AS source",
        "The operator's wells group.",
    ));

    let (catalogue, producer_stats, graph_stats) =
        merge_recipe_layers(&graph_mode(temp.path()), &state, producer, manifest);

    // The graph beats the producer on `count`; the manifest beats the graph on
    // `depth`; `spud` is the producer's alone and survives both.
    assert_eq!(
        query_cypher(&catalogue, "wells", "count"),
        "RETURN 'graph' AS source"
    );
    assert_eq!(
        query_cypher(&catalogue, "wells", "depth"),
        "RETURN 'manifest' AS source"
    );
    assert_eq!(
        query_cypher(&catalogue, "wells", "spud"),
        "RETURN 'producer-only' AS source"
    );
    // Group description: the closest layer that declared the group wins.
    assert_eq!(
        catalogue.get("wells").expect("wells").description,
        "The operator's wells group."
    );

    assert_eq!(producer_stats.served, 1, "only `spud` is served as written");
    assert_eq!(producer_stats.overridden, 1, "`count` lost to the graph");
    assert_eq!(graph_stats.served, 1, "only `count` is served as written");
    assert_eq!(graph_stats.overridden, 1, "`depth` lost to the manifest");

    // The overview hint and the bundled `recipe_queries` skill read this one
    // summary, so it has to count every layer's surviving queries.
    let summary = catalogue.discovery_summary().expect("a merged catalogue");
    assert_eq!((summary.recipe_count, summary.query_count), (1, 3));
}

#[test]
fn the_boot_summary_names_the_producer_layer_only_when_there_is_one() {
    assert_eq!(ProducerRecipeStats::default().summary(), None);

    let plain = ProducerRecipeStats {
        served: 3,
        overridden: 0,
    };
    assert_eq!(
        plain.summary().as_deref(),
        Some("producer recipes: 3 served")
    );

    let overridden = ProducerRecipeStats {
        served: 2,
        overridden: 1,
    };
    let summary = overridden.summary().expect("summary");
    assert!(
        summary.starts_with("producer recipes: 2 served"),
        "{summary}"
    );
    assert!(
        summary.contains("1 overridden by the graph or the manifest"),
        "{summary}"
    );
}
