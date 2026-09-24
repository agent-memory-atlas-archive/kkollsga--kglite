//! How `describe()` shows relationship embedding stores.
//!
//! Node stores render as an `<embeddings text_col= dim= count=/>` child of their
//! node type. Relationship stores use the same spelling in the connection
//! detail view, and in the self-closing `<conn>` map lines they become an
//! additive `embeddings="text_col(dim=D,count=N)"` attribute, so a parser that
//! reads those lines sees no new element. A graph without relationship stores
//! renders byte-identically to before.

use crate::graph::embeddings::text_column_of;
use crate::graph::schema::DirGraph;

use super::describe::xml_escape;

/// `(text column, dimension, count)` for each store on `connection_type`,
/// sorted by column.
fn stores_on(graph: &DirGraph, connection_type: &str) -> Vec<(String, usize, usize)> {
    let mut stores: Vec<(String, usize, usize)> = graph
        .edge_embeddings
        .iter()
        .filter(|((conn, _), _)| conn == connection_type)
        .map(|((_, name), store)| {
            (
                text_column_of(name).unwrap_or(name).to_string(),
                store.dimension(),
                store.len(),
            )
        })
        .collect();
    stores.sort();
    stores
}

/// The ` embeddings="…"` attribute for a `<conn>` map line; empty when the
/// relationship type carries no store.
pub(super) fn conn_embeddings_attr(graph: &DirGraph, connection_type: &str) -> String {
    let stores = stores_on(graph, connection_type);
    if stores.is_empty() {
        return String::new();
    }
    let rendered: Vec<String> = stores
        .iter()
        .map(|(column, dim, count)| format!("{}(dim={dim},count={count})", xml_escape(column)))
        .collect();
    format!(" embeddings=\"{}\"", rendered.join(","))
}

/// One `<embeddings/>` child per store, for the connection detail view.
pub(super) fn write_conn_embeddings(xml: &mut String, graph: &DirGraph, connection_type: &str) {
    for (column, dim, count) in stores_on(graph, connection_type) {
        xml.push_str(&format!(
            "    <embeddings text_col=\"{}\" dim=\"{dim}\" count=\"{count}\"/>\n",
            xml_escape(&column)
        ));
    }
}

const NODE_SEMANTIC: &str = "text_score(n, 'col', 'query'|[0.1,0.2,...], metric) — similarity; a list query is scored as your query vector, a string query is embedded via set_embedder() (metric: 'cosine'|'poincare'|'dot_product'|'euclidean'); embedding_norm(n, 'col') — L2 norm (hierarchy depth in Poincaré space)";

const RELATIONSHIP_SEMANTIC: &str = "relationships: vector_score(r, 'col_emb', $v) / text_score(r, 'col', 'query'|[...]) score a matched relationship exactly; CALL db.edge_embeddings.query({type:'T', text_property:'col', vector:$v | text:'query', top_k:10}) YIELD relationship, score, search_method ranks a whole store (HNSW once db.edge_embeddings.build_index has run)";

/// The `<semantic>` hint line, when the graph carries a node or a relationship
/// store. A node-only graph gets exactly its historical line.
pub(super) fn semantic_hint(graph: &DirGraph) -> Option<String> {
    let parts: Vec<&str> = [
        (!graph.embeddings.is_empty()).then_some(NODE_SEMANTIC),
        (!graph.edge_embeddings.is_empty()).then_some(RELATIONSHIP_SEMANTIC),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| format!("    <semantic hint=\"{}\"/>\n", parts.join("; ")))
}

#[cfg(test)]
#[path = "embeddings_view_tests.rs"]
mod tests;
