//! Tests for the `<skills>` section of a graph description.

use super::*;
use crate::datatypes::values::Value;
use crate::graph::introspection::describe::{compute_description, DescribeRequest};
use crate::graph::introspection::DescribeSurface;
use crate::graph::recipes;
use crate::graph::schema::NodeData;
use crate::graph::skills::{self, SkillRecord};
use crate::graph::storage::GraphWrite;
use std::collections::HashMap;

fn push_node(graph: &mut DirGraph, node_type: &str, id: u32, props: &[(&str, Value)]) {
    let node = NodeData::new(
        Value::UniqueId(id),
        Value::String(format!("{node_type}-{id}")),
        node_type.to_string(),
        props
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect::<HashMap<_, _>>(),
        &mut graph.interner,
    );
    let idx = graph.graph.add_node(node);
    graph
        .type_indices
        .entry_or_default(node_type.to_string())
        .push(idx);
}

/// `visible_types` counts drive the describe tier, so a fixture that wants a
/// specific inventory builder controls the number of ordinary node types.
fn graph_with_types(count: u32) -> DirGraph {
    let mut graph = DirGraph::new();
    for i in 0..count {
        push_node(&mut graph, &format!("T{i}"), i, &[("v", Value::Int64(1))]);
    }
    graph
}

fn add_skill(graph: &mut DirGraph, id: u32, name: &str, description: &str) {
    skills::set(
        graph,
        &SkillRecord {
            name: name.to_string(),
            description: description.to_string(),
            body: format!("# {name}"),
            ..SkillRecord::default()
        },
    )
    .unwrap_or_else(|error| panic!("skill {name} (node {id}): {error}"));
}

fn describe(graph: &DirGraph) -> String {
    compute_description(graph, &DescribeRequest::new(DescribeSurface::Python)).unwrap()
}

/// The index is the only route an agent has to a graph's own methodology: the
/// label is hidden from every type listing, so a skill nobody can name is a
/// skill nobody can fetch.
#[test]
fn both_skills_are_indexed_in_name_order() {
    let mut graph = graph_with_types(2);
    add_skill(&mut graph, 100, "wells", "How to query wells.");
    add_skill(&mut graph, 101, "areas", "How to query areas.");

    let described = describe(&graph);
    let index = described
        .split_once("<skills ")
        .map(|(_, rest)| rest.split_once("</skills>").expect("a closed element").0)
        .unwrap_or_else(|| panic!("no skills section in: {described}"));
    assert!(index.starts_with("count=\"2\""), "got: {index}");
    let areas = index.find("name=\"areas\"").expect("areas");
    let wells = index.find("name=\"wells\"").expect("wells");
    assert!(areas < wells, "skills must be sorted by name: {index}");
    assert!(
        index.contains("description=\"How to query areas.\""),
        "{index}"
    );
}

/// Every describe tier renders the section or the index vanishes on exactly
/// the graphs big enough to need it. The tier boundaries are 15 / 200 / 5000
/// core types.
#[test]
fn every_inventory_tier_renders_the_index() {
    for types in [1u32, 20, 250, 5001] {
        let mut graph = graph_with_types(types);
        add_skill(&mut graph, 9000, "wells", "How to query wells.");
        let described = describe(&graph);
        assert!(
            described.contains("<skills count=\"1\""),
            "tier with {types} types dropped the index: {described}"
        );
        assert!(described.contains("name=\"wells\""), "{described}");
    }
}

/// A graph carrying no skill is byte-identical to one built before the section
/// existed — the element is absent, not empty.
#[test]
fn a_graph_without_skills_renders_no_section() {
    let described = describe(&graph_with_types(3));
    assert!(!described.contains("<skills"), "got: {described}");
}

/// The hint has to name a call the reader can actually make, and each surface
/// spells it differently.
#[test]
fn each_surface_is_told_its_own_fetch_call() {
    let mut graph = graph_with_types(2);
    add_skill(&mut graph, 100, "wells", "How to query wells.");
    for (surface, expected) in [
        (DescribeSurface::Python, "get_skill('name')"),
        (DescribeSurface::Cli, "kglite skill GRAPH name"),
    ] {
        let described =
            compute_description(&graph, &DescribeRequest::new(surface)).expect("a description");
        assert!(
            described.contains(expected),
            "{surface:?} was not told {expected}: {described}"
        );
    }
}

/// The MCP server renders its own skills index from the registry it serves —
/// the graph layer under the operator's files, gated on the manifest opt-in.
/// A second index straight from the graph would contradict it, and on a server
/// that never opted in it would advertise skills nothing serves.
#[test]
fn the_mcp_surface_gets_no_index_from_the_graph() {
    let mut graph = graph_with_types(2);
    add_skill(&mut graph, 100, "wells", "How to query wells.");
    let described = compute_description(&graph, &DescribeRequest::new(DescribeSurface::Mcp))
        .expect("a description");
    assert!(!described.contains("<skills"), "got: {described}");
    assert!(!described.contains("wells"), "got: {described}");
}

/// The section lands inside an XML attribute, so a description carrying markup
/// or a newline must not break the document.
#[test]
fn a_hostile_description_stays_inside_its_attribute() {
    let mut graph = graph_with_types(2);
    add_skill(
        &mut graph,
        100,
        "wells",
        "Use <MATCH> & \"quotes\"\nacross lines.",
    );
    let described = describe(&graph);
    let line = described
        .lines()
        .find(|line| line.contains("name=\"wells\""))
        .expect("a skill line");
    assert!(line.ends_with("/>"), "the entry must be one line: {line}");
    assert!(
        line.contains("&lt;MATCH&gt; &amp; &quot;quotes&quot;"),
        "{line}"
    );
}

// ── The recipe catalogue ───────────────────────────────────────────────────

fn add_recipe(graph: &mut DirGraph, recipe: &str, name: &str, group_description: &str) {
    recipes::set(
        graph,
        &recipes::RecipeRecord {
            recipe: recipe.to_string(),
            name: name.to_string(),
            description: format!("Query {name}."),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false,
            }),
            cypher: "RETURN 1 AS n".to_string(),
            recipe_description: group_description.to_string(),
            tool: None,
        },
    )
    .unwrap_or_else(|error| panic!("recipe {recipe}/{name}: {error}"));
}

/// One child per *group*, with its query count — an agent picks a recipe, then
/// asks the catalogue for its queries and their schemas.
#[test]
fn recipes_are_indexed_by_group_with_their_query_counts() {
    let mut graph = graph_with_types(2);
    add_recipe(&mut graph, "wells", "count", "Asking about wells.");
    add_recipe(&mut graph, "wells", "deepest", "Asking about wells.");
    add_recipe(&mut graph, "areas", "list", "Asking about areas.");

    let described = describe(&graph);
    let index = described
        .split_once("<recipes ")
        .map(|(_, rest)| rest.split_once("</recipes>").expect("a closed element").0)
        .unwrap_or_else(|| panic!("no recipes section in: {described}"));
    assert!(index.starts_with("count=\"2\""), "got: {index}");
    let areas = index.find("name=\"areas\"").expect("areas");
    let wells = index.find("name=\"wells\"").expect("wells");
    assert!(areas < wells, "groups must be sorted by name: {index}");
    assert!(index.contains("name=\"wells\" queries=\"2\""), "{index}");
    assert!(index.contains("name=\"areas\" queries=\"1\""), "{index}");
    assert!(
        index.contains("description=\"Asking about areas.\""),
        "{index}"
    );
}

/// A graph carrying no recipe is byte-identical to one built before the
/// section existed — the element is absent, not empty.
#[test]
fn a_graph_without_recipes_renders_no_section() {
    let described = describe(&graph_with_types(3));
    assert!(!described.contains("<recipes"), "got: {described}");
}

/// Every describe tier renders it, and each surface is told a call it can
/// actually make. The MCP server is excluded for the same reason it is
/// excluded from the skills index: its overview reports the catalogue it
/// merged and serves, which is not this graph's records.
#[test]
fn every_tier_renders_the_catalogue_and_each_surface_gets_its_own_call() {
    for types in [1u32, 20, 250, 5001] {
        let mut graph = graph_with_types(types);
        add_recipe(&mut graph, "wells", "count", "Asking about wells.");
        assert!(
            describe(&graph).contains("<recipes count=\"1\""),
            "tier with {types} types dropped the catalogue"
        );
    }

    let mut graph = graph_with_types(2);
    add_recipe(&mut graph, "wells", "count", "Asking about wells.");
    for (surface, expected) in [
        (DescribeSurface::Python, "get_recipe('recipe', 'name')"),
        (DescribeSurface::Cli, "kglite query GRAPH"),
    ] {
        let described =
            compute_description(&graph, &DescribeRequest::new(surface)).expect("a description");
        assert!(
            described.contains(expected),
            "{surface:?} was not told {expected}: {described}"
        );
        assert!(described.contains("run_recipe_query"), "{described}");
    }

    let described = compute_description(&graph, &DescribeRequest::new(DescribeSurface::Mcp))
        .expect("a description");
    assert!(!described.contains("<recipes"), "got: {described}");
}
