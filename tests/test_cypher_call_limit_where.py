"""Absolute-value goldens for a LIMIT after a WHERE the executor cannot fold.

The differential corpus compares optimized against unoptimized *reads*, so it
cannot see the mutation path, and it cannot see a defect both runs share. These
pin the expected values directly.
"""

import pytest

import kglite


def _graph() -> kglite.KnowledgeGraph:
    graph = kglite.KnowledgeGraph()
    graph.cypher("UNWIND range(1, 6) AS k CREATE (:B {k: k})").to_list()
    return graph


def _keys(graph: kglite.KnowledgeGraph) -> list[int]:
    return [r["k"] for r in graph.cypher("MATCH (b:B) RETURN b.k AS k ORDER BY k").to_list()]


def test_delete_through_with_limit_after_non_pushable_where():
    graph = _graph()
    rows = graph.cypher("MATCH (b:B) WHERE b.k + 0 > 2 WITH b LIMIT 2 DELETE b RETURN count(*) AS n").to_list()
    assert rows == [{"n": 2}]
    # Exactly two of the four candidates (k = 3..6) are gone.
    survivors = _keys(graph)
    assert len(survivors) == 4
    assert survivors[:2] == [1, 2]


def test_set_through_with_limit_after_non_pushable_where():
    graph = _graph()
    rows = graph.cypher("MATCH (b:B) WHERE b.k + 0 > 2 WITH b LIMIT 2 SET b.hit = 1 RETURN count(*) AS n").to_list()
    assert rows == [{"n": 2}]
    hit = graph.cypher("MATCH (b:B) WHERE b.hit = 1 RETURN b.k AS k ORDER BY k").to_list()
    assert len(hit) == 2
    assert all(r["k"] > 2 for r in hit)


def test_read_form_of_the_mutation_query_agrees():
    graph = _graph()
    rows = graph.cypher("MATCH (b:B) WHERE b.k + 0 > 2 WITH b LIMIT 2 RETURN count(*) AS n").to_list()
    assert rows == [{"n": 2}]


def test_graphbridge_identified_edge_statement_counts_one_per_edge():
    """GraphBridge's verbatim identified-relationship probe: with the hint ahead
    of the correlated WHERE, every edge past the first two in scan order counted 0."""
    graph = kglite.KnowledgeGraph()
    graph.cypher("UNWIND range(1, 6) AS k CREATE (:Person {pid: 'p' + toString(k)}), (:Movie {pid: 'm' + toString(k)})")
    graph.cypher(
        "MATCH (p:Person), (m:Movie) WHERE right(p.pid, 1) = right(m.pid, 1) "
        "CREATE (p)-[:ACTED_IN {rid: 'e' + right(p.pid, 1)}]->(m)"
    ).to_list()
    query = (
        "MATCH (s:`Person`)-[r:`ACTED_IN`]->(t:`Movie`)\n"
        "CALL (r) {\n"
        "  MATCH ()-[candidate:`ACTED_IN`]->()\n"
        "  WHERE candidate.`rid` = r.`rid`\n"
        "  WITH candidate LIMIT 2\n"
        "  RETURN count(*) AS `__gb_ie_count`\n"
        "}\n"
        "WITH s, r, t, `__gb_ie_count`\n"
        "RETURN r.`rid` AS c0, `__gb_ie_count` AS d ORDER BY c0"
    )
    assert graph.cypher(query).to_list() == [{"c0": f"e{k}", "d": 1} for k in range(1, 7)]


def test_correlated_count_with_import_in_where():
    graph = _graph()
    graph.cypher("UNWIND range(1, 3) AS k CREATE (:A {k: k})").to_list()
    rows = graph.cypher(
        "MATCH (a:A) CALL (a) { MATCH (b:B) WHERE b.k >= a.k RETURN count(*) AS n } RETURN a.k AS k, n ORDER BY k"
    ).to_list()
    assert rows == [{"k": 1, "n": 6}, {"k": 2, "n": 5}, {"k": 3, "n": 4}]


def test_top_level_read_still_takes_the_limit_hint():
    """Non-vacuity: the bail is scoped to CALL bodies and mutations, so the
    top-level early stop must still fire."""
    graph = _graph()
    ops = [
        r["operation"] for r in graph.cypher("EXPLAIN MATCH (b:B) WHERE b.k + 0 > 2 RETURN b.k AS k LIMIT 2").to_list()
    ]
    assert "OptimizerPass push_limit_into_match" in ops
    declined = [
        r["operation"]
        for r in graph.cypher(
            "EXPLAIN MATCH (b:B) WHERE b.k + 0 > 2 WITH b LIMIT 2 DELETE b RETURN count(*) AS n"
        ).to_list()
    ]
    assert "OptimizerPass push_limit_into_match" not in declined


@pytest.mark.parametrize("spelling", ["CALL (a) {", "CALL { WITH a"])
def test_correlated_limit_after_where_is_per_outer_row(spelling):
    graph = _graph()
    graph.cypher("UNWIND range(1, 6) AS k CREATE (:A {k: k})").to_list()
    rows = graph.cypher(
        f"MATCH (a:A) {spelling} MATCH (b:B) WHERE b.k = a.k RETURN b.k AS x LIMIT 2 }} RETURN a.k AS ak, x ORDER BY ak"
    ).to_list()
    assert rows == [{"ak": k, "x": k} for k in range(1, 7)]
