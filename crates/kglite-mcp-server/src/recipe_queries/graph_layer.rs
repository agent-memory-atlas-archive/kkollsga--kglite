//! The layers of the served recipe catalogue that sit under the manifest's.
//!
//! A `.kgl` can ship the exact queries its skills name, so an operator who
//! serves such a graph gets the catalogue without copying Cypher into a
//! manifest; an embedding binary can ship the queries its own schema makes
//! answerable ([`ServerExtensions::with_recipes`]), which apply to every graph
//! it serves. The manifest still wins over both: it is the one source the
//! operator can edit, and a deployment must be able to correct or replace a
//! query without rebuilding the graph or the binary. Between the two lower
//! layers the graph wins, for the same reason — it is the more specific
//! statement, and the one closer to what is actually being served.
//!
//! Three sources, and the failure rules are deliberately not the same. A
//! manifest query that does not compile fails the boot — an operator typo is
//! theirs to fix, and they are looking at the file; a producer catalogue is
//! code and cannot even be constructed without compiling
//! (`RecipeCatalog::from_manifest_value` refuses it), so it fails the same way.
//! A graph record that does not compile is skipped with a warning: graph
//! content is data, it may have been written by a `CREATE` that bypassed
//! validation entirely, and one bad node must not take the rest of the
//! catalogue — or the deployment — down with it.

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

/// What the embedding binary's catalogue contributed to this boot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProducerRecipeStats {
    /// Compiled producer queries the merged catalogue serves as the binary
    /// wrote them.
    pub(crate) served: usize,
    /// Compiled producer queries a closer layer — the graph or the manifest —
    /// defines under the same `(recipe, name)` and therefore replaces.
    pub(crate) overridden: usize,
}

impl ProducerRecipeStats {
    /// Boot-summary fragment, or `None` when the binary carried no queries.
    /// No `skipped` twin: a producer catalogue that does not compile cannot be
    /// built, so nothing reaches here to skip.
    pub(crate) fn summary(&self) -> Option<String> {
        if self.served == 0 && self.overridden == 0 {
            return None;
        }
        let mut text = format!("producer recipes: {} served", self.served);
        if self.overridden > 0 {
            text.push_str(&format!(
                ", {} overridden by the graph or the manifest",
                self.overridden
            ));
        }
        Some(text)
    }
}

/// Is this exact `(recipe, name)` defined in `catalog`?
fn defines(catalog: &RecipeCatalog, recipe: &str, name: &str) -> bool {
    catalog
        .get(recipe)
        .is_some_and(|group| group.get(name).is_some())
}

/// Compose the three catalogue layers — `producer < graph < manifest` — and
/// report what each of the two lower ones contributed.
///
/// `recipes::merge(lower, higher)` composes associatively, so folding the
/// producer under the graph and the manifest over the pair is the same
/// catalogue however the sources were built.
pub(crate) fn merge_recipe_layers(
    mode: &Mode,
    graph_state: &GraphState,
    producer: RecipeCatalog,
    manifest: RecipeCatalog,
) -> (RecipeCatalog, ProducerRecipeStats, GraphRecipeStats) {
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
            if defines(&manifest, &group.name, &query.name) {
                stats.overridden += 1;
            } else {
                stats.served += 1;
            }
        }
    }
    let mut producer_stats = ProducerRecipeStats::default();
    for group in producer.recipes() {
        for query in group.queries() {
            if defines(&graph, &group.name, &query.name)
                || defines(&manifest, &group.name, &query.name)
            {
                producer_stats.overridden += 1;
            } else {
                producer_stats.served += 1;
            }
        }
    }
    let merged = recipes::merge(recipes::merge(producer, graph), manifest);
    (merged, producer_stats, stats)
}

#[cfg(test)]
#[path = "graph_layer_tests.rs"]
mod graph_layer_tests;
