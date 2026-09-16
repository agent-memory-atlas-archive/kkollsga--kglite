//! The agent-guidance sections of a graph description.
//!
//! What a graph carries *about itself*, as opposed to what it contains: the
//! `<skills>` index below, and the catalogue of graph-carried recipe queries
//! that joins it here. Both are written by [`write_agent_guidance`], which is
//! the single call the inventory builders make — a builder that renders one
//! and not the other is the failure mode this seam exists to prevent.
//!
//! A skill's label is hidden from every type enumeration, so this index is the
//! only route a reader has from a description to the methodology the graph
//! ships. It stays a catalogue: name, one-line summary, and the call that
//! fetches the body, which can run to 16 KiB. The MCP surface is the exception
//! — it serves a merged registry rather than this graph's records; see
//! [`fetch_call`].

use crate::graph::introspection::DescribeSurface;
use crate::graph::recipes;
use crate::graph::schema::DirGraph;
use crate::graph::skills;

use super::describe::xml_escape;

/// Write every agent-guidance section a description carries, in order.
pub(crate) fn write_agent_guidance(xml: &mut String, graph: &DirGraph, surface: DescribeSurface) {
    write_skills(xml, graph, surface);
    write_recipes(xml, graph, surface);
}

/// Write `<skills>`, or nothing when the graph carries none — an empty element
/// would tell a reader the feature exists and this graph opted out, which is
/// not what an ordinary graph is saying. Also nothing on a surface that serves
/// skills from somewhere other than this graph; see [`fetch_call`].
fn write_skills(xml: &mut String, graph: &DirGraph, surface: DescribeSurface) {
    let Some(fetch) = fetch_call(surface, "name") else {
        return;
    };
    let skills = skills::list(graph);
    if skills.is_empty() {
        return;
    }
    xml.push_str(&format!(
        "  <skills count=\"{}\" hint=\"Methodology this graph carries for working with itself. Read one with {}.\">\n",
        skills.len(),
        xml_escape(&fetch),
    ));
    for skill in &skills {
        xml.push_str(&format!(
            "    <skill name=\"{}\" description=\"{}\"/>\n",
            xml_escape(&skill.name),
            xml_escape(&summary(&skill.description)),
        ));
    }
    xml.push_str("  </skills>\n");
}

/// Write `<recipes>`, one child per group rather than per query: a catalogue
/// is read by an agent choosing a group, and a graph can carry dozens of
/// queries whose names mean nothing without their schemas. Absent when the
/// graph carries none, and on the MCP surface, which has its own
/// `<query-catalog/>` hint over the catalogue it actually merged and serves.
fn write_recipes(xml: &mut String, graph: &DirGraph, surface: DescribeSurface) {
    let Some(fetch) = recipe_fetch_call(surface) else {
        return;
    };
    let records = recipes::list(graph);
    if records.is_empty() {
        return;
    }
    // `list` is sorted by `(recipe, name)`, so a group's members are
    // contiguous and its first record carries the description the catalogue
    // would serve — the same rule `catalogue_from_graph` applies.
    let mut groups: Vec<(&str, &str, usize)> = Vec::new();
    for record in &records {
        match groups.last_mut() {
            Some((name, _, count)) if *name == record.recipe => *count += 1,
            _ => groups.push((&record.recipe, &record.recipe_description, 1)),
        }
    }
    xml.push_str(&format!(
        "  <recipes count=\"{}\" hint=\"Named read-only queries this graph carries for itself. Read one with {}; an MCP server serves them as run_recipe_query.\">\n",
        groups.len(),
        xml_escape(&fetch),
    ));
    for (name, description, queries) in groups {
        xml.push_str(&format!(
            "    <recipe name=\"{}\" queries=\"{queries}\" description=\"{}\"/>\n",
            xml_escape(name),
            xml_escape(&summary(description)),
        ));
    }
    xml.push_str("  </recipes>\n");
}

/// How this surface reads one stored query, or `None` for a surface that does
/// not serve this graph's recipes.
///
/// There is no `kglite recipe` subcommand — a CLI user writes Cypher — so the
/// CLI is pointed at the label itself, which is the honest answer rather than
/// a verb that does not exist. The MCP server answers `None` for the same
/// reason it does for skills: its overview already reports the catalogue it
/// merged, and that catalogue is not this graph's records.
fn recipe_fetch_call(surface: DescribeSurface) -> Option<String> {
    match surface {
        DescribeSurface::Python => Some("get_recipe('recipe', 'name')".to_string()),
        DescribeSurface::Cli => Some(
            "kglite query GRAPH \"MATCH (r:KgliteRecipe) RETURN r.recipe, r.name, r.cypher\""
                .to_string(),
        ),
        DescribeSurface::Mcp => None,
    }
}

/// How this surface fetches one skill body, or `None` for a surface that does
/// not serve this graph's skills.
///
/// [`DescribeSurface::call`] renders "call describe again" hints and hard-codes
/// that verb, so it cannot spell this one.
///
/// The MCP server answers `None` because it appends its own skills index to the
/// bare overview, rendered from the registry it actually serves: the graph
/// layer merged under the operator's files, gated on the manifest opt-in, with
/// invalid records dropped. A second index straight from the graph would
/// disagree with it on every one of those — on a server that never opted in it
/// would advertise skills nothing serves, and point at a `prompts/get` that
/// returns nothing.
fn fetch_call(surface: DescribeSurface, name: &str) -> Option<String> {
    match surface {
        DescribeSurface::Python => Some(format!("get_skill('{name}')")),
        DescribeSurface::Cli => Some(format!("kglite skill GRAPH {name}")),
        DescribeSurface::Mcp => None,
    }
}

/// One line about one skill: the description's first sentence, or its first
/// 160 bytes. Newlines collapse so an entry is always a single element on a
/// single line, matching the bare-`graph_overview` skills index.
fn summary(description: &str) -> String {
    const MAX: usize = 160;
    let text = description.replace(['\n', '\r'], " ");
    let text = text.trim();
    let sentence_end = text.char_indices().find_map(|(index, ch)| {
        let after = index + ch.len_utf8();
        (ch == '.' && text[after..].chars().next().is_none_or(char::is_whitespace)).then_some(after)
    });
    match sentence_end {
        Some(end) if end <= MAX => text[..end].to_string(),
        _ if text.len() <= MAX => text.to_string(),
        _ => {
            let mut cut = MAX;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            format!("{}\u{2026}", text[..cut].trim_end())
        }
    }
}

#[cfg(test)]
#[path = "skills_section_tests.rs"]
mod skills_section_tests;
