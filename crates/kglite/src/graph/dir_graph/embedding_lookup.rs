//! Embedding-store lookups on [`DirGraph`], split from `dir_graph/mod.rs`
//! (source-quality line ceiling).

use crate::graph::schema::{DirGraph, EmbeddingStore};

impl DirGraph {
    /// [`Self::embedding_store`] that first checks a mapped store against its
    /// recorded CRC32 (once per mapping). The lookup every query path uses;
    /// a corrupt mapped store is an `Err`, not a wrong ranking.
    pub fn embedding_store_verified(
        &self,
        node_type: &str,
        prop_name: &str,
    ) -> Result<Option<&EmbeddingStore>, String> {
        let Some(store) = self.embedding_store(node_type, prop_name) else {
            return Ok(None);
        };
        store
            .verify_storage()
            .map_err(|error| format!("embedding store '{node_type}.{prop_name}': {error}"))?;
        Ok(Some(store))
    }

    /// Look up an embedding store by `(&str, &str)`: a linear scan of the
    /// embeddings map (typically 1-3 entries), so no owned `String` key has to
    /// be allocated for a `HashMap` probe.
    #[inline]
    pub fn embedding_store(&self, node_type: &str, prop_name: &str) -> Option<&EmbeddingStore> {
        self.embeddings
            .iter()
            .find(|((nt, pn), _)| nt == node_type && pn == prop_name)
            .map(|(_, store)| store)
    }
}
