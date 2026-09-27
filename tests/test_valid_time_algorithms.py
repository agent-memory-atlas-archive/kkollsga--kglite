"""Graph algorithms under a valid-time context run on the valid slice, and
the nodes they yield are the graph's own; a frozen view counts only what is
visible at its instant.

The reference is a separate graph holding only the elements valid at the
instant. Every algorithm under ``FOR VALID_TIME AS OF`` on the full graph, and
unprefixed on ``freeze(valid_at=…)``, must answer as it does on the reference,
compared per user id.
"""

from __future__ import annotations

import datetime as dt
import math

import pytest

import kglite

T = dt.date(2007, 6, 30)
AS_OF = "FOR VALID_TIME AS OF $t "
PERIODS = [
    (dt.date(2000, 1, 1), dt.date(2004, 12, 31)),
    (dt.date(2005, 1, 1), dt.date(2009, 12, 31)),
    (dt.date(2010, 1, 1), None),
]
VALID = 1
CLIQUE = 5
CLIQUES = 4


def _elements():
    """Per period, ``CLIQUES`` cliques of ``CLIQUE`` versions joined in a ring
    by one link each, from a clique's last member to the next one's second.
    Version ``vid = 3 * entity + period``; links join versions of one period.
    The period-1 link leaving clique 0 ended in 2006 (model B: its endpoints
    stay valid, the link does not), and each clique's first member carries the
    declared label ``Hub``, whose interval ended in 2006."""
    nodes, links = [], []
    entities = CLIQUE * CLIQUES
    for entity in range(entities):
        for period in range(3):
            nodes.append({"vid": 3 * entity + period, "period": period, "hub": entity % CLIQUE == 0})
    for period in range(3):
        for clique in range(CLIQUES):
            members = [3 * (clique * CLIQUE + i) + period for i in range(CLIQUE)]
            for i, a in enumerate(members):
                for b in members[i + 1 :]:
                    links.append({"s": a, "t": b, "period": period, "ended": False})
            nxt = 3 * (((clique + 1) % CLIQUES) * CLIQUE + 1) + period
            links.append({"s": members[-1], "t": nxt, "period": period, "ended": period == VALID and clique == 0})
    return nodes, links


def _write(graph, nodes, links, declare):
    graph.cypher(
        "UNWIND $rows AS r CREATE (:V {id: r.vid, vid: r.vid, vf: r.vf, vt: r.vt})",
        params={
            "rows": [{"vid": n["vid"], "vf": PERIODS[n["period"]][0], "vt": PERIODS[n["period"]][1]} for n in nodes]
        },
    ).to_list()
    hubs = [n["vid"] for n in nodes if n["hub"]]
    graph.cypher(
        "UNWIND $hubs AS h MATCH (v:V {vid: h}) SET v:Hub, v.hf = date('2000-01-01'), v.ht = $end",
        params={"hubs": hubs, "end": dt.date(2006, 1, 1) if declare else dt.date(2040, 1, 1)},
    ).to_list()
    rows = [
        {
            "s": link["s"],
            "t": link["t"],
            "lf": PERIODS[link["period"]][0],
            "lt": dt.date(2006, 1, 1) if link["ended"] else PERIODS[link["period"]][1],
        }
        for link in links
    ]
    graph.cypher(
        "UNWIND $rows AS r MATCH (a:V {vid: r.s}), (b:V {vid: r.t}) CREATE (a)-[:L {lf: r.lf, lt: r.lt}]->(b)",
        params={"rows": rows},
    ).to_list()
    if declare:
        # A type none of whose nodes is valid at T.
        graph.cypher("CREATE (:Old {id: 1, of: date('2000-01-01'), ot: date('2004-01-01')})").to_list()
        graph.set_temporal("Old", "of", "ot")
        graph.set_temporal("V", "vf", "vt")
        graph.cypher("CALL db.temporal.declare({node: 'Hub', from: 'hf', to: 'ht', convention: 'closed'})").to_list()
        graph.cypher(
            "CALL db.temporal.declare({relationship: 'L', from: 'lf', to: 'lt', convention: 'closed'})"
        ).to_list()
    return graph


def _valid(nodes, links):
    """The elements valid at T: period-1 versions that are not hubs, and the
    period-1 links between them that did not end in 2006."""
    kept = [n for n in nodes if n["period"] == VALID and not n["hub"]]
    ids = {n["vid"] for n in kept}
    return kept, [
        link
        for link in links
        if link["period"] == VALID and not link["ended"] and link["s"] in ids and link["t"] in ids
    ]


def _graph(storage, tmp_path):
    if storage == "disk":
        return kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "graph"))
    if storage == "mapped":
        return kglite.KnowledgeGraph(storage="mapped")
    return kglite.KnowledgeGraph()


@pytest.fixture(scope="module")
def elements():
    return _elements()


@pytest.fixture(scope="module")
def reference(elements):
    return _write(kglite.KnowledgeGraph(), *_valid(*elements), declare=False)


@pytest.fixture(params=["memory", "mapped", "disk"])
def full(request, elements, tmp_path):
    return _write(_graph(request.param, tmp_path), *elements, declare=True)


def _tiers(full, query):
    """The query's rows under the prefix and on a frozen view."""
    return [
        full.cypher(AS_OF + query, params={"t": T}).to_list(),
        full.freeze(valid_at=T).cypher(query).to_list(),
    ]


def _scores(rows):
    return {row["vid"]: row["s"] for row in rows}


@pytest.mark.parametrize(
    "proc",
    ["pagerank", "betweenness", "degree", "closeness", "k_core", "clustering_coefficient", "eccentricity"],
)
def test_per_node_algorithms_answer_as_on_the_reference_slice(full, reference, proc):
    column = {"k_core": "coreness", "clustering_coefficient": "coefficient", "eccentricity": "eccentricity"}.get(
        proc, "score"
    )
    query = f"CALL {proc}() YIELD node, {column} RETURN node.vid AS vid, {column} AS s"
    expected = _scores(reference.cypher(query).to_list())
    assert len(expected) == (CLIQUE - 1) * CLIQUES
    for rows in _tiers(full, query):
        got = _scores(rows)
        assert got.keys() == expected.keys(), proc
        for vid, value in expected.items():
            assert math.isclose(got[vid], value, rel_tol=0, abs_tol=1e-9), (proc, vid, got[vid], value)


@pytest.mark.parametrize("proc", ["connected_components", "louvain", "leiden", "label_propagation"])
def test_partitions_answer_as_on_the_reference_slice(full, reference, proc):
    column = "component" if proc == "connected_components" else "community"
    query = f"CALL {proc}() YIELD node, {column} RETURN coalesce(node.vid, -1) AS vid, {column} AS c"

    def partition(rows):
        groups = {}
        for row in rows:
            groups.setdefault(row["c"], set()).add(row["vid"])
        return sorted(sorted(g) for g in groups.values())

    expected = partition(reference.cypher(query).to_list())
    for rows in _tiers(full, query):
        assert partition(rows) == expected, proc
    if proc == "connected_components":
        # Clique 0's link out ended in 2006: the ring is a chain, still one
        # component; without the prefix the three periods are three rings
        # (and the Old node a fourth component).
        assert len(expected) == 1
        assert len(partition(full.cypher(query).to_list())) == 4


@pytest.mark.parametrize("proc", ["triangle_count", "diameter"])
def test_aggregate_algorithms_answer_as_on_the_reference_slice(full, reference, proc):
    query = f"CALL {proc}()"
    expected = reference.cypher(query).to_list()
    for rows in _tiers(full, query):
        assert rows == expected, proc


def test_a_routed_node_is_the_base_node(full):
    """`YIELD node` binds the full graph's node: its element id and a match
    from it read the full graph."""
    query = (
        "CALL pagerank() YIELD node, score MATCH (v:V) WHERE elementId(v) = elementId(node) "
        "OPTIONAL MATCH (node)-[:L]->(n) RETURN node.vid AS vid, v.vid AS same, count(n) AS out"
    )
    guarded = full.cypher(AS_OF + query, params={"t": T}).to_list()
    assert len(guarded) == (CLIQUE - 1) * CLIQUES
    assert all(row["vid"] == row["same"] for row in guarded)
    assert all(row["vid"] % 3 == VALID for row in guarded)


def test_a_procedure_outside_the_registry_is_refused(full):
    with pytest.raises(kglite.KgError, match="procedure orphan_node"):
        full.cypher(AS_OF + "CALL orphan_node() YIELD node RETURN node", params={"t": T}).to_list()


def test_a_write_between_routed_queries_changes_the_slice():
    graph = _write(kglite.KnowledgeGraph(), *_elements(), declare=True)
    count = AS_OF + "CALL degree() YIELD node RETURN count(*) AS n"
    before = graph.cypher(count, params={"t": T}).to_list()[0]["n"]
    graph.cypher("MATCH (v:V {vid: 4}) SET v.vt = date('2006-01-01')").to_list()
    assert graph.cypher(count, params={"t": T}).to_list()[0]["n"] == before - 1


def test_the_slice_cap_refuses_at_execution(elements, monkeypatch):
    graph = _write(kglite.KnowledgeGraph(), *elements, declare=True)
    monkeypatch.setenv("KGLITE_TEMPORAL_SLICE_MAX_BYTES", "1")
    with pytest.raises(kglite.KgError, match=r"CALL pagerank\(\) under FOR VALID_TIME AS OF: .*slice cap"):
        graph.cypher(AS_OF + "CALL pagerank() YIELD node RETURN node", params={"t": T}).to_list()


def test_disk_refuses_over_the_element_cap_and_over_the_mask_cap(elements, tmp_path, monkeypatch):
    graph = _write(_graph("disk", tmp_path), *elements, declare=True)
    query = AS_OF + "CALL pagerank() YIELD node RETURN count(*) AS n"
    # The mask is built (and cached) before the slice, so its cap goes first.
    monkeypatch.setenv("KGLITE_TEMPORAL_DISK_MASK_MAX_BYTES", "1")
    with pytest.raises(kglite.KgError, match="Disk mask cap"):
        graph.cypher(query, params={"t": T}).to_list()
    monkeypatch.delenv("KGLITE_TEMPORAL_DISK_MASK_MAX_BYTES")
    monkeypatch.setenv("KGLITE_TEMPORAL_DISK_SLICE_MAX_ELEMENTS", "3")
    with pytest.raises(kglite.KgError, match="Disk-mode slice cap"):
        graph.cypher(query, params={"t": T}).to_list()
    monkeypatch.delenv("KGLITE_TEMPORAL_DISK_SLICE_MAX_ELEMENTS")
    assert graph.cypher(query, params={"t": T}).to_list() == [{"n": (CLIQUE - 1) * CLIQUES}]


def test_a_frozen_view_counts_what_is_visible(full, reference):
    frozen = full.freeze(valid_at=T)
    assert frozen.node_count() == reference.freeze().node_count() == (CLIQUE - 1) * CLIQUES
    assert frozen.node_types == ["V"], "no Old node is valid at T"
    assert full.freeze().node_count() == len(_elements()[0]) + 1
    assert {"Old", "V"} <= set(full.freeze().node_types)
    assert "valid_at=date('2007-06-30')" in repr(frozen)


# ── Scope names are checked against the graph, not the slice ────────────────

_READY = "CALL ready_set({relationship: $rel, done: 'n.status = \"done\"'%s}) YIELD node RETURN node.id AS id"


def _tasks():
    """Two valid tasks joined by ``DEPENDS_ON``, and an ``Old`` node none of
    whose kind is valid at ``T``."""
    graph = kglite.KnowledgeGraph()
    graph.cypher(
        "CREATE (a:Task {id: 'A', status: 'todo', vf: date('2000-01-01')}), "
        "(b:Task {id: 'B', status: 'todo', vf: date('2000-01-01'), vt: date('2040-01-01')}), "
        "(a)-[:DEPENDS_ON]->(b), "
        "(:Old {id: 'o', of: date('1990-01-01'), ot: date('1995-01-01')})"
    ).to_list()
    graph.set_temporal("Task", "vf", "vt")
    graph.set_temporal("Old", "of", "ot")
    return graph


def _routed(graph, query, params):
    """Run ``query`` under the prefix and on a frozen view, returning each
    result (or the error it raised)."""
    out = []
    for run in (
        lambda: graph.cypher(AS_OF + query, params={**params, "t": T}),
        lambda: graph.freeze(valid_at=T).cypher(query, params=params),
    ):
        try:
            result = run()
            out.append((result.to_list(), list(result.warnings)))
        except kglite.KgError as error:
            out.append(error)
    return out


def test_a_routed_ready_set_refuses_an_unknown_relationship():
    """A typo'd relationship makes every node ready vacuously; the slice has
    no relationship metadata to catch it, so the base graph must."""
    graph = _tasks()
    with pytest.raises(kglite.KgError, match="unknown relationship type 'DEPENDS_O'"):
        graph.cypher(_READY % "", params={"rel": "DEPENDS_O"}).to_list()
    for outcome in _routed(graph, _READY % "", {"rel": "DEPENDS_O"}):
        assert isinstance(outcome, kglite.KgError), outcome
        assert "unknown relationship type 'DEPENDS_O'" in str(outcome)
    for rows, _ in _routed(graph, _READY % "", {"rel": "DEPENDS_ON"}):
        assert rows == [{"id": "B"}]


def test_a_routed_call_on_a_locked_schema_refuses_an_unknown_node_type():
    graph = _tasks()
    graph.lock_schema()
    query = _READY % ", node_type: 'Tsk'"
    with pytest.raises(kglite.KgError, match="unknown node type 'Tsk'"):
        graph.cypher(query, params={"rel": "DEPENDS_ON"}).to_list()
    for outcome in _routed(graph, query, {"rel": "DEPENDS_ON"}):
        assert isinstance(outcome, kglite.KgError), outcome
        assert "unknown node type 'Tsk'" in str(outcome)


def test_a_type_with_no_valid_node_is_not_reported_unknown():
    """``Old`` exists; that none of its nodes is valid at ``T`` makes it empty
    there, not unknown."""
    graph = _tasks()
    query = "CALL pagerank({node_type: 'Old'}) YIELD node RETURN node.id AS id"
    for outcome in _routed(graph, query, {}):
        assert not isinstance(outcome, Exception), outcome
        rows, warnings = outcome
        assert rows == []
        assert not any("unknown node type" in w for w in warnings), warnings
