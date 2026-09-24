//! `type()`, `startNode()`, `endNode()`, `keys()` and `properties()` on a
//! relationship *value* — anything that is not a MATCH binding. Absolute
//! goldens: the optimiser differential corpus cannot see this class, because a
//! value-arm defect returns the same wrong answer with the passes on and off.
use super::*;
use crate::graph::edge_embeddings::upsert_edge_embeddings;
use crate::graph::languages::cypher::result::CypherResult;
use petgraph::graph::{EdgeIndex, NodeIndex};

/// Two nodes whose logical ids (10, 11) differ from their slots (0, 1), and
/// one `CLAIMS` relationship from 11 to 10 — pointing against slot order, so a
/// swapped start/end or a slot-for-id mix-up cannot pass.
fn claims_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    for id in [10, 11] {
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
        NodeIndex::new(1),
        NodeIndex::new(0),
        EdgeData::new(
            "CLAIMS".into(),
            HashMap::from([("rank".into(), Value::Int64(7))]),
            &mut graph.interner,
        ),
    );
    upsert_edge_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![(EdgeIndex::new(0), vec![1.0, 0.0])],
        Some("cosine"),
    )
    .unwrap();
    graph
}

fn read(graph: &DirGraph, source: &str) -> CypherResult {
    CypherExecutor::with_params(graph, &HashMap::new(), None)
        .execute(&parser::parse_cypher(source).unwrap())
        .unwrap()
}

fn mutate(graph: &mut DirGraph, source: &str) -> CypherResult {
    execute_mutable(
        graph,
        &parser::parse_cypher(source).unwrap(),
        HashMap::new(),
        crate::graph::algorithms::Interrupt::from_deadline(None),
    )
    .unwrap()
}

const ACCESSORS: &str = "RETURN type(rel) AS t, startNode(rel).id AS s, endNode(rel).id AS e, \
                         keys(rel) AS k, properties(rel) AS p";

fn expected_row() -> Vec<Value> {
    let mut props = crate::datatypes::PropMap::new();
    props.insert("rank", Value::Int64(7));
    props.insert("type", Value::String("CLAIMS".into()));
    vec![
        Value::String("CLAIMS".into()),
        Value::Int64(11),
        Value::Int64(10),
        Value::List(vec![
            Value::String("rank".into()),
            Value::String("type".into()),
        ]),
        Value::Map(props),
    ]
}

fn assert_accessors(graph: &DirGraph, producer: &str) {
    let result = read(graph, &format!("{producer} {ACCESSORS}"));
    assert_eq!(result.rows, vec![expected_row()], "{producer}: {result:?}");
}

#[test]
fn bound_relationship_is_the_reference() {
    assert_accessors(&claims_graph(), "MATCH ()-[rel:CLAIMS]->()");
}

#[test]
fn collected_relationship_value() {
    assert_accessors(
        &claims_graph(),
        "MATCH ()-[r:CLAIMS]->() WITH collect(r)[0] AS rel",
    );
}

#[test]
fn unwound_relationship_value() {
    assert_accessors(
        &claims_graph(),
        "MATCH ()-[r:CLAIMS]->() WITH collect(r) AS rs UNWIND rs AS rel",
    );
}

#[test]
fn edge_embeddings_query_relationship_column() {
    assert_accessors(
        &claims_graph(),
        "CALL db.edge_embeddings.query({type:'CLAIMS', text_property:'text', \
         vector:[1.0,0.0], top_k:1, exact:true}) YIELD relationship WITH relationship AS rel",
    );
}

#[test]
fn call_subquery_relationship_column() {
    assert_accessors(
        &claims_graph(),
        "CALL { MATCH ()-[r:CLAIMS]->() RETURN collect(r)[0] AS rel } WITH rel",
    );
}

#[test]
fn path_relationship_value() {
    assert_accessors(
        &claims_graph(),
        "MATCH p = ()-[:CLAIMS]->() WITH relationships(p)[0] AS rel",
    );
}

/// The accessors inside a write statement: the value carries this statement's
/// token, and the live slot is still the relationship it names.
#[test]
fn collected_relationship_value_inside_a_write_statement() {
    let mut graph = claims_graph();
    let result = mutate(
        &mut graph,
        &format!("MATCH ()-[r:CLAIMS]->() SET r.seen = true WITH collect(r)[0] AS rel {ACCESSORS}"),
    );
    assert_eq!(result.rows.len(), 1, "{result:?}");
    assert_eq!(result.rows[0][..3], expected_row()[..3], "{result:?}");
}

/// A binding whose slot was deleted and then reused by a different
/// relationship earlier in the same statement names nothing live: the
/// accessors must not report the replacement's type or endpoints.
#[test]
fn stale_binding_does_not_read_the_reused_slot() {
    let mut graph = claims_graph();
    let result = mutate(
        &mut graph,
        "MATCH (a:N {id: 11})-[r:CLAIMS]->(b:N {id: 10}) DELETE r \
         CREATE (b)-[fresh:OTHER]->(a) \
         RETURN type(r) AS t, startNode(r).id AS s, endNode(r).id AS e, \
         type(fresh) AS ft, id(r) = id(fresh) AS reused",
    );
    assert_eq!(result.rows.len(), 1, "{result:?}");
    assert_eq!(
        result.rows[0][4],
        Value::Boolean(true),
        "precondition: the slot was reused: {result:?}"
    );
    assert_eq!(result.rows[0][3], Value::String("OTHER".into()));
    assert_eq!(
        result.rows[0][..3],
        [Value::Null, Value::Null, Value::Null],
        "{result:?}"
    );
}

/// A value carries its own type and endpoints, so a snapshot taken before the
/// delete keeps reporting them after its slot is reused.
#[test]
fn snapshot_value_keeps_its_own_type_after_the_slot_is_reused() {
    let mut graph = claims_graph();
    let result = mutate(
        &mut graph,
        "MATCH (a:N {id: 11})-[r:CLAIMS]->(b:N {id: 10}) \
         WITH a, b, r, [r][0] AS snapshot DELETE r \
         CREATE (b)-[fresh:OTHER]->(a) \
         RETURN type(snapshot) AS t, startNode(snapshot).id AS s, \
         endNode(snapshot).id AS e, id(snapshot) = id(fresh) AS reused",
    );
    assert_eq!(
        result.rows,
        vec![vec![
            Value::String("CLAIMS".into()),
            Value::Int64(11),
            Value::Int64(10),
            Value::Boolean(true),
        ]],
        "{result:?}"
    );
}

/// A retired binding materialises as the token-only shape (empty type, no
/// token, pattern-order ids): every accessor on that value is null rather than
/// borrowing the replacement relationship now in the slot.
#[test]
fn token_only_stale_value_reads_as_null() {
    let mut graph = claims_graph();
    let result = mutate(
        &mut graph,
        "MATCH (a:N {id: 11})-[r:CLAIMS]->(b:N {id: 10}) DELETE r \
         CREATE (b)-[fresh:OTHER]->(a) WITH r, fresh, [r][0] AS rel \
         RETURN type(rel) AS t, startNode(rel).id AS s, endNode(rel).id AS e, \
         keys(rel) AS k, properties(rel) AS p, keys(r) AS bk, properties(r) AS bp, \
         id(rel) = id(fresh) AS reused",
    );
    assert_eq!(
        result.rows,
        vec![vec![
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Boolean(true),
        ]],
        "{result:?}"
    );
}
