//! MCP delivery for Cypher recipe catalogues.
//!
//! The catalogue model — recipes, queries, the closed JSON-Schema subset and
//! the read-only Cypher rules — lives in [`kglite::api::recipes`], because a
//! Python caller and a boot-time graph reader validate a stored query by
//! exactly the same rules. What stays here is the delivery: route
//! registration, the wire types, the result envelope and the structured error
//! payload an agent sees.

mod errors;
mod result;
mod routes;
mod wire;

#[cfg(test)]
mod catalog_tests;
#[cfg(test)]
mod result_tests;

pub(crate) use errors::RecipeErrorEnvelope;
pub(crate) use kglite::api::recipes::{
    query_conversion_error, CatalogSummary, RecipeCatalog, RecipeQueryDefinition,
    VariableIssueKind, VariablesValidationError, RECIPE_RESULT_ROW_LIMIT,
};
pub(crate) use result::{list_recipe_queries, run_recipe_query};
pub(crate) use routes::{
    register_recipe_query_routes, LIST_RECIPE_QUERIES_TOOL, RUN_RECIPE_QUERY_TOOL,
};
