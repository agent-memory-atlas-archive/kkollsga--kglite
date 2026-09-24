//! How `describe()` shows relationship embedding stores and relationship
//! retrieval lanes.
//!
//! Node stores render as an `<embeddings text_col= dim= count=/>` child of their
//! node type. Relationship stores use the same spelling in the connection
//! detail view, and in the self-closing `<conn>` map lines they become an
//! additive `embeddings="text_col(dim=D,count=N)"` attribute, so a parser that
//! reads those lines sees no new element. A graph without relationship stores
//! gets no relationship attribute, element or hint text.

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

const NODE_SEMANTIC: &str = "text_score(n, 'col', 'query'|[0.1,0.2,...], metric) — similarity; a list query is scored as your query vector, a string query is embedded via set_embedder() (metric: 'cosine'|'poincare'|'dot_product'|'euclidean'); embedding_norm(n, 'col_emb') — L2 norm (hierarchy depth in Poincaré space)";

const RELATIONSHIP_SEMANTIC: &str = "relationships: vector_score(r, 'col_emb', $v) / text_score(r, 'col', 'query'|[...]) score a matched relationship (ORDER BY … DESC LIMIT k is served from the store, through HNSW once indexed; {exact:true} forces exact); CALL db.edge_embeddings.query({type:'T' | types:['A','B'], text_property:'col', vector:$v | text:'query', top_k:10}) YIELD relationship, score, search_method, type ranks a whole store (or several, merged) (HNSW once db.edge_embeddings.build_index has run; deleting an embedded relationship or an endpoint drops that index to none until build_index runs again, and refresh_index refuses while there is none)";

/// The `<semantic>` hint line, when the graph carries a node or a relationship
/// store. A node-only graph gets the node line alone.
pub(super) fn semantic_hint(graph: &DirGraph) -> Option<String> {
    hint_line(
        "semantic",
        [
            (!graph.embeddings.is_empty()).then_some(NODE_SEMANTIC),
            (!graph.edge_embeddings.is_empty()).then_some(RELATIONSHIP_SEMANTIC),
        ],
    )
}

const NODE_LEXICAL: &str = "text_bm25(n, 'prop', 'query text') — BM25 relevance of the node's indexed text; 0.0 = indexed but shares no word with the query, null = no document for that row. Build with build_text_index(node_type, property).";

const RELATIONSHIP_LEXICAL: &str = "relationships: text_bm25(r, 'prop', 'query text') over an index built with CALL db.edge_text_index.build({type:'T', property:'prop'})";

const NODE_HYBRID: &str = "score_fuse(text_bm25(n, 'prop', $q), vector_score(n, 'col_emb', $qv)) — one score from both lanes (weights: a trailing list, e.g. [0.7, 0.3]). A lane that cannot see a row scores null and drops out of the average rather than zeroing it; all lanes absent = null. Rank with ORDER BY … DESC LIMIT k.";

const RELATIONSHIP_HYBRID: &str =
    "relationships: score_fuse(text_bm25(r, 'prop', $q), vector_score(r, 'prop_emb', $qv))";

/// Join the parts that apply into one hint line, or `None` when none does.
fn hint_line(element: &str, parts: [Option<&str>; 2]) -> Option<String> {
    let parts: Vec<&str> = parts.into_iter().flatten().collect();
    (!parts.is_empty()).then(|| format!("    <{element} hint=\"{}\"/>\n", parts.join("; ")))
}

/// The `<lexical>` hint: node text indexes, relationship text indexes, or both.
/// A graph with node text indexes only gets exactly its historical line.
pub(super) fn lexical_hint(graph: &DirGraph) -> Option<String> {
    hint_line(
        "lexical",
        [
            (!graph.text_indexes.is_empty()).then_some(NODE_LEXICAL),
            (!graph.edge_text_indexes.is_empty()).then_some(RELATIONSHIP_LEXICAL),
        ],
    )
}

/// The `<hybrid>` hint, for each entity that carries both retrieval lanes.
pub(super) fn hybrid_hint(graph: &DirGraph) -> Option<String> {
    hint_line(
        "hybrid",
        [
            (!graph.embeddings.is_empty() && !graph.text_indexes.is_empty()).then_some(NODE_HYBRID),
            (!graph.edge_embeddings.is_empty() && !graph.edge_text_indexes.is_empty())
                .then_some(RELATIONSHIP_HYBRID),
        ],
    )
}

#[cfg(test)]
#[path = "embeddings_view_tests.rs"]
mod tests;
