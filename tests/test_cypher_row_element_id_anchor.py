"""Per-row `elementId(n) = <row expression>` anchoring.

`UNWIND $rows AS row MATCH (n:L) WHERE elementId(n) = row.id` used to scan the
label once per driving row. The planner now records a per-row anchor that the
executor resolves on each driving row and seeds as a pre-binding, so the match
starts at that node. The anchor only narrows the search; the predicate stays.
These goldens pin the cases an optimizer-vs-naive comparison cannot see: both
paths share the row evaluation, so a wrong slot rule would be wrong in both.
"""

import pytest

from kglite import KnowledgeGraph

SHAPES = [
    "UNWIND $rows AS row MATCH (n:L) WHERE elementId(n) = row.id RETURN row.id AS r, n.k AS k ORDER BY r, k",
    "UNWIND $rows AS row MATCH (n) WHERE elementId(n) = row.id RETURN row.id AS r, n.k AS k ORDER BY r, k",
    "UNWIND $rows AS row MATCH (n:L) WHERE row.id = elementId(n) RETURN row.id AS r, n.k AS k ORDER BY r, k",
    "UNWIND $rows AS row WITH row MATCH (n:L) WHERE elementId(n) = row.id AND n.k >= 0 "
    "RETURN row.id AS r, n.k AS k ORDER BY r, k",
    "UNWIND $rows AS row OPTIONAL MATCH (n:L) WHERE elementId(n) = row.id RETURN row.id AS r, n.k AS k ORDER BY r, k",
    "UNWIND $rows AS row MATCH (n:L) WHERE elementId(n) = row.id OR n.k = 100 "
    "RETURN row.id AS r, n.k AS k ORDER BY r, k",
]


def _graph():
    """Slots 0-4 are :L (k = 0..4), 5-6 are :M, slot 3 is then deleted."""
    kg = KnowledgeGraph()
    kg.cypher("UNWIND range(0, 4) AS i CREATE (:L {id: i + 10, k: i})")
    kg.cypher("UNWIND range(5, 6) AS i CREATE (:M {id: i + 10, k: i})")
    kg.cypher("MATCH (n:L {k: 3}) DELETE n")
    return kg


def _rows(kg, query, rows):
    return kg.cypher(query, params={"rows": [{"id": v} for v in rows]}).to_list()


def _both(kg, query, rows):
    optimized = _rows(kg, query, rows)
    naive = kg.cypher(query, params={"rows": [{"id": v} for v in rows]}, disable_optimizer=True).to_list()
    assert optimized == naive, query
    return optimized


@pytest.mark.parametrize("query", SHAPES)
@pytest.mark.parametrize("count", [3, 70])
def test_row_anchor_agrees_with_the_unanchored_scan(query, count):
    # `count` straddles the 64-row transient-index threshold.
    kg = _graph()
    ids = ["0", "2", "3", "5", "999999", "abc", None, 4, 2.0, -1, "", " 1"]
    rows = (ids * (count // len(ids) + 1))[:count]
    _both(kg, query, rows)


def test_row_anchor_edge_cases_golden():
    kg = _graph()
    q = SHAPES[0]
    # string form kglite emits, and the integer spelling of the same value
    assert _both(kg, q, ["2"]) == [{"r": "2", "k": 2}]
    # integer: elementId is a string, so the predicate rejects it
    assert _both(kg, q, [2]) == []
    # null, non-numeric, empty, padded, float, negative, out of range
    for bad in (None, "abc", "", " 1", "1.0", 1.5, -1, "-1", "999999", "18446744073709551616"):
        assert _both(kg, q, [bad]) == [], bad
    # slot holds a node of another label: the typed pattern rejects it,
    # the untyped one matches it
    assert _both(kg, q, ["5"]) == []
    assert _both(kg, SHAPES[1], ["5"]) == [{"r": "5", "k": 5}]
    # tombstoned slot
    assert _both(kg, q, ["3"]) == []
    # duplicate rows yield duplicate matches
    assert _both(kg, q, ["1", "1"]) == [{"r": "1", "k": 1}, {"r": "1", "k": 1}]
    # OPTIONAL MATCH keeps the driving row with a null
    assert _both(kg, SHAPES[4], ["1", "999999"]) == [{"r": "1", "k": 1}, {"r": "999999", "k": None}]
    # OR must not narrow
    assert _both(kg, SHAPES[5], ["1"]) == [{"r": "1", "k": 1}]


def test_row_anchor_existing_binding_wins():
    kg = _graph()
    q = (
        "MATCH (a:L {k: 0}) UNWIND $rows AS row WITH a, row MATCH (a) "
        "WHERE elementId(a) = row.id RETURN row.id AS r, a.k AS k"
    )
    assert _both(kg, q, ["0", "1"]) == [{"r": "0", "k": 0}]


def test_row_anchor_bare_variable_and_elementid_of_other_node():
    kg = _graph()
    q = "UNWIND $rows AS x MATCH (n:L) WHERE elementId(n) = toString(x) RETURN x, n.k AS k ORDER BY x"
    assert kg.cypher(q, params={"rows": [1, 2, 3, 99]}).to_list() == [{"x": 1, "k": 1}, {"x": 2, "k": 2}]
    q = "MATCH (m:L {k: 4}) MATCH (n) WHERE elementId(n) = elementId(m) RETURN n.k AS k"
    assert _both(kg, q, []) == [{"k": 4}]


def test_row_anchor_set_and_delete_paths():
    kg = _graph()
    rows = ["0", "5", "999999", "3", None, "abc"]
    kg.cypher(
        "UNWIND $rows AS row MATCH (n:L) WHERE elementId(n) = row.id SET n.hit = true",
        params={"rows": [{"id": v} for v in rows]},
    )
    assert kg.cypher("MATCH (n) WHERE n.hit = true RETURN n.k AS k ORDER BY k").to_list() == [{"k": 0}]
    kg.cypher(
        "UNWIND $rows AS row MATCH (n:L) WHERE elementId(n) = row.id DELETE n",
        params={"rows": [{"id": v} for v in rows + ["0"]]},
    )
    assert kg.cypher("MATCH (n:L) RETURN n.k AS k ORDER BY k").to_list() == [{"k": 1}, {"k": 2}, {"k": 4}]
    # untyped delete reaches the :M node at slot 5
    kg.cypher("UNWIND $rows AS row MATCH (n) WHERE elementId(n) = row.id DELETE n", params={"rows": [{"id": "5"}]})
    assert kg.cypher("MATCH (n:M) RETURN n.k AS k").to_list() == [{"k": 6}]


def test_row_anchor_fires_and_is_not_a_scan():
    """Non-vacuity: the plan reports the pass, and a work budget that the
    per-row scan exceeds (rows x nodes) is met by the seek (rows)."""
    kg = KnowledgeGraph()
    kg.cypher("UNWIND range(1, 3000) AS i CREATE (:L {id: i, k: i})")
    rows = [{"id": str(i)} for i in range(0, 300)]
    q = "UNWIND $rows AS row MATCH (n:L) WHERE elementId(n) = row.id RETURN count(n) AS c"
    ops = [r["operation"] for r in kg.cypher("EXPLAIN " + q, params={"rows": rows}).to_list()]
    assert "OptimizerPass anchor_element_id" in ops
    assert kg.cypher(q, params={"rows": rows}, max_work_units=1000).to_list() == [{"c": 300}]
    with pytest.raises(Exception, match="(?i)work|limit|exceed"):
        kg.cypher(q, params={"rows": rows}, max_work_units=1000, disabled_passes=["anchor_element_id"])
    # a non-deterministic value must not be anchored
    nd = "UNWIND $rows AS row MATCH (n:L) WHERE elementId(n) = toString(toInteger(rand() * 3)) RETURN count(n) AS c"
    ops = [r["operation"] for r in kg.cypher("EXPLAIN " + nd, params={"rows": rows}).to_list()]
    assert "OptimizerPass anchor_element_id" not in ops
