"""A reader in one process keeps answering while another process saves past retention.

Generation pins are process-local, so the writer's second save prunes the
generation the reader still maps. The reader must give the answers it gave
before (scans, id and title lookups, edge walks), and its own save must now be
refused as stale rather than publish over the newer generations.
"""

from __future__ import annotations

import subprocess
import sys

import pandas as pd
import pytest

import kglite

WRITER = r"""
import sys, kglite
root = sys.argv[1]
for k in range(2):
    g = kglite.load(root)
    g.cypher("MATCH (o:Department {id: %d}) SET o.name = 'w' + toString(%d)" % (k + 1, k))
    g.cypher("CREATE (:Person {id: %d, name: 'late', grade: 0})" % (90000 + k))
    g.save()
    del g
"""


def _frames(n):
    ids = range(1, n + 1)
    people = pd.DataFrame({"id": ids, "name": [f"p{i}" for i in ids], "grade": [i % 7 for i in ids]})
    depts = pd.DataFrame({"id": range(1, 21), "name": [f"d{i}" for i in range(1, 21)]})
    edges = pd.DataFrame({"e": ids, "d": [(i % 20) + 1 for i in ids], "w": ids})
    return people, depts, edges


def _load_into(g, frames):
    people, depts, edges = frames
    g.add_nodes(people, "Person", "id", "name")
    g.add_nodes(depts, "Department", "id", "name")
    g.add_relationships(edges, "WORKS_IN", "Person", "e", "Department", "d")


def _answers(g):
    def rows(q):
        return g.cypher(q).to_list()

    return {
        "count": rows("MATCH (n) RETURN labels(n)[0] AS l, count(*) AS c ORDER BY l"),
        "id": rows("MATCH (e:Person {id: 7}) RETURN e.name AS n, e.grade AS g"),
        "title": rows("MATCH (n {title: 'p9'}) RETURN n.id AS id"),
        "title_dept": rows("MATCH (n {title: 'd3'}) RETURN n.id AS id"),
        "prefix": rows("MATCH (n:Person) WHERE n.title STARTS WITH 'p1' RETURN count(*) AS c"),
        "edges": rows("MATCH (e:Person)-[r:WORKS_IN]->(d:Department) RETURN count(*) AS c, sum(r.w) AS s"),
        "grade": rows("MATCH (e:Person) WHERE e.grade = 3 RETURN count(*) AS c"),
        "dept": rows("MATCH (o:Department) RETURN o.id AS id, o.name AS n ORDER BY id LIMIT 3"),
    }


def test_reader_answers_survive_retention_and_its_save_is_refused(tmp_path):
    root = tmp_path / "graph"
    frames = _frames(3000)
    g = kglite.KnowledgeGraph(storage="disk", path=str(root))
    _load_into(g, frames)
    g.save()
    del g

    reader = kglite.load(str(root))
    reader_generation = (root / "CURRENT").read_text().strip()
    _answers(reader)["count"]

    subprocess.run([sys.executable, "-c", WRITER, str(root)], check=True, timeout=120)
    assert not (root / "generations" / reader_generation).exists(), "retention did not prune the reader's generation"

    oracle = kglite.KnowledgeGraph()
    _load_into(oracle, frames)
    expected = _answers(oracle)
    got = _answers(reader)
    assert got == expected

    reader.cypher("MATCH (o:Department {id: 5}) SET o.name = 'reader'")
    with pytest.raises(ValueError, match="saved by another process"):
        reader.save()
    again = kglite.load(str(root))
    late = again.cypher("MATCH (e:Person) WHERE e.id >= 90000 RETURN count(*) AS c").scalar()
    assert late == 2
