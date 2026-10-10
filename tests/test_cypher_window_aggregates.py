"""Aggregate window functions: ``count/sum/avg/min/max ... OVER ([PARTITION BY ...])``.

Golden values are computed by hand; the differential corpus
(``test_cypher_differential.py``) covers the optimizer-on/off equivalence.
"""

import pytest

import kglite


@pytest.fixture
def graph() -> kglite.KnowledgeGraph:
    g = kglite.KnowledgeGraph()
    g.cypher(
        "CREATE (:Doc {name: 'a', lex: 2.0, grp: 'x'}), (:Doc {name: 'b', lex: 8.0, grp: 'x'}),"
        " (:Doc {name: 'c', lex: 4.0, grp: 'y'}), (:Doc {name: 'd', grp: 'y'})"
    )
    return g


@pytest.mark.parametrize("disabled", [False, True])
def test_whole_result_aggregates(graph, disabled):
    rows = graph.cypher(
        "MATCH (d:Doc) RETURN d.name AS name, max(d.lex) OVER () AS hi, min(d.lex) OVER () AS lo,"
        " sum(d.lex) OVER () AS total, count(d.lex) OVER () AS scored, count(*) OVER () AS n ORDER BY name",
        disable_optimizer=disabled,
    ).to_list()
    assert [r["name"] for r in rows] == ["a", "b", "c", "d"]
    for r in rows:
        assert (r["hi"], r["lo"], r["total"], r["scored"], r["n"]) == (8.0, 2.0, 14.0, 3, 4)


@pytest.mark.parametrize("disabled", [False, True])
def test_partitioned_aggregates(graph, disabled):
    rows = graph.cypher(
        "MATCH (d:Doc) RETURN d.name AS name, avg(d.lex) OVER (PARTITION BY d.grp) AS mean,"
        " max(d.lex) OVER (PARTITION BY d.grp) AS hi, count(*) OVER (PARTITION BY d.grp) AS n ORDER BY name",
        disable_optimizer=disabled,
    ).to_list()
    assert rows == [
        {"name": "a", "mean": 5.0, "hi": 8.0, "n": 2},
        {"name": "b", "mean": 5.0, "hi": 8.0, "n": 2},
        {"name": "c", "mean": 4.0, "hi": 4.0, "n": 2},
        {"name": "d", "mean": 4.0, "hi": 4.0, "n": 2},
    ]


def test_normalising_a_score_by_its_maximum_through_with(graph):
    rows = graph.cypher(
        "MATCH (d:Doc) WHERE d.lex IS NOT NULL WITH d.name AS name, d.lex AS lex, max(d.lex) OVER () AS top"
        " RETURN name, lex / top AS norm ORDER BY name"
    ).to_list()
    assert rows == [
        {"name": "a", "norm": 0.25},
        {"name": "b", "norm": 1.0},
        {"name": "c", "norm": 0.5},
    ]


def test_window_arguments_read_variables_the_return_does_not_project():
    g = kglite.KnowledgeGraph()
    rows = g.cypher(
        "UNWIND [3, 1, 2] AS x RETURN x * 10 AS y, max(x) OVER () AS hi,"
        " row_number() OVER (ORDER BY x) AS rn ORDER BY y"
    ).to_list()
    assert rows == [{"y": 10, "hi": 3, "rn": 1}, {"y": 20, "hi": 3, "rn": 2}, {"y": 30, "hi": 3, "rn": 3}]


def test_order_by_inside_an_aggregate_window_is_refused(graph):
    with pytest.raises(Exception, match="ORDER BY inside OVER is not supported for the aggregate max"):
        graph.cypher("MATCH (d:Doc) RETURN max(d.lex) OVER (ORDER BY d.name) AS running")


def test_an_unsupported_aggregate_names_the_supported_ones(graph):
    with pytest.raises(Exception, match=r"count, sum, avg, min and max, not 'collect'"):
        graph.cypher("MATCH (d:Doc) RETURN collect(d.name) OVER () AS names")
