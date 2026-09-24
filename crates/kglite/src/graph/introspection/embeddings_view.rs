//! How `describe()` shows relationship embedding stores and relationship
//! retrieval lanes.
//!
//! Node stores render as an `<embeddings text_col= dim= count=/>` child of their
//! node type. Relationship stores use the same spelling in the connection
//! detail view, and in the self-closing `<conn>` map lines they become an
//! additive `embeddings="text_col(dim=D,count=N)"` attribute, so a parser that
//! reads those lines sees no new element. A graph without relationship stores
//! gets no relationship attribute, element or hint text.
//!
//! Index presence is spelled the same way for both entities, and only when an
//! index exists (a graph without one renders byte-identically): an HNSW index
//! adds `index="hnsw"` to the `<embeddings/>` element (`,hnsw` inside the
//! `<conn>` attribute's parentheses), and a BM25 text index is a
//! `<text_index property="p"/>` element (a `text_index="p,…"` attribute on the
//! `<conn>` line).

use std::collections::HashMap;

use crate::graph::embeddings::text_column_of;
use crate::graph::schema::DirGraph;

use super::describe::xml_escape;

/// One store as `describe()` renders it.
struct StoreView {
    text_col: String,
    dim: usize,
    count: usize,
    hnsw: bool,
}

/// Each store on `connection_type`, sorted by column.
fn stores_on(graph: &DirGraph, connection_type: &str) -> Vec<StoreView> {
    let mut stores: Vec<StoreView> = graph
        .edge_embeddings
        .iter()
        .filter(|((conn, _), _)| conn == connection_type)
        .map(|((_, name), store)| StoreView {
            text_col: text_column_of(name).unwrap_or(name).to_string(),
            dim: store.dimension(),
            count: store.len(),
            hnsw: store.index_store().has_index(),
        })
        .collect();
    stores.sort_by(|left, right| left.text_col.cmp(&right.text_col));
    stores
}

/// The ` embeddings="…"` attribute for a `<conn>` map line, plus
/// ` text_index="…"` when the type has BM25 indexes; empty when the
/// relationship type carries neither.
pub(super) fn conn_embeddings_attr(graph: &DirGraph, connection_type: &str) -> String {
    let stores = stores_on(graph, connection_type);
    let mut attrs = String::new();
    if !stores.is_empty() {
        let rendered: Vec<String> = stores
            .iter()
            .map(|store| {
                format!(
                    "{}(dim={},count={}{})",
                    xml_escape(&store.text_col),
                    store.dim,
                    store.count,
                    if store.hnsw { ",hnsw" } else { "" }
                )
            })
            .collect();
        attrs.push_str(&format!(" embeddings=\"{}\"", rendered.join(",")));
    }
    let text = text_index_properties(&graph.edge_text_indexes, connection_type);
    if !text.is_empty() {
        let escaped: Vec<String> = text.iter().map(|p| xml_escape(p)).collect();
        attrs.push_str(&format!(" text_index=\"{}\"", escaped.join(",")));
    }
    attrs
}

/// The BM25-indexed properties of one node or relationship type, sorted.
fn text_index_properties<V>(
    indexes: &HashMap<(String, String), V>,
    type_name: &str,
) -> Vec<String> {
    let mut properties: Vec<String> = indexes
        .keys()
        .filter(|(owner, _)| owner == type_name)
        .map(|(_, property)| property.clone())
        .collect();
    properties.sort();
    properties
}

/// `<text_index property="p"/>` lines for one type's BM25 indexes.
fn write_text_indexes<V>(
    xml: &mut String,
    indent: &str,
    indexes: &HashMap<(String, String), V>,
    type_name: &str,
) {
    for property in text_index_properties(indexes, type_name) {
        xml.push_str(&format!(
            "{indent}<text_index property=\"{}\"/>\n",
            xml_escape(&property)
        ));
    }
}

/// One `<embeddings/>` child per store, then one `<text_index/>` per BM25
/// index, for the connection detail view.
pub(super) fn write_conn_embeddings(xml: &mut String, graph: &DirGraph, connection_type: &str) {
    for store in stores_on(graph, connection_type) {
        xml.push_str(&format!(
            "    <embeddings text_col=\"{}\" dim=\"{}\" count=\"{}\"{}/>\n",
            xml_escape(&store.text_col),
            store.dim,
            store.count,
            if store.hnsw { " index=\"hnsw\"" } else { "" }
        ));
    }
    write_text_indexes(xml, "    ", &graph.edge_text_indexes, connection_type);
}

/// The node twin of [`write_conn_embeddings`]: `<embeddings/>` per store and
/// `<text_index/>` per BM25 index of `node_type`, at `indent` + two spaces.
pub(super) fn write_node_embeddings(
    xml: &mut String,
    graph: &DirGraph,
    node_type: &str,
    indent: &str,
) {
    let child = format!("{indent}  ");
    let mut stores: Vec<(&str, &crate::graph::schema::EmbeddingStore)> = graph
        .embeddings
        .iter()
        .filter(|((nt, _), _)| nt == node_type)
        .map(|((_, name), store)| (text_column_of(name).unwrap_or(name.as_str()), store))
        .collect();
    stores.sort_by(|left, right| left.0.cmp(right.0));
    for (text_col, store) in stores {
        xml.push_str(&format!(
            "{child}<embeddings text_col=\"{}\" dim=\"{}\" count=\"{}\"{}/>\n",
            xml_escape(text_col),
            store.dimension,
            store.len(),
            if store.has_index() {
                " index=\"hnsw\""
            } else {
                ""
            }
        ));
    }
    write_text_indexes(xml, &child, &graph.text_indexes, node_type);
}

const NODE_SEMANTIC: &str = "text_score(n, 'col', 'query'|[0.1,0.2,...], metric) — similarity; a list query is scored as your query vector, a string query is embedded via set_embedder() (metric: 'cosine'|'poincare'|'dot_product'|'euclidean'); embedding_norm(n, 'col_emb') — L2 norm (hierarchy depth in Poincaré space)";

const RELATIONSHIP_SEMANTIC: &str = "relationships: vector_score(r, 'col_emb', $v) / text_score(r, 'col', 'query'|[...]) score a matched relationship, and embedding(r, 'col_emb') returns its stored vector (vector_score(r2, 'col_emb', embedding(r1, 'col_emb')) is relationship-to-relationship similarity) (ORDER BY … DESC LIMIT k is served from the store, through HNSW once indexed; {exact:true} forces exact); CALL db.relationship_embeddings.query({type:'T' | types:['A','B'], text_property:'col', vector:$v | text:'query', top_k:10}) YIELD relationship, score, search_method, type ranks a whole store (HNSW once db.relationship_embeddings.build_index has run; stores are per relationship type and text property — types:['A','B'], or neither type nor types, ranks several merged into one top-k, and MATCH ()-[r:A|B]->() … ORDER BY vector_score(r, …) DESC LIMIT k merges when every type carries the store; describe(cypher=['relationship_semantic']) has the details; deleting an embedded relationship or an endpoint drops that index to none until build_index runs again, and refresh_index refuses while there is none)";

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

const RELATIONSHIP_LEXICAL: &str = "relationships: text_bm25(r, 'prop', 'query text') over an index built with CALL db.relationship_text_index.build({type:'T', property:'prop'})";

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
