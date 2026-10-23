"""`CREATE FULLTEXT INDEX` over a Cypher DDL statement.

A single-label, single-property node statement installs the BM25 text index and
registers the name; the multi-label, multi-property and relationship forms parse
and are refused by name. The index answers `db.index.fulltext.queryNodes`,
shows up in `SHOW INDEXES` as FULLTEXT, and survives a save and a crash.
"""

from __future__ import annotations

import os
import subprocess
import sys
import textwrap

import pytest

import kglite
from kglite import KnowledgeGraph

HEAP = ["memory", "mapped"]  # a disk graph has no BM25 index
DDL = "CREATE FULLTEXT INDEX `f` FOR (n:`Doc`) ON EACH [n.`title`]"


def _graph(mode: str = "memory") -> KnowledgeGraph:
    graph = KnowledgeGraph(storage=mode)
    for i, title in enumerate(["quick red fox", "lazy red dog", "blue cat"]):
        graph.cypher("CREATE (:Doc {id: $i, title: $t, body: 'x'})", params={"i": i, "t": title})
    return graph


def _rows(graph: KnowledgeGraph, query: str = "SHOW INDEXES") -> list[dict]:
    return graph.cypher(query).to_list()


def _hits(graph: KnowledgeGraph, name: str, text: str) -> list[int]:
    rows = graph.cypher(
        "CALL db.index.fulltext.queryNodes($i, $q) YIELD node, score RETURN node.id AS id ORDER BY score DESC, id",
        params={"i": name, "q": text},
    ).to_list()
    return [r["id"] for r in rows]


@pytest.mark.parametrize("mode", HEAP)
def test_create_show_query_drop(mode):
    graph = _graph(mode)
    graph.cypher(DDL)
    [row] = _rows(graph)
    assert row["name"] == "f"
    assert row["type"] == "FULLTEXT"
    assert row["entityType"] == "NODE"
    assert row["labelsOrTypes"] == ["Doc"]
    assert row["properties"] == ["title"]
    [detail] = _rows(graph, "SHOW INDEXES YIELD name, indexProvider, options")
    assert detail["indexProvider"] == "fulltext-1.0"
    assert detail["options"] == {
        "indexConfig": {
            "fulltext.analyzer": "standard-no-stop-words",
            "fulltext.eventually_consistent": False,
        }
    }
    assert _hits(graph, "f", "red") == [0, 1]
    assert _hits(graph, "f", "cat") == [2]
    graph.cypher("DROP INDEX f")
    assert _rows(graph) == []
    with pytest.raises(Exception, match="f"):
        _hits(graph, "f", "red")


def test_if_not_exists_is_idempotent_and_a_repeat_is_an_error_without_it():
    graph = _graph()
    graph.cypher("CREATE FULLTEXT INDEX f IF NOT EXISTS FOR (n:Doc) ON EACH [n.title]")
    graph.cypher("CREATE FULLTEXT INDEX f IF NOT EXISTS FOR (n:Doc) ON EACH [n.title]")
    assert [r["name"] for r in _rows(graph)] == ["f"]
    with pytest.raises(Exception, match="already exists"):
        graph.cypher("CREATE FULLTEXT INDEX f FOR (n:Doc) ON EACH [n.title]")
    # A second name on the same property is a duplicate index, as in Neo4j.
    with pytest.raises(Exception, match="already exists"):
        graph.cypher("CREATE FULLTEXT INDEX g FOR (n:Doc) ON EACH [n.title]")
    graph.cypher("CREATE FULLTEXT INDEX g IF NOT EXISTS FOR (n:Doc) ON EACH [n.title]")
    assert [r["name"] for r in _rows(graph)] == ["f"]


def test_an_unnamed_index_answers_to_its_canonical_name():
    graph = _graph()
    graph.cypher("CREATE FULLTEXT INDEX FOR (n:Doc) ON EACH [n.title]")
    assert [r["name"] for r in _rows(graph)] == ["Doc.title"]
    assert _hits(graph, "Doc.title", "blue") == [2]
    graph.cypher("DROP INDEX `Doc.title`")
    assert _rows(graph) == []


def test_a_name_held_by_a_range_index_or_a_constraint_is_refused_and_builds_nothing():
    graph = _graph()
    graph.cypher("CREATE RANGE INDEX taken FOR (n:Doc) ON (n.body)")
    graph.cypher("CREATE CONSTRAINT held FOR (n:Doc) REQUIRE n.id IS UNIQUE")
    for name in ("taken", "held"):
        with pytest.raises(Exception, match=name):
            graph.cypher(f"CREATE FULLTEXT INDEX {name} FOR (n:Doc) ON EACH [n.title]")
    assert not any(r["type"] == "FULLTEXT" for r in _rows(graph))
    # IF NOT EXISTS over a taken name is the no-op, not an error.
    graph.cypher("CREATE FULLTEXT INDEX taken IF NOT EXISTS FOR (n:Doc) ON EACH [n.title]")
    assert not any(r["type"] == "FULLTEXT" for r in _rows(graph))


@pytest.mark.parametrize(
    "statement, needle",
    [
        ("CREATE FULLTEXT INDEX f FOR (n:Doc|Other) ON EACH [n.title]", "exactly one node label"),
        ("CREATE FULLTEXT INDEX f FOR (n:Doc) ON EACH [n.title, n.body]", "exactly one property"),
        ("CREATE FULLTEXT INDEX f FOR ()-[r:R]-() ON EACH [r.title]", "relationship"),
        (
            "CREATE FULLTEXT INDEX f FOR (n:Doc) ON EACH [n.title] "
            "OPTIONS {indexConfig: {`fulltext.analyzer`: 'english'}}",
            "standard-no-stop-words",
        ),
        (
            "CREATE FULLTEXT INDEX f FOR (n:Doc) ON EACH [n.title] OPTIONS {indexConfig: {`fulltext.bogus`: 1}}",
            "unknown full-text indexConfig key",
        ),
        (
            "CREATE FULLTEXT INDEX f FOR (n:Doc) ON EACH [n.title] OPTIONS {indexProvider: 'x'}",
            "indexProvider",
        ),
    ],
)
def test_forms_outside_the_supported_scope_are_refused_by_name(statement, needle):
    graph = _graph()
    with pytest.raises(Exception, match=needle):
        graph.cypher(statement)
    assert _rows(graph) == []


def test_the_declared_analyzer_and_consistency_options_are_accepted():
    graph = _graph()
    graph.cypher(
        "CREATE FULLTEXT INDEX f FOR (n:Doc) ON EACH [n.title] OPTIONS {indexConfig: "
        "{`fulltext.analyzer`: 'standard-no-stop-words', `fulltext.eventually_consistent`: true}}"
    )
    assert _hits(graph, "f", "fox") == [0]


def test_a_disk_graph_refuses_the_statement_clearly(tmp_path):
    graph = kglite.open(str(tmp_path / "g"), storage="disk")
    graph.cypher("CREATE (:Doc {id: 1, title: 'quick red fox'})")
    with pytest.raises(Exception, match="disk-backed"):
        graph.cypher(DDL)
    assert _rows(graph) == []


@pytest.mark.parametrize("mode", HEAP)
def test_the_index_and_its_name_survive_save_and_load(mode, tmp_path):
    graph = _graph(mode)
    graph.cypher(DDL)
    path = str(tmp_path / "g.kgl")
    graph.save(path)
    reloaded = kglite.load(path)
    [row] = _rows(reloaded)
    assert (row["name"], row["type"], row["labelsOrTypes"], row["properties"]) == ("f", "FULLTEXT", ["Doc"], ["title"])
    assert _hits(reloaded, "f", "red") == [0, 1]
    reloaded.cypher("DROP INDEX f")
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
g.cypher("CREATE (:Doc {id: 0, title: 'quick red fox'}), (:Doc {id: 1, title: 'lazy red dog'})")
"""


@pytest.mark.parametrize("storage", HEAP)
def test_the_index_survives_a_crash_and_a_later_drop_replays(tmp_path, storage):
    _child(tmp_path, storage, SEED + f'g.cypher("{DDL}")\n')
    assert not (tmp_path / "app.kgl").exists()
    g = kglite.open(str(tmp_path / "app.kgl"), durable=True)
    assert [(r["name"], r["type"]) for r in _rows(g)] == [("f", "FULLTEXT")]
    assert _hits(g, "f", "red") == [0, 1]
    del g  # releases the writer lease for the next child

    _child(tmp_path, storage, 'g.cypher("DROP INDEX f")\n')
    g = kglite.open(str(tmp_path / "app.kgl"), durable=True)
    assert _rows(g) == []


EMPTY_DDL = "CREATE FULLTEXT INDEX f FOR (n:Fresh) ON EACH [n.title]"


def _fresh_hits(graph: KnowledgeGraph, text: str) -> list[int]:
    return _hits(graph, "f", text)


@pytest.mark.parametrize("mode", HEAP)
def test_ddl_on_a_label_with_no_nodes_is_accepted_and_listed(mode):
    graph = KnowledgeGraph(storage=mode)
    graph.cypher(EMPTY_DDL)
    [row] = _rows(graph)
    assert (row["name"], row["type"], row["labelsOrTypes"], row["properties"]) == (
        "f",
        "FULLTEXT",
        ["Fresh"],
        ["title"],
    )
    assert _fresh_hits(graph, "red") == []
    graph.cypher("CREATE FULLTEXT INDEX f IF NOT EXISTS FOR (n:Fresh) ON EACH [n.title]")
    assert [r["name"] for r in _rows(graph)] == ["f"]
    with pytest.raises(Exception, match="already exists"):
        graph.cypher(EMPTY_DDL)
    graph.cypher("DROP INDEX f")
    assert _rows(graph) == []


def _create(g):
    g.cypher("CREATE (:Fresh {id: 1, title: 'quick red fox'}), (:Fresh {id: 2, title: 'blue cat'})")


def _merge(g):
    g.cypher("MERGE (:Fresh {id: 1, title: 'quick red fox'})")
    g.cypher("MERGE (:Fresh {id: 2, title: 'blue cat'})")


def _unwind(g):
    g.cypher(
        "UNWIND [{id: 1, t: 'quick red fox'}, {id: 2, t: 'blue cat'}] AS r CREATE (:Fresh {id: r['id'], title: r['t']})"
    )


def _add_nodes(g):
    import pandas as pd

    g.add_nodes(
        pd.DataFrame({"id": [1, 2], "title": ["quick red fox", "blue cat"]}),
        "Fresh",
        "id",
        "title",
    )


@pytest.mark.parametrize("write", [_create, _merge, _unwind, _add_nodes])
@pytest.mark.parametrize("mode", HEAP)
def test_nodes_written_after_the_ddl_are_found(mode, write):
    graph = KnowledgeGraph(storage=mode)
    graph.cypher(EMPTY_DDL)
    write(graph)
    assert _fresh_hits(graph, "red") == [1]
    assert _fresh_hits(graph, "cat") == [2]
    graph.cypher("MATCH (n:Fresh {id: 2}) SET n.title = 'red cat'")
    assert sorted(_fresh_hits(graph, "red")) == [1, 2]


@pytest.mark.parametrize("mode", HEAP)
def test_an_index_over_a_still_empty_label_survives_save_and_load(mode, tmp_path):
    graph = KnowledgeGraph(storage=mode)
    graph.cypher(EMPTY_DDL)
    path = str(tmp_path / "g.kgl")
    graph.save(path)
    reloaded = kglite.load(path)
    assert [r["name"] for r in _rows(reloaded)] == ["f"]
    assert _fresh_hits(reloaded, "red") == []
    _create(reloaded)
    assert _fresh_hits(reloaded, "red") == [1]
    reloaded.cypher("DROP INDEX f")
    assert _rows(reloaded) == []


@pytest.mark.parametrize("storage", HEAP)
def test_an_index_over_a_still_empty_label_survives_a_crash_then_fills(tmp_path, storage):
    _child(tmp_path, storage, f'g.cypher("{EMPTY_DDL}")\n')
    g = kglite.open(str(tmp_path / "app.kgl"), durable=True)
    assert [(r["name"], r["type"]) for r in _rows(g)] == [("f", "FULLTEXT")]
    assert _fresh_hits(g, "red") == []
    del g
    _child(
        tmp_path,
        storage,
        "g.cypher(\"CREATE (:Fresh {id: 1, title: 'quick red fox'})\")\n",
    )
    g = kglite.open(str(tmp_path / "app.kgl"), durable=True)
    assert _fresh_hits(g, "red") == [1]


def test_a_disk_graph_still_refuses_an_empty_label_index(tmp_path):
    graph = kglite.open(str(tmp_path / "g"), storage="disk")
    with pytest.raises(Exception, match="disk-backed"):
        graph.cypher(EMPTY_DDL)
