"""Graph-carried skills: the six Python methods and the contract they enforce.

A skill is markdown methodology stored as a node under the ``KgliteSkill``
system label — hidden from every node-type enumeration, ordinary to Cypher,
persisted in the ``.kgl``. These tests pin the Python surface: what each method
returns, which exception class each refusal raises, and that the SKILL.md
dialect an MCP skills directory serves round-trips through import/export.
"""

from __future__ import annotations

from pathlib import Path

import pandas as pd
import pytest

import kglite
from kglite import KnowledgeGraph

FIXTURES = Path(__file__).parent / "fixtures" / "skills"


@pytest.fixture
def g() -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.add_nodes(
        pd.DataFrame({"id": [1, 2], "title": ["a", "b"]}),
        "Well",
        "id",
        node_title_field="title",
    )
    return graph


def _skill_count(graph: KnowledgeGraph) -> int:
    return graph.cypher("MATCH (s:KgliteSkill) RETURN count(s) AS c").to_dicts()[0]["c"]


# ── CRUD ───────────────────────────────────────────────────────────────────


def test_list_is_empty_until_a_skill_is_set(g):
    assert g.list_skills() == []

    stored = g.set_skill("wells", "How to ask about wells.", body="# Wells\n\nstart here")

    listed = g.list_skills()
    assert len(listed) == 1
    row = listed[0]
    # Core returns listing records with an empty body; the wrapper drops the key
    # rather than report an empty string, which would read as "no body".
    assert "body" not in row
    assert row == {
        "name": "wells",
        "description": "How to ask about wells.",
        "references_tools": [],
        "delivery": "lazy",
    }
    assert stored["created"] is True
    assert stored["body"] == "# Wells\n\nstart here"


def test_get_returns_the_body(g):
    g.set_skill("wells", "desc", body="# Wells\n\nstart here")
    assert g.get_skill("wells")["body"] == "# Wells\n\nstart here"


def test_get_unknown_raises_node_not_found(g):
    with pytest.raises(kglite.NodeNotFoundError):
        g.get_skill("absent")


def test_set_again_updates_in_place(g):
    g.set_skill("wells", "first")
    again = g.set_skill("wells", "second", body="new body")

    assert again["created"] is False
    assert again["description"] == "second"
    assert g.get_skill("wells")["body"] == "new body"
    assert _skill_count(g) == 1, "MERGE must not duplicate the node"


def test_references_tools_survives_as_a_python_list(g):
    g.set_skill("wells", "desc", references_tools=["cypher_query", "graph_overview"])
    assert g.get_skill("wells")["references_tools"] == ["cypher_query", "graph_overview"]
    assert g.list_skills()[0]["references_tools"] == ["cypher_query", "graph_overview"]


def test_delete_reports_whether_there_was_anything_to_delete(g):
    g.set_skill("wells", "desc")
    assert g.delete_skill("wells") is True
    assert g.delete_skill("wells") is False
    assert g.list_skills() == []


# ── Validation refusals ────────────────────────────────────────────────────


@pytest.mark.parametrize(
    "kwargs",
    [
        {"name": "", "description": "d"},
        {"name": "   ", "description": "d"},
        {"name": "two words", "description": "d"},
        {"name": "a/b", "description": "d"},
        {"name": "wells", "description": ""},
        {"name": "wells", "description": "d", "delivery": "weird"},
        {"name": "wells", "description": "d", "body": "x" * (16 * 1024 + 1)},
    ],
)
def test_invalid_arguments_raise_argument_error(g, kwargs):
    with pytest.raises(kglite.ArgumentError):
        g.set_skill(**kwargs)
    assert _skill_count(g) == 0, "a refused write must leave the graph alone"


def test_a_body_at_the_ceiling_is_accepted(g):
    g.set_skill("wells", "desc", body="x" * (16 * 1024))
    assert len(g.get_skill("wells")["body"]) == 16 * 1024


def test_read_only_refuses_writes_and_changes_nothing(g):
    g.set_skill("wells", "desc")
    g.read_only(True)

    with pytest.raises(kglite.ArgumentError, match="read-only"):
        g.set_skill("fields", "desc")
    with pytest.raises(kglite.ArgumentError, match="read-only"):
        g.delete_skill("wells")

    assert [s["name"] for s in g.list_skills()] == ["wells"]
    g.read_only(False)
    assert g.set_skill("fields", "desc")["created"] is True


def test_a_schema_locked_graph_refuses_an_undeclared_skill_type(g):
    """A locked schema stops skills too, and says which type it does not know.

    The class is the Cypher path's own — `set_skill` writes through `MERGE`,
    so the schema-lock refusal arrives wrapped as a Cypher execution failure
    rather than a `SchemaError`, exactly as a hand-written `CREATE` would.
    """
    g.lock_schema()
    with pytest.raises(kglite.CypherExecutionError, match="Unknown node type 'KgliteSkill'"):
        g.set_skill("wells", "desc")
    # Read through `list_skills`, not Cypher: the lock refuses a `MATCH` on an
    # undeclared label too, which would mask the assertion with its own error.
    assert g.list_skills() == []


# ── The hidden label ───────────────────────────────────────────────────────


def test_the_label_is_hidden_from_enumerations_but_not_from_cypher(g):
    g.set_skill("wells", "desc")

    assert "KgliteSkill" not in g.node_types
    assert "KgliteSkill" not in g.describe()
    assert _skill_count(g) == 1


# ── Persistence ────────────────────────────────────────────────────────────


def test_skills_survive_a_save_load_round_trip(g, tmp_path):
    g.set_skill(
        "wells",
        "desc",
        body="# Wells",
        references_tools=["cypher_query"],
        delivery="eager",
    )
    path = str(tmp_path / "graph.kgl")
    g.save(path)

    reloaded = kglite.load(path)
    assert reloaded.get_skill("wells") == {
        "name": "wells",
        "description": "desc",
        "body": "# Wells",
        "references_tools": ["cypher_query"],
        "delivery": "eager",
    }


# ── Import / export ────────────────────────────────────────────────────────


def test_import_reads_a_directory_of_skill_markdown(g):
    # cypher_query.md is a verbatim copy of the server's own bundled skill, so
    # this also proves the mcp-methods frontmatter dialect parses unchanged.
    names = g.import_skills(str(FIXTURES))

    assert names == ["cypher_query", "wells"], "sorted, and the .txt is skipped"
    assert [s["name"] for s in g.list_skills()] == ["cypher_query", "wells"]
    assert g.get_skill("wells")["delivery"] == "eager"
    assert g.get_skill("wells")["references_tools"] == ["cypher_query", "graph_overview"]
    assert g.get_skill("cypher_query")["body"].startswith("# `cypher_query` methodology")


def test_import_reads_a_single_file(g):
    assert g.import_skills(str(FIXTURES / "wells.md")) == ["wells"]
    assert len(g.list_skills()) == 1


def test_import_of_a_missing_path_raises_file_error(g, tmp_path):
    with pytest.raises(kglite.FileError):
        g.import_skills(str(tmp_path / "nowhere"))


def test_import_of_malformed_frontmatter_names_the_file(g, tmp_path):
    bad = tmp_path / "broken.md"
    bad.write_text("---\ndescription: no name here\n---\n\nbody\n", encoding="utf-8")
    with pytest.raises(kglite.FileFormatError, match="broken.md"):
        g.import_skills(str(bad))


def test_export_then_import_into_a_fresh_graph_is_identical(g, tmp_path):
    g.import_skills(str(FIXTURES))
    out = tmp_path / "exported"

    assert g.export_skills(str(out)) == ["cypher_query", "wells"]
    assert sorted(p.name for p in out.iterdir()) == ["cypher_query.md", "wells.md"]

    fresh = KnowledgeGraph()
    fresh.import_skills(str(out))
    assert fresh.list_skills() == g.list_skills()
    for name in ("cypher_query", "wells"):
        assert fresh.get_skill(name) == g.get_skill(name)


# ── Interaction with an open transaction ───────────────────────────────────


def test_a_skill_write_makes_an_open_transaction_lose_its_commit_race(g):
    """`set_skill` is an ordinary graph mutation under the OCC contract.

    A transaction that began before the write and then mutates loses the race,
    exactly as it would against any other outside `cypher()` write.
    """
    tx = g.begin()
    g.set_skill("wells", "desc")
    tx.cypher("CREATE (:Well {id: 3})")

    with pytest.raises(kglite.TransactionConflictError, match="conflict"):
        tx.commit()

    assert [s["name"] for s in g.list_skills()] == ["wells"]
    assert g.cypher("MATCH (w:Well) RETURN count(w) AS c").to_dicts()[0]["c"] == 2
