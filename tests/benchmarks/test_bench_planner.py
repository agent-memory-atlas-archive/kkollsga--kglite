"""Cypher planner benchmarks.

Covers the 0.9.35 label-pair selectivity branch of `reorder_match_clauses`.
The fixture is intentionally label-skewed (one rare label + one common
one) so the new cost branch picks a meaningfully different driving side
than the old "sum of edge-type totals" proxy.

Numbers ride alongside the existing core benchmarks under
`make bench-save` / `make bench-compare`.
"""

import pandas as pd
import pytest

from kglite import KnowledgeGraph


def _build_skewed_graph(n_common: int = 5_000, n_rare: int = 50) -> KnowledgeGraph:
    """Two label classes (`Common`, `Rare`) joined by a shared `LINKS` edge type.

    Common: n_common nodes, all interconnected (n_common * 3 LINKS edges).
    Rare:   n_rare nodes, each linked to a single Common (n_rare LINKS edges).

    Result: total LINKS = n_common*3 + n_rare. Per-label-pair:
      (Common, LINKS, Common) = 3 * n_common
      (Rare,   LINKS, Common) = n_rare

    Querying `MATCH (a:Rare)-[:LINKS]->(b:Common)` should drive from Rare;
    with only the edge-type total, the planner sees `LINKS = ~15050` and
    can't distinguish.
    """
    kg = KnowledgeGraph()
    common = pd.DataFrame({"cid": list(range(n_common)), "name": [f"C{i}" for i in range(n_common)]})
    kg.add_nodes(common, "Common", "cid", "name")
    rare = pd.DataFrame({"rid": list(range(n_rare)), "name": [f"R{i}" for i in range(n_rare)]})
    kg.add_nodes(rare, "Rare", "rid", "name")

    # Common→Common edges (the bulk of the LINKS population)
    cc_src = []
    cc_tgt = []
    for i in range(n_common):
        for delta in (1, 7, 13):
            cc_src.append(i)
            cc_tgt.append((i + delta) % n_common)
    kg.add_connections(
        pd.DataFrame({"src": cc_src, "tgt": cc_tgt}),
        "LINKS",
        "Common",
        "src",
        "Common",
        "tgt",
    )

    # Rare→Common edges (the selective slice the planner should pick)
    rc = pd.DataFrame(
        {
            "src": list(range(n_rare)),
            "tgt": [(i * 91) % n_common for i in range(n_rare)],
        }
    )
    kg.add_connections(rc, "LINKS", "Rare", "src", "Common", "tgt")

    # Warm the planner caches so the first-bench-iteration cost isn't a
    # one-off O(E) scan. Mirrors how `make bench-compare` invokes
    # production queries that have already warmed these caches.
    kg.label_pair_counts()
    kg.cypher("MATCH (r:Rare)-[:LINKS]->(c:Common) RETURN count(*)").to_list()
    return kg


@pytest.fixture
def skewed_graph():
    return _build_skewed_graph()


@pytest.mark.benchmark
def test_bench_label_pair_counts_compute(benchmark):
    """Cold-cache compute of the label-pair triples. Captures the O(E)
    walk that the planner amortises into one-time cost per mutation
    epoch. Builds a fresh graph each round so the cache is always cold."""

    def setup():
        kg = _build_skewed_graph(n_common=2000, n_rare=20)
        # Invalidate so each timed iteration hits the cold path.
        kg.cypher("MATCH (a:Common {cid: 0}), (b:Common {cid: 1}) CREATE (a)-[:LINKS]->(b)").to_list()
        return (kg,), {}

    benchmark.pedantic(lambda kg: kg.label_pair_counts(), setup=setup, rounds=5, iterations=1)


@pytest.mark.benchmark
def test_bench_planner_two_match_with_skewed_labels(benchmark, skewed_graph):
    """Two MATCH clauses where one is selective on `Rare` and the other
    on `Common`. The new selectivity branch should drive from the Rare
    side. Measures end-to-end query latency with optimiser on."""
    g = skewed_graph
    benchmark(
        lambda: g.cypher(
            "MATCH (r:Rare {rid: 0})-[:LINKS]->(c:Common) "
            "MATCH (r2:Rare {rid: 1})-[:LINKS]->(c2:Common) "
            "RETURN c.name, c2.name"
        ).to_list()
    )


@pytest.mark.benchmark
def test_bench_label_pair_counts_warm_read(benchmark, skewed_graph):
    """Warm-cache read — must be O(triples), essentially free. If this
    is more than a few microseconds something has regressed in the
    Arc<RwLock<Option<...>>> read path."""
    g = skewed_graph
    benchmark(lambda: g.label_pair_counts())


# ---------------------------------------------------------------------------
# `x IS NULL OR x >= t` filters during matching
# ---------------------------------------------------------------------------
#
# The open-ended form must filter inside the pattern as its closed twin does.
# Each open cell ships its closed twin as the unchanged-path control; at
# NULL_OR_T the two return the same count, so their ratio isolates the NULL
# handling. NULL is an absent property (a second batch without `vt`).

NULL_OR_ENTITIES = 20_000
NULL_OR_VERSIONS = 5
NULL_OR_EDGES = 500_000
NULL_OR_T = 250
NULL_OR_ANCHORS = [(i * 97 + 3) % NULL_OR_ENTITIES for i in range(200)]


def _build_null_or_graph(entities: int, edges: int) -> KnowledgeGraph:
    """`entities * NULL_OR_VERSIONS` versioned nodes and `edges` versioned
    edges; the last version of each has no `vt`."""
    graph = KnowledgeGraph()
    n = entities * NULL_OR_VERSIONS
    rows = pd.DataFrame(
        {
            "nid": list(range(n)),
            "name": [f"E_{i}" for i in range(n)],
            "eid": [i // NULL_OR_VERSIONS for i in range(n)],
            "vf": [(i % NULL_OR_VERSIONS) * 100 for i in range(n)],
            "vt": [(i % NULL_OR_VERSIONS) * 100 + 99 for i in range(n)],
            "score": [float((i * 37) % 1000) for i in range(n)],
        }
    )
    is_open = rows["vf"] == (NULL_OR_VERSIONS - 1) * 100
    graph.add_nodes(rows[~is_open], "E", "nid", "name")
    graph.add_nodes(rows[is_open].drop(columns=["vt"]), "E", "nid", "name")
    graph.create_index("E", "eid")

    version = [i % NULL_OR_VERSIONS for i in range(edges)]
    links = pd.DataFrame(
        {
            "s": [i % n for i in range(edges)],
            # A fresh target per lap over the sources: repeated (s, t) pairs
            # would merge into one relationship.
            "t": [((i % n) * 7919 + 13 + (i // n) * 104_729) % n for i in range(edges)],
            "vf": [v * 100 for v in version],
            "vt": [v * 100 + 99 for v in version],
        }
    )
    open_links = links["vf"] == (NULL_OR_VERSIONS - 1) * 100
    graph.add_connections(links[~open_links], "R", "E", "s", "E", "t", columns=["vf", "vt"])
    graph.add_connections(links[open_links].drop(columns=["vt"]), "R", "E", "s", "E", "t", columns=["vf"])
    return graph


@pytest.fixture(scope="module")
def null_or_graph():
    return _build_null_or_graph(NULL_OR_ENTITIES, NULL_OR_EDGES)


_NULL_OR_NODE = "MATCH (a:E) WHERE {f} RETURN avg(a.score) AS s, count(*) AS c"
_NULL_OR_EDGE = "MATCH (a:E)-[r:R]->(b:E) WHERE a.eid IN $ids AND {f} RETURN count(*) AS c"


def _null_or_filter(var: str, open_ended: bool) -> str:
    if open_ended:
        return f"{var}.vf <= {NULL_OR_T} AND ({var}.vt IS NULL OR {var}.vt >= {NULL_OR_T})"
    return f"{var}.vf <= {NULL_OR_T} AND {var}.vt >= {NULL_OR_T}"


@pytest.mark.benchmark
def test_bench_null_or_node_scan_open(benchmark, null_or_graph):
    """Open-ended node filter over a 100k-node type."""
    query = _NULL_OR_NODE.format(f=_null_or_filter("a", True))
    result = benchmark(lambda: null_or_graph.cypher(query).to_list())
    assert result[0]["c"] > 0


@pytest.mark.benchmark
def test_bench_null_or_node_scan_closed(benchmark, null_or_graph):
    """Closed-form control for `null_or_node_scan_open`."""
    query = _NULL_OR_NODE.format(f=_null_or_filter("a", False))
    result = benchmark(lambda: null_or_graph.cypher(query).to_list())
    assert result[0]["c"] > 0


@pytest.mark.benchmark
def test_bench_null_or_anchored_edge_open(benchmark, null_or_graph):
    """Open-ended relationship filter behind a 200-entity anchor."""
    query = _NULL_OR_EDGE.format(f=_null_or_filter("r", True))
    result = benchmark(lambda: null_or_graph.cypher(query, params={"ids": NULL_OR_ANCHORS}).to_list())
    assert result[0]["c"] > 0


@pytest.mark.benchmark
def test_bench_null_or_anchored_edge_closed(benchmark, null_or_graph):
    """Closed-form control for `null_or_anchored_edge_open`."""
    query = _NULL_OR_EDGE.format(f=_null_or_filter("r", False))
    result = benchmark(lambda: null_or_graph.cypher(query, params={"ids": NULL_OR_ANCHORS}).to_list())
    assert result[0]["c"] > 0


def test_null_or_pairs_agree():
    """The open/closed pairs above compute one answer, and the NULL rows are
    real. Unmarked, so it runs in the default suite at a reduced scale."""
    entities = 2_000
    graph = _build_null_or_graph(entities, 50_000)
    anchors = [(i * 97 + 3) % entities for i in range(200)]
    for template, var, params in (
        (_NULL_OR_NODE, "a", None),
        (_NULL_OR_EDGE, "r", {"ids": anchors}),
    ):
        open_rows = graph.cypher(template.format(f=_null_or_filter(var, True)), params=params).to_list()
        closed_rows = graph.cypher(template.format(f=_null_or_filter(var, False)), params=params).to_list()
        assert open_rows == closed_rows
        assert open_rows[0]["c"] > 0
    nulls = graph.cypher("MATCH (a:E) WHERE a.vt IS NULL RETURN count(*) AS c").to_list()
    assert nulls == [{"c": entities}]
    open_edges = graph.cypher("MATCH ()-[r:R]->() WHERE r.vt IS NULL RETURN count(*) AS c").to_list()
    assert open_edges == [{"c": 50_000 // NULL_OR_VERSIONS}]


# ---------------------------------------------------------------------------
# The valid-time endpoint index
# ---------------------------------------------------------------------------
#
# Build: a write moves the graph version, and the next read of a declared
# type walks its rows again. `db.temporal.declarations()` reads the counts, so
# one round is one write plus one walk of VT_ROWS rows that also collects and
# sorts their endpoints. The twin sets the index byte cap to one byte: the
# same write and walk, counting only, so the pair's ratio isolates keeping the
# arrays.
#
# Resolve: a statement under FOR VALID_TIME AS OF resolves its filter — the
# segment of the instant and the mask it gives — and runs its guarded count.
# The rounds cycle through more instants (each its own segment) than
# the mask cache keeps, so every round builds a mask. The twin plans the same
# statements under EXPLAIN, which renders without resolving.

VT_ENTITIES = 4_000
VT_VERSIONS = 5
VT_START = pd.Timestamp("2000-01-01")
# Inside the first four versions' staggered start windows: every instant has
# a different count of started rows, so each is its own segment.
VT_INSTANTS = [
    (VT_START + pd.Timedelta(days=version * 730 + day)).date().isoformat()
    for version in range(4)
    for day in (50, 150, 250)
]
_VT_CAP_ENV = "KGLITE_TEMPORAL_INDEX_MAX_BYTES"


def _build_valid_time_graph() -> KnowledgeGraph:
    """VT_ENTITIES * VT_VERSIONS consecutive yearly versions, staggered by
    entity so the endpoints spread over many days."""
    graph = KnowledgeGraph()
    n = VT_ENTITIES * VT_VERSIONS
    vf = [VT_START + pd.Timedelta(days=(i % VT_VERSIONS) * 730 + (i // VT_VERSIONS) % 300) for i in range(n)]
    rows = pd.DataFrame(
        {
            "nid": list(range(n)),
            "name": [f"V_{i}" for i in range(n)],
            "vf": vf,
            "vt": [d + pd.Timedelta(days=729) for d in vf],
        }
    )
    graph.add_nodes(rows, "V", "nid", "name")
    graph.cypher("CREATE (:Marker {n: 0})").to_list()
    graph.cypher("CALL db.temporal.declare({node: 'V', from: 'vf', to: 'vt', convention: 'half_open'})").to_list()
    return graph


@pytest.fixture(scope="module")
def valid_time_graph():
    return _build_valid_time_graph()


def _write_then_count(graph: KnowledgeGraph) -> list:
    graph.cypher("MATCH (m:Marker) SET m.n = m.n + 1").to_list()
    return graph.cypher("CALL db.temporal.declarations() YIELD empty_rows RETURN empty_rows").to_list()


@pytest.mark.benchmark
def test_bench_valid_time_index_build(benchmark, valid_time_graph, monkeypatch):
    """One write, then the walk that rebuilds a 20k-row endpoint index."""
    monkeypatch.delenv(_VT_CAP_ENV, raising=False)
    result = benchmark(lambda: _write_then_count(valid_time_graph))
    assert result == [{"empty_rows": 0}]


@pytest.mark.benchmark
def test_bench_valid_time_index_build_counts_only(benchmark, valid_time_graph, monkeypatch):
    """Control for `valid_time_index_build`: the same walk, keeping nothing."""
    monkeypatch.setenv(_VT_CAP_ENV, "1")
    result = benchmark(lambda: _write_then_count(valid_time_graph))
    assert result == [{"empty_rows": 0}]


_VT_QUERY = "{prefix}FOR VALID_TIME AS OF date('{t}') MATCH (v:V) RETURN count(*) AS c"


def _cycle(graph: KnowledgeGraph, prefix: str):
    # A write first: an index the counts-only cell refused under its one-byte
    # cap stays refused for the rest of that graph version.
    graph.cypher("MATCH (m:Marker) SET m.n = m.n + 1").to_list()
    state = {"i": 0}

    def run():
        t = VT_INSTANTS[state["i"] % len(VT_INSTANTS)]
        state["i"] += 1
        return graph.cypher(_VT_QUERY.format(prefix=prefix, t=t)).to_list()

    return run


@pytest.mark.benchmark
def test_bench_valid_time_resolve_segment(benchmark, valid_time_graph, monkeypatch):
    """Plan, resolve (segment + a fresh mask) and run a context query: a
    guarded fused count, one bit test per node."""
    monkeypatch.delenv(_VT_CAP_ENV, raising=False)
    benchmark(_cycle(valid_time_graph, ""))


@pytest.mark.benchmark
def test_bench_valid_time_resolve_segment_explain(benchmark, valid_time_graph, monkeypatch):
    """Control for `valid_time_resolve_segment`: the same statements planned
    and rendered under EXPLAIN, resolving nothing."""
    monkeypatch.delenv(_VT_CAP_ENV, raising=False)
    result = benchmark(_cycle(valid_time_graph, "EXPLAIN "))
    assert result


# ---------------------------------------------------------------------------
# Id seeks among version nodes that share an id
# ---------------------------------------------------------------------------
#
# Every entity has two versions with one id: the old one (2000–2010) and the
# new one (from 2010), inserted last, so the id index holds the new one. At a
# past instant every seek's index hit is invisible, and the seek finds the
# old version through the type's duplicate-id map (built once per graph
# version, so a warm round measures the seeks). A seek must not walk the
# type: the check below compares the per-seek cost on a 20k-node and a
# 200k-node type.

SEEK_ENTITIES = 100_000  # two versions each: a 200k-node type
SEEK_IDS = 2_000
SEEK_QUERY = "FOR VALID_TIME AS OF date('2005-01-01') UNWIND $ids AS x MATCH (n:T {id: x}) RETURN n.name AS name"


def _build_seek_graph(entities: int, storage: str, path) -> KnowledgeGraph:
    if storage == "disk":
        graph = KnowledgeGraph(storage="disk", path=str(path))
    else:
        graph = KnowledgeGraph()
    graph.cypher(
        "UNWIND range(0, $n - 1) AS i CREATE (:T {id: i, name: 'old', vf: date('2000-01-01'), vt: date('2010-01-01')})",
        params={"n": entities},
    ).to_list()
    graph.cypher(
        "UNWIND range(0, $n - 1) AS i CREATE (:T {id: i, name: 'new', vf: date('2010-01-01')})",
        params={"n": entities},
    ).to_list()
    graph.cypher("CALL db.temporal.declare({node: 'T', from: 'vf', to: 'vt', convention: 'half_open'})").to_list()
    return graph


def _seek_ids(entities: int) -> list[int]:
    step = entities // SEEK_IDS
    return list(range(0, step * SEEK_IDS, step))


def _seek(graph: KnowledgeGraph, entities: int) -> list:
    return graph.cypher(SEEK_QUERY, params={"ids": _seek_ids(entities)}).to_list()


@pytest.fixture(scope="module", params=["memory", "disk"])
def seek_graph(request, tmp_path_factory):
    path = tmp_path_factory.mktemp("seek") / "graph"
    graph = _build_seek_graph(SEEK_ENTITIES, request.param, path)
    rows = _seek(graph, SEEK_ENTITIES)
    assert len(rows) == SEEK_IDS and {r["name"] for r in rows} == {"old"}
    return graph


@pytest.mark.benchmark
def test_bench_valid_time_id_seeks(benchmark, seek_graph):
    """2k id seeks under a context at a past instant on a 200k-node type,
    each index hit invisible."""
    rows = benchmark(lambda: _seek(seek_graph, SEEK_ENTITIES))
    assert len(rows) == SEEK_IDS


@pytest.mark.benchmark
@pytest.mark.parametrize("storage", ["memory", "disk"])
def test_valid_time_id_seeks_do_not_walk_the_type(storage, tmp_path):
    """The per-seek cost on a 200k-node type stays within 3x of a 20k-node
    one; a seek that walked the type would cost 10x. Meaningful in release."""
    import time

    per_seek = {}
    for entities in (SEEK_ENTITIES // 10, SEEK_ENTITIES):
        graph = _build_seek_graph(entities, storage, tmp_path / f"g{entities}")
        rows = _seek(graph, entities)  # builds the duplicate-id map
        assert {r["name"] for r in rows} == {"old"}
        best = float("inf")
        for _ in range(7):
            start = time.perf_counter()
            _seek(graph, entities)
            best = min(best, time.perf_counter() - start)
        per_seek[entities] = best / SEEK_IDS
    small, large = per_seek[SEEK_ENTITIES // 10], per_seek[SEEK_ENTITIES]
    print(f"\n{storage}: {small * 1e3:.4f} ms/seek at 20k nodes, {large * 1e3:.4f} ms/seek at 200k nodes")
    assert large < 3 * small, per_seek
