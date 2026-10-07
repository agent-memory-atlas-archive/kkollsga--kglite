"""The rows a graph holds that its declaration would refuse, counted at the
graph's current state: ``db.temporal.declarations()`` yields ``empty_rows``
(an inverted interval, or ``from == to`` under half-open — valid at no
instant) and ``unreadable_rows`` (a bound that is not NULL, a date, a
datetime or an ISO string), and ``describe()`` prints them when there are
any. Every writer refuses such rows
(``test_temporal_write_validation.py``, ``test_update_valid_time_check.py``);
a graph saved by an earlier version, which accepted them, can still hold them.
The tests open such graphs, written by the published 0.19.3 wheel
(``tests/fixtures/build_temporal_bad_bounds_fixture.py``).

The counts come from the per-version walk the endpoint index is built by, so
a write between two reads must show in the second: every write moves the
graph version, and nothing is served from the first read's walk. Inside one
statement the version moves only at commit, so the write engine drops the
cache after each writing clause.

Red proof: before the columns existed, yielding them failed as an unknown
column; the same-day inverted-datetime case returned the row.
"""

from __future__ import annotations

import pandas as pd
import pytest

import kglite
from tests.fixtures.compat_helpers import open_bad_bounds

MODES = [None, "mapped", "disk"]
MODE_IDS = ["memory", "mapped", "disk"]

COUNTS = (
    "CALL db.temporal.declarations() YIELD name, empty_rows, unreadable_rows RETURN name, empty_rows, unreadable_rows"
)


def _graph(storage, tmp_path) -> kglite.KnowledgeGraph:
    if storage == "disk":
        g = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    elif storage == "mapped":
        g = kglite.KnowledgeGraph(storage="mapped")
    else:
        g = kglite.KnowledgeGraph()
    g.cypher(
        """
        UNWIND [
          {id: 1, vf: date('2000-01-01'), vt: date('2004-12-31')},
          {id: 2, vf: date('2005-01-01'), vt: null}
        ] AS r CREATE (:Status {id: r.id, vf: r.vf, vt: r.vt})
        """
    ).to_list()
    g.cypher("CALL db.temporal.declare({node: 'Status', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
    return g


def _counts(g) -> list[dict]:
    return g.cypher(COUNTS).to_list()


def _legacy(name, storage, tmp_path) -> kglite.KnowledgeGraph:
    """``_graph`` with rows an earlier version wrote that every writer now refuses."""
    return open_bad_bounds(name, storage, tmp_path)


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_a_clean_declaration_counts_nothing(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    assert _counts(g) == [{"name": "Status", "empty_rows": 0, "unreadable_rows": 0}]
    assert "temporal_empty" not in g.describe()


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_each_write_between_two_reads_shows_in_the_second(storage, tmp_path) -> None:
    assert _counts(_graph(storage, tmp_path))[0]["empty_rows"] == 0
    assert _counts(_legacy("status_inverted", storage, tmp_path / "a")) == [
        {"name": "Status", "empty_rows": 1, "unreadable_rows": 0}
    ]
    g = _legacy("status_inverted_unreadable", storage, tmp_path / "b")
    assert _counts(g) == [{"name": "Status", "empty_rows": 1, "unreadable_rows": 1}]
    # A load and a Cypher write refuse the rows the counts report.
    with pytest.raises(kglite.ArgumentError, match="is after the to bound"):
        g.add_nodes(
            pd.DataFrame({"id": [4], "vf": pd.to_datetime(["2010-01-01"]), "vt": pd.to_datetime(["2009-01-01"])}),
            "Status",
            "id",
        )
    with pytest.raises(kglite.CypherExecutionError, match="someday"):
        g.cypher("CREATE (:Status {id: 3, vf: date('2006-01-01'), vt: 'someday'})").to_list()
    assert _counts(g) == [{"name": "Status", "empty_rows": 1, "unreadable_rows": 1}]
    g.cypher("MATCH (s:Status) WHERE s.id IN [1, 2] SET s.vt = null").to_list()
    assert _counts(g) == [{"name": "Status", "empty_rows": 0, "unreadable_rows": 0}]


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_a_write_shows_in_a_later_read_of_the_same_statement(storage, tmp_path) -> None:
    """The version moves only when the statement commits, so a read that
    filled the cache earlier in the statement must not answer a read after
    its write."""
    g = _legacy("status_inverted_unreadable", storage, tmp_path)
    rows = g.cypher(
        "CALL db.temporal.declarations() YIELD empty_rows AS before WITH before "
        "MATCH (s:Status {id: 1}) SET s.vt = null "
        "WITH before, count(*) AS written "
        "CALL db.temporal.declarations() YIELD empty_rows, unreadable_rows "
        "RETURN before, empty_rows, unreadable_rows"
    ).to_list()
    assert rows == [{"before": 1, "empty_rows": 0, "unreadable_rows": 1}]
    rows = g.cypher(
        "CALL db.temporal.declarations() YIELD unreadable_rows AS before WITH before "
        "MATCH (s:Status {id: 2}) SET s.vt = date('2006-01-01') "
        "WITH before, count(*) AS written CALL db.temporal.declarations() YIELD unreadable_rows "
        "RETURN before, unreadable_rows"
    ).to_list()
    assert rows == [{"before": 1, "unreadable_rows": 0}]
    assert _counts(g) == [{"name": "Status", "empty_rows": 0, "unreadable_rows": 0}]


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_describe_prints_the_counts_a_write_left(storage, tmp_path) -> None:
    g = _legacy("status_inverted_int_from", storage, tmp_path)
    assert 'temporal_empty="1" temporal_unreadable="1"' in g.describe()


def test_relationship_declarations_are_counted_per_source_type() -> None:
    g = kglite.KnowledgeGraph()
    g.cypher(
        """
        CREATE (f:Project {id: 1}), (l:Contract {id: 2}), (c:Company {id: 3}),
               (f)-[:HAS {ff: date('2000-01-01'), ft: date('2001-01-01')}]->(c),
               (l)-[:HAS {lf: date('2000-01-01'), lt: date('2001-01-01')}]->(c)
        """
    ).to_list()
    g.cypher(
        "CALL db.temporal.declare({relationship: 'HAS', source_type: 'Project', "
        "from: 'ff', to: 'ft', convention: 'closed'})"
    ).to_list()
    g.cypher("CALL db.temporal.declare({relationship: 'HAS', from: 'lf', to: 'lt', convention: 'closed'})").to_list()
    # The unkeyed declaration judges the Contract relationship's write.
    with pytest.raises(kglite.CypherExecutionError, match="HAS relationship from node '2' to node '3'"):
        g.cypher("MATCH (:Contract)-[r:HAS]->() SET r.lt = date('1999-01-01')").to_list()
    g.cypher("MATCH (:Project)-[r:HAS]->() SET r.lt = date('1999-01-01')").to_list()
    rows = g.cypher(
        "CALL db.temporal.declarations() YIELD source_type, empty_rows RETURN source_type, empty_rows"
    ).to_list()
    assert rows == [
        {"source_type": "Project", "empty_rows": 0},
        {"source_type": None, "empty_rows": 0},
    ]
    assert 'temporal="Project: ff..ft abutting=0; other sources: lf..lt abutting=0"' in g.describe()


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_an_inverted_pair_of_datetimes_on_one_day_is_valid_on_no_date(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    with pytest.raises(kglite.CypherExecutionError, match="node '1'.*is after the to bound"):
        g.cypher(
            "MATCH (s:Status {id: 1}) SET s.vf = datetime('2003-06-30T08:00:00'), "
            "s.vt = datetime('2003-06-30T00:00:00')"
        ).to_list()
    g = _legacy("status_same_day_datetimes", storage, tmp_path / "legacy")
    for instant in ("'2003-06-30'", "date('2003-06-30')", "datetime('2003-06-30T04:00:00')"):
        rows = g.cypher(f"MATCH (s:Status {{id: 1}}) WHERE valid_at(s, {instant}) RETURN s.id").to_list()
        assert rows == [], instant
    assert g.date("2003-06-30").select("Status").len() == 0
    assert _counts(g)[0]["empty_rows"] == 1
