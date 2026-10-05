"""Regenerate the committed `tests/fixtures/temporal_bad_bounds/` graphs.

Each holds a declared type with rows its declaration rejects: a bound that is
not a date, or an inverted interval. Every current writer refuses such a row —
Cypher `SET`/`CREATE`, the loaders, and since 0.19.4 the fluent `update()` and
the `store_as=` writers — so only a graph an earlier version saved can hold
one, and the readers' handling of such rows is tested on these. The
**published 0.19.3 wheel**, whose `update()` wrote without judging, produces
them in an isolated interpreter; they are committed as binary, each scenario
as `<name>.kgl` (opened in memory or mapped mode) and `<name>.disk/` (a disk
graph). 0.18.x cannot write them: it has no `db.temporal.declare` and no
half-open convention.

Run this only to regenerate them, from outside the repository root, so the
local `kglite/` package cannot shadow the installed wheel:

    uv venv /tmp/v0193 --python 3.12
    uv pip install --python /tmp/v0193/bin/python 'kglite==0.19.3' pandas
    cd /tmp && /tmp/v0193/bin/python <repo>/tests/fixtures/build_temporal_bad_bounds_fixture.py
"""

from __future__ import annotations

import datetime as dt
from pathlib import Path
import shutil
import sys

import pandas as pd

import kglite

WHEEL = "0.19.3"
OUT = Path(__file__).resolve().parent / "temporal_bad_bounds"


def _plant(g, label, key, value, **props):
    return g.select(label, temporal=False).where({key: value}).update(props)["graph"]


def _status(g, convention="closed"):
    g.cypher(
        """
        UNWIND [
          {id: 1, vf: date('2000-01-01'), vt: date('2004-12-31')},
          {id: 2, vf: date('2005-01-01'), vt: null}
        ] AS r CREATE (:Status {id: r.id, vf: r.vf, vt: r.vt})
        """
    ).to_list()
    g.cypher(f"CALL db.temporal.declare({{node: 'Status', from: 'vf', to: 'vt', convention: '{convention}'}})")
    return g


def _m_codes(g):
    g.add_nodes(
        pd.DataFrame({"code": ["1", "2"], "vf": ["2000-01-01", "2001-01-01"], "vt": ["2005-01-01", None]}),
        "M",
        "code",
        column_types={"vf": "validFrom", "vt": "validTo"},
        convention="half_open",
    )
    return g


def _m_versions(g):
    g.cypher(
        "CREATE (:M {id: 1, name: 'a', vf: date('2000-01-01')}),"
        " (:M {id: 1, name: 'b', vf: date('2000-01-01')}),"
        " (:M {id: 1, name: 'c', vf: date('2030-01-01'), vt: date('2040-01-01')}),"
        " (:M {id: 363, name: 'old', vf: date('1900-01-01'), vt: date('1999-12-31')}),"
        " (:M {id: 363, name: 'new', vf: date('2000-01-01')}),"
        " (:M {id: 999, name: 'bad', vf: date('1900-01-01')})"
    ).to_list()
    g.cypher("CALL db.temporal.declare({node: 'M', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    return g


def _site(g):
    g.cypher("CREATE (:Site {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')})").to_list()
    g.cypher("CALL db.temporal.declare({node: 'Site', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    return g


def _statuses_and_t(g):
    g.cypher(
        """
        UNWIND [
          {id: 1, title: 'Producing', vf: '2000-01-01', vt: '2010-06-01'},
          {id: 2, title: 'Shut down', vf: '2010-06-01', vt: null}
        ] AS r CREATE (:Status {id: r.id, title: r.title, vf: r.vf, vt: r.vt})
        """
    ).to_list()
    g.cypher("CALL db.temporal.declare({node: 'Status', from: 'vf', to: 'vt', convention: 'half_open'})")
    g.cypher("CREATE (:T {id: 'bad', title: 'bad', vf: date('2015-01-01'), vt: date('2016-12-31')})")
    g.set_temporal("T", "vf", "vt")
    g = _plant(g, "Status", "id", 2, vt=20210101)
    return _plant(g, "T", "id", "bad", vf="someday")


#: name -> builder taking an empty graph and returning the planted one.
SCENARIOS = {
    "status_inverted": lambda g: _plant(_status(g), "Status", "id", 1, vt=dt.date(1990, 1, 1)),
    "status_inverted_unreadable": lambda g: _plant(
        _plant(_status(g), "Status", "id", 1, vt=dt.date(1990, 1, 1)), "Status", "id", 2, vt="someday"
    ),
    "status_inverted_int_from": lambda g: _plant(
        _plant(_status(g), "Status", "id", 1, vt=dt.date(1990, 1, 1)), "Status", "id", 2, vf=2005
    ),
    "status_same_day_datetimes": lambda g: _plant(
        _status(g), "Status", "id", 1, vf=dt.datetime(2003, 6, 30, 8), vt=dt.datetime(2003, 6, 30)
    ),
    "m_int_to": lambda g: _plant(_m_codes(g), "M", "code", "2", vt=20210101),
    "m_inverted": lambda g: _plant(_m_codes(g), "M", "code", "1", vt=dt.date(1990, 1, 1)),
    "m_versions_int_to": lambda g: _plant(_m_versions(g), "M", "id", 999, vt=42),
    "site_int_to": lambda g: _plant(_site(g), "Site", "id", 1, vt=42),
    "status_and_t": _statuses_and_t,
}


def main() -> None:
    if kglite.__version__ != WHEEL:
        sys.exit(f"refusing: these fixtures are written by kglite {WHEEL}, not {kglite.__version__}")
    if Path(kglite.__file__).resolve().is_relative_to(OUT.parents[1]):
        sys.exit("refusing: the repository's own kglite package shadows the wheel; run from outside it")
    shutil.rmtree(OUT, ignore_errors=True)
    OUT.mkdir()
    for name, build in SCENARIOS.items():
        build(kglite.KnowledgeGraph()).save(str(OUT / f"{name}.kgl"))
        disk = OUT / f"{name}.disk"
        scratch = OUT / f"{name}.scratch"
        # `update()` returns a detached handle; saving it writes the planted
        # graph as a new disk directory, leaving the scratch build behind.
        build(kglite.KnowledgeGraph(storage="disk", path=str(scratch))).save(str(disk))
        shutil.rmtree(scratch)
        (disk / ".kglite.lock").unlink(missing_ok=True)
        memory = kglite.load(str(OUT / f"{name}.kgl"))
        reopened = kglite.load(str(shutil.copytree(disk, OUT / f"{name}.check")))
        query = "FOR VALID_TIME ALL MATCH (n) RETURN labels(n) AS l, n.id AS id, n.vf AS vf, n.vt AS vt"
        assert sorted(map(str, memory.cypher(query).to_list())) == sorted(map(str, reopened.cypher(query).to_list())), (
            name
        )
        shutil.rmtree(OUT / f"{name}.check")
        print(name, memory.cypher(query).to_list())


if __name__ == "__main__":
    main()
