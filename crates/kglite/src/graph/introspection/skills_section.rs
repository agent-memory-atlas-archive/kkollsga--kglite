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
//! fetches the body, which can run to 16 KiB.

use crate::graph::introspection::DescribeSurface;
use crate::graph::schema::DirGraph;
use crate::graph::skills;

use super::describe::xml_escape;

/// Write every agent-guidance section a description carries, in order.
pub(crate) fn write_agent_guidance(xml: &mut String, graph: &DirGraph, surface: DescribeSurface) {
    write_skills(xml, graph, surface);
}

/// Write `<skills>`, or nothing when the graph carries none — an empty element
/// would tell a reader the feature exists and this graph opted out, which is
/// not what an ordinary graph is saying.
fn write_skills(xml: &mut String, graph: &DirGraph, surface: DescribeSurface) {
    let skills = skills::list(graph);
    if skills.is_empty() {
        return;
    }
    xml.push_str(&format!(
        "  <skills count=\"{}\" hint=\"Methodology this graph carries for working with itself. Read one with {}.\">\n",
        skills.len(),
        xml_escape(&fetch_call(surface, "name")),
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

/// How this surface fetches one skill body.
///
/// [`DescribeSurface::call`] renders "call describe again" hints and hard-codes
/// that verb, so it cannot spell this one. The MCP answer is not a tool at all:
/// a skill reaches an agent host through the prompt surface.
fn fetch_call(surface: DescribeSurface, name: &str) -> String {
    match surface {
        DescribeSurface::Python => format!("get_skill('{name}')"),
        DescribeSurface::Cli => format!("kglite skill GRAPH {name}"),
        DescribeSurface::Mcp => format!("prompts/get {name}"),
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
