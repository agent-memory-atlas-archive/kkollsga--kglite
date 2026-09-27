"""The valid-time equivalence oracle, across storage modes.

A statement under ``FOR VALID_TIME AS OF t`` on the full graph must answer as
the same statement without the prefix does on a **reference slice**: a new
graph holding only the elements valid at ``t``. The slice is built from the
full graph's own Cypher ``valid_at`` — the validity evaluator, one call per declared
label a node carries (the four-argument form, so each call reads that
label's declaration and convention) — with a relationship kept when it is
valid and both endpoints are kept. It shares neither the endpoint index, the
masks nor the guard code. Rows are compared on user properties (``uid``,
``id(n)``), which a separately built graph keeps.

Modes: memory, mapped and disk, plus memory with the endpoint-index byte cap
at one byte, which leaves every target to the property guards Disk uses.
"""

from __future__ import annotations

import contextlib
import datetime as dt
import os
import tempfile

from hypothesis import HealthCheck, given, settings
from hypothesis import strategies as st
import pytest

import kglite

pytestmark = pytest.mark.parity

MODES = ("memory", "mapped", "disk", "memory_guards")
CAP_ENV = "KGLITE_TEMPORAL_INDEX_MAX_BYTES"
EXAMPLES = int(os.environ.get("KGLITE_VALID_TIME_ORACLE_EXAMPLES", "50"))

# Declarations: node labels by bound names, relationship types by source.
# Every bound property name is unique, so a four-argument valid_at names one
# declaration.
NODE_BOUNDS = {"A": ("a_from", "a_to", "closed"), "B": ("b_from", "b_to", "half_open")}
EDGE_BOUNDS = {
    ("R", None): ("r_from", "r_to", "closed"),
    ("S", "A"): ("sa_from", "sa_to", "half_open"),
    ("S", None): ("s_from", "s_to", "closed"),
}
DECLARATIONS = [
    "{node: 'A', from: 'a_from', to: 'a_to', convention: 'closed'}",
    "{node: 'B', from: 'b_from', to: 'b_to', convention: 'half_open'}",
    "{relationship: 'R', from: 'r_from', to: 'r_to', convention: 'closed'}",
    "{relationship: 'S', source_type: 'A', from: 'sa_from', to: 'sa_to', convention: 'half_open'}",
    "{relationship: 'S', from: 's_from', to: 's_to', convention: 'closed'}",
]

QUERIES = [
    "MATCH (n:A) RETURN n.uid AS u, id(n) AS i",
    "MATCH (n:B) RETURN n.uid AS u",
    "MATCH (n) RETURN n.uid AS u, id(n) AS i",
    "MATCH (a:A)-[r:R]->(b) RETURN a.uid AS a, b.uid AS b, r.eid AS e",
    "MATCH (a)-[:S]->() RETURN a.uid AS a",
    "MATCH (a)<-[r:S]-(b) RETURN a.uid AS a, r.eid AS e",
    "MATCH (a)-[:R]->(b)-[:S]->(c) RETURN a.uid AS a, b.uid AS b, c.uid AS c",
    "MATCH (a)-[r]-(b) RETURN a.uid AS a, b.uid AS b, r.eid AS e",
    "MATCH (n:B) RETURN count(*) AS c",
    "MATCH (n) RETURN count(n) AS c",
    "MATCH (n:A|C) RETURN count(n) AS c",
    "MATCH ()-[r]->() RETURN count(r) AS c",
    "MATCH ()-[r:S]->() RETURN count(*) AS c",
    "MATCH ()-[r:R]-() RETURN count(*) AS c",
    "MATCH (n) RETURN labels(n) AS l, count(*) AS c",
    "MATCH (n:A) RETURN n.uid AS u ORDER BY u DESC LIMIT 3",
    "MATCH (n) RETURN n.uid AS u, COUNT { (n)-[:R]->() } AS c",
    "MATCH (n) WHERE EXISTS { (n)<-[:S]-() } RETURN n.uid AS u",
    # Id seeks name the node they matched: among versions sharing an id, the
    # latest-inserted valid one, which is the reference slice's own choice.
    "MATCH (n:A {id: 1}) RETURN n.uid AS u",
    "MATCH (n {id: 2}) RETURN n.uid AS u",
    "UNWIND [0, 1, 2, 3] AS x MATCH (n:B {id: x}) RETURN x, n.uid AS u",
    "MATCH (n:A) RETURN n.id AS i, count(*) AS c",
    "MATCH (a:A)-[:S]->(b) WITH a, count(b) AS c RETURN a.uid AS a, c",
    # Variable-length segments: the trail expansion, the distance frontier
    # (with the undirected closed-trail probe), a zero-hop segment and the
    # relationship list, and one inside EXISTS.
    "MATCH (a:A)-[:R*1..3]->(b) RETURN a.uid AS a, b.uid AS b",
    "MATCH (a)-[:R|S*1..2]-(b) RETURN DISTINCT a.uid AS a, b.uid AS b",
    "MATCH p = (a)-[*1..2]->(b) RETURN [n IN nodes(p) | n.uid] AS ns, [r IN relationships(p) | r.eid] AS rs",
    "MATCH (a:A)-[rs:S*0..2]->(b) RETURN a.uid AS a, b.uid AS b, [r IN rs | r.eid] AS rs",
    "MATCH (n) WHERE EXISTS { (n)-[:R*1..2]->() } RETURN n.uid AS u",
    # OPTIONAL MATCH, COUNT { } and pattern comprehensions.
    "MATCH (n) OPTIONAL MATCH (n)-[:R]->(m) RETURN n.uid AS n, m.uid AS m",
    "MATCH (n) RETURN n.uid AS u, COUNT { (n)-[:S]-() } AS c",
    "MATCH (n) RETURN n.uid AS u, size([(n)-->(m) | m.uid]) AS c",
    "MATCH (n:A) RETURN n.uid AS u, [p = (n)-[:R]->() | length(p)] AS ls",
    # The streaming pipeline over matched rows: grouped aggregates, DISTINCT
    # over a node and a value, WITH … WHERE, and the ORDER BY … LIMIT heap.
    "MATCH (a:A)-[:R*1..3]->(b) RETURN count(DISTINCT b) AS c",
    "MATCH (a)-[r:S]->(b) RETURN a.uid AS a, count(b) AS c, count(DISTINCT b.uid) AS d",
    "MATCH (a)-[:R|S]-(b) WITH a, count(*) AS c WHERE c > 1 RETURN a.uid AS a, c",
    "MATCH (a)-[:R]->(b) RETURN a.uid AS a, sum(b.uid) AS s ORDER BY a DESC LIMIT 3",
    "MATCH (a)-->(b) RETURN min(b.uid) AS lo, max(b.uid) AS hi, avg(b.uid) AS m",
    # shortestPath and allShortestPaths.
    "MATCH (a:A), (b:B) MATCH p = shortestPath((a)-[*]-(b)) RETURN a.uid AS a, b.uid AS b, length(p) AS l",
    "MATCH (a:A), (b) WHERE a <> b MATCH p = allShortestPaths((a)-[:R|S*]->(b)) "
    "RETURN a.uid AS a, b.uid AS b, [r IN relationships(p) | r.eid] AS rs",
]
ORDERED = {
    "MATCH (n:A) RETURN n.uid AS u ORDER BY u DESC LIMIT 3",
    "MATCH (a)-[:R]->(b) RETURN a.uid AS a, sum(b.uid) AS s ORDER BY a DESC LIMIT 3",
}

YEARS = list(range(2000, 2011))
BOUND = st.one_of(
    st.sampled_from(YEARS).map(lambda y: dt.date(y, 1, 1)),
    st.sampled_from(YEARS).map(lambda y: dt.datetime(y, 6, 30, 12, 0)),
)


@st.composite
def interval(draw, half_open: bool):
    start = draw(st.one_of(st.none(), BOUND))
    end = draw(st.one_of(st.none(), BOUND))
    if start is not None and end is not None:
        a, b = sorted([start, end], key=_day_key)
        if half_open and _day_key(a) >= _day_key(b):
            return (a, None)
        return (a, b)
    return (start, end)


def _day_key(value):
    return value if isinstance(value, dt.datetime) else dt.datetime.combine(value, dt.time())


@st.composite
def graphs(draw):
    nodes = []
    for _ in range(draw(st.integers(3, 9))):
        primary = draw(st.sampled_from("ABC"))
        secondary = draw(st.sampled_from([None] + [label for label in "ABC" if label != primary]))
        bounds = {}
        for label in {primary, secondary} & set(NODE_BOUNDS):
            frm, to, convention = NODE_BOUNDS[label]
            bounds[frm], bounds[to] = draw(interval(convention == "half_open"))
        nodes.append({"primary": primary, "secondary": secondary, "id": draw(st.integers(0, 3)), "bounds": bounds})
    edges = []
    for _ in range(draw(st.integers(0, 12))):
        src = draw(st.integers(0, len(nodes) - 1))
        dst = draw(st.integers(0, len(nodes) - 1))
        rel = draw(st.sampled_from("RST"))
        key = (rel, "A" if rel == "S" and nodes[src]["primary"] == "A" else None)
        bounds = {}
        if key in EDGE_BOUNDS:
            frm, to, convention = EDGE_BOUNDS[key]
            bounds[frm], bounds[to] = draw(interval(convention == "half_open"))
        edges.append({"src": src, "dst": dst, "type": rel, "bounds": bounds})
    day = draw(st.sampled_from(YEARS))
    shift = draw(st.sampled_from([-1, 0, 1, 180]))
    instant = dt.date(day, 1, 1) + dt.timedelta(days=shift)
    return nodes, edges, instant


# Sentinels: every declared bound exists on some element, so each declaration
# and each four-argument valid_at is accepted whatever the draw holds.
SENTINEL_NODES = [
    {
        "primary": "A",
        "secondary": None,
        "id": 9,
        "bounds": {"a_from": dt.date(2003, 1, 1), "a_to": dt.date(2007, 1, 1)},
    },
    {
        "primary": "B",
        "secondary": None,
        "id": 9,
        "bounds": {"b_from": dt.date(2001, 1, 1), "b_to": dt.date(2009, 1, 1)},
    },
]
SENTINEL_EDGES = [
    {"src": 0, "dst": 1, "type": "R", "bounds": {"r_from": dt.date(2002, 1, 1), "r_to": dt.date(2008, 1, 1)}},
    {"src": 0, "dst": 1, "type": "S", "bounds": {"sa_from": dt.date(2004, 1, 1), "sa_to": dt.date(2006, 1, 1)}},
    {"src": 1, "dst": 0, "type": "S", "bounds": {"s_from": dt.date(2000, 1, 1), "s_to": dt.date(2005, 1, 1)}},
]


def _literal(value) -> str:
    if value is None:
        return "null"
    if isinstance(value, dt.datetime):
        return f"datetime('{value.isoformat()}')"
    return f"date('{value.isoformat()}')"


def _props(values: dict) -> str:
    return ", ".join(f"{k}: {_literal(v) if isinstance(v, dt.date) or v is None else v}" for k, v in values.items())


def _write(graph, nodes, edges, declare: bool):
    """Nodes carry their `uid`, relationships their `eid` and endpoint uids.
    The nodes go in one statement: a later statement would meet the schema's
    typo guard for a bound its label's first node did not carry."""
    patterns = []
    for node in nodes:
        labels = node["primary"] + (f":{node['secondary']}" if node["secondary"] else "")
        props = {"uid": node["uid"], "id": node["id"], **{k: v for k, v in node["bounds"].items() if v is not None}}
        patterns.append(f"(:{labels} {{{_props(props)}}})")
    if patterns:
        graph.cypher("CREATE " + ", ".join(patterns)).to_list()
    for edge in edges:
        props = {"eid": edge["eid"], **{k: v for k, v in edge["bounds"].items() if v is not None}}
        graph.cypher(
            f"MATCH (a {{uid: {edge['src']}}}), (b {{uid: {edge['dst']}}}) "
            f"CREATE (a)-[:{edge['type']} {{{_props(props)}}}]->(b)"
        ).to_list()
    if declare:
        for declaration in DECLARATIONS:
            graph.cypher(f"CALL db.temporal.declare({declaration})").to_list()


@contextlib.contextmanager
def _graph(mode: str):
    with tempfile.TemporaryDirectory() as directory:
        old_cap = os.environ.get(CAP_ENV)
        if mode == "memory_guards":
            os.environ[CAP_ENV] = "1"
        try:
            if mode == "mapped":
                yield kglite.KnowledgeGraph(storage="mapped")
            elif mode == "disk":
                yield kglite.KnowledgeGraph(storage="disk", path=os.path.join(directory, "graph"))
            else:
                yield kglite.KnowledgeGraph()
        finally:
            if old_cap is None:
                os.environ.pop(CAP_ENV, None)
            else:
                os.environ[CAP_ENV] = old_cap


def _reference(full, nodes, edges, instant) -> kglite.KnowledgeGraph:
    """The slice the full graph's own `valid_at` keeps at `instant`."""
    params = {"t": instant}
    node_checks = " AND ".join(
        f"(NOT '{label}' IN labels(n) OR valid_at(n, $t, '{frm}', '{to}'))"
        for label, (frm, to, _) in NODE_BOUNDS.items()
    )
    kept = {
        row["u"] for row in full.cypher(f"MATCH (n) WHERE {node_checks} RETURN n.uid AS u", params=params).to_list()
    }
    edge_ok = (
        "CASE type(r) WHEN 'R' THEN valid_at(r, $t, 'r_from', 'r_to') "
        "WHEN 'S' THEN CASE WHEN labels(a)[0] = 'A' THEN valid_at(r, $t, 'sa_from', 'sa_to') "
        "ELSE valid_at(r, $t, 's_from', 's_to') END ELSE true END"
    )
    kept_edges = {
        row["e"]
        for row in full.cypher(f"MATCH (a)-[r]->(b) WHERE {edge_ok} RETURN r.eid AS e", params=params).to_list()
    }
    reference = kglite.KnowledgeGraph()
    slice_nodes = [node for node in nodes if node["uid"] in kept]
    slice_edges = [edge for edge in edges if edge["eid"] in kept_edges and edge["src"] in kept and edge["dst"] in kept]
    _write(reference, slice_nodes, slice_edges, declare=False)
    return reference


def _answer(graph, query, ordered, params=None):
    rows = graph.cypher(query, params=params or {}).to_list()
    values = [tuple(sorted((k, repr(v)) for k, v in row.items())) for row in rows]
    return values if ordered else sorted(values)


def _numbered(nodes, edges):
    """Sentinels first; every node gets its `uid`, every edge its `eid`."""
    nodes = [{**node, "uid": uid} for uid, node in enumerate(SENTINEL_NODES + nodes)]
    shifted = [{**e, "src": e["src"] + len(SENTINEL_NODES), "dst": e["dst"] + len(SENTINEL_NODES)} for e in edges]
    edges = [{**edge, "eid": eid} for eid, edge in enumerate(SENTINEL_EDGES + shifted)]
    return nodes, edges


def _check(mode, nodes, edges, instant):
    nodes, edges = _numbered(nodes, edges)
    with _graph(mode) as full:
        _write(full, nodes, edges, declare=True)
        reference = _reference(full, nodes, edges, instant)
        for query in QUERIES:
            ordered = query in ORDERED
            guarded = _answer(full, f"FOR VALID_TIME AS OF $t {query}", ordered, {"t": instant})
            assert guarded == _answer(reference, query, ordered), f"{mode} at {instant}: {query}"


@pytest.mark.parametrize("mode", MODES)
@settings(
    max_examples=EXAMPLES,
    deadline=None,
    suppress_health_check=[HealthCheck.too_slow, HealthCheck.data_too_large],
)
@given(case=graphs())
def test_valid_time_context_equals_the_reference_slice(mode, case):
    nodes, edges, instant = case
    _check(mode, nodes, edges, instant)


@pytest.mark.parametrize("mode", MODES)
def test_valid_time_golden_counts_anchor_the_oracle(mode):
    """A fixed draw with its answers written out, so the oracle cannot pass
    by both sides being empty: at 2004-06-01 the A sentinel is valid, the B
    sentinel is valid, node 2 (A, closed 2001–2003) is not, node 3 (B
    secondary on a C node, 2004–2010 half-open) is."""
    nodes = [
        {
            "primary": "A",
            "secondary": None,
            "id": 1,
            "bounds": {"a_from": dt.date(2001, 1, 1), "a_to": dt.date(2003, 1, 1)},
        },
        {
            "primary": "C",
            "secondary": "B",
            "id": 1,
            "bounds": {"b_from": dt.date(2004, 1, 1), "b_to": dt.date(2010, 1, 1)},
        },
    ]
    edges = [
        {"src": 0, "dst": 1, "type": "R", "bounds": {"r_from": dt.date(2000, 1, 1)}},
        {"src": 1, "dst": 0, "type": "S", "bounds": {"s_from": dt.date(2004, 1, 1)}},
        {"src": 1, "dst": 0, "type": "T", "bounds": {}},
    ]
    instant = dt.date(2004, 6, 1)
    _check(mode, nodes, edges, instant)
    with _graph(mode) as full:
        _write(full, *_numbered(nodes, edges), declare=True)
        count = "FOR VALID_TIME AS OF $t MATCH (n) RETURN count(n) AS c"
        assert full.cypher(count, params={"t": instant}).to_list() == [{"c": 3}]
        rels = "FOR VALID_TIME AS OF $t MATCH ()-[r]->() RETURN count(r) AS c"
        # The three sentinel relationships are valid and join valid nodes;
        # the drawn R, S and T each touch the invisible node 2.
        assert full.cypher(rels, params={"t": instant}).to_list() == [{"c": 3}]
        unguarded = "MATCH ()-[r]->() RETURN count(r) AS c"
        assert full.cypher(unguarded).to_list() == [{"c": 6}]


@pytest.mark.parametrize("mode", ("mapped", "disk", "memory_guards"))
def test_valid_time_bench_cells_answer_like_their_views(mode):
    """The temporal benchmark's cells at a small scale, the full graph in
    each mode against its in-memory view twin (the memory twin runs in the
    default suite, `test_temporal_cells_answer_like_their_views`)."""
    from tests.benchmarks import test_bench_temporal as bench

    scale = bench.Scale(260, 100, 600, 6_200, (6, 300), 20, 10)
    frames = bench._frames(scale)
    params = bench._params(scale)
    with _graph(mode) as graph:
        full = bench._load(frames, declared=True, kg=graph)
        view = bench._load(bench._view_frames(full, frames), declared=False)
        for name, cell in bench.CELLS.items():
            guarded = bench._rows(full, cell.context, params)
            assert guarded == bench._rows(view, cell.plain, params), f"{mode}: {name}"
            assert guarded != bench._rows(full, cell.plain, params), f"{mode}: {name} filters nothing"
