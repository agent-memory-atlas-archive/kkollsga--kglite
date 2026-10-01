"""Type and property names are data: they must never become a path.

A node type or property named ``../../pwn`` used to name a directory or file
component of a disk save (the per-type sidecar ``columns/<type>/columns.zst``)
and of the column spill files (``<spill>/<type>/<token>/<property>.i64``), so a
crafted name wrote outside the generation, and one more ``../`` outside the
graph directory. Names now reach the filesystem only through interned-key hex
stems (sidecar directories) and numbers (a spill directory is a per-store
token, a spill file is ``slot_<n>``), and the sidecar directory is recorded in
the column metadata rather than recovered from the directory name.

Every case walks the sandbox before and after and asserts that everything new is
under the graph directory (or the spill directory it configured), and that the
graph, including the values that live in a sidecar, round-trips.

Run: pytest tests/test_hostile_names_stay_inside_the_graph.py
"""

from __future__ import annotations

import os

import pandas as pd
import pytest

import kglite

HOSTILE_TYPES = [
    "../x",
    "../../pwn",
    "../../../pwn",
    "../../../../pwn",
    "a/b",
    "..",
    ".",
    "",
    "con",
    "nul.txt",
    "a\\b",
    "x" * 300,
    "Ünïcödé/日本",
    "ABSOLUTE",  # replaced by an absolute path inside the sandbox per test
]


def _tree(root) -> set[str]:
    seen: set[str] = set()
    for base, dirs, files in os.walk(root):
        for name in dirs + files:
            seen.add(os.path.relpath(os.path.join(base, name), root))
    return seen


def _resolve(name: str, sandbox) -> str:
    return str(sandbox / "abs_sink") if name == "ABSOLUTE" else name


def _outside(before: set[str], after: set[str], allowed: tuple[str, ...]) -> list[str]:
    return sorted(p for p in after - before if not any(p == a or p.startswith(a + os.sep) for a in allowed))


def _staff(graph, node_type: str) -> None:
    frame = pd.DataFrame({"id": [1, 2, 3], "name": ["Ada", "Grace", "Edsger"]})
    graph.add_nodes(frame, node_type, "id", "name")
    # An int and a string in one property make a Mixed column, which is the one
    # shape that is written as a per-type sidecar.
    graph.cypher("MATCH (n {id: 1}) SET n.badge = 7")
    graph.cypher("MATCH (n {id: 2}) SET n.badge = 'B-2'")


@pytest.mark.parametrize("name", HOSTILE_TYPES)
def test_a_disk_save_with_a_hostile_type_name_writes_only_inside_the_graph(name, tmp_path):
    sandbox = tmp_path / "sandbox"
    box = sandbox / "box"
    box.mkdir(parents=True)
    node_type = _resolve(name, sandbox)
    path = str(box / "g")
    before = _tree(sandbox)

    graph = kglite.KnowledgeGraph(storage="disk", path=path)
    _staff(graph, node_type)
    graph.save()
    del graph

    assert _outside(before, _tree(sandbox), (os.path.join("box", "g"),)) == []
    reloaded = kglite.load(path)
    rows = reloaded.cypher("MATCH (n) RETURN n.id AS id, n.badge AS badge, labels(n) AS labels ORDER BY id").to_list()
    assert [(r["id"], r["badge"]) for r in rows] == [(1, 7), (2, "B-2"), (3, None)]
    assert all(r["labels"] == [node_type] for r in rows)


def test_a_second_save_and_reopen_keep_a_hostile_type_and_its_sidecar(tmp_path):
    sandbox = tmp_path / "sandbox"
    box = sandbox / "box"
    box.mkdir(parents=True)
    path = str(box / "g")
    node_type = "../../pwn"
    before = _tree(sandbox)

    graph = kglite.KnowledgeGraph(storage="disk", path=path)
    _staff(graph, node_type)
    graph.save()
    del graph

    reopened = kglite.load(path)
    reopened.cypher("MATCH (n {id: 3}) SET n.badge = 3.5")
    reopened.save()
    del reopened

    assert _outside(before, _tree(sandbox), (os.path.join("box", "g"),)) == []
    final = kglite.load(path)
    rows = final.cypher("MATCH (n) RETURN n.id AS id, n.badge AS badge ORDER BY id").to_list()
    assert [(r["id"], r["badge"]) for r in rows] == [(1, 7), (2, "B-2"), (3, 3.5)]


@pytest.mark.parametrize("prop", ["../../pwn", "a/b", "..", "con", "x" * 300, "__id__"])
def test_a_hostile_property_name_round_trips_on_disk_without_leaving_the_graph(prop, tmp_path):
    sandbox = tmp_path / "sandbox"
    box = sandbox / "box"
    box.mkdir(parents=True)
    path = str(box / "g")
    before = _tree(sandbox)

    graph = kglite.KnowledgeGraph(storage="disk", path=path)
    frame = pd.DataFrame({"id": [1, 2, 3], "name": ["Ada", "Grace", "Edsger"], prop: [10, 20, 30]})
    graph.add_nodes(frame, "Employee", "id", "name")
    graph.save()
    del graph

    assert _outside(before, _tree(sandbox), (os.path.join("box", "g"),)) == []
    reloaded = kglite.load(path)
    rows = reloaded.cypher(f"MATCH (n:Employee) RETURN n.id AS id, n.`{prop}` AS v ORDER BY id").to_list()
    assert [(r["id"], r["v"]) for r in rows] == [(1, 10), (2, 20), (3, 30)]


@pytest.mark.parametrize(
    "node_type,prop",
    [("../../pwn", "salary"), ("Employee", "../../../../pwn"), ("a/b", "c/d"), ("ABSOLUTE", "salary")],
)
def test_the_column_spill_of_a_mapped_graph_stays_inside_its_spill_directory(node_type, prop, tmp_path, monkeypatch):
    sandbox = tmp_path / "sandbox"
    box = sandbox / "box"
    spill = box / "spill"
    spill.mkdir(parents=True)
    monkeypatch.setenv("KGLITE_TMPDIR", str(spill))
    node_type = _resolve(node_type, sandbox)
    before = _tree(sandbox)

    graph = kglite.KnowledgeGraph(storage="mapped")
    rows = 5000
    frame = pd.DataFrame({"id": list(range(rows)), "name": [f"e{i}" for i in range(rows)], prop: list(range(rows))})
    graph.add_nodes(frame, node_type, "id", "name")

    after = _tree(sandbox)
    assert _outside(before, after, (os.path.join("box", "spill"),)) == []
    assert any(p.startswith(os.path.join("box", "spill")) for p in after - before), "the spill did not run"
    total = graph.cypher("MATCH (n) RETURN count(n) AS n, sum(n.`" + prop + "`) AS s").to_list()[0]
    assert (total["n"], total["s"]) == (rows, sum(range(rows)))
