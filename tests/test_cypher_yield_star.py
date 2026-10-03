"""``CALL ... YIELD *`` yields every column the procedure registry declares.

Red proof: before the form was accepted every statement here failed with
"Expected column name in YIELD, got `*`".
"""

import pytest

import kglite


@pytest.fixture
def graph():
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:Alpha {id: 1, title: 'a', vf: '2000-01-01', vt: '2010-01-01'}), (:Beta {id: 2, title: 'b'})")
    return g


def bare_columns(g, call):
    return list(g.cypher(call).columns)


@pytest.mark.parametrize(
    "call",
    [
        "db.labels()",
        "db.relationshipTypes()",
        "db.graph_stats()",
        "db.temporal.declarations()",
        "db.indexes()",
    ],
)
def test_yield_star_columns_equal_the_bare_call(graph, call):
    """The registry backs both forms, so they must agree column for column."""
    assert list(graph.cypher(f"CALL {call} YIELD * RETURN *").columns) == bare_columns(graph, f"CALL {call}")


def test_operator_case_on_a_declared_graph(graph):
    graph.cypher("CALL db.temporal.declare({node: 'Alpha', from: 'vf', to: 'vt', convention: 'half_open'})").to_list()
    res = graph.cypher("CALL db.temporal.declarations() YIELD * RETURN *")
    rows = res.to_list()
    assert [r["name"] for r in rows] == ["Alpha"]
    assert list(res.columns) == bare_columns(graph, "CALL db.temporal.declarations()")


def test_yield_star_then_where_and_with(graph):
    rows = graph.cypher("CALL db.labels() YIELD * WHERE label = 'Beta' RETURN label").to_list()
    assert rows == [{"label": "Beta"}]
    rows = graph.cypher("CALL db.labels() YIELD * WITH label ORDER BY label RETURN collect(label) AS ls").to_list()
    assert rows == [{"ls": ["Alpha", "Beta"]}]


def test_yield_star_mid_pipeline_binds_every_column(graph):
    rows = graph.cypher("MATCH (n:Alpha) CALL db.labels() YIELD * RETURN n.title AS t, label ORDER BY label").to_list()
    assert rows == [{"t": "a", "label": "Alpha"}, {"t": "a", "label": "Beta"}]


def test_standalone_bare_call_is_unchanged(graph):
    assert graph.cypher("CALL db.labels()").to_list() == [{"label": "Alpha"}, {"label": "Beta"}] or sorted(
        r["label"] for r in graph.cypher("CALL db.labels()").to_list()
    ) == ["Alpha", "Beta"]
    with pytest.raises(Exception, match="CALL requires a YIELD clause"):
        graph.cypher("CALL db.labels() RETURN *")


@pytest.mark.parametrize(
    "query",
    ["CALL db.labels() YIELD *, label", "CALL db.labels() YIELD label, *", "CALL db.labels() YIELD * AS x"],
)
def test_yield_star_cannot_be_mixed(graph, query):
    with pytest.raises(Exception, match=r"YIELD \* cannot be combined"):
        graph.cypher(query)


def test_unknown_procedure_keeps_its_error(graph):
    with pytest.raises(Exception, match="Unknown procedure 'no.such.proc'"):
        graph.cypher("CALL no.such.proc() YIELD *")
