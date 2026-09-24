use super::*;
use crate::graph::edge_embeddings::{edge_store_key, EdgeEmbeddingStore};
use crate::graph::embeddings::store_key;
use crate::graph::schema::EmbeddingStore;

/// Node stores `Doc.text`, `Doc.summary`; relationship stores `CITES.context`,
/// `SUPPORTS.context`. The hints read only which stores exist.
fn graph() -> DirGraph {
    let mut graph = DirGraph::new();
    for column in ["text", "summary"] {
        graph
            .embeddings
            .insert(store_key("Doc", column), EmbeddingStore::new(2));
    }
    for ty in ["CITES", "SUPPORTS"] {
        graph.edge_embeddings.insert(
            edge_store_key(ty, "context"),
            EdgeEmbeddingStore::new(2, None),
        );
    }
    graph
}

const REL: EmbeddingEntity = EmbeddingEntity::Relationship;
const NODE: EmbeddingEntity = EmbeddingEntity::Node;

#[test]
fn a_store_name_passed_for_the_column_names_the_column() {
    let hint = missing_store_hint(&graph(), REL, "CITES", "context_emb", Surface::Method);
    assert_eq!(
        hint,
        " Did you mean 'context'? The text column is 'context'; 'context_emb' is the \
         embedding store's own name."
    );
}

#[test]
fn a_misspelled_column_gets_a_did_you_mean() {
    let hint = missing_store_hint(&graph(), REL, "CITES", "contxt", Surface::Cypher);
    assert_eq!(hint, " Did you mean 'context'?");
}

#[test]
fn a_misspelled_type_gets_a_did_you_mean() {
    let hint = missing_store_hint(&graph(), REL, "CITEZ", "context", Surface::Method);
    assert_eq!(hint, " Did you mean 'CITES'?");
}

#[test]
fn a_store_on_the_other_entity_is_named_with_the_remedy_in_the_callers_surface() {
    let g = graph();
    assert_eq!(
        missing_store_hint(&g, NODE, "CITES", "context", Surface::Method),
        " 'CITES.context' is a relationship embedding store — pass entity='relationship' (or \
         call the relationship_* method)."
    );
    assert_eq!(
        missing_store_hint(&g, NODE, "CITES", "context", Surface::Cypher),
        " 'CITES.context' is a relationship embedding store — use db.relationship_embeddings.*."
    );
    assert_eq!(
        missing_store_hint(&g, REL, "Doc", "text", Surface::Method),
        " 'Doc.text' is a node embedding store — pass entity='node' (or call the node_* method)."
    );
}

#[test]
fn an_unrelated_column_lists_the_types_stores_then_every_store() {
    let g = graph();
    assert_eq!(
        missing_store_hint(&g, NODE, "Doc", "zzzzzz", Surface::Method),
        " Node embedding stores of 'Doc': summary, text."
    );
    assert_eq!(
        missing_store_hint(&g, REL, "WROTE", "zzzzzz", Surface::Method),
        " Relationship embedding stores: CITES.context, SUPPORTS.context."
    );
    assert_eq!(
        missing_store_hint(&DirGraph::new(), REL, "WROTE", "x", Surface::Method),
        ""
    );
}

#[test]
fn a_column_only_the_other_entity_carries_is_pointed_there() {
    let g = graph();
    assert_eq!(
        missing_column_hint(&g, NODE, "context", Surface::Method),
        " 'context' is a relationship embedding store (on CITES, SUPPORTS) — pass \
         entity='relationship' (or call the relationship_* method)."
    );
    assert_eq!(
        missing_column_hint(&g, REL, "contxt", Surface::Cypher),
        " Did you mean 'context'?"
    );
    assert_eq!(
        missing_column_hint(&g, REL, "context_emb", Surface::Cypher),
        " Did you mean 'context'? The text column is 'context'; 'context_emb' is the \
         embedding store's own name."
    );
}

#[test]
fn remedies_are_spelled_for_the_surface() {
    assert_eq!(
        Surface::Method.build_index(REL, "CITES", "context"),
        "build_relationship_vector_index('CITES', 'context')"
    );
    assert_eq!(
        Surface::Cypher.build_index(NODE, "Doc", "text"),
        "CALL db.node_embeddings.build_index({type: 'Doc', text_column: 'text'})"
    );
    assert!(
        no_index_to_refresh(REL, "CITES", "context", Surface::Method)
            .ends_with("Build one with build_relationship_vector_index('CITES', 'context').")
    );
    assert!(
        no_index_to_refresh(NODE, "Doc", "text", Surface::Cypher).ends_with(
            "Build one with CALL db.node_embeddings.build_index({type: 'Doc', text_column: \
             'text'})."
        )
    );
}
