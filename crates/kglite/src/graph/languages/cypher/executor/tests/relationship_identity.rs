use super::*;
use crate::datatypes::values::{NodeValue, PathValue, RelValue};
use crate::datatypes::PropMap;
use crate::graph::languages::cypher::result::CypherResult;
use std::collections::HashSet;

fn graph_with_one_relationship() -> DirGraph {
    let mut graph = DirGraph::new();
    for id in 1..=2 {
        let node = GraphWrite::add_node(
            &mut graph.graph,
            NodeData::new(
                Value::Int64(id),
                Value::String(format!("n{id}")),
                "N".into(),
                HashMap::new(),
                &mut graph.interner,
            ),
        );
        graph.type_indices.entry_or_default("N".into()).push(node);
    }
    GraphWrite::add_edge(
        &mut graph.graph,
        petgraph::graph::NodeIndex::new(0),
        petgraph::graph::NodeIndex::new(1),
        EdgeData::new(
            "R".into(),
            HashMap::from([("tag".into(), Value::String("old".into()))]),
            &mut graph.interner,
        ),
    );
    graph
}

/// Every relationship token reachable inside a value, however nested. The
/// assertions that a published result carries no token cannot be written as
/// value comparisons: equality deliberately ignores the field.
fn relationship_tokens(
    value: &Value,
) -> Box<dyn Iterator<Item = Option<crate::datatypes::values::RelationshipIncarnation>> + '_> {
    match value {
        Value::Relationship(rel) => Box::new(std::iter::once(rel.incarnation)),
        Value::Path(path) => Box::new(path.rels.iter().map(|rel| rel.incarnation)),
        Value::List(items) => Box::new(items.iter().flat_map(relationship_tokens)),
        Value::Map(map) => Box::new(map.iter().flat_map(|(_, item)| relationship_tokens(item))),
        _ => Box::new(std::iter::empty()),
    }
}

fn run_mutation(graph: &mut DirGraph, query: &str) -> CypherResult {
    let parsed = parser::parse_cypher(query).unwrap();
    super::super::write::execute_mutable(
        graph,
        &parsed,
        HashMap::new(),
        crate::graph::algorithms::Interrupt::from_deadline(None),
    )
    .unwrap()
}

#[test]
fn stale_binding_cannot_read_reused_slot_or_its_embedding() {
    let mut graph = graph_with_one_relationship();
    let result = run_mutation(
        &mut graph,
        "MATCH (a:N)-[r:R]->(b:N) DELETE r CREATE (a)-[fresh:R {tag:'fresh', text:'t'}]->(b) \
         WITH r, fresh CALL db.relationship_embeddings.set({type:'R', text_property:'text', \
         entries:[{relationship:fresh, vector:[1.0,0.0]}]}) YIELD stored \
         RETURN r.tag AS stale_tag, fresh.tag AS fresh_tag, r AS stale, \
         vector_score(r,'text_emb',[1.0,0.0]) AS stale_score, \
         embedding_norm(r,'text_emb') AS stale_norm, \
         vector_score(fresh,'text_emb',[1.0,0.0]) AS fresh_score",
    );
    assert_eq!(result.rows.len(), 1, "{result:?}");
    assert_eq!(result.rows[0][0], Value::Null);
    assert_eq!(result.rows[0][1], Value::String("fresh".into()));
    let Value::Relationship(stale) = &result.rows[0][2] else {
        panic!("expected stale relationship")
    };
    assert!(stale.properties.is_empty());
    assert_eq!(result.rows[0][3], Value::Null);
    assert_eq!(result.rows[0][4], Value::Null);
    assert_eq!(result.rows[0][5], Value::Float64(1.0));
}

#[test]
fn optimized_vector_score_where_does_not_keep_stale_reused_slot() {
    let mut graph = graph_with_one_relationship();
    let result = run_mutation(
        &mut graph,
        "MATCH (a:N)-[r:R]->(b:N) DELETE r CREATE (a)-[fresh:R {text:'t'}]->(b) \
         WITH r, fresh CALL db.relationship_embeddings.set({type:'R', text_property:'text', \
         entries:[{relationship:fresh, vector:[1.0,0.0]}]}) YIELD stored \
         WITH r WHERE vector_score(r,'text_emb',[1.0,0.0]) > 0.5 RETURN r",
    );
    assert!(result.rows.is_empty(), "{result:?}");
}

#[test]
fn detach_delete_retires_incident_slot_but_fresh_reuse_is_valid() {
    let mut graph = DirGraph::new();
    let source = GraphWrite::add_node(
        &mut graph.graph,
        NodeData::new(
            Value::Int64(1),
            Value::String("source".into()),
            "N".into(),
            HashMap::new(),
            &mut graph.interner,
        ),
    );
    let target = GraphWrite::add_node(
        &mut graph.graph,
        NodeData::new(
            Value::Int64(2),
            Value::String("target".into()),
            "N".into(),
            HashMap::new(),
            &mut graph.interner,
        ),
    );
    let edge = GraphWrite::add_edge(
        &mut graph.graph,
        source,
        target,
        EdgeData::new("R".into(), HashMap::new(), &mut graph.interner),
    );
    let mut identities =
        super::super::relationship_identity::StatementRelationshipIdentities::new();
    let stale = identities.capture(edge);

    super::super::delete_clause::invalidate_deleted_relationships(
        &graph,
        &HashSet::from([source]),
        &HashSet::new(),
        true,
        &mut identities,
    )
    .unwrap();
    crate::graph::mutation::maintain::detach_delete_nodes(&mut graph, &HashSet::from([source]));
    let replacement = GraphWrite::add_node(
        &mut graph.graph,
        NodeData::new(
            Value::Int64(3),
            Value::String("replacement".into()),
            "N".into(),
            HashMap::new(),
            &mut graph.interner,
        ),
    );
    let reused = GraphWrite::add_edge(
        &mut graph.graph,
        replacement,
        target,
        EdgeData::new("R".into(), HashMap::new(), &mut graph.interner),
    );
    assert_eq!(reused, edge, "fixture must exercise physical slot reuse");
    let fresh = identities.capture(reused);
    assert!(!identities.accepts(reused, stale));
    assert!(identities.accepts(reused, fresh));
}

#[test]
fn published_result_recursively_matches_untrusted_relationship_values() {
    let token = super::super::relationship_identity::StatementRelationshipIdentities::new()
        .capture(petgraph::graph::EdgeIndex::new(7));
    let expected = RelValue::new(7, 1, 2, "R".into(), PropMap::default());
    let mut trusted = expected.clone();
    trusted.incarnation = Some(token);
    let nested = Value::List(vec![
        Value::Path(Box::new(PathValue {
            nodes: Vec::<NodeValue>::new(),
            rels: vec![trusted.clone()],
        })),
        Value::Map(PropMap::from_pairs(vec![(
            "relationship".into(),
            Value::Relationship(Box::new(trusted.clone())),
        )])),
    ]);
    let mut result = CypherResult {
        columns: vec!["direct".into(), "nested".into()],
        rows: vec![vec![Value::Relationship(Box::new(trusted)), nested]],
        stats: None,
        profile: None,
        diagnostics: None,
        lazy: None,
    };

    crate::graph::languages::cypher::result::clear_published_relationship_incarnations(&mut result);
    // `RelValue`'s equality ignores the incarnation, so the structural
    // assertions below cannot see whether the token survived. Read the field.
    assert!(
        relationship_tokens(&result.rows[0][0])
            .chain(relationship_tokens(&result.rows[0][1]))
            .all(|token| token.is_none()),
        "publication must clear every nested token: {:?}",
        result.rows[0]
    );
    assert_eq!(
        result.rows[0][0],
        Value::Relationship(Box::new(expected.clone()))
    );
    assert_eq!(
        result.rows[0][1],
        Value::List(vec![
            Value::Path(Box::new(PathValue {
                nodes: vec![],
                rels: vec![expected.clone()],
            })),
            Value::Map(PropMap::from_pairs(vec![(
                "relationship".into(),
                Value::Relationship(Box::new(expected)),
            )])),
        ])
    );
}

#[test]
fn lazy_public_materialization_scrubs_nested_relationship_tokens() {
    let graph = DirGraph::new();
    let token = super::super::relationship_identity::StatementRelationshipIdentities::new()
        .capture(petgraph::graph::EdgeIndex::new(4));
    let expected = RelValue::new(4, 1, 2, "R".into(), PropMap::default());
    let mut trusted = expected.clone();
    trusted.incarnation = Some(token);
    let mut pending = ResultRow::new();
    pending.projected.insert(
        "nested".into(),
        Value::List(vec![Value::Path(Box::new(PathValue {
            nodes: vec![],
            rels: vec![trusted],
        }))]),
    );
    let descriptor = crate::graph::languages::cypher::result::LazyResultDescriptor::new(
        vec![pending],
        vec![crate::graph::languages::cypher::ast::ReturnItem {
            expression: Expression::Variable("nested".into()),
            alias: None,
        }],
        &graph,
    );

    let row = crate::graph::languages::cypher::result::materialise_lazy_row(&descriptor, &graph, 0)
        .unwrap();
    assert!(
        relationship_tokens(&row[0]).all(|token| token.is_none()),
        "lazy materialisation must clear every nested token: {row:?}"
    );
    assert_eq!(
        row,
        vec![Value::List(vec![Value::Path(Box::new(PathValue {
            nodes: vec![],
            rels: vec![expected],
        }))])]
    );
}

/// Three nodes in a chain, each hop carrying a distinct `text` property, so a
/// path over the chain exercises multi-hop materialisation and the embedding
/// procedures have something to embed.
fn graph_with_two_hop_chain() -> DirGraph {
    let mut graph = DirGraph::new();
    for id in 1..=3 {
        let node = GraphWrite::add_node(
            &mut graph.graph,
            NodeData::new(
                Value::Int64(id),
                Value::String(format!("n{id}")),
                "N".into(),
                HashMap::new(),
                &mut graph.interner,
            ),
        );
        graph.type_indices.entry_or_default("N".into()).push(node);
    }
    for (source, target, text) in [(0, 1, "alpha"), (1, 2, "beta")] {
        GraphWrite::add_edge(
            &mut graph.graph,
            petgraph::graph::NodeIndex::new(source),
            petgraph::graph::NodeIndex::new(target),
            EdgeData::new(
                "R".into(),
                HashMap::from([("text".into(), Value::String(text.into()))]),
                &mut graph.interner,
            ),
        );
    }
    graph
}

fn try_mutation(graph: &mut DirGraph, query: &str) -> Result<CypherResult, String> {
    let parsed = parser::parse_cypher(query).unwrap();
    super::super::write::execute_mutable(
        graph,
        &parsed,
        HashMap::new(),
        crate::graph::algorithms::Interrupt::from_deadline(None),
    )
}

/// A relationship reached through the bound variable and the same relationship
/// reached through `relationships(p)` are the same value. The derived
/// `PartialEq` compared the transient statement incarnation — present on the
/// binding, absent on the path — so every one of these answered `false` inside
/// a write statement while a read statement answered `true` (0.17.12 answered
/// `true` in both).
#[test]
fn path_relationship_equals_the_bound_relationship_inside_a_write_statement() {
    let mut graph = graph_with_one_relationship();
    let result = run_mutation(
        &mut graph,
        "MATCH p = (a:N)-[r:R]->(b:N) SET a.touched = 1 \
         WITH p, r RETURN r = relationships(p)[0] AS eq, r IN relationships(p) AS member, \
         [x IN relationships(p) WHERE x = r | x.tag] AS matched",
    );
    assert_eq!(
        result.rows,
        vec![vec![
            Value::Boolean(true),
            Value::Boolean(true),
            Value::List(vec![Value::String("old".into())]),
        ]],
        "{result:?}"
    );
}

/// `DISTINCT` and `ORDER BY` are the container contract, and a container has
/// one notion of "same key": the bound relationship and the path's view of it
/// must fold into a single group. Both the aggregate and the projection form
/// counted two.
#[test]
fn bound_and_path_relationships_are_one_key_for_distinct_and_ordering() {
    let mut graph = graph_with_one_relationship();
    let collected = run_mutation(
        &mut graph,
        "MATCH p = (a:N)-[r:R]->(b:N) SET a.touched = 1 \
         WITH p, r UNWIND [r, relationships(p)[0]] AS x \
         RETURN size(collect(DISTINCT x)) AS distinct_count",
    );
    assert_eq!(collected.rows, vec![vec![Value::Int64(1)]], "{collected:?}");

    let mut graph = graph_with_one_relationship();
    let projected = run_mutation(
        &mut graph,
        "MATCH p = (a:N)-[r:R]->(b:N) SET a.touched = 1 \
         WITH p, r UNWIND [r, relationships(p)[0]] AS x \
         WITH DISTINCT x ORDER BY x RETURN count(*) AS rows",
    );
    assert_eq!(projected.rows, vec![vec![Value::Int64(1)]], "{projected:?}");
}

/// `MATCH ()-[r]->() WHERE r IN rels DELETE r` deleted nothing: the membership
/// test compared incarnations. The delete itself goes through the ordinary
/// edge binding, so this asserts only what the predicate selects.
#[test]
fn membership_against_a_path_relationship_list_selects_the_bound_edge() {
    let mut graph = graph_with_one_relationship();
    let result = run_mutation(
        &mut graph,
        "MATCH p = (a:N)-[:R]->(b:N) SET a.touched = 1 \
         WITH collect(relationships(p)[0]) AS rels \
         MATCH ()-[r:R]->() WHERE r IN rels DELETE r RETURN size(rels) AS listed",
    );
    assert_eq!(result.rows, vec![vec![Value::Int64(1)]], "{result:?}");
    assert_eq!(
        result.stats.as_ref().map(|s| s.relationships_deleted),
        Some(1),
        "{result:?}"
    );
}

/// Path-derived relationships were refused by every write procedure with
/// "relationship was not bound by this statement" — a false claim: the path
/// came from this statement's own MATCH. Path materialisation now supplies the
/// statement token.
#[test]
fn path_relationships_are_accepted_by_set_and_remove() {
    let mut graph = graph_with_two_hop_chain();
    let stored = run_mutation(
        &mut graph,
        "MATCH p = (a:N)-[r:R]->(b:N) WHERE a.id = 1 WITH p, relationships(p)[0] AS pr \
         CALL db.relationship_embeddings.set({type:'R', text_property:'text', \
         entries:[{relationship: pr, vector:[1.0,0.0]}]}) YIELD stored RETURN stored",
    );
    assert_eq!(stored.rows, vec![vec![Value::Int64(1)]], "{stored:?}");

    let removed = run_mutation(
        &mut graph,
        "MATCH p = (a:N)-[r:R]->(b:N) WHERE a.id = 1 WITH p \
         CALL db.relationship_embeddings.remove({type:'R', text_property:'text', \
         relationships:[relationships(p)[0]]}) YIELD removed RETURN removed",
    );
    assert_eq!(removed.rows, vec![vec![Value::Int64(1)]], "{removed:?}");
}

/// The same acceptance for a variable-length path, whose relationships only
/// ever exist as materialised values — there is no bound variable to fall back
/// on, so the refusal made the whole shape unreachable.
#[test]
fn variable_length_path_relationships_are_accepted_by_set() {
    let mut graph = graph_with_two_hop_chain();
    let stored = run_mutation(
        &mut graph,
        "MATCH p = (a:N)-[:R*2..2]->(c:N) WITH relationships(p) AS rels UNWIND rels AS pr \
         CALL db.relationship_embeddings.set({type:'R', text_property:'text', \
         entries:[{relationship: pr, vector:[1.0,0.0]}]}) YIELD stored RETURN max(stored) AS stored",
    );
    assert_eq!(stored.rows, vec![vec![Value::Int64(2)]], "{stored:?}");
}

/// `db.relationship_embeddings.embed` takes the same values, so the generation path
/// reaches path relationships too.
#[test]
fn path_relationships_are_accepted_by_embed() {
    let mut graph = graph_with_two_hop_chain();
    let model = StubEmbedder { dimension: 2 };
    let service = crate::graph::edge_embedding_generation::EmbeddingExecutionService {
        model: &model,
        interrupt: crate::graph::algorithms::Interrupt::from_deadline(None),
    };
    let parsed = parser::parse_cypher(
        "MATCH p = (a:N)-[:R*2..2]->(c:N) WITH relationships(p) AS rels \
         CALL db.relationship_embeddings.embed({type:'R', text_property:'text', relationships: rels}) \
         YIELD embedded RETURN embedded",
    )
    .unwrap();
    let result = super::super::write::execute_mutable_with_csv(
        &mut graph,
        &parsed,
        HashMap::new(),
        crate::graph::algorithms::Interrupt::from_deadline(None),
        super::super::write::MutationLimits {
            max_work_units: None,
            row_limit: None,
        },
        &super::super::load_csv::CsvImportPolicy::Denied,
        Some(&service),
    )
    .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Int64(2)]], "{result:?}");
}

struct StubEmbedder {
    dimension: usize,
}

impl crate::graph::embedder::Embedder for StubEmbedder {
    fn dimension(&self) -> usize {
        self.dimension
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        Ok((0..texts.len()).map(|i| vec![i as f32, 1.0]).collect())
    }
    fn model_id(&self) -> Option<String> {
        Some("stub".to_string())
    }
    fn load(&self) -> Result<(), String> {
        Ok(())
    }
    fn unload(&self) {}
}

/// A path relationship whose edge was deleted earlier in the same statement is
/// not written: the hop's bind-time token no longer names what occupies the
/// slot, so the hop materialises as a tombstone and the procedure refuses it.
#[test]
fn path_relationship_deleted_earlier_in_the_statement_is_refused() {
    let mut graph = graph_with_two_hop_chain();
    let error = try_mutation(
        &mut graph,
        "MATCH p = (a:N)-[r:R]->(b:N) WHERE a.id = 1 DELETE r WITH p \
         CALL db.relationship_embeddings.set({type:'R', text_property:'text', \
         entries:[{relationship: relationships(p)[0], vector:[1.0,0.0]}]}) YIELD stored \
         RETURN stored",
    )
    .unwrap_err();
    assert!(
        error.contains("db.relationship_embeddings.set"),
        "unexpected error: {error}"
    );
    assert!(
        graph.edge_embeddings.is_empty(),
        "a refused statement must leave no store"
    );
}

/// A relationship that arrived as a query parameter never passed through this
/// statement's MATCH, so it carries no token and stays refused — the equality
/// change must not weaken that.
#[test]
fn round_tripped_parameter_relationship_is_still_refused() {
    let mut graph = graph_with_two_hop_chain();
    let parsed = parser::parse_cypher(
        "CALL db.relationship_embeddings.set({type:'R', text_property:'text', \
         entries:[{relationship: $rel, vector:[1.0,0.0]}]}) YIELD stored RETURN stored",
    )
    .unwrap();
    let param = RelValue::new(0, 0, 1, "R".into(), PropMap::default());
    let error = super::super::write::execute_mutable(
        &mut graph,
        &parsed,
        HashMap::from([(
            "rel".to_string(),
            Value::Relationship(Box::new(param.clone())),
        )]),
        crate::graph::algorithms::Interrupt::from_deadline(None),
    )
    .unwrap_err();
    assert!(
        error.contains("db.relationship_embeddings.set"),
        "unexpected error: {error}"
    );
    assert!(
        graph.edge_embeddings.is_empty(),
        "a refused statement must leave no store"
    );
}

/// A relationship handed back in as a parameter compares equal to the bound
/// relationship it describes. The parameter carries no statement token and the
/// binding does, so the derived equality called them different values: inside a
/// write statement `WHERE r = $rel` and `WHERE r IN $rels` — the shape a caller
/// writes after fetching relationships in an earlier query — matched nothing.
/// Equality is the five public fields; the token only gates writes.
#[test]
fn a_parameter_relationship_compares_equal_to_the_bound_relationship() {
    let mut graph = graph_with_one_relationship();
    let param = RelValue::new(
        0,
        0,
        1,
        "R".into(),
        PropMap::from_pairs(vec![("tag".into(), Value::String("old".into()))]),
    );
    let parsed = parser::parse_cypher(
        "MATCH (a:N)-[r:R]->(b:N) SET a.touched = 1 \
         RETURN r = $rel AS eq, r IN [$rel] AS member",
    )
    .unwrap();
    let result = super::super::write::execute_mutable(
        &mut graph,
        &parsed,
        HashMap::from([("rel".to_string(), Value::Relationship(Box::new(param)))]),
        crate::graph::algorithms::Interrupt::from_deadline(None),
    )
    .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Value::Boolean(true), Value::Boolean(true)]],
        "{result:?}"
    );
}

// ---------------------------------------------------------------------------
// Path hop identity (P3b)
// ---------------------------------------------------------------------------

/// The hop a MATCH bound is the hop the path keeps. A `DELETE` + `CREATE` that
/// reuses the storage slot inside the same statement used to be invisible to
/// the path: materialisation read the slot at *use* time and stamped the
/// *current* token, so `relationships(p)` handed back the replacement edge —
/// properties and all — as a member of a path that never matched it.
#[test]
fn path_hop_does_not_follow_a_reused_relationship_slot() {
    let mut graph = graph_with_one_relationship();
    let result = run_mutation(
        &mut graph,
        "MATCH p = (a:N)-[r:R]->(b:N) DELETE r CREATE (a)-[fresh:R {tag:'fresh'}]->(b) \
         WITH p, relationships(p) AS rels, relationships(p)[0] AS pr \
         RETURN size(rels) AS n, pr.tag AS tag, pr.type AS rel_type",
    );
    assert_eq!(result.rows.len(), 1, "{result:?}");
    assert_eq!(result.rows[0][0], Value::Int64(1), "{result:?}");
    assert_eq!(result.rows[0][1], Value::Null, "{result:?}");
    assert_eq!(result.rows[0][2], Value::String("R".into()), "{result:?}");
}

/// The same substituted hop must not reach a write procedure. Before the hop
/// carried its bind-time token the procedure saw the *current* token for the
/// slot, accepted it, and wrote the replacement edge's vector.
#[test]
fn stale_path_hop_is_refused_by_edge_embedding_set() {
    let mut graph = graph_with_one_relationship();
    let error = try_mutation(
        &mut graph,
        "MATCH p = (a:N)-[r:R]->(b:N) DELETE r CREATE (a)-[fresh:R {tag:'fresh'}]->(b) \
         WITH p CALL db.relationship_embeddings.set({type:'R', text_property:'text', \
         entries:[{relationship: relationships(p)[0], vector:[1.0,0.0]}]}) YIELD stored \
         RETURN stored",
    )
    .unwrap_err();
    assert!(
        error.contains("entries[0] is a 'R' relationship deleted or replaced earlier"),
        "expected a stale refusal, got: {error}"
    );
    assert!(
        graph.edge_embeddings.is_empty(),
        "a refused statement must leave no store"
    );
}

/// `DELETE` on the substituted hop is the destructive twin of the write
/// procedure: accepting it would delete an edge the MATCH never selected.
#[test]
fn stale_path_hop_is_refused_by_delete() {
    let mut graph = graph_with_one_relationship();
    let error = try_mutation(
        &mut graph,
        "MATCH p = (a:N)-[r:R]->(b:N) DELETE r CREATE (a)-[fresh:R {tag:'fresh'}]->(b) \
         WITH p, relationships(p)[0] AS pr DELETE pr RETURN 1 AS done",
    )
    .unwrap_err();
    assert!(
        error.contains("stale"),
        "expected a stale refusal, got: {error}"
    );
}

/// A path bound *after* the reuse names the fresh edge, and writes to it.
#[test]
fn path_bound_after_the_reuse_sees_the_fresh_relationship() {
    let mut graph = graph_with_one_relationship();
    let result = run_mutation(
        &mut graph,
        "MATCH (a:N)-[r:R]->(b:N) DELETE r CREATE (a)-[fresh:R {tag:'fresh', text:'t'}]->(b) \
         WITH a MATCH p = (a)-[:R]->() WITH p, relationships(p)[0] AS pr \
         CALL db.relationship_embeddings.set({type:'R', text_property:'text', \
         entries:[{relationship: pr, vector:[1.0,0.0]}]}) YIELD stored \
         RETURN pr.tag AS tag, stored",
    );
    assert_eq!(
        result.rows,
        vec![vec![Value::String("fresh".into()), Value::Int64(1)]],
        "{result:?}"
    );
}

/// A read statement has no identity tracking at all: every hop token is absent
/// and `relationships(p)` still materialises the live edge. This is the cell
/// that proves the fix costs the read path nothing observable.
#[test]
fn read_statement_path_relationships_are_unchanged() {
    let graph = graph_with_one_relationship();
    let parsed = parser::parse_cypher(
        "MATCH p = (a:N)-[r:R]->(b:N) WITH r, p, relationships(p)[0] AS pr \
         RETURN r = pr AS eq, size(relationships(p)) AS n, pr AS rel",
    )
    .unwrap();
    let params = HashMap::new();
    let executor = CypherExecutor::with_params(&graph, &params, None);
    let result = executor.execute(&parsed).unwrap();
    assert_eq!(result.rows.len(), 1, "{result:?}");
    assert_eq!(result.rows[0][0], Value::Boolean(true), "{result:?}");
    assert_eq!(result.rows[0][1], Value::Int64(1), "{result:?}");
    let Value::Relationship(rel) = &result.rows[0][2] else {
        panic!("expected a relationship, got {:?}", result.rows[0][2])
    };
    assert_eq!(rel.rel_type, "R");
    assert_eq!(
        rel.properties.get("tag"),
        Some(&Value::String("old".into()))
    );
}

/// Every hop of a variable-length path carries its own bind-time token, so
/// retiring one hop's slot leaves the other hop writable and the retired one
/// refused — not the whole path, and not the replacement edge.
#[test]
fn variable_length_path_hops_carry_per_hop_tokens() {
    let mut graph = graph_with_two_hop_chain();
    let error = try_mutation(
        &mut graph,
        "MATCH p = (a:N)-[:R*2..2]->(c:N) WITH p \
         MATCH (x:N)-[r:R]->(y:N) WHERE x.id = 1 DELETE r \
         CREATE (x)-[:R {text:'fresh'}]->(y) WITH p UNWIND relationships(p) AS pr \
         CALL db.relationship_embeddings.set({type:'R', text_property:'text', \
         entries:[{relationship: pr, vector:[1.0,0.0]}]}) YIELD stored RETURN stored",
    )
    .unwrap_err();
    assert!(
        error.contains("deleted or replaced earlier in this statement"),
        "expected a stale refusal, got: {error}"
    );
}

/// A `FOREACH` that creates relationships, then a `MATCH` over them in the same
/// statement: the path is bound after the creation, so its hops are current and
/// the write procedure accepts them.
#[test]
fn path_matched_after_a_foreach_create_is_writable() {
    let mut graph = graph_with_one_relationship();
    let result = run_mutation(
        &mut graph,
        "MATCH (a:N)-[:R]->(b:N) \
         FOREACH (i IN [1] | CREATE (a)-[:S {text:'made'}]->(b)) \
         WITH 1 AS ignored MATCH p = (:N)-[:S]->(:N) WITH p, relationships(p)[0] AS pr \
         CALL db.relationship_embeddings.set({type:'S', text_property:'text', \
         entries:[{relationship: pr, vector:[1.0,0.0]}]}) YIELD stored RETURN stored",
    );
    assert_eq!(result.rows, vec![vec![Value::Int64(1)]], "{result:?}");
}

/// The same for `MERGE`: the created relationship is only reachable through a
/// path bound after it, and the path's hop must be writable.
#[test]
fn path_matched_after_a_merge_is_writable() {
    let mut graph = graph_with_one_relationship();
    let result = run_mutation(
        &mut graph,
        "MATCH (a:N)-[:R]->(b:N) MERGE (a)-[m:S {text:'merged'}]->(b) \
         WITH 1 AS ignored MATCH p = (:N)-[:S]->(:N) WITH p, relationships(p)[0] AS pr \
         CALL db.relationship_embeddings.set({type:'S', text_property:'text', \
         entries:[{relationship: pr, vector:[1.0,0.0]}]}) YIELD stored RETURN stored",
    );
    assert_eq!(result.rows, vec![vec![Value::Int64(1)]], "{result:?}");
}
