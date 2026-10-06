"""Several variable-length expansions in one query, with absolute expected values.

A second `-[*..]->` after another one used to find nothing beyond its
zero-hop row: both unnamed segments left the same internal binding name, and
the join took the second's path for a conflict with the first's.  The graph is
a two-parent family tree, so the interesting answer is the common ancestor:

    A -FATHER-> F -FATHER-> G
    B -MOTHER-> M -FATHER-> G
"""

import pytest

import kglite

FAMILY = (
    "CREATE (a:P {title:'A'}),(b:P {title:'B'}),(f:P {title:'F'}),(m:P {title:'M'}),(g:P {title:'G'}), "
    "(a)-[:FATHER]->(f),(b)-[:MOTHER]->(m),(f)-[:FATHER]->(g),(m)-[:FATHER]->(g)"
)


@pytest.fixture(params=["memory", "mapped", "disk"])
def family(request, tmp_path):
    mode = request.param
    if mode == "memory":
        graph = kglite.KnowledgeGraph()
    elif mode == "mapped":
        graph = kglite.KnowledgeGraph(storage="mapped")
    else:
        graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "family-disk"))
    graph.cypher(FAMILY)
    return graph


def _rows(graph, query):
    """Every column of every row as a sorted list of tuples, run with and without the optimizer."""
    out = []
    for kwargs in ({}, {"disable_optimizer": True}):
        rows = graph.cypher(query, **kwargs).to_list()
        out.append(sorted(tuple(row.values()) for row in rows))
    assert out[0] == out[1], "optimizer changed the answer"
    return out[0]


@pytest.mark.parametrize("low", [0, 1])
def test_common_ancestor_with_named_paths(family, low):
    query = (
        "MATCH (x {title:'A'}) MATCH (y {title:'B'}) "
        f"MATCH px=(x)-[:FATHER|MOTHER*{low}..20]->(c) MATCH py=(y)-[:FATHER|MOTHER*{low}..20]->(c) "
        "RETURN c.title"
    )
    assert _rows(family, query) == [("G",)]


def test_common_ancestor_without_path_variables(family):
    query = (
        "MATCH (x {title:'A'}) MATCH (y {title:'B'}) "
        "MATCH (x)-[:FATHER|MOTHER*1..20]->(c) MATCH (y)-[:FATHER|MOTHER*1..20]->(c) RETURN c.title"
    )
    assert _rows(family, query) == [("G",)]


def test_common_ancestor_in_one_match_clause(family):
    query = (
        "MATCH (x {title:'A'})-[:FATHER|MOTHER*1..20]->(c), (y {title:'B'})-[:FATHER|MOTHER*1..20]->(c) RETURN c.title"
    )
    assert _rows(family, query) == [("G",)]


def test_independent_expansions_cross_join_with_zero_hops(family):
    query = "MATCH (x {title:'A'})-[*0..20]->(c) MATCH (y {title:'B'})-[*0..20]->(c2) RETURN c.title, c2.title"
    assert _rows(family, query) == [(c, c2) for c in ("A", "F", "G") for c2 in ("B", "G", "M")]


@pytest.mark.parametrize("bound", ["1..20", "1..2", "2..2"])
def test_independent_expansions_cross_join(family, bound):
    query = f"MATCH (x {{title:'A'}})-[*{bound}]->(c) MATCH (y {{title:'B'}})-[*{bound}]->(c2) RETURN c.title, c2.title"
    expected = {
        "1..20": [("F", "G"), ("F", "M"), ("G", "G"), ("G", "M")],
        "1..2": [("F", "G"), ("F", "M"), ("G", "G"), ("G", "M")],
        "2..2": [("G", "G")],
    }[bound]
    assert _rows(family, query) == expected


def test_second_expansion_with_unbound_start_counts_every_pair(family):
    query = "MATCH (x {title:'A'})-[*1..20]->(c) MATCH (y)-[*1..20]->(c2) RETURN count(*)"
    # A reaches two nodes; the whole graph has 6 reachable (source, target) pairs.
    assert _rows(family, query) == [(12,)]


def test_expansions_separated_by_with(family):
    query = "MATCH (x {title:'A'})-[*1..20]->(c) WITH c MATCH (y {title:'B'})-[*1..20]->(c2) RETURN c.title, c2.title"
    assert _rows(family, query) == [("F", "G"), ("F", "M"), ("G", "G"), ("G", "M")]
    shared = "MATCH (x {title:'A'})-[*1..20]->(c) WITH c MATCH (y {title:'B'})-[*1..20]->(c) RETURN c.title"
    assert _rows(family, shared) == [("G",)]


def test_expansion_inside_optional_match(family):
    query = "MATCH (x {title:'A'})-[*1..20]->(c) OPTIONAL MATCH (y {title:'B'})-[*1..20]->(c2) RETURN c.title, c2.title"
    assert _rows(family, query) == [("F", "G"), ("F", "M"), ("G", "G"), ("G", "M")]


def test_expansion_inside_exists_and_count_subqueries(family):
    exists = "MATCH (x {title:'A'})-[*1..20]->(c) WHERE EXISTS { (y {title:'B'})-[*1..20]->(c) } RETURN c.title"
    assert _rows(family, exists) == [("G",)]
    count = "MATCH (x {title:'A'})-[*1..20]->(c) RETURN c.title, COUNT { (y {title:'B'})-[*1..20]->(c) } AS n"
    assert _rows(family, count) == [("F", 0), ("G", 1)]


def test_expansion_inside_call_subquery(family):
    query = (
        "MATCH (x {title:'A'})-[*1..20]->(c) "
        "CALL { MATCH (y {title:'B'})-[*1..20]->(c2) RETURN c2 } RETURN c.title, c2.title"
    )
    assert _rows(family, query) == [("F", "G"), ("F", "M"), ("G", "G"), ("G", "M")]


def test_expansion_inside_pattern_comprehension(family):
    query = "MATCH (x {title:'A'})-[*1..20]->(c) RETURN c.title, [(y {title:'B'})-[*1..20]->(z) | z.title] AS zs"
    rows = family.cypher(query).to_list()
    assert sorted((row["c.title"], sorted(row["zs"])) for row in rows) == [("F", ["G", "M"]), ("G", ["G", "M"])]


def test_shortest_path_after_an_expansion(family):
    query = (
        "MATCH (x {title:'A'})-[*1..20]->(c) MATCH (y {title:'B'}) "
        "MATCH sp=shortestPath((y)-[*1..20]->(c)) RETURN c.title, length(sp)"
    )
    assert _rows(family, query) == [("G", 2)]


def test_named_relationship_list_still_pins_a_later_pattern(family):
    # Re-using `r` means the second segment must walk exactly the first's
    # relationships, which no walk from B can.
    query = "MATCH (x {title:'A'})-[r*1..20]->(c) MATCH (y {title:'B'})-[r*1..20]->(c2) RETURN c.title, c2.title"
    assert _rows(family, query) == []
    same_start = "MATCH (x {title:'A'})-[r*1..2]->(c) MATCH (x)-[r*1..2]->(c2) RETURN c.title, c2.title"
    assert _rows(family, same_start) == [("F", "F"), ("G", "G")]


def test_each_comma_pattern_keeps_its_own_path(family):
    query = (
        "MATCH px=(x {title:'A'})-[*1..20]->(c), (y {title:'B'})-[*1..20]->(c2) RETURN c.title, c2.title, length(px)"
    )
    assert _rows(family, query) == [("F", "G", 1), ("F", "M", 1), ("G", "G", 2), ("G", "M", 2)]


def test_fixed_hop_comma_pattern_keeps_its_own_path(family):
    query = (
        "MATCH px=(a {title:'A'})-[:FATHER]->(f), (b {title:'B'})-[:MOTHER]->(m) "
        "RETURN [n IN nodes(px) | n.title] AS names"
    )
    assert _rows(family, query) == [(["A", "F"],)]


def test_path_through_two_variable_length_segments(family):
    query = (
        "MATCH px=(x {title:'A'})-[*1..2]->(c)-[*1..2]->(d) "
        "RETURN c.title, d.title, length(px), [n IN nodes(px) | n.title] AS names"
    )
    assert _rows(family, query) == [("F", "G", 2, ["A", "F", "G"])]


def test_common_ancestor_with_path_variables_on_comma_patterns(family):
    # The path variable may name any comma-separated pattern, not only the first.
    query = (
        "MATCH (x {title:'A'}),(y {title:'B'}), px=(x)-[:FATHER|MOTHER*0..20]->(c), "
        "py=(y)-[:FATHER|MOTHER*0..20]->(c) RETURN c.title"
    )
    assert _rows(family, query) == [("G",)]


def test_each_comma_part_binds_its_own_path_variable(family):
    query = (
        "MATCH px=(x {title:'A'})-[*1..20]->(c), py=(y {title:'B'})-[*1..20]->(c2) "
        "RETURN c.title, c2.title, length(px), length(py)"
    )
    assert _rows(family, query) == [("F", "G", 1, 2), ("F", "M", 1, 1), ("G", "G", 2, 2), ("G", "M", 2, 1)]


def test_path_variable_on_a_later_fixed_hop_pattern(family):
    query = (
        "MATCH (a {title:'A'}), py=(b {title:'B'})-[:MOTHER]->(m) RETURN a.title, [n IN nodes(py) | n.title] AS names"
    )
    assert _rows(family, query) == [("A", ["B", "M"])]


def test_path_variable_on_an_optional_match_comma_pattern(family):
    query = (
        "MATCH (a {title:'A'}) "
        "OPTIONAL MATCH (a)-[:FATHER]->(f), py=(f)-[:FATHER]->(g) "
        "RETURN f.title, g.title, length(py)"
    )
    assert _rows(family, query) == [("F", "G", 1)]


def test_shortest_path_is_refused_on_a_later_pattern(family):
    with pytest.raises(Exception, match="first pattern"):
        family.cypher("MATCH (c {title:'B'}), p=shortestPath((a {title:'A'})-[*]->(g {title:'G'})) RETURN length(p)")
