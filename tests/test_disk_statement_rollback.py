"""A mutating statement that fails partway leaves a disk graph exactly as it was.

Disk statements roll back through a whole-graph snapshot; a property-``SET``-only
statement instead journals the cells, titles and column types it overwrites and
undoes those in place (``StatementCheckpoint::DiskCells``), because the snapshot
route deep-copies every touched column on the statement's first write. Each
statement kind below fails *after* real writes (an overflowing ``duration``
evaluated at row 250 of 300) and is checked on a reopened disk graph, where the
columns are served from the saved file, and on the graph as first built.

Answers are compared with the pre-statement ones through Cypher, then again
after a save and reopen, so a rollback that left a column of the wrong type or
an appended row behind shows up in what the file holds too.
"""

from __future__ import annotations

from typing import NamedTuple

import pandas as pd
import pytest

import kglite

BOOM = "duration({months: 2147483648})"
ROWS = 400


def _staff() -> pd.DataFrame:
    ids = list(range(1, ROWS + 1))
    return pd.DataFrame(
        {
            "id": ids,
            "title": [f"Employee {i}" for i in ids],
            "grade": [0] * ROWS,
            "name": [f"n{i}" for i in ids],
        }
    )


def snapshot(graph: kglite.KnowledgeGraph) -> list[tuple[str, int, str]]:
    """Every node with all its properties, as the statement-level reads see them."""
    return sorted(
        (row["label"], row["id"], repr(sorted(row["props"].items())))
        for row in graph.cypher("MATCH (n) RETURN labels(n)[0] AS label, n.id AS id, properties(n) AS props").to_list()
    )


class Disk(NamedTuple):
    graph: kglite.KnowledgeGraph
    path: str


@pytest.fixture(params=["reopened", "fresh"])
def disk(request, tmp_path):
    path = str(tmp_path / "staff")
    g = kglite.KnowledgeGraph(storage="disk", path=path)
    g.add_nodes(_staff(), "Staff", "id", "title")
    g.save(path)
    if request.param == "reopened":
        del g
        g = kglite.load(path)
    return Disk(g, path)


STATEMENTS = {
    "set_demotes_a_column": (
        "UNWIND range(1, 300) AS i MATCH (n:Staff {id: i}) "
        f"SET n.grade = CASE WHEN i = 100 THEN 'unknown' ELSE i END, "
        f"n.extra = CASE WHEN i = 250 THEN {BOOM} ELSE i END"
    ),
    "set_title_and_new_property": (
        "UNWIND range(1, 300) AS i MATCH (n:Staff {id: i}) "
        f"SET n.title = 'changed', n.fresh = CASE WHEN i = 250 THEN {BOOM} ELSE i END"
    ),
    "create": (
        f"UNWIND range(1, 300) AS i CREATE (:Staff {{id: 1000 + i, grade: CASE WHEN i = 250 THEN {BOOM} ELSE i END}})"
    ),
    "delete": (f"MATCH (n:Staff) WHERE n.id <= 300 DELETE n WITH count(n) AS c CREATE (:Blocked {{v: {BOOM}}})"),
    "merge_create_branch": (
        "UNWIND range(1, 300) AS i MERGE (n:Staff {id: 2000 + i}) "
        f"ON CREATE SET n.grade = CASE WHEN i = 250 THEN {BOOM} ELSE i END"
    ),
    "merge_match_branch": (
        "UNWIND range(1, 300) AS i MERGE (n:Staff {id: i}) "
        f"ON MATCH SET n.grade = CASE WHEN i = 250 THEN {BOOM} ELSE 77 END"
    ),
}


@pytest.mark.parametrize("kind", sorted(STATEMENTS))
def test_a_failed_statement_changes_nothing(disk, kind):
    graph, path = disk
    before = snapshot(graph)
    with pytest.raises(Exception, match="duration"):
        graph.cypher(STATEMENTS[kind])
    assert snapshot(graph) == before

    # What the file holds agrees, and the graph is still writable and durable.
    graph.save(path)
    reopened = kglite.load(path)
    assert snapshot(reopened) == before
    graph.cypher("MATCH (n:Staff {id: 3}) SET n.grade = 3")
    assert graph.cypher("MATCH (n:Staff {id: 3}) RETURN n.grade AS g").to_list() == [{"g": 3}]
    assert graph.cypher("MATCH (n:Staff {id: 4}) RETURN n.grade AS g").to_list() == [{"g": 0}]


def test_a_statement_that_fits_the_column_type_lands(disk):
    graph = disk.graph
    """Non-vacuity: the statements above would change the graph if they did not fail."""
    before = snapshot(graph)
    graph.cypher("UNWIND range(1, 300) AS i MATCH (n:Staff {id: i}) SET n.grade = i, n.fresh = i")
    assert snapshot(graph) != before
    assert graph.cypher("MATCH (n:Staff {id: 300}) RETURN n.grade AS g, n.fresh AS f").to_list() == [
        {"g": 300, "f": 300}
    ]
