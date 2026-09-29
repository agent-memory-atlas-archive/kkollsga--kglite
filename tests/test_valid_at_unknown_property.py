"""`valid_at` / `valid_during` refuse a bound property the type does not have.

A null bound is open, so a misspelled property name (`'validfrom'`) read null on
every row and answered the query as if that side were unbounded — silently,
with a plausible count. A property that no element of the type has is now an
error naming it, as `db.temporal.declare` already refused one; a property the
type has but one row leaves null stays open.
"""

from __future__ import annotations

import pandas as pd
import pytest

import kglite


@pytest.fixture
def graph() -> kglite.KnowledgeGraph:
    g = kglite.KnowledgeGraph()
    g.cypher(
        "CREATE (:M {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}),"
        " (:M {id: 2, vf: date('2012-01-01')}), (:M {id: 3})"
    ).to_list()
    return g


@pytest.mark.parametrize("disable_optimizer", [False, True])
def test_a_null_bound_on_a_known_property_stays_open(graph, disable_optimizer) -> None:
    rows = graph.cypher(
        "MATCH (m:M) WHERE valid_at(m, date('2005-01-01'), 'vf', 'vt') RETURN m.id AS id ORDER BY id",
        disable_optimizer=disable_optimizer,
    ).to_list()
    assert rows == [{"id": 1}, {"id": 3}]


@pytest.mark.parametrize("disable_optimizer", [False, True])
@pytest.mark.parametrize(
    ("query", "missing"),
    [
        ("MATCH (m:M) WHERE valid_at(m, date('2005-01-01'), 'vfx', 'vt') RETURN count(*) AS c", "vfx"),
        ("MATCH (m:M) WHERE valid_at(m, date('2005-01-01'), 'vf', 'vtx') RETURN m.id AS id", "vtx"),
        (
            "MATCH (m:M) WHERE valid_during(m, date('2005-01-01'), date('2006-01-01'), 'validfrom', 'vt')"
            " RETURN count(*) AS c",
            "validfrom",
        ),
    ],
)
def test_a_property_no_node_has_is_refused(graph, query, missing, disable_optimizer) -> None:
    with pytest.raises(kglite.CypherExecutionError, match=f"property '{missing}' does not exist on node type 'M'"):
        graph.cypher(query, disable_optimizer=disable_optimizer).to_list()


def test_relationships_are_checked_and_declared_bounds_nobody_set_stay_open() -> None:
    g = kglite.KnowledgeGraph()
    g.add_nodes(pd.DataFrame({"id": ["a", "b"]}), "M", "id")
    # No period has ended yet: the valid_to column is all null, so no stored
    # relationship holds it — the declaration is what makes it known.
    g.add_relationships(
        pd.DataFrame({"s": ["a"], "t": ["b"], "vf": ["2000-01-01"], "vt": [None]}),
        "R",
        "M",
        "s",
        "M",
        "t",
        column_types={"vf": "validFrom", "vt": "validTo"},
    )
    ok = g.cypher("MATCH ()-[r:R]->() WHERE valid_at(r, date('2005-01-01'), 'vf', 'vt') RETURN count(*) AS c")
    assert ok.to_list() == [{"c": 1}]
    with pytest.raises(kglite.CypherExecutionError, match="property 'vtx' does not exist on relationship type 'R'"):
        g.cypher("MATCH ()-[r:R]->() WHERE valid_at(r, date('2005-01-01'), 'vf', 'vtx') RETURN count(*) AS c").to_list()


def _set_bound_graph(storage: str, tmp_path) -> kglite.KnowledgeGraph:
    """`H` created with only `rf`; `rt` exists solely through a later `SET`."""
    if storage == "disk":
        g = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "set-bound-disk"))
    elif storage == "mapped":
        g = kglite.KnowledgeGraph(storage="mapped")
    else:
        g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:A {id: 1}), (:B {id: 1}), (:B {id: 2}), (:B {id: 3})")
    g.cypher("MATCH (a:A), (b:B {id: 1}) CREATE (a)-[:H {rf: date('2000-01-01')}]->(b)")
    g.cypher("MATCH ()-[h:H]->() SET h.rt = date('2001-01-01')")
    return g


_BOUND_QUERY = "MATCH ()-[h:H]->() WHERE valid_at(h, date('2000-06-01'), 'rf', 'rt') RETURN count(*) AS n"


@pytest.mark.parametrize("storage", ["memory", "mapped", "disk"])
@pytest.mark.parametrize(
    "writer",
    [
        "MATCH (a:A), (b:B {id: 2}) CREATE (a)-[:H {rf: date('2002-01-01')}]->(b)",
        "MATCH (a:A), (b:B {id: 2}) MERGE (a)-[:H {rf: date('2002-01-01')}]->(b)",
    ],
    ids=["create", "merge"],
)
def test_a_relationship_bound_only_a_set_wrote_is_known(storage, writer, tmp_path) -> None:
    # `SET r.p` records `p` on the relationship type, so a row that leaves it
    # null reads open instead of the whole type being told it has no `rt`.
    g = _set_bound_graph(storage, tmp_path)
    g.cypher(writer)
    assert g.cypher(_BOUND_QUERY).to_list() == [{"n": 1}]


@pytest.mark.parametrize("storage", ["memory", "mapped", "disk"])
def test_a_set_that_reaches_some_relationships_records_the_name(storage, tmp_path) -> None:
    # No write after the SET: its rows carry `rt`, the others leave it null.
    g = _set_bound_graph(storage, tmp_path)
    g.cypher("MATCH ()-[h:H]->() REMOVE h.rt")
    g.cypher("MATCH (a:A), (b:B {id: 2}) CREATE (a)-[:H {rf: date('2002-01-01')}]->(b)")
    g.cypher("MATCH ()-[h:H]->(:B {id: 2}) SET h.rt2 = date('2003-01-01')")
    query = _BOUND_QUERY.replace("'rt'", "'rt2'")
    assert g.cypher(query).to_list() == [{"n": 1}]


def test_a_set_relationship_property_survives_save_and_reaches_describe(tmp_path) -> None:
    g = _set_bound_graph("memory", tmp_path)
    g.cypher("MATCH (a:A), (b:B {id: 2}) CREATE (a)-[:H {rf: date('2002-01-01')}]->(b)")
    g.cypher("MATCH (a:A), (b:B {id: 3}) CREATE (a)-[:K]->(b)")
    g.cypher("MATCH ()-[k:K]->() SET k.w = 1.5")
    assert '<prop name="w"' in g.describe(connections=["K"])

    path = str(tmp_path / "set-bound.kgl")
    g.save(path)
    loaded = kglite.load(path)
    assert loaded.cypher(_BOUND_QUERY).to_list() == [{"n": 1}]
    assert '<prop name="w"' in loaded.describe(connections=["K"])
