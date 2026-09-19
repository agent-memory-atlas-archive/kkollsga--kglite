//! The catalogue text `run_recipe_query` publishes in `tools/list`.
//!
//! Two fixed routes serve a whole catalogue, so what a named per-query tool
//! would have put in front of the agent — the pair to call, what it answers,
//! which variables it takes — has to arrive through this one description and
//! the enums beside it. Without it the tool is generic, and an agent that
//! cannot read the routing off the tool list goes looking for it
//! (`list_recipe_queries`, then raw Cypher), which is the round trip this
//! block exists to remove.

use serde_json::{Map, Value};

use super::{CatalogSummary, RecipeCatalog};

pub(super) const RUN_SUMMARY: &str = "Run one exact, boot-validated, read-only Cypher recipe query with strictly validated variables. Returns all rows up to the MCP payload limit or a structured error; use cypher_query for unmatched or broader questions.";

pub(super) const VARIABLES_DESCRIPTION: &str = "Variables for the chosen query, as named in the catalogue block in this tool's description. Required even for a parameter-free query (pass `{}`).";

/// Longest per-query catalogue block this description carries.
///
/// Every tool description is sent to the client on every session, so a large
/// catalogue would spend the agent's context before it asks anything. Past
/// this the block degrades to `recipe.query` names and points at
/// `list_recipe_queries` for the parameters, which is a call the agent can
/// choose to make rather than a cost it always pays.
const BLOCK_LIMIT: usize = 4_000;

const FULL_HEADER: &str = "Catalogue — call with `recipe` and `query` from this list:";
const NAMES_HEADER: &str = "Catalogue — see list_recipe_queries for parameters:";

/// The static sentence, then the catalogue rendered under it.
pub(super) fn run_tool_description(catalog: &RecipeCatalog) -> String {
    if catalog.is_empty() {
        return RUN_SUMMARY.to_string();
    }
    format!("{RUN_SUMMARY}\n\n{}", catalog_block(catalog))
}

/// The block on its own: the full per-query form while it fits, else names.
fn catalog_block(catalog: &RecipeCatalog) -> String {
    let full = full_block(catalog);
    if full.len() <= BLOCK_LIMIT {
        full
    } else {
        names_block(catalog)
    }
}

/// What the bare `graph_overview` hint says about the served catalogue.
///
/// The counts alone told an agent a catalogue exists; the names tell it
/// whether the question in front of it is already answered by one, which is
/// the decision the hint is read for.
#[derive(Clone, Debug)]
pub(crate) struct CatalogHint {
    pub(crate) summary: CatalogSummary,
    /// `recipe.query`, in catalogue order.
    pub(crate) names: Vec<String>,
}

/// The hint for a catalogue that is actually served — `None` for an absent or
/// empty one, which registers no routes and must add no discovery text.
pub(crate) fn catalog_hint(catalog: &RecipeCatalog) -> Option<CatalogHint> {
    catalog.discovery_summary().map(|summary| CatalogHint {
        summary,
        names: qualified_names(catalog),
    })
}

/// `recipe.query` for every query, in the catalogue's own order.
fn qualified_names(catalog: &RecipeCatalog) -> Vec<String> {
    catalog
        .recipes()
        .flat_map(|recipe| {
            recipe
                .queries()
                .map(move |query| format!("{}.{}", recipe.name, query.name))
        })
        .collect()
}

/// Recipe names, then query names, both in catalogue order and deduplicated —
/// the `enum` arrays that tell a client which pairs exist at all.
pub(super) fn schema_enums(catalog: &RecipeCatalog) -> (Vec<String>, Vec<String>) {
    let recipes: Vec<String> = catalog
        .recipes()
        .map(|recipe| recipe.name.clone())
        .collect();
    let mut queries: Vec<String> = Vec::new();
    for recipe in catalog.recipes() {
        for query in recipe.queries() {
            if !queries.contains(&query.name) {
                queries.push(query.name.clone());
            }
        }
    }
    (recipes, queries)
}

fn full_block(catalog: &RecipeCatalog) -> String {
    let mut block = String::from(FULL_HEADER);
    for recipe in catalog.recipes() {
        for query in recipe.queries() {
            block.push_str(&format!(
                "\n{}.{} — {}; params: {}",
                recipe.name,
                query.name,
                query.description,
                render_parameters(query.parameters.as_json())
            ));
        }
    }
    block
}

fn names_block(catalog: &RecipeCatalog) -> String {
    format!("{NAMES_HEADER}\n{}", qualified_names(catalog).join(", "))
}

/// One query's parameters as `name: type [one of …] [=default] (required)`.
///
/// Read from the author's own schema rather than the compiled node, because
/// that is the document the client is also shown, and a default or an enum
/// value is quoted here exactly as it must be spelled in `variables`.
fn render_parameters(schema: &Map<String, Value>) -> String {
    let properties = schema.get("properties").and_then(Value::as_object);
    let Some(properties) = properties.filter(|properties| !properties.is_empty()) else {
        return "none".to_string();
    };
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    properties
        .iter()
        .map(|(name, property)| {
            let mut rendered = format!("{name}: {}", render_type(property));
            if let Some(values) = property.get("enum").and_then(Value::as_array) {
                let values: Vec<String> = values.iter().map(Value::to_string).collect();
                rendered.push_str(&format!(" one of [{}]", values.join(",")));
            }
            if let Some(default) = property.get("default") {
                rendered.push_str(&format!(" ={default}"));
            }
            if required.iter().any(|entry| entry.as_str() == Some(name)) {
                rendered.push_str(" (required)");
            }
            rendered
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_type(property: &Value) -> String {
    match property.get("type") {
        Some(Value::String(name)) => name.clone(),
        Some(Value::Array(names)) => names
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("|"),
        _ => "any".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog(description: &str) -> RecipeCatalog {
        RecipeCatalog::from_manifest_value(Some(&json!({
            "review": {
                "description": "Review operations.",
                "queries": {
                    "search": {
                        "description": description,
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "query": {"type": "string"},
                                "limit": {"type": "integer", "default": 5}
                            },
                            "required": ["query"],
                            "additionalProperties": false
                        },
                        "cypher": "MATCH (d:Doc) WHERE d.title CONTAINS $query RETURN d.title AS title ORDER BY title LIMIT $limit"
                    }
                }
            }
        })))
        .expect("valid catalog")
    }

    /// The fallback is a cliff, so the test stands on both sides of it: one
    /// character past the limit is the whole difference between the two forms.
    #[test]
    fn a_block_past_the_limit_degrades_to_names_and_a_pointer() {
        // Ascii padding, so a byte length is a character count: one `x` of
        // description is the baseline the padding is measured against.
        let base = full_block(&catalog("x")).len();
        let at_limit = catalog(&"x".repeat(BLOCK_LIMIT - base + 1));
        assert_eq!(full_block(&at_limit).len(), BLOCK_LIMIT);
        assert_eq!(
            catalog_block(&at_limit),
            full_block(&at_limit),
            "a block that exactly fits is published in full"
        );

        let past_limit = catalog(&"x".repeat(BLOCK_LIMIT - base + 2));
        assert_eq!(full_block(&past_limit).len(), BLOCK_LIMIT + 1);
        let rendered = catalog_block(&past_limit);
        assert_eq!(
            rendered,
            "Catalogue — see list_recipe_queries for parameters:\nreview.search"
        );
        assert!(
            !rendered.contains("params:"),
            "the names-only form carries no parameters: {rendered}"
        );
    }

    #[test]
    fn an_empty_catalogue_publishes_the_static_sentence_alone() {
        assert_eq!(
            run_tool_description(&RecipeCatalog::default()),
            RUN_SUMMARY,
            "a server with no catalogue registers no route, and the text must \
             not imply one"
        );
    }
}
