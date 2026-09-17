//! **Absolute goldens for the fused `MATCH … WITH <group>, count(…)` path.**
//!
//! Two silent wrong answers lived behind `fuse_match_with_aggregate`, both
//! producing rows nobody asked for and both invisible to the parity oracles
//! because only the *optimised* plan was wrong:
//!
//! 1. `try_fast_with_aggregate_via_histogram` counted every peer of the
//!    connection type and never applied the GROUP node's own label, so
//!    `MATCH (c)-[:CHILD_OF]->(p:Software) WITH p, count(c)` returned a row
//!    for `:Api` and `:Doc` parents too. The histogram is served by the
//!    in-memory backend as well as disk, so every storage mode was affected.
//!    An `:A|B` alternation on that node was dropped the same way, and on the
//!    source side `node_type` read only the FIRST alternation branch.
//! 2. The two-MATCH variant deduplicated M1's group keys without keeping the
//!    row multiplicity it stood for, so a group key M1 bound n times reported
//!    `count(r) / n`.
//!
//! Every expectation below is computed by hand from [`parent_child_graph`];
//! each case also asserts the optimised and unoptimised plans agree, so a
//! regression shows up as a divergence even if the golden is miscopied.

use super::*;
use crate::graph::languages::cypher::planner;

/// Nine nodes over one `CHILD_OF` edge type:
///
/// ```text
/// parents:  s:Software   a:Api   d:Doc
/// children: c1:Software -> a     c4:Api  -> s
///           c2:Software -> a     c5:Api  -> d
///           c3:Software -> s     c6:Doc  -> d
/// ```
///
/// So: `s` has 2 children (1 Software, 1 Api), `a` has 2 (both Software),
/// `d` has 2 (1 Api, 1 Doc).
fn parent_child_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    let specs = [
        (1u32, "s", "Software"),
        (2, "a", "Api"),
        (3, "d", "Doc"),
        (4, "c1", "Software"),
        (5, "c2", "Software"),
        (6, "c3", "Software"),
        (7, "c4", "Api"),
        (8, "c5", "Api"),
        (9, "c6", "Doc"),
    ];
    let mut idx = Vec::new();
    for (id, concept_id, label) in specs {
        let node = NodeData::new(
            Value::UniqueId(id),
            Value::String(concept_id.to_string()),
            label.to_string(),
            HashMap::from([(
                "concept_id".to_string(),
                Value::String(concept_id.to_string()),
            )]),
            &mut graph.interner,
        );
        let i = graph.graph.add_node(node);
        graph
            .type_indices
            .entry_or_default(label.to_string())
            .push(i);
        idx.push(i);
    }
    for (child, parent) in [(3usize, 1usize), (4, 1), (5, 0), (6, 0), (7, 2), (8, 2)] {
        let edge = EdgeData::new("CHILD_OF".to_string(), HashMap::new(), &mut graph.interner);
        graph.graph.add_edge(idx[child], idx[parent], edge);
    }
    graph.register_connection_type("CHILD_OF".to_string());
    graph
}

/// Run on both plans, require agreement, and return the rows sorted so the
/// golden does not depend on hash-map iteration order. Neither plan orders an
/// ungrouped aggregate, so the agreement check sorts too — it is asserting
/// the same multiset of rows, not the same sequence.
fn rows(graph: &DirGraph, query: &str) -> Vec<Vec<Value>> {
    let no_params = HashMap::new();
    let parsed = parser::parse_cypher(query)
        .unwrap_or_else(|e| panic!("query failed to parse: {query}\n  error: {e}"));
    let plain = CypherExecutor::with_params(graph, &no_params, None)
        .execute(&parsed)
        .unwrap_or_else(|e| panic!("query failed: {query}\n  error: {e}"));
    let mut optimized = parsed;
    planner::optimize(&mut optimized, graph, &no_params);
    let planned = CypherExecutor::with_params(graph, &no_params, None)
        .execute(&optimized)
        .unwrap_or_else(|e| panic!("optimised query failed: {query}\n  error: {e}"));
    let sorted = |mut rs: Vec<Vec<Value>>| {
        rs.sort_by_key(|r| format!("{r:?}"));
        rs
    };
    let out = sorted(planned.rows);
    assert_eq!(
        sorted(plain.rows),
        out,
        "optimised and unoptimised plans disagree for: {query}"
    );
    out
}

fn expect(graph: &DirGraph, query: &str, want: &[(&str, i64)]) {
    let got = rows(graph, query);
    let want: Vec<Vec<Value>> = want
        .iter()
        .map(|(id, k)| vec![Value::String((*id).to_string()), Value::Int64(*k)])
        .collect();
    assert_eq!(got, want, "wrong rows for: {query}");
}

/// The reported shape: only `s` is `:Software`, and it has two children.
/// Pre-fix this also returned `a` (`:Api`) and `d` (`:Doc`), because the
/// histogram path applied no label filter to the group node at all.
#[test]
fn label_constrained_histogram_group_keeps_only_matching_parents() {
    let g = parent_child_graph();
    expect(
        &g,
        "MATCH (c)-[:CHILD_OF]->(p:Software) WITH p, count(c) AS k \
         RETURN p.concept_id AS id, k",
        &[("s", 2)],
    );
}

/// The same pattern written end-first. `optimize_pattern_start_node` reverses
/// one spelling into the other, so both must be pinned.
#[test]
fn label_constrained_histogram_group_survives_pattern_reversal() {
    let g = parent_child_graph();
    expect(
        &g,
        "MATCH (p:Software)<-[:CHILD_OF]-(c) WITH p, count(c) AS k \
         RETURN p.concept_id AS id, k",
        &[("s", 2)],
    );
}

/// `count(r)` over the edge variable and `count(*)` take the same fused
/// clause as `count(c)`; all three dropped the label.
#[test]
fn label_constrained_histogram_group_applies_to_every_count_spelling() {
    let g = parent_child_graph();
    expect(
        &g,
        "MATCH (c)-[r:CHILD_OF]->(p:Software) WITH p, count(r) AS k \
         RETURN p.concept_id AS id, k",
        &[("s", 2)],
    );
    expect(
        &g,
        "MATCH (c)-[:CHILD_OF]->(p:Software) WITH p, count(*) AS k \
         RETURN p.concept_id AS id, k",
        &[("s", 2)],
    );
}

/// A downstream `ORDER BY … LIMIT` (absorbed as `top_k`), a bare `LIMIT`, and
/// a `WITH … WHERE` all re-enter through the same fused clause.
#[test]
fn label_constrained_histogram_group_applies_under_limit_and_where() {
    let g = parent_child_graph();
    expect(
        &g,
        "MATCH (c)-[:CHILD_OF]->(p:Software) WITH p, count(c) AS k \
         RETURN p.concept_id AS id, k ORDER BY k DESC LIMIT 3",
        &[("s", 2)],
    );
    expect(
        &g,
        "MATCH (c)-[:CHILD_OF]->(p:Software) WITH p, count(c) AS k LIMIT 3 \
         RETURN p.concept_id AS id, k",
        &[("s", 2)],
    );
    expect(
        &g,
        "MATCH (c)-[:CHILD_OF]->(p:Software) WITH p, count(c) AS k WHERE k > 0 \
         RETURN p.concept_id AS id, k",
        &[("s", 2)],
    );
}

/// Alternation on the group node: `:Software|Api` keeps `s` and `a` and must
/// drop `d`. Reading `node_type` alone would have kept only `s`; reading
/// nothing (the pre-fix behaviour) kept `d` as well.
#[test]
fn label_alternation_on_the_group_node_keeps_every_branch_and_only_those() {
    let g = parent_child_graph();
    expect(
        &g,
        "MATCH (c)-[:CHILD_OF]->(p:Software|Api) WITH p, count(c) AS k \
         RETURN p.concept_id AS id, k",
        &[("a", 2), ("s", 2)],
    );
}

/// Alternation on the SOURCE node. This fixture reaches the answer through
/// the generic path — the branch-narrowing `node_type` read lives in the
/// disk-only source sweep, which only
/// `tests/test_cypher_fused_aggregate_labels.py` (disk parameter) exercises.
/// The golden is here so the two suites pin the same expected rows.
#[test]
fn label_alternation_on_the_source_node_counts_every_branch() {
    let g = parent_child_graph();
    expect(
        &g,
        "MATCH (c:Software|Api)-[:CHILD_OF]->(p) WITH p, count(c) AS k \
         RETURN p.concept_id AS id, k",
        &[("a", 2), ("d", 1), ("s", 2)],
    );
}

/// Two-MATCH fusion: M1 binds `p = s` twice (once per child), M2 finds two
/// `CHILD_OF` edges into `s`, and the join yields 2 × 2 = 4 rows. Pre-fix the
/// planner's group-key dedup reported 2.
#[test]
fn two_match_fusion_keeps_the_first_match_row_multiplicity() {
    let g = parent_child_graph();
    expect(
        &g,
        "MATCH (c)-[:CHILD_OF]->(p:Software) MATCH (p)<-[r:CHILD_OF]-() \
         WITH p, count(r) AS k RETURN p.concept_id AS id, k",
        &[("s", 4)],
    );
}

/// Same multiplicity bug with a label on M2's far endpoint: every parent is
/// bound twice by M1, and M2 matches one `:Api` child for `s` and one for `d`.
#[test]
fn two_match_fusion_multiplicity_applies_with_a_labelled_secondary_pattern() {
    let g = parent_child_graph();
    expect(
        &g,
        "MATCH (c)-[:CHILD_OF]->(p) MATCH (p)<-[r:CHILD_OF]-(:Api) \
         WITH p, count(r) AS k RETURN p.concept_id AS id, k",
        &[("d", 2), ("s", 2)],
    );
}

/// Guard rail: shapes that were already correct stay correct — a property map
/// on the group node (which bails fusion outright), a label on the source
/// only, and the `RETURN`-side aggregate that never had the defect.
#[test]
fn unaffected_aggregate_shapes_keep_their_answers() {
    let g = parent_child_graph();
    expect(
        &g,
        "MATCH (c)-[:CHILD_OF]->(p {concept_id:'s'}) WITH p, count(c) AS k \
         RETURN p.concept_id AS id, k",
        &[("s", 2)],
    );
    expect(
        &g,
        "MATCH (c:Software)-[:CHILD_OF]->(p) WITH p, count(c) AS k \
         RETURN p.concept_id AS id, k",
        &[("a", 2), ("s", 1)],
    );
    expect(
        &g,
        "MATCH (c)-[:CHILD_OF]->(p:Software) RETURN p.concept_id AS id, count(c) AS k",
        &[("s", 2)],
    );
}
