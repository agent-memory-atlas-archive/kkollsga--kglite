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
use crate::graph::schema::DirGraph;
use crate::graph::skills;

use super::describe::xml_escape;

/// Write every agent-guidance section a description carries, in order.
pub(crate) fn write_agent_guidance(xml: &mut String, graph: &DirGraph, surface: DescribeSurface) {
    write_skills(xml, graph, surface);
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
