"""An empty interval under ``half_open`` — ``from`` equal to ``to`` — is
stored, warned about once per write and counted in ``empty_rows``, and is
valid at no instant: no as-of route may return it, while a statement without
the context (lineage, as-known-at) still reads it.

Absolute expected values, per storage mode, on every route an as-of answer
can take: the Cypher context (``valid_at=`` and the ``FOR VALID_TIME AS OF``
prefix) over the endpoint index, over the property guards (memory with the
index byte cap at one byte, and Disk, which builds no endpoint index), the
``valid_at`` / ``valid_during`` functions, a frozen view and its slice, an
algorithm procedure under the context (Disk's instant mask), and the fluent
date context, ``valid_at`` / ``valid_during`` and ``traverse(at=)``.

Red proof: before the rule changed, every write of these rows was refused
("equals the to bound … an empty interval"), so none of them could be stored.
"""

from __future__ import annotations

import contextlib
import os
import tempfile
import warnings

import pandas as pd
import pytest

import kglite

pytestmark = pytest.mark.parity

MODES = ("memory", "mapped", "disk", "memory_guards")
CAP_ENV = "KGLITE_TEMPORAL_INDEX_MAX_BYTES"

# Status is declared half_open. 1 and 3 are ordinary versions; 2 is empty at
# date grain; 4 is empty across grains (a date start, a datetime end at that
# day's midnight). Day is declared closed: its from == to row is a one-day
# interval and stays valid. OP is declared half_open; its eid 2 is empty.
SETUP = [
    "CREATE (:Status {id: 1, vf: date('2000-01-01'), vt: date('2005-01-01')}), "
    "(:Status {id: 2, vf: date('2005-01-01'), vt: date('2005-01-01')}), "
    "(:Status {id: 3, vf: date('2005-01-01'), vt: null}), "
    "(:Status {id: 4, vf: date('2006-06-30'), vt: datetime('2006-06-30T00:00')}), "
    "(:Day {id: 5, vf: date('2005-01-01'), vt: date('2005-01-01')}), "
    "(:Co {id: 10}), (:Co {id: 11})",
    "MATCH (s:Status {id: 1}), (c:Co {id: 10}) "
    "CREATE (s)-[:OP {eid: 1, vf: date('2000-01-01'), vt: date('2005-01-01')}]->(c)",
    "MATCH (s:Status {id: 3}), (c:Co {id: 11}) "
    "CREATE (s)-[:OP {eid: 2, vf: date('2005-01-01'), vt: date('2005-01-01')}]->(c)",
    "MATCH (s:Status {id: 3}), (c:Co {id: 10}) CREATE (s)-[:OP {eid: 3, vf: date('2005-01-01'), vt: null}]->(c)",
]
DECLARE = [
    "CALL db.temporal.declare({node: 'Status', from: 'vf', to: 'vt', convention: 'half_open'})",
    "CALL db.temporal.declare({node: 'Day', from: 'vf', to: 'vt', convention: 'closed'})",
    "CALL db.temporal.declare({relationship: 'OP', from: 'vf', to: 'vt', convention: 'half_open'})",
]


@contextlib.contextmanager
def _graph(mode: str):
    with tempfile.TemporaryDirectory() as directory:
        old_cap = os.environ.get(CAP_ENV)
        if mode == "memory_guards":
            os.environ[CAP_ENV] = "1"
        try:
            if mode == "mapped":
                g = kglite.KnowledgeGraph(storage="mapped")
            elif mode == "disk":
                g = kglite.KnowledgeGraph(storage="disk", path=os.path.join(directory, "graph"))
            else:
                g = kglite.KnowledgeGraph()
            for query in SETUP:
                g.cypher(query).to_list()
            declared = [g.cypher(query) for query in DECLARE]
            yield g, [w for result in declared for w in result.warnings]
        finally:
            if old_cap is None:
                os.environ.pop(CAP_ENV, None)
            else:
                os.environ[CAP_ENV] = old_cap


def _ids(graph, query, **kwargs) -> list:
    return sorted(row["id"] for row in graph.cypher(query, **kwargs).to_list())


STATUS = "MATCH (s:Status) RETURN s.id AS id"
EDGES = "MATCH (:Status)-[r:OP]->(:Co) RETURN r.eid AS id"


@pytest.mark.parametrize("mode", MODES)
def test_the_declaration_accepts_warns_and_counts(mode) -> None:
    with _graph(mode) as (g, declared):
        empty = [w for w in declared if "empty interval" in w]
        assert len(empty) == 2, declared
        assert empty[0].startswith(
            "2 of 4 rows of node label 'Status' have an empty interval under convention 'half_open'"
        ), empty[0]
        assert "the first is node '2'" in empty[0], empty[0]
        assert empty[1].startswith("1 of 3 rows of relationship type 'OP'"), empty[1]
        counts = g.cypher("CALL db.temporal.declarations() YIELD name, empty_rows RETURN name, empty_rows").to_list()
        assert sorted((r["name"], r["empty_rows"]) for r in counts) == [("Day", 0), ("OP", 1), ("Status", 2)]


@pytest.mark.parametrize("mode", MODES)
def test_the_cypher_context_never_returns_an_empty_row(mode) -> None:
    with _graph(mode) as (g, declared):
        for t in ("2005-01-01", "2006-06-30"):
            assert _ids(g, STATUS, valid_at=t) == [3], t
            assert _ids(g, f"FOR VALID_TIME AS OF date('{t}') {STATUS}") == [3], t
        assert _ids(g, EDGES, valid_at="2005-01-01") == [3]
        assert _ids(g, "MATCH (s:Status)-[:OP]->(c:Co) RETURN c.id AS id", valid_at="2005-01-01") == [10]
        # closed: from == to is a valid day.
        assert _ids(g, "MATCH (d:Day) RETURN d.id AS id", valid_at="2005-01-01") == [5]
        assert _ids(g, "MATCH (n) WHERE n:Status OR n:Day RETURN n.id AS id", valid_at="2005-01-01") == [3, 5]


@pytest.mark.parametrize("mode", MODES)
def test_the_validity_functions_never_admit_an_empty_row(mode) -> None:
    with _graph(mode) as (g, declared):
        at = "MATCH (s:Status) WHERE valid_at(s, date('2005-01-01')) RETURN s.id AS id"
        assert _ids(g, at) == [3]
        during = "MATCH (s:Status) WHERE valid_during(s, date('2004-06-01'), date('2007-01-01')) RETURN s.id AS id"
        assert _ids(g, during) == [1, 3]
        edges = "MATCH ()-[r:OP]->() WHERE valid_at(r, date('2005-01-01')) RETURN r.eid AS id"
        assert _ids(g, edges) == [3]


@pytest.mark.parametrize("mode", MODES)
def test_a_frozen_view_its_slice_and_an_algorithm_never_hold_an_empty_row(mode) -> None:
    with _graph(mode) as (g, declared):
        frozen = g.freeze(valid_at="2005-01-01")
        assert _ids(frozen, STATUS) == [3]
        assert _ids(frozen, EDGES) == [3]
        assert _ids(frozen._valid_time_slice(), STATUS) == [3]
        # An algorithm under the context runs on the valid slice (Disk:
        # through its instant mask).
        algo = "CALL connected_components() YIELD node RETURN node.id AS id"
        assert _ids(g, algo, valid_at="2005-01-01") == [3, 5, 10, 11]
        assert _ids(g, algo, valid_at="2006-06-30") == [3, 10, 11]


@pytest.mark.parametrize("mode", MODES)
def test_the_fluent_filters_never_return_an_empty_row(mode) -> None:
    with _graph(mode) as (g, declared):

        def ids(kg) -> list:
            return sorted(n["id"] for n in kg.collect())

        assert ids(g.date("2005-01-01").select("Status")) == [3]
        everything = g.select("Status", temporal=False)
        assert ids(everything.valid_at("2005-01-01")) == [3]
        assert ids(everything.valid_at("2006-06-30")) == [3]
        assert ids(everything.valid_during("2004-06-01", "2007-01-01")) == [1, 3]
        three = g.select("Status", temporal=False).where({"id": 3})
        assert ids(three.traverse("OP", at="2005-01-01")) == [10]


@pytest.mark.parametrize("mode", MODES)
def test_a_statement_without_the_context_still_reads_an_empty_row(mode) -> None:
    with _graph(mode) as (g, declared):
        assert _ids(g, STATUS) == [1, 2, 3, 4]
        assert _ids(g, EDGES) == [1, 2, 3]
        assert sorted(n["id"] for n in g.select("Status", temporal=False).collect()) == [1, 2, 3, 4]


@pytest.mark.parametrize("mode", MODES)
def test_a_load_keeps_an_empty_row_with_one_warning(mode) -> None:
    with _graph(mode) as (g, declared):
        rows = pd.DataFrame(
            {
                "id": [6, 7, 8],
                "vf": pd.to_datetime(["2010-01-01", "2011-01-01", "2012-01-01"]),
                "vt": pd.to_datetime(["2010-01-01", None, "2012-01-01"]),
            }
        )
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            g.add_nodes(rows, "Status", "id")
        empty = [str(w.message) for w in caught if "empty interval" in str(w.message)]
        assert len(empty) == 1, [str(w.message) for w in caught]
        assert empty[0].startswith("2 of 3 rows written have an empty interval under convention 'half_open'")
        assert "the first is row 0 (0-based) of the load" in empty[0]
        assert _ids(g, STATUS) == [1, 2, 3, 4, 6, 7, 8]
        assert _ids(g, STATUS, valid_at="2012-01-01") == [3, 7]
        counts = g.cypher("CALL db.temporal.declarations() YIELD name, empty_rows RETURN name, empty_rows").to_list()
        assert ("Status", 4) in [(r["name"], r["empty_rows"]) for r in counts]
