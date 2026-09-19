//! The catalogue text `run_recipe_query` publishes in `tools/list`.
//!
//! Two fixed routes serve the whole catalogue, and only the queries whose
//! author asked for a `tool:` also get a route of their own — so for
//! everything else the pair to call, what it answers and which variables it
//! takes have to arrive through this one description and the enums beside
//! it. Without it the tool is generic, and an agent that cannot read the
//! routing off the tool list goes looking for it (`list_recipe_queries`,
//! then raw Cypher), which is the round trip this block exists to remove.

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use super::{CatalogSummary, RecipeCatalog};

pub(super) const RUN_SUMMARY: &str = "Run one exact, boot-validated, read-only Cypher recipe query with strictly validated variables. Returns all rows up to the MCP payload limit or a structured error; use cypher_query for unmatched or broader questions.";

pub(super) const VARIABLES_DESCRIPTION: &str = "Variables for the chosen query, as named in the catalogue block in this tool's description. Required even for a parameter-free query (pass `{}`).";

/// How much of the catalogue this tool description carries.
///
/// Every tool description is sent to the client on every session, so an
/// unbounded catalogue would spend the agent's context before it asks
/// anything. What the ceiling buys is bounded bytes; what it must not cost is
/// the machine-readable half — a `recipe.query` name without its parameters
/// sends the agent to `list_recipe_queries` to learn what to pass, which is
/// the round trip this block exists to remove. So prose is what gives way
/// first: descriptions are shortened, largest first, and only far enough to
/// fit.
///
/// The defaults are measured against a help-desk catalogue whose descriptions
/// are routing paragraphs (seven queries, ~4 000 characters of prose,
/// 2026-09-19): it publishes whole. Only the skill bodies the framework
/// appends *after* this block are outside the ceiling — they are the skill
/// registry's budget, not this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CatalogBudgets {
    /// Longest rendered block, in **bytes**.
    pub(crate) block: usize,
    /// Longest per-query description, in **characters**, once shortening
    /// begins. Zero drops the prose and keeps names and parameters.
    pub(crate) description: usize,
}

impl Default for CatalogBudgets {
    fn default() -> Self {
        Self {
            block: 16_000,
            description: 600,
        }
    }
}

impl CatalogBudgets {
    /// `extensions.recipe_catalog: {block_budget, description_budget}`.
    ///
    /// Absent block, or either key absent, leaves that default in place. An
    /// unknown key or a non-integer is a boot error rather than a dropped
    /// key: a budget that parsed and was then ignored is this crate's
    /// recurring defect shape.
    pub(crate) fn from_manifest_value(raw: Option<&Value>) -> Result<Self> {
        let Some(raw) = raw else {
            return Ok(Self::default());
        };
        let map = raw.as_object().context(
            "extensions.recipe_catalog must be a mapping of budget names to integers \
             (block_budget, description_budget)",
        )?;
        let mut budgets = Self::default();
        for (key, value) in map {
            let n = value.as_u64().with_context(|| {
                format!(
                    "extensions.recipe_catalog.{key} must be a non-negative integer; found {value}"
                )
            })?;
            match key.as_str() {
                "block_budget" => {
                    anyhow::ensure!(
                        n > 0,
                        "extensions.recipe_catalog.block_budget must be at least 1 byte; \
                         a zero budget publishes no catalogue at all"
                    );
                    budgets.block = n as usize;
                }
                "description_budget" => budgets.description = n as usize,
                other => anyhow::bail!(
                    "extensions.recipe_catalog: unknown key `{other}` \
                     (block_budget, description_budget)"
                ),
            }
        }
        Ok(budgets)
    }
}

/// Which of the three forms the block was rendered in — what
/// `list_recipe_queries` must tell the agent it is still good for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CatalogForm {
    /// Every description whole.
    Full,
    /// Names and parameters whole, descriptions shortened (or, at a zero
    /// description budget, dropped).
    Capped,
    /// Names alone — reached only when names and parameters together already
    /// exceed the block budget.
    Names,
}

const FULL_HEADER: &str = "Catalogue — call with `recipe` and `query` from this list:";
const CAPPED_HEADER: &str = "Catalogue — call with `recipe` and `query` from this list; descriptions are shortened, see list_recipe_queries for the full text:";
const NAMES_HEADER: &str = "Catalogue — see list_recipe_queries for parameters:";

/// The static sentence, then the catalogue rendered under it.
pub(super) fn run_tool_description(
    catalog: &RecipeCatalog,
    budgets: &CatalogBudgets,
) -> (String, CatalogForm) {
    if catalog.is_empty() {
        return (RUN_SUMMARY.to_string(), CatalogForm::Full);
    }
    let (block, form) = catalog_block(catalog, budgets);
    (format!("{RUN_SUMMARY}\n\n{block}"), form)
}

/// The block on its own: whole while it fits, then descriptions shortened to
/// the highest level that fits, and only then names.
fn catalog_block(catalog: &RecipeCatalog, budgets: &CatalogBudgets) -> (String, CatalogForm) {
    let full = block_at(catalog, FULL_HEADER, None);
    if full.len() <= budgets.block {
        return (full, CatalogForm::Full);
    }
    // Water-filling: a uniform level cuts only the descriptions above it, so
    // raising it as far as the budget allows is the same thing as shortening
    // largest-first. Level 0 is names and parameters — the floor the decision
    // says is unconditional; when even that does not fit, names are all that
    // is left.
    if block_at(catalog, CAPPED_HEADER, Some(0)).len() > budgets.block {
        return (names_block(catalog), CatalogForm::Names);
    }
    let (mut fits, mut over) = (0usize, budgets.description);
    while fits < over {
        let level = fits + (over - fits).div_ceil(2);
        if block_at(catalog, CAPPED_HEADER, Some(level)).len() <= budgets.block {
            fits = level;
        } else {
            over = level - 1;
        }
    }
    (
        block_at(catalog, CAPPED_HEADER, Some(fits)),
        CatalogForm::Capped,
    )
}

/// What `list_recipe_queries` is still worth calling for, given what the run
/// tool's description actually published.
pub(super) fn list_tool_description(form: CatalogForm) -> String {
    let tail = match form {
        CatalogForm::Full => {
            "The routing is already in run_recipe_query's description; call this only for a \
             recipe not listed there."
        }
        CatalogForm::Capped => {
            "run_recipe_query's description carries every query's name and parameters, with \
             descriptions shortened to fit; call this for the full description of a query \
             before using it."
        }
        CatalogForm::Names => {
            "run_recipe_query's description lists the `recipe.query` names only; call this \
             for their descriptions and parameter schemas."
        }
    };
    format!(
        "List the boot-validated Cypher recipe catalog. Omit `recipe` for compact recipe \
         summaries; provide it to inspect that recipe's named queries and parameter schemas. \
         {tail}"
    )
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

/// One line per query under `header`, with each description shortened to
/// `cap` characters (`None` = whole).
fn block_at(catalog: &RecipeCatalog, header: &str, cap: Option<usize>) -> String {
    let mut block = String::from(header);
    for recipe in catalog.recipes() {
        for query in recipe.queries() {
            let params = render_parameters(query.parameters.as_json());
            let name = format!("{}.{}", recipe.name, query.name);
            // A query with its own route is reachable both ways; the marker
            // is what tells an agent reading this block that the one-call
            // form exists, which is the whole point of having asked for it.
            let tool = query
                .tool
                .as_deref()
                .map(|tool| format!(" → tool: {tool}"))
                .unwrap_or_default();
            match cap {
                None => block.push_str(&format!(
                    "\n{name} — {}; params: {params}{tool}",
                    query.description
                )),
                Some(cap) => match shorten(&query.description, cap) {
                    Some(text) => {
                        block.push_str(&format!("\n{name} — {text}; params: {params}{tool}"))
                    }
                    None => block.push_str(&format!("\n{name}; params: {params}{tool}")),
                },
            }
        }
    }
    block
}

/// `description` at `cap` characters with `…` when it was cut, or `None` at a
/// zero cap — the line then carries its name and parameters alone.
fn shorten(description: &str, cap: usize) -> Option<String> {
    if cap == 0 {
        return None;
    }
    let mut rest = description.chars();
    let head: String = rest.by_ref().take(cap).collect();
    Some(if rest.next().is_none() {
        head
    } else {
        format!("{head}…")
    })
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

    /// Many queries, `description_length` characters of prose each, one
    /// required string parameter — the shape that puts a block over a ceiling.
    fn wide_catalog(lengths: &[usize]) -> RecipeCatalog {
        let mut queries = serde_json::Map::new();
        for (index, length) in lengths.iter().enumerate() {
            queries.insert(
                format!("query_{index:04}"),
                json!({
                    "description": "d".repeat(*length),
                    "parameters": {
                        "type": "object",
                        "properties": {"topic": {"type": "string"}},
                        "required": ["topic"],
                        "additionalProperties": false
                    },
                    "cypher": "MATCH (d:Doc) WHERE d.title CONTAINS $topic RETURN d.title AS title"
                }),
            );
        }
        RecipeCatalog::from_manifest_value(Some(&json!({
            "wide": {"description": "Wide catalogue.", "queries": queries}
        })))
        .expect("valid catalog")
    }

    /// The first threshold is still a cliff, so the test stands on both sides
    /// of it — but what is on the far side is now the *capped* form, not the
    /// names. One byte of prose too many costs prose, never the schema.
    #[test]
    fn a_block_past_the_ceiling_shortens_prose_and_keeps_every_parameter() {
        let budgets = CatalogBudgets::default();
        // Ascii padding, so a byte length is a character count: one `x` of
        // description is the baseline the padding is measured against.
        let base = block_at(&catalog("x"), FULL_HEADER, None).len();
        let at_ceiling = catalog(&"x".repeat(budgets.block - base + 1));
        assert_eq!(
            block_at(&at_ceiling, FULL_HEADER, None).len(),
            budgets.block
        );
        assert_eq!(
            catalog_block(&at_ceiling, &budgets),
            (block_at(&at_ceiling, FULL_HEADER, None), CatalogForm::Full),
            "a block that exactly fits is published in full"
        );

        let past_ceiling = catalog(&"x".repeat(budgets.block - base + 2));
        assert_eq!(
            block_at(&past_ceiling, FULL_HEADER, None).len(),
            budgets.block + 1
        );
        let (rendered, form) = catalog_block(&past_ceiling, &budgets);
        assert_eq!(form, CatalogForm::Capped);
        assert_eq!(
            rendered,
            format!(
                "{CAPPED_HEADER}\nreview.search — {}…; params: limit: integer =5, \
                 query: string (required)",
                "x".repeat(budgets.description)
            ),
            "the description is cut to the budget and marked; the parameters are untouched"
        );
    }

    /// Largest-first: the entries that fit keep their prose whole, and only
    /// the ones above the level are cut — to the level, not below it.
    #[test]
    fn shortening_is_largest_first_and_stops_at_the_description_budget() {
        let budgets = CatalogBudgets::default();
        let mut lengths = vec![1_500; 12];
        lengths.extend([50, 40]);
        let wide = wide_catalog(&lengths);
        assert!(
            block_at(&wide, FULL_HEADER, None).len() > budgets.block,
            "the fixture must be over the ceiling, or it proves nothing"
        );

        let (rendered, form) = catalog_block(&wide, &budgets);
        assert_eq!(form, CatalogForm::Capped);
        assert!(rendered.len() <= budgets.block, "{}", rendered.len());
        assert_eq!(
            rendered
                .matches(&format!("{}…", "d".repeat(budgets.description)))
                .count(),
            12,
            "every over-budget description is cut to exactly the budget"
        );
        for short in [50, 40] {
            assert!(
                rendered.contains(&format!("— {}; params:", "d".repeat(short))),
                "a description under the level is published whole"
            );
        }
        assert_eq!(
            rendered
                .matches("; params: topic: string (required)")
                .count(),
            14,
            "every parameter line survives byte for byte: {rendered}"
        );
    }

    /// The floor: names *and* parameters. Names alone are reached only when
    /// even that does not fit — a catalogue of hundreds of queries, which is
    /// the case the fallback was written for.
    #[test]
    fn names_only_is_reached_only_when_names_and_parameters_alone_overflow() {
        let budgets = CatalogBudgets::default();

        // 250 entries: prose far over the ceiling, names + parameters still
        // under it, so the floor is what gets published.
        let floor = wide_catalog(&vec![400; 250]);
        let (rendered, form) = catalog_block(&floor, &budgets);
        assert_eq!(form, CatalogForm::Capped);
        assert_eq!(
            rendered
                .matches("; params: topic: string (required)")
                .count(),
            250
        );

        let overflowing = wide_catalog(&vec![400; 600]);
        assert!(
            block_at(&overflowing, CAPPED_HEADER, Some(0)).len() > budgets.block,
            "the fixture must overflow on names and parameters alone"
        );
        let (rendered, form) = catalog_block(&overflowing, &budgets);
        assert_eq!(form, CatalogForm::Names);
        assert!(rendered.starts_with(NAMES_HEADER), "{rendered}");
        assert!(
            !rendered.contains("params:"),
            "the names-only form carries no parameters"
        );
    }

    /// A query served under its own tool name says so in the catalogue: the
    /// agent reading this block otherwise has two routes to the same query
    /// and no reason to prefer the cheaper one.
    #[test]
    fn a_query_with_its_own_tool_is_marked_in_the_block() {
        let named = RecipeCatalog::from_manifest_value(Some(&json!({
            "review": {
                "description": "Review operations.",
                "queries": {
                    "search": {
                        "description": "Find a document.",
                        "tool": "find_document",
                        "parameters": {
                            "type": "object",
                            "properties": {"query": {"type": "string"}},
                            "required": ["query"],
                            "additionalProperties": false
                        },
                        "cypher": "MATCH (d:Doc) WHERE d.title CONTAINS $query RETURN d.title AS title"
                    }
                }
            }
        })))
        .expect("valid catalog");
        let (rendered, _) = catalog_block(&named, &CatalogBudgets::default());
        assert!(
            rendered.ends_with("params: query: string (required) → tool: find_document"),
            "{rendered}"
        );
        let (plain, _) = catalog_block(&catalog("x"), &CatalogBudgets::default());
        assert!(!plain.contains("→ tool:"), "{plain}");
    }

    #[test]
    fn the_manifest_sets_both_budgets_and_refuses_anything_else() {
        assert_eq!(
            CatalogBudgets::from_manifest_value(None).expect("absent block"),
            CatalogBudgets {
                block: 16_000,
                description: 600
            }
        );
        let set = CatalogBudgets::from_manifest_value(Some(
            &json!({"block_budget": 4_000, "description_budget": 100}),
        ))
        .expect("both keys");
        assert_eq!(
            set,
            CatalogBudgets {
                block: 4_000,
                description: 100
            }
        );
        assert_eq!(
            CatalogBudgets::from_manifest_value(Some(&json!({"description_budget": 0})))
                .expect("one key")
                .block,
            16_000,
            "an omitted key keeps its default"
        );

        // The operator's budgets are what the block is rendered against.
        let (rendered, form) = catalog_block(&help_desk_catalog(), &set);
        assert_eq!(form, CatalogForm::Capped);
        assert!(rendered.len() <= 4_000, "{}", rendered.len());
        assert_eq!(
            rendered.matches(&format!("{}…", "d".repeat(100))).count(),
            7
        );

        for bad in [
            json!([]),
            json!({"block_budget": "4000"}),
            json!({"block_budget": -1}),
            json!({"block_budget": 0}),
            json!({"descripton_budget": 100}),
        ] {
            assert!(
                CatalogBudgets::from_manifest_value(Some(&bad)).is_err(),
                "{bad} must fail the boot rather than be silently ignored"
            );
        }
    }

    /// `list_recipe_queries` is the pointer the run tool's description sends
    /// an agent to, so it has to describe the form that was actually rendered
    /// — the 0.17.11 contradiction was a degraded block still claiming the
    /// routing was complete elsewhere.
    #[test]
    fn the_list_description_states_the_form_that_was_rendered() {
        let full = list_tool_description(CatalogForm::Full);
        let capped = list_tool_description(CatalogForm::Capped);
        let names = list_tool_description(CatalogForm::Names);
        assert!(full.contains("call this only for a recipe not listed there"));
        assert!(capped.contains("descriptions shortened to fit"));
        assert!(names.contains("names only"));
        assert!(full != capped && capped != names);
    }

    /// The seven `rms_help` queries an operator measured on 0.17.11
    /// (descriptions 703/682/642/573/528/416/407 characters, one or two
    /// required string parameters each): a help-desk catalogue whose
    /// descriptions are routing paragraphs, which is what they are read for.
    fn help_desk_catalog() -> RecipeCatalog {
        let mut queries = serde_json::Map::new();
        for (index, length) in [703, 682, 642, 573, 528, 416, 407].into_iter().enumerate() {
            queries.insert(
                format!("query_{index}"),
                json!({
                    "description": "d".repeat(length),
                    "parameters": {
                        "type": "object",
                        "properties": {"topic": {"type": "string"}},
                        "required": ["topic"],
                        "additionalProperties": false
                    },
                    "cypher": "MATCH (d:Doc) WHERE d.title CONTAINS $topic RETURN d.title AS title"
                }),
            );
        }
        RecipeCatalog::from_manifest_value(Some(&json!({
            "rms_help": {"description": "Help-desk operations.", "queries": queries}
        })))
        .expect("valid catalog")
    }

    #[test]
    fn a_help_desk_catalogue_publishes_every_description_and_parameter() {
        let (rendered, form) = catalog_block(&help_desk_catalog(), &CatalogBudgets::default());
        assert_eq!(form, CatalogForm::Full);
        assert!(
            rendered.contains(&"d".repeat(703)),
            "the longest routing paragraph is published whole: {rendered}"
        );
        assert_eq!(
            rendered.matches("params: topic: string (required)").count(),
            7,
            "every parameter line survives: {rendered}"
        );
    }

    #[test]
    fn an_empty_catalogue_publishes_the_static_sentence_alone() {
        assert_eq!(
            run_tool_description(&RecipeCatalog::default(), &CatalogBudgets::default()).0,
            RUN_SUMMARY,
            "a server with no catalogue registers no route, and the text must \
             not imply one"
        );
    }
}
