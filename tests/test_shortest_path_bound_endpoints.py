"""shortestPath / allShortestPaths endpoints that an earlier clause bound.

Before the fix a bound endpoint was only honoured when both endpoints were
ordinary pattern bindings. One bound endpoint, or a node value from WITH /
UNWIND / startNode(r) / a parameter, fell back to resolving both endpoint
patterns from scratch: all-pairs paths, the bound variable re-bound, the input
rows dropped. The optimised and unoptimised plans gave the same wrong answer,
so the differential corpus could not see it. These are absolute answers, run
in every storage mode.
"""

import pandas as pd
import pytest

from kglite import KnowledgeGraph


@pytest.fixture(params=["default", "mapped", "disk"])
def graph(request, tmp_path):
    options = {"storage": request.param}
    if request.param == "disk":
        options["path"] = str(tmp_path / "graph")
    g = KnowledgeGraph(**options)
    # a -R-> b -R-> c -R-> d; a, b and c are :P, d is :Q.
    g.add_nodes(pd.DataFrame({"id": [1, 2, 3], "name": ["a", "b", "c"]}), "P", "id", "name")
    g.add_nodes(pd.DataFrame({"id": [4], "name": ["d"]}), "Q", "id", "name")
    g.add_connections(pd.DataFrame({"s": [1, 2], "t": [2, 3]}), "R", "P", "s", "P", "t")
    g.add_connections(pd.DataFrame({"s": [3], "t": [4]}), "R", "P", "s", "Q", "t")
    return g


def rows(graph, query, **params):
    return graph.cypher(query, params=params or None).to_list()


@pytest.mark.parametrize("fn", ["shortestPath", "allShortestPaths"])
def test_one_bound_endpoint_anchors_and_is_not_rebound(graph, fn):
    assert rows(
        graph,
        f"MATCH (a:P {{name:'a'}}) MATCH p = {fn}((a)-[:R*..5]-(b:Q)) "
        "RETURN a.name AS a, [n IN nodes(p) | n.name] AS path",
    ) == [{"a": "a", "path": ["a", "b", "c", "d"]}]


def test_bound_endpoint_on_the_right_hand_side(graph):
    assert rows(
        graph,
        "MATCH (b:P {name:'b'}) MATCH p = shortestPath((x:P)-[:R*..5]-(b)) "
        "RETURN x.name AS x, length(p) AS n ORDER BY x",
    ) == [{"x": "a", "n": 1}, {"x": "c", "n": 1}]


@pytest.mark.parametrize(
    ("pattern", "expected"),
    [
        ("(a)-[:R*]->(q:Q)", [{"n": 2}]),
        ("(a)<-[:R*]-(q:P)", [{"n": 1}]),
        ("(a)-[:R*]-(q:P)", [{"n": 1}, {"n": 1}]),
    ],
)
def test_bound_endpoint_respects_direction(graph, pattern, expected):
    assert (
        rows(
            graph,
            f"MATCH (a:P {{name:'b'}}) MATCH p = shortestPath({pattern}) RETURN length(p) AS n",
        )
        == expected
    )


def test_bound_endpoint_with_a_labelled_and_propertied_free_endpoint(graph):
    assert rows(
        graph,
        "MATCH (a:P {name:'a'}) MATCH p = shortestPath((a)-[:R*]-(x:P {name:'c'})) RETURN x.name AS x, length(p) AS n",
    ) == [{"x": "c", "n": 2}]


def test_bound_endpoint_must_satisfy_the_endpoint_label(graph):
    assert (
        rows(
            graph,
            "MATCH (a:P {name:'a'}) MATCH p = shortestPath((a:Q)-[:R*]-(x:P)) RETURN a.name AS a",
        )
        == []
    )


def test_both_endpoints_bound_pair_per_row(graph):
    assert rows(
        graph,
        "MATCH (a:P) MATCH (q:Q) MATCH p = shortestPath((a)-[:R*]-(q)) RETURN a.name AS a, length(p) AS n ORDER BY a",
    ) == [{"a": "a", "n": 3}, {"a": "b", "n": 2}, {"a": "c", "n": 1}]


@pytest.mark.parametrize(
    "prefix",
    [
        "MATCH (n:P) WITH n ORDER BY n.name WITH collect(n) AS ns WITH ns[0] AS a, ns[2] AS b ",
        "MATCH (x:P {name:'a'}), (y:P {name:'c'}) WITH x AS a, y AS b ",
    ],
    ids=["collect-index", "with-alias"],
)
def test_two_node_values_anchor(graph, prefix):
    assert rows(
        graph,
        prefix + "MATCH p = shortestPath((a)-[:R*..5]-(b)) RETURN [n IN nodes(p) | n.name] AS path",
    ) == [{"path": ["a", "b", "c"]}]


def test_start_node_value_anchors(graph):
    assert rows(
        graph,
        "MATCH (:P {name:'a'})-[r:R]->() WITH startNode(r) AS a MATCH (b:Q) "
        "MATCH p = shortestPath((a)-[:R*..5]-(b)) RETURN [n IN nodes(p) | n.name] AS path",
    ) == [{"path": ["a", "b", "c", "d"]}]


def test_unwound_node_values_anchor_per_row(graph):
    assert rows(
        graph,
        "MATCH (n:P) WITH collect(n) AS ns UNWIND ns AS a "
        "MATCH p = shortestPath((a)-[:R*..5]-(q:Q)) RETURN a.name AS a, length(p) AS n ORDER BY a",
    ) == [{"a": "a", "n": 3}, {"a": "b", "n": 2}, {"a": "c", "n": 1}]


def test_free_endpoint_property_from_the_row(graph):
    assert rows(
        graph,
        "UNWIND ['a', 'b'] AS nm MATCH p = shortestPath((x:P {name: nm})-[:R*]-(q:Q)) "
        "RETURN nm, length(p) AS n ORDER BY nm",
    ) == [{"nm": "a", "n": 3}, {"nm": "b", "n": 2}]


def test_input_rows_survive_an_unbound_shortest_path(graph):
    assert rows(
        graph,
        "UNWIND [1, 2] AS x MATCH p = shortestPath((a:P {name:'a'})-[:R*]-(q:Q)) RETURN x, length(p) AS n ORDER BY x",
    ) == [{"x": 1, "n": 3}, {"x": 2, "n": 3}]


@pytest.mark.parametrize(
    "prefix",
    ["OPTIONAL MATCH (a:P {name:'zzz'}) ", "WITH null AS a "],
    ids=["optional-miss", "null"],
)
def test_null_endpoint_yields_no_rows(graph, prefix):
    assert rows(graph, prefix + "MATCH p = shortestPath((a)-[:R*]-(q:Q)) RETURN length(p) AS n") == []


def test_relationship_value_endpoint_is_an_error(graph):
    with pytest.raises(Exception, match="holds a relationship"):
        rows(
            graph,
            "MATCH ()-[r:R]->() WITH collect(r)[0] AS a MATCH p = shortestPath((a)-[:R*]-(q:Q)) RETURN length(p) AS n",
        )


def test_where_after_an_opening_shortest_path_filters(graph):
    assert rows(
        graph,
        "MATCH p = shortestPath((a:P)-[:R*]-(q:Q)) WHERE length(p) = 1 RETURN a.name AS a",
    ) == [{"a": "c"}]


# Hop bounds and the one-relationship shape. The bounds used to be ignored
# (`*..2` returned a 3-hop path, `*2..5` a 1-hop one) and a chain of
# relationships searched along its first relationship only.


@pytest.fixture(params=["default", "mapped", "disk"])
def long_chain(request, tmp_path):
    """C 1 -N-> ... -N-> C 13: twelve hops, past the 10-hop var-length MATCH cap."""
    options = {"storage": request.param}
    if request.param == "disk":
        options["path"] = str(tmp_path / "graph")
    g = KnowledgeGraph(**options)
    g.add_nodes(pd.DataFrame({"id": list(range(1, 14))}), "C", "id", "id")
    g.add_connections(pd.DataFrame({"s": list(range(1, 13)), "t": list(range(2, 14))}), "N", "C", "s", "C", "t")
    return g


@pytest.mark.parametrize("fn", ["shortestPath", "allShortestPaths"])
@pytest.mark.parametrize(
    ("rel", "expected"),
    [("[:R*..2]", []), ("[:R*1..3]", [{"n": 3}]), ("[:R]", []), ("[:R*0]", [])],
)
def test_a_written_maximum_bounds_the_search(graph, fn, rel, expected):
    assert rows(graph, f"MATCH p = {fn}((a:P {{name:'a'}})-{rel}-(q:Q)) RETURN length(p) AS n") == expected


@pytest.mark.parametrize(
    "prefix",
    [
        "MATCH (a:P {name:'a'}) OPTIONAL MATCH ",
        "MATCH (a:P {name:'a'}) CALL { WITH a MATCH ",
    ],
    ids=["optional", "call-subquery"],
)
def test_a_written_maximum_binds_in_optional_and_subquery_forms(graph, prefix):
    tail = " RETURN length(p) AS n } RETURN n" if "CALL" in prefix else " RETURN length(p) AS n"
    got = rows(graph, prefix + "p = shortestPath((a)-[:R*..2]-(q:Q))" + tail)
    assert got == ([{"n": None}] if "OPTIONAL" in prefix else [])


@pytest.mark.parametrize(
    "pattern",
    [
        "shortestPath((a:C {id:1})-[:N*]-(b:C {id:13}))",
        "shortestPath((a:C {id:1})-[:N*1..]->(b:C {id:13}))",
        "allShortestPaths((a:C {id:1})-[*]-(b:C {id:13}))",
    ],
)
def test_an_open_maximum_is_unbounded_for_shortest_path(long_chain, pattern):
    assert rows(long_chain, f"MATCH p = {pattern} RETURN length(p) AS n") == [{"n": 12}]


def test_the_var_length_match_cap_is_unchanged(long_chain):
    assert rows(long_chain, "MATCH p = (a:C {id:1})-[:N*]->(b:C {id:13}) RETURN length(p) AS n") == []
    assert rows(long_chain, "MATCH p = shortestPath((a:C {id:1})-[:N*..11]-(b:C {id:13})) RETURN length(p) AS n") == []


@pytest.mark.parametrize(
    ("pattern", "written"),
    [
        ("shortestPath((a:P)-[:R*2..5]-(q:Q))", "`*2..5`"),
        ("shortestPath((a:P)-[:R*2..]-(q:Q))", "`*2..`"),
        ("allShortestPaths((a:P)-[:R*3]->(q:Q))", "`*3`"),
    ],
)
def test_a_minimum_above_one_is_refused(graph, pattern, written):
    fn = pattern.split("(")[0]
    with pytest.raises(Exception, match=rf"{fn}\(\) does not support a minimum length other than 0 or 1.*{written}"):
        rows(graph, f"MATCH p = {pattern} RETURN length(p) AS n")


@pytest.mark.parametrize(
    "query",
    [
        "MATCH p = shortestPath((a:P {name:'a'})-[:R]->(m:Q)-[:R*]->(b:Q)) RETURN m",
        "MATCH (a:P {name:'a'}) OPTIONAL MATCH p = allShortestPaths((a)-[*]-(m)-[*]-(b:Q)) RETURN m",
    ],
)
def test_a_pattern_with_more_than_one_relationship_is_refused(graph, query):
    with pytest.raises(Exception, match="exactly one relationship.*2 relationships across 3 nodes"):
        rows(graph, query)
