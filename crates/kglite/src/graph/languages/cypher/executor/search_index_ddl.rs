//! Execution of `CREATE FULLTEXT INDEX` and `CREATE VECTOR INDEX`.
//!
//! Both statements parse in full; execution refuses them with the binding
//! route that does work.

use super::super::ast::*;
use super::super::result::MutationStats;
use crate::graph::dir_graph::DirGraph;

pub(super) fn execute_create_fulltext_index(
    _graph: &mut DirGraph,
    _create: &CreateFulltextIndex,
) -> Result<MutationStats, String> {
    Err(
        "CREATE FULLTEXT INDEX is not supported yet. Use your binding's \
         build_text_index(node_type, property) — every binding reaches it (Python, Rust, and \
         the C ABI's kglite_session_build_text_index, which Java and the other C consumers \
         wrap) — then rank with `text_bm25(n, 'property', 'query')`. Run `SHOW INDEXES` to \
         see what is installed."
            .to_string(),
    )
}

pub(super) fn execute_create_vector_index(
    _graph: &mut DirGraph,
    _create: &CreateVectorIndex,
) -> Result<MutationStats, String> {
    Err(
        "CREATE VECTOR INDEX is not supported yet: a KGLite vector index is built over an \
         embedding store, not over a stored list property. Use your binding's \
         build_vector_index(node_type, text_column, ...) — every binding reaches it (Python, \
         Rust, and the C ABI's kglite_session_build_vector_index, which Java and the other C \
         consumers wrap). Run `SHOW INDEXES` to see what is installed."
            .to_string(),
    )
}
