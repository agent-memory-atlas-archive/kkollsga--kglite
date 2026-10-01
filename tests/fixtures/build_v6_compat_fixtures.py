"""Regenerate the committed 0.19.0 read-compatibility fixtures.

The register-scale program bumped the `.kgl` container to v7 and put a format
envelope on the disk-graph layout. v6 files and disk directories written by
kglite 0.19.0 are still *read*, because a persisted file outlives the binary
that wrote it. Read-compat asserted against files this tree wrote itself would
be circular, so these fixtures are produced by the **published 0.19.0 wheel**
in an isolated interpreter and committed as binary.

Run this only to regenerate them (a fixture that no longer loads is a finding,
not a regeneration prompt):

    uv venv /tmp/v6venv --python 3.14
    uv pip install --python /tmp/v6venv/bin/python 'kglite==0.19.0' pandas
    /tmp/v6venv/bin/python tests/fixtures/build_v6_compat_fixtures.py

The script refuses to run on anything but 0.19.0, and refuses to keep an
artifact that does not carry the 0.19.0 signature (a v6 container header; a
disk directory with no `disk_format` field and a bare-array `columns_meta`),
so a regeneration under the wrong interpreter fails loudly instead of quietly
re-pinning the current format against itself.

What it writes under `tests/fixtures/kgl_v6/`:

* `graph.kgl` + `graph.expected.json` — a small register-shaped graph: a
  versioned type with int64 ids, an integer title, a declared valid-time
  interval, typed and Mixed timestamp columns and an Int32 column; an anchor
  type, `OF` edges carrying properties, and a string-keyed type.
* `durable/` + `durable.expected.json` — a durable session that checkpointed
  and then took more writes, killed with `os._exit` so the write-ahead log
  still carries un-checkpointed frames.
* `disk/` + `disk.expected.json` — the same content as `graph.kgl`, built with
  `storage="disk"` and saved twice, so the directory has generations.
* `disk_int_title/` + `disk_int_title.expected.json` — a disk directory whose
  only type has an integer title and no `Mixed` column, which 0.19.0 kept in a
  per-type zstd sidecar (a non-string title forced one) and a later build moves
  into its per-type column file.

Name any of `graph`, `durable`, `disk`, `int-title` on the command line to
regenerate only those (for example `python build_v6_compat_fixtures.py graph
disk`); with none, all four are written.
"""

from __future__ import annotations

import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap

FIXTURE_DIR = Path(__file__).resolve().parent / "kgl_v6"
GENERATOR_VERSION = "0.19.0"
V6_HEADER = b"RGF\x06\x02"

#: Read back on both sides of every fixture. Ordered so the comparison is
#: positional and a reordering counts as a difference.
QUERIES = {
    "counts": "MATCH (p:Employment) RETURN count(p) AS versions",
    "anchors": "MATCH (o:Employee) RETURN count(o) AS anchors",
    "typed": (
        "MATCH (p:Employment) RETURN p.id AS id, p.title AS title, p.status AS status, "
        "p.hire_year AS hire_year, p.vf AS vf, p.vt AS vt, p.seen AS seen ORDER BY p.id"
    ),
    "mixed": "MATCH (p:Employment) RETURN p.id AS id, p.rec AS rec ORDER BY p.id",
    "as_of_2006": "FOR VALID_TIME AS OF datetime('2006-06-15T00:00:00') MATCH (p:Employment) RETURN count(*) AS c",
    "as_of_micros": (
        "FOR VALID_TIME AS OF datetime('2001-01-01T00:00:00.123456') MATCH (p:Employment) RETURN count(*) AS c"
    ),
    "as_of_2021": "FOR VALID_TIME AS OF datetime('2021-01-01T00:00:00') MATCH (p:Employment) RETURN count(*) AS c",
    "id_lookup": "MATCH (p:Employment {id: 3200000000005}) RETURN p.id AS id, p.title AS title, p.status AS status",
    "missing_id": "MATCH (p:Employment {id: 3200000009999}) RETURN count(p) AS c",
    "title_lookup": "MATCH (p:Employment) WHERE p.title = 3100000000002 RETURN p.id AS id ORDER BY p.id",
    "edges": (
        "MATCH (p:Employment)-[r:OF]->(o:Employee) RETURN p.id AS version, o.id AS obj, "
        "r.since AS since, r.role AS role ORDER BY p.id"
    ),
    "edge_count": "MATCH (:Employment)-[r:OF]->(:Employee) RETURN count(r) AS c",
    "tags": "MATCH (t:Tag) RETURN t.id AS id, t.title AS title, t.weight AS weight ORDER BY t.id",
    "units": (
        "MATCH (u:Unit) RETURN u.id AS id, u.title AS title, u.area AS area, u.floor AS floor, "
        "u.use AS use, u.active AS active ORDER BY u.id"
    ),
    "unit_by_id": "MATCH (u:Unit {id: 3300000000003}) RETURN u.title AS title, u.area AS area",
    "unit_edges": "MATCH (o:Employee)-[:HAS_UNIT]->(u:Unit) RETURN o.id AS obj, u.id AS unit ORDER BY u.id",
}

INT_TITLE_QUERIES = {
    "count": "MATCH (b:Badge) RETURN count(b) AS badges",
    "all": "MATCH (b:Badge) RETURN b.id AS id, b.title AS title, b.grade AS grade, b.active AS active ORDER BY b.id",
    "by_title": "MATCH (b:Badge) WHERE b.title = 7100000000003 RETURN b.id AS id",
    "by_id": "MATCH (b:Badge {id: 3400000000002}) RETURN b.title AS title, b.grade AS grade",
    "grade_sum": "MATCH (b:Badge) RETURN sum(b.grade) AS total",
}

DURABLE_QUERIES = {
    "events": "MATCH (e:Event) RETURN e.id AS id, e.kind AS kind, e.weight AS weight ORDER BY e.id",
    "counts": "MATCH (n:Event) RETURN count(n) AS events",
}


def _capture(graph, queries: dict[str, str]) -> dict[str, list]:
    """Query results as JSON-stable values (datetimes as their `str`)."""
    rows = {name: graph.cypher(query).to_list() for name, query in queries.items()}
    return json.loads(json.dumps(rows, default=str, sort_keys=True))


def _populate(graph, save) -> None:
    """Build the fixture content; `save()` is called twice so a disk directory
    ends with two generations."""
    import pandas as pd

    import kglite  # noqa: F401  (the version check ran in main)

    n = 8
    ident = [3100000000001 + i // 2 for i in range(n)]  # two versions per object
    ends = pd.Series(
        [
            "2003-03-03 00:00:00",
            None,
            "2008-08-08 12:00:00.250000",
            None,
            "2015-01-01 00:00:00",
            None,
            None,
            "2019-09-09 09:09:09.999999",
        ]
    )
    starts = pd.Series(
        [
            "2001-01-01 00:00:00.123456",
            "2003-03-03 00:00:00",
            "2004-04-04 04:04:04",
            "2008-08-08 12:00:00.250000",
            "2010-10-10 00:00:00",
            "2015-01-01 00:00:00",
            "2012-12-12 00:00:00",
            "2016-06-06 06:06:06",
        ]
    )
    seen = pd.Series([None, "2005-05-05 01:02:03.500000", "2010-10-10 00:00:00", None] * 2)

    def ts(series):
        return pd.to_datetime(series, format="ISO8601").astype("datetime64[us]")

    versions = pd.DataFrame(
        {
            "id": [3200000000001 + i for i in range(n)],
            "ident": ident,
            "vf": ts(starts),
            "vt": ts(ends),
            "seen": ts(seen),
            "status": ["active", "active", "terminated", "active", "offer_made", "active", "active", "terminated"],
            "hire_year": pd.Series([1998, 2004, 1650, 2011, 2020, 1987, 1999, 2016], dtype="int32"),
        }
    )
    anchors = pd.DataFrame({"id": sorted(set(ident))})
    edges = pd.DataFrame(
        {
            "id": versions["id"],
            "ident": versions["ident"],
            "since": [2001, 2003, 2004, 2008, 2010, 2015, 2012, 2016],
            "role": ["a", "b", "a", "b", "a", "b", "a", "b"],
        }
    )
    tags = pd.DataFrame({"id": ["t-alpha", "t-beta"], "title": ["Alpha", "Beta"], "weight": [1.5, -0.25]})

    graph.add_nodes(versions, "Employment", "id", "ident")
    graph.set_temporal("Employment", "vf", "vt", convention="half_open")
    graph.add_nodes(anchors, "Employee", "id")
    graph.add_relationships(edges, "OF", "Employment", "id", "Employee", "ident", columns=["since", "role"])
    graph.add_nodes(tags, "Tag", "id", "title")
    # No timestamp, Mixed column or integer title here: this is a type 0.19.0's
    # disk layout keeps in its mmap `columns.bin` + `columns_meta` pair (`Employment`
    # goes to a per-type sidecar for all three reasons).
    units = pd.DataFrame(
        {
            "id": [3300000000001, 3300000000002, 3300000000003, 3300000000004],
            "name": ["unit-a", "unit-b", "unit-c", "unit-d"],
            "area": [54.5, 61.0, 120.25, 33.0],
            "floor": [0, 1, 2, -1],
            "use": ["residential", "office", "residential", "storage"],
            "active": [True, True, False, True],
        }
    )
    graph.add_nodes(units, "Unit", "id", "name")
    graph.add_relationships(
        pd.DataFrame({"obj": sorted(set(ident))[: len(units)], "unit": units["id"]}),
        "HAS_UNIT",
        "Employee",
        "obj",
        "Unit",
        "unit",
    )
    # A Mixed column: timestamps beside a string in one property.
    graph.cypher("MATCH (p:Employment {id: 3200000000001}) SET p.rec = datetime('2010-01-01T00:00:00.500000')")
    graph.cypher("MATCH (p:Employment {id: 3200000000002}) SET p.rec = 'legacy'")
    graph.cypher("MATCH (p:Employment {id: 3200000000003}) SET p.rec = datetime('2011-02-03T04:05:06')")
    save()

    # Second save: one more logical change, so a disk directory carries a
    # second generation and the first is retained beside it.
    graph.cypher("MATCH (p:Employment {id: 3200000000004}) SET p.status = 'promoted'")
    graph.cypher("MATCH (t:Tag {id: 't-beta'}) SET t.weight = 2.75")
    save()


def _write_plain_fixture() -> None:
    import kglite

    graph = kglite.KnowledgeGraph()
    target = FIXTURE_DIR / "graph.kgl"
    _populate(graph, lambda: graph.save(str(target)))

    header = target.read_bytes()[:5]
    if header != V6_HEADER:
        target.unlink()
        raise SystemExit(f"refusing to commit a non-v6 checkpoint: header is {header!r}")

    (FIXTURE_DIR / "graph.expected.json").write_text(
        json.dumps(_capture(graph, QUERIES), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(f"wrote {target} ({target.stat().st_size} bytes)")


def _write_disk_fixture() -> None:
    import kglite

    target = FIXTURE_DIR / "disk"
    if target.exists():
        shutil.rmtree(target)
    graph = kglite.KnowledgeGraph(storage="disk", path=str(target))
    _populate(graph, graph.save)
    expected = _capture(graph, QUERIES)
    del graph

    metas = sorted(target.rglob("columns_meta.json"))
    if not metas:
        raise SystemExit("the disk fixture has no columns_meta.json — not a 0.19.0 layout")
    for meta in metas:
        if not isinstance(json.loads(meta.read_text(encoding="utf-8")), list):
            raise SystemExit(f"{meta} is not a bare array — this is not a 0.19.0-written directory")
    for meta in target.rglob("disk_graph_meta.json"):
        if "disk_format" in json.loads(meta.read_text(encoding="utf-8")):
            raise SystemExit(f"{meta} already carries disk_format — this is not a 0.19.0-written directory")
    generations = sorted(p.name for p in (target / "generations").iterdir())
    if len(generations) < 2:
        raise SystemExit(f"expected two generations, found {generations or 'none'}")
    covered = {entry["type_name"] for meta in metas for entry in json.loads(meta.read_text(encoding="utf-8"))}
    if "Unit" not in covered:
        raise SystemExit(f"columns_meta covers {sorted(covered)}; the fixture needs the Unit type in it")

    # Reopening the directory consumes nothing, but capture from a reload so the
    # committed expectation is what a *reader* sees, not what the writer held.
    scratch = Path(tempfile.mkdtemp()) / "disk"
    shutil.copytree(target, scratch)
    reloaded = kglite.load(str(scratch))
    if _capture(reloaded, QUERIES) != expected:
        raise SystemExit("the reloaded disk graph answers differently from the graph that wrote it")
    del reloaded

    for stray in list(target.rglob("*.lock-owner")) + list(target.rglob("LOCK")):
        stray.unlink()
    (FIXTURE_DIR / "disk.expected.json").write_text(
        json.dumps(expected, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    total = sum(p.stat().st_size for p in target.rglob("*") if p.is_file())
    print(f"wrote {target}/ ({total} bytes, generations {generations})")


def _write_int_title_disk_fixture() -> None:
    import pandas as pd

    import kglite

    target = FIXTURE_DIR / "disk_int_title"
    if target.exists():
        shutil.rmtree(target)
    graph = kglite.KnowledgeGraph(storage="disk", path=str(target))
    badges = pd.DataFrame(
        {
            "id": [3400000000001 + i for i in range(5)],
            "badge": [7100000000001 + i for i in range(5)],
            "grade": [3, 7, -2, 11, 0],
            "active": [True, True, False, True, False],
        }
    )
    graph.add_nodes(badges, "Badge", "id", "badge")
    graph.save()
    graph.cypher("MATCH (b:Badge {id: 3400000000002}) SET b.grade = 8")
    graph.save()
    expected = _capture(graph, INT_TITLE_QUERIES)
    del graph

    if list(target.rglob("columns_meta.json")):
        raise SystemExit("the int-title fixture has a columns_meta — 0.19.0 should have sidecar'd it")
    sidecars = sorted(target.rglob("columns.zst"))
    if not sidecars:
        raise SystemExit("the int-title type is not in a per-type sidecar — not the 0.19.0 layout")
    for meta in target.rglob("disk_graph_meta.json"):
        if "disk_format" in json.loads(meta.read_text(encoding="utf-8")):
            raise SystemExit(f"{meta} already carries disk_format — this is not a 0.19.0-written directory")

    scratch = Path(tempfile.mkdtemp()) / "disk_int_title"
    shutil.copytree(target, scratch)
    reloaded = kglite.load(str(scratch))
    if _capture(reloaded, INT_TITLE_QUERIES) != expected:
        raise SystemExit("the reloaded int-title graph answers differently from the graph that wrote it")
    del reloaded
    # `_pending_edges.bin` is the 16 MiB sparse edge-buffer scratch a graph with no
    # edges leaves at the root; the loader never reads it.
    for stray in (
        list(target.rglob("*.lock-owner")) + list(target.rglob("LOCK")) + list(target.rglob("_pending_edges.bin"))
    ):
        stray.unlink()
    (FIXTURE_DIR / "disk_int_title.expected.json").write_text(
        json.dumps(expected, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    total = sum(p.stat().st_size for p in target.rglob("*") if p.is_file())
    print(f"wrote {target}/ ({total} bytes, sidecars {[str(p.relative_to(target)) for p in sidecars]})")


#: Runs in a child that is killed with `os._exit`, so no Drop, no clean close,
#: and whatever the log holds past the checkpoint stays there.
DURABLE_CHILD = textwrap.dedent(
    """
    import kglite, os
    assert kglite.__version__ == {version!r}, kglite.__version__ + " at " + kglite.__file__
    path = {path!r}
    g = kglite.open(path, durable=True)
    for i in range(6):
        g.cypher("CREATE (:Event {{id: %d, kind: 'checkpointed', weight: %f}})" % (i, i * 1.5))
    g.save(path)                      # checkpoint: these six are in the .kgl
    for i in range(6, 11):
        g.cypher("CREATE (:Event {{id: %d, kind: 'logged', weight: %f}})" % (i, i * 1.5))
    os._exit(0)                       # the last five exist only in the log
    """
)


def _write_durable_fixture() -> None:
    import kglite

    target = FIXTURE_DIR / "durable"
    if target.exists():
        shutil.rmtree(target)
    target.mkdir(parents=True)
    graph_path = target / "app.kgl"

    subprocess.run(
        [sys.executable, "-c", DURABLE_CHILD.format(path=str(graph_path), version=GENERATOR_VERSION)],
        check=True,
        # `python -c` puts the *current directory* first on `sys.path`, so a run
        # from the repo root would import the repo's own editable `kglite/` and
        # write the very format this fixture exists to predate.
        cwd=tempfile.gettempdir(),
    )

    header = graph_path.read_bytes()[:5]
    if header != V6_HEADER:
        raise SystemExit(f"refusing to commit a non-v6 checkpoint: header is {header!r}")

    # A lock-owner record names the pid that held the path; committing it would
    # make every later open contend with a process that has not existed since
    # 2026. Recovery does not need it.
    for stray in target.glob("*.lock-owner"):
        stray.unlink()

    sidecars = sorted(p.name for p in target.iterdir() if p.name != "app.kgl")
    if not sidecars:
        raise SystemExit("no write-ahead sidecar was left behind — fixture would prove nothing")
    log_bytes = sum((target / name).stat().st_size for name in sidecars)
    if log_bytes < 64:
        raise SystemExit(f"write-ahead sidecars total {log_bytes} bytes — too small to carry frames")

    # Capture the recovered state from a COPY: opening replays the log and
    # re-checkpoints, which would consume the very thing being committed.
    scratch = Path(tempfile.mkdtemp()) / "durable"
    shutil.copytree(target, scratch)
    recovered = kglite.open(str(scratch / "app.kgl"), durable=True)
    expectation = _capture(recovered, DURABLE_QUERIES)
    del recovered
    if expectation["counts"][0]["events"] != 11:
        raise SystemExit(f"recovery yielded {expectation['counts']} events, expected 11")

    (FIXTURE_DIR / "durable.expected.json").write_text(
        json.dumps(expectation, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(f"wrote {target}/ (checkpoint + {sidecars}, {log_bytes} log bytes)")


def main() -> None:
    import kglite

    if kglite.__version__ != GENERATOR_VERSION:
        raise SystemExit(
            f"these fixtures must be written by kglite {GENERATOR_VERSION}, "
            f"not {kglite.__version__} — see this file's docstring"
        )
    if Path(kglite.__file__).resolve().is_relative_to(Path.cwd().resolve() / "kglite"):
        raise SystemExit("the repo's own kglite package is shadowing the wheel; run from elsewhere")

    FIXTURE_DIR.mkdir(parents=True, exist_ok=True)
    parts = {
        "graph": _write_plain_fixture,
        "durable": _write_durable_fixture,
        "disk": _write_disk_fixture,
        "int-title": _write_int_title_disk_fixture,
    }
    chosen = sys.argv[1:] or list(parts)
    unknown = [name for name in chosen if name not in parts]
    if unknown:
        raise SystemExit(f"unknown fixture {unknown}; choose from {sorted(parts)}")
    for name in chosen:
        parts[name]()


if __name__ == "__main__":
    main()
