"""`CREATE VECTOR INDEX` over a stored list property.

The index reads the vectors from the node property itself (no embedding store to
load) and stays fresh through every write path: a write marks the slot, the next
query folds it. The tests below pin the freshness of each path, the statement
rollback, the one-source-per-column rule, and the persistence of the declaration
(the structure is rebuilt from the rows on load and WAL replay).
"""

from __future__ import annotations

import os
import subprocess
import sys
import textwrap

import pandas as pd
import pytest

import kglite
from kglite import KnowledgeGraph

HEAP = ["memory", "mapped"]  # a disk graph refuses the index
DDL = (
    "CREATE VECTOR INDEX `v` FOR (n:`Doc`) ON (n.`emb`) "
    "OPTIONS {indexConfig: {`vector.dimensions`: 3, `vector.similarity_function`: 'cosine'}}"
)
QUERY = "CALL db.index.vector.queryNodes('v', 50, $q) YIELD node, score RETURN node.id AS id, score"
X, Y, Z = [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]


def _graph(mode: str = "memory") -> KnowledgeGraph:
    graph = KnowledgeGraph(storage=mode)
    graph.cypher(
        "CREATE (:Doc {id: 0, emb: [1.0, 0.0, 0.0]}), (:Doc {id: 1, emb: [0.0, 1.0, 0.0]}), "
        "(:Doc {id: 2, emb: [0.0, 0.0, 1.0]})"
    )
    return graph


def _indexed(mode: str = "memory") -> KnowledgeGraph:
    graph = _graph(mode)
    graph.cypher(DDL)
    return graph


def _ids(graph, vector=None) -> list[int]:
    """Every indexed node id, best match first."""
    rows = graph.cypher(QUERY, params={"q": vector or [1.0, 0.5, 0.25]}).to_list()
    return [r["id"] for r in rows]


def _members(graph) -> set[int]:
    return set(_ids(graph))


def _rows(graph, query: str = "SHOW INDEXES") -> list[dict]:
    return graph.cypher(query).to_list()


@pytest.mark.parametrize("mode", HEAP)
def test_create_query_search_show_drop(mode):
    graph = _indexed(mode)
    [row] = _rows(graph, "SHOW INDEXES YIELD name, type, entityType, labelsOrTypes, properties, state, options")
    assert row["name"] == "v"
    assert row["type"] == "VECTOR"
    assert row["entityType"] == "NODE"
    assert row["labelsOrTypes"] == ["Doc"]
    assert row["properties"] == ["emb"]
    assert row["state"] == "ONLINE"
    config = row["options"]["indexConfig"]
    assert config["vector.dimensions"] == 3
    assert config["vector.similarity_function"].lower() == "cosine"
    assert _ids(graph, X)[0] == 0
    assert _ids(graph, Y)[0] == 1
    searched = graph.cypher(
        "MATCH (n:Doc) SEARCH n IN (VECTOR INDEX v FOR $q LIMIT 2) SCORE AS s RETURN n.id AS id, s ORDER BY s DESC",
        params={"q": Z},
    ).to_list()
    assert searched[0]["id"] == 2
    assert len(searched) == 2
    graph.cypher("DROP INDEX v")
    assert _rows(graph) == []
    with pytest.raises(Exception, match="v"):
        _ids(graph)


@pytest.mark.parametrize("mode", HEAP)
def test_euclidean_metric_ranks_by_distance(mode):
    graph = KnowledgeGraph(storage=mode)
    graph.cypher("CREATE (:Doc {id: 0, emb: [10.0, 0.0, 0.0]}), (:Doc {id: 1, emb: [1.0, 0.0, 0.0]})")
    graph.cypher(DDL.replace("'cosine'", "'euclidean'"))
    assert _ids(graph, [0.9, 0.0, 0.0]) == [1, 0]
    [row] = _rows(graph, "SHOW INDEXES YIELD options")
    assert row["options"]["indexConfig"]["vector.similarity_function"].lower() == "euclidean"


# Each statement changes what the index must hold; the expected ids are the
# members after it. Every write path that can change the property or the
# membership of an indexed node appears here.
FRESHNESS = [
    ("create", "CREATE (:Doc {id: 7, emb: [0.5, 0.5, 0.5]})", {0, 1, 2, 7}),
    ("create-no-vector", "CREATE (:Doc {id: 7})", {0, 1, 2}),
    ("create-wrong-dimension", "CREATE (:Doc {id: 7, emb: [1.0, 2.0]})", {0, 1, 2}),
    ("create-non-numeric", "CREATE (:Doc {id: 7, emb: ['a', 'b', 'c']})", {0, 1, 2}),
    ("create-null-member", "CREATE (:Doc {id: 7, emb: [1.0, null, 0.0]})", {0, 1, 2}),
    ("create-zero-vector", "CREATE (:Doc {id: 7, emb: [0.0, 0.0, 0.0]})", {0, 1, 2}),
    ("create-other-label", "CREATE (:Other {id: 7, emb: [1.0, 1.0, 1.0]})", {0, 1, 2}),
    ("create-integers-widen", "CREATE (:Doc {id: 7, emb: [1, 2, 3]})", {0, 1, 2, 7}),
    ("unwind-create", "UNWIND [7, 8, 9] AS i CREATE (:Doc {id: i, emb: [1.0, 1.0, 1.0]})", {0, 1, 2, 7, 8, 9}),
    ("set-property", "MATCH (n:Doc {id: 0}) SET n.emb = [0.0, 1.0, 1.0]", {0, 1, 2}),
    ("set-property-invalid", "MATCH (n:Doc {id: 0}) SET n.emb = [1.0]", {1, 2}),
    ("set-property-null", "MATCH (n:Doc {id: 0}) SET n.emb = null", {1, 2}),
    (
        "set-gives-vector",
        "MATCH (n:Doc {id: 0}) SET n.emb = null WITH n MATCH (m:Doc {id: 0}) SET m.emb = [1.0, 1.0, 1.0]",
        {0, 1, 2},
    ),
    ("set-map-replace", "MATCH (n:Doc {id: 0}) SET n = {id: 0, emb: [2.0, 2.0, 2.0]}", {0, 1, 2}),
    ("set-map-replace-drops-vector", "MATCH (n:Doc {id: 0}) SET n = {id: 0}", {1, 2}),
    ("set-map-add", "MATCH (n:Doc {id: 0}) SET n += {emb: [3.0, 3.0, 3.0]}", {0, 1, 2}),
    ("set-map-add-other-key", "MATCH (n:Doc {id: 0}) SET n += {note: 'x'}", {0, 1, 2}),
    ("remove-property", "MATCH (n:Doc {id: 0}) REMOVE n.emb", {1, 2}),
    ("delete", "MATCH (n:Doc {id: 0}) DELETE n", {1, 2}),
    ("detach-delete", "MATCH (n:Doc) WHERE n.id < 2 DETACH DELETE n", {2}),
    ("delete-then-create-reuses-slot", "MATCH (n:Doc {id: 0}) DELETE n WITH 1 AS one CREATE (:Doc {id: 8})", {1, 2}),
    ("secondary-label-added", "MATCH (n:Doc {id: 0}) SET n:Extra", {0, 1, 2}),
    ("secondary-label-removed", "MATCH (n:Doc {id: 0}) SET n:Extra WITH n REMOVE n:Extra", {0, 1, 2}),
    ("merge-create", "MERGE (n:Doc {id: 7}) ON CREATE SET n.emb = [1.0, 1.0, 1.0]", {0, 1, 2, 7}),
    ("merge-create-inline", "MERGE (n:Doc {id: 7, emb: [1.0, 1.0, 1.0]})", {0, 1, 2, 7}),
    ("merge-match", "MERGE (n:Doc {id: 0}) ON MATCH SET n.emb = [0.0, 2.0, 2.0]", {0, 1, 2}),
    ("merge-match-removes", "MERGE (n:Doc {id: 0}) ON MATCH SET n.emb = [9.0]", {1, 2}),
]


@pytest.mark.parametrize("mode", HEAP)
@pytest.mark.parametrize("statement,members", [pytest.param(s, m, id=i) for i, s, m in FRESHNESS])
def test_every_write_path_keeps_the_index_fresh(mode, statement, members):
    graph = _indexed(mode)
    assert _members(graph) == {0, 1, 2}
    graph.cypher(statement)
    assert _members(graph) == members
    # The property is the source of truth: an index rebuilt from the rows holds
    # exactly what the incrementally folded one did.
    graph.cypher("DROP INDEX v")
    graph.cypher(DDL)
    assert _members(graph) == members


@pytest.mark.parametrize("mode", HEAP)
def test_the_updated_vector_is_what_ranks(mode):
    graph = _indexed(mode)
    assert _ids(graph, X)[0] == 0
    graph.cypher("MATCH (n:Doc {id: 0}) SET n.emb = [0.0, 0.0, 5.0]")
    assert _ids(graph, [1.0, 0.1, 0.0])[0] == 1
    assert _ids(graph, [0.0, 0.1, 1.0])[0] in (0, 2)
    graph.cypher("MATCH (n:Doc {id: 1}) SET n += {emb: [1.0, 0.0, 0.0]}")
    assert _ids(graph, [1.0, 0.1, 0.0])[0] == 1
    graph.cypher("MATCH (n:Doc {id: 1}) SET n += {emb: [0.0, 0.0, 2.0]}")
    graph.cypher("MATCH (n:Doc {id: 2}) SET n.emb = [1.0, 0.2, 0.0]")
    assert _ids(graph, [1.0, 0.1, 0.0])[0] == 2


@pytest.mark.parametrize("mode", HEAP)
def test_a_node_created_in_a_deleted_slot_does_not_inherit_the_old_vector(mode):
    graph = _indexed(mode)
    graph.cypher("MATCH (n:Doc {id: 2}) DELETE n")
    graph.cypher("CREATE (:Other {id: 20, emb: [0.0, 0.0, 1.0]})")
    graph.cypher("CREATE (:Doc {id: 21})")
    assert _members(graph) == {0, 1}
    ranked = graph.cypher(QUERY, params={"q": Z}).to_list()
    assert 20 not in [r["id"] for r in ranked]
    assert 21 not in [r["id"] for r in ranked]


@pytest.mark.parametrize("mode", HEAP)
def test_a_node_created_in_a_slot_an_earlier_fold_vacated_is_indexed(mode):
    # The fold that saw the delete has moved the watermark past the slot, so the
    # creation into it is only noticed through the creation hook.
    graph = _indexed(mode)
    graph.cypher("MATCH (n:Doc {id: 2}) DELETE n")
    assert _members(graph) == {0, 1}
    graph.cypher("CREATE (:Doc {id: 30, emb: [0.0, 0.0, 1.0]})")
    assert _members(graph) == {0, 1, 30}
    graph.cypher("MATCH (n:Doc {id: 30}) DELETE n")
    assert _members(graph) == {0, 1}
    graph.add_nodes(pd.DataFrame({"id": [31], "title": ["x"], "emb": [[0.0, 0.0, 1.0]]}), "Doc", "id", "title")
    assert _members(graph) == {0, 1, 31}


@pytest.mark.parametrize("mode", HEAP)
def test_bulk_loaders_keep_the_index_fresh(mode):
    graph = KnowledgeGraph(storage=mode)
    frame = pd.DataFrame({"id": [0, 1], "name": ["a", "b"], "emb": [X, Y]})
    graph.add_nodes(frame, "Doc", "id", "name")
    graph.cypher(DDL)
    assert _members(graph) == {0, 1}
    more = pd.DataFrame({"id": [1, 2], "name": ["b", "c"], "emb": [[1.0, 1.0, 0.0], Z]})
    graph.add_nodes(more, "Doc", "id", "name")
    assert _members(graph) == {0, 1, 2}
    assert _ids(graph, [1.0, 1.0, 0.0])[0] == 1
    # a second batch that rewrites an existing node's vector
    graph.add_nodes(pd.DataFrame({"id": [0], "name": ["a"], "emb": [[0.0, 0.0, 9.0]]}), "Doc", "id", "name")
    assert _ids(graph, Z)[0] in (0, 2)
    graph.add_nodes_bulk(
        [
            {
                "node_type": "Doc",
                "unique_id_field": "id",
                "node_title_field": "name",
                "data": pd.DataFrame({"id": [5], "name": ["e"], "emb": [[2.0, 2.0, 2.0]]}),
            }
        ]
    )
    assert _members(graph) == {0, 1, 2, 5}


FAILING_TAIL = (
    " WITH n CALL db.index.vector.queryNodes('v', 50, [1.0, 0.5, 0.25]) YIELD node RETURN node, 1 / 0 AS boom"
)


@pytest.mark.parametrize("mode", HEAP)
@pytest.mark.parametrize(
    "write,members",
    [
        ("MATCH (n:Doc {id: 0}) SET n.emb = [0.0, 1.0, 1.0] REMOVE n.emb", {0, 1, 2}),
        ("MATCH (n:Doc {id: 0}) REMOVE n.emb", {0, 1, 2}),
        ("CREATE (n:Doc {id: 9, emb: [1.0, 1.0, 1.0]})", {0, 1, 2}),
        ("MATCH (n:Doc {id: 0}) DELETE n", {0, 1, 2}),
    ],
    ids=["set-then-remove", "remove", "create", "delete"],
)
def test_a_failed_statement_leaves_the_pre_statement_index(mode, write, members):
    graph = _indexed(mode)
    with pytest.raises(Exception):
        graph.cypher(write + FAILING_TAIL)
    assert _members(graph) == members
    assert _ids(graph, X)[0] == 0


@pytest.mark.parametrize("mode", HEAP)
def test_a_rolled_back_transaction_leaves_the_pre_transaction_index(mode):
    graph = _indexed(mode)
    tx = graph.begin()
    tx.cypher("MATCH (n:Doc {id: 0}) DELETE n")
    tx.cypher("CREATE (:Doc {id: 9, emb: [1.0, 1.0, 1.0]})")
    tx.cypher("MATCH (n:Doc {id: 1}) SET n.emb = [5.0]")
    assert _members(tx) == {2, 9}
    tx.rollback()
    assert _members(graph) == {0, 1, 2}
    assert _ids(graph, X)[0] == 0


@pytest.mark.parametrize("mode", HEAP)
def test_a_committed_transaction_keeps_its_writes(mode):
    graph = _indexed(mode)
    with graph.begin() as tx:
        tx.cypher("CREATE (:Doc {id: 9, emb: [1.0, 1.0, 1.0]})")
    assert _members(graph) == {0, 1, 2, 9}


@pytest.mark.parametrize("mode", HEAP)
def test_show_indexes_reports_the_nodes_the_index_skips(mode):
    graph = _indexed(mode)
    [row] = _rows(graph)
    assert row.get("unembedded", 0) == 0
    graph.cypher("CREATE (:Doc {id: 7}), (:Doc {id: 8, emb: [1.0]})")
    [row] = _rows(graph)
    assert row.get("unembedded") == 2
    assert row["stale"] is False


@pytest.mark.parametrize("mode", HEAP)
def test_vacuum_rebuilds_the_index_over_the_renumbered_nodes(mode):
    graph = _indexed(mode)
    graph.cypher("CREATE (:Doc {id: 3, emb: [1.0, 1.0, 1.0]})")
    graph.cypher("MATCH (n:Doc {id: 0}) DELETE n")
    graph.vacuum()
    assert _members(graph) == {1, 2, 3}
    graph.cypher("MATCH (n:Doc {id: 1}) SET n.emb = [4.0, 0.0, 0.0]")
    assert _ids(graph, X)[0] == 1


def test_if_not_exists_is_idempotent_and_a_repeat_is_an_error_without_it():
    graph = _graph()
    graph.cypher(DDL.replace("INDEX `v`", "INDEX `v` IF NOT EXISTS"))
    graph.cypher(DDL.replace("INDEX `v`", "INDEX `v` IF NOT EXISTS"))
    assert [r["name"] for r in _rows(graph)] == ["v"]
    with pytest.raises(Exception, match="already"):
        graph.cypher(DDL)
    with pytest.raises(Exception, match="already"):
        graph.cypher(DDL.replace("INDEX `v`", "INDEX `w`"))
    graph.cypher("DROP INDEX v IF EXISTS")
    graph.cypher("DROP INDEX v IF EXISTS")
    assert _rows(graph) == []


def test_a_name_another_index_or_constraint_holds_is_refused():
    graph = _graph()
    graph.cypher("CREATE INDEX byid FOR (n:Doc) ON (n.id)")
    with pytest.raises(Exception, match="byid"):
        graph.cypher(DDL.replace("`v`", "byid"))
    graph.cypher("CREATE CONSTRAINT uniq FOR (n:Doc) REQUIRE n.id IS UNIQUE")
    with pytest.raises(Exception, match="uniq"):
        graph.cypher(DDL.replace("`v`", "uniq"))
    graph.cypher(DDL)
    assert {r["name"] for r in _rows(graph)} >= {"v", "byid"}


def test_a_column_is_either_property_sourced_or_embedder_managed():
    graph = _graph()
    graph.cypher(DDL)
    with pytest.raises(Exception, match="source column"):
        graph.set_embeddings("Doc", "emb", {0: X})
    with pytest.raises(Exception, match="source column"):
        graph.cypher(
            "MATCH (n:Doc {id: 0}) WITH collect(n) AS ns "
            "CALL db.node_embeddings.set({type: 'Doc', text_column: 'emb', "
            "entries: [{node: ns[0], vector: [1.0, 0.0, 0.0]}]}) "
            "YIELD stored RETURN stored"
        )
    graph.cypher("DROP INDEX v")
    graph.set_embeddings("Doc", "emb", {0: X})
    with pytest.raises(Exception, match="embedding store"):
        graph.cypher(DDL)
    assert _rows(graph) == []


@pytest.mark.parametrize(
    "statement,needle",
    [
        (DDL.replace("`vector.dimensions`: 3, ", ""), "vector.dimensions"),
        (DDL.replace("3", "0"), "vector.dimensions"),
        (DDL.replace("'cosine'", "'manhattan'"), "similarity_function"),
        (DDL.replace("`vector.similarity_function`", "`vector.hnsw.m`"), "hnsw.m"),
        (DDL.replace("OPTIONS", "OPTIONS {indexProvider: 'x'} --"), "indexProvider"),
        ("CREATE VECTOR INDEX FOR (n:Doc) ON (n.emb) OPTIONS {indexConfig: {`vector.dimensions`: 3}}", "needs a name"),
        (
            "CREATE VECTOR INDEX v FOR ()-[r:T]-() ON (r.emb) OPTIONS {indexConfig: {`vector.dimensions`: 3}}",
            "relationship",
        ),
    ],
)
def test_forms_outside_the_supported_scope_are_refused_by_name(statement, needle):
    graph = _graph()
    with pytest.raises(Exception, match=needle):
        graph.cypher(statement)
    assert _rows(graph) == []


def test_a_query_of_the_wrong_dimension_is_refused():
    graph = _indexed()
    with pytest.raises(Exception, match="dimension"):
        graph.cypher(QUERY, params={"q": [1.0, 0.0]})


def test_an_index_can_be_declared_before_any_node_exists():
    graph = KnowledgeGraph()
    graph.cypher(DDL)
    assert _ids(graph) == []
    graph.cypher("CREATE (:Doc {id: 1, emb: [1.0, 0.0, 0.0]})")
    assert _ids(graph) == [1]


def test_a_disk_graph_refuses_the_statement_clearly(tmp_path):
    graph = kglite.open(str(tmp_path / "g"), storage="disk")
    graph.cypher("CREATE (:Doc {id: 1, emb: [1.0, 0.0, 0.0]})")
    with pytest.raises(Exception, match="disk-backed"):
        graph.cypher(DDL)
    assert _rows(graph) == []


@pytest.mark.parametrize("mode", HEAP)
def test_the_index_and_its_name_survive_save_and_load(mode, tmp_path):
    graph = _indexed(mode)
    graph.cypher("MATCH (n:Doc {id: 0}) SET n.emb = [0.0, 0.0, 2.0]")
    path = str(tmp_path / "g.kgl")
    graph.save(path)
    reloaded = kglite.load(path)
    [row] = _rows(reloaded, "SHOW INDEXES YIELD name, type, labelsOrTypes, properties, options")
    assert (row["name"], row["type"], row["labelsOrTypes"], row["properties"]) == ("v", "VECTOR", ["Doc"], ["emb"])
    assert row["options"]["indexConfig"]["vector.dimensions"] == 3
    assert _members(reloaded) == {0, 1, 2}
    assert _ids(reloaded, Z)[0] in (0, 2)
    # the loaded graph keeps the index fresh
    reloaded.cypher("CREATE (:Doc {id: 5, emb: [1.0, 1.0, 1.0]})")
    reloaded.cypher("MATCH (n:Doc {id: 1}) DELETE n")
    assert _members(reloaded) == {0, 2, 5}
    reloaded.cypher("DROP INDEX v")
    assert _rows(reloaded) == []


def _child(tmp_path, storage: str, body: str) -> None:
    script = textwrap.dedent(
        f"""
        import kglite, os
        path = {str(tmp_path / "app.kgl")!r}
        kwargs = {{"durable": True}}
        if {storage!r} != "memory" and not os.path.exists(path):
            kwargs["storage"] = {storage!r}
        g = kglite.open(path, **kwargs)
        {textwrap.indent(textwrap.dedent(body), "        ").strip()}
        os._exit(0)
        """
    )
    subprocess.run([sys.executable, "-c", script], check=True, env=dict(os.environ))


SEED = """
g.cypher("CREATE (:Doc {id: 0, emb: [1.0, 0.0, 0.0]}), (:Doc {id: 1, emb: [0.0, 1.0, 0.0]})")
"""


@pytest.mark.parametrize("storage", HEAP)
def test_the_index_survives_a_crash_and_its_later_writes_replay(tmp_path, storage):
    _child(
        tmp_path,
        storage,
        SEED
        + f'g.cypher("{DDL}")\n'
        + 'g.cypher("CREATE (:Doc {id: 2, emb: [0.0, 0.0, 1.0]})")\n'
        + 'g.cypher("MATCH (n:Doc {id: 0}) DELETE n")\n',
    )
    assert not (tmp_path / "app.kgl").exists()
    g = kglite.open(str(tmp_path / "app.kgl"), durable=True)
    assert [(r["name"], r["type"]) for r in _rows(g)] == [("v", "VECTOR")]
    assert _members(g) == {1, 2}
    g.cypher("CREATE (:Doc {id: 3, emb: [1.0, 1.0, 1.0]})")
    assert _members(g) == {1, 2, 3}
    del g  # releases the writer lease for the next child

    _child(tmp_path, storage, 'g.cypher("DROP INDEX v")\n')
    g = kglite.open(str(tmp_path / "app.kgl"), durable=True)
    assert _rows(g) == []
    assert g.cypher("MATCH (n:Doc) RETURN count(n) AS c").to_list() == [{"c": 3}]
