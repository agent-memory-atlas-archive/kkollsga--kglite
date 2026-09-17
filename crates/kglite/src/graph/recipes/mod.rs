//! Cypher recipe catalogues — named, parameterised, read-only stored queries.
//!
//! Re-exported as [`kglite::api::recipes`](crate::api::recipes). A catalogue is
//! a two-level map: a *recipe* groups *queries*, and each query carries a
//! description, a closed JSON-Schema parameter object and the Cypher it runs.
//!
//! Validation lives here rather than in the agent host that serves the
//! catalogue because every consumer asks the same questions of a stored query:
//! is it read-only, does its schema use only keywords we can enforce, and does
//! the schema describe exactly the `$parameters` the Cypher references. The
//! MCP server owns the routes, the wire types and the result envelope; it does
//! not own the contract.
//!
//! **A catalogue is validated once and never mutated.** `from_manifest_value`
//! compiles the whole thing or fails, so a route handler holding a
//! [`RecipeCatalog`] never has to re-check a query it is about to run.

mod records;
mod schema;
mod validation;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::{Map, Value};

use crate::graph::languages::cypher;

pub use records::{
    catalogue_from_graph, delete, export_path, export_value, get, import_path, import_value, list,
    render_markdown, set, validate, RecipeRecord, RecipeWarning, SetOutcome, RECIPE_LABEL,
};
#[cfg(feature = "okf")]
pub use records::{parse_markdown, set_from_markdown};
pub use schema::ParameterSchema;
pub use validation::{
    query_conversion_error, VariableIssue, VariableIssueKind, VariablesValidationError,
};

const RECIPE_KEYS: &[&str] = &["description", "queries"];
const QUERY_KEYS: &[&str] = &["description", "parameters", "cypher"];

/// Maximum rows a single recipe result may carry.
///
/// A stored literal `LIMIT` equal to this value is rejected at compile time: it
/// would make an overflowing query look complete before the caller can observe
/// and report its true cardinality.
pub const RECIPE_RESULT_ROW_LIMIT: usize = 200;

/// Why a catalogue, a recipe, a query or its parameter schema is invalid.
///
/// The message is **flattened**, not chained: it names the recipe, the query
/// and the broken rule in one line, so a caller that prints plain `Display` —
/// a boot log, a Python exception — still says which stored query is wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeCatalogError {
    message: String,
}

impl RecipeCatalogError {
    /// The rule that was broken, without any enclosing identification.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Prefix the message with the thing that contained the failure, the way
    /// `recipe "code_review": query "callers": cypher must be read-only` reads.
    fn context(self, prefix: impl fmt::Display) -> Self {
        Self {
            message: format!("{prefix}: {}", self.message),
        }
    }
}

impl fmt::Display for RecipeCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RecipeCatalogError {}

impl From<RecipeCatalogError> for crate::error::KgError {
    fn from(error: RecipeCatalogError) -> Self {
        crate::error::KgError::Argument(error.message)
    }
}

/// Result of compiling any part of a catalogue.
pub type CatalogResult<T> = Result<T, RecipeCatalogError>;

fn invalid(message: impl Into<String>) -> RecipeCatalogError {
    RecipeCatalogError {
        message: message.into(),
    }
}

/// Cheap immutable catalog dimensions used by boot logging and discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogSummary {
    pub recipe_count: usize,
    pub query_count: usize,
}

/// All validated recipes from one source, ordered deterministically by name.
#[derive(Debug, Clone, Default)]
pub struct RecipeCatalog {
    recipes: BTreeMap<String, RecipeDefinition>,
}

impl RecipeCatalog {
    /// Parse an `extensions.cypher_recipes` mapping. Absence and an empty
    /// mapping are the only disabled shapes; malformed or partially configured
    /// catalogs are an error the caller decides how to treat — the MCP server
    /// fails boot on a manifest one, and skips an invalid graph-carried record.
    pub fn from_manifest_value(raw: Option<&Value>) -> CatalogResult<Self> {
        let Some(raw) = raw else {
            return Ok(Self::default());
        };
        let recipes = raw
            .as_object()
            .ok_or_else(|| invalid("must be a mapping of recipe names"))?;
        if recipes.is_empty() {
            return Ok(Self::default());
        }

        let mut parsed = BTreeMap::new();
        for (name, raw_recipe) in recipes {
            validate_identifier(name, "recipe")?;
            let recipe = RecipeDefinition::parse(name, raw_recipe)
                .map_err(|error| error.context(format!("recipe {name:?}")))?;
            parsed.insert(name.clone(), recipe);
        }
        Ok(Self { recipes: parsed })
    }

    pub fn is_empty(&self) -> bool {
        self.recipes.is_empty()
    }

    pub fn summary(&self) -> CatalogSummary {
        CatalogSummary {
            recipe_count: self.recipes.len(),
            query_count: self
                .recipes
                .values()
                .map(|recipe| recipe.queries.len())
                .sum(),
        }
    }

    /// Discovery dimensions only when route registration is enabled.
    /// Absent and explicitly empty sources both compile to an empty catalog
    /// and therefore contribute no overview hint.
    pub fn discovery_summary(&self) -> Option<CatalogSummary> {
        (!self.is_empty()).then(|| self.summary())
    }

    pub fn recipes(&self) -> impl ExactSizeIterator<Item = &RecipeDefinition> {
        self.recipes.values()
    }

    pub fn get(&self, name: &str) -> Option<&RecipeDefinition> {
        self.recipes.get(name)
    }

    /// Add one already-compiled query, creating its group with
    /// `recipe_description` if the group is new.
    ///
    /// An existing group **keeps** the description it was first given, so the
    /// caller decides which one wins by the order it feeds records in;
    /// `catalogue_from_graph` feeds them in name order, which is where D14's
    /// "first node in name order" rule comes from. A second query with a name
    /// already in the group replaces it.
    pub fn insert_query(
        &mut self,
        recipe: &str,
        recipe_description: &str,
        query: RecipeQueryDefinition,
    ) {
        let group = self
            .recipes
            .entry(recipe.to_string())
            .or_insert_with(|| RecipeDefinition {
                name: recipe.to_string(),
                description: recipe_description.to_string(),
                queries: BTreeMap::new(),
            });
        group.queries.insert(query.name.clone(), query);
    }
}

/// Lay a manifest catalogue over a graph-carried one.
///
/// The manifest wins per `(recipe, name)` and per group description; a group
/// or query only the graph has is kept. Both sides are ordered maps, so the
/// result is the same whatever order the two were built in.
pub fn merge(graph: RecipeCatalog, manifest: RecipeCatalog) -> RecipeCatalog {
    let mut merged = graph;
    for (name, group) in manifest.recipes {
        match merged.recipes.get_mut(&name) {
            Some(existing) => {
                existing.description = group.description;
                existing.queries.extend(group.queries);
            }
            None => {
                merged.recipes.insert(name, group);
            }
        }
    }
    merged
}

/// One named group of related query operations.
#[derive(Debug, Clone)]
pub struct RecipeDefinition {
    pub name: String,
    pub description: String,
    queries: BTreeMap<String, RecipeQueryDefinition>,
}

impl RecipeDefinition {
    fn parse(name: &str, raw: &Value) -> CatalogResult<Self> {
        let map = object_with_only(raw, RECIPE_KEYS, "recipe")?;
        let description = required_nonempty_string(map, "description")?;
        let raw_queries = map
            .get("queries")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("queries must be a non-empty mapping"))?;
        if raw_queries.is_empty() {
            return Err(invalid("queries must be a non-empty mapping"));
        }

        let mut queries = BTreeMap::new();
        for (query_name, raw_query) in raw_queries {
            validate_identifier(query_name, "query")?;
            let query = RecipeQueryDefinition::parse(query_name, raw_query)
                .map_err(|error| error.context(format!("query {query_name:?}")))?;
            queries.insert(query_name.clone(), query);
        }
        Ok(Self {
            name: name.to_string(),
            description,
            queries,
        })
    }

    pub fn queries(&self) -> impl ExactSizeIterator<Item = &RecipeQueryDefinition> {
        self.queries.values()
    }

    pub fn get(&self, name: &str) -> Option<&RecipeQueryDefinition> {
        self.queries.get(name)
    }
}

/// One stored, parameterized, read-only Cypher operation.
#[derive(Debug, Clone)]
pub struct RecipeQueryDefinition {
    pub name: String,
    pub description: String,
    pub parameters: ParameterSchema,
    pub cypher: String,
}

impl RecipeQueryDefinition {
    fn parse(name: &str, raw: &Value) -> CatalogResult<Self> {
        let map = object_with_only(raw, QUERY_KEYS, "query")?;
        let description = required_nonempty_string(map, "description")?;
        let cypher_source = required_nonempty_string(map, "cypher")?;
        let raw_parameters = map
            .get("parameters")
            .ok_or_else(|| invalid("parameters is required"))?;

        Self::compile(name, description, cypher_source, raw_parameters)
    }

    /// Compile one already-destructured query definition.
    ///
    /// The graph-carried record path arrives with the four fields in hand
    /// rather than as a JSON mapping, and must be held to exactly the rules
    /// [`parse`](Self::parse) applies — a record that would fail at boot has to
    /// fail where it is written.
    pub fn compile(
        name: &str,
        description: String,
        cypher_source: String,
        raw_parameters: &Value,
    ) -> CatalogResult<Self> {
        let features = cypher::query_features(&cypher_source)
            .map_err(|error| invalid(format!("cypher is not a valid KGLite query: {error}")))?;
        validate_read_only_query(&features)?;

        let parameter_names = cypher::parameter_names(&cypher_source).map_err(|error| {
            invalid(format!("could not collect Cypher parameter names: {error}"))
        })?;
        let parameters = ParameterSchema::compile_root(raw_parameters, &parameter_names)
            .map_err(|error| error.context("parameters schema is invalid"))?;

        Ok(Self {
            name: name.to_string(),
            description,
            parameters,
            cypher: cypher_source,
        })
    }

    pub fn validate_variables(
        &self,
        variables: &Map<String, Value>,
    ) -> Result<(), VariablesValidationError> {
        self.parameters.validate_variables(variables)
    }
}

fn validate_read_only_query(features: &cypher::QueryFeatures) -> CatalogResult<()> {
    if features.explain {
        return Err(invalid("EXPLAIN is not allowed in recipe queries"));
    }
    if features.profile {
        return Err(invalid("PROFILE is not allowed in recipe queries"));
    }
    if features.format_csv {
        return Err(invalid("FORMAT CSV is not allowed in recipe queries"));
    }
    if features.has_load_csv {
        return Err(invalid("LOAD CSV is not allowed in recipe queries"));
    }
    if features
        .literal_limits
        .contains(&(RECIPE_RESULT_ROW_LIMIT as i64))
    {
        return Err(invalid(format!(
            "literal LIMIT {RECIPE_RESULT_ROW_LIMIT} is reserved for the recipe result payload cap; stored queries must not hide overflow from the server"
        )));
    }
    if features.is_mutation {
        return Err(invalid(
            "cypher must be read-only; mutation clauses are not allowed",
        ));
    }
    Ok(())
}

fn object_with_only<'a>(
    raw: &'a Value,
    allowed: &[&str],
    label: &str,
) -> CatalogResult<&'a Map<String, Value>> {
    let map = raw
        .as_object()
        .ok_or_else(|| invalid(format!("{label} must be a mapping")))?;
    let allowed: BTreeSet<_> = allowed.iter().copied().collect();
    let unknown: Vec<_> = map
        .keys()
        .filter(|key| !allowed.contains(key.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(invalid(format!("unsupported {label} keys: {unknown:?}")));
    }
    Ok(map)
}

fn required_nonempty_string(map: &Map<String, Value>, key: &str) -> CatalogResult<String> {
    let value = map
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid(format!("{key} must be a non-empty string")))?;
    Ok(value.to_string())
}

/// Recipe and query names are catalogue identifiers: they key the MCP request,
/// so they must stay a plain token no wire format has to escape.
pub fn validate_identifier(identifier: &str, label: &str) -> CatalogResult<()> {
    let mut chars = identifier.chars();
    let valid_start = chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_');
    if !valid_start || !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return Err(invalid(format!(
            "{label} identifier {identifier:?} must match ^[A-Za-z_][A-Za-z0-9_]*$"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
