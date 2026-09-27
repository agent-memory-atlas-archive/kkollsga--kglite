"""Valid-time query cells: a guarded query on the full graph against the same
question asked on a graph that holds only the as-of slice.

Every A/B cell comes in two halves over the same data:

* ``*_context`` — the full graph (five periods of every versioned element), the
  question asked "as of" ``T`` with the statement prefix
  ``FOR VALID_TIME AS OF $t`` on the plain query (the guard in the pattern matcher). The
  variable-length cell keeps the two-argument ``valid_at(x, $t)`` spelling on
  every node of the path until variable-length relationships run under a
  context.
* ``*_view`` — the **view twin**: the same question, unguarded, on a graph
  built from only the elements that ``valid_at`` reports valid at ``T`` (every
  node and relationship the full graph holds valid at ``T``, no declarations).

When the mask-backed view lands (``freeze(valid_at=)``), each ``*_view`` cell
switches from the copied twin to it. The cell names, the graphs and the
expected answers stay the same, so the cells re-measure the guard and the view
as each lands. The module-scoped fixtures assert, before anything is timed, that
each context answer equals its view answer and differs from the unguarded
full-graph answer, so a cell cannot measure a filter that stopped filtering.

Default scale — 20k nodes / 100k relationships, 20% of the versioned elements
valid at ``T`` (one of five periods):

* ``E`` version nodes (node-declared ``vf``/``vt``; the last period open-ended)
  joined by timeless ``R`` relationships between same-period versions.
* SODIR-shaped: timeless ``Field`` and ``Company`` nodes, 33k ``HAS_LICENSEE``
  and 5k ``HAS_OPERATOR`` relationships (edge-declared).
* A/B cells: anchored 1-hop, open-ended node scan, var-length ``*1..3`` and
  ``count(*)``; the SODIR anchored 1-hop and the Q4 time-consistent 3-hop.
* Agent cells: ``count(*)`` per field, top-k ``ORDER BY … LIMIT``, degree count.
* Disk: one SODIR-shaped licensee hop on a disk graph, edge-declared (the guard
  reads each candidate relationship's bounds) against its node-declared twin
  (the same intervals as ``Stake`` fact nodes), in the ``valid_at`` spelling
  and in the pushed comparison spelling (``x.from <= $t AND (x.to IS NULL OR
  x.to >= $t)``, which the planner moves into the matcher's edge filter and
  node matchers — the property-guard route a disk context takes).
* Controls: two cells that never reach the pattern matcher or the storage
  backend (protocol item 8), the drift meter for every capture of this file.

``bench_heavy`` (200k / 1M, opt-in with ``-m bench_heavy``): the same graphs at
10× plus text and 64-d embeddings, with the full A/B set — anchored 1-hop and
3-hop, global 1-hop, node scan, var-length, ``count(*)``, SODIR Q4, BM25
top-10, vector top-10, PageRank and Louvain.
"""

from __future__ import annotations

from dataclasses import dataclass
import datetime as dt

import numpy as np
import pandas as pd
import pytest

from kglite import KnowledgeGraph

T = dt.date(2009, 6, 30)
PERIODS = 5
VALID_PERIOD = 2  # 2008-01-01 .. 2011-12-31 holds T
SEED = 20260926


@dataclass(frozen=True)
class Scale:
    entities: int  # each has PERIODS version nodes
    fields: int
    companies: int
    r_edges: int
    licensees_per_period: tuple[int, int]  # (count, how many field-periods get one extra)
    anchors: int
    field_anchors: int
    text: bool = False


# 13,000 E + 1,000 Field + 6,000 Company = 20,000 nodes;
# 62,000 R + 33,000 HAS_LICENSEE + 5,000 HAS_OPERATOR = 100,000 relationships.
DEFAULT = Scale(2_600, 1_000, 6_000, 62_000, (6, 3_000), 200, 50)
# 10x: 200,000 nodes / 1,000,000 relationships.
HEAVY = Scale(26_000, 10_000, 60_000, 620_000, (6, 30_000), 200, 50, text=True)
DIM = 64


def _period_bounds(period: int) -> tuple[pd.Timestamp, pd.Timestamp]:
    start = pd.Timestamp(2000 + 4 * period, 1, 1)
    end = pd.NaT if period == PERIODS - 1 else pd.Timestamp(2003 + 4 * period, 12, 31)
    return start, end


def _bounds(periods: np.ndarray) -> tuple[pd.Series, pd.Series]:
    starts, ends = zip(*(_period_bounds(int(p)) for p in range(PERIODS)))
    vf = pd.Series(pd.to_datetime([starts[p] for p in periods]))
    vt = pd.Series(pd.to_datetime([ends[p] for p in periods]))
    return vf, vt


def _texts(rng: np.random.Generator, n: int) -> list[str]:
    zipf = 1.0 / np.arange(1, 5_001) ** 1.05
    zipf /= zipf.sum()
    idx = rng.choice(5_000, size=(n, 8), p=zipf)
    return [" ".join(f"w{i}" for i in row) for row in idx]


def _frames(scale: Scale) -> dict[str, pd.DataFrame]:
    rng = np.random.default_rng(SEED)
    n_e = scale.entities * PERIODS
    vid = np.arange(n_e)
    period = vid % PERIODS
    vf, vt = _bounds(period)
    e = pd.DataFrame(
        {
            "vid": vid,
            "name": [f"E{i}" for i in vid],
            "eid": vid // PERIODS,
            "v": period,
            "vf": vf,
            "vt": vt,
            "score": rng.random(n_e),
        }
    )
    if scale.text:
        e["body"] = _texts(rng, n_e)

    # R joins two versions of the same period, so every path through R stays
    # inside one period: guarding the anchor decides the whole path.
    pairs: set[tuple[int, int]] = set()
    while len(pairs) < scale.r_edges:
        need = scale.r_edges - len(pairs)
        p = rng.integers(0, PERIODS, need)
        src = rng.integers(0, scale.entities, need) * PERIODS + p
        dst = rng.integers(0, scale.entities, need) * PERIODS + p
        pairs.update(zip(src.tolist(), dst.tolist()))
    r = pd.DataFrame(sorted(pairs)[: scale.r_edges], columns=["s", "t"])

    fields = pd.DataFrame({"fid": np.arange(scale.fields), "name": [f"F{i}" for i in range(scale.fields)]})
    companies = pd.DataFrame({"cid": np.arange(scale.companies), "name": [f"C{i}" for i in range(scale.companies)]})

    # Licensees of field f in period p: `base + j` companies, distinct across
    # (p, j) for one field, so no two periods share an endpoint pair.
    per, extra = scale.licensees_per_period
    lic_rows, op_rows = [], []
    slot = 0
    for f in range(scale.fields):
        for p in range(PERIODS):
            count = per + (1 if slot < extra else 0)
            slot += 1
            for j in range(count):
                c = (f * 37 + p * 1_009 + j * 131) % scale.companies
                lic_rows.append((f, c, p, round(float(rng.random()) * 100, 2)))
            op_rows.append((f, (f * 37 + p * 1_009) % scale.companies, p))
    lic = pd.DataFrame(lic_rows, columns=["f", "c", "p", "share"])
    lic["lf"], lic["lt"] = _bounds(lic["p"].to_numpy())
    op = pd.DataFrame(op_rows, columns=["f", "c", "p"])
    op["of"], op["ot"] = _bounds(op["p"].to_numpy())
    return {"E": e, "R": r, "Field": fields, "Company": companies, "LIC": lic, "OP": op}


def _load(frames: dict[str, pd.DataFrame], *, declared: bool, kg: KnowledgeGraph | None = None) -> KnowledgeGraph:
    """Load the frames into `kg` (a new in-memory graph by default)."""
    kg = KnowledgeGraph() if kg is None else kg
    kg.add_nodes(frames["E"], "E", "vid", "name")
    kg.add_nodes(frames["Field"], "Field", "fid", "name")
    kg.add_nodes(frames["Company"], "Company", "cid", "name")
    kg.add_connections(frames["R"], "R", "E", "s", "E", "t")
    lic = frames["LIC"][["f", "c", "share", "lf", "lt"]]
    kg.add_connections(lic, "HAS_LICENSEE", "Field", "f", "Company", "c")
    kg.add_connections(frames["OP"][["f", "c", "of", "ot"]], "HAS_OPERATOR", "Field", "f", "Company", "c")
    kg.create_index("E", "eid")
    if declared:
        kg.set_temporal("E", "vf", "vt")
        kg.set_temporal("HAS_LICENSEE", "lf", "lt")
        kg.set_temporal("HAS_OPERATOR", "of", "ot")
    return kg


def _view_frames(full: KnowledgeGraph, frames: dict[str, pd.DataFrame]) -> dict[str, pd.DataFrame]:
    """The frames of every element the full graph's `valid_at` keeps at `T`."""
    p = {"t": T}
    valid_e = {row["id"] for row in full.cypher("MATCH (a:E) WHERE valid_at(a, $t) RETURN a.id AS id", params=p)}

    def valid_pairs(rel: str) -> set[tuple[int, int]]:
        q = f"MATCH (f:Field)-[r:{rel}]->(c:Company) WHERE valid_at(r, $t) RETURN f.id AS f, c.id AS c"
        return {(row["f"], row["c"]) for row in full.cypher(q, params=p)}

    def keep(frame: pd.DataFrame, pairs: set[tuple[int, int]]) -> pd.DataFrame:
        mask = [(f, c) in pairs for f, c in zip(frame["f"], frame["c"])]
        return frame[mask]

    view = dict(frames)
    view["E"] = frames["E"][frames["E"]["vid"].isin(valid_e)]
    r = frames["R"]
    view["R"] = r[r["s"].isin(valid_e) & r["t"].isin(valid_e)]
    view["LIC"] = keep(frames["LIC"], valid_pairs("HAS_LICENSEE"))
    view["OP"] = keep(frames["OP"], valid_pairs("HAS_OPERATOR"))
    return view


@dataclass(frozen=True)
class Cell:
    plain: str  # the question, unguarded — run on the view twin
    context: str  # the question as of $t — run on the full graph


def _va(*names: str) -> str:
    return " AND ".join(f"valid_at({name}, $t)" for name in names)


AS_OF_T = "FOR VALID_TIME AS OF $t "


def _prefixed(plain: str) -> Cell:
    return Cell(plain, AS_OF_T + plain)


CELLS: dict[str, Cell] = {
    "anchored_1hop": _prefixed("MATCH (a:E)-[:R]->(b:E) WHERE a.eid IN $ids RETURN count(*) AS n"),
    "node_scan_open": _prefixed("MATCH (a:E) RETURN avg(a.score) AS s"),
    # Variable-length relationships do not run under a context yet.
    "var_length_1_3": Cell(
        "MATCH (a:E)-[:R*1..3]->(b:E) WHERE a.eid IN $ids RETURN count(DISTINCT b) AS n",
        "MATCH p = (a:E)-[:R*1..3]->(b:E) WHERE a.eid IN $ids "
        "AND all(x IN nodes(p) WHERE valid_at(x, $t)) RETURN count(DISTINCT b) AS n",
    ),
    "count_star": _prefixed("MATCH (a:E) RETURN count(*) AS n"),
    "sodir_anchored_1hop": _prefixed(
        "MATCH (f:Field)-[r:HAS_LICENSEE]->(c:Company) WHERE f.id IN $fids RETURN count(*) AS n"
    ),
    "sodir_q4_3hop": _prefixed(
        "MATCH (f:Field)-[o:HAS_OPERATOR]->(op:Company)<-[r1:HAS_LICENSEE]-(f2:Field)"
        "-[r2:HAS_LICENSEE]->(p:Company) WHERE f.id IN $fids AND p <> op RETURN count(*) AS n"
    ),
    "agent_count_per_field": _prefixed(
        "MATCH (f:Field)-[r:HAS_LICENSEE]->(c:Company) RETURN f.name AS field, count(*) AS n"
    ),
    "agent_top_k": _prefixed("MATCH (a:E) RETURN a.eid AS eid, a.score AS s ORDER BY s DESC LIMIT 10"),
    "agent_degree": _prefixed(
        "MATCH (c:Company)<-[r:HAS_LICENSEE]-(:Field) "
        "RETURN c.name AS company, count(r) AS deg ORDER BY deg DESC, company LIMIT 10"
    ),
}

# The P1b spelling of the open-ended node scan: pushed into the node matchers.
NODE_SCAN_PUSHED = "MATCH (a:E) WHERE a.vf <= $t AND (a.vt IS NULL OR a.vt >= $t) RETURN avg(a.score) AS s"

CONTROLS = {
    "unwind_sum": "UNWIND range(1, 300000) AS x RETURN sum(x % 7) AS s",
    "unwind_max": "UNWIND range(1, 300000) AS x RETURN max(x * 3 % 11) AS m",
}


def _params(scale: Scale) -> dict[str, object]:
    rng = np.random.default_rng(SEED + 1)
    return {
        "t": T,
        "ids": [int(x) for x in rng.choice(scale.entities, scale.anchors, replace=False)],
        "fids": [int(x) for x in rng.choice(scale.fields, scale.field_anchors, replace=False)],
    }


def _rows(kg: KnowledgeGraph, query: str, params: dict[str, object]) -> list[tuple]:
    rows = [
        tuple(round(v, 9) if isinstance(v, float) else v for v in row.values())
        for row in kg.cypher(query, params=params)
    ]
    return sorted(rows, key=repr)


@dataclass
class Pair:
    full: KnowledgeGraph
    view: KnowledgeGraph
    params: dict[str, object]


def _pair(scale: Scale) -> Pair:
    frames = _frames(scale)
    full = _load(frames, declared=True)
    view = _load(_view_frames(full, frames), declared=False)
    params = _params(scale)
    for name, cell in CELLS.items():
        guarded = _rows(full, cell.context, params)
        assert guarded == _rows(view, cell.plain, params), f"{name}: context answer differs from the view"
        assert guarded != _rows(full, cell.plain, params), f"{name}: the guard filters nothing"
    assert _rows(full, NODE_SCAN_PUSHED, params) == _rows(view, CELLS["node_scan_open"].plain, params)
    return Pair(full, view, params)


@pytest.fixture(scope="module")
def pair() -> Pair:
    return _pair(DEFAULT)


def _run(kg: KnowledgeGraph, query: str, params: dict[str, object]):
    return kg.cypher(query, params=params).to_list()


# ── Default-scale A/B and agent cells ────────────────────────────────────────


@pytest.mark.benchmark
@pytest.mark.parametrize("name", list(CELLS))
def test_temporal_context(benchmark, pair, name):
    benchmark(_run, pair.full, CELLS[name].context, pair.params)


@pytest.mark.benchmark
@pytest.mark.parametrize("name", list(CELLS))
def test_temporal_view(benchmark, pair, name):
    benchmark(_run, pair.view, CELLS[name].plain, pair.params)


@pytest.mark.benchmark
def test_temporal_context_node_scan_pushed(benchmark, pair):
    benchmark(_run, pair.full, NODE_SCAN_PUSHED, pair.params)


@pytest.mark.benchmark
@pytest.mark.parametrize("name", list(CONTROLS))
def test_temporal_control(benchmark, pair, name):
    benchmark(_run, pair.full, CONTROLS[name], {})


# ── Disk: edge-declared hop against its node-declared twin ───────────────────

DISK_GUARDS = {
    "valid_at": "valid_at({x}, $t)",
    "pushed": "{x}.lf <= $t AND ({x}.lt IS NULL OR {x}.lt >= $t)",
}
DISK_SHAPES = {
    "edge_declared": (
        "MATCH (f:Field)-[r:HAS_LICENSEE]->(c:Company) WHERE f.id IN $fids AND {g} RETURN count(*) AS n",
        "r",
    ),
    "node_declared": ("MATCH (f:Field)-[:HOLDS]->(s:Stake) WHERE f.id IN $fids AND {g} RETURN count(*) AS n", "s"),
}


def _disk_query(shape: str, guard: str) -> str:
    template, var = DISK_SHAPES[shape]
    if guard == "context":
        # The statement prefix: on disk every guard reads the bounds.
        return AS_OF_T + template.format(g="true")
    return template.format(g=DISK_GUARDS[guard].format(x=var))


def _disk_pair(path: str) -> tuple[KnowledgeGraph, dict[str, object]]:
    frames = _frames(DEFAULT)
    kg = KnowledgeGraph(storage="disk", path=path)
    kg.add_nodes(frames["Field"], "Field", "fid", "name")
    kg.add_nodes(frames["Company"], "Company", "cid", "name")
    lic = frames["LIC"]
    kg.add_connections(lic[["f", "c", "share", "lf", "lt"]], "HAS_LICENSEE", "Field", "f", "Company", "c")
    stakes = lic.assign(sid=np.arange(len(lic)), name=[f"S{i}" for i in range(len(lic))])
    kg.add_nodes(stakes[["sid", "name", "share", "lf", "lt"]], "Stake", "sid", "name")
    kg.add_connections(stakes[["f", "sid"]], "HOLDS", "Field", "f", "Stake", "sid")
    kg.set_temporal("HAS_LICENSEE", "lf", "lt")
    kg.set_temporal("Stake", "lf", "lt")
    params = _params(DEFAULT)
    counts = {(s, g): _rows(kg, _disk_query(s, g), params) for s in DISK_SHAPES for g in [*DISK_GUARDS, "context"]}
    assert len(set(map(tuple, counts.values()))) == 1, f"disk twins disagree: {counts}"
    return kg, params


@pytest.fixture(scope="module")
def disk_graph(tmp_path_factory) -> tuple[KnowledgeGraph, dict[str, object]]:
    return _disk_pair(str(tmp_path_factory.mktemp("temporal_disk") / "graph"))


@pytest.mark.benchmark
@pytest.mark.parametrize("guard", [*DISK_GUARDS, "context"])
@pytest.mark.parametrize("shape", list(DISK_SHAPES))
def test_temporal_disk(benchmark, disk_graph, shape, guard):
    kg, params = disk_graph
    benchmark(_run, kg, _disk_query(shape, guard), params)


# ── Correctness of the cells themselves (runs in the default suite) ─────────


def test_temporal_cells_answer_like_their_views():
    """Every context spelling returns its view twin's rows at a small scale."""
    _pair(Scale(260, 100, 600, 6_200, (6, 300), 20, 10))


# ── bench_heavy: 200k / 1M, I2's full A/B set ────────────────────────────────

HEAVY_CELLS: dict[str, Cell] = {
    **{
        name: CELLS[name]
        for name in ("anchored_1hop", "node_scan_open", "var_length_1_3", "count_star", "sodir_q4_3hop")
    },
    "anchored_3hop": _prefixed(
        "MATCH (a:E)-[:R]->(b:E)-[:R]->(c:E)-[:R]->(d:E) WHERE a.eid IN $ids RETURN count(*) AS n"
    ),
    "global_1hop": _prefixed("MATCH (a:E)-[:R]->(b:E) RETURN sum(b.score) AS s"),
    "bm25_top10": Cell(
        "MATCH (n:E) RETURN n.eid AS e, text_bm25(n, 'body', 'w7 w311') AS s ORDER BY s DESC, e LIMIT 10",
        f"MATCH (n:E) WHERE {_va('n')} "
        "RETURN n.eid AS e, text_bm25(n, 'body', 'w7 w311') AS s ORDER BY s DESC, e LIMIT 10",
    ),
    "vector_top10": Cell(
        "MATCH (n:E) RETURN n.eid AS e, vector_score(n, 'body_emb', $q) AS s ORDER BY s DESC LIMIT 10",
        f"MATCH (n:E) WHERE {_va('n')} "
        "RETURN n.eid AS e, vector_score(n, 'body_emb', $q) AS s ORDER BY s DESC LIMIT 10",
    ),
    "pagerank": Cell(
        "CALL pagerank({node_type: 'E'}) YIELD node, score RETURN count(*) AS n",
        f"CALL pagerank({{node_type: 'E', where: 'valid_at(n, date(\"{T.isoformat()}\"))'}}) "
        "YIELD node, score RETURN count(*) AS n",
    ),
    "louvain": Cell(
        "CALL louvain({node_type: 'E'}) YIELD node, community RETURN count(*) AS n",
        f"CALL louvain({{node_type: 'E', where: 'valid_at(n, date(\"{T.isoformat()}\"))'}}) "
        "YIELD node, community RETURN count(*) AS n",
    ),
}
# Answers that legitimately differ between the tiers: BM25 statistics come
# from the whole corpus on the full graph, HNSW is approximate on both.
HEAVY_UNCOMPARED = {"bm25_top10", "vector_top10"}


def _add_retrieval(kg: KnowledgeGraph, frames: dict[str, pd.DataFrame]) -> None:
    e = frames["E"]
    rng = np.random.default_rng(SEED + 2)
    vectors = rng.standard_normal((int(e["vid"].max()) + 1, DIM)).astype(np.float32)
    kg.set_embeddings("E", "body", {int(v): vectors[v].tolist() for v in e["vid"]})
    kg.build_node_vector_index("E", "body")
    kg.build_text_index("E", "body")


@pytest.fixture(scope="module")
def heavy_pair() -> Pair:
    frames = _frames(HEAVY)
    full = _load(frames, declared=True)
    view_frames = _view_frames(full, frames)
    view = _load(view_frames, declared=False)
    _add_retrieval(full, frames)
    _add_retrieval(view, view_frames)
    params = {**_params(HEAVY), "q": np.random.default_rng(SEED + 3).standard_normal(DIM).tolist()}
    for name, cell in HEAVY_CELLS.items():
        if name not in HEAVY_UNCOMPARED:
            assert _rows(full, cell.context, params) == _rows(view, cell.plain, params), name
    return Pair(full, view, params)


@pytest.mark.bench_heavy
@pytest.mark.parametrize("name", list(HEAVY_CELLS))
def test_temporal_heavy_context(benchmark, heavy_pair, name):
    benchmark(_run, heavy_pair.full, HEAVY_CELLS[name].context, heavy_pair.params)


@pytest.mark.bench_heavy
@pytest.mark.parametrize("name", list(HEAVY_CELLS))
def test_temporal_heavy_view(benchmark, heavy_pair, name):
    benchmark(_run, heavy_pair.view, HEAVY_CELLS[name].plain, heavy_pair.params)


@pytest.mark.bench_heavy
@pytest.mark.parametrize("name", list(CONTROLS))
def test_temporal_heavy_control(benchmark, heavy_pair, name):
    benchmark(_run, heavy_pair.full, CONTROLS[name], {})
