//! The graph-carried layer of the served recipe catalogue.
//!
//! A `.kgl` can ship the exact queries its skills name, so an operator who
//! serves such a graph gets the catalogue without copying Cypher into a
//! manifest. The manifest still wins: it is the one source the operator can
//! edit, and a deployment must be able to correct or replace a query the graph
//! ships without rebuilding the graph.
//!
//! Two sources, two failure rules, deliberately different. A manifest query
//! that does not compile fails the boot — an operator typo is theirs to fix,
//! and they are looking at the file. A graph record that does not compile is
//! skipped with a warning: graph content is data, it may have been written by
//! a `CREATE` that bypassed validation entirely, and one bad node must not take
//! the rest of the catalogue — or the deployment — down with it.

use kglite::api::recipes::{self, RecipeCatalog, RecipeWarning};

use crate::tools::GraphState;
use crate::Mode;

/// What the graph contributed to the catalogue this boot serves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GraphRecipeStats {
    /// Compiled graph queries the merged catalogue serves as the graph wrote
    /// them.
    pub(crate) served: usize,
    /// Compiled graph queries the manifest defines under the same
    /// `(recipe, name)` and therefore replaces.
    pub(crate) overridden: usize,
    /// One `recipe/name: reason` per record that did not compile, in graph
    /// order.
    pub(crate) skipped: Vec<String>,
}

impl GraphRecipeStats {
    /// Boot-summary fragment, or `None` when the graph carried no records at
    /// all — silence is the right report for the common case, and matches what
    /// the skills line does.
    pub(crate) fn summary(&self) -> Option<String> {
        if self.served == 0 && self.overridden == 0 && self.skipped.is_empty() {
            return None;
        }
        let mut text = format!("graph recipes: {} served", self.served);
        if self.overridden > 0 {
            text.push_str(&format!(", {} overridden by the manifest", self.overridden));
        }
        if !self.skipped.is_empty() {
            text.push_str(&format!(
                ", {} skipped: {}",
                self.skipped.len(),
                self.skipped.join("; ")
            ));
        }
        Some(text)
    }
}

/// Read the active graph's own catalogue, plus the records it refused.
///
/// **Graph and watch modes only**, for the reason `read_graph_skills` gives:
/// those are the two modes whose graph is open by the time the catalogue is
/// built. The workspace modes build theirs on first activation, long after the
/// routes are registered, and the catalogue is immutable after boot.
fn read_graph_catalogue(
    mode: &Mode,
    graph_state: &GraphState,
) -> (RecipeCatalog, Vec<RecipeWarning>) {
    if !matches!(mode, Mode::Graph { .. } | Mode::Watch { .. }) {
        return (RecipeCatalog::default(), Vec::new());
    }
    graph_state
        .with_kg(|kg| recipes::catalogue_from_graph(kg.dir()))
        .unwrap_or_default()
}

/// Lay the manifest catalogue over the graph's own and report what the graph
/// contributed.
pub(crate) fn merge_graph_recipes(
    mode: &Mode,
    graph_state: &GraphState,
    manifest: RecipeCatalog,
) -> (RecipeCatalog, GraphRecipeStats) {
    let (graph, warnings) = read_graph_catalogue(mode, graph_state);
    let mut stats = GraphRecipeStats::default();
    for warning in warnings {
        tracing::warn!(
            recipe = %warning.recipe,
            query = %warning.name,
            reason = %warning.reason,
            "graph-carried recipe query skipped"
        );
        stats.skipped.push(format!(
            "{}/{}: {}",
            warning.recipe, warning.name, warning.reason
        ));
    }
    for group in graph.recipes() {
        for query in group.queries() {
            let replaced = manifest
                .get(&group.name)
                .is_some_and(|manifest_group| manifest_group.get(&query.name).is_some());
            if replaced {
                stats.overridden += 1;
            } else {
                stats.served += 1;
            }
        }
    }
    (recipes::merge(graph, manifest), stats)
}

#[cfg(test)]
#[path = "graph_layer_tests.rs"]
mod graph_layer_tests;
