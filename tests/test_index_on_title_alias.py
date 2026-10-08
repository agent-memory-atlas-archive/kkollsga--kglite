"""A property index on a name that resolves to the title is read by lookups.

`name` answers a node's title when the node stores no `name`, so an index on
it has to file such a node under its title or it holds a strict subset of what a
scan matches. These tests pin the answers (absolute goldens, indexed against an
unindexed twin) and the reports `create_index` / `CREATE INDEX` owe. That the
lookup *skips the scan* is pinned structurally in the engine's
`matcher_property_index_tests` (`try_index_lookup` answering `Some`), where the
counter lives.
"""

import random

import pandas as pd
import pytest

from kglite import KnowledgeGraph

MODES = ["memory", "mapped", "disk"]


def make_graph(mode, tmp_path):
    if mode == "memory":
        return KnowledgeGraph()
    if mode == "mapped":
        return KnowledgeGraph(storage="mapped")
    return KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))


def ids(graph, query, **params):
    return sorted(row["i"] for row in graph.cypher(query, params=params).to_list())


# Four ways a node comes to answer `name`: a stored name, a title only, both
# (the stored name wins), and neither.
SEED = """
CREATE (:P {id: 1, name: 'Ann'}),
       (:P {id: 2, title: 'Ann'}),
       (:P {id: 3, title: 'Bo', name: 'Cy'}),
       (:P {id: 4}),
       (:P {id: 5, title: 'Dee'})
"""

# value -> ids of the nodes whose `name` resolves to it.
NAME_GOLDEN = {"Ann": [1, 2], "Cy": [3], "Bo": [], "Dee": [5], "absent": []}


@pytest.mark.parametrize("mode", MODES)
def test_name_index_answers_for_nodes_that_store_no_name(mode, tmp_path):
    graph = make_graph(mode, tmp_path)
    graph.cypher(SEED)
    result = graph.cypher("CREATE INDEX FOR (n:P) ON (n.name)")
    assert result.warnings == []
    for value, expected in NAME_GOLDEN.items():
        assert ids(graph, "MATCH (n:P {name: $v}) RETURN n.id AS i", v=value) == expected
        assert ids(graph, "MATCH (n:P) WHERE n.name = $v RETURN n.id AS i", v=value) == expected
    assert ids(graph, "MATCH (n:P) WHERE n.name IN ['Ann', 'Dee'] RETURN n.id AS i") == [1, 2, 5]
    assert ids(graph, "MATCH (n:P) WHERE n.name STARTS WITH 'D' RETURN n.id AS i") == [5]


@pytest.mark.parametrize("mode", MODES)
def test_create_index_reports_a_name_index_as_serving(mode, tmp_path):
    graph = make_graph(mode, tmp_path)
    graph.cypher(SEED)
    built = graph.create_index("P", "name")
    assert built["serves_lookups"] is True
    assert built["not_serving"] is None


@pytest.mark.parametrize("mode", ["memory", "mapped"])
def test_name_index_follows_writes(mode, tmp_path):
    graph = make_graph(mode, tmp_path)
    graph.cypher(SEED)
    graph.cypher("CREATE INDEX FOR (n:P) ON (n.name)")
    q = "MATCH (n:P {name: $v}) RETURN n.id AS i"

    # Retitling a node that stores no name moves what `name` answers.
    graph.cypher("MATCH (n:P {id: 2}) SET n.title = 'Eve'")
    assert ids(graph, q, v="Ann") == [1]
    assert ids(graph, q, v="Eve") == [2]

    # Retitling one that stores a name does not.
    graph.cypher("MATCH (n:P {id: 3}) SET n.title = 'Fay'")
    assert ids(graph, q, v="Cy") == [3]
    assert ids(graph, q, v="Fay") == []

    # `SET n.name` stores a name and sets the title with it.
    graph.cypher("MATCH (n:P {id: 5}) SET n.name = 'Gus'")
    assert ids(graph, q, v="Dee") == []
    assert ids(graph, q, v="Gus") == [5]

    # REMOVE clears both, so the node answers no name at all.
    graph.cypher("MATCH (n:P {id: 3}) REMOVE n.name")
    assert ids(graph, q, v="Cy") == []
    assert ids(graph, q, v="Fay") == []

    graph.cypher("MATCH (n:P {id: 1}) DETACH DELETE n")
    assert ids(graph, q, v="Ann") == []

    # A node created with a title alone is found through the index.
    graph.cypher("CREATE (:P {id: 9, title: 'Hal'})")
    assert ids(graph, q, v="Hal") == [9]


@pytest.mark.parametrize("mode", ["memory", "mapped"])
def test_merge_on_an_indexed_name_is_idempotent(mode, tmp_path):
    graph = make_graph(mode, tmp_path)
    graph.cypher("CREATE INDEX FOR (n:P) ON (n.name)")
    for _ in range(3):
        graph.cypher("MERGE (n:P {name: 'Ann'})")
    graph.cypher("CREATE (:P {id: 7, title: 'Bo'})")
    graph.cypher("MERGE (n:P {name: 'Bo'})")
    assert graph.cypher("MATCH (n:P) RETURN count(n) AS c").to_list() == [{"c": 2}]


def test_title_index_follows_a_name_write(tmp_path):
    graph = KnowledgeGraph()
    graph.cypher("CREATE (:P {id: 1, title: 'Ann'}), (:P {id: 2, title: 'Bo'})")
    graph.cypher("CREATE INDEX FOR (n:P) ON (n.title)")
    graph.cypher("MATCH (n:P {id: 1}) SET n.name = 'Cy'")
    assert ids(graph, "MATCH (n:P {title: $v}) RETURN n.id AS i", v="Cy") == [1]
    assert ids(graph, "MATCH (n:P {title: $v}) RETURN n.id AS i", v="Ann") == []
    graph.cypher("MATCH (n:P {id: 2}) REMOVE n.name")
    assert ids(graph, "MATCH (n:P {title: $v}) RETURN n.id AS i", v="Bo") == []


@pytest.mark.parametrize("kind", ["equality", "range", "title", "composite"])
@pytest.mark.parametrize("seed", range(4))
def test_indexed_answers_equal_unindexed_answers_under_random_writes(kind, seed):
    """Every write path keeps the `name` index equal to what a scan reads."""
    rnd = random.Random(seed)
    indexed, plain = KnowledgeGraph(), KnowledgeGraph()
    both = (indexed, plain)
    for graph in both:
        graph.cypher("CREATE (:P {id: 0, title: 'seed', k: 1})")
    if kind == "range":
        indexed.cypher("CREATE RANGE INDEX FOR (n:P) ON (n.name)")
    elif kind == "composite":
        indexed.cypher("CREATE INDEX FOR (n:P) ON (n.name, n.k)")
    else:
        indexed.cypher("CREATE INDEX FOR (n:P) ON (n.name)")
    if kind == "title":
        indexed.cypher("CREATE INDEX FOR (n:P) ON (n.title)")
    values = ["a", "b", "c", "d"]
    queries = [
        "MATCH (n:P {name: $v}) RETURN n.id AS i ORDER BY i",
        "MATCH (n:P) WHERE n.name = $v RETURN n.id AS i ORDER BY i",
        "MATCH (n:P) WHERE n.name IN [$v, 'zz'] RETURN n.id AS i ORDER BY i",
        "MATCH (n:P) WHERE n.name >= $v RETURN n.id AS i ORDER BY i",
        "MATCH (n:P {title: $v}) RETURN n.id AS i ORDER BY i",
        "MATCH (n:P {name: $v, k: 1}) RETURN n.id AS i ORDER BY i",
    ]
    next_id = 0
    for step in range(120):
        op = rnd.choice(
            ["title", "name", "both", "none", "set_name", "set_title", "rm_name", "rm_title", "delete", "merge"]
        )
        v, target = rnd.choice(values), rnd.randint(0, next_id + 1)
        statement, params = None, {"v": v, "t": target}
        if op in ("title", "name", "both", "none"):
            next_id += 1
            params["i"] = next_id
            props = {
                "title": "id: $i, title: $v, k: 1",
                "name": "id: $i, name: $v, k: 1",
                "both": "id: $i, title: $v, name: 'c', k: 1",
                "none": "id: $i, k: 1",
            }[op]
            statement = f"CREATE (:P {{{props}}})"
        elif op == "set_name":
            statement = "MATCH (n:P {id: $t}) SET n.name = $v"
        elif op == "set_title":
            statement = "MATCH (n:P {id: $t}) SET n.title = $v"
        elif op == "rm_name":
            statement = "MATCH (n:P {id: $t}) REMOVE n.name"
        elif op == "rm_title":
            statement = "MATCH (n:P {id: $t}) REMOVE n.title"
        elif op == "delete":
            statement = "MATCH (n:P {id: $t}) DETACH DELETE n"
        else:
            statement = "MERGE (n:P {name: $v})"
        for graph in both:
            graph.cypher(statement, params=params)
        for probe in values + ["seed", "zz"]:
            for query in queries:
                got = indexed.cypher(query, params={"v": probe}).to_list()
                want = plain.cypher(query, params={"v": probe}).to_list()
                assert got == want, (step, op, query, probe)


@pytest.mark.parametrize("mode", MODES)
def test_declared_title_field_index_is_read(mode, tmp_path):
    graph = make_graph(mode, tmp_path)
    frame = pd.DataFrame({"tid": [1, 2, 3], "label": ["Ann", "Bo", "Ann"], "k": ["x", "y", "z"]})
    graph.add_nodes(frame, "T", "tid", "label")
    built = graph.create_index("T", "label")
    assert built["serves_lookups"] is True
    got = ids(graph, "MATCH (n:T {label: $v}) RETURN n.id AS i", v="Ann")
    assert got == [1, 3]
    assert ids(graph, "MATCH (n:T {title: $v}) RETURN n.id AS i", v="Bo") == [2]
    assert ids(graph, "MATCH (n:T {name: $v}) RETURN n.id AS i", v="Bo") == [2]


@pytest.mark.parametrize("mode", MODES)
def test_declared_id_field_index_is_read(mode, tmp_path):
    graph = make_graph(mode, tmp_path)
    frame = pd.DataFrame({"starId": ["s1", "s2"], "title": ["Ann", "Bo"]})
    graph.add_nodes(frame, "Star", "starId", "title")
    graph.create_index("Star", "starId")
    assert graph.cypher("MATCH (n:Star {starId: 's2'}) RETURN n.title AS t").to_list() == [{"t": "Bo"}]


def test_type_string_alias_index_says_it_does_not_serve():
    """`label` answers the node type on a node that stores none: no index holds that."""
    graph = KnowledgeGraph()
    graph.cypher("CREATE (:P {id: 1, label: 'A'}), (:P {id: 2})")
    built = graph.create_index("P", "label")
    assert built["serves_lookups"] is False
    assert "node type" in built["not_serving"]

    result = graph.cypher("CREATE INDEX FOR (n:P) ON (n.type)")
    assert len(result.warnings) == 1
    assert "queries will not read it" in result.warnings[0]
    assert ids(graph, "MATCH (n:P {label: 'P'}) RETURN n.id AS i") == [2]
    assert ids(graph, "MATCH (n:P {label: 'A'}) RETURN n.id AS i") == [1]


def test_a_failed_statement_restores_the_name_index():
    indexed, plain = KnowledgeGraph(), KnowledgeGraph()
    for graph in (indexed, plain):
        graph.cypher("CREATE (:P {id: 1, title: 'Ann'}), (:P {id: 2, name: 'Bo'})")
    indexed.cypher("CREATE INDEX FOR (n:P) ON (n.name)")
    for graph in (indexed, plain):
        with pytest.raises(Exception):
            graph.cypher("MATCH (n:P) SET n.title = 'Zed' WITH n RETURN 1 / 0")
        with pytest.raises(Exception):
            graph.cypher("MATCH (n:P) REMOVE n.name WITH n RETURN 1 / 0")
    for value in ["Ann", "Bo", "Zed"]:
        query = "MATCH (n:P {name: $v}) RETURN n.id AS i ORDER BY i"
        assert (
            indexed.cypher(query, params={"v": value}).to_list() == plain.cypher(query, params={"v": value}).to_list()
        )
    assert ids(indexed, "MATCH (n:P {name: 'Ann'}) RETURN n.id AS i") == [1]
    assert ids(indexed, "MATCH (n:P {name: 'Bo'}) RETURN n.id AS i") == [2]


def test_a_saved_name_index_is_rebuilt_on_load(tmp_path):
    import kglite

    graph = KnowledgeGraph()
    graph.cypher(SEED)
    graph.cypher("CREATE INDEX FOR (n:P) ON (n.name)")
    path = str(tmp_path / "g.kgl")
    graph.save(path)
    loaded = kglite.load(path)
    assert loaded.create_index("P", "name")["created"] is False
    for value, expected in NAME_GOLDEN.items():
        assert ids(loaded, "MATCH (n:P {name: $v}) RETURN n.id AS i", v=value) == expected
