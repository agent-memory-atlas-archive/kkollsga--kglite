//! `valid_at` on `run_recipe_query` and the named recipe tools: the stored
//! query runs behind the core helper's `FOR VALID_TIME AS OF` prefix.

use std::sync::Arc;

use kglite::api::storage::StorageMode;
use mcp_methods::server::McpServer;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::ServiceExt;
use serde_json::{json, Map, Value};

use super::wire::RunRecipeQueryArgs;
use super::{register_recipe_query_routes, run_recipe_query, RecipeCatalog, RecipeRouteOptions};
use crate::tools::GraphState;

const WELLS: &str = "MATCH (w:Well) RETURN w.id AS id ORDER BY id";
const OWN_CONTEXT: &str =
    "FOR VALID_TIME AS OF date('2001-01-01') MATCH (w:Well) RETURN w.id AS id";

/// Well 1 (2000–2010, closed) and well 2 (from 2005), declared on `vf`/`vt`.
fn wells_state(dir: &std::path::Path) -> GraphState {
    let state = GraphState::new(None);
    state
        .create_in_mode(&dir.join("wells.kgl"), StorageMode::Memory)
        .expect("activate graph");
    state
        .with_active_mut(|active| {
            let graph = kglite::api::make_dir_graph_mut(active.kg.dir_mut());
            let params = std::collections::HashMap::new();
            let options = kglite::api::session::ExecuteOptions::eager(&params);
            for statement in [
                "CREATE (:Well {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}), \
                 (:Well {id: 2, vf: date('2005-01-01')})",
                "CALL db.temporal.declare({node: 'Well', from: 'vf', to: 'vt', \
                 convention: 'closed'}) YIELD declared RETURN declared",
            ] {
                kglite::api::session::execute_mut(graph, statement, &options)
                    .unwrap_or_else(|error| panic!("{statement}: {error}"));
            }
        })
        .expect("active graph");
    state
}

fn catalogue() -> Arc<RecipeCatalog> {
    let no_parameters = json!({
        "type": "object", "properties": {}, "required": [], "additionalProperties": false
    });
    Arc::new(
        RecipeCatalog::from_manifest_value(Some(&json!({
            "wells": {
                "description": "Well lookups.",
                "queries": {
                    "list": {
                        "description": "Every well.",
                        "parameters": no_parameters,
                        "cypher": WELLS,
                        "tool": "list_wells"
                    },
                    "pinned": {
                        "description": "Wells as of 2001.",
                        "parameters": no_parameters,
                        "cypher": OWN_CONTEXT
                    }
                }
            }
        })))
        .expect("valid catalogue"),
    )
}

fn args(query: &str, valid_at: Option<&str>) -> RunRecipeQueryArgs {
    RunRecipeQueryArgs {
        recipe: "wells".into(),
        query: query.into(),
        variables: Map::new(),
        include_cypher: true,
        valid_at: valid_at.map(str::to_string),
    }
}

fn run(state: &GraphState, args: RunRecipeQueryArgs) -> Value {
    serde_json::to_value(run_recipe_query(state, &catalogue(), args)).unwrap()
}

#[test]
fn run_recipe_query_answers_as_of_valid_at_and_echoes_the_instant() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = wells_state(temp.path());

    let all = run(&state, args("list", None));
    assert_eq!(all["result"]["rows"], json!([[1], [2]]));
    assert_eq!(all["cypher"], WELLS);
    assert!(all["result"]["diagnostics"].get("temporal").is_none());

    let as_of = run(&state, args("list", Some("2003-06-30")));
    assert_eq!(as_of["result"]["rows"], json!([[1]]));
    assert_eq!(
        as_of["cypher"],
        format!("FOR VALID_TIME AS OF date('2003-06-30') {WELLS}"),
        "include_cypher reports the text that ran"
    );
    let echo = &as_of["result"]["diagnostics"]["temporal"];
    assert_eq!(echo["instant"], "2003-06-30");
    assert_eq!(echo["route"], "guarded");
    assert_eq!(echo["targets"], json!(["(:Well)"]));
}

#[test]
fn a_doubled_or_unreadable_instant_is_a_query_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = wells_state(temp.path());
    for (query, valid_at, expected) in [
        ("pinned", "2003-06-30", "already has a FOR"),
        ("list", "last tuesday", "valid_at"),
    ] {
        let error = run(&state, args(query, Some(valid_at)));
        assert_eq!(error["code"], "query_failed", "{error}");
        let message = error["details"]["cause"]["message"].as_str().unwrap();
        assert!(message.contains(expected), "{message}");
        assert_eq!(error["details"]["cause"]["kglite_code"], "InvalidArgument");
    }
}

fn structured(result: &CallToolResult) -> Value {
    result
        .structured_content
        .clone()
        .expect("structured content")
}

#[tokio::test]
async fn a_named_recipe_tool_takes_valid_at_beside_its_variables() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = wells_state(temp.path());
    let mut server = McpServer::new(Default::default());
    register_recipe_query_routes(
        &mut server,
        state,
        catalogue(),
        &RecipeRouteOptions::default(),
    )
    .unwrap();
    let schema = server
        .tool_router_mut()
        .get("list_wells")
        .expect("named route")
        .input_schema
        .clone();
    assert_eq!(schema["properties"]["valid_at"]["type"], "string");
    assert_eq!(schema["additionalProperties"], json!(false));

    let (server_transport, client_transport) = tokio::io::duplex(16 * 1024);
    let server_handle = tokio::spawn(async move { server.serve(server_transport).await });
    let client = ().serve(client_transport).await.expect("start MCP client");
    let call = |arguments: Value| {
        CallToolRequestParams::new("list_wells")
            .with_arguments(arguments.as_object().unwrap().clone())
    };
    let all = client.call_tool(call(json!({}))).await.unwrap();
    assert_eq!(structured(&all)["result"]["rows"], json!([[1], [2]]));
    let as_of = client
        .call_tool(call(json!({"valid_at": "2003-06-30"})))
        .await
        .unwrap();
    assert_eq!(as_of.is_error, Some(false));
    let as_of = structured(&as_of);
    assert_eq!(as_of["result"]["rows"], json!([[1]]));
    assert_eq!(
        as_of["result"]["diagnostics"]["temporal"]["instant"],
        "2003-06-30"
    );
    let bad = client
        .call_tool(call(json!({"valid_at": "someday"})))
        .await
        .unwrap();
    assert_eq!(bad.is_error, Some(true));
    assert_eq!(structured(&bad)["code"], "query_failed");

    client.cancel().await.expect("stop MCP client");
    server_handle.abort();
}
