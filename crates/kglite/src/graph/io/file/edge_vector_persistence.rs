//! The optional `edge_vector_index` `.kgl` section: built relationship HNSW
//! indexes, so a reloaded relationship store answers through HNSW without a
//! rebuild, exactly as a node store does.
//!
//! The payload is the node index's KGLVIDX1 payload (`vector_persistence`),
//! keyed by `(relationship type, embedding property)`. It is its own section
//! rather than extra entries in `vector_index` so node indexes never depend on
//! a reader understanding relationship entries.
//!
//! The section is written only when some relationship store carries a built
//! index, and its metadata key is skipped at zero, so every other graph —
//! node-only files in particular — writes exactly the bytes it wrote before the
//! section existed. It can only ever appear in a core v4 file (relationship
//! stores force v4), which no release before the one adding it can open, so no
//! older reader has to skip it.
//!
//! `.kgl` only: disk generations persist neither the node nor the relationship
//! index, and a reopened disk graph reports no index until it is rebuilt.

use super::vector_persistence::{attach_decoded_index, decode_index_payload, encode_index_payload};
use super::zstd_compress;
use crate::graph::schema::DirGraph;
use std::io;

/// Canonical `section_digests` key, and the section's name in errors.
pub(super) const EDGE_VECTOR_INDEX_SECTION: &str = "edge_vector_index";

/// The compressed section, or `None` when no relationship store is indexed.
pub(super) fn encode_edge_vector_index_section(graph: &DirGraph) -> io::Result<Option<Vec<u8>>> {
    let stores = graph
        .edge_embeddings
        .iter()
        .map(|((conn, prop), store)| (conn.as_str(), prop.as_str(), store.index_store()));
    encode_index_payload(stores)?
        .map(|payload| zstd_compress(&payload))
        .transpose()
}

/// Attach decoded relationship indexes to their stores. Best-effort like the
/// node section: an unreadable payload or an index whose shape no longer fits
/// its store is skipped, leaving the store unindexed. Must run after the
/// relationship stores are installed and their norms rebuilt.
pub(super) fn decode_edge_vector_indexes(payload: &[u8], graph: &mut DirGraph) {
    for mut entry in decode_index_payload(payload) {
        if let Some(store) = graph.edge_embeddings.get_mut(&entry.take_key()) {
            attach_decoded_index(store.index_store_mut(), entry);
        }
    }
}

#[cfg(test)]
#[path = "edge_vector_persistence_tests.rs"]
mod tests;
