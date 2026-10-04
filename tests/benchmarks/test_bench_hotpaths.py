"""Hot-path benchmarks for the Phase 3 optimisation candidates.

Each benchmark isolates one suspected hot path so before/after deltas
attribute cleanly to a single change:

- DISTINCT keying (per-row Debug-format String allocation)
- RETURN n materialization on wide schemas (full property-map clone)
- UNWIND expansion (per-item row clone)
- WHERE with property access (per-row alias resolution)
- ORDER BY + LIMIT over expressions
- atomic-checkpoint selection for trivial in-memory mutations

Sized at 50k nodes so per-row costs dominate fixed overheads while a
full round stays comfortably under pytest-benchmark's calibration
budget. Run with: make bench (or pytest tests/benchmarks/ -m benchmark).
"""

import itertools

import pandas as pd
import pytest

from kglite import KnowledgeGraph

N = 50_000

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def hot_graph():
    """50k nodes, 12 properties each (wide schema), 100k edges.

    high_card has ~N/2 distinct values (stress dedup keying);
    mid_card has 1000; low_card has 10 (GROUP BY shape).
    """
    graph = KnowledgeGraph()

    nodes = pd.DataFrame(
        {
            "nid": list(range(N)),
            "name": [f"Node_{i}" for i in range(N)],
            "high_card": [f"hc_{i % (N // 2)}" for i in range(N)],
            "mid_card": [f"mc_{i % 1000}" for i in range(N)],
            "low_card": [f"lc_{i % 10}" for i in range(N)],
            "value": [float(i) for i in range(N)],
            "rank_val": [(i * 7919) % N for i in range(N)],
            "flag": [i % 2 == 0 for i in range(N)],
            "p1": [f"a{i % 97}" for i in range(N)],
            "p2": [f"b{i % 89}" for i in range(N)],
            "p3": [float(i % 83) for i in range(N)],
            "p4": [i % 79 for i in range(N)],
        }
    )
    graph.add_nodes(nodes, "Item", "nid", "name")

    edges = pd.DataFrame(
        {
            "from_id": [i % N for i in range(2 * N)],
            "to_id": [(i * 7 + 13) % N for i in range(2 * N)],
            "weight": [float(i % 100) for i in range(2 * N)],
        }
    )
    graph.add_connections(edges, "LINKS", "Item", "from_id", "Item", "to_id", columns=["weight"])

    return graph


# ---------------------------------------------------------------------------
# DISTINCT keying
# ---------------------------------------------------------------------------


@pytest.mark.benchmark
def test_bench_count_distinct_high_card(benchmark, hot_graph):
    """count(DISTINCT prop) with ~25k distinct string values."""
    benchmark(hot_graph.cypher, "MATCH (n:Item) RETURN count(DISTINCT n.high_card)")


@pytest.mark.benchmark
def test_bench_collect_distinct_mid_card(benchmark, hot_graph):
    """collect(DISTINCT prop) with 1000 distinct values."""
    benchmark(hot_graph.cypher, "MATCH (n:Item) RETURN size(collect(DISTINCT n.mid_card))")


@pytest.mark.benchmark
def test_bench_return_distinct_rows(benchmark, hot_graph):
    """RETURN DISTINCT over two columns (row-level dedup)."""
    benchmark(
        hot_graph.cypher,
        "MATCH (n:Item) RETURN DISTINCT n.mid_card, n.low_card",
    )


# ---------------------------------------------------------------------------
# RETURN n materialization (wide schema)
# ---------------------------------------------------------------------------


@pytest.mark.benchmark
def test_bench_return_whole_nodes_wide(benchmark, hot_graph):
    """RETURN n materializes all 12 properties x 5k nodes."""
    benchmark(hot_graph.cypher, "MATCH (n:Item) WHERE n.rank_val < 5000 RETURN n")


@pytest.mark.benchmark
def test_bench_return_two_props(benchmark, hot_graph):
    """Same selectivity, projecting 2 properties instead of the node."""
    benchmark(
        hot_graph.cypher,
        "MATCH (n:Item) WHERE n.rank_val < 5000 RETURN n.name, n.value",
    )


@pytest.mark.benchmark
def test_bench_collect_nodes(benchmark, hot_graph):
    """collect(n) per group — node materialization inside aggregation."""
    benchmark(
        hot_graph.cypher,
        "MATCH (n:Item) WHERE n.rank_val < 5000 RETURN n.low_card, size(collect(n))",
    )


# ---------------------------------------------------------------------------
# UNWIND expansion
# ---------------------------------------------------------------------------


@pytest.mark.benchmark
def test_bench_unwind_literal_x_rows(benchmark, hot_graph):
    """5k matched rows x UNWIND 10 — per-item row clone stress."""
    benchmark(
        hot_graph.cypher,
        "MATCH (n:Item) WHERE n.rank_val < 5000 UNWIND [1,2,3,4,5,6,7,8,9,10] AS x RETURN count(x)",
    )


@pytest.mark.benchmark
def test_bench_unwind_range_aggregate(benchmark, hot_graph):
    """Pure UNWIND throughput: 100k items into an aggregate."""
    benchmark(hot_graph.cypher, "UNWIND range(1, 100000) AS x RETURN sum(x)")


# ---------------------------------------------------------------------------
# WHERE / alias resolution
# ---------------------------------------------------------------------------


@pytest.mark.benchmark
def test_bench_where_multi_prop(benchmark, hot_graph):
    """WHERE touching three properties per row (alias-resolution heavy)."""
    benchmark(
        hot_graph.cypher,
        "MATCH (n:Item) WHERE n.value > 25000.0 AND n.p4 < 40 AND n.flag RETURN count(n)",
    )


@pytest.mark.benchmark
def test_bench_order_by_desc_limit(benchmark, hot_graph):
    """Single-key numeric DESC + LIMIT — the *improving-stream* top-K shape.

    `value` ascends with node order, so scanning DESC beats the retained worst
    on every row: the top-K retention path runs 50 000 times, not 25, and its
    per-row cost is the whole query's cost. That makes this cell, not its ASC
    twin below, the one that moves when the collector's retention path changes
    — 0.15.14's first cut regressed it ~20% while the ASC cell *improved* 1.4×,
    and a probe that measured only ASC read the regression as an improvement.
    Keep both directions.
    """
    benchmark(
        hot_graph.cypher,
        "MATCH (n:Item) RETURN n.name, n.value ORDER BY n.value DESC LIMIT 25",
    )


@pytest.mark.benchmark
def test_bench_order_by_asc_limit(benchmark, hot_graph):
    """Single-key numeric ASC + LIMIT — the *rejecting-stream* twin.

    Same query, opposite direction: the first 25 rows fill the heap and every
    later row is rejected by one comparison, so this cell measures the
    comparison path and the DESC cell above measures retention. They regress
    independently.
    """
    benchmark(
        hot_graph.cypher,
        "MATCH (n:Item) RETURN n.name, n.value ORDER BY n.value ASC LIMIT 25",
    )


@pytest.mark.benchmark
def test_bench_order_by_two_keys_limit(benchmark, hot_graph):
    """Multi-key ORDER BY + LIMIT — the leaderboard/pagination shape.

    Until 0.15.14 both top-K passes bailed on more than one sort item, so this
    ran a full O(n log n) sort plus a full projection of all 50k rows (~8x the
    single-key cell). It now takes the same bounded-heap path, with `low_card`
    forcing 5000-way ties on the leading key so the second key does real work.
    """
    benchmark(
        hot_graph.cypher,
        "MATCH (n:Item) RETURN n.name, n.value ORDER BY n.low_card, n.value DESC LIMIT 25",
    )


# ---------------------------------------------------------------------------
# Mutation checkpoint selection
# ---------------------------------------------------------------------------


@pytest.mark.benchmark
def test_bench_single_node_create_delete_large_graph(benchmark, hot_graph):
    """A trivial write must not deep-clone the unrelated 50k-node graph."""

    def cycle():
        hot_graph.cypher("CREATE (:Scratch {id: 9999999})")
        hot_graph.cypher("MATCH (n:Scratch {id: 9999999}) DELETE n")

    benchmark(cycle)


def _fork_then_write(graph, counter):
    """One round of "hold a view, then write" — the copy-on-write fork shape.

    `select` re-shares the Arc, so the `SET` that follows is a *first* write
    against a shared base and pays whatever the fork costs.

    The write alternates one node between two values rather than minting a new
    one per round: the index keeps a constant size over the thousands of rounds
    pytest-benchmark runs, so the cell measures the fork instead of the index
    growing under it.
    """
    holder = graph.select("Item")
    value = 900_000 + (next(counter) % 2)
    graph.cypher(
        "MATCH (n:Item {id: 1234}) SET n.rank_val = $v",
        params={"v": value},
    )
    del holder


@pytest.mark.benchmark
def test_bench_held_view_write_with_range_index(benchmark, hot_graph):
    """First write after a fork, with a **range** index on the written property.

    `range_indices` was a plain `BTreeMap` deep-cloned by every fork, so this
    cell measured 0.93 ms at 100k against 0.05 ms for the same shape with an
    equality index (P4, 2026-08-13). It now shares its levels like the other
    index families; the equality cell below is its control, and the two must
    stay within a small factor of each other.
    """
    hot_graph.build_id_indices(["Item"])
    hot_graph.create_range_index("Item", "rank_val")
    counter = itertools.count()
    benchmark(_fork_then_write, hot_graph, counter)


@pytest.mark.benchmark
def test_bench_held_view_write_with_eq_index(benchmark, hot_graph):
    """Control for the cell above: identical shape, equality index instead.

    `property_indices` has been layered since D2, so this cell is the floor the
    range cell is measured against — a regression in *both* is machine drift,
    a regression in one is the index.
    """
    hot_graph.build_id_indices(["Item"])
    hot_graph.create_index("Item", "rank_val")
    counter = itertools.count()
    benchmark(_fork_then_write, hot_graph, counter)


@pytest.mark.benchmark
def test_bench_cartesian_node_scans_limit(benchmark, hot_graph):
    """LIMIT 20 must cap node-only cartesian expansion before materialization."""
    benchmark(
        hot_graph.cypher,
        "MATCH (a:Item), (b:Item) RETURN a.nid, b.nid LIMIT 20",
    )


@pytest.mark.benchmark
def test_bench_two_hop_global_count(benchmark, hot_graph):
    """A pure count must stream exact path cardinality without building rows."""
    benchmark(
        hot_graph.cypher,
        "MATCH (a:Item)-[:LINKS]->(b:Item)-[:LINKS]->(c:Item) RETURN count(*) AS paths",
    )


@pytest.mark.benchmark
def test_bench_fixed_path_relationship_materialization(benchmark, hot_graph):
    """Exact relationship lookup for an anchored fixed-length path."""
    benchmark(
        hot_graph.cypher,
        "MATCH p=(a:Item {nid: 1})-[:LINKS]->(b:Item)-[:LINKS]->(c:Item) "
        "RETURN sum(size(relationships(p))) AS relationships",
    )


@pytest.mark.benchmark
def test_bench_variable_path_relationship_materialization(benchmark, hot_graph):
    """Relationship-unique trail tracking for an anchored variable path."""
    benchmark(
        hot_graph.cypher,
        "MATCH p=(a:Item {nid: 1})-[:LINKS*1..3]->(b:Item) RETURN sum(size(relationships(p))) AS relationships",
    )


# ---------------------------------------------------------------------------
# Point lookup with a live fluent clone (Arc shared) + rel-heavy named-var
# match — the two perf landmines fixed in the opencypher-contract branch.
# ---------------------------------------------------------------------------


@pytest.fixture
def prop_edge_graph():
    """20k nodes / 100k edges with 5 string props per edge.

    Sized so a per-edge property-map clone in the matcher (the old
    `MatchBinding::Edge { properties }` fill for named edge vars)
    dominates the query time if it ever comes back.
    """
    n_nodes, n_edges = 20_000, 100_000
    graph = KnowledgeGraph()
    nodes = pd.DataFrame(
        {
            "nid": list(range(n_nodes)),
            "name": [f"N_{i}" for i in range(n_nodes)],
        }
    )
    graph.add_nodes(nodes, "PN", "nid", "name")
    edges = pd.DataFrame(
        {
            "src": [i % n_nodes for i in range(n_edges)],
            "dst": [(i * 7 + 13) % n_nodes for i in range(n_edges)],
            "p1": [f"alpha_{i % 50}" for i in range(n_edges)],
            "p2": [f"beta_{i % 40}" for i in range(n_edges)],
            "p3": [f"gamma_{i % 30}" for i in range(n_edges)],
            "p4": [f"delta_{i % 20}" for i in range(n_edges)],
            "p5": [f"epsilon_{i % 10}" for i in range(n_edges)],
        }
    )
    graph.add_connections(edges, "PR", "PN", "src", "PN", "dst", columns=["p1", "p2", "p3", "p4", "p5"])
    return graph


@pytest.mark.benchmark
def test_bench_node_lookup_while_arc_shared(benchmark, hot_graph):
    """node() point lookup with a fresh fluent clone sharing the Arc.

    Each round re-shares the Arc (select) then does the point lookup —
    if node() ever regresses to `Arc::make_mut`, every round deep-copies
    the whole 50k-node graph and this jumps by ~4 orders of magnitude.
    """
    hot_graph.build_id_indices(["Item"])

    def share_then_lookup():
        holder = hot_graph.select("Item")
        got = hot_graph.node("Item", 1234)
        assert got is not None
        del holder

    benchmark(share_then_lookup)


# ---------------------------------------------------------------------------
# Point lookup by id — hit vs absent key
# ---------------------------------------------------------------------------


@pytest.mark.benchmark
def test_bench_point_lookup_id_hit(benchmark, hot_graph):
    """`MATCH (n:Item {id: X})` for an id that exists — the control cell.

    Answered by the per-type id index in O(1); flat in graph size. Read it
    alongside the miss cell below: the two must stay within the same order of
    magnitude, which is the whole point of the fix.
    """
    result = benchmark(hot_graph.cypher, "MATCH (n:Item {id: 1234}) RETURN n.name AS nm")
    assert len(result.to_list()) == 1


@pytest.mark.benchmark
def test_bench_point_lookup_id_miss(benchmark, hot_graph):
    """Same lookup for an id that does *not* exist.

    The anchor used to fall through to a full-type scan whenever the id index
    could not resolve the key, so every absent key cost O(V) to prove a result
    that was always empty — 0.39 ms at 50k nodes and 1.56 ms at 200k, against
    ~2.5 us for a hit. Absent keys are the common case for upsert probes and
    for SET/MERGE over externally-sourced id lists.
    """
    result = benchmark(hot_graph.cypher, "MATCH (n:Item {id: 999999}) RETURN n.name AS nm")
    assert result.to_list() == []


@pytest.mark.benchmark
def test_bench_unwind_point_lookup_misses(benchmark, hot_graph):
    """200 absent ids through the UNWIND point-lookup shape.

    Per-row amplification of the cell above: one full-type scan per unwound
    id. The measured cliff was 6.7 s for 16k absent ids versus 6.5 ms for the
    same count of hits.
    """
    ids = list(range(900_000, 900_200))
    result = benchmark(
        lambda: hot_graph.cypher(
            "UNWIND $ids AS i MATCH (n:Item {id: i}) RETURN count(n) AS c",
            params={"ids": ids},
        )
    )
    assert result.to_list() == [{"c": 0}]


@pytest.mark.benchmark
def test_bench_unwind_point_lookup_hits(benchmark, hot_graph):
    """The same UNWIND shape over 200 ids that all exist — control cell."""
    ids = list(range(200))
    result = benchmark(
        lambda: hot_graph.cypher(
            "UNWIND $ids AS i MATCH (n:Item {id: i}) RETURN count(n) AS c",
            params={"ids": ids},
        )
    )
    assert result.to_list() == [{"c": 200}]


# ---------------------------------------------------------------------------
# elementId() slot anchoring — the unlabelled point lookup an IDE sends
# ---------------------------------------------------------------------------
#
# `MATCH (v) WHERE elementId(v) = $eid` carries no label, so without the
# `anchor_element_id` pass it is a full node scan plus a per-row predicate
# (measured 28 s on a G.V() node-expansion round trip). Anchored, the slot is
# seeded as a pre-binding and the shape is a point lookup. The hit cells below
# must land in the same order of magnitude as the id point lookup above; the
# out-of-range cell is the miss control — it must not cost a scan either.


@pytest.mark.benchmark
def test_bench_element_id_untyped_param(benchmark, hot_graph):
    """`MATCH (v) WHERE elementId(v) = $eid` — the round-tripped element_id."""
    result = benchmark(
        lambda: hot_graph.cypher(
            "MATCH (v) WHERE elementId(v) = $eid RETURN v.name AS nm",
            params={"eid": "1234"},
        )
    )
    assert len(result.to_list()) == 1


@pytest.mark.benchmark
def test_bench_element_id_neighbourhood(benchmark, hot_graph):
    """The expansion the anchor exists for: neighbours of the clicked node."""
    result = benchmark(
        lambda: hot_graph.cypher(
            "MATCH p=(v)--() WHERE elementId(v) = $eid RETURN count(p) AS c",
            params={"eid": "1234"},
        )
    )
    assert result.to_list()[0]["c"] > 0


@pytest.mark.benchmark
def test_bench_element_id_out_of_range_miss(benchmark, hot_graph):
    """A slot past the end of the graph — the miss control: no rows, no scan."""
    result = benchmark(
        lambda: hot_graph.cypher(
            "MATCH (v) WHERE elementId(v) = $eid RETURN v.name AS nm",
            params={"eid": "9999999"},
        )
    )
    assert result.to_list() == []


# ---------------------------------------------------------------------------
# IN membership over a large list
# ---------------------------------------------------------------------------
#
# Membership used to be O(rows x |list|) at every evaluation site: 71 ms for a
# 1 000-element list over 50k rows, 576 ms at 16 000, 7.4 s at 64 000. The two
# list sizes below bracket that curve — with a coercion-normalized set they
# must be within a small constant of each other, not 8x apart. The small-list
# and range cells are the controls: neither goes through the index, so a
# regression there is the instrument, not the fix.


@pytest.mark.benchmark
def test_bench_in_param_list_1k(benchmark, hot_graph):
    """`WHERE n.p4 IN $vals` with 1 000 elements over 50k rows (param form)."""
    vals = list(range(1_000))
    result = benchmark(
        lambda: hot_graph.cypher(
            "MATCH (n:Item) WHERE n.rank_val IN $vals RETURN count(n) AS c",
            params={"vals": vals},
        )
    )
    assert result.to_list() == [{"c": 1_000}]


@pytest.mark.benchmark
def test_bench_in_param_list_16k(benchmark, hot_graph):
    """The same shape with 16 000 elements — 16x the list, ~1x the work."""
    vals = list(range(16_000))
    result = benchmark(
        lambda: hot_graph.cypher(
            "MATCH (n:Item) WHERE n.rank_val IN $vals RETURN count(n) AS c",
            params={"vals": vals},
        )
    )
    assert result.to_list() == [{"c": 16_000}]


@pytest.mark.benchmark
def test_bench_in_literal_list_1k(benchmark, hot_graph):
    """The literal-list form of the 1 000-element cell.

    A literal list reaches the executor as `Predicate::In`; on the fused
    MATCH+WHERE path it was never constant-folded, so it stayed a per-row
    linear scan even though the folded `InLiteralSet` form existed.
    """
    literal = "[" + ", ".join(str(i) for i in range(1_000)) + "]"
    result = benchmark(lambda: hot_graph.cypher(f"MATCH (n:Item) WHERE n.rank_val IN {literal} RETURN count(n) AS c"))
    assert result.to_list() == [{"c": 1_000}]


@pytest.mark.benchmark
def test_bench_in_string_list_1k(benchmark, hot_graph):
    """String membership, 1 000 elements — strings cost ~3.3x integers."""
    vals = [f"hc_{i}" for i in range(1_000)]
    result = benchmark(
        lambda: hot_graph.cypher(
            "MATCH (n:Item) WHERE n.high_card IN $vals RETURN count(n) AS c",
            params={"vals": vals},
        )
    )
    assert result.to_list() == [{"c": 2_000}]


@pytest.mark.benchmark
def test_bench_in_small_list(benchmark, hot_graph):
    """Control: an 8-element list stays on the linear scan — no index built."""
    vals = list(range(8))
    result = benchmark(
        lambda: hot_graph.cypher(
            "MATCH (n:Item) WHERE n.rank_val IN $vals RETURN count(n) AS c",
            params={"vals": vals},
        )
    )
    assert result.to_list() == [{"c": 8}]


@pytest.mark.benchmark
def test_bench_range_predicate_scan(benchmark, hot_graph):
    """Control: the same scan with a range predicate instead of IN."""
    result = benchmark(
        hot_graph.cypher,
        "MATCH (n:Item) WHERE n.rank_val < 1000 RETURN count(n) AS c",
    )
    assert result.to_list() == [{"c": 1_000}]


@pytest.mark.benchmark
def test_bench_named_rel_match_count(benchmark, prop_edge_graph):
    """MATCH with a *named* edge variable over 100k property-heavy edges.

    count(r) needs r bound but never reads its properties — the binding
    must stay index-only (no per-edge property-map clone in the matcher).
    """
    result = benchmark(prop_edge_graph.cypher, "MATCH (a:PN)-[r:PR]->(b:PN) RETURN count(r) AS c")
    assert result[0]["c"] == 100_000


@pytest.fixture
def chain_count_graph():
    """Four layers joined by distinct relationship types: 2 000 Team -> 100
    Dept <- 1 000 Project <- 20 000 Task, 3 funding links per project.

    The chain has 20 000 x 3 x 20 = 1.2M paths over 25k relationships, so the
    frontier DP touches the relationships and the matcher the paths.
    """
    graph = KnowledgeGraph()
    for label, n in (("Team", 2_000), ("Dept", 100), ("Project", 1_000), ("Task", 20_000)):
        frame = pd.DataFrame({"nid": list(range(n)), "name": [f"{label}_{i}" for i in range(n)]})
        graph.add_nodes(frame, label, "nid", "name")
    led_by = pd.DataFrame({"f": list(range(2_000)), "c": [i % 100 for i in range(2_000)]})
    graph.add_connections(led_by, "LED_BY", "Team", "f", "Dept", "c")
    funded_by = pd.DataFrame(
        {"l": [i % 1_000 for i in range(3_000)], "c": [(i * 7 + i // 1_000) % 100 for i in range(3_000)]}
    )
    graph.add_connections(funded_by, "FUNDED_BY", "Project", "l", "Dept", "c")
    tasks = pd.DataFrame({"w": list(range(20_000)), "l": [i % 1_000 for i in range(20_000)]})
    graph.add_connections(tasks, "IN_PROJECT", "Task", "w", "Project", "l")
    return graph


@pytest.mark.benchmark
def test_bench_hop3_chain_count(benchmark, chain_count_graph):
    """Linear three-hop chain `count(*)` over millions of paths.

    Planned as `FusedChainPathCount` (`fuse_chain_path_count`), a degree-product
    DP; the matcher route materialises every path. The plan assertion keeps the
    cell from silently timing the matcher.
    """
    query = "MATCH (f:Team)-[:LED_BY]->(c:Dept)<-[:FUNDED_BY]-(l:Project)<-[:IN_PROJECT]-(w:Task) RETURN count(*) AS n"
    plan = [row["operation"] for row in chain_count_graph.cypher("EXPLAIN " + query)]
    assert "OptimizerPass fuse_chain_path_count" in plan
    result = benchmark(chain_count_graph.cypher, query)
    expected = chain_count_graph.cypher(query, disabled_passes=["fuse_chain_path_count"]).to_list()
    assert result.to_list() == expected


@pytest.mark.benchmark
@pytest.mark.parametrize("target", ["w", "c"])
def test_bench_hop3_chain_distinct(benchmark, chain_count_graph, target):
    """Linear three-hop chain `count(DISTINCT x)` for the far end and a middle node.

    Planned as `FusedChainDistinctCount` (`fuse_chain_path_count`): a forward
    and a backward reachability sweep over the 25k relationships, where the
    matcher route builds 1.2M paths. The plan assertion keeps the cell from
    silently timing the matcher.
    """
    query = (
        "MATCH (f:Team)-[:LED_BY]->(c:Dept)<-[:FUNDED_BY]-(l:Project)<-[:IN_PROJECT]-(w:Task) "
        f"RETURN count(DISTINCT {target}) AS n"
    )
    plan = [row["operation"] for row in chain_count_graph.cypher("EXPLAIN " + query)]
    assert any(op.startswith("FusedChainDistinctCount") for op in plan), plan
    result = benchmark(chain_count_graph.cypher, query)
    expected = chain_count_graph.cypher(query, disabled_passes=["fuse_chain_path_count"]).to_list()
    assert result.to_list() == expected


STREAM_CHAIN = "MATCH (f:Team)-[:LED_BY]->(c:Dept)<-[:FUNDED_BY]-(l:Project)<-[:IN_PROJECT]-(w:Task)"


@pytest.mark.benchmark
@pytest.mark.parametrize(
    ("name", "tail"),
    [
        ("mid", "WHERE c.id < 3 RETURN c.title AS c, sum(w.id * 0.5) AS s, count(*) AS n"),
        ("full", "RETURN c.title AS c, sum(w.id * 0.5) AS s, avg(w.id) AS a"),
    ],
)
def test_bench_stream_match_aggregate(benchmark, chain_count_graph, name, tail):
    """Un-fused grouped aggregate over a three-hop chain, streamed.

    The leading MATCH feeds the aggregate chunk by chunk instead of
    materialising every path first (`mid` ~36k paths, `full` 1.2M); the answer
    must equal the materialised route's bit for bit.
    """
    query = f"{STREAM_CHAIN} {tail}"
    plan = [row["operation"] for row in chain_count_graph.cypher("EXPLAIN " + query)]
    assert not any(op.startswith("Fused") for op in plan), plan
    result = benchmark(chain_count_graph.cypher, query)
    expected = chain_count_graph.cypher(query, streaming=False).to_list()
    assert result.to_list() == expected


@pytest.fixture
def versioned_staff_graph():
    """30 000 people, 90 000 half-open versions (three abutting periods each).

    Built with the same loader calls a versioned register uses; `set_temporal`
    declares the interval, so `valid_at(e, d)` reads it.
    """
    people, versions = 30_000, 3
    rows = []
    for person in range(people):
        start = 10_957 + (person * 7) % 4_000  # days since 1970, from 2000-01-01
        for v in range(versions):
            end = start + 300 + (person + v * 13) % 700
            rows.append((person * versions + v, f"Person {person} v{v}", start, end if v < versions - 1 else None))
            start = end
    frame = pd.DataFrame(rows, columns=["vid", "title", "start", "end"])
    for column in ("start", "end"):
        frame[column] = pd.to_datetime(frame[column], unit="D")
    graph = KnowledgeGraph()
    graph.add_nodes(frame, "Staff", "vid", "title")
    graph.set_temporal("Staff", "start", "end", "half_open")
    return graph


@pytest.mark.benchmark
def test_bench_unwind_valid_at_count(benchmark, versioned_staff_graph):
    """`UNWIND <24 instants> MATCH (x:T) WHERE valid_at(x, d) RETURN d, count(*)`.

    Planned as `FusedValidAtJoin` in count mode (`fuse_unwind_valid_at`): two
    binary searches per instant. The unfused route joins every instant to every
    version (24 x 90 000 rows) before filtering.
    """
    instants = [f"{2000 + i % 12}-{1 + i // 12 * 5:02d}-15" for i in range(24)]
    query = "UNWIND $ds AS d MATCH (e:Staff) WHERE valid_at(e, date(d)) RETURN d, count(*) AS n"
    params = {"ds": instants}
    plan = [row["operation"] for row in versioned_staff_graph.cypher("EXPLAIN " + query, params=params)]
    assert "OptimizerPass fuse_unwind_valid_at" in plan
    result = benchmark(versioned_staff_graph.cypher, query, params=params)
    expected = versioned_staff_graph.cypher(query, params=params, disabled_passes=["fuse_unwind_valid_at"])
    assert sorted(map(repr, result.to_list())) == sorted(map(repr, expected.to_list()))


@pytest.mark.benchmark
def test_bench_unwind_valid_at_rows(benchmark, versioned_staff_graph):
    """The row form of the same join: one scan, a bit test per version and instant."""
    instants = [f"{2000 + i % 12}-{1 + i // 12 * 5:02d}-15" for i in range(24)]
    query = "UNWIND $ds AS d MATCH (e:Staff) WHERE valid_at(e, date(d)) RETURN d, e.title AS t"
    params = {"ds": instants}
    plan = [row["operation"] for row in versioned_staff_graph.cypher("EXPLAIN " + query, params=params)]
    assert "OptimizerPass fuse_unwind_valid_at" in plan
    result = benchmark(versioned_staff_graph.cypher, query, params=params)
    expected = versioned_staff_graph.cypher(query, params=params, disabled_passes=["fuse_unwind_valid_at"])
    assert sorted(map(repr, result.to_list())) == sorted(map(repr, expected.to_list()))


@pytest.fixture
def declared_types_with_secondary_graph():
    """40 declared types of 50 nodes each, one undeclared 20 000-node type, and
    an undeclared secondary label on one declared node.

    The shape of a register with many versioned types: the default valid-time
    context compiles a filter for the declared targets a statement reaches,
    and statements that name only the undeclared type reach none.
    """
    graph = KnowledgeGraph()
    for i in range(40):
        frame = pd.DataFrame(
            {
                "id": range(i * 50, (i + 1) * 50),
                "title": [f"t{j}" for j in range(50)],
                "vf": pd.to_datetime(["2000-01-01"] * 50),
                "vt": pd.to_datetime(["2010-01-01" if j % 2 else "2099-01-01" for j in range(50)]),
            }
        )
        graph.add_nodes(frame, f"T{i}", "id", "title")
    slides = pd.DataFrame({"id": range(10**6, 10**6 + 20_000), "title": [f"s{j}" for j in range(20_000)]})
    graph.add_nodes(slides, "Slide", "id", "title")
    for i in range(40):
        graph.cypher(f"CALL db.temporal.declare({{node: 'T{i}', from: 'vf', to: 'vt', convention: 'half_open'}})")
    graph.cypher("MATCH (n:T0) WHERE n.id < 5 SET n:Extra")
    return graph


@pytest.mark.benchmark
def test_bench_default_context_undeclared_limit(benchmark, declared_types_with_secondary_graph):
    """`MATCH (s:Slide) RETURN s.title LIMIT 10` under the default context.

    Reaches no declared target, so it plans and runs as the `FOR VALID_TIME ALL`
    cell below does; the two cells differ only by the context's fixed cost.
    """
    graph = declared_types_with_secondary_graph
    query = "MATCH (s:Slide) RETURN s.title LIMIT 10"
    result = benchmark(graph.cypher, query)
    assert result.diagnostics["temporal"]["targets"] == []
    assert len(result.to_list()) == 10


@pytest.mark.benchmark
def test_bench_all_context_undeclared_limit(benchmark, declared_types_with_secondary_graph):
    """The control for the cell above: the same statement reading every version."""
    graph = declared_types_with_secondary_graph
    result = benchmark(graph.cypher, "FOR VALID_TIME ALL MATCH (s:Slide) RETURN s.title LIMIT 10")
    assert len(result.to_list()) == 10


@pytest.mark.benchmark
def test_bench_default_context_declared_hop_limit(benchmark, declared_types_with_secondary_graph):
    """A two-type hop under the default context: the template names only the
    two types it reaches, not the other 38 declared ones."""
    graph = declared_types_with_secondary_graph
    graph.cypher(
        "MATCH (a:T0), (b:T1) WHERE a.id = b.id - 50 CREATE (a)-[:R]->(b)",
    ).to_list()
    query = "MATCH (a:T0)-[:R]->(b:T1) RETURN a.title, b.title LIMIT 10"
    result = benchmark(graph.cypher, query)
    assert result.diagnostics["temporal"]["targets"] == ["(:T0)", "(:T1)"]
    assert len(result.to_list()) == 10


@pytest.fixture
def declared_edges_graph():
    """20 000 declared nodes, 200 000 declared relationships of one type (a
    tenth expired, a tenth with an expired endpoint) and 50 000 undeclared
    ones: the shape of a register whose relationship counts the default
    valid-time context must answer without testing every relationship."""
    graph = KnowledgeGraph()
    n = 20_000
    nodes = pd.DataFrame(
        {
            "id": range(n),
            "title": [f"n{i}" for i in range(n)],
            "vf": pd.to_datetime(["2000-01-01"] * n),
            "vt": pd.to_datetime(["2010-01-01" if i % 10 == 0 else "2099-01-01" for i in range(n)]),
        }
    )
    graph.add_nodes(nodes, "Party", "id", "title")
    m = 200_000
    contracts = pd.DataFrame(
        {
            "a": [i % n for i in range(m)],
            "b": [(i * 7 + 1) % n for i in range(m)],
            "vf": pd.to_datetime(["2000-01-01"] * m),
            "vt": pd.to_datetime(["2010-01-01" if i % 10 == 3 else "2099-01-01" for i in range(m)]),
        }
    )
    graph.add_connections(contracts, "CONTRACTED", "Party", "a", "Party", "b", columns=["vf", "vt"])
    knows = pd.DataFrame({"a": [i % n for i in range(50_000)], "b": [(i * 3 + 2) % n for i in range(50_000)]})
    graph.add_connections(knows, "KNOWS", "Party", "a", "Party", "b")
    graph.cypher("CALL db.temporal.declare({node: 'Party', from: 'vf', to: 'vt', convention: 'half_open'})")
    graph.cypher(
        "CALL db.temporal.declare({relationship: 'CONTRACTED', from: 'vf', to: 'vt', convention: 'half_open'})"
    )
    return graph


@pytest.mark.benchmark
def test_bench_default_context_declared_edge_count(benchmark, declared_edges_graph):
    """`MATCH ()-[r:CONTRACTED]->() RETURN count(r)` under the default context:
    the masks' stored per-type answer, not a test of every relationship."""
    graph = declared_edges_graph
    query = "MATCH ()-[r:CONTRACTED]->() RETURN count(r) AS n"
    expected = graph.cypher(query).to_list()[0]["n"]
    assert 0 < expected < 200_000
    result = benchmark(graph.cypher, query)
    assert result.to_list()[0]["n"] == expected


@pytest.mark.benchmark
def test_bench_all_context_declared_edge_count(benchmark, declared_edges_graph):
    """The control for the cell above: the same count reading every version."""
    result = benchmark(
        declared_edges_graph.cypher, "FOR VALID_TIME ALL MATCH ()-[r:CONTRACTED]->() RETURN count(r) AS n"
    )
    assert result.to_list()[0]["n"] == 200_000


GROUPED_PARTY_COUNT = (
    "MATCH (a:Party)-[:CONTRACTED]->(b:Party) WITH b, count(a) AS n RETURN b.title AS t, n ORDER BY n DESC, t LIMIT 10"
)


@pytest.mark.benchmark
def test_bench_default_context_grouped_peer_count(benchmark, declared_edges_graph):
    """A grouped count over one hop under the default context: after a few
    repeats the per-node counts come from the cached histogram instead of a walk
    of every incident relationship."""
    graph = declared_edges_graph
    expected = graph.cypher(GROUPED_PARTY_COUNT).to_list()
    assert len(expected) == 10
    result = benchmark(graph.cypher, GROUPED_PARTY_COUNT)
    assert result.to_list() == expected


@pytest.mark.benchmark
def test_bench_all_context_grouped_peer_count(benchmark, declared_edges_graph):
    """The same grouped count reading every version: the unfiltered histogram."""
    graph = declared_edges_graph
    query = "FOR VALID_TIME ALL " + GROUPED_PARTY_COUNT
    expected = graph.cypher(query).to_list()
    assert len(expected) == 10
    result = benchmark(graph.cypher, query)
    assert result.to_list() == expected


@pytest.mark.benchmark
def test_bench_default_context_optional_peer_count(benchmark, declared_edges_graph):
    """An `OPTIONAL MATCH` grouped count under the default context."""
    graph = declared_edges_graph
    query = "MATCH (b:Party) OPTIONAL MATCH (b)<-[:CONTRACTED]-(a:Party) RETURN b.title AS t, count(a) AS n"
    expected = sorted((r["t"], r["n"]) for r in graph.cypher(query).to_list())
    result = benchmark(graph.cypher, query)
    assert sorted((r["t"], r["n"]) for r in result.to_list()) == expected
