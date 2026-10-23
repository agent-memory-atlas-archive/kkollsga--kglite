"""Goldens for `hoist_where_over_call_subquery`.

The differential corpus (`call_where_*`) proves the hoisted plan answers what
the plain one does; these pin the expected values, and that the CALL body runs
only for the rows the filter keeps (`PROFILE` rows_in) — and for every row when
the pass is off or the body disqualifies it.

`social_graph`: Person_1..Person_20 with age 20 + i; each knows the next three
(none past Person_20), so the outgoing KNOWS count is 3, 3, ..., 2, 1, 0.
"""

import pytest

PASS = "hoist_where_over_call_subquery"
CNT = "CALL (p) { MATCH (p)-[:KNOWS]->(q) RETURN count(q) AS c }"


def _call_rows_in(graph, query, **kwargs):
    profile = graph.cypher("PROFILE " + query, **kwargs).profile
    return next(step["rows_in"] for step in profile if step["clause"] == "CallSubquery")


@pytest.mark.parametrize(
    "query,expected,rows_in",
    [
        (
            f"MATCH (p:Person) {CNT} WHERE p.age > 36 RETURN p.name AS n, c ORDER BY n",
            [("Person_17", 3), ("Person_18", 2), ("Person_19", 1), ("Person_20", 0)],
            4,
        ),
        (
            f"MATCH (p:Person) {CNT} WITH p, c WHERE p.name = 'Person_20' RETURN p.name AS n, c",
            [("Person_20", 0)],
            1,
        ),
        (
            # `c > 0` reads a CALL output and stays behind it; `p.age` moves
            f"MATCH (p:Person) {CNT} WHERE p.age > 38 AND c > 0 RETURN p.name AS n, c ORDER BY n",
            [("Person_19", 1)],
            2,
        ),
        (
            # a body that matches nothing still returns its one row of zero
            "MATCH (p:Person) CALL (p) { MATCH (p)-[:NOPE]->(q) RETURN count(q) AS c } "
            "WITH p, c WHERE p.age < 23 RETURN p.name AS n, c ORDER BY n",
            [("Person_1", 0), ("Person_2", 0)],
            2,
        ),
    ],
)
def test_hoisted_answer_and_body_runs(social_graph, query, expected, rows_in):
    rows = [tuple(r.values()) for r in social_graph.cypher(query).to_list()]
    assert rows == expected
    off = [tuple(r.values()) for r in social_graph.cypher(query, disabled_passes=[PASS]).to_list()]
    assert off == expected
    assert _call_rows_in(social_graph, query) == rows_in
    assert _call_rows_in(social_graph, query, disabled_passes=[PASS]) == 20


def test_the_body_runs_only_for_the_rows_the_filter_keeps(social_graph):
    query = f"MATCH (p:Person) {CNT} WHERE p.age > 38 RETURN p.name AS n, c"
    assert _call_rows_in(social_graph, query) == 2
    assert _call_rows_in(social_graph, query, disabled_passes=[PASS]) == 20


def test_collect_limit_body_keeps_its_answer(social_graph):
    query = (
        "MATCH (p:Person) CALL (p) { OPTIONAL MATCH (a)-[:KNOWS]->(p) WITH a LIMIT 2 RETURN collect(a) AS ps } "
        "WITH p, size(ps) AS n WITH p, n WHERE p.age = 25 RETURN p.name AS name, n"
    )
    assert social_graph.cypher(query).to_list() == [{"name": "Person_5", "n": 2}]
    assert _call_rows_in(social_graph, query) == 1


@pytest.mark.parametrize(
    "query,expected_rows_in,expected",
    [
        # a conjunct over a CALL output cannot move
        (f"MATCH (p:Person) {CNT} WHERE c > 2 RETURN count(p) AS n", 20, [{"n": 17}]),
        # a LIMIT between the CALL and the filter cannot be crossed
        (
            f"MATCH (p:Person) {CNT} WITH p, c ORDER BY p.age LIMIT 5 WHERE p.age > 22 "
            "RETURN p.name AS n, c ORDER BY n",
            20,
            [{"n": "Person_3", "c": 3}, {"n": "Person_4", "c": 3}, {"n": "Person_5", "c": 3}],
        ),
        # a body that can return zero rows keeps the CALL where it was
        (
            "MATCH (p:Person) CALL (p) { MATCH (p)-[:KNOWS]->(q) WHERE q.age > 38 RETURN q.age AS a } "
            "WITH p, a WHERE p.age > 38 RETURN p.name AS n, a ORDER BY n",
            20,
            [{"n": "Person_19", "a": 40}],
        ),
        # a volatile body keeps the CALL where it was
        (
            "MATCH (p:Person) CALL (p) { MATCH (p)-[:KNOWS]->(q) WHERE rand() < 2.0 RETURN count(q) AS c } "
            "WITH p, c WHERE p.age > 38 RETURN p.name AS n, c ORDER BY n",
            20,
            [{"n": "Person_19", "c": 1}, {"n": "Person_20", "c": 0}],
        ),
    ],
)
def test_a_bail_shape_leaves_the_call_over_every_row(social_graph, query, expected_rows_in, expected):
    assert social_graph.cypher(query).to_list() == expected
    assert _call_rows_in(social_graph, query) == expected_rows_in


@pytest.mark.parametrize(
    "tail,expected,rows_in",
    [
        # sort keys read only the pre-CALL `p`: the CALL runs for the cut alone
        (
            "RETURN p.name AS n, c ORDER BY p.name DESC LIMIT 4",
            [("Person_9", 3), ("Person_8", 3), ("Person_7", 3), ("Person_6", 3)],
            4,
        ),
        (
            "WITH p, c WHERE p.age > 25 RETURN p.name AS n, c ORDER BY p.name LIMIT 3",
            [("Person_10", 3), ("Person_11", 3), ("Person_12", 3)],
            3,
        ),
        # a SKIP, or a sort key over a CALL output, keeps the CALL over every row
        (
            "RETURN p.name AS n, c ORDER BY p.name SKIP 2 LIMIT 3",
            [("Person_11", 3), ("Person_12", 3), ("Person_13", 3)],
            20,
        ),
        ("RETURN p.name AS n, c ORDER BY c, p.name LIMIT 2", [("Person_20", 0), ("Person_19", 1)], 20),
    ],
)
def test_top_k_runs_the_call_for_the_cut_only(social_graph, tail, expected, rows_in):
    query = f"MATCH (p:Person) {CNT} {tail}"
    rows = [tuple(r.values()) for r in social_graph.cypher(query).to_list()]
    assert rows == expected
    off = [tuple(r.values()) for r in social_graph.cypher(query, disabled_passes=[PASS]).to_list()]
    assert off == expected
    assert _call_rows_in(social_graph, query) == rows_in
    assert _call_rows_in(social_graph, query, disabled_passes=[PASS]) == 20
