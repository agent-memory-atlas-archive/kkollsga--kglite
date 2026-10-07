"""Pattern comprehension, `COUNT {}` and `EXISTS {}` semantics.

Independently designed cases over graphs written for this file. The contract:

- `[(n)-->(m) | expr]` yields one list element per match of the pattern from
  the bound node, with `[]` when nothing matches.
- A path variable (`[p = ... | p]`) binds each matched path.
- A pattern anchored on a null node has no matches: the comprehension is `[]`,
  `COUNT {}` is 0 and `EXISTS {}` is false.
"""

from collections import Counter

import kglite


def graph(*statements: str) -> kglite.KnowledgeGraph:
    g = kglite.KnowledgeGraph()
    for statement in statements:
        g.cypher(statement)
    return g


def rows(g: kglite.KnowledgeGraph, query: str) -> list[dict]:
    return g.cypher(query).to_list()


def path_shape(path: dict) -> tuple:
    """Flatten a path to (labels, type, labels, type, labels, ...)."""
    nodes = [tuple(node["labels"]) for node in path["nodes"]]
    out = [nodes[0]]
    for rel, node in zip(path["relationships"], nodes[1:]):
        out += [rel["type"], node]
    return tuple(out)


CHAIN = "CREATE (:Src {v: 1})-[:LINK]->(:Mid {v: 2})-[:LINK]->(:Fin {v: 3})"


def test_comprehension_collects_one_element_per_outgoing_match():
    g = graph("CREATE (h:Hub {v: 0}), (h)-[:E]->(:Leaf {v: 1}), (h)-[:E]->(:Leaf {v: 2}), (:Lone {v: 9})")
    result = {r["v"]: sorted(r["out"]) for r in rows(g, "MATCH (n) RETURN n.v AS v, [(n)-[:E]->(m) | m.v] AS out")}
    assert result == {0: [1, 2], 1: [], 2: [], 9: []}


def test_comprehension_binds_whole_paths():
    g = graph(CHAIN)
    result = rows(g, "MATCH (n) RETURN labels(n)[0] AS l, [p = (n)-->() | p] AS ps")
    shapes = Counter((r["l"], tuple(path_shape(p) for p in r["ps"])) for r in result)
    assert shapes == Counter(
        [
            ("Src", ((("Src",), "LINK", ("Mid",)),)),
            ("Mid", ((("Mid",), "LINK", ("Fin",)),)),
            ("Fin", ()),
        ]
    )


def test_comprehension_label_filter_in_pattern():
    g = graph("CREATE (a:Root), (a)-[:E]->(:Red {v: 1}), (a)-[:E]->(:Blue {v: 2})")
    assert rows(g, "MATCH (n:Root) RETURN [(n)-->(m:Blue) | m.v] AS out") == [{"out": [2]}]


def test_comprehension_between_two_bound_nodes():
    g = graph("CREATE (a:P {v: 1}), (b:Q {v: 2}), (c:Q {v: 3}), (a)-[:E]->(b)")
    result = rows(g, "MATCH (a:P), (b:Q) RETURN b.v AS v, size([(a)-->(b) | 1]) AS n")
    assert sorted((r["v"], r["n"]) for r in result) == [(2, 1), (3, 0)]


def test_comprehension_projects_relationship_properties():
    g = graph("CREATE (a:N {v: 1}), (b:N {v: 2}), (c:N {v: 3}), (a)-[:E {w: 'x'}]->(b), (b)-[:E]->(c)")
    result = {r["v"]: r["ws"] for r in rows(g, "MATCH (n:N) RETURN n.v AS v, [(n)-[r:E]->() | r.w] AS ws")}
    assert result == {1: ["x"], 2: [None], 3: []}


def test_aggregate_over_comprehension_counts_rows_not_elements():
    g = graph("CREATE (a:K), (:K), (:K), (a)-[:E]->(:Z)")
    assert rows(g, "MATCH (n:K) RETURN count([(n)-[:E]->() | 1]) AS c") == [{"c": 3}]


def test_comprehension_nested_in_list_comprehension():
    g = graph(
        "CREATE (a:Top {v: 1}), (m:Mid), (m)-[:E]->(:Y), (m)-[:E]->(:Y), (a)-[:E]->(m)",
    )
    result = rows(g, "MATCH p = (n:Top)-->() RETURN [x IN nodes(p) | size([(x)-->(:Y) | 1])] AS sizes")
    assert result == [{"sizes": [0, 2]}]


def test_comprehension_after_with_alongside_aggregate():
    g = graph(CHAIN)
    result = rows(g, "MATCH (n)-->(m) WITH [(n)-->(k) | k.v] AS ks, count(m) AS c RETURN ks, c")
    assert sorted((tuple(r["ks"]), r["c"]) for r in result) == [((2,), 1), ((3,), 1)]


def test_variable_length_comprehension_after_with():
    g = graph(CHAIN)
    result = rows(g, "MATCH (a:Src), (b:Fin) WITH [(a)-[*]->(b) | 1] AS hits, count(a) AS c RETURN size(hits) AS n, c")
    assert result == [{"n": 1, "c": 1}]


def test_null_anchor_matches_nothing_in_clauses():
    g = graph(CHAIN)
    assert rows(g, "OPTIONAL MATCH (a:Missing) WITH a MATCH (a)-->(b) RETURN b") == []
    assert rows(g, "OPTIONAL MATCH (a:Missing) WITH a OPTIONAL MATCH (a)-->(b) RETURN b") == [{"b": None}]


def test_null_anchor_in_comprehension_count_and_exists():
    g = graph(CHAIN)
    result = rows(
        g,
        "OPTIONAL MATCH (a:Missing) RETURN [(a)-->(b) | b] AS out, [p = (a)<--() | p] AS inc, "
        "COUNT { (a)-->() } AS c, EXISTS { (a)-->() } AS e",
    )
    assert result == [{"out": [], "inc": [], "c": 0, "e": False}]


def test_null_anchor_exists_filter_drops_the_row():
    g = graph(CHAIN)
    result = rows(g, "OPTIONAL MATCH (a:Missing) WITH a WHERE EXISTS { (a)-->() } RETURN count(*) AS n")
    assert result == [{"n": 0}]


def test_bound_anchor_count_and_exists_are_positive():
    g = graph(CHAIN)
    result = rows(g, "MATCH (a:Src) RETURN COUNT { (a)-->() } AS c, EXISTS { (a)-->() } AS e")
    assert result == [{"c": 1, "e": True}]
