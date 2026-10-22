//! Rebase a graph that serves its topology from a `.kgl` onto the file a save
//! just wrote.
//!
//! A write to a frozen graph lands in a copy-on-write overlay over the file's
//! topology, and the overlay never folds back, so it holds every write since
//! the load. A save writes the merged graph, which is exactly the state the
//! overlay and its base represent together. Mapping that new file and serving
//! the graph from it gives the same graph with an empty overlay: the heap
//! returns to what a fresh load holds and every cost that grew with the
//! overlay resets.
//!
//! ## What is replaced, and what is not
//!
//! The storage backend is: the topology, the column stores and the peer counts
//! all come from the new file. So are the node member lists and the id index,
//! which describe the same slots and which the new file serves in place. Everything
//! else on the `DirGraph` is keyed by node and edge slots, which a save preserves
//! (vacant slots stay vacant), or is state a load never owned (versions, the log
//! wrapper, the declared indexes and their built contents, embeddings, locks), so
//! it stays.
//! A reader holding the previous `Arc<DirGraph>` keeps the previous backend,
//! and with it the old mapping: the old file is unlinked by the save's rename,
//! and a Unix mapping outlives the unlink.
//!
//! ## When it declines
//!
//! The rebase is an optimisation of what a save leaves behind, never part of
//! the save, so every doubt keeps the graph as it is: a graph that is not an
//! overlay over a frozen base; a platform that copies sections rather than
//! mapping them (Windows); a file that did not record the raw topology
//! sections (a small graph, or `topology="memory"` policy); a file whose slot
//! space differs from the live graph's.

use std::sync::Arc;

use crate::graph::dir_graph::DirGraph;
use crate::graph::storage::mode::StorageMode;
use crate::graph::storage::GraphRead;

use super::{load_file_inner, LoadOptions};

/// Outcome of [`rebase_onto_saved`], for the tests and the diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rebase {
    /// The graph now serves from the saved file with an empty overlay.
    Done,
    /// The graph is not an overlay over a frozen base, or holds no writes.
    NoOverlay,
    /// Left as it was, for the reason given.
    Declined(&'static str),
}

/// Serve `graph` from the `.kgl` just saved at `path`. Never fails the save:
/// any problem leaves the graph untouched.
pub(crate) fn rebase_onto_saved(graph: &mut Arc<DirGraph>, path: &str) -> Rebase {
    if !graph.graph.has_frozen_base() || graph.graph.overlay_entries().is_none_or(|n| n == 0) {
        return Rebase::NoOverlay;
    }
    if cfg!(windows) {
        return Rebase::Declined("sections are copied, not mapped, on this platform");
    }
    let options = LoadOptions::new()
        .with_storage(StorageMode::Mapped)
        .with_defer_index_rebuild(true);
    let Ok(loaded) = load_file_inner(path, &options) else {
        return Rebase::Declined("the saved file did not load");
    };
    let Ok(mut fresh) = Arc::try_unwrap(loaded) else {
        return Rebase::Declined("the loaded graph is shared");
    };
    let live = &graph.graph;
    if !fresh.graph.is_frozen()
        || fresh.graph.node_count() != live.node_count()
        || fresh.graph.edge_count() != live.edge_count()
        || fresh.graph.node_bound() != live.node_bound()
        || fresh.graph.edge_bound() != live.edge_bound()
    {
        return Rebase::Declined("the saved file does not describe the live slot space");
    }
    // The new stores may be mapped from spill files under the loaded graph's
    // temp directories; they must live as long as the graph that now serves
    // them.
    let spill_dirs = fresh
        .temp_dirs
        .lock()
        .map(|mut dirs| std::mem::take(&mut *dirs))
        .unwrap_or_default();
    let dir = crate::graph::handle::make_dir_graph_mut_preserving_lineage(graph);
    let backend = std::mem::replace(&mut fresh.graph, crate::graph::schema::GraphBackend::new());
    if !dir.graph.rebase_frozen(backend) {
        return Rebase::Declined("the live backend is not an overlay over a frozen base");
    }
    // The member lists and the id index describe the same slots; the loaded
    // pair serves from the file's sections, where the live pair holds the
    // heap maps the writes (and the lookups after them) built.
    dir.type_indices = std::mem::take(&mut fresh.type_indices);
    dir.id_indices = std::mem::take(&mut fresh.id_indices);
    if let Ok(mut dirs) = dir.temp_dirs.lock() {
        dirs.extend(spill_dirs);
    }
    dir.mapped_origin = fresh.mapped_origin.clone();
    dir.reset_write_budget_after_rebase(std::fs::metadata(path).map_or(0, |m| m.len()));
    Rebase::Done
}
