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
    wells_state_with(dir, None)
}

/// [`wells_state`] on a server configured with a valid-time default.
fn wells_state_with(
    dir: &std::path::Path,
    default: Option<kglite::api::temporal::ValidTimeDefault>,
) -> GraphState {
    let state = GraphState::new(None).with_valid_time_default(default);
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

    // Without `valid_at` the graph's declarations default the statement to
    // today: well 1 closed in 2005.
    let current = run(&state, args("list", None));
    assert_eq!(current["result"]["rows"], json!([[2]]));
    assert_eq!(current["cypher"], WELLS);
    assert_eq!(
        current["result"]["diagnostics"]["temporal"]["source"],
        "default"
    );

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
fn valid_at_all_reads_every_version_and_echoes_the_source_all() {
    use kglite::api::temporal::ValidTimeDefault;
    for default in [None, Some(ValidTimeDefault::parse("2003-06-30").unwrap())] {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = wells_state_with(temp.path(), default);
        let all = run(&state, args("list", Some("all")));
        assert_eq!(all["result"]["rows"], json!([[1], [2]]), "{default:?}");
        assert_eq!(
            all["cypher"],
            format!("FOR VALID_TIME ALL {WELLS}"),
            "include_cypher reports the text that ran"
        );
        let echo = &all["result"]["diagnostics"]["temporal"];
        assert_eq!(echo["source"], "all");
        assert_eq!(echo["instant"], "all");
    }
}

#[test]
fn the_configured_default_governs_unprefixed_recipe_runs() {
    use kglite::api::temporal::ValidTimeDefault;
    let (temp, other) = (
        tempfile::tempdir().expect("tempdir"),
        tempfile::tempdir().expect("tempdir"),
    );
    let all = wells_state_with(temp.path(), Some(ValidTimeDefault::All));
    let rows = run(&all, args("list", None));
    assert_eq!(rows["result"]["rows"], json!([[1], [2]]));
    let echo = &rows["result"]["diagnostics"]["temporal"];
    assert_eq!(
        (&echo["source"], &echo["instant"]),
        (&json!("default"), &json!("all"))
    );

    let pinned = wells_state_with(other.path(), ValidTimeDefault::parse("2003-06-30").ok());
    let rows = run(&pinned, args("list", None));
    assert_eq!(rows["result"]["rows"], json!([[1]]));
    let echo = &rows["result"]["diagnostics"]["temporal"];
    assert_eq!(
        (&echo["source"], &echo["instant"]),
        (&json!("default"), &json!("2003-06-30"))
    );
    // A call's own valid_at still wins over the server's default.
    let own = run(&pinned, args("list", Some("2008-01-01")));
    assert_eq!(own["result"]["rows"], json!([[1], [2]]));
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
    let current = client.call_tool(call(json!({}))).await.unwrap();
    assert_eq!(structured(&current)["result"]["rows"], json!([[2]]));
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
