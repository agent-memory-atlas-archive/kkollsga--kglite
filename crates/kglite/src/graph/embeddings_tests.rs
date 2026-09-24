//! Unit tests for the lifted embedding-ingest primitives.

use super::*;
use crate::graph::schema::NodeData;
use crate::graph::storage::GraphWrite;
use std::collections::HashMap;

/// A graph of `Doc` nodes carrying a `summary` property.
fn docs(ids: &[i64]) -> DirGraph {
    let mut g = DirGraph::new();
    for &id in ids {
        let mut props = HashMap::new();
        props.insert("summary".to_string(), Value::String(format!("text {id}")));
        let nd = NodeData::new(
            Value::Int64(id),
            Value::String(format!("d{id}")),
            "Doc".to_string(),
            props,
            &mut g.interner,
        );
        let idx = GraphWrite::add_node(&mut g.graph, nd);
        g.type_indices.entry_or_default("Doc".to_string()).push(idx);
    }
    g.build_id_index("Doc");
    g
}

fn batch(entries: &[(i64, [f32; 2])]) -> Vec<(Value, Vec<f32>)> {
    entries
        .iter()
        .map(|(id, v)| (Value::Int64(*id), v.to_vec()))
        .collect()
}

fn store_of(g: &DirGraph) -> &EmbeddingStore {
    g.embeddings
        .get(&("Doc".to_string(), "summary_emb".to_string()))
        .expect("store")
}

#[test]
fn set_writes_the_store_and_bumps_the_version() {
    let mut g = docs(&[1, 2]);
    let before = g.version();
    let report = set_embeddings(
        &mut g,
        "Doc",
        "summary",
        None,
        batch(&[(1, [1.0, 0.0]), (2, [0.0, 1.0])]),
    )
    .unwrap();

    assert_eq!(
        report,
        EmbeddingIngestReport {
            embeddings_stored: 2,
            dimension: 2,
            skipped: 0,
            store_created: true,
        }
    );
    assert_eq!(store_of(&g).len(), 2);
    assert!(
        g.version() > before,
        "a non-empty write must bump the version — a receiver that decides \
         'did this write anything?' by comparing versions drops the write otherwise"
    );
}

#[test]
fn empty_batch_is_a_true_no_op_and_does_not_bump() {
    let mut g = docs(&[1]);
    let before = g.version();
    let empty: Vec<(Value, Vec<f32>)> = Vec::new();
    let report = set_embeddings(&mut g, "Doc", "summary", None, empty).unwrap();

    assert_eq!(report, EmbeddingIngestReport::default());
    assert!(g.embeddings.is_empty());
    assert_eq!(g.version(), before);
}

#[test]
fn unresolvable_ids_are_skipped_and_counted() {
    let mut g = docs(&[1]);
    let report = set_embeddings(
        &mut g,
        "Doc",
        "summary",
        None,
        batch(&[(1, [1.0, 0.0]), (99, [0.0, 1.0])]),
    )
    .unwrap();

    assert_eq!(report.embeddings_stored, 1);
    assert_eq!(report.skipped, 1);
}

/// Every id missing means nothing is written — including no empty store, and
/// no version bump for a call that stored nothing.
#[test]
fn all_ids_missing_writes_nothing() {
    let mut g = docs(&[1]);
    let before = g.version();
    let report =
        set_embeddings(&mut g, "Doc", "summary", None, batch(&[(99, [1.0, 0.0])])).unwrap();

    assert_eq!(report.embeddings_stored, 0);
    assert_eq!(report.dimension, 0);
    assert_eq!(report.skipped, 1);
    assert!(g.embeddings.is_empty());
    assert_eq!(g.version(), before);
}

#[test]
fn mismatched_dimensions_are_rejected_before_any_write() {
    let mut g = docs(&[1, 2]);
    let err = set_embeddings(
        &mut g,
        "Doc",
        "summary",
        None,
        vec![
            (Value::Int64(1), vec![1.0f32, 0.0]),
            (Value::Int64(2), vec![1.0f32, 0.0, 0.0]),
        ],
    )
    .unwrap_err();

    assert!(err.contains("Inconsistent embedding dimensions"), "{err}");
    assert!(
        g.embeddings.is_empty(),
        "validate-then-apply: a rejected batch leaves the graph untouched"
    );
}

#[test]
fn unknown_node_type_is_rejected() {
    let mut g = docs(&[1]);
    let err =
        set_embeddings(&mut g, "Ghost", "summary", None, batch(&[(1, [1.0, 0.0])])).unwrap_err();
    assert!(err.contains("does not exist"), "{err}");
}

/// The typo guard: passing the *store* name where the *column* name belongs.
#[test]
fn unknown_source_column_is_rejected() {
    let mut g = docs(&[1]);
    let err = set_embeddings(
        &mut g,
        "Doc",
        "summary_emb",
        None,
        batch(&[(1, [1.0, 0.0])]),
    )
    .unwrap_err();
    assert!(err.contains("not found on any 'Doc' node"), "{err}");
}

/// Unified with `set_embeddings` — `add_embeddings` used to accept a column
/// that exists on no node and quietly create an unreachable store.
#[test]
fn add_applies_the_same_source_column_check() {
    let mut g = docs(&[1]);
    let err = add_embeddings(
        &mut g,
        "Doc",
        "summary_emb",
        None,
        batch(&[(1, [1.0, 0.0])]),
    )
    .unwrap_err();
    assert!(err.contains("not found on any 'Doc' node"), "{err}");
    assert!(g.embeddings.is_empty());
}

#[test]
fn add_creates_then_extends_one_store() {
    let mut g = docs(&[1, 2]);
    let first = add_embeddings(&mut g, "Doc", "summary", None, batch(&[(1, [1.0, 0.0])])).unwrap();
    assert!(first.store_created);
    assert_eq!(first.embeddings_stored, 1);

    let second = add_embeddings(&mut g, "Doc", "summary", None, batch(&[(2, [0.0, 1.0])])).unwrap();
    assert!(!second.store_created);
    assert_eq!(second.embeddings_stored, 2, "the first batch survived");
    assert_eq!(g.embeddings.len(), 1);
}

#[test]
fn add_enforces_the_existing_store_dimension() {
    let mut g = docs(&[1, 2]);
    add_embeddings(&mut g, "Doc", "summary", None, batch(&[(1, [1.0, 0.0])])).unwrap();
    let err = add_embeddings(
        &mut g,
        "Doc",
        "summary",
        None,
        vec![(Value::Int64(2), vec![1.0f32, 0.0, 0.0])],
    )
    .unwrap_err();

    assert!(err.contains("store has 2 but got 3"), "{err}");
    assert_eq!(store_of(&g).len(), 1, "the rejected batch wrote nothing");
}

#[test]
fn set_replaces_the_store_rather_than_extending_it() {
    let mut g = docs(&[1, 2]);
    add_embeddings(&mut g, "Doc", "summary", None, batch(&[(1, [1.0, 0.0])])).unwrap();
    let report = set_embeddings(&mut g, "Doc", "summary", None, batch(&[(2, [0.0, 1.0])])).unwrap();

    assert_eq!(report.embeddings_stored, 1);
    assert_eq!(store_of(&g).len(), 1);
}

#[test]
fn metric_is_recorded_on_the_creating_call() {
    let mut g = docs(&[1, 2]);
    add_embeddings(
        &mut g,
        "Doc",
        "summary",
        Some("euclidean"),
        batch(&[(1, [1.0, 0.0])]),
    )
    .unwrap();
    assert_eq!(store_of(&g).metric.as_deref(), Some("euclidean"));

    // A later add extends the existing store, whose metric already stands.
    add_embeddings(
        &mut g,
        "Doc",
        "summary",
        Some("cosine"),
        batch(&[(2, [0.0, 1.0])]),
    )
    .unwrap();
    assert_eq!(store_of(&g).metric.as_deref(), Some("euclidean"));
}

#[test]
fn borrowed_slices_are_accepted_without_an_intermediate_copy() {
    let mut g = docs(&[1, 2]);
    let packed: Vec<f32> = vec![1.0, 0.0, 0.0, 1.0];
    let entries = [Value::Int64(1), Value::Int64(2)]
        .into_iter()
        .zip(packed.as_chunks::<2>().0.iter().map(|c| &c[..]));
    let report = set_embeddings(&mut g, "Doc", "summary", None, entries).unwrap();
    assert_eq!(report.embeddings_stored, 2);
}

#[test]
fn index_build_reports_defaults_and_the_resolved_metric() {
    let mut g = docs(&[1, 2, 3]);
    set_embeddings(
        &mut g,
        "Doc",
        "summary",
        Some("euclidean"),
        batch(&[(1, [1.0, 0.0]), (2, [0.0, 1.0]), (3, [0.5, 0.5])]),
    )
    .unwrap();

    let report =
        build_vector_index(&mut g, "Doc", "summary", None, None, None, None, None).unwrap();
    assert_eq!(report.indexed, 3);
    assert_eq!(
        report.metric, "euclidean",
        "the store's metric is inherited"
    );
    assert_eq!(report.m, HnswParams::default().m);
    assert!(store_of(&g).has_index());
}

#[test]
fn index_build_clamps_out_of_range_tuning() {
    let mut g = docs(&[1, 2]);
    set_embeddings(
        &mut g,
        "Doc",
        "summary",
        None,
        batch(&[(1, [1.0, 0.0]), (2, [0.0, 1.0])]),
    )
    .unwrap();
    let report = build_vector_index(
        &mut g,
        "Doc",
        "summary",
        Some(0),
        Some(0),
        Some(0),
        None,
        None,
    )
    .unwrap();
    assert_eq!(report.m, 2);
}

/// A vector write no longer costs the index: neither arm of `set_embedding`
/// moves an existing slot, so the write is recorded as a catch-up delta and
/// the index stays. (Before 0.16.10 every write dropped it, which made
/// `embed_texts(mode='changed')` on five documents cost a corpus rebuild.)
#[test]
fn a_vector_write_becomes_a_catch_up_delta() {
    let mut g = docs(&[1, 2, 3]);
    set_embeddings(
        &mut g,
        "Doc",
        "summary",
        None,
        batch(&[(1, [1.0, 0.0]), (2, [0.0, 1.0])]),
    )
    .unwrap();
    build_vector_index(&mut g, "Doc", "summary", None, None, None, None, None).unwrap();
    assert!(store_of(&g).has_index());
    assert!(
        !store_of(&g).index_is_stale(),
        "a fresh build covers itself"
    );

    // Replaced in place: same slot, new content.
    add_embeddings(&mut g, "Doc", "summary", None, batch(&[(2, [0.3, 0.7])])).unwrap();
    assert!(store_of(&g).has_index(), "the slot layout did not move");
    assert_eq!(store_of(&g).delta_size(), 1);

    // Appended: a slot above the index's coverage.
    add_embeddings(&mut g, "Doc", "summary", None, batch(&[(3, [0.9, 0.1])])).unwrap();
    assert_eq!(store_of(&g).delta_size(), 2);
    assert_eq!(store_of(&g).indexed_slots(), 2, "not yet caught up");

    assert_eq!(refresh_vector_index(&g, "Doc", "summary"), Ok(2));
    assert!(!store_of(&g).index_is_stale());
    assert_eq!(store_of(&g).indexed_slots(), 3);
}

/// Catch-up indexes vectors; it never creates them. A node with no embedding
/// is reported as unembedded and stays out of the delta, so no query can turn
/// into an embedding run.
#[test]
fn catch_up_never_embeds_an_unembedded_node() {
    let mut g = docs(&[1, 2, 3]);
    set_embeddings(
        &mut g,
        "Doc",
        "summary",
        None,
        batch(&[(1, [1.0, 0.0]), (2, [0.0, 1.0])]),
    )
    .unwrap();
    build_vector_index(&mut g, "Doc", "summary", None, None, None, None, None).unwrap();

    let status = list_vector_indexes(&g);
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].unembedded, 1, "Doc 3 has no vector");
    assert_eq!(status[0].delta, 0, "and is therefore not a delta");
    assert!(!status[0].stale);

    refresh_vector_index(&g, "Doc", "summary").unwrap();
    assert_eq!(store_of(&g).len(), 2, "the refresh embedded nothing");
    assert_eq!(list_vector_indexes(&g)[0].unembedded, 1);
}

/// A refresh with no index to refresh refuses, naming the store and the build
/// call. It answered `0` — "nothing outstanding" — which is what an agent read
/// after a node delete had dropped the index; a missing store answered `0` too.
#[test]
fn refresh_vector_index_refuses_without_an_index_or_a_store() {
    let mut g = docs(&[1, 2, 3]);
    set_embeddings(
        &mut g,
        "Doc",
        "summary",
        None,
        batch(&[(1, [1.0, 0.0]), (2, [0.0, 1.0])]),
    )
    .unwrap();
    let error = refresh_vector_index(&g, "Doc", "summary").unwrap_err();
    assert!(
        error.contains("no vector index on 'Doc.summary_emb'"),
        "{error}"
    );
    assert!(
        error.contains("build_vector_index('Doc', 'summary')"),
        "{error}"
    );
    let error = refresh_vector_index(&g, "Doc", "nope").unwrap_err();
    assert!(
        error.contains("no embedding store 'Doc.nope_emb'"),
        "{error}"
    );

    build_vector_index(&mut g, "Doc", "summary", None, None, None, None, None).unwrap();
    assert_eq!(refresh_vector_index(&g, "Doc", "summary"), Ok(0));
    assert!(drop_vector_index(&mut g, "Doc", "summary"));
    assert!(refresh_vector_index(&g, "Doc", "summary").is_err());

    // Read-only does not turn the refusal back into a silent zero.
    g.read_only = true;
    assert!(refresh_vector_index(&g, "Doc", "summary").is_err());
}

/// The ceiling is the caller's, and a rebuild keeps it.
#[test]
fn the_auto_refresh_limit_bounds_inline_catch_up() {
    let mut g = docs(&[1, 2, 3, 4]);
    set_embeddings(
        &mut g,
        "Doc",
        "summary",
        None,
        batch(&[(1, [1.0, 0.0]), (2, [0.0, 1.0])]),
    )
    .unwrap();
    build_vector_index(&mut g, "Doc", "summary", None, None, None, None, Some(1)).unwrap();
    assert_eq!(store_of(&g).auto_refresh_limit(), 1);

    add_embeddings(&mut g, "Doc", "summary", None, batch(&[(3, [0.9, 0.1])])).unwrap();
    assert!(
        store_of(&g).can_auto_refresh(),
        "one vector is at the limit"
    );

    add_embeddings(&mut g, "Doc", "summary", None, batch(&[(4, [0.1, 0.9])])).unwrap();
    assert!(
        !store_of(&g).can_auto_refresh(),
        "two is over it — the query serves an exact scan instead"
    );
    assert!(store_of(&g).index_is_stale());

    build_vector_index(&mut g, "Doc", "summary", None, None, None, None, None).unwrap();
    assert_eq!(
        store_of(&g).auto_refresh_limit(),
        1,
        "a rebuild keeps the ceiling its author set"
    );
}

#[test]
fn index_build_requires_a_store() {
    let mut g = docs(&[1]);
    let err =
        build_vector_index(&mut g, "Doc", "summary", None, None, None, None, None).unwrap_err();
    assert!(
        err.contains("No embedding store 'Doc.summary_emb'"),
        "{err}"
    );
}

/// Passing the *store* name where the text column belongs derives
/// `summary_emb_emb`, which can never exist. The error has to name the column
/// that would have worked, or the caller re-reads their own spelling as
/// correct.
#[test]
fn index_build_on_a_store_name_names_the_text_column() {
    let mut g = docs(&[1]);
    set_embeddings(&mut g, "Doc", "summary", None, batch(&[(1, [1.0, 0.0])])).unwrap();

    let err =
        build_vector_index(&mut g, "Doc", "summary_emb", None, None, None, None, None).unwrap_err();
    assert!(
        err.contains("No embedding store 'Doc.summary_emb_emb'"),
        "{err}"
    );
    assert!(err.contains("Did you mean 'summary'?"), "{err}");
    assert!(
        err.contains("build_vector_index() takes the text column"),
        "{err}"
    );
}

/// A genuinely unknown column has no suffix story to tell, so the error falls
/// back to what the type does have embedded.
#[test]
fn index_build_on_an_unknown_column_lists_the_embedded_columns() {
    let mut g = docs(&[1]);
    set_embeddings(&mut g, "Doc", "summary", None, batch(&[(1, [1.0, 0.0])])).unwrap();

    let err = build_vector_index(&mut g, "Doc", "nope", None, None, None, None, None).unwrap_err();
    assert!(err.contains("No embedding store 'Doc.nope_emb'"), "{err}");
    assert!(err.contains("summary"), "{err}");
}

#[test]
fn poincare_stays_on_the_exact_path() {
    let mut g = docs(&[1]);
    set_embeddings(
        &mut g,
        "Doc",
        "summary",
        Some("poincare"),
        batch(&[(1, [0.1, 0.2])]),
    )
    .unwrap();
    let err =
        build_vector_index(&mut g, "Doc", "summary", None, None, None, None, None).unwrap_err();
    assert!(err.contains("poincare"), "{err}");
}

#[test]
fn store_key_derives_the_emb_suffix_once() {
    assert_eq!(
        store_key("Doc", "summary"),
        ("Doc".to_string(), "summary_emb".to_string())
    );
}

#[test]
fn list_embeddings_projects_source_column_and_defaults_metric() {
    let mut g = docs(&[1, 2]);
    assert!(
        list_embeddings(&g).is_empty(),
        "a graph with no stores lists nothing"
    );

    set_embeddings(
        &mut g,
        "Doc",
        "summary",
        Some("dot_product"),
        batch(&[(1, [1.0, 0.0]), (2, [0.0, 1.0])]),
    )
    .unwrap();

    let listing = list_embeddings(&g);
    assert_eq!(
        listing,
        vec![EmbeddingStoreInfo {
            node_type: "Doc".to_string(),
            // both spellings: the source column this API takes, and the store
            // name Cypher's vector_score takes
            text_column: "summary".to_string(),
            store_name: "summary_emb".to_string(),
            dimension: 2,
            count: 2,
            metric: "dot_product".to_string(),
        }]
    );
}

#[test]
fn list_embeddings_defaults_an_unrecorded_metric_to_cosine() {
    let mut g = docs(&[1]);
    set_embeddings(&mut g, "Doc", "summary", None, batch(&[(1, [1.0, 0.0])])).unwrap();
    assert_eq!(list_embeddings(&g)[0].metric, "cosine");
}

// ---------------------------------------------------------------------------
// Identity-alias source columns
//
// `add_nodes(df, "Doc", "id", "name")` hoists the title column *out* of the
// property map and registers `name` as the type's title alias, so a source
// column the user still thinks of as `name` is neither `title` nor a live
// property. These pin that the ingest guard resolves it the way every read
// path does.
// ---------------------------------------------------------------------------

/// `docs()` with `title_alias` registered as the type's original title column
/// — the state `add_nodes(df, "Doc", "id", <title_alias>)` leaves behind.
fn docs_titled(ids: &[i64], title_alias: &str) -> DirGraph {
    let mut g = docs(ids);
    g.title_field_aliases_mut()
        .insert("Doc".to_string(), title_alias.to_string());
    g
}

/// `docs()` with `id_alias` registered as the type's original id column.
fn docs_ided(ids: &[i64], id_alias: &str) -> DirGraph {
    let mut g = docs(ids);
    g.id_field_aliases_mut()
        .insert("Doc".to_string(), id_alias.to_string());
    g
}

#[test]
fn a_per_type_title_alias_is_an_accepted_source_column() {
    let mut g = docs_titled(&[1, 2], "name");
    let report = set_embeddings(&mut g, "Doc", "name", None, batch(&[(1, [1.0, 0.0])])).unwrap();
    assert_eq!(report.embeddings_stored, 1);
}

#[test]
fn a_per_type_id_alias_is_an_accepted_source_column() {
    let mut g = docs_ided(&[1, 2], "doc_no");
    let report = set_embeddings(&mut g, "Doc", "doc_no", None, batch(&[(1, [1.0, 0.0])])).unwrap();
    assert_eq!(report.embeddings_stored, 1);
}

/// No alias map at all: `name` still resolves — structurally, to the title —
/// exactly as `MATCH (n:Doc) RETURN n.name` does.
#[test]
fn the_soft_alias_name_is_accepted_without_any_alias_map() {
    let mut g = docs(&[1]);
    assert!(g.title_field_aliases.is_empty() && g.id_field_aliases.is_empty());
    let report = set_embeddings(&mut g, "Doc", "name", None, batch(&[(1, [1.0, 0.0])])).unwrap();
    assert_eq!(report.embeddings_stored, 1);
}

#[test]
fn the_soft_alias_label_is_accepted() {
    let mut g = docs(&[1]);
    let report = set_embeddings(&mut g, "Doc", "label", None, batch(&[(1, [1.0, 0.0])])).unwrap();
    assert_eq!(report.embeddings_stored, 1);
}

#[test]
fn add_embeddings_accepts_the_same_identity_aliases() {
    let mut g = docs_titled(&[1, 2], "name");
    let report = add_embeddings(&mut g, "Doc", "name", None, batch(&[(1, [1.0, 0.0])])).unwrap();
    assert_eq!(report.embeddings_stored, 1);
}

/// **Store-key decision.** The store is keyed by the spelling the caller
/// passed — resolving `name` to `title` for the *read* never renames the
/// *store*. Pinned because the alternative (canonicalising the key) would
/// strand every store already written under the raw spelling: `add_nodes`'
/// `<col>_emb` ingest keys raw, so does every `.kgl` written before this
/// change, and Cypher's `text_score(n, col, q)` rewrite has no node type to
/// resolve with.
#[test]
fn the_store_is_keyed_by_the_spelling_the_caller_used() {
    let mut g = docs_titled(&[1], "name");
    set_embeddings(&mut g, "Doc", "name", None, batch(&[(1, [1.0, 0.0])])).unwrap();
    assert!(g
        .embeddings
        .contains_key(&("Doc".to_string(), "name_emb".to_string())));
    assert!(!g
        .embeddings
        .contains_key(&("Doc".to_string(), "title_emb".to_string())));
}

/// Accepting aliases must not degrade the typo guard into "anything goes".
#[test]
fn an_unknown_column_is_still_rejected_on_an_aliased_type() {
    let mut g = docs_titled(&[1], "name");
    let err =
        set_embeddings(&mut g, "Doc", "headline", None, batch(&[(1, [1.0, 0.0])])).unwrap_err();
    assert!(err.contains("not found on any 'Doc' node"), "{err}");
}

/// Aliases are per type: another type's title column is not this type's.
#[test]
fn another_types_title_alias_is_not_accepted() {
    let mut g = docs(&[1]);
    g.title_field_aliases_mut()
        .insert("Other".to_string(), "headline".to_string());
    let err =
        set_embeddings(&mut g, "Doc", "headline", None, batch(&[(1, [1.0, 0.0])])).unwrap_err();
    assert!(err.contains("not found on any 'Doc' node"), "{err}");
}

/// The store-name typo stays rejected even on a graph that has alias maps —
/// registering aliases must not make the `_emb` guard fall through.
#[test]
fn the_store_name_typo_is_still_rejected_on_an_aliased_type() {
    let mut g = docs_titled(&[1], "name");
    let err = set_embeddings(
        &mut g,
        "Doc",
        "summary_emb",
        None,
        batch(&[(1, [1.0, 0.0])]),
    )
    .unwrap_err();
    assert!(err.contains("not found on any 'Doc' node"), "{err}");
}

/// `list_embeddings` counts live vectors, so deleting an embedded node drops
/// the count. It read `slot_to_node.len()` before the deletion chokepoint
/// pruned anything, so a graph that had deleted every embedded node still
/// reported a full store.
#[test]
fn deleting_an_embedded_node_drops_the_listed_count() {
    use std::collections::HashSet;

    let mut g = docs(&[1, 2, 3]);
    set_embeddings(
        &mut g,
        "Doc",
        "summary",
        None,
        batch(&[(1, [1.0, 0.0]), (2, [0.0, 1.0]), (3, [1.0, 1.0])]),
    )
    .unwrap();
    assert_eq!(list_embeddings(&g)[0].count, 3);

    let doomed = g
        .lookup_by_id("Doc", &Value::Int64(2))
        .expect("Doc 2 is present");
    crate::graph::mutation::maintain::detach_delete_nodes(&mut g, &HashSet::from([doomed]));

    assert_eq!(list_embeddings(&g)[0].count, 2);
    assert_eq!(store_of(&g).validate_shape(), Ok(()));
    // The store itself stays — an emptied store is still a declared column,
    // and dropping it would change what `list_embeddings` enumerates.
    let all_docs: HashSet<_> = [1i64, 3]
        .into_iter()
        .map(|id| g.lookup_by_id("Doc", &Value::Int64(id)).expect("present"))
        .collect();
    crate::graph::mutation::maintain::detach_delete_nodes(&mut g, &all_docs);
    assert_eq!(list_embeddings(&g).len(), 1);
    assert_eq!(list_embeddings(&g)[0].count, 0);
}

/// Deleting an embedded node drops the store's HNSW index: it addresses
/// vectors by slot, and the prune moves the tail slot into the vacated one.
/// A stale index would hand back the pruned slot — the same ghost, one layer
/// up. The index is a rebuildable cache, so dropping it is the v1 answer.
#[test]
fn deleting_an_embedded_node_invalidates_the_vector_index() {
    use crate::graph::algorithms::hnsw::HnswParams;
    use crate::graph::algorithms::vector::DistanceMetric;
    use std::collections::HashSet;

    let ids: Vec<i64> = (1..=8).collect();
    let mut g = docs(&ids);
    set_embeddings(
        &mut g,
        "Doc",
        "summary",
        None,
        ids.iter()
            .map(|&id| (Value::Int64(id), vec![id as f32, 1.0]))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    g.embeddings
        .get_mut(&("Doc".to_string(), "summary_emb".to_string()))
        .expect("store")
        .build_index(DistanceMetric::Cosine, HnswParams::default(), 7)
        .expect("build index");
    assert!(store_of(&g).has_index());

    let untouched = g
        .lookup_by_id("Doc", &Value::Int64(4))
        .expect("Doc 4 is present");
    crate::graph::mutation::maintain::detach_delete_nodes(&mut g, &HashSet::from([untouched]));
    assert!(
        !store_of(&g).has_index(),
        "the index still addresses the slot layout the prune changed"
    );
}

/// Deleting a node of a type that carries no store touches no store at all —
/// the guard that keeps the un-embedded graph (the overwhelmingly common one)
/// at zero cost per deleted node, stated as behaviour rather than as timing.
#[test]
fn deleting_an_unembedded_node_leaves_every_store_intact() {
    use std::collections::HashSet;

    let mut g = docs(&[1, 2, 3]);
    set_embeddings(
        &mut g,
        "Doc",
        "summary",
        None,
        batch(&[(1, [1.0, 0.0]), (2, [0.0, 1.0])]),
    )
    .unwrap();
    let before = (
        store_of(&g).slot_to_node.clone(),
        store_of(&g).data.clone(),
        store_of(&g).norms.clone(),
    );

    let unembedded = g
        .lookup_by_id("Doc", &Value::Int64(3))
        .expect("Doc 3 is present but was never embedded");
    crate::graph::mutation::maintain::detach_delete_nodes(&mut g, &HashSet::from([unembedded]));

    let after = (
        store_of(&g).slot_to_node.clone(),
        store_of(&g).data.clone(),
        store_of(&g).norms.clone(),
    );
    assert_eq!(after, before);
}

// ─── Catch-up soundness: the recall gate (decision 11c, G5) ────────────────
//
// HNSW is approximate, so "the caught-up index returns exactly what a rebuilt
// one returns" is the wrong oracle — two builds over the same vectors already
// disagree at the margin, and the concurrent build is not even reproducible
// run to run. What must hold is that catching up does not *degrade* the index:
// its recall against the exact scan stays at the recall a batch build over the
// same N+M vectors achieves, less a tolerance.

/// Deterministic pseudo-random unit-ish vectors — no rng dependency, and the
/// same corpus on every run, so a recall number is comparable across runs.
fn corpus(n: usize, dim: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut state = seed | 1;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        ((state >> 11) as f64 / ((1u64 << 53) as f64)) as f32 - 0.5
    };
    (0..n).map(|_| (0..dim).map(|_| next()).collect()).collect()
}

fn graph_with_vectors(vectors: &[Vec<f32>], embedded: usize) -> DirGraph {
    let ids: Vec<i64> = (1..=vectors.len() as i64).collect();
    let mut g = docs(&ids);
    let entries: Vec<(Value, Vec<f32>)> = ids
        .iter()
        .take(embedded)
        .map(|&id| (Value::Int64(id), vectors[(id - 1) as usize].clone()))
        .collect();
    set_embeddings(&mut g, "Doc", "summary", Some("cosine"), entries).unwrap();
    g
}

/// Fraction of the exact top-k the index returns, averaged over `queries`.
fn recall_at_k(g: &DirGraph, vectors: &[Vec<f32>], queries: &[Vec<f32>], k: usize) -> f64 {
    use crate::graph::algorithms::vector::{vector_search, DistanceMetric, VectorSearchOptions};
    use crate::graph::schema::CurrentSelection;

    let selection = CurrentSelection::new();
    let mut hits = 0usize;
    for query in queries {
        let exact = vector_search(
            g,
            &selection,
            "summary_emb",
            query,
            &VectorSearchOptions::default()
                .with_top_k(k)
                .with_metric(DistanceMetric::Cosine)
                .with_exact(true),
        )
        .unwrap();
        let approx = vector_search(
            g,
            &selection,
            "summary_emb",
            query,
            &VectorSearchOptions::default()
                .with_top_k(k)
                .with_metric(DistanceMetric::Cosine)
                .with_exact(false),
        )
        .unwrap();
        let approx_ids: Vec<_> = approx.iter().map(|r| r.node_idx).collect();
        hits += exact
            .iter()
            .filter(|r| approx_ids.contains(&r.node_idx))
            .count();
    }
    let _ = vectors;
    hits as f64 / (queries.len() * k) as f64
}

#[test]
fn incremental_catch_up_holds_the_recall_a_batch_build_achieves() {
    const N: usize = 400;
    const M: usize = 100;
    const DIM: usize = 16;
    const K: usize = 10;
    const EPSILON: f64 = 0.05;

    let vectors = corpus(N + M, DIM, 0xA11CE);
    let queries = corpus(40, DIM, 0xB0B);

    // Reference: the *worse* of two batch builds over all N+M vectors. HNSW's
    // link graph is built concurrently and is not identical run to run, so a
    // single build is a sample, not a constant — comparing against one made
    // this gate fail roughly once in twelve runs on its own noise. Two builds
    // measure the band a plain rebuild already moves within, and the epsilon
    // is then a real tolerance rather than a stand-in for that band.
    let mut batched = graph_with_vectors(&vectors, N + M);
    build_vector_index(&mut batched, "Doc", "summary", None, None, None, None, None).unwrap();
    let first = recall_at_k(&batched, &vectors, &queries, K);
    build_vector_index(&mut batched, "Doc", "summary", None, None, None, None, None).unwrap();
    let second = recall_at_k(&batched, &vectors, &queries, K);
    let batch_recall = first.min(second);
    assert!(
        batch_recall > 0.5,
        "the reference index must actually retrieve: {first} / {second}"
    );

    // Candidate: build over N, then add M and let the query fold them in.
    let mut incremental = graph_with_vectors(&vectors, N);
    build_vector_index(
        &mut incremental,
        "Doc",
        "summary",
        None,
        None,
        None,
        None,
        Some(M),
    )
    .unwrap();
    let added: Vec<(Value, Vec<f32>)> = (N..N + M)
        .map(|i| (Value::Int64(i as i64 + 1), vectors[i].clone()))
        .collect();
    add_embeddings(&mut incremental, "Doc", "summary", None, added).unwrap();
    assert_eq!(store_of(&incremental).delta_size(), M);
    assert_eq!(store_of(&incremental).indexed_slots(), N);

    let caught_up_recall = recall_at_k(&incremental, &vectors, &queries, K);
    assert!(
        !store_of(&incremental).index_is_stale(),
        "the query must have folded the delta in on its way through"
    );
    assert_eq!(store_of(&incremental).indexed_slots(), N + M);
    assert!(
        caught_up_recall >= batch_recall - EPSILON,
        "catch-up recall {caught_up_recall} fell below the batch build's \
         {batch_recall} by more than {EPSILON}"
    );
}

/// The mutation control for the gate above: an index that skips part of its
/// delta must be *caught*. Here the delta is left unindexed entirely (over the
/// ceiling), which is exactly what a refresh that dropped slots would look
/// like to the index — and the coverage assertion goes red.
#[test]
fn an_uncaught_delta_is_visible_as_missing_coverage() {
    // Above `HNSW_AUTO_MIN`, so a query would genuinely reach the index path:
    // the staleness below is the ceiling refusing, not the corpus being too
    // small for the index to be consulted at all.
    const N: usize = 400;
    const M: usize = 50;
    let vectors = corpus(N + M, 16, 0xA11CE);

    let mut g = graph_with_vectors(&vectors, N);
    // A ceiling below the delta: no query will fold it in.
    build_vector_index(&mut g, "Doc", "summary", None, None, None, None, Some(1)).unwrap();
    let added: Vec<(Value, Vec<f32>)> = (N..N + M)
        .map(|i| (Value::Int64(i as i64 + 1), vectors[i].clone()))
        .collect();
    add_embeddings(&mut g, "Doc", "summary", None, added).unwrap();

    let queries = corpus(5, 16, 0xB0B);
    let _ = recall_at_k(&g, &vectors, &queries, 10);
    assert!(
        g.embeddings[&("Doc".to_string(), "summary_emb".to_string())].index_is_stale(),
        "an over-ceiling delta must stay outstanding, not be silently absorbed"
    );
    assert_eq!(store_of(&g).indexed_slots(), N, "and stay uncovered");

    // …and the results are still right, because the query fell back to the
    // exact scan rather than searching an index that does not cover them.
    let one_uncovered = &vectors[N + M - 1];
    let found = {
        use crate::graph::algorithms::vector::{
            vector_search, DistanceMetric, VectorSearchOptions,
        };
        use crate::graph::schema::CurrentSelection;
        vector_search(
            &g,
            &CurrentSelection::new(),
            "summary_emb",
            one_uncovered,
            &VectorSearchOptions::default()
                .with_top_k(1)
                .with_metric(DistanceMetric::Cosine)
                .with_exact(false),
        )
        .unwrap()
    };
    assert_eq!(
        found[0].node_idx.index(),
        N + M - 1,
        "a stale vector index costs speed, never the right answer"
    );
}

/// A read-only graph may not write an index, so it serves the exact scan and
/// leaves the delta outstanding for a writable handle to fold in.
#[test]
fn a_read_only_graph_serves_the_exact_scan_instead_of_catching_up() {
    // Above `HNSW_AUTO_MIN` for the same reason as the test above.
    let vectors = corpus(450, 8, 7);
    let mut g = graph_with_vectors(&vectors, 400);
    build_vector_index(&mut g, "Doc", "summary", None, None, None, None, None).unwrap();
    let added: Vec<(Value, Vec<f32>)> = (400..450)
        .map(|i| (Value::Int64(i as i64 + 1), vectors[i].clone()))
        .collect();
    add_embeddings(&mut g, "Doc", "summary", None, added).unwrap();
    g.read_only = true;

    assert_eq!(refresh_vector_index(&g, "Doc", "summary"), Ok(0));
    let queries = corpus(3, 8, 11);
    let _ = recall_at_k(&g, &vectors, &queries, 5);
    assert!(
        store_of(&g).index_is_stale(),
        "a read-only handle must not perform the one write catch-up would be"
    );
}

// ─── SHOW INDEXES / db.indexes ────────────────────────────────────────────

#[test]
fn show_indexes_reports_a_vector_index_under_its_source_column() {
    use crate::graph::introspection::schema_overview::{collect_indexes_structured, IndexKind};

    let mut g = docs(&[1, 2, 3]);
    g.create_index("Doc", "summary");
    set_embeddings(
        &mut g,
        "Doc",
        "summary",
        None,
        batch(&[(1, [1.0, 0.0]), (2, [0.0, 1.0])]),
    )
    .unwrap();

    assert!(
        !collect_indexes_structured(&g)
            .iter()
            .any(|info| info.kind == IndexKind::Vector),
        "vectors alone are not an installed index — list_embeddings() reports those"
    );

    build_vector_index(&mut g, "Doc", "summary", None, None, None, None, None).unwrap();
    let rows = collect_indexes_structured(&g);
    let vector: Vec<_> = rows
        .iter()
        .filter(|info| info.kind == IndexKind::Vector)
        .collect();

    assert_eq!(vector.len(), 1);
    assert_eq!(
        vector[0].name, "Doc.summary",
        "keyed on the source column, not the 'summary_emb' store"
    );
    assert_eq!(vector[0].kind.neo4j_type(), "VECTOR");
    assert_eq!(vector[0].stale, Some(false));
    assert_eq!(vector[0].delta, Some(0));
    assert_eq!(vector[0].unembedded, Some(1), "Doc 3 carries no vector");
    assert_eq!(
        rows.iter()
            .filter(|info| info.name == "Doc.summary")
            .count(),
        2,
        "the equality index and the vector index share one canonical name"
    );

    add_embeddings(&mut g, "Doc", "summary", None, batch(&[(3, [0.5, 0.5])])).unwrap();
    let rows = collect_indexes_structured(&g);
    let vector = rows
        .iter()
        .find(|info| info.kind == IndexKind::Vector)
        .unwrap();
    assert_eq!(vector.stale, Some(true));
    assert_eq!(vector.delta, Some(1));
    assert_eq!(vector.unembedded, Some(0), "and now every Doc is embedded");
}

// ─── Cypher index DDL over a vector index ──────────────────────────────────

/// Run one DDL statement, returning its mutation stats. (These cases live here
/// rather than in `schema_ddl.rs`'s own test module because that file sits at
/// the repository's god-file line ceiling.)
fn run_ddl(
    graph: &mut DirGraph,
    query: &str,
) -> Result<crate::graph::languages::cypher::result::MutationStats, String> {
    let parsed =
        crate::graph::languages::cypher::parser::parse_cypher(query).map_err(|e| e.to_string())?;
    let result = crate::graph::languages::cypher::executor::write::execute_mutable(
        graph,
        &parsed,
        HashMap::new(),
        crate::graph::algorithms::Interrupt::default(),
    )?;
    Ok(result.stats.unwrap_or_default())
}

/// `DROP INDEX Label.prop` removes every structure registered under that
/// name, and `SHOW INDEXES` prints a built vector index under exactly that
/// name — so it has to go too. The vectors stay: dropping an accelerator is
/// not a data verb.
#[test]
fn drop_index_by_canonical_name_takes_the_vector_index_with_it() {
    use crate::graph::embeddings::{build_vector_index, has_vector_index, set_embeddings};

    let mut graph = docs(&[1, 2]);
    set_embeddings(
        &mut graph,
        "Doc",
        "summary",
        None,
        vec![
            (Value::Int64(1), vec![1.0f32, 0.0]),
            (Value::Int64(2), vec![0.0f32, 1.0]),
        ],
    )
    .expect("embed");
    build_vector_index(&mut graph, "Doc", "summary", None, None, None, None, None).expect("build");
    assert!(has_vector_index(&graph, "Doc", "summary"));

    let stats = run_ddl(&mut graph, "DROP INDEX Doc.summary").expect("drop");
    assert_eq!(stats.indexes_removed, 1);
    assert!(!has_vector_index(&graph, "Doc", "summary"));
    assert_eq!(
        graph.embeddings[&("Doc".to_string(), "summary_emb".to_string())].len(),
        2,
        "the vectors survive — DROP INDEX drops the accelerator, not the data"
    );
}

/// …and a name that `SHOW INDEXES` prints must never come back as "no
/// index named", which is why the vector row exists only once one is built.
#[test]
fn every_listed_index_name_is_droppable() {
    use crate::graph::embeddings::{build_vector_index, set_embeddings};
    use crate::graph::introspection::schema_overview::collect_indexes_structured;

    let mut graph = docs(&[1, 2]);
    set_embeddings(
        &mut graph,
        "Doc",
        "summary",
        None,
        vec![(Value::Int64(1), vec![1.0f32, 0.0])],
    )
    .expect("embed");
    build_vector_index(&mut graph, "Doc", "summary", None, None, None, None, None).expect("build");

    let names: Vec<String> = collect_indexes_structured(&graph)
        .iter()
        .map(|info| info.name.clone())
        .collect();
    assert!(names.contains(&"Doc.summary".to_string()));
    for name in names {
        run_ddl(&mut graph, &format!("DROP INDEX {name}"))
            .unwrap_or_else(|e| panic!("SHOW INDEXES listed '{name}' but DROP refused: {e}"));
    }
}

// ── embed_property: the changed-mode pass every binding shares ───────────────

/// Counts what a pass asked of the model, so "did this re-embed the corpus?"
/// is an assertion rather than an inference.
#[derive(Default)]
struct StubEmbedder {
    dimension: usize,
    /// Every text the pass sent, in order.
    seen: std::sync::Mutex<Vec<String>>,
    loads: std::sync::atomic::AtomicUsize,
    unloads: std::sync::atomic::AtomicUsize,
    /// When set, `embed` fails with this message instead of answering.
    fails: Option<String>,
    /// When set, every returned vector has this length instead of `dimension`.
    wrong_width: Option<usize>,
    /// When set, the model answers one vector short of the batch it was given.
    short_batch: bool,
}

impl StubEmbedder {
    fn new(dimension: usize) -> Self {
        StubEmbedder {
            dimension,
            ..StubEmbedder::default()
        }
    }
    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
    fn loads(&self) -> usize {
        self.loads.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl crate::graph::embedder::Embedder for StubEmbedder {
    fn dimension(&self) -> usize {
        self.dimension
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        if let Some(message) = &self.fails {
            return Err(message.clone());
        }
        self.seen.lock().unwrap().extend_from_slice(texts);
        let width = self.wrong_width.unwrap_or(self.dimension);
        let answered = texts.len() - usize::from(self.short_batch && !texts.is_empty());
        Ok((0..answered).map(|i| vec![i as f32; width]).collect())
    }
    fn model_id(&self) -> Option<String> {
        Some("stub".to_string())
    }
    fn load(&self) -> Result<(), String> {
        self.loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn unload(&self) {
        self.unloads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn embed_docs(
    graph: &mut std::sync::Arc<DirGraph>,
    model: &StubEmbedder,
    mode: EmbedMode,
) -> Result<EmbedOutcome, EmbedError> {
    embed_property(graph, "Doc", "summary", mode, model, &EmbedHooks::default())
}

/// Overwrite one node's text through the ordinary write path, so the next
/// `Changed` pass has exactly one stale row.
fn retext(graph: &mut std::sync::Arc<DirGraph>, id: i64, text: &str) {
    let params = HashMap::new();
    crate::graph::session::execute_mut(
        crate::graph::handle::make_dir_graph_mut(graph),
        &format!("MATCH (d:Doc) WHERE d.id = {id} SET d.summary = '{text}'"),
        &crate::graph::session::ExecuteOptions::eager(&params),
    )
    .expect("the retext write");
}

/// The node slot of the `n`th `Doc` — `docs()` adds them in id order.
fn doc_slot(graph: &DirGraph, nth: usize) -> usize {
    graph.type_indices.get("Doc").expect("Doc nodes").to_vec()[nth].index()
}

#[test]
fn changed_mode_embeds_only_the_rows_whose_text_moved() {
    let mut g = std::sync::Arc::new(docs(&[1, 2, 3]));
    let model = StubEmbedder::new(2);

    let first = embed_docs(&mut g, &model, EmbedMode::Changed).unwrap();
    assert_eq!(first.embedded, 3);
    assert_eq!(first.dimension, 2);
    assert_eq!(model.seen().len(), 3);

    retext(&mut g, 2, "a different summary");
    let second = embed_docs(&mut g, &model, EmbedMode::Changed).unwrap();
    assert_eq!(second.embedded, 1, "only the retexted node");
    assert_eq!(second.skipped_existing, 2);
    assert_eq!(second.reembedded_changed, 1);
    assert_eq!(model.seen()[3..], ["a different summary".to_string()]);

    let third = embed_docs(&mut g, &model, EmbedMode::Changed).unwrap();
    assert_eq!(third.embedded, 0, "nothing moved since");
    assert_eq!(model.seen().len(), 4, "and the model was asked nothing");
}

#[test]
fn all_mode_rebuilds_the_store_and_missing_mode_ignores_stale_text() {
    let mut g = std::sync::Arc::new(docs(&[1, 2]));
    let model = StubEmbedder::new(2);
    embed_docs(&mut g, &model, EmbedMode::Missing).unwrap();
    retext(&mut g, 1, "moved on");

    let missing = embed_docs(&mut g, &model, EmbedMode::Missing).unwrap();
    assert_eq!(
        missing.embedded, 0,
        "`missing` asks whether a vector exists, never whether it is current"
    );
    assert_eq!(missing.skipped_existing, 2);

    let all = embed_docs(&mut g, &model, EmbedMode::All).unwrap();
    assert_eq!(all.embedded, 2, "`all` rebuilds the store from scratch");
    assert_eq!(all.skipped_existing, 0);
}

#[test]
fn an_idle_pass_leaves_the_model_alone_unless_the_caller_wants_its_dimension() {
    let mut g = std::sync::Arc::new(docs(&[1]));
    let model = StubEmbedder::new(2);
    embed_docs(&mut g, &model, EmbedMode::Changed).unwrap();
    assert_eq!(model.loads(), 1);

    let idle = embed_docs(&mut g, &model, EmbedMode::Changed).unwrap();
    assert_eq!(idle.embedded, 0);
    assert_eq!(
        model.loads(),
        1,
        "a rebuild that changed no note must not pay a model load"
    );
    assert_eq!(
        idle.dimension, 2,
        "the store's own dimension answers without the model"
    );

    let hooks = EmbedHooks {
        load_when_idle: true,
        ..EmbedHooks::default()
    };
    let asked =
        embed_property(&mut g, "Doc", "summary", EmbedMode::Changed, &model, &hooks).unwrap();
    assert_eq!(asked.embedded, 0);
    assert_eq!(model.loads(), 2, "the wheel's contract: always a dimension");
}

#[test]
fn a_label_with_no_nodes_is_a_no_op_and_an_unknown_column_is_an_error() {
    let mut g = std::sync::Arc::new(docs(&[1]));
    let model = StubEmbedder::new(2);

    let empty = embed_property(
        &mut g,
        "Nothing",
        "summary",
        EmbedMode::Changed,
        &model,
        &EmbedHooks::default(),
    )
    .unwrap();
    assert_eq!(empty, EmbedOutcome::default(), "nothing to embed, no model");
    assert_eq!(model.loads(), 0);

    let bad = embed_property(
        &mut g,
        "Doc",
        "nowhere",
        EmbedMode::Changed,
        &model,
        &EmbedHooks::default(),
    );
    assert!(
        matches!(bad, Err(EmbedError::Column(ref m)) if m.contains("nowhere")),
        "{bad:?}"
    );
}

#[test]
fn a_dimension_change_is_refused_and_leaves_the_store_untouched() {
    let mut g = std::sync::Arc::new(docs(&[1, 2]));
    embed_docs(&mut g, &StubEmbedder::new(2), EmbedMode::All).unwrap();
    retext(&mut g, 1, "changed");

    let wider = StubEmbedder::new(8);
    let refused = embed_docs(&mut g, &wider, EmbedMode::Changed);
    assert_eq!(refused, Err(EmbedError::Dimension { store: 2, model: 8 }));
    assert_eq!(store_of(&g).dimension, 2, "the store is as it was");
    assert_eq!(wider.unloads.load(std::sync::atomic::Ordering::SeqCst), 1);

    let rebuilt = embed_docs(&mut g, &wider, EmbedMode::All).unwrap();
    assert_eq!(rebuilt.dimension, 8, "`all` is the way through");
    assert_eq!(store_of(&g).dimension, 8);
}

#[test]
fn a_failing_model_writes_nothing_and_is_still_unloaded() {
    let mut g = std::sync::Arc::new(docs(&[1, 2]));
    let broken = StubEmbedder {
        fails: Some("the model is on fire".to_string()),
        ..StubEmbedder::new(2)
    };
    assert_eq!(
        embed_docs(&mut g, &broken, EmbedMode::Changed),
        Err(EmbedError::Model("the model is on fire".to_string()))
    );
    assert_eq!(broken.unloads.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        g.embeddings.is_empty(),
        "a failed pass leaves the graph as it found it"
    );

    let lying = StubEmbedder {
        wrong_width: Some(5),
        ..StubEmbedder::new(2)
    };
    let refused = embed_docs(&mut g, &lying, EmbedMode::Changed);
    assert!(
        matches!(refused, Err(EmbedError::Output(ref m)) if m.contains("dimension 5")),
        "{refused:?}"
    );
    assert!(g.embeddings.is_empty());

    // A model that answers one vector short would otherwise write the batch's
    // *first* texts' vectors against the right slots and drop the rest in
    // silence — the pass would report every text embedded.
    let short = StubEmbedder {
        short_batch: true,
        ..StubEmbedder::new(2)
    };
    let refused = embed_docs(&mut g, &short, EmbedMode::Changed);
    assert!(
        matches!(refused, Err(EmbedError::Output(ref m)) if m.contains("1 vectors for 2 texts")),
        "{refused:?}"
    );
    assert!(g.embeddings.is_empty());
}

#[test]
fn the_hooks_see_every_batch_and_can_wrap_the_model_call() {
    let mut g = std::sync::Arc::new(docs(&[1, 2, 3, 4, 5]));
    let model = StubEmbedder::new(2);
    let started = std::cell::Cell::new(0usize);
    let batches = std::cell::RefCell::new(Vec::new());
    let wrapped = std::cell::Cell::new(0usize);
    let start = |total: usize| started.set(total);
    let batch = |done: usize| batches.borrow_mut().push(done);
    let embed = |texts: &[String]| {
        wrapped.set(wrapped.get() + 1);
        crate::graph::embedder::Embedder::embed(&model, texts)
    };
    let hooks = EmbedHooks {
        batch_size: 2,
        embed_batch: Some(&embed),
        on_start: Some(&start),
        on_batch: Some(&batch),
        ..EmbedHooks::default()
    };
    let outcome =
        embed_property(&mut g, "Doc", "summary", EmbedMode::Changed, &model, &hooks).unwrap();

    assert_eq!(outcome.embedded, 5);
    assert_eq!(started.get(), 5, "the count arrives before the first batch");
    assert_eq!(*batches.borrow(), vec![2, 2, 1]);
    assert_eq!(wrapped.get(), 3, "every batch went through the wrapper");

    // …and an idle pass draws no bar at all.
    started.set(0);
    batches.borrow_mut().clear();
    embed_property(&mut g, "Doc", "summary", EmbedMode::Changed, &model, &hooks).unwrap();
    assert_eq!(started.get(), 0);
    assert!(batches.borrow().is_empty());
}

#[test]
fn a_written_pass_stamps_the_model_and_the_hashes_a_carry_moves() {
    let mut g = std::sync::Arc::new(docs(&[1]));
    embed_docs(&mut g, &StubEmbedder::new(2), EmbedMode::Changed).unwrap();
    let store = store_of(&g);
    assert_eq!(store.model_id.as_deref(), Some("stub"));
    let idx = doc_slot(&g, 0);
    assert!(store.get_embedding(idx).is_some());
    assert!(
        !store.is_stale(idx, EmbeddingStore::text_hash("text 1")),
        "the hash beside the vector is the text that produced it"
    );
}

/// An explicit build metric becomes the store's metric when the store declares
/// none, so a later metric-less query resolves the metric the index answers
/// under.
///
/// Before the fix the build returned `metric: "euclidean"` and built a
/// euclidean HNSW while the store kept `None` — which resolves to cosine — so
/// every metric-less query mismatched the index, fell back to the exact scan
/// for good, and `list_embeddings` reported the metric nothing used.
#[test]
fn an_explicit_build_metric_becomes_the_stores_metric() {
    let mut g = docs(&[1, 2, 3]);
    set_embeddings(
        &mut g,
        "Doc",
        "summary",
        None,
        batch(&[(1, [1.0, 0.0]), (2, [0.0, 1.0]), (3, [0.5, 0.5])]),
    )
    .unwrap();
    assert_eq!(store_of(&g).metric, None);

    let report = build_vector_index(
        &mut g,
        "Doc",
        "summary",
        None,
        None,
        None,
        Some("euclidean"),
        None,
    )
    .unwrap();
    assert_eq!(report.metric, "euclidean");
    assert_eq!(store_of(&g).metric.as_deref(), Some("euclidean"));
    assert_eq!(
        list_embeddings(&g)
            .into_iter()
            .map(|info| info.metric)
            .collect::<Vec<_>>(),
        vec!["euclidean".to_string()]
    );
}

/// A build metric the store contradicts is refused rather than silently
/// producing an index the store's own default scoring cannot use.
#[test]
fn a_build_metric_contradicting_the_store_is_refused() {
    let mut g = docs(&[1, 2]);
    set_embeddings(
        &mut g,
        "Doc",
        "summary",
        Some("cosine"),
        batch(&[(1, [1.0, 0.0]), (2, [0.0, 1.0])]),
    )
    .unwrap();
    let error = build_vector_index(
        &mut g,
        "Doc",
        "summary",
        None,
        None,
        None,
        Some("euclidean"),
        None,
    )
    .unwrap_err();
    assert!(error.contains("declares metric 'cosine'"), "{error}");
    assert!(error.contains("requested 'euclidean'"), "{error}");
    assert!(!store_of(&g).has_index(), "the refusal builds nothing");
    assert_eq!(store_of(&g).metric.as_deref(), Some("cosine"));

    // The store's own metric still builds, and a metric-less build is unchanged.
    build_vector_index(
        &mut g,
        "Doc",
        "summary",
        None,
        None,
        None,
        Some("cosine"),
        None,
    )
    .unwrap();
    assert!(store_of(&g).has_index());
    assert_eq!(store_of(&g).metric.as_deref(), Some("cosine"));
}

/// The node mirror of the relationship manual-write rule: a caller-supplied
/// vector owns its cell, so `add_embeddings` drops the generated `text_hash`
/// and the store-wide model stamp even when the vector it writes is
/// byte-identical to the one already there. Without that, `embed_texts`
/// (`mode='changed'`) compares the generated hash and skips a node a manual
/// write took over.
#[test]
fn a_manual_add_of_an_identical_vector_clears_the_generated_hash() {
    let mut g = docs(&[1, 2]);
    set_embeddings(
        &mut g,
        "Doc",
        "summary",
        None,
        batch(&[(1, [1.0, 0.0]), (2, [0.0, 1.0])]),
    )
    .unwrap();
    {
        let store = g
            .embeddings
            .get_mut(&("Doc".to_string(), "summary_emb".to_string()))
            .expect("store");
        store.set_text_hash(0, 11);
        store.set_text_hash(1, 22);
        store.model_id = Some("model-a".to_string());
    }

    add_embeddings(&mut g, "Doc", "summary", None, batch(&[(1, [1.0, 0.0])])).unwrap();

    let store = store_of(&g);
    assert_eq!(store.get_embedding(0), Some(&[1.0, 0.0][..]));
    assert_eq!(
        store.text_hashes.get(&0),
        None,
        "the manual write owns this cell"
    );
    assert_eq!(
        store.text_hashes.get(&1),
        Some(&22),
        "an unselected cell keeps its generated hash"
    );
    assert_eq!(store.model_id, None);
}

/// The `Doc.summary` store's cells in dense slot order — slot order is scan
/// order, and scan order decides score ties.
fn dense_cells(g: &DirGraph) -> Vec<(usize, Vec<f32>)> {
    let store = store_of(g);
    store
        .slot_to_node
        .iter()
        .map(|&node| {
            (
                node,
                store.get_embedding(node).expect("dense slot").to_vec(),
            )
        })
        .collect()
}

fn run_cypher(graph: &mut DirGraph, source: &str) -> Result<(), String> {
    let params = HashMap::new();
    crate::graph::session::execute::execute_mut(
        graph,
        source,
        &crate::graph::session::execute::ExecuteOptions::eager(&params),
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

/// A rolled-back node `DELETE` leaves the HNSW index where it found it. The
/// prune invalidates it and the undo's `restore_embedding` invalidates it
/// again, so without the captured index state a statement that failed *after*
/// a delete silently dropped an index it never touched: the vectors came back
/// and `SHOW INDEXES` reported none built.
#[test]
fn a_failed_statement_reverses_a_node_delete_with_its_vector_index() {
    let mut g = docs(&[1, 2, 3]);
    // `d3` keeps an incoming relationship, so the second, non-DETACH delete is
    // refused — a failure *after* the first delete, not a refusal before it.
    run_cypher(
        &mut g,
        "MATCH (a:Doc), (b:Doc) WHERE a.id = 2 AND b.id = 3 CREATE (a)-[:LINKS]->(b)",
    )
    .unwrap();
    set_embeddings(
        &mut g,
        "Doc",
        "summary",
        None,
        batch(&[(1, [1.0, 0.0]), (2, [0.0, 1.0]), (3, [0.6, 0.8])]),
    )
    .unwrap();
    build_vector_index(&mut g, "Doc", "summary", None, None, None, None, None).unwrap();
    let before_index = list_vector_indexes(&g);
    let before_cells = dense_cells(&g);
    assert!(before_index[0].built && !before_index[0].stale);

    let error = run_cypher(
        &mut g,
        "MATCH (n:Doc) WHERE n.id = 1 DELETE n WITH 1 AS kept \
         MATCH (m:Doc) WHERE m.id = 3 DELETE m RETURN kept",
    )
    .expect_err("the second delete must fail after the first one succeeded");
    assert!(error.contains("DETACH DELETE"), "{error}");

    assert_eq!(list_vector_indexes(&g), before_index);
    assert_eq!(dense_cells(&g), before_cells);
}
