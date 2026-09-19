//! Fixed MCP route registration for the boot-validated recipe catalog.

use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use mcp_methods::server::McpServer;
use rmcp::handler::server::router::tool::ToolRoute;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{CallToolResponse, Tool, ToolAnnotations};
use rmcp::ErrorData as McpError;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::description::{run_tool_description, schema_enums, VARIABLES_DESCRIPTION};
use super::wire::{
    structured_error_result, ListRecipeQueriesArgs, ListRecipeQueriesOutput, RunRecipeQueryArgs,
    RunRecipeQueryOutput,
};
use super::{list_recipe_queries, run_recipe_query, RecipeCatalog, RecipeErrorEnvelope};
use crate::tools::GraphState;

type DynFut<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

pub(crate) const LIST_RECIPE_QUERIES_TOOL: &str = "list_recipe_queries";
pub(crate) const RUN_RECIPE_QUERY_TOOL: &str = "run_recipe_query";

const LIST_DESCRIPTION: &str = "List the boot-validated Cypher recipe catalog. Omit `recipe` for compact recipe summaries; provide it to inspect that recipe's named queries and parameter schemas. The routing is already in run_recipe_query's description; call this only for a recipe not listed there.";

/// Register the two catalog routes as one ownership unit.
///
/// The router's normal `add_route` operation replaces an existing route with
/// the same name. Preflight both fixed names before adding either so a domain
/// or manifest collision cannot leave a partially registered catalog.
pub(crate) fn register_recipe_query_routes(
    server: &mut McpServer,
    state: GraphState,
    catalog: Arc<RecipeCatalog>,
) -> Result<usize> {
    if catalog.is_empty() {
        return Ok(0);
    }

    let collisions = [LIST_RECIPE_QUERIES_TOOL, RUN_RECIPE_QUERY_TOOL]
        .into_iter()
        .filter(|name| server.tool_router_mut().map.contains_key(*name))
        .collect::<Vec<_>>();
    if !collisions.is_empty() {
        anyhow::bail!(
            "Cypher recipe routes conflict with already-registered tool(s): {}",
            collisions.join(", ")
        );
    }

    let list_catalog = catalog.clone();
    server.tool_router_mut().add_route(ToolRoute::new_dyn(
        recipe_tool::<ListRecipeQueriesArgs, ListRecipeQueriesOutput>(
            LIST_RECIPE_QUERIES_TOOL,
            LIST_DESCRIPTION.to_string(),
        ),
        move |ctx: ToolCallContext<'_, McpServer>| -> DynFut<'_, Result<CallToolResponse, McpError>> {
            let catalog = list_catalog.clone();
            let arguments = ctx.arguments.clone();
            Box::pin(async move {
                let result = match deserialize_arguments::<ListRecipeQueriesArgs>(arguments) {
                    Ok(args) => list_recipe_queries(&catalog, args).into_call_tool_result(),
                    Err(error) => structured_error_result(error),
                };
                Ok(result.into())
            })
        },
    ));

    server.tool_router_mut().add_route(ToolRoute::new_dyn(
        run_tool(&catalog),
        move |ctx: ToolCallContext<'_, McpServer>| -> DynFut<'_, Result<CallToolResponse, McpError>> {
            let catalog = catalog.clone();
            let state = state.clone();
            let arguments = ctx.arguments.clone();
            Box::pin(async move {
                let result = match deserialize_arguments::<RunRecipeQueryArgs>(arguments) {
                    Ok(args) => run_recipe_query(&state, &catalog, args).into_call_tool_result(),
                    Err(error) => structured_error_result(error),
                };
                Ok(result.into())
            })
        },
    ));
    crate::raw_query_routes::protect_query_route(
        server,
        RUN_RECIPE_QUERY_TOOL,
        crate::raw_query_routes::RECIPE_QUERY_POINTER,
    );

    Ok(2)
}

/// The run route's published tool: the catalogue in the description, and the
/// names it accepts in the schema beside it.
///
/// The `enum` arrays are grafted onto the schemars-derived document rather
/// than declared on [`RunRecipeQueryArgs`], because the accepted values are
/// this deployment's merged catalogue and a derive cannot see it. A `oneOf`
/// per query was considered and rejected in planning: clients render it
/// inconsistently, and the enum plus the description block is what named
/// manifest tools already put in front of an agent.
fn run_tool(catalog: &RecipeCatalog) -> Tool {
    let mut tool = recipe_tool::<RunRecipeQueryArgs, RunRecipeQueryOutput>(
        RUN_RECIPE_QUERY_TOOL,
        run_tool_description(catalog),
    );
    let (recipes, queries) = schema_enums(catalog);
    let mut schema = tool.input_schema.as_ref().clone();
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        set_enum(properties, "recipe", recipes);
        set_enum(properties, "query", queries);
        if let Some(variables) = properties
            .get_mut("variables")
            .and_then(Value::as_object_mut)
        {
            variables.insert(
                "description".to_string(),
                Value::String(VARIABLES_DESCRIPTION.to_string()),
            );
        }
    }
    tool.input_schema = Arc::new(schema);
    tool
}

fn set_enum(properties: &mut Map<String, Value>, name: &str, values: Vec<String>) {
    let Some(property) = properties.get_mut(name).and_then(Value::as_object_mut) else {
        return;
    };
    property.insert(
        "enum".to_string(),
        Value::Array(values.into_iter().map(Value::String).collect()),
    );
}

fn recipe_tool<I, O>(name: &'static str, description: String) -> Tool
where
    I: schemars::JsonSchema + 'static,
    O: schemars::JsonSchema + 'static,
{
    Tool::new_with_raw(name, Some(description.into()), Arc::new(Map::new()))
        .with_input_schema::<I>()
        .with_output_schema::<O>()
        .with_annotations(
            ToolAnnotations::new()
                .read_only(true)
                .destructive(false)
                .idempotent(true)
                .open_world(false),
        )
}

fn deserialize_arguments<T: DeserializeOwned>(
    arguments: Option<Map<String, Value>>,
) -> Result<T, RecipeErrorEnvelope> {
    serde_json::from_value(Value::Object(arguments.unwrap_or_default()))
        .map_err(|error| RecipeErrorEnvelope::invalid_request(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::super::description::RUN_SUMMARY;
    use super::*;
    use kglite::api::storage::StorageMode;
    use rmcp::model::{CallToolRequestParams, CallToolResult};
    use rmcp::ServiceExt;
    use serde_json::json;

    fn catalog() -> Arc<RecipeCatalog> {
        Arc::new(
            RecipeCatalog::from_manifest_value(Some(&json!({
                "review": {
                    "description": "Review operations.",
                    "queries": {
                        "empty": {
                            "description": "Return a deterministic empty result.",
                            "parameters": {
                                "type": "object",
                                "properties": {},
                                "required": [],
                                "additionalProperties": false
                            },
                            "cypher": "UNWIND [] AS value RETURN value ORDER BY value"
                        }
                    }
                }
            })))
            .expect("valid catalog"),
        )
    }

    fn numeric_catalog() -> Arc<RecipeCatalog> {
        Arc::new(
            RecipeCatalog::from_manifest_value(Some(
                &serde_json::from_str(
                    r#"{
                        "review": {
                            "description": "Review operations.",
                            "queries": {
                                "echo": {
                                    "description": "Echo a number.",
                                    "parameters": {
                                        "type": "object",
                                        "properties": {"value": {"type": "number"}},
                                        "required": ["value"],
                                        "additionalProperties": false
                                    },
                                    "cypher": "RETURN $value AS value"
                                }
                            }
                        }
                    }"#,
                )
                .unwrap(),
            ))
            .expect("valid numeric catalog"),
        )
    }

    fn assert_safe_contract(tool: &Tool) {
        let annotations = tool.annotations.as_ref().expect("annotations");
        assert_eq!(annotations.read_only_hint, Some(true));
        assert_eq!(annotations.destructive_hint, Some(false));
        assert_eq!(annotations.idempotent_hint, Some(true));
        assert_eq!(annotations.open_world_hint, Some(false));
        assert_eq!(tool.input_schema.get("type"), Some(&json!("object")));
        assert!(tool.output_schema.is_some(), "declared output schema");
    }

    fn structured_json(result: &CallToolResult) -> Value {
        let structured = result
            .structured_content
            .clone()
            .expect("structured content");
        let text = result.content[0].as_text().expect("text fallback");
        assert_eq!(text.text, structured.to_string());
        assert_eq!(
            serde_json::from_str::<Value>(&text.text).unwrap(),
            structured
        );
        structured
    }

    fn described_catalog() -> Arc<RecipeCatalog> {
        Arc::new(
            RecipeCatalog::from_manifest_value(Some(&json!({
                "review": {
                    "description": "Review operations.",
                    "queries": {
                        "callers": {
                            "description": "Functions calling the target.",
                            "parameters": {
                                "type": "object",
                                "properties": {"qualified_name": {"type": "string"}},
                                "required": ["qualified_name"],
                                "additionalProperties": false
                            },
                            "cypher": "MATCH (f:Function) WHERE f.qualified_name = $qualified_name RETURN f.qualified_name AS name ORDER BY name"
                        },
                        "search": {
                            "description": "Search the corpus.",
                            "parameters": {
                                "type": "object",
                                "properties": {
                                    "query": {"type": "string"},
                                    "limit": {"type": ["integer", "null"], "default": 5},
                                    "corpus": {"type": "string", "enum": ["docs", "api"], "default": "docs"}
                                },
                                "required": ["query"],
                                "additionalProperties": false
                            },
                            "cypher": "MATCH (d:Doc) WHERE d.title CONTAINS $query AND d.corpus = $corpus RETURN d.title AS title ORDER BY title LIMIT $limit"
                        }
                    }
                },
                "wells": {
                    "description": "Well operations.",
                    "queries": {
                        "count": {
                            "description": "Count the wells.",
                            "parameters": {
                                "type": "object",
                                "properties": {},
                                "required": [],
                                "additionalProperties": false
                            },
                            "cypher": "MATCH (w:Well) RETURN count(w) AS wells"
                        }
                    }
                }
            })))
            .expect("valid described catalog"),
        )
    }

    /// Everything a named per-query tool would have put in `tools/list`: the
    /// pairs, what each answers, and the variables each takes. An agent that
    /// has to call `list_recipe_queries` to learn them pays a round trip the
    /// tool list could have answered.
    #[test]
    fn the_run_description_carries_the_whole_catalogue() {
        let mut server = McpServer::new(Default::default());
        register_recipe_query_routes(&mut server, GraphState::default(), described_catalog())
            .unwrap();
        let router = server.tool_router_mut();
        let run = router.get(RUN_RECIPE_QUERY_TOOL).expect("run route");
        let description = run.description.as_deref().expect("description");

        assert!(
            description.starts_with(RUN_SUMMARY),
            "the static sentence stays the prefix: {description}"
        );
        assert!(
            description.contains(
                "review.callers — Functions calling the target.; params: qualified_name: string (required)"
            ),
            "{description}"
        );
        assert!(
            description.contains(
                "review.search — Search the corpus.; params: corpus: string one of [\"docs\",\"api\"] =\"docs\", limit: integer|null =5, query: string (required)"
            ),
            "{description}"
        );
        assert!(
            description.contains("wells.count — Count the wells.; params: none"),
            "{description}"
        );
        assert!(
            description.find("review.callers").unwrap() < description.find("wells.count").unwrap(),
            "catalogue order is the catalogue's own: {description}"
        );

        let list = router.get(LIST_RECIPE_QUERIES_TOOL).expect("list route");
        let list_description = list.description.as_deref().expect("description");
        assert!(
            list_description.contains("run_recipe_query"),
            "the listing tool points at the block that replaced it: {list_description}"
        );
    }

    #[test]
    fn the_run_input_schema_enumerates_the_real_recipe_and_query_names() {
        let mut server = McpServer::new(Default::default());
        register_recipe_query_routes(&mut server, GraphState::default(), described_catalog())
            .unwrap();
        let router = server.tool_router_mut();
        let run = router.get(RUN_RECIPE_QUERY_TOOL).expect("run route");

        assert_eq!(
            run.input_schema["properties"]["recipe"]["enum"],
            json!(["review", "wells"])
        );
        assert_eq!(
            run.input_schema["properties"]["query"]["enum"],
            json!(["callers", "search", "count"]),
            "every query name, in catalogue order, deduplicated"
        );
        assert_eq!(
            run.input_schema["properties"]["variables"]["type"],
            json!("object"),
            "variables stays an open object, not a per-query oneOf"
        );
        let variables_description = run.input_schema["properties"]["variables"]["description"]
            .as_str()
            .expect("variables description");
        assert!(
            variables_description.contains("catalogue"),
            "{variables_description}"
        );
    }

    #[test]
    fn empty_catalog_registers_nothing() {
        let mut server = McpServer::new(Default::default());
        let before = server.tool_router_mut().map.len();
        assert_eq!(
            register_recipe_query_routes(
                &mut server,
                GraphState::default(),
                Arc::new(RecipeCatalog::default()),
            )
            .unwrap(),
            0
        );
        assert_eq!(server.tool_router_mut().map.len(), before);
    }

    #[test]
    fn registration_is_atomic_and_never_replaces_an_owner() {
        let mut server = McpServer::new(Default::default());
        server.register_typed_tool::<ListRecipeQueriesArgs, _>(
            LIST_RECIPE_QUERIES_TOOL,
            "Existing owner.",
            |_| "owned".to_string(),
        );

        let error = register_recipe_query_routes(&mut server, GraphState::default(), catalog())
            .expect_err("collision must fail");

        assert!(error.to_string().contains(LIST_RECIPE_QUERIES_TOOL));
        assert_eq!(
            server
                .tool_router_mut()
                .get(LIST_RECIPE_QUERIES_TOOL)
                .and_then(|tool| tool.description.as_deref()),
            Some("Existing owner.")
        );
        assert!(!server
            .tool_router_mut()
            .map
            .contains_key(RUN_RECIPE_QUERY_TOOL));
    }

    #[test]
    fn routes_publish_closed_schemas_and_all_safe_annotations() {
        let mut server = McpServer::new(Default::default());
        register_recipe_query_routes(&mut server, GraphState::default(), catalog()).unwrap();

        let router = server.tool_router_mut();
        let list = router.get(LIST_RECIPE_QUERIES_TOOL).expect("list route");
        let run = router.get(RUN_RECIPE_QUERY_TOOL).expect("run route");
        assert_safe_contract(list);
        assert_safe_contract(run);
        assert_eq!(
            list.input_schema.get("additionalProperties"),
            Some(&json!(false))
        );
        assert_eq!(
            run.input_schema.get("required"),
            Some(&json!(["recipe", "query", "variables"]))
        );
        assert_eq!(
            run.input_schema.get("additionalProperties"),
            Some(&json!(false))
        );
        let list_output = Value::Object(
            list.output_schema
                .as_ref()
                .expect("list output schema")
                .as_ref()
                .clone(),
        )
        .to_string();
        assert!(list_output.contains("recipes"));
        assert!(list_output.contains("invalid_request"));
        let run_output = Value::Object(
            run.output_schema
                .as_ref()
                .expect("run output schema")
                .as_ref()
                .clone(),
        )
        .to_string();
        assert!(run_output.contains("result"));
        for contract_field in [
            "invalid_request",
            "unknown_recipe",
            "unknown_query",
            "invalid_variables",
            "no_active_graph",
            "stale_graph",
            "query_failed",
            "result_limit_exceeded",
            "failure_message",
            "observed_count",
            "limit",
        ] {
            assert!(
                run_output.contains(contract_field),
                "run output schema is missing {contract_field:?}"
            );
        }
    }

    #[tokio::test]
    async fn real_handshake_preserves_structured_empty_and_invalid_request_results() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = GraphState::default();
        state
            .create_in_mode(&temp.path().join("empty.kgl"), StorageMode::Memory)
            .expect("create active graph");
        let mut server = McpServer::new(Default::default());
        register_recipe_query_routes(&mut server, state, catalog()).unwrap();

        let (server_transport, client_transport) = tokio::io::duplex(16 * 1024);
        let server_handle = tokio::spawn(async move { server.serve(server_transport).await });
        let client = ().serve(client_transport).await.expect("start MCP client");

        let listed = client.peer().list_tools(None).await.expect("list tools");
        let by_name = listed
            .tools
            .iter()
            .map(|tool| (tool.name.as_ref(), tool))
            .collect::<std::collections::HashMap<_, _>>();
        assert_safe_contract(by_name[LIST_RECIPE_QUERIES_TOOL]);
        assert_safe_contract(by_name[RUN_RECIPE_QUERY_TOOL]);

        let malformed = [
            CallToolRequestParams::new(RUN_RECIPE_QUERY_TOOL),
            CallToolRequestParams::new(RUN_RECIPE_QUERY_TOOL).with_arguments(
                json!({
                    "recipe": "review",
                    "query": "empty",
                    "variables": {},
                    "extra": true
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
            CallToolRequestParams::new(RUN_RECIPE_QUERY_TOOL).with_arguments(
                json!({"recipe": 3, "query": "empty", "variables": {}})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        ];
        for request in malformed {
            let invalid = client
                .call_tool(request)
                .await
                .expect("invalid request is a tool error value");
            assert_eq!(invalid.is_error, Some(true));
            assert_eq!(structured_json(&invalid)["code"], "invalid_request");
        }

        let list = client
            .call_tool(CallToolRequestParams::new(LIST_RECIPE_QUERIES_TOOL))
            .await
            .expect("compact listing succeeds");
        assert_eq!(list.is_error, Some(false));
        assert_eq!(structured_json(&list)["recipes"][0]["name"], "review");

        let unknown = client
            .call_tool(
                CallToolRequestParams::new(LIST_RECIPE_QUERIES_TOOL)
                    .with_arguments(json!({"recipe": "missing"}).as_object().unwrap().clone()),
            )
            .await
            .expect("unknown recipe is a tool error value");
        assert_eq!(unknown.is_error, Some(true));
        assert_eq!(structured_json(&unknown)["code"], "unknown_recipe");

        let empty = client
            .call_tool(
                CallToolRequestParams::new(RUN_RECIPE_QUERY_TOOL).with_arguments(
                    json!({"recipe": "review", "query": "empty", "variables": {}})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .expect("empty query succeeds");
        assert_eq!(empty.is_error, Some(false));
        let value = structured_json(&empty);
        assert_eq!(value["result"]["columns"], json!(["value"]));
        assert_eq!(value["result"]["rows"], json!([]));
        assert_eq!(value["result"]["row_count"], 0);

        client.cancel().await.expect("stop MCP client");
        server_handle.abort();
    }

    #[tokio::test]
    async fn registered_recipe_route_rejects_unrepresentable_integer_variables() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = GraphState::default();
        state
            .create_in_mode(&temp.path().join("empty.kgl"), StorageMode::Memory)
            .expect("create active graph");
        let mut server = McpServer::new(Default::default());
        register_recipe_query_routes(&mut server, state, numeric_catalog()).unwrap();

        let (server_transport, client_transport) = tokio::io::duplex(16 * 1024);
        let server_handle = tokio::spawn(async move { server.serve(server_transport).await });
        let client = ().serve(client_transport).await.expect("start MCP client");
        let arguments = serde_json::json!({
            "recipe": "review",
            "query": "echo",
            "variables": {"value": u64::MAX}
        });
        let result = client
            .call_tool(
                CallToolRequestParams::new(RUN_RECIPE_QUERY_TOOL)
                    .with_arguments(arguments.as_object().unwrap().clone()),
            )
            .await
            .expect("overflow is a structured tool error");
        assert_eq!(result.is_error, Some(true));
        let structured = structured_json(&result);
        assert_eq!(structured["code"], "invalid_variables");
        assert_eq!(structured["details"]["issues"][0]["path"], "$.value");

        client.cancel().await.expect("stop MCP client");
        server_handle.abort();
    }
}
