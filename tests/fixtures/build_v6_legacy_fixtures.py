"""Regenerate the committed 0.19.0 fixtures for the two layouts the main set lacks.

* `kgl_v6/ntriples_disk/` — a disk directory built by `load_ntriples` under
  0.19.0: the shared root `seg_000/columns.bin` with a bare-array
  `columns_meta.json` and its `columns_meta.bin.zst` twin, and a flat root
  with no `CURRENT` pointer.
* `kgl_v6/durable_kinds/` — a durable session that checkpointed and then took
  writes carrying timestamps, dates, integer ids beyond 32 bits, nulls and a
  mixed-kind property, killed with `os._exit` so the write-ahead log holds
  frames with those values.

Run only to regenerate (a fixture that stops loading is a finding):

    uv venv /tmp/v6venv --python 3.14
    uv pip install --python /tmp/v6venv/bin/python 'kglite==0.19.0' pandas
    /tmp/v6venv/bin/python tests/fixtures/build_v6_legacy_fixtures.py

The pinned answers are what 0.19.0 itself returned.
"""

# ruff: noqa: E501  (N-Triples lines and Cypher stay on one line)
from __future__ import annotations

import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

import kglite

OUT = Path(__file__).resolve().parent / "kgl_v6"

NT = """\
<http://www.wikidata.org/entity/Q1> <http://www.w3.org/2000/01/rdf-schema#label> "One"@en .
<http://www.wikidata.org/entity/Q1> <http://www.wikidata.org/prop/direct/P31> <http://www.wikidata.org/entity/Q5> .
<http://www.wikidata.org/entity/Q1> <http://www.wikidata.org/prop/direct/P569> "1950-01-01T00:00:00Z"^^<http://www.w3.org/2001/XMLSchema#dateTime> .
<http://www.wikidata.org/entity/Q1> <http://www.wikidata.org/prop/direct/P1082> "42"^^<http://www.w3.org/2001/XMLSchema#decimal> .
<http://www.wikidata.org/entity/Q2> <http://www.w3.org/2000/01/rdf-schema#label> "Two"@en .
<http://www.wikidata.org/entity/Q2> <http://www.wikidata.org/prop/direct/P31> <http://www.wikidata.org/entity/Q5> .
<http://www.wikidata.org/entity/Q2> <http://www.wikidata.org/prop/direct/P569> "1960-06-15T00:00:00Z"^^<http://www.w3.org/2001/XMLSchema#dateTime> .
<http://www.wikidata.org/entity/Q1> <http://www.wikidata.org/prop/direct/P26> <http://www.wikidata.org/entity/Q2> .
<http://www.wikidata.org/entity/Q3> <http://www.w3.org/2000/01/rdf-schema#label> "Oslo"@en .
<http://www.wikidata.org/entity/Q3> <http://www.wikidata.org/prop/direct/P31> <http://www.wikidata.org/entity/Q515> .
<http://www.wikidata.org/entity/Q1> <http://www.wikidata.org/prop/direct/P19> <http://www.wikidata.org/entity/Q3> .
"""

NTRIPLES_QUERIES = {
    "nodes": "MATCH (n) RETURN labels(n) AS l, n.id AS id, n.title AS title, n.P569 AS born, n.P1082 AS pop ORDER BY n.id",
    "edges": "MATCH (a)-[r]->(b) RETURN a.id AS a, type(r) AS t, b.id AS b ORDER BY a.id, t",
    "by_title": "MATCH (n {title: 'Two'}) RETURN n.id AS id",
}

DURABLE_QUERIES = {
    "events": (
        "MATCH (e:Ev) RETURN e.id AS id, e.kind AS kind, e.at AS at, e.d AS d, e.t AS t, e.mix AS mix ORDER BY e.id"
    ),
    "recent": "MATCH (e:Ev) WHERE e.at >= datetime('2011-01-01T00:00:00') RETURN e.id AS id ORDER BY e.id",
}

DURABLE_CHILD = """
import kglite, os
g = kglite.open({path!r}, durable=True)
for i in range(4):
    g.cypher("CREATE (:Ev {{id: %d, kind: 'cp', at: datetime('2010-01-0%dT00:00:00.%06d'), d: date('2020-0%d-01'), t: %d}})" % (i, i+1, i*7, i+1, 7000000000000+i))
g.save({path!r})
for i in range(4, 9):
    g.cypher("CREATE (:Ev {{id: %d, kind: 'wal', at: datetime('2011-01-0%dT01:02:03.%06d'), d: date('2021-0%d-02'), t: %d}})" % (i, i-3, i*11, i-3, 7000000000000+i))
g.cypher("MATCH (e:Ev {{id: 1}}) SET e.at = null, e.mix = date('1999-01-01')")
g.cypher("MATCH (e:Ev {{id: 2}}) SET e.mix = 'txt'")
os._exit(0)
"""


def _capture(graph, queries: dict[str, str]) -> dict:
    rows = {name: graph.cypher(query).to_list() for name, query in queries.items()}
    return json.loads(json.dumps(rows, default=str, sort_keys=True))


def main() -> None:
    assert kglite.__version__ == "0.19.0", f"run under the published 0.19.0 wheel, not {kglite.__version__}"
    work = Path(tempfile.mkdtemp())
    try:
        nt = work / "tiny.nt"
        nt.write_text(NT, encoding="utf-8")
        disk = work / "ntriples_disk"
        graph = kglite.KnowledgeGraph(storage="disk", path=str(disk))
        graph.load_ntriples(
            str(nt), node_types={"Q5": "Person", "Q515": "City"}, predicate_labels={"P26": "SPOUSE", "P19": "BORN_IN"}
        )
        expected = _capture(graph, NTRIPLES_QUERIES)
        del graph
        # A scratch file preallocated for edge staging; not part of the graph.
        (disk / "seg_000" / "_pending_edges.bin").unlink()
        target = OUT / "ntriples_disk"
        shutil.rmtree(target, ignore_errors=True)
        shutil.copytree(disk, target)
        (OUT / "ntriples_disk.expected.json").write_text(json.dumps(expected, indent=1, sort_keys=True) + "\n")

        durable = work / "durable_kinds"
        durable.mkdir()
        path = durable / "app.kgl"
        subprocess.run([sys.executable, "-c", DURABLE_CHILD.format(path=str(path))], check=True, cwd=work)
        target = OUT / "durable_kinds"
        shutil.rmtree(target, ignore_errors=True)
        shutil.copytree(durable, target)
        # What 0.19.0 itself recovers from the log, read from a scratch copy
        # because opening a durable directory replays and re-checkpoints it.
        scratch = work / "recovered"
        shutil.copytree(durable, scratch)
        graph = kglite.open(str(scratch / "app.kgl"), durable=True)
        recovered = _capture(graph, DURABLE_QUERIES)
        del graph
        (OUT / "durable_kinds.expected.json").write_text(json.dumps(recovered, indent=1, sort_keys=True) + "\n")
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
