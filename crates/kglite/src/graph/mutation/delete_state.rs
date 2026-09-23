//! Node-deletion's above-storage half.
//!
//! `GraphWrite::remove_node` takes the node out of the backend. Four pieces
//! of state that a deleted node owns live one layer *above* the backend, on
//! `DirGraph` itself, so nothing inside storage can see them go — and each is
//! readable only while the node still exists. They are removed here, in the
//! same loop as the backend removal and in the order that keeps each read
//! valid.
//!
//! Extracted from `maintain.rs` to keep that file under the god-file LoC
//! ceiling. The index/bucket sweeps that follow a deletion stay there, in
//! `detach_delete_nodes`, which is this module's only caller.

use std::collections::HashSet;

use petgraph::graph::NodeIndex;

use crate::graph::schema::DirGraph;
use crate::graph::storage::GraphWrite;

/// Drop `node_idx`'s vector from every embedding store that holds one, and
/// journal each removal so a statement rollback can put it back.
///
/// **Why deletion must reach this map.** `EmbeddingStore` is keyed by the
/// global `NodeIndex`, and `StableDiGraph` hands a freed index straight to
/// the next node created. A vector left behind is therefore not merely stale
/// bookkeeping: the next node to land on that slot — of *any* type, embedded
/// or not — inherits it and comes back as a full-similarity top hit from
/// `vector_search`, on both the scan and the HNSW path.
///
/// The store's HNSW index is journalled with the vectors. `remove_embedding`
/// invalidates it and the undo's `restore_embedding` invalidates it again, so
/// without the captured state a rolled-back `DELETE` gave the vectors back and
/// kept the index dropped — `list_vector_indexes` reporting no index for one
/// the statement never touched. Taking the state costs nothing on the removing
/// path, since the removal drops it anyway; a store with no index is skipped.
///
/// Costs one hash probe per store, and stores are per `(node_type, property)`
/// — a handful, independent of graph size. The `is_empty` guard keeps the
/// overwhelmingly common un-embedded graph at zero cost per deleted node.
fn prune_doomed_embeddings(graph: &mut DirGraph, node_idx: NodeIndex) {
    if graph.embeddings.is_empty() {
        return;
    }
    let node = node_idx.index();
    let journalling = graph.graph.undo_journal_mut().is_some();
    let mut removed: Vec<((String, String), _)> = Vec::new();
    let mut indexes: Vec<((String, String), _)> = Vec::new();
    for (key, store) in graph.embeddings.iter_mut() {
        let prior_index =
            (journalling && store.has_index()).then(|| (key.clone(), store.take_index_state()));
        let Some(prior) = store.remove_embedding(node) else {
            // This store held no vector for the node, so nothing invalidated
            // its index: give back what the probe took.
            if let Some((_, state)) = prior_index {
                store.restore_index_state(state);
            }
            continue;
        };
        indexes.extend(prior_index);
        removed.push((key.clone(), prior));
    }
    if removed.is_empty() {
        return;
    }
    if let Some(journal) = graph.graph.undo_journal_mut() {
        // Index entries first, so reverse replay lands them last — after every
        // `restore_embedding` has invalidated the index again.
        for (store_key, prior) in indexes {
            journal.note_vector_index_replaced(store_key, prior);
        }
        for (store_key, prior) in removed {
            journal.note_embedding_removed(store_key, node, prior);
        }
    }
}

/// Drop `node_idx`'s document from every text index that holds one.
///
/// **Why deletion must reach this map**, and why it is not journalled the way
/// [`prune_doomed_embeddings`] is. A text index addresses documents *by*
/// `NodeIndex`, and `StableDiGraph` hands a freed index straight to the next
/// node created, so a document left behind is inherited: the new node — of any
/// type, of any content — scores as the deleted one's text. That is a wrong
/// answer, so the prune is unconditional.
///
/// A rolled-back delete would therefore leave the node restored and its
/// document gone, so the prune journals `UndoEntry::TextDocPruned` — which
/// carries no pre-image and instead marks the slot for the next refresh to
/// re-read. Deriving the document again from the text the rollback restores is
/// both cheaper than keeping a second copy of every deleted document and
/// exactly what a rebuild would produce.
///
/// The journal only survives a *reversal*: a committed delete discards it, so
/// deleting a million nodes prunes a million documents and leaves the index
/// with an empty dirty set. Deletion is not staleness.
///
/// Costs one hash probe per index, and indexes are per `(node_type, property)`
/// — a handful, independent of graph size. The `is_empty` guard keeps the
/// overwhelmingly common un-indexed graph at zero cost per deleted node.
fn prune_doomed_text_docs(graph: &mut DirGraph, node_idx: NodeIndex) {
    if graph.text_indexes.is_empty() {
        return;
    }
    // Outside a statement window there is nothing to reverse, so the prune
    // stays what it was: no key clones, no journal vector, on the path a
    // million-node delete takes.
    if graph.graph.undo_journal_mut().is_none() {
        for store in graph.text_indexes.values_mut() {
            store.remove_node(node_idx);
        }
        return;
    }
    let pruned: Vec<(String, String)> = graph
        .text_indexes
        .iter_mut()
        .filter_map(|(key, store)| store.remove_node(node_idx).then(|| key.clone()))
        .collect();
    let Some(journal) = graph.graph.undo_journal_mut() else {
        return;
    };
    for store_key in pruned {
        journal.note_text_doc_pruned(store_key, node_idx.index());
    }
}

/// Remove each doomed node from storage, carrying the four pieces of state
/// that live *above* storage and so cannot be recovered afterwards.
///
/// All four are read while the node still exists and are lost the moment it
/// does not, which is why they are here rather than in `detach_delete_nodes`'
/// sweeps:
///
/// - **The change-capture before-image's labels.** The capture wrapper reads
///   the node's properties and title as it removes it, but secondary labels
///   live in `DirGraph::secondary_label_index`, one layer above the backend.
///   A delete is the one event whose only informative half is `before`, so an
///   image missing its labels is the whole loss.
/// - **The dropped timeseries entry.** `timeseries_store` is O(V) and so is
///   deliberately not part of the checkpoint's schema clone; statement
///   rollback recovers it from the undo journal instead.
/// - **The node's embeddings.** Same ownership story as the timeseries, with a
///   sharper failure mode: the freed `NodeIndex` is reused, so a vector left
///   behind is inherited rather than merely orphaned. See
///   [`prune_doomed_embeddings`].
/// - **The node's text-index documents.** The same inheritance hazard, one
///   layer further up: a BM25 document is addressed by `NodeIndex` directly.
///   See [`prune_doomed_text_docs`].
pub(super) fn remove_doomed_nodes(graph: &mut DirGraph, nodes_to_delete: &HashSet<NodeIndex>) {
    let captures_before = graph.graph.captures_before_images();
    for &node_idx in nodes_to_delete {
        let doomed_labels = captures_before.then(|| graph.secondary_label_names(node_idx));
        // Before the removal, while the slot still names this node: a disk
        // graph's persistent index bundles are mmap snapshots that no delete
        // path rewrites, and the slot may be handed straight back out to a node
        // with a different indexed value.
        crate::graph::index_freshness::write_hooks::note_node_removed(graph, node_idx);
        GraphWrite::remove_node(&mut graph.graph, node_idx);
        if let Some(labels) = doomed_labels {
            graph.graph.backfill_node_before_labels(node_idx, labels);
        }
        prune_doomed_embeddings(graph, node_idx);
        prune_doomed_text_docs(graph, node_idx);
        let Some(prior) = graph.timeseries_store.remove(&node_idx.index()) else {
            continue;
        };
        if let Some(journal) = graph.graph.undo_journal_mut() {
            journal.note_timeseries_removed(node_idx.index(), prior);
        }
    }
}
